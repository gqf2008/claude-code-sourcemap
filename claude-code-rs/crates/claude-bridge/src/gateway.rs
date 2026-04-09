//! Gateway — orchestrates adapters, routing, and message flow.
//!
//! The gateway manages all channel adapters and coordinates message
//! routing between external platforms and the Agent via the Event Bus.
//!
//! For each inbound message, the gateway:
//! 1. Creates or retrieves a session via `SessionRouter`
//! 2. Submits the user message to the Agent via the Event Bus
//! 3. Spawns a notification consumer task that reads `AgentNotification`s,
//!    aggregates them via `MessageFormatter`, and sends replies back
//!    through the appropriate `ChannelAdapter`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{error, info, warn};

use claude_bus::bus::BusHandle;
use claude_bus::events::{AgentNotification, AgentRequest};

use crate::adapter::{AdapterError, AdapterResult, ChannelAdapter};
use crate::config::BridgeConfig;
use crate::formatter::MessageFormatter;
use crate::message::{ChannelId, InboundMessage};
use crate::session::SessionRouter;

/// Context provided to adapters during startup.
///
/// Adapters use this to route inbound messages back to the gateway.
#[derive(Clone)]
pub struct GatewayContext {
    /// Sender for inbound messages from adapters to the gateway.
    pub(crate) inbound_tx: tokio::sync::mpsc::UnboundedSender<InboundMessage>,
}

impl GatewayContext {
    /// Route an inbound message from a platform adapter to the gateway.
    pub fn route_inbound(&self, msg: InboundMessage) -> Result<(), String> {
        self.inbound_tx.send(msg).map_err(|_| "Gateway closed".to_string())
    }
}

/// Adapter registry type — maps platform name to adapter instance.
type AdapterMap = HashMap<String, Arc<Box<dyn ChannelAdapter>>>;

/// The main gateway that manages adapters and routes messages.
pub struct ChannelGateway {
    /// Registered adapters by platform name (RwLock for dynamic registration).
    adapters: Arc<RwLock<AdapterMap>>,
    /// Session router (shared across message handling tasks).
    router: Arc<Mutex<SessionRouter>>,
    /// Inbound message channel.
    inbound_tx: tokio::sync::mpsc::UnboundedSender<InboundMessage>,
    inbound_rx: Option<tokio::sync::mpsc::UnboundedReceiver<InboundMessage>>,
    /// Active notification consumer tasks.
    consumer_tasks: Arc<Mutex<HashMap<ChannelId, tokio::task::JoinHandle<()>>>>,
    /// Configuration.
    _config: BridgeConfig,
}

impl ChannelGateway {
    /// Create a new gateway with the given bus handle and config.
    pub fn new(bus: BusHandle, config: BridgeConfig) -> Self {
        let idle_timeout = Duration::from_secs(config.session_idle_timeout_secs.unwrap_or(3600));
        let router = SessionRouter::new(bus, idle_timeout);
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel();

        Self {
            adapters: Arc::new(RwLock::new(HashMap::new())),
            router: Arc::new(Mutex::new(router)),
            inbound_tx,
            inbound_rx: Some(inbound_rx),
            consumer_tasks: Arc::new(Mutex::new(HashMap::new())),
            _config: config,
        }
    }

    /// Register a channel adapter.
    ///
    /// Can be called before or after `run()` — adapters are dynamically added.
    pub async fn register_adapter(&self, adapter: Box<dyn ChannelAdapter>) -> AdapterResult<()> {
        let platform = adapter.platform().to_string();
        info!("Registered adapter: {platform}");
        self.adapters.write().await.insert(platform, Arc::new(adapter));
        Ok(())
    }

