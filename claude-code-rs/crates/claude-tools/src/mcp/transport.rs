//! MCP stdio transport — JSON-RPC 2.0 over stdin/stdout.
//!
//! Spawns a child process and communicates via newline-delimited JSON-RPC
//! messages on stdin/stdout.  Aligned with the MCP specification.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};

// ── JSON-RPC 2.0 types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
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

// ── Stdio Transport ──────────────────────────────────────────────────────────

/// Manages a child process and communicates via newline-delimited JSON-RPC.
pub struct StdioTransport {
    child: Child,
    stdin: BufWriter<tokio::process::ChildStdin>,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: AtomicU64,
}

impl StdioTransport {
    /// Spawn a child process for MCP communication.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Merge environment variables
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server: {} {}", command, args.join(" ")))?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to capture stdin of MCP server")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to capture stdout of MCP server")?;

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a JSON-RPC request and wait for the corresponding response.
    pub async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        // Write request as a single JSON line
        let line = serde_json::to_string(&request)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

        // Read responses until we find one with matching id
        loop {
            let msg = self.read_message().await?;
            match msg {
                JsonRpcMessage::Response(resp) if resp.id == Some(id) => {
                    if let Some(error) = resp.error {
                        anyhow::bail!(
                            "MCP error {}: {} {}",
                            error.code,
                            error.message,
                            error.data.map(|d| d.to_string()).unwrap_or_default()
                        );
                    }
                    return Ok(resp.result.unwrap_or(Value::Null));
                }
                JsonRpcMessage::Notification(_) => {
                    // Notifications are fire-and-forget; skip them while waiting
                    continue;
                }
                _ => {
                    // Response with non-matching id or unexpected request — skip
                    continue;
                }
            }
        }
    }

    /// Send a notification (no response expected).
    pub async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        };
        let line = serde_json::to_string(&notification)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Read one JSON-RPC message from stdout.
    async fn read_message(&mut self) -> Result<JsonRpcMessage> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = self.stdout.read_line(&mut line).await?;
            if bytes_read == 0 {
                anyhow::bail!("MCP server closed stdout (EOF)");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue; // skip blank lines
            }
            let msg: JsonRpcMessage = serde_json::from_str(trimmed)
                .with_context(|| format!("Invalid JSON-RPC from MCP server: {}", trimmed))?;
            return Ok(msg);
        }
    }

    /// Gracefully close the transport and kill the child process.
    pub async fn close(&mut self) -> Result<()> {
        // Try to kill the child process
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        Ok(())
    }

    /// Check if the child process is still running.
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Best-effort kill on drop (non-async)
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "initialize".to_string(),
            params: Some(serde_json::json!({"capabilities": {}})),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_jsonrpc_response_deserialization() {
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
    fn test_jsonrpc_error_response() {
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
    fn test_jsonrpc_notification_deserialization() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":50}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Notification(notif) => {
                assert_eq!(notif.method, "notifications/progress");
            }
            _ => panic!("Expected Notification"),
        }
    }
}
