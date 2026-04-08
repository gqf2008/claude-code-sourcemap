//! JSON-RPC 2.0 protocol types.
//!
//! Implements the [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification)
//! with typed request/response/notification/error types.
//!
//! # Wire format examples
//!
//! ```json
//! // Request
//! {"jsonrpc":"2.0","id":1,"method":"agent.submit","params":{"text":"hello"}}
//!
//! // Success response
//! {"jsonrpc":"2.0","id":1,"result":{"ok":true}}
//!
//! // Error response
//! {"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid request"}}
//!
//! // Notification (no id, no response expected)
//! {"jsonrpc":"2.0","method":"agent.textDelta","params":{"text":"Hello"}}
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC version string — always "2.0".
pub const JSONRPC_VERSION: &str = "2.0";

// ── Request ──────────────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request message.
///
/// Sent from client to server. The server MUST reply with a `Response`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version — MUST be "2.0".
    pub jsonrpc: String,

    /// Unique request identifier (number or string).
    pub id: RequestId,

    /// Method name to invoke.
    pub method: String,

    /// Method parameters (positional or named). May be omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Create a new request with the given method and params.
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

// ── Response ─────────────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 response message.
///
/// Sent from server to client in reply to a `Request`. Contains either
/// a `result` (success) or an `error` (failure), never both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version — MUST be "2.0".
    pub jsonrpc: String,

    /// Must match the `id` of the corresponding request.
    pub id: RequestId,

    /// Success result (present on success, absent on error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Error object (present on error, absent on success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Create a success response.
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    pub fn error(id: RequestId, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

// ── Notification ─────────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 notification message.
///
/// Sent in either direction without an `id`. No response is expected.
/// Used for server → client event streaming (agent notifications).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Protocol version — MUST be "2.0".
    pub jsonrpc: String,

    /// Method/event name.
    pub method: String,

    /// Event parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Create a new notification.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            method: method.into(),
            params,
        }
    }
}

// ── Error ────────────────────────────────────────────────────────────────────

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Integer error code.
    pub code: i32,
    /// Short human-readable description.
    pub message: String,
    /// Additional error data (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Standard JSON-RPC 2.0 error codes.
pub mod error_codes {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i32 = -32603;

    // ── Application-specific codes (–32000 to –32099) ────────────────────

    /// Agent is busy processing another request.
    pub const AGENT_BUSY: i32 = -32001;
    /// Session does not exist.
    pub const SESSION_NOT_FOUND: i32 = -32002;
    /// Permission denied by the user.
    pub const PERMISSION_DENIED: i32 = -32003;
    /// Context window exceeded.
    pub const CONTEXT_OVERFLOW: i32 = -32004;
    /// API provider returned an error.
    pub const API_ERROR: i32 = -32005;
}

// ── Request ID ───────────────────────────────────────────────────────────────

/// JSON-RPC request identifier — can be a number or string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        Self::Number(n)
    }
}

impl From<i32> for RequestId {
    fn from(n: i32) -> Self {
        Self::Number(n as i64)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{}", n),
            Self::String(s) => write!(f, "{}", s),
        }
    }
}

// ── Wire message (union type for parsing) ────────────────────────────────────

/// A raw JSON-RPC message that hasn't been classified yet.
///
/// Used during deserialization to determine whether the incoming message
/// is a Request, Response, or Notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawMessage {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Classified message type.
#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl RawMessage {
    /// Classify a raw message into a typed `Message`.
    pub fn classify(self) -> Result<Message, RpcError> {
        if self.jsonrpc != JSONRPC_VERSION {
            return Err(RpcError::new(
                error_codes::INVALID_REQUEST,
                format!("Expected jsonrpc \"2.0\", got \"{}\"", self.jsonrpc),
            ));
        }

        match (self.id, self.method.as_deref(), self.result.is_some() || self.error.is_some()) {
            // Has id + method → Request
            (Some(id), Some(method), false) => Ok(Message::Request(Request {
                jsonrpc: self.jsonrpc,
                id,
                method: method.to_string(),
                params: self.params,
            })),

            // Has id + result/error → Response
            (Some(id), _, true) => Ok(Message::Response(Response {
                jsonrpc: self.jsonrpc,
                id,
                result: self.result,
                error: self.error,
            })),

            // No id + method → Notification
            (None, Some(method), false) => Ok(Message::Notification(Notification {
                jsonrpc: self.jsonrpc,
                method: method.to_string(),
                params: self.params,
            })),

            // Ambiguous or invalid
            _ => Err(RpcError::new(
                error_codes::INVALID_REQUEST,
                "Cannot classify message: must be request (id+method), response (id+result/error), or notification (method only)",
            )),
        }
    }
}

impl From<Request> for RawMessage {
    fn from(r: Request) -> Self {
        Self {
            jsonrpc: r.jsonrpc,
            id: Some(r.id),
            method: Some(r.method),
            params: r.params,
            result: None,
            error: None,
        }
    }
}

impl From<Response> for RawMessage {
    fn from(r: Response) -> Self {
        Self {
            jsonrpc: r.jsonrpc,
            id: Some(r.id),
            method: None,
            params: None,
            result: r.result,
            error: r.error,
        }
    }
}

