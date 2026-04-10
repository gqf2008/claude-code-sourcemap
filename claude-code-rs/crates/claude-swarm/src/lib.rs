//! Actor-based multi-agent swarm network for Claude Code.
//!
//! Uses the `kameo` actor framework to implement a typed, supervised agent
//! network. Integrated into the main agent via MCP protocol (like computer-use).
//!
//! # Architecture
//!
//! - `AgentActor` — wraps a single AI agent session (API client + conversation state)
//! - `SwarmCoordinator` — manages team topology, agent lifecycle, message routing
//! - `SwarmMcpServer` — exposes swarm operations as MCP tools for the host agent
//!
//! # Integration
//!
//! The swarm is registered as an MCP tool server in `claude-agent`, using the same
//! `ToolBridge` pattern as `claude-computer-use`.

pub mod actors;
pub mod network;
pub mod server;
pub mod messages;

pub use network::SwarmNetwork;
pub use server::SwarmMcpServer;
