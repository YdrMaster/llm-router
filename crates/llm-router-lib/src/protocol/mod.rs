mod anthropic;
mod openai;

use bytes::Bytes;

pub use anthropic::AnthropicProtocol;
pub use openai::OpenAiProtocol;

/// 请求体解析结果
pub struct ParsedRequest {
    pub(super) model: String,
}

/// 用于 models 列表响应的模型信息
pub struct ModelInfo {
    pub(super) id: String,
    pub(super) object: String,
    pub(super) created: u64,
    pub(super) owned_by: String,
}

/// 从 JSON body 解析 model 字段的通用函数
fn parse_model_from_json(
    body: Bytes,
    error_msg: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    let model = json
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(error_msg)?
        .to_string();
    Ok(model)
}

/// 协议处理器 trait
pub trait Protocol: Send + Sync {
    /// 检查此协议是否匹配请求路径
    fn matches(&self, path: &str, content_type: Option<&str>) -> bool;

    /// 解析请求体并提取 model 字段
    fn parse(&self, body: Bytes)
    -> Result<ParsedRequest, Box<dyn std::error::Error + Send + Sync>>;

    fn using_x_api_key(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_from_json_valid() {
        let body = Bytes::from(r#"{"model": "test-model"}"#);
        let result = parse_model_from_json(body, "error").unwrap();
        assert_eq!(result, "test-model");
    }

    #[test]
    fn test_parse_model_from_json_missing_model() {
        let body = Bytes::from(r#"{"messages": []}"#);
        let result = parse_model_from_json(body, "missing model");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing model"));
    }

    #[test]
    fn test_parse_model_from_json_invalid_json() {
        let body = Bytes::from("not valid json");
        let result = parse_model_from_json(body, "error");
        assert!(result.is_err());
    }

    #[test]
    fn test_model_info_serialization() {
        let info = ModelInfo {
            id: "test-model".to_string(),
            object: "model".to_string(),
            created: 1234567890,
            owned_by: "test-org".to_string(),
        };

        // 确保可以序列化
        let json = serde_json::json!({
            "id": info.id,
            "object": info.object,
            "created": info.created,
            "owned_by": info.owned_by
        });

        assert_eq!(json["id"], "test-model");
        assert_eq!(json["object"], "model");
        assert_eq!(json["created"], 1234567890);
        assert_eq!(json["owned_by"], "test-org");
    }
}
