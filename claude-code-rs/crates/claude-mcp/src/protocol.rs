//! JSON-RPC 2.0 message types for the MCP protocol.
//!
//! These types handle serialization/deserialization of JSON-RPC 2.0 messages
//! used by MCP over both stdio and SSE transports.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON-RPC 2.0 types ──────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: u64, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard JSON-RPC 2.0 error codes.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A JSON-RPC 2.0 notification (request without id, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC message that can be either a request, response, or notification.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Response(JsonRpcResponse),
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

impl<'de> serde::Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let obj = value.as_object().ok_or_else(|| {
            serde::de::Error::custom("JSON-RPC message must be an object")
        })?;

        // Response: has "result" or "error"
        if obj.contains_key("result") || obj.contains_key("error") {
            return serde_json::from_value(value.clone())
                .map(JsonRpcMessage::Response)
                .map_err(serde::de::Error::custom);
        }
        // Request: has "method" and "id"
        if obj.contains_key("method") && obj.contains_key("id") {
            return serde_json::from_value(value.clone())
                .map(JsonRpcMessage::Request)
                .map_err(serde::de::Error::custom);
        }
        // Notification: has "method" but no "id"
        if obj.contains_key("method") {
            return serde_json::from_value(value.clone())
                .map(JsonRpcMessage::Notification)
                .map_err(serde::de::Error::custom);
        }

        Err(serde::de::Error::custom(
            "Cannot determine JSON-RPC message type",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_new() {
        let req = JsonRpcRequest::new(1, "initialize", Some(serde_json::json!({"capabilities": {}})));
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id, 1);
        assert_eq!(req.method, "initialize");
    }

    #[test]
    fn response_success() {
        let resp = JsonRpcResponse::success(1, serde_json::json!({"ok": true}));
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn response_error() {
        let resp = JsonRpcResponse::error(2, error_codes::METHOD_NOT_FOUND, "Method not found");
        assert_eq!(resp.id, Some(2));
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
    }

    #[test]
    fn notification_new() {
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        assert_eq!(notif.jsonrpc, "2.0");
        assert_eq!(notif.method, "notifications/initialized");
        assert!(notif.params.is_none());
    }

    #[test]
    fn request_serialization() {
        let req = JsonRpcRequest::new(1, "initialize", Some(serde_json::json!({"capabilities": {}})));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn response_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"tools":{}}}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(resp) => {
                assert_eq!(resp.id, Some(1));
                assert!(resp.result.is_some());
                assert!(resp.error.is_none());
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn error_response_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(resp) => {
                assert_eq!(resp.id, Some(2));
                let err = resp.error.unwrap();
                assert_eq!(err.code, -32601);
                assert_eq!(err.message, "Method not found");
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn notification_deserialization() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":50}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Notification(notif) => {
                assert_eq!(notif.method, "notifications/progress");
            }
            _ => panic!("Expected Notification"),
        }
    }

    #[test]
    fn request_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"test"}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Request(req) => {
                assert_eq!(req.id, 5);
                assert_eq!(req.method, "tools/call");
            }
            _ => panic!("Expected Request"),
        }
    }

    #[test]
    fn invalid_message_fails() {
        let json = r#"{"jsonrpc":"2.0"}"#;
        let result: Result<JsonRpcMessage, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn non_object_fails() {
        let result: Result<JsonRpcMessage, _> = serde_json::from_str("42");
        assert!(result.is_err());
    }

    #[test]
    fn error_codes_values() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }
}
