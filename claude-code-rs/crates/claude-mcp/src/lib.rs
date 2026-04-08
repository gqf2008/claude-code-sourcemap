//! MCP (Model Context Protocol) — protocol types, client, transport, and server registry.
//!
//! This crate provides the core MCP protocol implementation:
//! - [`protocol`] — JSON-RPC 2.0 message types
//! - [`types`] — MCP domain types (tools, resources, capabilities)
//! - [`client`] — MCP client (initialize → list → call → close)
//! - [`transport`] — Stdio transport (child process JSON-RPC)
//! - [`sse`] — SSE transport (HTTP Server-Sent Events)
//! - [`registry`] — Multi-server management and config discovery
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │            McpManager (registry)         │
//! │  connect / disconnect / all_tools        │
//! └──────┬───────────────┬──────────────────┘
//! │       McpClient      │      McpClient    │
//! │  (protocol layer)    │  (protocol layer) │
//! └──────┬───────────────┴──────┬────────────┘
//! │  StdioTransport  │   │  SseTransport │
//! │  (JSON-RPC/stdio)│   │  (JSON-RPC/SSE)│
//! └──────────────────┘   └────────────────┘
//! ```

pub mod protocol;
pub mod types;
pub mod client;
pub mod transport;
pub mod sse;
pub mod registry;
pub mod bus;

pub use protocol::*;
pub use types::*;
pub use client::McpClient;
pub use registry::{McpManager, format_mcp_tool_name, parse_mcp_tool_name, is_mcp_tool, load_mcp_configs, discover_mcp_configs};
pub use bus::McpBusAdapter;
