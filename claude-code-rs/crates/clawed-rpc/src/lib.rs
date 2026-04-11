//! JSON-RPC 2.0 server for claude-code-rs.
//!
//! Exposes the Agent Core capabilities over JSON-RPC, supporting multiple
//! transport layers (stdio, TCP). Each connection creates an `RpcSession`
//! that bridges JSON-RPC messages to the internal event bus.
//!
//! # Architecture
//!
//! ```text
//!                          JSON-RPC 2.0
//!   ┌──────────┐    ┌─────────────────────┐    ┌───────────┐
//!   │  Client   │───▶│  Transport (stdio/  │───▶│ RpcSession│
//!   │(IDE/Web)  │◀───│   TCP/WebSocket)    │◀───│           │
//!   └──────────┘    └─────────────────────┘    └─────┬─────┘
//!                                                     │ ClientHandle
//!                                              ┌──────┴──────┐
//!                                              │  Event Bus   │
//!                                              └─────────────┘
//! ```
//!
//! # Modules
//!
//! - [`protocol`] — JSON-RPC 2.0 message types
//! - [`methods`] — Method routing (JSON-RPC ↔ AgentRequest/Notification)
//! - [`transport`] — Transport trait and implementations
//! - [`session`] — Per-connection session management
//! - [`server`] — Multi-transport server
//! - [`error`] — Error types

pub mod error;
pub mod methods;
pub mod protocol;
pub mod server;
pub mod session;
pub mod transport;

// Re-exports for convenience
pub use protocol::{Message, Notification, RawMessage, Request, RequestId, Response, RpcError};
pub use server::RpcServer;
pub use session::RpcSession;
pub use transport::Transport;
