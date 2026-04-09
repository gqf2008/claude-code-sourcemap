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

use tokio::sync::Mutex;
use tracing::{error, info, warn};

use claude_bus::bus::BusHandle;
use claude_bus::events::{AgentNotification, AgentRequest};

use crate::adapter::{AdapterResult, ChannelAdapter};
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

/// The main gateway that manages adapters and routes messages.
pub struct ChannelGateway {
    /// Registered adapters by platform name (Arc for sharing with consumer tasks).
    adapters: Arc<HashMap<String, Arc<Box<dyn ChannelAdapter>>>>,
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
            adapters: Arc::new(HashMap::new()),
            router: Arc::new(Mutex::new(router)),
            inbound_tx,
            inbound_rx: Some(inbound_rx),
            consumer_tasks: Arc::new(Mutex::new(HashMap::new())),
            _config: config,
        }
    }

    /// Register a channel adapter.
    pub fn register_adapter(&mut self, adapter: Box<dyn ChannelAdapter>) {
        let platform = adapter.platform().to_string();
        info!("Registered adapter: {}", platform);
        Arc::get_mut(&mut self.adapters)
            .expect("register_adapter must be called before run()")
            .insert(platform, Arc::new(adapter));
    }

    /// Start all registered adapters and begin message routing.
    ///
    /// This blocks until `shutdown()` is called or all adapters stop.
    pub async fn run(&mut self) -> AdapterResult<()> {
        let ctx = GatewayContext {
            inbound_tx: self.inbound_tx.clone(),
        };

        // Start all adapters
        for (platform, adapter) in Arc::get_mut(&mut self.adapters)
            .expect("run() must not be called concurrently")
            .iter_mut()
        {
            info!("Starting adapter: {}", platform);
            Arc::get_mut(adapter)
                .expect("adapter must not be shared yet")
                .start(ctx.clone()).await?;
        }

        // Process inbound messages
        let mut inbound_rx = self.inbound_rx.take()
            .expect("Gateway can only be run once");

        let router = Arc::clone(&self.router);
        let adapters = Arc::clone(&self.adapters);
        let consumer_tasks = Arc::clone(&self.consumer_tasks);

        info!("Gateway running with {} adapters", self.adapters.len());

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
                        while let Ok(notif) = notif_rx.recv().await {
                            let is_done = formatter.push(&notif);

                            // Send typing indicator on tool use
                            if matches!(notif, AgentNotification::ToolUseStart { .. }) {
                                if let Some(adapter) = adapters_ref.get(&ch.platform) {
                                    let _ = adapter.send_typing(&ch).await;
                                }
                            }

                            if is_done {
                                let out = formatter.finish();
                                if !out.text.is_empty() {
                                    if let Some(adapter) = adapters_ref.get(&ch.platform) {
                                        if let Err(e) = adapter.send_message(&ch, out).await {
                                            error!("[{}] Failed to send reply: {}", ch, e);
                                        }
                                    }
                                }
                                formatter = MessageFormatter::new();
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

        for (platform, adapter) in self.adapters.as_ref() {
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
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use claude_bus::bus::EventBus;

    #[test]
    fn gateway_creation() {
        let (bus, _client) = EventBus::new(64);
        let config = BridgeConfig::default();
        let gateway = ChannelGateway::new(bus, config);
        assert_eq!(gateway.adapter_count(), 0);
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
}
