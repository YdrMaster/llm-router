//! LLM Router Library
//!
//! 一个可配置的 LLM 请求路由和负载均衡库，支持中间件拦截。
//!
//! # 模块结构
//!
//! - [`config`]: 配置定义和解析
//! - [`health`]: 后端健康检查
//! - [`protocol`]: 协议解析（OpenAI、Anthropic 等）
//! - [`middleware`]: 中间件拦截器 trait
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use llm_router_lib::{Config, serve, middleware::{Middleware, InterceptAction, RequestContext}};
//! use bytes::Bytes;
//! use hyper::{Request, Response};
//! use std::sync::Arc;
//!
//! // 实现自定义中间件
//! struct MyMiddleware;
//!
//! impl Middleware for MyMiddleware {
//!     fn intercept_request(
//!         &self,
//!         req: &mut Request<Bytes>,
//!         context: &RequestContext,
//!     ) -> InterceptAction {
//!         // 可以在这里审计、过滤或修改请求
//!         InterceptAction::Continue
//!     }
//!
//!     fn intercept_response(
//!         &self,
//!         resp: &mut Response<Bytes>,
//!         context: &RequestContext,
//!     ) -> InterceptAction {
//!         // 可以在这里审计、过滤或修改响应
//!         InterceptAction::Continue
//!     }
//! }
//!
//! fn main() {
//!     let config = Config::load("config.toml").unwrap();
//!     let middleware: Arc<dyn Middleware> = Arc::new(MyMiddleware);
//!     tokio::runtime::Runtime::new().unwrap()
//!         .block_on(serve(config, middleware))
//!         .unwrap();
//! }
//! ```

pub mod config;
pub mod health;
pub mod middleware;
pub mod protocol;
mod serve;

// 重新导出常用类型
pub use config::Config;
pub use middleware::{BoxBody, InterceptAction, Middleware, RequestContext};
pub use serve::serve;
