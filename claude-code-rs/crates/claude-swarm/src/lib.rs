//! Actor-based multi-agent swarm network for Claude Code.
//!
//! Uses the `kameo` actor framework to implement a typed, supervised agent
//! network. Integrated into the main agent via MCP protocol.
//!
//! # Architecture
//!
//! - `AgentActor` — wraps a single AI agent session (API client + conversation state)
//! - `SwarmCoordinator` — manages team topology, agent lifecycle, message routing
//! - `SwarmMcpServer` — exposes swarm operations as MCP tools for the host agent
//! - `bridge` — `Tool` trait adapters for registering swarm tools into `ToolRegistry`
//!
//! # Integration
//!
//! Enable with `CLAUDE_CODE_SWARM=1`. The engine builder calls
//! `claude_swarm::bridge::register_swarm_tools()` to register all 8 tools.

pub mod actors;
pub mod bridge;
pub mod messages;
pub mod network;
pub mod server;

pub use bridge::register_swarm_tools;
pub use network::SwarmNetwork;
pub use server::SwarmMcpServer;
