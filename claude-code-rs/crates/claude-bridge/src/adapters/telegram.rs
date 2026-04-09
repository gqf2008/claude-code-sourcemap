//! Telegram adapter — connects to Telegram Bot API.
//!
//! Supports:
//! - Long-polling (getUpdates) mode
//! - Webhook mode
//! - Markdown message formatting
//!
//! # Setup
//!
//! 1. Create a bot via @BotFather on Telegram
//! 2. Copy the bot token
//! 3. Configure `BridgeConfig.telegram` with the bot token

use async_trait::async_trait;
use tokio::sync::{watch, Mutex};
use tracing::{error, info, warn};

use crate::adapter::{AdapterError, AdapterResult, ChannelAdapter};
use crate::config::TelegramConfig;
use crate::gateway::GatewayContext;
use crate::message::{ChannelId, InboundMessage, OutboundMessage, SenderInfo};

/// Telegram Bot API base URL.
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// Telegram adapter state.
pub struct TelegramAdapter {
    config: TelegramConfig,
    http: reqwest::Client,
    /// Gateway context (set on start).
    ctx: Option<GatewayContext>,
    /// Polling task handle + cancel signal.
    /// Wrapped in Mutex so `stop(&self)` can take ownership.
    poll_task: Mutex<Option<(tokio::task::JoinHandle<()>, watch::Sender<bool>)>>,
}

