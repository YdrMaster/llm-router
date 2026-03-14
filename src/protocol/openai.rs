use bytes::Bytes;

use super::{ModelInfo, ParsedRequest, Protocol, parse_model_from_json};

/// OpenAI 协议处理器
pub struct OpenAiProtocol;

impl OpenAiProtocol {
    /// 生成 OpenAI 风格的模型列表响应
    pub fn list_models(models: &[ModelInfo]) -> String {
        let data: Vec<serde_json::Value> = models
            .iter()
            .map(|m| {
                let ModelInfo {
                    id,
                    object,
                    created,
                    owned_by,
                } = m;
                serde_json::json!({
                    "id": id,
                    "object": object,
                    "created": created,
                    "owned_by": owned_by
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
        if path.starts_with("/chat/completions") {
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
        let model = parse_model_from_json(body, "Missing 'model' field in OpenAI request")?;
        Ok(ParsedRequest { model })
    }

    fn using_x_api_key(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_valid_path_and_content_type() {
        let protocol = OpenAiProtocol;
        assert!(protocol.matches("/chat/completions", Some("application/json")));
        assert!(protocol.matches("/chat/completions/some-subpath", Some("application/json")));
    }

    #[test]
    fn test_matches_wrong_path() {
        let protocol = OpenAiProtocol;
        assert!(!protocol.matches("/completions", Some("application/json")));
        assert!(!protocol.matches("/messages", Some("application/json")));
        assert!(!protocol.matches("/v1/messages", Some("application/json")));
    }

    #[test]
    fn test_matches_missing_content_type() {
        let protocol = OpenAiProtocol;
        assert!(!protocol.matches("/chat/completions", None));
    }

    #[test]
    fn test_matches_wrong_content_type() {
        let protocol = OpenAiProtocol;
        assert!(!protocol.matches("/chat/completions", Some("text/plain")));
    }

    #[test]
    fn test_parse_valid_request() {
        let protocol = OpenAiProtocol;
        let body = Bytes::from(r#"{"model": "gpt-4", "messages": []}"#);
        let result = protocol.parse(body).unwrap();
        assert_eq!(result.model, "gpt-4");
    }

    #[test]
    fn test_parse_missing_model() {
        let protocol = OpenAiProtocol;
        let body = Bytes::from(r#"{"messages": []}"#);
        let result = protocol.parse(body);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_models() {
        let models = vec![
            ModelInfo {
                id: "model-1".to_string(),
                object: "model".to_string(),
                created: 1000,
                owned_by: "org-1".to_string(),
            },
            ModelInfo {
                id: "model-2".to_string(),
                object: "model".to_string(),
                created: 2000,
                owned_by: "org-2".to_string(),
            },
        ];

        let result = OpenAiProtocol::list_models(&models);
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();

        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 2);
        assert_eq!(json["data"][0]["id"], "model-1");
        assert_eq!(json["data"][1]["id"], "model-2");
    }
}
