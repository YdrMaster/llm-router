use bytes::Bytes;

use super::{ParsedRequest, Protocol, parse_model_from_json};

/// Anthropic 协议处理器
pub struct AnthropicProtocol;

impl Protocol for AnthropicProtocol {
    fn matches(&self, path: &str, content_type: Option<&str>) -> bool {
        // Anthropic messages 端点
        if path.starts_with("/v1/messages") {
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
        let model = parse_model_from_json(body, "Missing 'model' field in Anthropic request")?;
        Ok(ParsedRequest { model })
    }

    fn using_x_api_key(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_valid_path_and_content_type() {
        let protocol = AnthropicProtocol;
        assert!(protocol.matches("/v1/messages", Some("application/json")));
    }

    #[test]
    fn test_matches_wrong_path() {
        let protocol = AnthropicProtocol;
        assert!(!protocol.matches("/chat/completions", Some("application/json")));
        assert!(!protocol.matches("/messages", Some("application/json"))); // 缺少 /v1
    }

    #[test]
    fn test_matches_missing_content_type() {
        let protocol = AnthropicProtocol;
        assert!(!protocol.matches("/v1/messages", None));
    }

    #[test]
    fn test_matches_wrong_content_type() {
        let protocol = AnthropicProtocol;
        assert!(!protocol.matches("/v1/messages", Some("text/plain")));
    }

    #[test]
    fn test_parse_valid_request() {
        let protocol = AnthropicProtocol;
        let body = Bytes::from(r#"{"model": "claude-3", "messages": []}"#);
        let result = protocol.parse(body).unwrap();
        assert_eq!(result.model, "claude-3");
    }

    #[test]
    fn test_parse_missing_model() {
        let protocol = AnthropicProtocol;
        let body = Bytes::from(r#"{"messages": []}"#);
        let result = protocol.parse(body);
        assert!(result.is_err());
    }
}
