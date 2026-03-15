use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use log::{info, warn};
use tokio::net::TcpListener;

use crate::config::{BackendConfig, Config, LoadBalancerConfig};
use crate::health::BackendHealth;
use crate::middleware::{InterceptAction, Middleware, RequestContext};
use crate::protocol::{AnthropicProtocol, ModelInfo, OpenAiProtocol, Protocol};

/// 通用响应体类型（用于非流式响应）
type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

/// 转发请求的参数封装
struct ForwardRequestParams {
    path: String,
    body_bytes: Bytes,
    backend_name: String,
    original_model: String,
    original_headers: HashMap<String, Box<[u8]>>,
    using_x_api_key: bool,
}

#[derive(Clone)]
struct Server {
    backends: Arc<HashMap<String, Backend>>,
    load_balancers: Arc<HashMap<String, LoadBalancerConfig>>,
    aliases: Arc<HashMap<String, Box<str>>>,
    router: Arc<HashMap<String, Box<[Box<str>]>>>,
    protocols: Vec<Arc<dyn Protocol>>,
    http_client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    middleware: Arc<dyn Middleware>,
}

/// 运行时后端，包含配置和健康状态
struct Backend {
    config: BackendConfig,
    health: BackendHealth,
}

impl Server {
    fn new(config: Config, middleware: Arc<dyn Middleware>) -> Self {
        let protocols: Vec<Arc<dyn Protocol>> =
            vec![Arc::new(OpenAiProtocol), Arc::new(AnthropicProtocol)];

        // 创建支持 HTTP 和 HTTPS 的连接器
        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);
        http_connector.enforce_http(false);

        let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .unwrap()
            .https_or_http()
            .enable_http1()
            .wrap_connector(http_connector);

