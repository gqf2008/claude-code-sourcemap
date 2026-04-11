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
//! - `types` — core data types (TeamFile, TeamMember, TeamContext) for persistent team state
//! - `helpers` — team file I/O, discovery, color picking
//! - `conflict` — file-level conflict tracking for multi-agent edits
//! - `team_create/delete/status` — Tool trait impls for team lifecycle management
//!
//! # Integration
//!
//! Enable with `CLAUDE_CODE_SWARM=1`. The engine builder calls
//! `clawed_swarm::bridge::register_swarm_tools()` to register all tools.

pub mod actors;
pub mod bridge;
pub mod bus_adapter;
pub mod session;
pub mod conflict;
pub mod helpers;
pub mod messages;
pub mod network;
pub mod server;
pub mod team_create;
pub mod team_delete;
pub mod team_status;
pub mod types;

pub use bridge::register_swarm_tools;
pub use bus_adapter::{SwarmNotifier, SharedNotifier, shared_notifier};
pub use conflict::FileConflictTracker;
pub use network::SwarmNetwork;
pub use server::SwarmMcpServer;
pub use team_create::TeamCreateTool;
pub use team_delete::TeamDeleteTool;
pub use team_status::{TeamStatusTool, format_team_summary};
pub use types::*;
