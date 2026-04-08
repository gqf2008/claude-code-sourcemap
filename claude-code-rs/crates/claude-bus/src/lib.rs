//! `claude-bus` — Internal event bus and JSON-RPC protocol layer.
//!
//! This crate decouples the **Agent Core** (query loop, tool execution,
//! permissions, hooks) from the **UI layer** (terminal REPL, IDE extension,
//! Web UI) through a typed message bus.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │  Agent Core (claude-agent)           │
//! │  holds BusHandle                     │
//! │  sends: AgentNotification            │
//! │  receives: AgentRequest              │
//! │  sends: PermissionRequest            │
//! │  receives: PermissionResponse        │
//! └──────────────┬──────────────────────┘
//!                │ tokio channels
//! ┌──────────────┴──────────────────────┐
//! │  UI Client (claude-cli / IDE / Web)  │
//! │  holds ClientHandle                  │
//! │  receives: AgentNotification         │
//! │  sends: AgentRequest                 │
//! │  receives: PermissionRequest         │
//! │  sends: PermissionResponse           │
//! └─────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! let (bus, client) = claude_bus::EventBus::new(256);
//!
//! // Agent core side
//! tokio::spawn(async move {
//!     bus.notify(AgentNotification::TextDelta { text: "Hello".into() });
//! });
//!
//! // UI client side
//! while let Ok(event) = client.notifications().recv().await {
//!     println!("Got: {:?}", event);
//! }
//! ```

pub mod events;
pub mod bus;

pub use events::*;
pub use bus::*;
