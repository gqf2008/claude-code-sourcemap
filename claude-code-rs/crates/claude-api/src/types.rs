use serde::{Deserialize, Serialize};

// ── Request types ──

#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Extended thinking (chain of thought) configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
}

impl Default for MessagesRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: 4096,
            messages: Vec::new(),
            system: None,
            tools: None,
            stream: false,
            stop_sequences: None,
            temperature: None,
            top_p: None,
            thinking: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
    /// Optional TTL hint: `"5m"` (default) or `"1h"` (for eligible users/orgs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// Optional scope: `"global"` for org-wide cache sharing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl CacheControl {
    /// Standard ephemeral cache control (no TTL/scope).
    pub fn ephemeral() -> Self {
        Self {
            control_type: "ephemeral".into(),
            ttl: None,
            scope: None,
        }
    }

    /// Ephemeral with global scope (for org-wide cache sharing).
    pub fn ephemeral_global() -> Self {
        Self {
            control_type: "ephemeral".into(),
            ttl: None,
            scope: Some("global".into()),
        }
    }

    /// Ephemeral with 1-hour TTL and global scope (for eligible users).
    pub fn ephemeral_1h_global() -> Self {
        Self {
            control_type: "ephemeral".into(),
            ttl: Some("1h".into()),
            scope: Some("global".into()),
        }
    }
}

/// Extended thinking configuration — enables chain-of-thought reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// "enabled" to turn on extended thinking.
    #[serde(rename = "type")]
    pub thinking_type: String,
    /// Token budget for thinking (e.g. 10000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub role: String,
    pub content: Vec<ApiContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ApiContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ── Response types ──

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub content: Vec<ResponseContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: ApiUsage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

