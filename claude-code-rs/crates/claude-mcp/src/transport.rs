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
    ///
    /// Times out after 60 seconds to prevent indefinite blocking if the server
    /// dies or stops responding.
    pub async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params);

        let line = serde_json::to_string(&request)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

        const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
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
                    other => {
                        tracing::warn!("MCP protocol desync: expected response id={}, got {:?}", id, other);
                        anyhow::bail!("MCP protocol desynchronization: unexpected message while waiting for id={}", id);
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                anyhow::bail!("MCP request timed out after {}s: method={}", REQUEST_TIMEOUT.as_secs(), method)
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
    ///
    /// Each individual `read_line` call is guarded by [`READ_LINE_TIMEOUT`] to
    /// detect hung MCP servers even when the outer request timeout has not yet
    /// elapsed (e.g. a server that writes partial output but never a newline).
    async fn read_message(&mut self) -> Result<JsonRpcMessage> {
        const READ_LINE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        let mut line = String::new();
        loop {
            line.clear();

            // Check child is still alive before blocking on stdout
            if let Ok(Some(status)) = self.child.try_wait() {
                anyhow::bail!("MCP server exited with {status} before producing a response");
            }

            let bytes_read = tokio::time::timeout(READ_LINE_TIMEOUT, self.stdout.read_line(&mut line))
                .await
                .map_err(|_| anyhow::anyhow!("MCP server read timed out after {}s (no output)", READ_LINE_TIMEOUT.as_secs()))?
                .context("Failed to read from MCP server stdout")?;

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
        // Best-effort immediate kill (can't async in Drop).
        // Callers should call close() explicitly for graceful shutdown.
        let _ = self.child.start_kill();
        // Attempt non-blocking wait to reap the child and avoid zombies.
        let _ = self.child.try_wait();
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
