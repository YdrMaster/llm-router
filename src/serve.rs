#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::CONTENT_TYPE;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use log::{info, warn};
use tokio::net::TcpListener;

use crate::config::{BackendConfig, Config, RouteGroup};
use crate::health::BackendHealth;
use crate::protocol::{AnthropicProtocol, ModelInfo, OpenAiProtocol, Protocol};

/// 通用响应体类型（用于非流式响应）
type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

#[derive(Clone)]
struct Server {
    backends: Arc<HashMap<String, Backend>>,
    router: Arc<HashMap<String, RouteGroup>>,
    protocols: Vec<Arc<dyn Protocol>>,
    http_client: Client<HttpConnector, Full<Bytes>>,
}

/// 运行时后端，包含配置和健康状态
struct Backend {
    config: BackendConfig,
    health: BackendHealth,
}

impl Server {
    fn new(config: Config) -> Self {
        let protocols: Vec<Arc<dyn Protocol>> =
            vec![Arc::new(OpenAiProtocol), Arc::new(AnthropicProtocol)];

        let http_client = Client::builder(hyper_util::rt::TokioExecutor::new()).build_http();

        // 将 Config 转换为运行时数据结构
        let backends: HashMap<String, Backend> = config
            .backend
            .into_iter()
            .map(|(name, config)| {
                (
                    name,
                    Backend {
                        config,
                        health: BackendHealth::new(),
                    },
                )
            })
            .collect();

        Server {
            backends: Arc::new(backends),
            router: Arc::new(config.router),
            protocols,
            http_client,
        }
    }

    /// 查找给定模型名称的第一个健康后端
    fn find_backend(&self, model: &str) -> Option<(String, &Backend)> {
        let route = self.router.get(model)?;

        // 按顺序遍历后端，查找第一个健康的后端
        for backend_name in &route.backends {
            let name = backend_name.as_ref();
            if let Some(backend) = self.backends.get(name)
                && backend.health.is_healthy()
            {
                return Some((name.to_string(), backend));
            }
        }

        // 所有后端都不健康，仍然返回第一个（可能会失败）
        let backend_name = route.backends.first()?.as_ref();
        let backend = self.backends.get(backend_name)?;
        Some((backend_name.to_string(), backend))
    }

    /// 修改请求体，如果配置了则替换 model 并添加 api-key
    fn modify_request_body(
        &self,
        body_bytes: Bytes,
        backend: &Backend,
        original_model: &str,
    ) -> Bytes {
        // 解析 JSON 请求体
        let mut json: serde_json::Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(_) => return body_bytes, // 如果解析失败，返回原始请求体
        };