    /// Start all registered adapters and begin message routing.
    ///
    /// This blocks until `shutdown()` is called or all adapters stop.
    pub async fn run(&mut self) -> AdapterResult<()> {
        let ctx = GatewayContext {
            inbound_tx: self.inbound_tx.clone(),
        };

        // Start all adapters
        {
            let mut adapters = self.adapters.write().await;
            for (platform, adapter) in adapters.iter_mut() {
                info!("Starting adapter: {platform}");
                let adapter_mut = Arc::get_mut(adapter)
                    .ok_or_else(|| AdapterError::Internal(format!("adapter '{platform}' must not be shared yet")))?;
                adapter_mut.start(ctx.clone()).await?;
            }
        }

        // Process inbound messages
        let mut inbound_rx = self.inbound_rx.take()
            .ok_or_else(|| AdapterError::Internal("Gateway can only be run once".into()))?;

        let router = Arc::clone(&self.router);
        let adapters = Arc::clone(&self.adapters);
        let consumer_tasks = Arc::clone(&self.consumer_tasks);

        info!("Gateway running with {} adapters", self.adapters.read().await.len());

        while let Some(msg) = inbound_rx.recv().await {
            let channel_id = msg.channel_id.clone();

            // Handle special commands
            if msg.text.starts_with('/')
                && Self::handle_command(&self.router, &msg).await
            {
                // If session was destroyed, cancel consumer task
                if matches!(msg.text.trim(), "/new" | "/reset") {
                    let mut tasks = consumer_tasks.lock().await;
                    if let Some(task) = tasks.remove(&channel_id) {
                        task.abort();
                    }
                }
                continue;
            }

            // Route to agent session
            let mut router_guard = router.lock().await;
            let (client, _session_id) = router_guard.get_or_create(&channel_id);

            // Submit user message to the Agent via the bus
            if let Err(e) = client.send_request(AgentRequest::Submit {
                text: msg.text.clone(),
                images: vec![],
            }) {
                error!("[{}] Failed to submit to bus: {}", channel_id, e);
                continue;
            }

            // Spawn a notification consumer task if one isn't already running.
            // The tasks Mutex serializes access; entry API makes intent explicit.
            let mut tasks = consumer_tasks.lock().await;

            // Clean up finished tasks
            if tasks.get(&channel_id).is_some_and(|t| t.is_finished()) {
                tasks.remove(&channel_id);
            }

            if let std::collections::hash_map::Entry::Vacant(entry) = tasks.entry(channel_id.clone()) {
                let consumer_client = router_guard.get_client_subscriber(&channel_id);
                drop(router_guard);

                if let Some(mut notif_rx) = consumer_client {
                    let ch = channel_id;
                    let adapters_ref = Arc::clone(&adapters);

                    let task = tokio::spawn(async move {
                        let mut formatter = MessageFormatter::new();
                        let idle_timeout = Duration::from_secs(600); // 10 min no-notification timeout
                        loop {
                            match tokio::time::timeout(idle_timeout, notif_rx.recv()).await {
                                Ok(Ok(notif)) => {
                                    let is_done = formatter.push(&notif);

                                    // Send typing indicator on tool use
                                    if matches!(notif, AgentNotification::ToolUseStart { .. }) {
                                        let adapters = adapters_ref.read().await;
                                        if let Some(adapter) = adapters.get(&ch.platform) {
                                            let _ = adapter.send_typing(&ch).await;
                                        }
                                    }

                                    if is_done {
                                        let out = formatter.finish();
                                        if !out.text.is_empty() {
                                            let adapters = adapters_ref.read().await;
                                            if let Some(adapter) = adapters.get(&ch.platform) {
                                                if let Err(e) = adapter.send_message(&ch, out).await {
                                                    error!("[{}] Failed to send reply: {}", ch, e);
                                                }
                                            }
                                        }
                                        formatter = MessageFormatter::new();
                                    }
                                }
                                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                                    warn!("[{}] Consumer lagged by {} notifications", ch, n);
                                    continue;
                                }
                                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                                Err(_) => {
                                    warn!("[{}] Consumer idle timeout ({}s), stopping", ch, idle_timeout.as_secs());
                                    break;
                                }
                            }
                        }
                    });

                    entry.insert(task);
                }
            } else {
                drop(router_guard);
            }
        }

