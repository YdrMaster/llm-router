use bytes::Bytes;
use hyper::{Request, Response};

// 重新导出 BoxBody 类型别名
pub type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

/// 请求上下文，包含路由决策相关信息
pub struct RequestContext {
    /// 请求的模型名称
    pub model: String,
    /// 目标后端名称（如果已选择）
    pub backend: Option<String>,
    /// 请求路径
    pub path: String,
}

impl RequestContext {
    pub fn new(model: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            backend: None,
            path: path.into(),
        }
    }

    pub fn with_backend(mut self, backend: impl Into<String>) -> Self {
        self.backend = Some(backend.into());
        self
    }
}

/// 拦截动作
pub enum InterceptAction {
    /// 继续处理（请求/响应已被修改）
    Continue,
    /// 阻止并返回自定义响应
    Block(BoxBody),
}

/// 中间件 trait，用于在请求/响应处理过程中进行拦截和修改
///
/// 所有拦截方法都是同步的，接收 `&mut` 参数以便直接修改请求/响应
pub trait Middleware: Send + Sync {
    /// 请求拦截
    ///
    /// # 参数
    /// - `req`: 可变请求引用，可直接修改请求内容
    /// - `context`: 请求上下文
    ///
    /// # 返回
    /// - `InterceptAction::Continue`: 继续处理（请求已被修改）
    /// - `InterceptAction::Block(resp)`: 阻止请求，直接返回指定响应
    fn intercept_request(
        &self,
        req: &mut Request<Bytes>,
        context: &RequestContext,
    ) -> InterceptAction;

    /// 响应拦截
    ///
    /// # 参数
    /// - `resp`: 可变响应引用，可直接修改响应内容
    /// - `context`: 请求上下文
    ///
    /// # 返回
    /// - `InterceptAction::Continue`: 继续处理（响应已被修改）
    /// - `InterceptAction::Block(resp)`: 阻止响应，返回新的响应
    fn intercept_response(
        &self,
        resp: &mut Response<Bytes>,
        context: &RequestContext,
    ) -> InterceptAction;
}

/// 空中间件实现，不进行任何拦截
pub struct NoOpMiddleware;

impl Middleware for NoOpMiddleware {
    fn intercept_request(
        &self,
        _req: &mut Request<Bytes>,
        _context: &RequestContext,
    ) -> InterceptAction {
        InterceptAction::Continue
    }

    fn intercept_response(
        &self,
        _resp: &mut Response<Bytes>,
        _context: &RequestContext,
    ) -> InterceptAction {
        InterceptAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_context_creation() {
        let ctx = RequestContext::new("test-model", "/v1/messages");
        assert_eq!(ctx.model, "test-model");
        assert_eq!(ctx.path, "/v1/messages");
        assert!(ctx.backend.is_none());

        let ctx = ctx.with_backend("backend1");
        assert_eq!(ctx.backend, Some("backend1".to_string()));
    }

    #[test]
    fn test_no_op_middleware() {
        let middleware = NoOpMiddleware;
        let mut req = Request::builder()
            .uri("/test")
            .body(Bytes::new())
            .unwrap();

        let ctx = RequestContext::new("model", "/test");
        assert!(matches!(
            middleware.intercept_request(&mut req, &ctx),
            InterceptAction::Continue
        ));
    }
}
