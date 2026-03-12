use bytes::Bytes;

use super::{ParsedRequest, Protocol};

/// Anthropic 协议处理器
pub struct AnthropicProtocol;

impl Protocol for AnthropicProtocol {
    fn matches(&self, path: &str, content_type: Option<&str>) -> bool {
        // Anthropic messages 端点
        if path.contains("/v1/messages") {
            return match content_type {
                Some(ct) => ct.contains("application/json"),
                None => false,
            };
        }
        false
    }

    fn parse(
        &self,
        body: Bytes,
    ) -> Result<ParsedRequest, Box<dyn std::error::Error + Send + Sync>> {
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'model' field in Anthropic request")?
            .to_string();
        Ok(ParsedRequest { model })
    }
}