        let http_client = Client::builder(hyper_util::rt::TokioExecutor::new())
            .pool_max_idle_per_host(32)
            .build(https_connector);

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
            load_balancers: Arc::new(config.load_balancer),
            aliases: Arc::new(config.aliases),
            router: Arc::new(config.router),
            protocols,
            http_client,
            middleware,
        }
    }

    /// 从负载均衡池中选择一个后端
    fn select_from_load_balancer(&self, lb_name: &str) -> Option<String> {
        let lb = self.load_balancers.get(lb_name)?;
        
        // 生成随机种子（对于 Shuffle 策略）
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as usize;
        
        let index = lb.select_index(seed);
        lb.get_backend(index).map(|s| s.to_string())
    }

    /// 查找给定模型名称的第一个健康后端
    fn find_backend(&self, model: &str) -> Option<(String, &Backend)> {
        // 先查别名表，获取最终的路由名
        let route_name = self.aliases.get(model).map_or(model, |v| v.as_ref());

        // 查路由表获取后端列表
        let backends = self.router.get(route_name)?;

        // 按顺序遍历后端，查找第一个健康的后端
        for backend_name in backends.iter() {
            let name = backend_name.as_ref();
            
            // 检查是否是负载均衡池
            let actual_backend_name = if self.load_balancers.contains_key(name) {
                // 从负载均衡池中选择一个后端
                self.select_from_load_balancer(name)?
            } else {
                name.to_string()
            };
            
            if let Some(backend) = self.backends.get(&actual_backend_name)
                && backend.health.is_healthy()
            {
                return Some((actual_backend_name, backend));
            }
        }

        // 所有后端都不健康，仍然返回第一个（可能会失败）
        let first_name = backends.first()?.as_ref();
        
        // 检查第一个是否是负载均衡池
        let actual_first = if self.load_balancers.contains_key(first_name) {
            self.select_from_load_balancer(first_name)?
        } else {
            first_name.to_string()
        };
        
        let backend = self.backends.get(&actual_first)?;
        Some((actual_first, backend))
    }

    /// 修改请求体，如果配置了则替换 model
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

        // 替换 model 字段：优先使用后端配置的 model，否则使用原始 model
        if let Some(obj) = json.as_object_mut() {
            let model_to_use = backend.config.model.as_deref().unwrap_or(original_model);
            obj.insert(
                "model".to_string(),
                serde_json::Value::String(model_to_use.to_string()),
            );
        }

        // 序列化回字节
        match serde_json::to_vec(&json) {
            Ok(vec) => Bytes::from(vec),
            Err(_) => body_bytes,
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

        info!("{method} {path}");

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
                        .map_err(std::io::Error::other)
                        .boxed(),
                )
                .unwrap());
        }

        // 收集所有请求头
        let original_headers: HashMap<String, Box<[u8]>> = req
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_lowercase(), value))
            .filter(|(name, _)| {
                let name = name.as_str();
                name != "host" && name != CONTENT_TYPE && name != CONTENT_LENGTH
            })
            .map(|(name, value)| (name, value.as_bytes().into()))
            .collect();

        let mut body_bytes = match req.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                warn!("Failed to collect request body: {e}");
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "text/plain")
                    .body(
                        Full::from("Failed to read request body")
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .unwrap());
            }
        };

        // 尝试匹配并解析每个协议
        let Some(protocol) = self
            .protocols
            .iter()
            .find(|p| p.matches(&path, content_type.as_deref()))
        else {
            warn!("No matching protocol for path: {path}, content-type: {content_type:?}");
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(CONTENT_TYPE, "text/plain")
                .body(
                    Full::from("No matching protocol")
                        .map_err(std::io::Error::other)
                        .boxed(),
                )
                .unwrap());
        };

        match protocol.parse(body_bytes.clone()) {
            Ok(parsed) => {
                info!("Matched protocol, model: {}", parsed.model);

                // 创建请求上下文
                let context = RequestContext::new(parsed.model.clone(), path.clone());

                // 构建可变请求用于中间件拦截 (使用 Bytes 作为 body 类型)
                let mut req_builder = Request::builder()
                    .method(Method::POST)
                    .uri(format!("/{}", path.trim_start_matches('/')));

                // 重新构建请求头
                for (name, value) in &original_headers {
                    req_builder = req_builder.header(name, &**value);
                }

                let mut intercepted_req = req_builder
                    .body(body_bytes.clone())
                    .unwrap();

                // 调用请求拦截器
                match self.middleware.intercept_request(&mut intercepted_req, &context) {
                    InterceptAction::Block(boxed_body) => {
                        // 中间件阻止了请求，直接返回
                        return Ok(Response::builder()
                            .status(StatusCode::FORBIDDEN)
                            .body(boxed_body)
                            .unwrap());
                    }
                    InterceptAction::Continue => {
                        // 继续处理，使用可能被修改的请求体
                        // 从 intercepted_req 获取可能被修改的 body (Bytes 类型)
                        body_bytes = intercepted_req.into_body();
                    }
                }

                // 查找此模型的后端并尝试故障转移
                self.handle_with_failover(
                    path,
                    body_bytes,
                    &parsed.model,
                    original_headers,
                    protocol.using_x_api_key(),
                )
                .await
            }
            Err(e) => {
                warn!("Failed to parse request: {e}");
                Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "text/plain")
                    .body(
                        Full::from(format!("Invalid request: {e}"))
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .unwrap())
            }
        }
    }

    /// 处理带重试和后端故障转移的请求
    async fn handle_with_failover(
        &self,
        path: String,
        body_bytes: Bytes,
        mut model_name: &str,
        original_headers: HashMap<String, Box<[u8]>>,
        using_x_api_key: bool,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        let mut tried_backends: HashSet<String> = HashSet::new();

        // 解析模型名（处理别名）
        if let Some(alias) = self.aliases.get(model_name) {
            model_name = alias;
        };
        let backends_list = self.router.get(model_name);

        loop {
            // 查找下一个要尝试的后端
            let backend_result = if tried_backends.is_empty() {
                self.find_backend(model_name)
            } else {
                // 尝试查找尚未尝试的后端
                let mut next_backend = None;
                if let Some(backends) = backends_list {
                    for backend_name in backends.iter() {
                        let name = backend_name.as_ref();
                        
                        // 检查是否是负载均衡池
                        let actual_name = if self.load_balancers.contains_key(name) {
                            // 从负载均衡池中选择一个后端
                            self.select_from_load_balancer(name).ok_or_else(|| {
                                std::io::Error::other(format!("Load balancer '{}' returned no backend", name))
                            })?
                        } else {
                            name.to_string()
                        };
                        
                        if !tried_backends.contains(&actual_name)
                            && let Some(backend) = self.backends.get(&actual_name)
                        {
                            next_backend = Some((actual_name, backend));
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
                    warn!("All backends failed for model: {model_name}");
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .header(CONTENT_TYPE, "text/plain")
                        .body(
                            Full::from("All backends unavailable")
                                .map_err(|_| std::io::Error::other("error"))
                                .boxed(),
                        )
                        .unwrap());
                }
            };

            info!(
                "Trying backend: {backend_name} (retry={}, cooldown={:?})",
                backend.config.retry, backend.config.cooldown
            );
            tried_backends.insert(backend_name.clone());

            // 尝试带重试地转发请求
            let params = ForwardRequestParams {
                path: path.clone(),
                body_bytes: body_bytes.clone(),
                backend_name: backend_name.clone(),
                original_model: model_name.to_string(),
                original_headers: original_headers.clone(),
                using_x_api_key,
            };

            match self.forward_with_retry(&params, backend).await {
                Ok(response) => {
                    // 检查响应是否表示错误（5xx 状态）
                    let status = response.status();
                    if status.is_server_error() {
                        warn!("Backend {backend_name} returned server error: {status}");
                        self.handle_backend_failure(backend, &backend_name);
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
                    warn!("Backend {backend_name} failed after retries");
                    self.handle_backend_failure(backend, &backend_name);
                    // 继续循环尝试下一个后端
                    continue;
                }
            }
        }
    }

    /// 处理后端失败：记录失败并检查是否应进入冷却
    fn handle_backend_failure(&self, backend: &Backend, backend_name: &str) {
        if backend.health.record_failure(backend.config.retry) {
            backend.health.set_cooldown(backend.config.cooldown);
            warn!(
                "Backend {backend_name} entered cooldown for {:?}",
                backend.config.cooldown
            );
        }
    }

    /// 带重试逻辑的转发请求
    async fn forward_with_retry(
        &self,
        params: &ForwardRequestParams,
        backend: &Backend,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        for attempt in 0..backend.config.retry {
            if attempt > 0 {
                info!(
                    "Retry {}/{} for backend {}",
                    attempt + 1,
                    backend.config.retry,
                    params.backend_name,
                )
            }

            match self.forward_request(params, backend).await {
                Ok(response) => {
                    return Ok(response);
                }
                Err(e) => {
                    warn!(
                        "Attempt {} failed for backend {}: {e}",
                        attempt + 1,
                        params.backend_name,
                    );
                    // 继续下一次重试
                }
            }
        }

        warn!(
            "Backend {} failed after {} attempts",
            params.backend_name,
            backend.config.retry
        );
        Err(std::io::Error::other("error"))
    }

    async fn forward_request(
        &self,
        params: &ForwardRequestParams,
        backend: &Backend,
    ) -> Result<Response<BoxBody>, std::io::Error> {
        let url = format!("{}{}", backend.config.base_url, params.path);

        // 如果后端配置了自定义 model，则修改请求体
        let modified_body = self.modify_request_body(
            params.body_bytes.clone(),
            backend,
            &params.original_model,
        );

        // 构建转发请求
        let mut req_builder = Request::builder()
            .method(Method::POST)
            .uri(&url)
            .header(CONTENT_TYPE, "application/json");

        let mut original_headers = params.original_headers.clone();
        let mut using_x_api_key = params.using_x_api_key;

        if let Some(api_key) = backend.config.api_key.as_deref() {
            if original_headers.remove("x-api-key").is_some() {
                using_x_api_key = true
            } else if original_headers.remove(AUTHORIZATION.as_str()).is_some() {
                using_x_api_key = false
            }

            if using_x_api_key {
                req_builder = req_builder.header("x-api-key", api_key)
            } else {
                req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {api_key}"))
            }
        }

        // 转发所有原始 headers
        for (name, value) in original_headers {
            req_builder = req_builder.header(&*name, &*value)
        }

        let forward_req = req_builder.body(Full::from(modified_body)).unwrap();

        // 发送请求到后端
        match self.http_client.request(forward_req).await {
            Ok(response) => {
                let (parts, body) = response.into_parts();

                info!(
                    "Backend {} response status: {}",
                    params.backend_name,
                    parts.status
                );

                // 收集响应体以便中间件拦截
                let body_bytes = body.collect().await.unwrap().to_bytes();

                // 构建可变响应 (使用 Bytes 作为 body 类型)
                let mut intercepted_resp = Response::from_parts(parts.clone(), body_bytes);

                // 创建响应上下文
                let context = RequestContext::new(&params.original_model, &params.path)
                    .with_backend(&params.backend_name);

                // 调用响应拦截器
                match self.middleware.intercept_response(&mut intercepted_resp, &context) {
                    InterceptAction::Block(boxed_body) => {
                        // 中间件阻止了响应，返回新响应
                        Ok(Response::builder()
                            .status(StatusCode::OK)
                            .body(boxed_body)
                            .unwrap())
                    }
                    InterceptAction::Continue => {
                        // 使用可能被修改的响应
                        let (parts, bytes_body) = intercepted_resp.into_parts();
                        let boxed_body = Full::from(bytes_body)
                            .map_err(std::io::Error::other)
                            .boxed();
                        Ok(Response::from_parts(parts, boxed_body))
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to connect to backend {}: {e}",
                    params.backend_name
                );
                Err(std::io::Error::other(e))
            }
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
                        .map_err(std::io::Error::other)
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
            .body(Full::from(body).map_err(std::io::Error::other).boxed())
            .unwrap())
    }
}

pub async fn serve(
    config: Config,
    middleware: Arc<dyn Middleware>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port = config.service.port;
    let server = Server::new(config, middleware);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{addr}");

    let server = Arc::new(server);

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        info!("Accepted connection from {remote_addr}");

        let server = Arc::clone(&server);
        tokio::spawn(async move {
            if let Err(e) = server.handle_connection(stream).await {
                warn!("Error handling connection from {remote_addr}: {e}");
            }
        });
    }
}
