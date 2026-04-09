//! RPC Server — manages transports, sessions, and lifecycle.
//!
//! The server accepts connections from multiple transports (stdio, TCP)
//! and creates an `RpcSession` for each connection, binding it to a new
//! `ClientHandle` on the shared event bus.
//!
//! # Usage
//!
//! ```rust,ignore
//! let (bus_handle, _) = EventBus::new(256);
//! let mut server = RpcServer::new(bus_handle);
//!
//! // Option A: stdio (single session)
//! server.serve_stdio().await;
//!
//! // Option B: TCP (multi-session)
//! server.serve_tcp("127.0.0.1:9100").await?;
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;
use tracing::{error, info, warn};
use uuid::Uuid;

use claude_bus::bus::BusHandle;

use crate::session::RpcSession;
use crate::transport::stdio::StdioTransport;
use crate::transport::tcp::TcpListener;

/// Maximum concurrent TCP sessions before rejecting new connections.
const MAX_TCP_SESSIONS: usize = 64;

/// RPC server managing transport listeners and active sessions.
pub struct RpcServer {
    bus: Arc<BusHandle>,
    shutdown: Arc<Notify>,
    session_count: Arc<AtomicUsize>,
    /// Optional auth token for TCP connections. If set, clients must send
    /// `{"method":"auth","params":{"token":"..."}}` as their first message.
    auth_token: Option<String>,
}

impl RpcServer {
    /// Create a new server bound to an event bus.
    pub fn new(bus: BusHandle) -> Self {
        Self {
            bus: Arc::new(bus),
            shutdown: Arc::new(Notify::new()),
            session_count: Arc::new(AtomicUsize::new(0)),
            auth_token: None,
        }
    }

    /// Create a server with token-based authentication for TCP connections.
    pub fn with_auth(bus: BusHandle, token: impl Into<String>) -> Self {
        Self {
            bus: Arc::new(bus),
            shutdown: Arc::new(Notify::new()),
            session_count: Arc::new(AtomicUsize::new(0)),
            auth_token: Some(token.into()),
        }
    }

    /// Serve a single session over stdio (stdin/stdout).
    ///
    /// This blocks until the stdio connection closes (typically when the
    /// parent process exits). Used by IDE extensions.
    pub async fn serve_stdio(self) {
        let transport = StdioTransport::new();
        let client = self.bus.new_client();
        let session_id = format!("stdio-{}", &Uuid::new_v4().to_string()[..8]);

        info!("Serving stdio session: {}", session_id);

        self.session_count.fetch_add(1, Ordering::Relaxed);

        let session = RpcSession::new(session_id, Box::new(transport), client);
        session.run().await;

        self.session_count.fetch_sub(1, Ordering::Relaxed);

        info!("Stdio session ended");
    }

    /// Serve multiple sessions over TCP.
    ///
    /// Listens for connections and spawns an `RpcSession` for each.
    /// If `auth_token` is set, clients must authenticate as their first message.
    /// Returns when `shutdown()` is called or the listener errors.
    pub async fn serve_tcp(&self, addr: &str) -> Result<(), std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        info!("TCP server listening on {}", local_addr);

        let shutdown = Arc::clone(&self.shutdown);

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((mut transport, peer_addr)) => {
                            // Connection limit check
                            let current = self.session_count.load(Ordering::Relaxed);
                            if current >= MAX_TCP_SESSIONS {
                                warn!("Connection limit reached ({}/{}), rejecting {}", current, MAX_TCP_SESSIONS, peer_addr);
                                drop(transport);
                                continue;
                            }

                            // Auth handshake (if token is configured)
                            if let Some(ref expected_token) = self.auth_token {
                                match authenticate_connection(&mut transport, expected_token).await {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        warn!("Auth failed from {}, closing", peer_addr);
                                        let _ = transport.close().await;
                                        continue;
                                    }
                                    Err(e) => {
                                        warn!("Auth error from {}: {}", peer_addr, e);
                                        let _ = transport.close().await;
                                        continue;
                                    }
                                }
                            }

                            let session_id = format!("tcp-{}", &Uuid::new_v4().to_string()[..8]);
                            info!("[{}] New connection from {} ({}/{})", session_id, peer_addr, current + 1, MAX_TCP_SESSIONS);

                            let client = self.bus.new_client();
                            let session = RpcSession::new(session_id.clone(), Box::new(transport), client);