#[allow(dead_code)]
impl TelegramAdapter {
    /// Create a new Telegram adapter.
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            ctx: None,
            poll_task: Mutex::new(None),
        }
    }

    /// Build the API URL for a method.
    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", TELEGRAM_API_BASE, self.config.bot_token, method)
    }

    /// Send a text message to a chat.
    async fn send_text(&self, chat_id: &str, text: &str) -> AdapterResult<String> {
        let resp = self.http.post(self.api_url("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
                "parse_mode": "Markdown",
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let desc = body.get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(AdapterError::PlatformApi(format!("Telegram API: {}", desc)));
        }

        let message_id = body
            .pointer("/result/message_id")
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string())
            .unwrap_or_default();

        Ok(message_id)
    }

    /// Send a typing action to a chat.
    async fn send_chat_action(&self, chat_id: &str) -> AdapterResult<()> {
        let _ = self.http.post(self.api_url("sendChatAction"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "action": "typing",
            }))
            .send()
            .await?;
        Ok(())
    }

    /// Parse a Telegram update into an InboundMessage.
    pub fn parse_update(update: &serde_json::Value) -> Option<InboundMessage> {
        let message = update.get("message")?;
        let text = message.get("text").and_then(|v| v.as_str())?;
        let chat = message.get("chat")?;
        let chat_id = chat.get("id").and_then(|v| v.as_i64())?;
        let from = message.get("from")?;
        let user_id = from.get("id").and_then(|v| v.as_i64())?;

        let first_name = from.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
        let last_name = from.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = format!("{} {}", first_name, last_name).trim().to_string();

        let message_id = message.get("message_id")
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string());

        let reply_to = message.get("reply_to_message")
            .and_then(|v| v.get("message_id"))
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string());

        Some(InboundMessage {
            channel_id: ChannelId::new("telegram", chat_id.to_string()),
            sender: SenderInfo::new(user_id.to_string(), display_name),
            text: text.to_string(),
            attachments: vec![],
            message_id,
            reply_to,
            raw: Some(update.clone()),
        })
    }

    /// Check if a chat is allowed by the config.
    fn is_chat_allowed(&self, chat_id: i64) -> bool {
        match &self.config.allowed_chat_ids {
            Some(ids) if !ids.is_empty() => ids.contains(&chat_id),
            _ => true, // Allow all if not configured
        }
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn platform(&self) -> &str {
        "telegram"
    }

    async fn start(&mut self, ctx: GatewayContext) -> AdapterResult<()> {
        self.ctx = Some(ctx.clone());

        let use_webhook = self.config.use_webhook.unwrap_or(false);
        if use_webhook {
            info!("Telegram adapter started (webhook mode)");
            // Webhook mode: messages come via HTTP, similar to Feishu
        } else {
            info!("Telegram adapter started (polling mode)");
            // Start polling in a background task with cancellation support
            let http = self.http.clone();
            let api_url = self.api_url("getUpdates");
            let allowed_chat_ids = self.config.allowed_chat_ids.clone();
            let (cancel_tx, mut cancel_rx) = watch::channel(false);

            let poll_task = tokio::spawn(async move {
                let mut offset: Option<i64> = None;

                loop {
                    let mut params = serde_json::json!({
                        "timeout": 30,
                    });
                    if let Some(off) = offset {
                        params["offset"] = serde_json::json!(off);
                    }

                    // Race: poll request vs cancellation
                    let poll_result = tokio::select! {
                        biased;
                        _ = cancel_rx.changed() => {
                            if *cancel_rx.borrow() {
                                info!("Telegram polling cancelled gracefully");
                                break;
                            }
                            continue;
                        }
                        result = http.post(&api_url).json(&params).send() => result,
                    };

                    match poll_result {
                        Ok(resp) => {
                            if let Ok(body) = resp.json::<serde_json::Value>().await {
                                if let Some(updates) = body.get("result").and_then(|v| v.as_array()) {
                                    for update in updates {
                                        if let Some(update_id) = update.get("update_id").and_then(|v| v.as_i64()) {
                                            offset = Some(update_id + 1);
                                        }
                                        if let Some(msg) = TelegramAdapter::parse_update(update) {
                                            // Check allowed chats
                                            if let Some(ref ids) = allowed_chat_ids {
                                                if !ids.is_empty() {
                                                    if let Ok(cid) = msg.channel_id.channel.parse::<i64>() {
                                                        if !ids.contains(&cid) {
                                                            continue;
                                                        }
                                                    }
                                                }
                                            }
                                            if let Err(e) = ctx.route_inbound(msg) {
                                                error!("Failed to route Telegram message: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("Telegram polling error: {}", e);
                            // Back off, but check for cancellation during sleep
                            tokio::select! {
                                biased;
                                _ = cancel_rx.changed() => {
                                    if *cancel_rx.borrow() {
                                        info!("Telegram polling cancelled during backoff");
                                        break;
                                    }
                                }
                                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                            }
                        }
                    }
                }
            });

            *self.poll_task.lock().await = Some((poll_task, cancel_tx));
        }

        Ok(())
    }

    async fn send_message(&self, channel: &ChannelId, msg: OutboundMessage) -> AdapterResult<()> {
        self.send_text(&channel.channel, &msg.text).await?;
        Ok(())
    }

    async fn send_typing(&self, channel: &ChannelId) -> AdapterResult<()> {
        self.send_chat_action(&channel.channel).await
    }

    async fn stop(&self) -> AdapterResult<()> {
        if let Some((mut task, cancel_tx)) = self.poll_task.lock().await.take() {
            // Signal graceful shutdown
            let _ = cancel_tx.send(true);
            // Wait for clean exit, then force-abort if stuck
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {
                    warn!("Telegram polling task did not stop in 3s, aborting");
                    task.abort();
                }
            }
        }
        info!("Telegram adapter stopped");
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_telegram_update() {
        let update = serde_json::json!({
            "update_id": 123456,
            "message": {
                "message_id": 42,
                "chat": { "id": 789, "type": "private" },
                "from": {
                    "id": 111,
                    "first_name": "Alice",
                    "last_name": "Smith"
                },
                "text": "Hello bot!"
            }
        });

        let msg = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(msg.text, "Hello bot!");
        assert_eq!(msg.channel_id.platform, "telegram");
        assert_eq!(msg.channel_id.channel, "789");
        assert_eq!(msg.sender.user_id, "111");
        assert_eq!(msg.sender.display_name, "Alice Smith");
        assert_eq!(msg.message_id.as_deref(), Some("42"));
    }

    #[test]
    fn parse_telegram_reply() {
        let update = serde_json::json!({
            "update_id": 123457,
            "message": {
                "message_id": 43,
                "chat": { "id": 789, "type": "private" },
                "from": { "id": 111, "first_name": "Alice" },
                "text": "Reply text",
                "reply_to_message": {
                    "message_id": 42,
                    "text": "Original"
                }
            }
        });

        let msg = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(msg.reply_to.as_deref(), Some("42"));
    }

    #[test]
    fn parse_telegram_update_no_text() {
        let update = serde_json::json!({
            "update_id": 123458,
            "message": {
                "message_id": 44,
                "chat": { "id": 789 },
                "from": { "id": 111 },
                "photo": [{}]
            }
        });

        assert!(TelegramAdapter::parse_update(&update).is_none());
    }

    #[test]
    fn telegram_adapter_platform() {
        let config = TelegramConfig {
            bot_token: "123:ABC".into(),
            use_webhook: None,
            webhook_url: None,
            allowed_chat_ids: None,
        };
        let adapter = TelegramAdapter::new(config);
        assert_eq!(adapter.platform(), "telegram");
    }

    #[test]
    fn chat_allowed_check() {
        let config = TelegramConfig {
            bot_token: "token".into(),
            use_webhook: None,
            webhook_url: None,
            allowed_chat_ids: Some(vec![100, 200]),
        };
        let adapter = TelegramAdapter::new(config);
        assert!(adapter.is_chat_allowed(100));
        assert!(adapter.is_chat_allowed(200));
        assert!(!adapter.is_chat_allowed(300));
    }

    #[test]
    fn chat_allowed_empty_allows_all() {
        let config = TelegramConfig {
            bot_token: "token".into(),
            use_webhook: None,
            webhook_url: None,
            allowed_chat_ids: None,
        };
        let adapter = TelegramAdapter::new(config);
        assert!(adapter.is_chat_allowed(999));
    }

    #[test]
    fn api_url_generation() {
        let config = TelegramConfig {
            bot_token: "123:ABC".into(),
            use_webhook: None,
            webhook_url: None,
            allowed_chat_ids: None,
        };
        let adapter = TelegramAdapter::new(config);
        assert_eq!(
            adapter.api_url("sendMessage"),
            "https://api.telegram.org/bot123:ABC/sendMessage"
        );
    }
}
