//! claude-bridge — External messaging channel gateway.
//!
//! Connects external platforms (Feishu/Lark, Telegram, WeChat, DingTalk)
//! to the Agent via the Event Bus. Each platform adapter translates between
//! platform-native messages and `AgentRequest`/`AgentNotification`.
//!
//! # Architecture
//!
//! ```text
//!   Feishu ──→ webhook ──→ FeishuAdapter ──→ SessionRouter
//!   Telegram ──→ poll ──→ TelegramAdapter ─↗   ↓
//!                                           ClientHandle ──→ Bus ──→ Agent
//! ```
//!
//! All adapters implement the `ChannelAdapter` trait and communicate with
//! the gateway, which manages adapter lifecycle and session routing.

pub mod adapter;
pub mod config;
pub mod formatter;
pub mod gateway;
pub mod message;
pub mod session;
pub mod webhook;

pub mod adapters;

// Re-exports
pub use adapter::ChannelAdapter;
pub use config::BridgeConfig;
pub use gateway::ChannelGateway;
pub use message::{ChannelId, InboundMessage, OutboundMessage, SenderInfo};
pub use session::SessionRouter;
