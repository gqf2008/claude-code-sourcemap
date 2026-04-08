//! Agent type definitions for sub-agent spawning.
//!
//! The `Agent` tool implementation lives in `claude-agent::dispatch_agent`
//! (as `DispatchAgentTool`) — it holds the unified `AgentType` enum with
//! full metadata (system prompts, model preferences, tool allow-lists).
//!
//! This module previously contained a parallel `SpawnAgentType` enum.
//! It was removed because `AgentType` is the single source of truth;
//! keeping two overlapping enums led to drift and confusion.
//!
//! If a crate cannot depend on `claude-agent` but needs agent type
//! information, consider moving `AgentType` to `claude-core` instead
//! of re-creating a parallel definition here.
