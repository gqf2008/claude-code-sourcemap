//! JSON-RPC 2.0 transport over stdio using Content-Length framing.
//!
//! Language servers communicate via this wire format:
//!   Content-Length: <N>\r\n
//!   \r\n
//!   <N bytes of UTF-8 JSON>

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout};

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Write a JSON-RPC message to stdin with Content-Length framing.
pub fn write_message(stdin: &mut ChildStdin, msg: &Value) -> Result<()> {
    let body = serde_json::to_string(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes())?;
    stdin.write_all(body.as_bytes())?;
    stdin.flush()?;
    Ok(())
}

/// Read a JSON-RPC message from stdout (blocking).
pub fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_length: Option<usize> = None;

    // Parse headers
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            bail!("LSP server closed stdout unexpectedly");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // End of headers
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = val.trim().parse().ok();
        }
    }

    let len = content_length.context("Missing Content-Length header from LSP server")?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let msg: Value = serde_json::from_slice(&body)?;
    Ok(msg)
}

/// Read messages until we get one with the given request id (skips notifications).
pub fn read_response(reader: &mut BufReader<ChildStdout>, id: i64) -> Result<Value> {
    loop {
        let msg = read_message(reader)?;
        if let Some(msg_id) = msg.get("id").and_then(|v| v.as_i64()) {
            if msg_id == id {
                if let Some(err) = msg.get("error") {
                    bail!("LSP error: {}", err);
                }
                return Ok(msg["result"].clone());
            }
        }
        // Skip notifications (no id) and responses for other ids
    }
}