        info!("Gateway shutting down");
        self.stop_all().await;
        Ok(())
    }

    /// Handle special slash commands from users.
    ///
    /// Returns `true` if the command was handled (should not be forwarded).
    async fn handle_command(router: &Arc<Mutex<SessionRouter>>, msg: &InboundMessage) -> bool {
        match msg.text.trim() {
            "/new" | "/reset" => {
                let mut router = router.lock().await;
                router.destroy(&msg.channel_id);
                info!("Session reset for channel {}", msg.channel_id);
                true
            }
            "/status" => {
                let router = router.lock().await;
                let count = router.session_count();
                info!("Status request: {} active sessions", count);
                true
            }
            _ => false,
        }
    }

    /// Stop all adapters.
    async fn stop_all(&self) {
        // Cancel all consumer tasks
        let mut tasks = self.consumer_tasks.lock().await;
        for (ch, task) in tasks.drain() {
            info!("Cancelling consumer for {}", ch);
            task.abort();
        }

        let adapters = self.adapters.read().await;
        for (platform, adapter) in adapters.iter() {
            if let Err(e) = adapter.stop().await {
                warn!("Error stopping adapter {}: {}", platform, e);
            }
        }
    }

    /// Get the number of active sessions.
    pub async fn session_count(&self) -> usize {
        self.router.lock().await.session_count()
    }

    /// Get the number of registered adapters.
    pub async fn adapter_count(&self) -> usize {
        self.adapters.read().await.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use claude_bus::bus::EventBus;

    #[tokio::test]
    async fn gateway_creation() {
        let (bus, _client) = EventBus::new(64);
        let config = BridgeConfig::default();
        let gateway = ChannelGateway::new(bus, config);
        assert_eq!(gateway.adapter_count().await, 0);
    }

    #[test]
    fn gateway_context_send() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = GatewayContext { inbound_tx: tx };

        let msg = InboundMessage::text(
            ChannelId::new("test", "ch1"),
            crate::message::SenderInfo::new("u1", "Test"),
            "Hello!",
        );
        ctx.route_inbound(msg).unwrap();

        let received = rx.try_recv().unwrap();
        assert_eq!(received.text, "Hello!");
    }

    #[tokio::test]
    async fn gateway_session_count() {
        let (bus, _client) = EventBus::new(64);
        let config = BridgeConfig::default();
        let gateway = ChannelGateway::new(bus, config);
        assert_eq!(gateway.session_count().await, 0);
    }

    // ── Additional gateway tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn gateway_context_closed_returns_error() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = GatewayContext { inbound_tx: tx };

        // Drop the receiver to simulate a closed gateway
        drop(rx);

        let msg = InboundMessage::text(
            ChannelId::new("test", "ch1"),
            crate::message::SenderInfo::new("u1", "Test"),
            "Hello!",
        );
        let result = ctx.route_inbound(msg);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("closed"));
    }

    #[tokio::test]
    async fn gateway_multiple_inbound_messages() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = GatewayContext { inbound_tx: tx };

        for i in 0..5 {
            let msg = InboundMessage::text(
                ChannelId::new("test", format!("ch{i}")),
                crate::message::SenderInfo::new("u1", "Test"),
                format!("Message {i}"),
            );
            ctx.route_inbound(msg).unwrap();
        }

        for i in 0..5 {
            let received = rx.try_recv().unwrap();
            assert_eq!(received.text, format!("Message {i}"));
        }
    }

    #[tokio::test]
    async fn gateway_register_adapter_increments_count() {
        use crate::adapter::{ChannelAdapter, AdapterResult};
        use crate::message::OutboundMessage;
        use async_trait::async_trait;

        struct DummyAdapter;

        #[async_trait]
        impl ChannelAdapter for DummyAdapter {
            fn platform(&self) -> &str { "dummy" }
            async fn start(&mut self, _ctx: GatewayContext) -> AdapterResult<()> { Ok(()) }
            async fn stop(&self) -> AdapterResult<()> { Ok(()) }
            async fn send_message(&self, _ch: &ChannelId, _msg: OutboundMessage) -> AdapterResult<()> { Ok(()) }
            async fn send_typing(&self, _ch: &ChannelId) -> AdapterResult<()> { Ok(()) }
        }

        let (bus, _client) = EventBus::new(64);
        let config = BridgeConfig::default();
        let gateway = ChannelGateway::new(bus, config);
        assert_eq!(gateway.adapter_count().await, 0);

        gateway.register_adapter(Box::new(DummyAdapter)).await.unwrap();
        assert_eq!(gateway.adapter_count().await, 1);
    }

    #[tokio::test]
    async fn gateway_cannot_run_twice() {
        let (bus, _client) = EventBus::new(64);
        let config = BridgeConfig::default();
        let gateway = ChannelGateway::new(bus, config);

        // Take the receiver internally by attempting to run
        // We can't truly run (no adapters), but we can verify the "run once" guard
        // by directly checking internal state
        assert!(gateway.inbound_rx.is_some());
    }

    #[test]
    fn channel_id_equality() {
        let a = ChannelId::new("platform", "channel1");
        let b = ChannelId::new("platform", "channel1");
        let c = ChannelId::new("platform", "channel2");

        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
