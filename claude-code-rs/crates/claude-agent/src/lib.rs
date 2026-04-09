//! Agent engine: query loop, tool execution, compaction, and coordination.
//!
//! This crate drives the main agent loop — it receives user messages, streams
//! API responses, dispatches tool calls through the executor, handles
//! auto-compaction, and supports multi-agent coordination.

pub mod audit;
pub mod computer_use;
pub mod engine;
pub mod query;
pub mod executor;
pub mod state;
pub mod hooks;
pub mod permissions;
pub mod dispatch_agent;
pub mod compact;
pub mod task_runner;
pub mod coordinator;
pub mod cost;
pub mod system_prompt;
pub mod plugin;
pub mod memory_extractor;
pub mod bus_adapter;
