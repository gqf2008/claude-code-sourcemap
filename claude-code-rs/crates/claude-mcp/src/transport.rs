//! MCP stdio transport — JSON-RPC 2.0 over stdin/stdout.
//!
//! Spawns a child process and communicates via newline-delimited JSON-RPC
//! messages on stdin/stdout. Aligned with the MCP specification.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};

use crate::protocol::{JsonRpcRequest, JsonRpcMessage, JsonRpcNotification};

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
        let request = JsonRpcRequest::new(id, method, params);

        let line = serde_json::to_string(&request)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

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
                JsonRpcMessage::Notification(_) => continue,
                _ => continue,
            }
        }
    }

    /// Send a notification (no response expected).
    pub async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = JsonRpcNotification::new(method, params);
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
                continue;
            }
            let msg: JsonRpcMessage = serde_json::from_str(trimmed)
                .with_context(|| format!("Invalid JSON-RPC from MCP server: {trimmed}"))?;
            return Ok(msg);
        }
    }

    /// Gracefully close the transport and kill the child process.
    pub async fn close(&mut self) -> Result<()> {
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
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_protocol_new() {
        let req = JsonRpcRequest::new(1, "test", None);
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id, 1);
        assert_eq!(req.method, "test");
    }
}
