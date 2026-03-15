mod logger;

use llm_router_lib::{
    middleware::{InterceptAction, Middleware, RequestContext},
    serve, Config,
};
use bytes::Bytes;
use hyper::{Request, Response};
use std::sync::Arc;

/// 默认中间件实现，不进行任何拦截
struct DefaultMiddleware;

impl Middleware for DefaultMiddleware {
    fn intercept_request(
        &self,
        _req: &mut Request<Bytes>,
        _context: &RequestContext,
    ) -> InterceptAction {
        // 默认不拦截，继续处理
        InterceptAction::Continue
    }

    fn intercept_response(
        &self,
        _resp: &mut Response<Bytes>,
        _context: &RequestContext,
    ) -> InterceptAction {
        // 默认不拦截，继续处理
        InterceptAction::Continue
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

    // 创建中间件
    let middleware: Arc<dyn Middleware> = Arc::new(DefaultMiddleware);

    // 创建 Tokio 运行时并启动服务器
    tokio::runtime::Runtime::new()
        .expect("Failed to create Tokio runtime")
        .block_on(serve(config, middleware))
        .expect("Server encountered an error")
}
