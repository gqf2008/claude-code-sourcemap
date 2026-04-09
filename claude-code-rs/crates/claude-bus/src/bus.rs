//! Event bus — typed channels connecting Agent Core to UI clients.
//!
//! The bus provides two halves:
//! - [`BusHandle`] — held by the Agent Core, sends notifications, receives requests
//! - [`ClientHandle`] — held by UI clients, receives notifications, sends requests
//!
//! ## Channel topology
//!
//! ```text
//! AgentNotification:  Core ──broadcast──→ Client(s)  (1:N, lossy on slow receivers)
//! AgentRequest:       Client ──mpsc────→ Core        (N:1, backpressure via bounded)
//! PermissionRequest:  Core ──broadcast──→ Client(s)  (1:N, first responder wins)
//! PermissionResponse: Client ──mpsc────→ Core        (1:1, paired with request)
//! ```

use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::events::{AgentNotification, AgentRequest, PermissionRequest, PermissionResponse, RiskLevel};

// ── EventBus ─────────────────────────────────────────────────────────────────

/// The event bus factory. Call [`EventBus::new`] to create a paired
/// `(BusHandle, ClientHandle)`.
pub struct EventBus;

impl EventBus {
    /// Create a new event bus with the given broadcast channel capacity.
    ///
    /// `capacity` controls the broadcast buffer for notifications.
    /// Slow receivers that fall behind by more than `capacity` messages
    /// will miss intermediate events (they get a `Lagged` error and can
    /// continue from the latest).
    ///
    /// Returns `(core_handle, client_handle)`.
    #[allow(clippy::new_ret_no_self)]
    #[must_use] 
    pub fn new(capacity: usize) -> (BusHandle, ClientHandle) {
        /// Maximum queued requests before backpressure.
        const REQUEST_QUEUE_CAP: usize = 1024;
        /// Maximum queued permission responses before backpressure.
        const PERM_RESP_QUEUE_CAP: usize = 256;

        let (notify_tx, notify_rx) = broadcast::channel(capacity);
        let (request_tx, request_rx) = mpsc::channel(REQUEST_QUEUE_CAP);
        let (perm_req_tx, perm_req_rx) = broadcast::channel(capacity);
        let (perm_resp_tx, perm_resp_rx) = mpsc::channel(PERM_RESP_QUEUE_CAP);

        let bus = BusHandle {
            notify_tx: notify_tx.clone(),
            request_rx,
            request_tx: request_tx.clone(),
            perm_req_tx: perm_req_tx.clone(),
            perm_resp_rx,
            _perm_resp_tx: perm_resp_tx.clone(),
        };

        let client = ClientHandle {
            notify_rx,
            _notify_tx: notify_tx,
            request_tx,
            perm_req_rx,
            _perm_req_tx: perm_req_tx,
            perm_resp_tx: Some(perm_resp_tx),
        };

        (bus, client)
    }
}

// ── BusHandle (Agent Core side) ──────────────────────────────────────────────

/// Handle held by the Agent Core. Provides:
/// - Send notifications to all subscribers
/// - Receive requests from UI clients
/// - Send permission requests and receive responses
/// - Create new client handles
pub struct BusHandle {
    notify_tx: broadcast::Sender<AgentNotification>,
    request_rx: mpsc::Receiver<AgentRequest>,
    request_tx: mpsc::Sender<AgentRequest>,
    perm_req_tx: broadcast::Sender<PermissionRequest>,
    perm_resp_rx: mpsc::Receiver<PermissionResponse>,
    /// Kept alive to prevent the mpsc channel from closing.
    /// Only the primary client gets a clone (secondary clients cannot respond).
    _perm_resp_tx: mpsc::Sender<PermissionResponse>,
}

impl BusHandle {
    /// Broadcast a notification to all subscribed clients.
    ///
    /// Returns the number of receivers that got the message.
    /// Returns 0 if no clients are listening (this is not an error).
    pub fn notify(&self, event: AgentNotification) -> usize {
        self.notify_tx.send(event).unwrap_or(0)
    }

    /// Receive the next request from a UI client.
    ///
    /// Returns `None` when all client handles have been dropped.
    pub async fn recv_request(&mut self) -> Option<AgentRequest> {
        self.request_rx.recv().await
    }

    /// Try to receive a request without blocking.
    pub fn try_recv_request(&mut self) -> Option<AgentRequest> {
        self.request_rx.try_recv().ok()
    }

