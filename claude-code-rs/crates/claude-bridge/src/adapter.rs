//! ChannelAdapter trait — platform integration interface.
//!
//! Each external messaging platform (Feishu, Telegram, etc.) implements
//! this trait to handle inbound messages and send outbound replies.

use async_trait::async_trait;

use crate::gateway::GatewayContext;
use crate::message::{ChannelId, OutboundMessage};

/// Errors from adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("Platform API error: {0}")]
    PlatformApi(String),
    #[error("Authentication error: {0}")]
    Auth(String),
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Adapter error: {0}")]
    Other(String),
}

pub type AdapterResult<T> = Result<T, AdapterError>;

/// Platform-specific channel adapter.
///
/// Adapters are responsible for:
/// - Receiving messages from the platform (via webhook or polling)
/// - Sending messages back to the platform
/// - Managing platform-specific authentication and rate limiting
#[async_trait]
pub trait ChannelAdapter: Send + Sync + 'static {
    /// Platform identifier (e.g., "feishu", "telegram", "wechat", "dingtalk").
    fn platform(&self) -> &str;

    /// Start the adapter.
    ///
    /// This may register webhooks, start long-polling, or set up event listeners.
    /// The `ctx` provides access to the gateway for routing inbound messages.
    async fn start(&mut self, ctx: GatewayContext) -> AdapterResult<()>;

    /// Send a message to a specific channel on the platform.
    async fn send_message(&self, channel: &ChannelId, msg: OutboundMessage) -> AdapterResult<()>;

    /// Send a typing indicator to a channel (optional).
    ///
    /// Platforms that don't support typing indicators can use the default no-op.
    async fn send_typing(&self, _channel: &ChannelId) -> AdapterResult<()> {
        Ok(())
    }

    /// Update a previously sent message (for streaming edits).
    ///
    /// `message_id` is the platform-specific message ID returned from `send_message`.
    /// Platforms that don't support message editing can use the default no-op.
    async fn update_message(
        &self,
        _channel: &ChannelId,
        _message_id: &str,
        _msg: OutboundMessage,
    ) -> AdapterResult<()> {
        Ok(())
    }

    /// Stop the adapter gracefully.
    async fn stop(&self) -> AdapterResult<()>;
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_error_display() {
        let err = AdapterError::PlatformApi("rate limited".into());
        assert_eq!(err.to_string(), "Platform API error: rate limited");

        let err = AdapterError::Auth("invalid token".into());
        assert_eq!(err.to_string(), "Authentication error: invalid token");

        let err = AdapterError::Other("something went wrong".into());
        assert_eq!(err.to_string(), "Adapter error: something went wrong");
    }

    #[test]
    fn adapter_result_ok() {
        let r: AdapterResult<i32> = Ok(42);
        assert!(matches!(r, Ok(42)));
    }
}