        // 如果后端配置了自定义 model，则替换
        if let Some(ref backend_model) = backend.config.model {
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(backend_model.to_string()),
                );
            }
        } else {
            // 否则保留原始 model
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(original_model.to_string()),
                );
            }
        }

        // 序列化回字节
        match serde_json::to_vec(&json) {
            Ok(vec) => Bytes::from(vec),
            Err(_) => body_bytes,
        }
    }

    pub async fn serve(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let port = self.backends.iter().next().map(|_| 8000).unwrap_or(8000); // Placeholder
        let addr = SocketAddr::from(([0, 0, 0, 0], port as u16));
        let listener = TcpListener::bind(addr).await?;
        info!("Listening on http://{}", addr);

        let server = Arc::new(self);

        loop {
            let (stream, remote_addr) = listener.accept().await?;
            info!("Accepted connection from {}", remote_addr);

            let server = Arc::clone(&server);
            tokio::spawn(async move {
                if let Err(e) = server.handle_connection(stream).await {
                    warn!("Error handling connection from {}: {}", remote_addr, e);
                }
            });
        }
    }

    async fn handle_connection(
        &self,
        stream: tokio::net::TcpStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let io = TokioIo::new(stream);
        let server = self.clone();

        http1::Builder::new()
            .serve_connection(
                io,
                service_fn(move |req| {
                    let server = server.clone();
                    async move { server.handle_request(req).await }
                }),
            )
            .await?;

        Ok(())
    }

    async fn handle_request(
        &self,
        req: Request<Incoming>,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        // 记录请求信息
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let content_type = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        info!("{} {}", method, path);

        // 处理 GET 请求获取模型列表
        if method == Method::GET {
            return self.handle_models_list(&path);
        }

        // 只处理 POST 请求用于 completions/messages
        if method != Method::POST {
            return Ok(Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(CONTENT_TYPE, "text/plain")
                .body(
                    Full::from("Method not allowed")
                        .map_err(|_| std::io::Error::other("error"))
                        .boxed(),
                )
                .unwrap());
        }

        // 收集请求体
        // 先提取原始请求的 Authorization 头（用于后端未配置 api_key 时转发）
        let original_auth_header = req
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body_bytes = match req.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                warn!("Failed to collect request body: {}", e);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "text/plain")
                    .body(
                        Full::from("Failed to read request body")
                            .map_err(|_| std::io::Error::other("error"))
                            .boxed(),
                    )
                    .unwrap());
            }
        };

        // 尝试匹配并解析每个协议
        let mut matched = false;
        let mut model_name = String::new();
        for protocol in &self.protocols {
            if protocol.matches(&path, content_type.as_deref()) {
                match protocol.parse(body_bytes.clone()) {
                    Ok(parsed) => {
                        info!("Matched protocol, model: {}", parsed.model);
                        model_name = parsed.model;
                        matched = true;
                        break;
                    }
                    Err(e) => {
                        warn!("Failed to parse request: {}", e);
                        return Ok(Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header(CONTENT_TYPE, "text/plain")
                            .body(
                                Full::from(format!("Invalid request: {}", e))
                                    .map_err(|_| {
                                        std::io::Error::other("error")
                                    })
                                    .boxed(),
                            )
                            .unwrap());
                    }
                }
            }
        }

        if !matched {
            warn!(
                "No matching protocol for path: {}, content-type: {:?}",
                path, content_type
            );
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(CONTENT_TYPE, "text/plain")
                .body(
                    Full::from("No matching protocol")
                        .map_err(|_| std::io::Error::other("error"))
                        .boxed(),
                )
                .unwrap());
        }

        // 查找此模型的后端并尝试故障转移
        self.handle_with_failover(path, body_bytes, &model_name, original_auth_header)
            .await
    }

    /// 处理带重试和后端故障转移的请求
    async fn handle_with_failover(
        &self,
        path: String,
        body_bytes: Bytes,
        model_name: &str,
        original_auth_header: Option<String>,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        let mut tried_backends: Vec<String> = Vec::new();

        loop {
            // 查找下一个要尝试的后端
            let backend_result = if tried_backends.is_empty() {
                self.find_backend(model_name)
            } else {
                // 尝试查找尚未尝试的后端
                let mut next_backend = None;
                if let Some(route) = self.router.get(model_name) {
                    for backend_name in &route.backends {
                        let name = backend_name.as_ref();
                        if !tried_backends.contains(&name.to_string())
                            && let Some(backend) = self.backends.get(name)
                        {
                            next_backend = Some((name.to_string(), backend));
                            break;
                        }
                    }
                }
                next_backend
            };

            let (backend_name, backend) = match backend_result {
                Some(b) => b,
                None => {
                    // 没有更多后端可尝试
                    warn!("All backends failed for model: {}", model_name);
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .header(CONTENT_TYPE, "text/plain")
                        .body(
                            Full::from("All backends unavailable")
                                .map_err(|_| {
                                    std::io::Error::other("error")
                                })
                                .boxed(),
                        )
                        .unwrap());
                }
            };

            info!(
                "Trying backend: {} (retry={}, cooldown={:?})",
                backend_name, backend.config.retry, backend.config.cooldown
            );
            tried_backends.push(backend_name.clone());

            // 尝试带重试地转发请求
            match self
                .forward_with_retry(
                    path.clone(),
                    body_bytes.clone(),
                    &backend_name,
                    backend,
                    model_name,
                    original_auth_header.clone(),
                )
                .await
            {
                Ok(response) => {
                    // 检查响应是否表示错误（5xx 状态）
                    let status = response.status();
                    if status.is_server_error() {
                        warn!("Backend {} returned server error: {}", backend_name, status);

                        // 记录失败并检查是否应进入冷却
                        if backend.health.record_failure(backend.config.retry) {
                            // 进入冷却
                            backend.health.set_cooldown(backend.config.cooldown);
                            warn!(
                                "Backend {} entered cooldown for {:?}",
                                backend_name, backend.config.cooldown
                            );
                        }
                        // 继续循环尝试下一个后端
                        let _ = response;
                        continue;
                    } else {
                        // 成功 - 记录并返回
                        backend.health.record_success();
                        return Ok(response);
                    }
                }
                Err(_) => {
                    // 重试后后端连接仍失败
                    warn!("Backend {} failed after retries", backend_name);

                    // 记录失败并检查是否应进入冷却
                    if backend.health.record_failure(backend.config.retry) {
                        // 进入冷却
                        backend.health.set_cooldown(backend.config.cooldown);
                        warn!(
                            "Backend {} entered cooldown for {:?}",
                            backend_name, backend.config.cooldown
                        );
                    }
                    // 继续循环尝试下一个后端
                    continue;
                }
            }
        }
    }

    /// 带重试逻辑的转发请求
    async fn forward_with_retry(
        &self,
        path: String,
        body_bytes: Bytes,
        backend_name: &str,
        backend: &Backend,
        original_model: &str,
        original_auth_header: Option<String>,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        for attempt in 0..backend.config.retry {
            if attempt > 0 {
                info!(
                    "Retry {}/{} for backend {}",
                    attempt + 1,
                    backend.config.retry,
                    backend_name
                );
            }

            match self
                .forward_request(
                    path.clone(),
                    body_bytes.clone(),
                    backend,
                    original_model,
                    backend_name,
                    original_auth_header.clone(),
                )
                .await
            {
                Ok(response) => {
                    return Ok(response);
                }
                Err(e) => {
                    warn!(
                        "Attempt {} failed for backend {}: {}",
                        attempt + 1,
                        backend_name,
                        e
                    );
                    // 继续下一次重试
                }
            }
        }

        warn!(
            "Backend {} failed after {} attempts",
            backend_name, backend.config.retry
        );
        Err(std::io::Error::other("error"))
    }

    async fn forward_request(
        &self,
        path: String,
        body_bytes: Bytes,
        backend: &Backend,
        original_model: &str,
        backend_name: &str,
        original_auth_header: Option<String>,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        let url = format!("{}{}", backend.config.base_url, path);

        // 如果后端配置了自定义 model 或 api_key，则修改请求体
        let modified_body = self.modify_request_body(body_bytes, backend, original_model);

        // 构建转发请求
        let mut req_builder = Request::builder()
            .method(Method::POST)
            .uri(&url)
            .header(CONTENT_TYPE, "application/json");

        // 如果后端配置了 api_key，使用配置的 api_key
        // 否则，如果原请求带有 Authorization 头，则保留
        if let Some(ref api_key) = backend.config.api_key {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", api_key));
        } else if let Some(auth_header) = original_auth_header {
            req_builder = req_builder.header("Authorization", auth_header);
        }

        let forward_req = req_builder.body(Full::from(modified_body)).unwrap();

        // 发送请求到后端
        match self.http_client.request(forward_req).await {
            Ok(response) => {
                let (parts, body) = response.into_parts();

                info!("Backend {} response status: {}", backend_name, parts.status);

                // 流式转发后端响应体
                Ok(Response::from_parts(
                    parts,
                    body.map_err(|_| std::io::Error::other("error"))
                        .boxed(),
                ))
            }
            Err(_e) => Err(std::io::Error::other("error")),
        }
    }

    fn handle_models_list(&self, path: &str) -> Result<Response<BoxBody>, std::io::Error> {
        // 仅支持 OpenAI 风格的 /v1/models 端点
        if path != "/v1/models" {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(CONTENT_TYPE, "text/plain")
                .body(
                    Full::from("Models endpoint not found")
                        .map_err(|_| std::io::Error::other("error"))
                        .boxed(),
                )
                .unwrap());
        }

        // 从路由配置构建模型列表
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let models: Vec<ModelInfo> = self
            .router
            .keys()
            .map(|name| ModelInfo {
                id: name.clone(),
                object: "model".to_string(),
                created: now,
                owned_by: "llm-router".to_string(),
            })
            .collect();

        let body = OpenAiProtocol::list_models(&models);

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(
                Full::from(body)
                    .map_err(|_| std::io::Error::other("error"))
                    .boxed(),
            )
            .unwrap())
    }
}

pub async fn serve(config: Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port = config.service.port;
    let server = Server::new(config);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{}", addr);

    let server = Arc::new(server);

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        info!("Accepted connection from {}", remote_addr);

        let server = Arc::clone(&server);
        tokio::spawn(async move {
            if let Err(e) = server.handle_connection(stream).await {
                warn!("Error handling connection from {}: {}", remote_addr, e);
            }
        });
    }
}
