mod anthropic;
mod openai;

use bytes::Bytes;

pub use anthropic::AnthropicProtocol;
pub use openai::OpenAiProtocol;

/// 请求体解析结果
pub struct ParsedRequest {
    pub model: String,
}

/// 用于 models 列表响应的模型信息
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

/// 协议处理器 trait
pub trait Protocol: Send + Sync {
    /// 检查此协议是否匹配请求路径
    fn matches(&self, path: &str, content_type: Option<&str>) -> bool;

    /// 解析请求体并提取 model 字段
    fn parse(&self, body: Bytes)
    -> Result<ParsedRequest, Box<dyn std::error::Error + Send + Sync>>;
}
