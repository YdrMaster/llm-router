use bytes::Bytes;

use super::{ModelInfo, ParsedRequest, Protocol};

/// OpenAI 协议处理器
pub struct OpenAiProtocol;

impl OpenAiProtocol {
    /// 生成 OpenAI 风格的模型列表响应
    pub fn list_models(models: &[ModelInfo]) -> String {
        let data: Vec<serde_json::Value> = models
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "object": m.object,
                    "created": m.created,
                    "owned_by": m.owned_by
                })
            })
            .collect();

        serde_json::json!({
            "object": "list",
            "data": data
        })
        .to_string()
    }
}

impl Protocol for OpenAiProtocol {
    fn matches(&self, path: &str, content_type: Option<&str>) -> bool {
        // OpenAI chat completions 端点
        if path.contains("/chat/completions") {
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
            .ok_or("Missing 'model' field in OpenAI request")?
            .to_string();
        Ok(ParsedRequest { model })
    }
}