                            let count = Arc::clone(&self.session_count);
                            count.fetch_add(1, Ordering::Relaxed);
                            tokio::spawn(async move {
                                // Drop guard ensures count is decremented even on panic
                                struct SessionGuard(Arc<AtomicUsize>, String);
                                impl Drop for SessionGuard {
                                    fn drop(&mut self) {
                                        self.0.fetch_sub(1, Ordering::Relaxed);
                                        info!("[{}] Session closed", self.1);
                                    }
                                }
                                let _guard = SessionGuard(count, session_id);
                                session.run().await;
                            });
                        }
                        Err(e) => {
                            error!("Accept error: {}", e);
                        }
                    }
                }
                _ = shutdown.notified() => {
                    info!("TCP server shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Signal the server to shut down gracefully.
    pub fn shutdown(&self) {
        info!("Shutdown signal sent");
        self.shutdown.notify_waiters();
    }

    /// Get the current number of active sessions.
    pub fn session_count(&self) -> usize {
        self.session_count.load(Ordering::Relaxed)
    }
}

// ── Auth handshake ───────────────────────────────────────────────────────────

use crate::protocol::{error_codes, RawMessage, Response, RpcError};
use crate::transport::Transport;

/// Authenticate a new TCP connection by reading the first message as an auth request.
///
/// Expected format: `{"method":"auth","params":{"token":"<secret>"}}`
/// Returns Ok(true) if authenticated, Ok(false) if auth failed, Err on transport error.
async fn authenticate_connection(
    transport: &mut impl Transport,
    expected_token: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // Read first message with a timeout
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        transport.read_message(),
    ).await
        .map_err(|_| "Auth timeout: no message received within 5s")?
        .map_err(|e| format!("Auth transport error: {e}"))?
        .ok_or("Auth failed: connection closed before auth message")?;

    // Check method == "auth" and extract token
    let is_auth = msg.method.as_deref() == Some("auth");
    let token = msg.params
        .as_ref()
        .and_then(|p| p.get("token"))
        .and_then(|v| v.as_str());

    if is_auth && token == Some(expected_token) {
        // Send success response
        let resp = Response::success(
            msg.id.unwrap_or(crate::protocol::RequestId::Null),
            serde_json::json!({"authenticated": true}),
        );
        transport.write_message(&RawMessage::from(resp)).await
            .map_err(|e| format!("Auth write error: {e}"))?;
        Ok(true)
    } else {
        // Send auth failure
        let resp = Response::error(
            msg.id.unwrap_or(crate::protocol::RequestId::Null),
            RpcError::new(error_codes::INVALID_PARAMS, "Authentication failed: invalid or missing token"),
        );
        transport.write_message(&RawMessage::from(resp)).await
            .map_err(|e| format!("Auth write error: {e}"))?;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_bus::bus::EventBus;

    #[tokio::test]
    async fn server_creation() {
        let (bus_handle, _client) = EventBus::new(64);
        let server = RpcServer::new(bus_handle);
        assert_eq!(server.session_count(), 0);
    }

    #[tokio::test]
    async fn tcp_server_accept_connection() {
        let (bus_handle, _client) = EventBus::new(64);
        let server = Arc::new(RpcServer::new(bus_handle));

        // Start TCP server on random port
        let server_clone = Arc::clone(&server);
        let serve_task = tokio::spawn(async move {
            server_clone.serve_tcp("127.0.0.1:0").await.unwrap();
        });

        // Give server time to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Shutdown
        server.shutdown();
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            serve_task,
        ).await;
    }

    #[tokio::test]
    async fn server_shutdown_signal() {
        let (bus_handle, _client) = EventBus::new(64);
        let server = RpcServer::new(bus_handle);
        server.shutdown();
        // Just verify it doesn't panic
    }

    #[tokio::test]
    async fn server_with_auth_creation() {
        let (bus_handle, _client) = EventBus::new(64);
        let server = RpcServer::with_auth(bus_handle, "secret-token");
        assert_eq!(server.session_count(), 0);
        assert!(server.auth_token.is_some());
    }

    #[tokio::test]
    async fn server_no_auth_by_default() {
        let (bus_handle, _client) = EventBus::new(64);
        let server = RpcServer::new(bus_handle);
        assert!(server.auth_token.is_none());
    }
}