impl From<Notification> for RawMessage {
    fn from(n: Notification) -> Self {
        Self {
            jsonrpc: n.jsonrpc,
            id: None,
            method: Some(n.method),
            params: n.params,
            result: None,
            error: None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::new(1, "agent.submit", Some(serde_json::json!({"text": "hello"})));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"agent.submit\""));

        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, RequestId::Number(1));
        assert_eq!(back.method, "agent.submit");
    }

    #[test]
    fn request_with_string_id() {
        let req = Request::new("req-abc", "agent.abort", None);
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, RequestId::String("req-abc".into()));
        assert!(back.params.is_none());
    }

    #[test]
    fn response_success_roundtrip() {
        let resp = Response::success(RequestId::Number(42), serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));

        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, RequestId::Number(42));
        assert!(back.result.is_some());
        assert!(back.error.is_none());
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = Response::error(
            RequestId::Number(1),
            RpcError::new(error_codes::METHOD_NOT_FOUND, "Method not found"),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(!json.contains("\"result\""));

        let back: Response = serde_json::from_str(&json).unwrap();
        assert!(back.error.is_some());
        let err = back.error.unwrap();
        assert_eq!(err.code, -32601);
    }

    #[test]
    fn notification_roundtrip() {
        let notif = Notification::new("agent.textDelta", Some(serde_json::json!({"text": "hi"})));
        let json = serde_json::to_string(&notif).unwrap();
        assert!(!json.contains("\"id\""));
        assert!(json.contains("\"method\":\"agent.textDelta\""));

        let back: Notification = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "agent.textDelta");
    }

    #[test]
    fn notification_no_params() {
        let notif = Notification::new("agent.historyCleared", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(!json.contains("\"params\""));
    }

    #[test]
    fn rpc_error_display() {
        let err = RpcError::new(error_codes::PARSE_ERROR, "Parse error");
        assert_eq!(err.to_string(), "[-32700] Parse error");
    }

    #[test]
    fn rpc_error_with_data() {
        let err = RpcError::new(-32600, "Bad request")
            .with_data(serde_json::json!({"field": "method"}));
        assert!(err.data.is_some());
    }

    #[test]
    fn request_id_from_types() {
        assert_eq!(RequestId::from(42_i32), RequestId::Number(42));
        assert_eq!(RequestId::from(42_i64), RequestId::Number(42));
        assert_eq!(RequestId::from("abc"), RequestId::String("abc".into()));
        assert_eq!(RequestId::from("abc".to_string()), RequestId::String("abc".into()));
    }

    #[test]
    fn request_id_display() {
        assert_eq!(RequestId::Number(42).to_string(), "42");
        assert_eq!(RequestId::String("abc".into()).to_string(), "abc");
    }

    #[test]
    fn raw_message_classify_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"agent.submit","params":{"text":"hi"}}"#;
        let raw: RawMessage = serde_json::from_str(json).unwrap();
        let msg = raw.classify().unwrap();
        assert!(matches!(msg, Message::Request(r) if r.method == "agent.submit"));
    }

    #[test]
    fn raw_message_classify_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let raw: RawMessage = serde_json::from_str(json).unwrap();
        let msg = raw.classify().unwrap();
        assert!(matches!(msg, Message::Response(r) if r.result.is_some()));
    }

    #[test]
    fn raw_message_classify_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"agent.textDelta","params":{"text":"hi"}}"#;
        let raw: RawMessage = serde_json::from_str(json).unwrap();
        let msg = raw.classify().unwrap();
        assert!(matches!(msg, Message::Notification(n) if n.method == "agent.textDelta"));
    }

    #[test]
    fn raw_message_classify_error_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Not found"}}"#;
        let raw: RawMessage = serde_json::from_str(json).unwrap();
        let msg = raw.classify().unwrap();
        assert!(matches!(msg, Message::Response(r) if r.error.is_some()));
    }

    #[test]
    fn raw_message_reject_bad_version() {
        let json = r#"{"jsonrpc":"1.0","id":1,"method":"test"}"#;
        let raw: RawMessage = serde_json::from_str(json).unwrap();
        let err = raw.classify().unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
    }

    #[test]
    fn raw_from_request() {
        let req = Request::new(1, "test", None);
        let raw = RawMessage::from(req);
        assert!(raw.id.is_some());
        assert!(raw.method.is_some());
    }

    #[test]
    fn raw_from_response() {
        let resp = Response::success(1.into(), serde_json::json!(null));
        let raw = RawMessage::from(resp);
        assert!(raw.id.is_some());
        assert!(raw.result.is_some());
    }

    #[test]
    fn raw_from_notification() {
        let notif = Notification::new("test", None);
        let raw = RawMessage::from(notif);
        assert!(raw.id.is_none());
        assert!(raw.method.is_some());
    }

    #[test]
    fn error_codes_values() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
        assert_eq!(error_codes::AGENT_BUSY, -32001);
        assert_eq!(error_codes::SESSION_NOT_FOUND, -32002);
        assert_eq!(error_codes::PERMISSION_DENIED, -32003);
    }
}
