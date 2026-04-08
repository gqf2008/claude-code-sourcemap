//! Gateway — orchestrates adapters, routing, and message flow.
//!
//! The gateway manages all channel adapters and coordinates message
//! routing between external platforms and the Agent via the Event Bus.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{error, info, warn};

use claude_bus::bus::BusHandle;
use claude_bus::events::AgentRequest;

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
    /// Registered adapters by platform name.
    adapters: HashMap<String, Box<dyn ChannelAdapter>>,
    /// Session router (shared across message handling tasks).
    router: Arc<Mutex<SessionRouter>>,
    /// Active message formatters per channel.
    formatters: Arc<Mutex<HashMap<ChannelId, MessageFormatter>>>,
    /// Inbound message channel.
    inbound_tx: tokio::sync::mpsc::UnboundedSender<InboundMessage>,
    inbound_rx: Option<tokio::sync::mpsc::UnboundedReceiver<InboundMessage>>,
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
            adapters: HashMap::new(),
            router: Arc::new(Mutex::new(router)),
            formatters: Arc::new(Mutex::new(HashMap::new())),
            inbound_tx,
            inbound_rx: Some(inbound_rx),
            _config: config,
        }
    }

    /// Register a channel adapter.
    pub fn register_adapter(&mut self, adapter: Box<dyn ChannelAdapter>) {
        let platform = adapter.platform().to_string();
        info!("Registered adapter: {}", platform);
        self.adapters.insert(platform, adapter);
    }

    /// Start all registered adapters and begin message routing.
    ///
    /// This blocks until `shutdown()` is called or all adapters stop.
    pub async fn run(&mut self) -> AdapterResult<()> {
        let ctx = GatewayContext {
            inbound_tx: self.inbound_tx.clone(),
        };

        // Start all adapters
        for (platform, adapter) in &mut self.adapters {
            info!("Starting adapter: {}", platform);
            adapter.start(ctx.clone()).await?;
        }

        // Process inbound messages
        let mut inbound_rx = self.inbound_rx.take()
            .expect("Gateway can only be run once");

        let router = Arc::clone(&self.router);
        let formatters = Arc::clone(&self.formatters);

        info!("Gateway running with {} adapters", self.adapters.len());

        while let Some(msg) = inbound_rx.recv().await {
            let channel_id = msg.channel_id.clone();

            // Handle special commands
            if msg.text.starts_with('/')
                && self.handle_command(&msg).await
            {
                continue;
            }

            // Route to agent session
            let mut router = router.lock().await;
            let (client, session_id) = router.get_or_create(&channel_id);
            let session_id = session_id.to_string();

            // Submit as AgentRequest
            if let Err(e) = client.send_request(AgentRequest::Submit {
                text: msg.text.clone(),
                images: vec![],
            }) {
                error!("[{}] Failed to submit to bus: {}", session_id, e);
                continue;
            }

            // Create a formatter for this channel if needed
            let mut fmts = formatters.lock().await;
            fmts.entry(channel_id.clone())
                .or_insert_with(MessageFormatter::new);

            drop(router);
            drop(fmts);
        }

        info!("Gateway shutting down");
        self.stop_all().await;
        Ok(())
    }

    /// Handle special slash commands from users.
    ///
    /// Returns `true` if the command was handled (should not be forwarded).
    async fn handle_command(&self, msg: &InboundMessage) -> bool {
        match msg.text.trim() {
            "/new" | "/reset" => {
                let mut router = self.router.lock().await;
                router.destroy(&msg.channel_id);
                info!("Session reset for channel {}", msg.channel_id);
                true
            }
            "/status" => {
                let router = self.router.lock().await;
                let count = router.session_count();
                info!("Status request: {} active sessions", count);
                true
            }
            _ => false,
        }
    }

    /// Stop all adapters.
    async fn stop_all(&self) {
        for (platform, adapter) in &self.adapters {
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
