//! MCP SSE transport — JSON-RPC 2.0 over Server-Sent Events.
//!
//! Connects to a remote MCP server via HTTP SSE endpoint.
//!
//! Protocol:
//!   1. Client GETs the SSE endpoint → receives a stream of events
//!   2. Server sends an `endpoint` event with the POST URL for requests
//!   3. Client POSTs JSON-RPC requests to that endpoint
//!   4. Responses arrive via the SSE stream (matched by request id)

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use crate::protocol::*;
use crate::types::McpSseConfig;

struct PendingRequest {
    tx: oneshot::Sender<JsonRpcResponse>,
}

/// SSE transport for MCP — connects via HTTP SSE and POSTs requests.
pub struct SseTransport {
    http: reqwest::Client,
    post_url: String,
    base_url: String,
    headers: HashMap<String, String>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    notification_rx: mpsc::UnboundedReceiver<JsonRpcNotification>,
    _listener_handle: tokio::task::JoinHandle<()>,
}

impl SseTransport {
    /// Connect to an SSE MCP server.
    pub async fn connect(config: &McpSseConfig) -> Result<Self> {
        info!("Connecting to MCP SSE server at {}", config.url);

        let http = reqwest::Client::builder()
            .user_agent("claude-code-rs/0.1 MCP-SSE")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut header_map = reqwest::header::HeaderMap::new();
        for (k, v) in &config.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }

        let response = http
            .get(&config.url)
            .headers(header_map)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .with_context(|| format!("Failed to connect to SSE endpoint: {}", config.url))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "SSE connection failed with status {}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }

        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut post_url: Option<String> = None;

        let endpoint_timeout = std::time::Duration::from_secs(30);
        let deadline = tokio::time::Instant::now() + endpoint_timeout;

        use futures::StreamExt;
        while post_url.is_none() {
            match tokio::time::timeout_at(deadline, byte_stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buffer.find("\n\n") {
                        let event_block = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();
                        if let Some(url) = parse_sse_endpoint_event(&event_block) {
                            post_url = Some(resolve_url(&config.url, &url));
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    anyhow::bail!("SSE stream error while waiting for endpoint: {}", e);
                }
                Ok(None) => {
                    anyhow::bail!("SSE stream ended before receiving endpoint event");
                }
                Err(_) => {
                    anyhow::bail!("Timeout waiting for SSE endpoint event ({}s)", endpoint_timeout.as_secs());
                }
            }
        }

        let post_url = post_url.unwrap();
        info!("MCP SSE: POST endpoint = {}", post_url);

        let pending: Arc<Mutex<HashMap<u64, PendingRequest>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();

        let pending_clone = Arc::clone(&pending);
        let listener_handle = tokio::spawn(async move {
            let mut buf = buffer;
            loop {
                match byte_stream.next().await {
                    Some(Ok(chunk)) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buf.find("\n\n") {
                            let event_block = buf[..pos].to_string();
                            buf = buf[pos + 2..].to_string();
                            if let Some(data) = extract_sse_data(&event_block) {
                                route_sse_message(&data, &pending_clone, &notification_tx).await;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        warn!("MCP SSE stream error: {}", e);
                        break;
                    }
                    None => {
                        debug!("MCP SSE stream ended");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            http,
            post_url,
            base_url: config.url.clone(),
            headers: config.headers.clone(),
            next_id: AtomicU64::new(1),
            pending,
            notification_rx,
            _listener_handle: listener_handle,
        })
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, PendingRequest { tx });
        }

        let mut header_map = reqwest::header::HeaderMap::new();
        header_map.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        for (k, v) in &self.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }

        let body = serde_json::to_string(&request)?;
        let post_response = self
            .http
            .post(&self.post_url)
            .headers(header_map)
            .body(body)
            .send()
            .await
            .with_context(|| format!("Failed to POST to MCP endpoint: {}", self.post_url))?;

        if !post_response.status().is_success() {
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            anyhow::bail!(
                "MCP POST failed with status {}: {}",
                post_response.status(),
                post_response.text().await.unwrap_or_default()
            );
        }

        let timeout = std::time::Duration::from_secs(300);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                if let Some(error) = response.error {
                    anyhow::bail!(
                        "MCP error {}: {} {}",
                        error.code,
                        error.message,
                        error.data.map(|d| d.to_string()).unwrap_or_default()
                    );
                }
                Ok(response.result.unwrap_or(Value::Null))
            }
            Ok(Err(_)) => anyhow::bail!("MCP SSE response channel closed"),
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                anyhow::bail!("MCP request timed out after {}s", timeout.as_secs())
            }
        }
    }

    /// Send a notification (fire-and-forget).
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = JsonRpcNotification::new(method, params);

        let mut header_map = reqwest::header::HeaderMap::new();
        header_map.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        for (k, v) in &self.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }

        let body = serde_json::to_string(&notification)?;
        let _ = self.http.post(&self.post_url).headers(header_map).body(body).send().await?;
        Ok(())
    }

    /// Try to receive a server-initiated notification (non-blocking).
    pub fn try_recv_notification(&mut self) -> Option<JsonRpcNotification> {
        self.notification_rx.try_recv().ok()
    }

    /// Base URL of the SSE server.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

// ── SSE parsing helpers ──────────────────────────────────────────────────────

fn parse_sse_endpoint_event(block: &str) -> Option<String> {
    let mut event_type = None;
    let mut data = None;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = Some(rest.trim().to_string());
        }
    }
    if event_type.as_deref() == Some("endpoint") { data } else { None }
}

fn extract_sse_data(block: &str) -> Option<String> {
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn resolve_url(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }
    if let Some(pos) = base.find("://") {
        let after_scheme = &base[pos + 3..];
        if let Some(slash_pos) = after_scheme.find('/') {
            let origin = &base[..pos + 3 + slash_pos];
            return format!("{}{}", origin, relative);
        }
    }
    format!("{}{}", base.trim_end_matches('/'), relative)
}

async fn route_sse_message(
    data: &str,
    pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
    notification_tx: &mpsc::UnboundedSender<JsonRpcNotification>,
) {
    if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(data) {
        if let Some(id) = response.id {
            let mut pending = pending.lock().await;
            if let Some(req) = pending.remove(&id) {
                let _ = req.tx.send(response);
                return;
            }
        }
    }
    if let Ok(notification) = serde_json::from_str::<JsonRpcNotification>(data) {
        let _ = notification_tx.send(notification);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoint_event() {
        let block = "event: endpoint\ndata: /messages";
        assert_eq!(parse_sse_endpoint_event(block), Some("/messages".to_string()));
    }

    #[test]
    fn parse_non_endpoint_event() {
        let block = "event: message\ndata: {\"id\": 1}";
        assert_eq!(parse_sse_endpoint_event(block), None);
    }

    #[test]
    fn extract_data_field() {
        let block = "event: message\ndata: {\"jsonrpc\":\"2.0\"}";
        assert_eq!(extract_sse_data(block), Some("{\"jsonrpc\":\"2.0\"}".to_string()));
    }

    #[test]
    fn resolve_relative_url() {
        assert_eq!(resolve_url("https://mcp.example.com/sse", "/messages"), "https://mcp.example.com/messages");
    }

    #[test]
    fn resolve_absolute_url() {
        assert_eq!(resolve_url("https://mcp.example.com/sse", "https://other.com/api"), "https://other.com/api");
    }

    #[test]
    fn resolve_url_no_path() {
        assert_eq!(resolve_url("https://mcp.example.com", "/messages"), "https://mcp.example.com/messages");
    }
}