// ── SSE Stream events ──

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessagesResponse },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: ResponseContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: DeltaBlock },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDeltaData,
        usage: Option<DeltaUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: ApiError },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum DeltaBlock {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageDeltaData {
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeltaUsage {
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── SystemBlock ─────────────────────────────────────────────────────

    #[test]
    fn system_block_serde() {
        let block = SystemBlock {
            block_type: "text".into(),
            text: "You are helpful.".into(),
            cache_control: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "You are helpful.");
        // "block_type" should NOT appear — it's renamed to "type"
        assert!(json.get("block_type").is_none());

        // Roundtrip
        let back: SystemBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back.block_type, "text");
        assert_eq!(back.text, "You are helpful.");
    }

    // ── ThinkingConfig ──────────────────────────────────────────────────

    #[test]
    fn thinking_config_serde() {
        let cfg = ThinkingConfig {
            thinking_type: "enabled".into(),
            budget_tokens: Some(10000),
        };
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["type"], "enabled");
        assert_eq!(json["budget_tokens"], 10000);
        assert!(json.get("thinking_type").is_none());

        let back: ThinkingConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.thinking_type, "enabled");
        assert_eq!(back.budget_tokens, Some(10000));
    }

    // ── ApiContentBlock ─────────────────────────────────────────────────

    #[test]
    fn api_content_text_serde() {
        let block = ApiContentBlock::Text {
            text: "Hello".into(),
            cache_control: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "Hello");

        let back: ApiContentBlock = serde_json::from_value(json).unwrap();
        match back {
            ApiContentBlock::Text { text, .. } => assert_eq!(text, "Hello"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn api_content_tool_use_serde() {
        let block = ApiContentBlock::ToolUse {
            id: "tu_123".into(),
            name: "read_file".into(),
            input: json!({"path": "/foo.txt"}),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "tu_123");
        assert_eq!(json["name"], "read_file");

        let back: ApiContentBlock = serde_json::from_value(json).unwrap();
        match back {
            ApiContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_123");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "/foo.txt");
            }
            _ => panic!("Expected ToolUse variant"),
        }
    }

    #[test]
    fn api_content_tool_result_serde() {
        let block = ApiContentBlock::ToolResult {
            tool_use_id: "tu_123".into(),
            content: vec![ToolResultContent::Text { text: "file contents".into() }],
            is_error: false,
            cache_control: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "tu_123");
        assert_eq!(json["is_error"], false);

        let back: ApiContentBlock = serde_json::from_value(json).unwrap();
        match back {
            ApiContentBlock::ToolResult { tool_use_id, content, is_error, .. } => {
                assert_eq!(tool_use_id, "tu_123");
                assert!(!is_error);
                assert_eq!(content.len(), 1);
            }
            _ => panic!("Expected ToolResult variant"),
        }
    }

    // ── StreamEvent ─────────────────────────────────────────────────────

    #[test]
    fn stream_event_message_start() {
        let json = json!({
            "type": "message_start",
            "message": {
                "id": "msg_01",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-sonnet-4-20250514",
                "stop_reason": null,
                "usage": { "input_tokens": 10, "output_tokens": 0 }
            }
        });
        let event: StreamEvent = serde_json::from_value(json).unwrap();
        match event {
            StreamEvent::MessageStart { message } => {
                assert_eq!(message.id, "msg_01");
                assert_eq!(message.role, "assistant");
            }
            _ => panic!("Expected MessageStart"),
        }
    }

    #[test]
    fn stream_event_content_block_delta_text() {
        let json = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "Hello" }
        });
        let event: StreamEvent = serde_json::from_value(json).unwrap();
        match event {
            StreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(index, 0);
                match delta {
                    DeltaBlock::TextDelta { text } => assert_eq!(text, "Hello"),
                    _ => panic!("Expected TextDelta"),
                }
            }
            _ => panic!("Expected ContentBlockDelta"),
        }
    }

    #[test]
    fn stream_event_content_block_delta_json() {
        let json = json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": { "type": "input_json_delta", "partial_json": "{\"path\":" }
        });
        let event: StreamEvent = serde_json::from_value(json).unwrap();
        match event {
            StreamEvent::ContentBlockDelta { delta, .. } => {
                match delta {
                    DeltaBlock::InputJsonDelta { partial_json } => {
                        assert_eq!(partial_json, "{\"path\":");
                    }
                    _ => panic!("Expected InputJsonDelta"),
                }
            }
            _ => panic!("Expected ContentBlockDelta"),
        }
    }

    #[test]
    fn stream_event_ping() {
        let json = json!({"type": "ping"});
        let event: StreamEvent = serde_json::from_value(json).unwrap();
        assert!(matches!(event, StreamEvent::Ping));
    }

    #[test]
    fn stream_event_error() {
        let json = json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" }
        });
        let event: StreamEvent = serde_json::from_value(json).unwrap();
        match event {
            StreamEvent::Error { error } => {
                assert_eq!(error.error_type, "overloaded_error");
                assert_eq!(error.message, "Overloaded");
            }
            _ => panic!("Expected Error"),
        }
    }

    // ── DeltaBlock ──────────────────────────────────────────────────────

    #[test]
    fn delta_block_text_delta() {
        let json = json!({"type": "text_delta", "text": "world"});
        let delta: DeltaBlock = serde_json::from_value(json).unwrap();
        match delta {
            DeltaBlock::TextDelta { text } => assert_eq!(text, "world"),
            _ => panic!("Expected TextDelta"),
        }
    }

    // ── ApiUsage ────────────────────────────────────────────────────────

    #[test]
    fn api_usage_defaults() {
        // Optional cache fields should default to None when absent
        let json = json!({"input_tokens": 42, "output_tokens": 7});
        let usage: ApiUsage = serde_json::from_value(json).unwrap();
        assert_eq!(usage.input_tokens, 42);
        assert_eq!(usage.output_tokens, 7);
        assert!(usage.cache_creation_input_tokens.is_none());
        assert!(usage.cache_read_input_tokens.is_none());
    }

    // ── CacheControl ────────────────────────────────────────────────────

    #[test]
    fn cache_control_ephemeral_serialization() {
        let cc = CacheControl::ephemeral();
        let json = serde_json::to_value(&cc).unwrap();
        assert_eq!(json, json!({"type": "ephemeral"}));
        // No ttl or scope should appear
        assert!(json.get("ttl").is_none());
        assert!(json.get("scope").is_none());
    }

    #[test]
    fn cache_control_ephemeral_global_serialization() {
        let cc = CacheControl::ephemeral_global();
        let json = serde_json::to_value(&cc).unwrap();
        assert_eq!(json, json!({"type": "ephemeral", "scope": "global"}));
    }

    #[test]
    fn cache_control_1h_global_serialization() {
        let cc = CacheControl::ephemeral_1h_global();
        let json = serde_json::to_value(&cc).unwrap();
        assert_eq!(json, json!({"type": "ephemeral", "ttl": "1h", "scope": "global"}));
    }

    #[test]
    fn cache_control_deserialization_with_extra_fields() {
        let json = json!({"type": "ephemeral", "ttl": "5m", "scope": "org"});
        let cc: CacheControl = serde_json::from_value(json).unwrap();
        assert_eq!(cc.control_type, "ephemeral");
        assert_eq!(cc.ttl.as_deref(), Some("5m"));
        assert_eq!(cc.scope.as_deref(), Some("org"));
    }

    #[test]
    fn cache_control_deserialization_minimal() {
        let json = json!({"type": "ephemeral"});
        let cc: CacheControl = serde_json::from_value(json).unwrap();
        assert_eq!(cc.control_type, "ephemeral");
        assert!(cc.ttl.is_none());
        assert!(cc.scope.is_none());
    }

    #[test]
    fn system_block_with_cache_control_roundtrip() {
        let block = SystemBlock {
            block_type: "text".into(),
            text: "Hello".into(),
            cache_control: Some(CacheControl::ephemeral_global()),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["cache_control"]["type"], "ephemeral");
        assert_eq!(json["cache_control"]["scope"], "global");
        // Roundtrip
        let back: SystemBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back.cache_control.unwrap().scope.as_deref(), Some("global"));
    }
}
