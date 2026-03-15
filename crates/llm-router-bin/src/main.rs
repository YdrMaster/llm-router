mod logger;

use llm_router_lib::{
    middleware::{InterceptAction, Middleware, RequestContext},
    serve, Config,
};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Request, Response};
use log::{info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Token 用量统计
#[derive(Debug, Default, Clone)]
struct TokenStats {
    /// 输入 token 总数
    input_tokens: u64,
    /// 输出 token 总数
    output_tokens: u64,
    /// 请求次数
    request_count: u64,
}

// 线程局部的 API key 存储
thread_local! {
    static CURRENT_API_KEY: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// 中间件实现：API key 验证和 token 用量统计
struct ApiKeyMiddleware {
    /// 有效的 API key 列表（从配置加载）
    valid_keys: Arc<Vec<String>>,
    /// Token 用量统计（按 API key 分组）
    stats: Arc<Mutex<HashMap<String, TokenStats>>>,
}

impl ApiKeyMiddleware {
    fn new(valid_keys: Vec<String>) -> Self {
        Self {
            valid_keys: Arc::new(valid_keys),
            stats: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 从请求头中提取 API key
    fn extract_api_key(&self, req: &Request<Bytes>) -> Option<String> {
        // 尝试从 Authorization 头提取
        if let Some(auth) = req.headers().get("Authorization")
            && let Ok(auth_str) = auth.to_str()
                && let Some(key) = auth_str.strip_prefix("Bearer ") {
                    return Some(key.to_string());
                }
        // 尝试从 X-API-Key 头提取
        if let Some(api_key) = req.headers().get("X-API-Key")
            && let Ok(key_str) = api_key.to_str() {
                return Some(key_str.to_string());
            }
        None
    }

    /// 验证 API key 是否有效
    fn is_valid_key(&self, key: &str) -> bool {
        // 如果配置了有效 key 列表，则进行验证
        // 如果列表为空，则允许所有请求（兼容模式）
        if self.valid_keys.is_empty() {
            return true;
        }
        self.valid_keys.iter().any(|k| k == key)
    }

    /// 记录 token 用量
    fn record_usage(&self, api_key: &str, input_tokens: u64, output_tokens: u64) {
        let mut stats = self.stats.lock().unwrap();
        let entry = stats.entry(api_key.to_string()).or_default();
        entry.input_tokens += input_tokens;
        entry.output_tokens += output_tokens;
        entry.request_count += 1;
    }

    /// 从响应体中解析 token 用量
    fn parse_token_usage(&self, body: &Bytes) -> (u64, u64) {
        // 尝试解析 JSON 响应体中的 usage 字段
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body)
            && let Some(usage) = json.get("usage") {
                let input = usage
                    .get("prompt_tokens")
                    .or_else(|| usage.get("input_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output = usage
                    .get("completion_tokens")
                    .or_else(|| usage.get("output_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                return (input, output);
            }
        (0, 0)
    }

    /// 打印统计信息
    fn print_stats(&self) {
        let stats = self.stats.lock().unwrap();
        if stats.is_empty() {
            return;
        }
        info!("=== API Key Usage Statistics ===");
        for (key, stat) in stats.iter() {
            // 隐藏 key 的中间部分
            let masked_key = if key.len() > 8 {
                format!("{}***{}", &key[..4], &key[key.len() - 4..])
            } else {
                key.clone()
            };
            info!(
                "API Key {}: {} requests, {} input tokens, {} output tokens",
                masked_key, stat.request_count, stat.input_tokens, stat.output_tokens
            );
        }
        info!("==================================");
    }
}

impl Middleware for ApiKeyMiddleware {
    fn intercept_request(
        &self,
        req: &mut Request<Bytes>,
        context: &RequestContext,
    ) -> InterceptAction {
        // 提取并验证 API key
        let api_key = match self.extract_api_key(req) {
            Some(key) => key,
            None => {
                warn!(
                    "Blocked request without API key: model={}, path={}",
                    context.model, context.path
                );
                return InterceptAction::Block(
                    http_body_util::Full::from("Missing API key")
                        .map_err(std::io::Error::other)
                        .boxed(),
                );
            }
        };

        // 验证 API key
        if !self.is_valid_key(&api_key) {
            warn!(
                "Blocked request with invalid API key: model={}, path={}",
                context.model, context.path
            );
            return InterceptAction::Block(
                http_body_util::Full::from("Invalid API key")
                    .map_err(std::io::Error::other)
                    .boxed(),
            );
        }

        // API key 有效，继续处理
        info!(
            "Allowed request: model={}, path={}, api_key={}***",
            context.model,
            context.path,
            &api_key[..std::cmp::min(4, api_key.len())]
        );

        // 将 API key 存储到线程局部变量中，以便响应拦截时使用
        CURRENT_API_KEY.with(|k| *k.borrow_mut() = Some(api_key));

        InterceptAction::Continue
    }

    fn intercept_response(
        &self,
        resp: &mut Response<Bytes>,
        context: &RequestContext,
    ) -> InterceptAction {
        // 从线程局部变量中获取 API key
        let api_key = CURRENT_API_KEY.with(|k| {
            k.borrow().clone().unwrap_or_else(|| "unknown".to_string())
        });

        // 解析响应体中的 token 用量
        let body_bytes = resp.body();
        let (input_tokens, output_tokens) = self.parse_token_usage(body_bytes);

        if input_tokens > 0 || output_tokens > 0 {
            self.record_usage(&api_key, input_tokens, output_tokens);

            info!(
                "Token usage: model={}, api_key={}***, input={}, output={}",
                context.model,
                &api_key[..std::cmp::min(4, api_key.len())],
                input_tokens,
                output_tokens
            );
        }

        // 清除线程局部变量
        CURRENT_API_KEY.with(|k| *k.borrow_mut() = None);

        InterceptAction::Continue
    }
}

impl Drop for ApiKeyMiddleware {
    fn drop(&mut self) {
        // 在程序退出时打印统计信息
        self.print_stats();
    }
}

fn main() {
    // 加载配置文件
    let config = Config::load(
        std::env::args()
            .nth(1)
            .as_deref()
            .unwrap_or("config.toml"),
    )
    .expect("Failed to load config");

    // 初始化日志
    logger::init(config.service.log_level);

    // 从环境变量加载有效的 API key 列表
    // 格式：VALID_API_KEYS=key1,key2,key3
    let valid_keys: Vec<String> = std::env::var("VALID_API_KEYS")
        .ok()
        .map(|s| s.split(',').map(|k| k.trim().to_string()).collect())
        .unwrap_or_default();

    if valid_keys.is_empty() {
        info!("No VALID_API_KEYS set, allowing all API keys (compatibility mode)");
    } else {
        info!("Loaded {} valid API keys", valid_keys.len());
    }

    // 创建中间件
    let middleware: Arc<dyn Middleware> = Arc::new(ApiKeyMiddleware::new(valid_keys));

    // 创建 Tokio 运行时并启动服务器
    tokio::runtime::Runtime::new()
        .expect("Failed to create Tokio runtime")
        .block_on(serve(config, middleware))
        .expect("Server encountered an error")
}
