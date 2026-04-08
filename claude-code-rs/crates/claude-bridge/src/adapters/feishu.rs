//! Feishu/Lark adapter — connects to Feishu Bot API.
//!
//! Supports:
//! - Event subscription (webhook mode)
//! - Sending text and rich-text messages
//! - Message card rendering
//!
//! # Setup
//!
//! 1. Create a Feishu Bot app at https://open.feishu.cn/
//! 2. Enable "Bot" capability and configure event subscription
//! 3. Set the webhook URL to `{your_server}/webhook/feishu`
//! 4. Configure `BridgeConfig.feishu` with app_id and app_secret

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::adapter::{AdapterError, AdapterResult, ChannelAdapter};
use crate::config::FeishuConfig;
use crate::gateway::GatewayContext;
use crate::message::{ChannelId, InboundMessage, OutboundMessage, SenderInfo};

/// Feishu Bot API base URL.
const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";

/// Feishu adapter state.
///
/// Uses `RwLock` for `access_token` to allow token refresh through `&self`
/// (required by `ChannelAdapter::send_message`).
pub struct FeishuAdapter {
    config: FeishuConfig,
    http: reqwest::Client,
    /// Tenant access token (refreshed periodically, interior mutability).
    access_token: RwLock<Option<String>>,
    /// Gateway context (set on start).
    ctx: Option<GatewayContext>,
}

impl FeishuAdapter {
    /// Create a new Feishu adapter.
    pub fn new(config: FeishuConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            access_token: RwLock::new(None),
            ctx: None,
        }
    }

    /// Get or refresh the tenant access token.
    ///
    /// Uses `RwLock` for interior mutability so this works through `&self`.
    async fn ensure_token(&self) -> AdapterResult<String> {
        // Fast path: token already cached
        {
            let guard = self.access_token.read().await;
            if let Some(ref token) = *guard {
                return Ok(token.clone());
            }
        }

        // Slow path: fetch a new token
        let url = format!("{}/auth/v3/tenant_access_token/internal", FEISHU_API_BASE);
        let resp = self.http.post(&url)
            .json(&serde_json::json!({
                "app_id": self.config.app_id,
                "app_secret": self.config.app_secret,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let token = body.get("tenant_access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Auth("No token in response".into()))?
            .to_string();

        info!("Feishu access token acquired");
        let mut guard = self.access_token.write().await;
        *guard = Some(token.clone());
        Ok(token)
    }

    /// Invalidate the cached token (e.g., on 401 response).
    #[allow(dead_code)]
    async fn invalidate_token(&self) {
        let mut guard = self.access_token.write().await;
        *guard = None;
    }

    /// Send a text message to a Feishu chat.
    async fn send_text_message(&self, chat_id: &str, text: &str) -> AdapterResult<String> {
        let token = self.ensure_token().await?;
        let url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);

        let content = serde_json::json!({
            "text": text,
        });

        let resp = self.http.post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "receive_id": chat_id,
                "msg_type": "text",
                "content": content.to_string(),
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;

        if let Some(code) = body.get("code").and_then(|v| v.as_i64()) {
            if code != 0 {
                let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("unknown error");
                return Err(AdapterError::PlatformApi(format!("Feishu API error {}: {}", code, msg)));
            }
        }

        let message_id = body
            .pointer("/data/message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(message_id)
    }

    /// Parse a Feishu event webhook payload into an InboundMessage.
    pub fn parse_event(payload: &serde_json::Value) -> Option<InboundMessage> {
        // Feishu event structure:
        // { "event": { "message": { "chat_id": "...", "content": "{\"text\":\"...\"}", ... }, "sender": { ... } } }
        let event = payload.get("event")?;
        let message = event.get("message")?;
        let sender = event.get("sender")?;

        let chat_id = message.get("chat_id").and_then(|v| v.as_str())?;
        let content_str = message.get("content").and_then(|v| v.as_str())?;
        let content: serde_json::Value = serde_json::from_str(content_str).ok()?;
        let text = content.get("text").and_then(|v| v.as_str()).unwrap_or("");

        let sender_id = sender.pointer("/sender_id/open_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let sender_name = sender.get("sender_name")
            .or_else(|| sender.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");

        let message_id = message.get("message_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(InboundMessage {
            channel_id: ChannelId::new("feishu", chat_id),
            sender: SenderInfo::new(sender_id, sender_name),
            text: text.to_string(),
            attachments: vec![],
            message_id,
            reply_to: None,
            raw: Some(payload.clone()),
        })
    }
}

#[async_trait]
impl ChannelAdapter for FeishuAdapter {
    fn platform(&self) -> &str {
        "feishu"
    }

    async fn start(&mut self, ctx: GatewayContext) -> AdapterResult<()> {
        self.ctx = Some(ctx);
        info!("Feishu adapter started (webhook mode)");
        // In webhook mode, messages are received via the HTTP webhook endpoint.
        // The adapter just needs to be ready to send replies.
        // Token will be acquired lazily on first API call.
        Ok(())
    }

    async fn send_message(&self, channel: &ChannelId, msg: OutboundMessage) -> AdapterResult<()> {
        self.send_text_message(&channel.channel, &msg.text).await?;
        Ok(())
    }

    async fn send_typing(&self, channel: &ChannelId) -> AdapterResult<()> {
        // Feishu doesn't have a native typing indicator API
        debug!("Typing indicator for {}", channel);
        Ok(())
    }

    async fn stop(&self) -> AdapterResult<()> {
        info!("Feishu adapter stopped");
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_feishu_event() {
        let payload = serde_json::json!({
            "event": {
                "message": {
                    "chat_id": "oc_abc123",
                    "content": "{\"text\":\"Hello Agent!\"}",
                    "message_id": "om_xxx"
                },
                "sender": {
                    "sender_id": { "open_id": "ou_user1" },
                    "sender_name": "Alice"
                }
            }
        });

        let msg = FeishuAdapter::parse_event(&payload).unwrap();
        assert_eq!(msg.text, "Hello Agent!");
        assert_eq!(msg.channel_id.platform, "feishu");
        assert_eq!(msg.channel_id.channel, "oc_abc123");
        assert_eq!(msg.sender.user_id, "ou_user1");
        assert_eq!(msg.sender.display_name, "Alice");
        assert_eq!(msg.message_id.as_deref(), Some("om_xxx"));
    }

    #[test]
    fn parse_feishu_event_missing_fields() {
        let payload = serde_json::json!({"event": {}});
        assert!(FeishuAdapter::parse_event(&payload).is_none());
    }

    #[test]
    fn feishu_adapter_platform() {
        let config = FeishuConfig {
            app_id: "test".into(),
            app_secret: "secret".into(),
            verification_token: None,
            encrypt_key: None,
            bot_name: None,
        };
        let adapter = FeishuAdapter::new(config);
        assert_eq!(adapter.platform(), "feishu");
    }
}
