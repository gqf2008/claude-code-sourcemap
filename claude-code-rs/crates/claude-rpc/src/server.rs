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

use tokio::sync::{Mutex, Notify};
use tracing::{error, info};
use uuid::Uuid;

use claude_bus::bus::BusHandle;

use crate::session::RpcSession;
use crate::transport::stdio::StdioTransport;
use crate::transport::tcp::TcpListener;

/// RPC server managing transport listeners and active sessions.
pub struct RpcServer {
    bus: Arc<BusHandle>,
    shutdown: Arc<Notify>,
    session_count: Arc<Mutex<usize>>,
}

impl RpcServer {
    /// Create a new server bound to an event bus.
    pub fn new(bus: BusHandle) -> Self {
        Self {
            bus: Arc::new(bus),
            shutdown: Arc::new(Notify::new()),
            session_count: Arc::new(Mutex::new(0)),
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

        {
            let mut count = self.session_count.lock().await;
            *count += 1;
        }

        let session = RpcSession::new(session_id, Box::new(transport), client);
        session.run().await;

        {
            let mut count = self.session_count.lock().await;
            *count -= 1;
        }

        info!("Stdio session ended");
    }

    /// Serve multiple sessions over TCP.
    ///
    /// Listens for connections and spawns an `RpcSession` for each.
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
                        Ok((transport, peer_addr)) => {
                            let session_id = format!("tcp-{}", &Uuid::new_v4().to_string()[..8]);
                            info!("[{}] New connection from {}", session_id, peer_addr);

                            let client = self.bus.new_client();
                            let session = RpcSession::new(session_id.clone(), Box::new(transport), client);

                            let count = Arc::clone(&self.session_count);
                            tokio::spawn(async move {
                                {
                                    let mut c = count.lock().await;
                                    *c += 1;
                                }

                                session.run().await;

                                {
                                    let mut c = count.lock().await;
                                    *c -= 1;
                                }
                                info!("[{}] Session closed", session_id);
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
        self.shutdown.notify_one();
    }

    /// Get the current number of active sessions.
    pub async fn session_count(&self) -> usize {
        *self.session_count.lock().await
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use claude_bus::bus::EventBus;

    #[tokio::test]
    async fn server_creation() {
        let (bus_handle, _client) = EventBus::new(64);
        let server = RpcServer::new(bus_handle);
        assert_eq!(server.session_count().await, 0);
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
}
