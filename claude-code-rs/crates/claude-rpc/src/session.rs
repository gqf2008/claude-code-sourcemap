//! RpcSession — binds a transport connection to a bus ClientHandle.
//!
//! Each RPC session maps one client connection (transport) to one
//! `ClientHandle` on the event bus. It:
//!
//! 1. Reads JSON-RPC requests from the transport → parses → sends AgentRequest
//! 2. Receives AgentNotification from the bus → converts → writes JSON-RPC notifications
//! 3. Handles permission requests by forwarding them as JSON-RPC notifications
//!    and waiting for the client's response
//!
//! Architecture: single `tokio::select!` loop that multiplexes inbound transport
//! messages, outbound bus notifications, and permission requests. No mutex needed.

use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use claude_bus::bus::ClientHandle;

use crate::methods::{notification_to_jsonrpc, parse_request};
use crate::protocol::{error_codes, Message, Notification, RawMessage, RequestId, Response, RpcError};
use crate::transport::{Transport, TransportError};

/// A single RPC session: one transport connection + one bus ClientHandle.
pub struct RpcSession {
    id: String,
    transport: Box<dyn Transport>,
    client: ClientHandle,
}

impl RpcSession {
    /// Create a new session binding a transport to a client handle.
    pub fn new(
        id: impl Into<String>,
        transport: Box<dyn Transport>,
        client: ClientHandle,
    ) -> Self {
        Self {
            id: id.into(),
            transport,
            client,
        }
    }

