//! Bridge configuration — credentials and settings for adapters.

use serde::{Deserialize, Serialize};

/// Top-level bridge configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// Webhook server listen address (e.g., "0.0.0.0:8080").
    pub webhook_addr: Option<String>,

    /// Session idle timeout in seconds (default: 3600).
    pub session_idle_timeout_secs: Option<u64>,

    /// Feishu/Lark adapter config.
    pub feishu: Option<FeishuConfig>,

    /// Telegram adapter config.
    pub telegram: Option<TelegramConfig>,

    /// WeChat Work adapter config.
    pub wechat: Option<WechatConfig>,

    /// DingTalk adapter config.
    pub dingtalk: Option<DingtalkConfig>,
}

/// Feishu/Lark adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    /// App ID.
    pub app_id: String,
    /// App Secret.
    pub app_secret: String,
    /// Verification token for webhook events.
    pub verification_token: Option<String>,
    /// Encrypt key for webhook events.
    pub encrypt_key: Option<String>,
    /// Bot name (displayed in messages).
    pub bot_name: Option<String>,
}

/// Telegram adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Bot token from @BotFather.
    pub bot_token: String,
    /// Use webhook mode instead of polling.
    pub use_webhook: Option<bool>,
    /// Webhook URL (required if use_webhook is true).
    pub webhook_url: Option<String>,
    /// Allowed chat IDs (empty = allow all).
    pub allowed_chat_ids: Option<Vec<i64>>,
}

/// WeChat Work adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WechatConfig {
    /// Corp ID.
    pub corp_id: String,
    /// Agent ID.
    pub agent_id: String,
    /// Agent Secret.
    pub agent_secret: String,
    /// Token for message callback.
    pub token: Option<String>,
    /// Encoding AES key.
    pub encoding_aes_key: Option<String>,
}

/// DingTalk adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingtalkConfig {
    /// App Key.
    pub app_key: String,
    /// App Secret.
    pub app_secret: String,
    /// Robot code.
    pub robot_code: Option<String>,
}

impl BridgeConfig {
    /// Load configuration from environment variables.
    ///
    /// Environment variable naming convention:
    /// - `BRIDGE_WEBHOOK_ADDR` → webhook_addr
    /// - `BRIDGE_FEISHU_APP_ID` → feishu.app_id
    /// - `BRIDGE_TELEGRAM_BOT_TOKEN` → telegram.bot_token
    /// - etc.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(addr) = std::env::var("BRIDGE_WEBHOOK_ADDR") {
            config.webhook_addr = Some(addr);
        }

        if let Ok(timeout) = std::env::var("BRIDGE_SESSION_IDLE_TIMEOUT") {
            if let Ok(secs) = timeout.parse() {
                config.session_idle_timeout_secs = Some(secs);
            }
        }

        // Feishu
        if let (Ok(app_id), Ok(app_secret)) = (
            std::env::var("BRIDGE_FEISHU_APP_ID"),
            std::env::var("BRIDGE_FEISHU_APP_SECRET"),
        ) {
            config.feishu = Some(FeishuConfig {
                app_id,
                app_secret,
                verification_token: std::env::var("BRIDGE_FEISHU_VERIFICATION_TOKEN").ok(),
                encrypt_key: std::env::var("BRIDGE_FEISHU_ENCRYPT_KEY").ok(),
                bot_name: std::env::var("BRIDGE_FEISHU_BOT_NAME").ok(),
            });
        }

        // Telegram
        if let Ok(bot_token) = std::env::var("BRIDGE_TELEGRAM_BOT_TOKEN") {
            config.telegram = Some(TelegramConfig {
                bot_token,
                use_webhook: std::env::var("BRIDGE_TELEGRAM_USE_WEBHOOK")
                    .ok()
                    .map(|v| v == "true" || v == "1"),
                webhook_url: std::env::var("BRIDGE_TELEGRAM_WEBHOOK_URL").ok(),
                allowed_chat_ids: None,
            });
        }

        config
    }

    /// Load configuration from a JSON file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Check which adapters have valid configuration.
    pub fn enabled_platforms(&self) -> Vec<&str> {
        let mut platforms = vec![];
        if self.feishu.is_some() { platforms.push("feishu"); }
        if self.telegram.is_some() { platforms.push("telegram"); }
        if self.wechat.is_some() { platforms.push("wechat"); }
        if self.dingtalk.is_some() { platforms.push("dingtalk"); }
        platforms
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = BridgeConfig::default();
        assert!(config.webhook_addr.is_none());
        assert!(config.feishu.is_none());
        assert!(config.telegram.is_none());
        assert!(config.enabled_platforms().is_empty());
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = BridgeConfig {
            webhook_addr: Some("0.0.0.0:8080".into()),
            session_idle_timeout_secs: Some(7200),
            feishu: Some(FeishuConfig {
                app_id: "cli_xxx".into(),
                app_secret: "secret".into(),
                verification_token: None,
                encrypt_key: None,
                bot_name: Some("TestBot".into()),
            }),
            telegram: Some(TelegramConfig {
                bot_token: "123:ABC".into(),
                use_webhook: Some(false),
                webhook_url: None,
                allowed_chat_ids: Some(vec![12345]),
            }),
            wechat: None,
            dingtalk: None,
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: BridgeConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.webhook_addr.as_deref(), Some("0.0.0.0:8080"));
        assert!(deserialized.feishu.is_some());
        assert!(deserialized.telegram.is_some());
        assert!(deserialized.wechat.is_none());

        let platforms = deserialized.enabled_platforms();
        assert_eq!(platforms, vec!["feishu", "telegram"]);
    }

    #[test]
    fn enabled_platforms() {
        let mut config = BridgeConfig::default();
        assert!(config.enabled_platforms().is_empty());

        config.telegram = Some(TelegramConfig {
            bot_token: "token".into(),
            use_webhook: None,
            webhook_url: None,
            allowed_chat_ids: None,
        });
        assert_eq!(config.enabled_platforms(), vec!["telegram"]);
    }

    #[test]
    fn session_idle_timeout_default() {
        let config = BridgeConfig::default();
        // Default is 3600 seconds (1 hour) when used by SessionRouter
        assert_eq!(config.session_idle_timeout_secs, None);
    }
}