    /// Send a permission request to the UI and wait for a response.
    ///
    /// This is the primary mechanism for tool permission checks in the
    /// decoupled architecture. The core sends a request describing what
    /// the tool wants to do, and blocks until the UI responds.
    ///
    /// Returns `None` if the UI is disconnected or no client responds
    /// within the timeout (default 30s). Callers should treat `None`
    /// as denial.
    pub async fn request_permission(
        &mut self,
        tool_name: &str,
        input: serde_json::Value,
        risk_level: RiskLevel,
        description: &str,
    ) -> Option<PermissionResponse> {
        self.request_permission_with_timeout(
            tool_name,
            input,
            risk_level,
            description,
            std::time::Duration::from_secs(30),
        )
        .await
    }

    /// Like [`request_permission`](Self::request_permission) but with a
    /// custom timeout. Useful for non-interactive clients (RPC, Bridge)
    /// that may not have a permission UI.
    pub async fn request_permission_with_timeout(
        &mut self,
        tool_name: &str,
        input: serde_json::Value,
        risk_level: RiskLevel,
        description: &str,
        timeout: std::time::Duration,
    ) -> Option<PermissionResponse> {
        let request_id = Uuid::new_v4().to_string();
        let req = PermissionRequest {
            request_id: request_id.clone(),
            tool_name: tool_name.to_string(),
            input,
            risk_level,
            description: description.to_string(),
        };

        // Send request to UI
        if self.perm_req_tx.send(req).is_err() {
            return None; // UI disconnected
        }

        // Wait for matching response with timeout.
        // Without a timeout, non-interactive clients (Bridge, RPC without
        // permission handler) would hang forever.
        let wait = async {
            while let Some(resp) = self.perm_resp_rx.recv().await {
                if resp.request_id == request_id {
                    return Some(resp);
                }
                tracing::warn!(
                    "Received permission response for unknown request: {}",
                    resp.request_id
                );
            }
            None // Channel closed
        };

        if let Ok(result) = tokio::time::timeout(timeout, wait).await { result } else {
            tracing::warn!(
                "Permission request timed out for tool '{}' after {:?}, auto-denying",
                tool_name,
                timeout,
            );
            None
        }
    }

    /// Get the notification sender (for cloning to sub-agents).
    #[must_use] 
    pub fn notify_sender(&self) -> broadcast::Sender<AgentNotification> {
        self.notify_tx.clone()
    }

    /// Create a new `ClientHandle` connected to this bus.
    ///
    /// Multiple clients can coexist — all receive notifications (broadcast),
    /// and all share the same request channel (mpsc to core).
    /// Permission requests are broadcast to all clients for display purposes,
    /// but only the primary client can respond (secondary clients have no
    /// permission response channel to prevent spoofing).
    #[must_use] 
    pub fn new_client(&self) -> ClientHandle {
        ClientHandle {
            notify_rx: self.notify_tx.subscribe(),
            _notify_tx: self.notify_tx.clone(),
            request_tx: self.request_tx.clone(),
            perm_req_rx: self.perm_req_tx.subscribe(),
            _perm_req_tx: self.perm_req_tx.clone(),
            perm_resp_tx: None, // secondary clients cannot respond to permissions
        }
    }

    /// Subscribe to the request channel as a receiver (for testing).
    ///
    /// NOTE: The primary request receiver is held by `BusHandle` itself
    /// (via `recv_request`). This creates a *notification* subscription
    /// for the purpose of observing requests in tests.
    #[must_use] 
    pub fn subscribe_requests(&self) -> mpsc::Receiver<AgentRequest> {
        // This is a workaround: we can't really "subscribe" to mpsc.
        // For tests, create a new channel pair and swap in the new receiver.
        // In production, the adapter uses recv_request() on the BusHandle.
        //
        // For the RPC server test, we instead just check that the session
        // handles requests properly through the normal flow.
        //
        // We provide a dummy receiver here that will never receive anything.
        let (_tx, rx) = mpsc::channel(1);
        rx
    }
}

// ── ClientHandle (UI side) ───────────────────────────────────────────────────

/// Handle held by UI clients (REPL, IDE, Web). Provides:
/// - Receive notifications from the Agent Core
/// - Send requests to the Agent Core
/// - Receive permission requests and send responses
pub struct ClientHandle {
    notify_rx: broadcast::Receiver<AgentNotification>,
    /// Keep the sender alive so `notify_rx` doesn't get `Closed`.
    _notify_tx: broadcast::Sender<AgentNotification>,
    request_tx: mpsc::Sender<AgentRequest>,
    perm_req_rx: broadcast::Receiver<PermissionRequest>,
    /// Keep the sender alive so `perm_req_rx` doesn't get `Closed`.
    _perm_req_tx: broadcast::Sender<PermissionRequest>,
    /// Permission response channel. Only the primary client can respond;
    /// secondary clients (via `new_client()`) have `None` to prevent spoofing.
    perm_resp_tx: Option<mpsc::Sender<PermissionResponse>>,
}