    /// Session identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Run the session event loop.
    ///
    /// Uses a single `tokio::select!` loop that multiplexes:
    /// - Inbound: transport → parse → send_request to bus
    /// - Outbound: bus notifications → serialize → transport
    /// - Permissions: bus permission requests → serialize → transport
    ///
    /// Returns when the connection closes or an unrecoverable error occurs.
    pub async fn run(mut self) {
        let session_id = self.id.clone();
        info!("[{}] Session started", session_id);

        // Get a separate notification receiver so we don't need &mut client for both
        // recv_notification and recv_permission_request simultaneously.
        let mut notif_rx = self.client.subscribe_notifications();

        loop {
            tokio::select! {
                // ── Inbound: transport → bus ──────────────────────
                msg = self.transport.read_message() => {
                    match msg {
                        Ok(Some(raw)) => {
                            self.handle_inbound(&session_id, raw).await;
                        }
                        Ok(None) => {
                            info!("[{}] Connection closed", session_id);
                            break;
                        }
                        Err(TransportError::Json(e)) => {
                            error!("[{}] JSON parse error: {}", session_id, e);
                            let resp = Response::error(
                                RequestId::Null,
                                RpcError::new(error_codes::PARSE_ERROR, e.to_string()),
                            );
                            if let Err(we) = self.transport.write_message(&RawMessage::from(resp)).await {
                                debug!("[{}] Failed to write parse error response: {}", session_id, we);
                            }
                        }
                        Err(e) => {
                            error!("[{}] Transport error: {}", session_id, e);
                            break;
                        }
                    }
                }

                // ── Outbound: bus notifications → transport ──────
                notif = notif_rx.recv() => {
                    match notif {
                        Ok(n) => {
                            let jsonrpc = notification_to_jsonrpc(&n);
                            if let Err(e) = self.transport.write_message(&RawMessage::from(jsonrpc)).await {
                                debug!("[{}] Write error: {}", session_id, e);
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("[{}] Lagged by {} notifications", session_id, n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            debug!("[{}] Bus closed", session_id);
                            break;
                        }
                    }
                }

                // ── Permission requests → transport ──────────────
                perm = self.client.recv_permission_request() => {
                    match perm {
                        Some(req) => {
                            let notif = Notification::new("agent.permissionRequest", Some(serde_json::json!({
                                "request_id": req.request_id,
                                "tool_name": req.tool_name,
                                "input": req.input,
                                "risk_level": req.risk_level.to_string(),
                                "description": req.description,
                            })));
                            if let Err(e) = self.transport.write_message(&RawMessage::from(notif)).await {
                                debug!("[{}] Permission write error: {}", session_id, e);
                                break;
                            }
                        }
                        None => {
                            debug!("[{}] Permission channel closed", session_id);
                            // Don't break — permission channel closing doesn't end the session
                        }
                    }
                }
            }
        }

        let _ = self.transport.close().await;
        info!("[{}] Session ended", session_id);
    }

    /// Handle an inbound JSON-RPC message from the transport.
    async fn handle_inbound(&mut self, session_id: &str, raw: RawMessage) {
        let fallback_id = raw.id.clone();
        match raw.classify() {
            Ok(Message::Request(req)) => {
                let request_id = req.id.clone();
                match parse_request(&req.method, req.params) {
                    Ok(agent_req) => {
                        if let Err(e) = self.client.send_request(agent_req) {
                            let resp = Response::error(
                                request_id,
                                RpcError::new(error_codes::INTERNAL_ERROR, e.to_string()),
                            );
                            if let Err(we) = self.transport.write_message(&RawMessage::from(resp)).await {
                                debug!("[{}] Failed to write error response: {}", session_id, we);
                            }
                        } else {
                            let resp = Response::success(
                                request_id,
                                serde_json::json!({"ok": true}),
                            );
                            if let Err(we) = self.transport.write_message(&RawMessage::from(resp)).await {
                                debug!("[{}] Failed to write success response: {}", session_id, we);
                            }
                        }
                    }
                    Err(rpc_err) => {
                        let resp = Response::error(request_id, rpc_err);
                        if let Err(we) = self.transport.write_message(&RawMessage::from(resp)).await {
                            debug!("[{}] Failed to write method error response: {}", session_id, we);
                        }
                    }
                }
            }
            Ok(Message::Notification(notif)) => {
                debug!("[{}] Client notification: {}", session_id, notif.method);
            }
            Ok(Message::Response(_)) => {
                warn!("[{}] Unexpected response from client", session_id);
            }
            Err(rpc_err) => {
                let resp = Response::error(
                    fallback_id.unwrap_or(RequestId::Null),
                    rpc_err,
                );
                if let Err(we) = self.transport.write_message(&RawMessage::from(resp)).await {
                    debug!("[{}] Failed to write classify error response: {}", session_id, we);
                }
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Request;
    use crate::transport::stdio::test_transport::ChannelTransport;
    use claude_bus::bus::EventBus;
    use claude_bus::events::AgentNotification;

    #[tokio::test]
    async fn session_request_response() {
        let (mut bus_handle, client) = EventBus::new(64);
        let (client_transport, mut server_side) = ChannelTransport::pair(64);

        // Spawn a fake agent core that responds to requests
        tokio::spawn(async move {
            if let Some(_req) = bus_handle.recv_request().await {
                bus_handle.notify(AgentNotification::HistoryCleared);
            }
        });

        let session = RpcSession::new("test-1", Box::new(client_transport), client);
        let session_task = tokio::spawn(session.run());

        // Send a request from the "client" side
        let req = Request::new(1, "agent.clearHistory", None);
        server_side.write_message(&RawMessage::from(req)).await.unwrap();

        // Should get a success response
        let resp = server_side.read_message().await.unwrap().unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());

        // Should also get the HistoryCleared notification
        let notif = server_side.read_message().await.unwrap().unwrap();
        assert_eq!(notif.method.as_deref(), Some("agent.historyCleared"));

        // Close the server side to end the session
        drop(server_side);
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            session_task,
        ).await;
    }

    #[tokio::test]
    async fn session_unknown_method() {
        let (_bus_handle, client) = EventBus::new(64);
        let (client_transport, mut server_side) = ChannelTransport::pair(64);

        let session = RpcSession::new("test-2", Box::new(client_transport), client);
        let session_task = tokio::spawn(session.run());

        // Send unknown method
        let req = Request::new(1, "unknown.method", None);
        server_side.write_message(&RawMessage::from(req)).await.unwrap();

        // Should get error response
        let resp = server_side.read_message().await.unwrap().unwrap();
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);

        drop(server_side);
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            session_task,
        ).await;
    }

    #[tokio::test]
    async fn session_handles_connection_close() {
        let (_bus_handle, client) = EventBus::new(64);
        let (client_transport, server_side) = ChannelTransport::pair(64);

        let session = RpcSession::new("test-3", Box::new(client_transport), client);

        // Drop server side immediately
        drop(server_side);

        // Session should complete quickly
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            session.run(),
        ).await;
        assert!(result.is_ok());
    }
}
