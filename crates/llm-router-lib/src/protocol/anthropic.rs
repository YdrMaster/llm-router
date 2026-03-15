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

    #[test]
    fn test_matches_path_variations() {
        let protocol = AnthropicProtocol;
        
        // 有效路径
        assert!(protocol.matches("/v1/messages", Some("application/json")));
        assert!(protocol.matches("/v1/messages/some-subpath", Some("application/json")));
        
        // 无效路径
        assert!(!protocol.matches("/messages", Some("application/json"))); // 缺少 /v1
        assert!(!protocol.matches("/api/messages", Some("application/json")));
        assert!(!protocol.matches("/chat/completions", Some("application/json")));
    }

    #[test]
    fn test_matches_content_type_variations() {
        let protocol = AnthropicProtocol;
        
        // 有效 content-type
        assert!(protocol.matches("/v1/messages", Some("application/json")));
        assert!(protocol.matches("/v1/messages", Some("application/json; charset=utf-8")));
        
        // 无效 content-type - 注意：实际代码对大小写敏感
        assert!(!protocol.matches("/v1/messages", Some("text/plain")));
        assert!(!protocol.matches("/v1/messages", Some("application/xml")));
        assert!(!protocol.matches("/v1/messages", None));
        assert!(!protocol.matches("/v1/messages", Some("APPLICATION/JSON"))); // 大小写敏感
    }

    #[test]
    fn test_parse_with_extra_fields() {
        let protocol = AnthropicProtocol;
        let body = Bytes::from(r#"{"model": "claude-3-opus", "messages": [], "max_tokens": 1000, "temperature": 0.7}"#);
        let result = protocol.parse(body).unwrap();
        assert_eq!(result.model, "claude-3-opus");
    }

    #[test]
    fn test_parse_model_with_special_chars() {
        let protocol = AnthropicProtocol;
        let body = Bytes::from(r#"{"model": "claude-3-5-sonnet-20241022", "messages": []}"#);
        let result = protocol.parse(body).unwrap();
        assert_eq!(result.model, "claude-3-5-sonnet-20241022");
    }

    #[test]
    fn test_parse_invalid_json() {
        let protocol = AnthropicProtocol;
        let body = Bytes::from("not valid json");
        let result = protocol.parse(body);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_body() {
        let protocol = AnthropicProtocol;
        let body = Bytes::from("{}");
        let result = protocol.parse(body);
        assert!(result.is_err());
    }

    #[test]
    fn test_using_x_api_key() {
        let protocol = AnthropicProtocol;
        assert!(protocol.using_x_api_key());
    }
}