impl ClientHandle {
    /// Receive the next notification from the Agent Core.
    ///
    /// If the client falls behind, intermediate messages are skipped
    /// (`broadcast::Lagged`) and this returns the next available message.
    pub async fn recv_notification(&mut self) -> Option<AgentNotification> {
        loop {
            match self.notify_rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Client lagged by {} notifications, catching up", n);
                    continue; // Try again with the latest
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Send a request to the Agent Core.
    pub fn send_request(&self, request: AgentRequest) -> Result<(), SendError> {
        self.request_tx
            .try_send(request)
            .map_err(|_| SendError::DISCONNECTED)
    }

    /// Receive the next permission request from the Agent Core.
    ///
    /// All clients receive permission requests (broadcast). Only the first
    /// client to respond with a matching `request_id` wins.
    pub async fn recv_permission_request(&mut self) -> Option<PermissionRequest> {
        loop {
            match self.perm_req_rx.recv().await {
                Ok(req) => return Some(req),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Client lagged by {} permission requests, catching up", n);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Respond to a permission request.
    ///
    /// Only the primary client can respond; secondary clients return `Err`.
    pub fn send_permission_response(&self, response: PermissionResponse) -> Result<(), SendError> {
        match &self.perm_resp_tx {
            Some(tx) => tx.try_send(response).map_err(|_| SendError::DISCONNECTED),
            None => Err(SendError::DISCONNECTED), // secondary client — not authorized
        }
    }

    /// Convenience: submit a user message.
    pub fn submit(&self, text: impl Into<String>) -> Result<(), SendError> {
        self.send_request(AgentRequest::Submit {
            text: text.into(),
            images: vec![],
        })
    }

    /// Convenience: send abort signal.
    pub fn abort(&self) -> Result<(), SendError> {
        self.send_request(AgentRequest::Abort)
    }

    /// Convenience: send shutdown signal.
    pub fn shutdown(&self) -> Result<(), SendError> {
        self.send_request(AgentRequest::Shutdown)
    }

    /// Create an additional notification subscriber.
    ///
    /// Useful for spawning multiple consumers (e.g., one for display,
    /// one for logging).
    #[must_use] 
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<AgentNotification> {
        self._notify_tx.subscribe()
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Error when sending to a disconnected bus.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Bus disconnected: the other end has been dropped")]
pub struct SendError;

impl SendError {
    /// Sentinel value for when the bus is disconnected.
    pub const DISCONNECTED: Self = Self;
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_notification_flow() {
        let (bus, mut client) = EventBus::new(16);

        bus.notify(AgentNotification::TextDelta {
            text: "Hello".into(),
        });
        bus.notify(AgentNotification::TextDelta {
            text: " world".into(),
        });

        let e1 = client.recv_notification().await.unwrap();
        let e2 = client.recv_notification().await.unwrap();

        match e1 {
            AgentNotification::TextDelta { text } => assert_eq!(text, "Hello"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
        match e2 {
            AgentNotification::TextDelta { text } => assert_eq!(text, " world"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_flow() {
        let (mut bus, client) = EventBus::new(16);

        client
            .send_request(AgentRequest::Submit {
                text: "Fix bug".into(),
                images: vec![],
            })
            .unwrap();

        let req = bus.recv_request().await.unwrap();
        match req {
            AgentRequest::Submit { text, .. } => assert_eq!(text, "Fix bug"),
            other => panic!("Expected Submit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permission_request_response() {
        let (mut bus, mut client) = EventBus::new(16);

        // Spawn the core side: send permission request, wait for response
        let core = tokio::spawn(async move {
            let resp = bus
                .request_permission("Bash", serde_json::json!({"cmd": "ls"}), RiskLevel::Low, "List files")
                .await
                .unwrap();
            assert!(resp.granted);
            assert!(resp.remember);
        });

        // UI side: receive permission request, send response
        let perm = client.recv_permission_request().await.unwrap();
        assert_eq!(perm.tool_name, "Bash");
        assert_eq!(perm.risk_level, RiskLevel::Low);

        client
            .send_permission_response(PermissionResponse {
                request_id: perm.request_id,
                granted: true,
                remember: true,
            })
            .unwrap();

        core.await.unwrap();
    }

    #[tokio::test]
    async fn abort_signal() {
        let (mut bus, client) = EventBus::new(16);

        client.abort().unwrap();

        let req = bus.recv_request().await.unwrap();
        assert!(matches!(req, AgentRequest::Abort));
    }

    #[tokio::test]
    async fn shutdown_signal() {
        let (mut bus, client) = EventBus::new(16);

        client.shutdown().unwrap();

        let req = bus.recv_request().await.unwrap();
        assert!(matches!(req, AgentRequest::Shutdown));
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let (bus, client) = EventBus::new(16);

        // Create a second subscriber
        let mut sub2 = client.subscribe_notifications();

        bus.notify(AgentNotification::SessionStart {
            session_id: "s1".into(),
            model: "sonnet".into(),
        });

        // Both should receive the event
        // We need to use the second subscriber directly since client's notify_rx
        // also gets the event
        let e2 = sub2.recv().await.unwrap();
        match e2 {
            AgentNotification::SessionStart { session_id, .. } => {
                assert_eq!(session_id, "s1");
            }
            other => panic!("Expected SessionStart, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnected_send_error() {
        let (_bus, client) = EventBus::new(16);

        // Drop the bus handle (core disconnected)
        drop(_bus);

        // Request should fail with SendError — but mpsc channels only error
        // when the receiver is dropped. Since we dropped bus (which owns request_rx),
        // sending should fail.
        let result = client.send_request(AgentRequest::Abort);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn submit_convenience() {
        let (mut bus, client) = EventBus::new(16);

        client.submit("Hello there").unwrap();

        let req = bus.recv_request().await.unwrap();
        match req {
            AgentRequest::Submit { text, images } => {
                assert_eq!(text, "Hello there");
                assert!(images.is_empty());
            }
            other => panic!("Expected Submit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_recv_empty() {
        let (mut bus, _client) = EventBus::new(16);

        // No requests pending
        assert!(bus.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn try_recv_with_pending() {
        let (mut bus, client) = EventBus::new(16);

        client.submit("test").unwrap();

        let req = bus.try_recv_request();
        assert!(req.is_some());
        assert!(matches!(req.unwrap(), AgentRequest::Submit { .. }));
    }

    #[tokio::test]
    async fn high_throughput_notifications() {
        let (bus, mut client) = EventBus::new(1024);

        // Send 500 notifications rapidly
        for i in 0..500 {
            bus.notify(AgentNotification::TextDelta {
                text: format!("chunk-{i}"),
            });
        }

        // Receive all
        for i in 0..500 {
            let event = client.recv_notification().await.unwrap();
            match event {
                AgentNotification::TextDelta { text } => {
                    assert_eq!(text, format!("chunk-{i}"));
                }
                other => panic!("Expected TextDelta, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn agent_lifecycle_events() {
        let (bus, mut client) = EventBus::new(16);

        bus.notify(AgentNotification::AgentSpawned {
            agent_id: "a1".into(),
            name: Some("reviewer".into()),
            agent_type: "explore".into(),
            background: true,
        });
        bus.notify(AgentNotification::AgentProgress {
            agent_id: "a1".into(),
            text: "Found 3 files".into(),
        });
        bus.notify(AgentNotification::AgentComplete {
            agent_id: "a1".into(),
            result: "Review complete".into(),
            is_error: false,
        });

        // Verify lifecycle
        let e1 = client.recv_notification().await.unwrap();
        assert!(matches!(e1, AgentNotification::AgentSpawned { .. }));

        let e2 = client.recv_notification().await.unwrap();
        assert!(matches!(e2, AgentNotification::AgentProgress { .. }));

        let e3 = client.recv_notification().await.unwrap();
        assert!(matches!(e3, AgentNotification::AgentComplete { .. }));
    }

    #[tokio::test]
    async fn permission_request_timeout_auto_denies() {
        let (mut bus, _client) = EventBus::new(16);

        // No one handles the permission request → should timeout and return None
        let result = bus
            .request_permission_with_timeout(
                "Bash",
                serde_json::json!({"cmd": "rm -rf /"}),
                RiskLevel::High,
                "Delete everything",
                std::time::Duration::from_millis(50),
            )
            .await;

        assert!(result.is_none(), "Timed-out permission request should return None (deny)");
    }

    #[tokio::test]
    async fn permission_response_within_timeout() {
        let (mut bus, mut client) = EventBus::new(16);

        let core = tokio::spawn(async move {
            let resp = bus
                .request_permission_with_timeout(
                    "FileRead",
                    serde_json::json!({}),
                    RiskLevel::Low,
                    "Read a file",
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert!(resp.granted);
        });

        let perm = client.recv_permission_request().await.unwrap();
        client
            .send_permission_response(PermissionResponse {
                request_id: perm.request_id,
                granted: true,
                remember: false,
            })
            .unwrap();

        core.await.unwrap();
    }
}
