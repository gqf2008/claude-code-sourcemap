//! Agent type definitions for sub-agent spawning.
//!
//! The actual `Agent` tool implementation lives in `claude-agent::dispatch_agent`
//! (as `DispatchAgentTool`) because it requires `ApiClient` and coordinator state.
//! This module provides shared type definitions used by both crates.

/// Agent types that can be spawned via the Agent tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnAgentType {
    /// General-purpose coding agent (full tool access).
    Coder,
    /// Read-only exploration/research agent.
    Explorer,
    /// Verification agent (runs tests, read-only + bash).
    Verification,
    /// Worker agent (coordinator-spawned, full access).
    Worker,
}

impl SpawnAgentType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "coder" | "general" | "general-purpose" => Some(Self::Coder),
            "explorer" | "explore" | "research" => Some(Self::Explorer),
            "verification" | "verify" | "test" => Some(Self::Verification),
            "worker" => Some(Self::Worker),
            _ => None,
        }
    }

    pub fn default_background(&self) -> bool {
        matches!(self, Self::Worker | Self::Explorer)
    }

    pub fn allowed_tools(&self) -> &[&str] {
        match self {
            Self::Coder => &[
                "Read", "Edit", "Write", "Glob", "Grep", "LS",
                "Bash", "AskUser", "WebFetch",
            ],
            Self::Explorer => &[
                "Read", "Glob", "Grep", "LS", "Bash", "WebFetch",
            ],
            Self::Verification => &[
                "Read", "Glob", "Grep", "LS", "Bash",
            ],
            Self::Worker => &[
                "Read", "Edit", "Write", "MultiEdit", "Glob", "Grep", "LS",
                "Bash", "Git", "WebFetch", "TodoRead", "TodoWrite",
            ],
        }
    }

    pub fn max_turns(&self) -> u32 {
        match self {
            Self::Coder => 30,
            Self::Explorer => 15,
            Self::Verification => 10,
            Self::Worker => 50,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_type_parsing() {
        assert_eq!(SpawnAgentType::parse("coder"), Some(SpawnAgentType::Coder));
        assert_eq!(SpawnAgentType::parse("general"), Some(SpawnAgentType::Coder));
        assert_eq!(SpawnAgentType::parse("explorer"), Some(SpawnAgentType::Explorer));
        assert_eq!(SpawnAgentType::parse("explore"), Some(SpawnAgentType::Explorer));
        assert_eq!(SpawnAgentType::parse("verification"), Some(SpawnAgentType::Verification));
        assert_eq!(SpawnAgentType::parse("test"), Some(SpawnAgentType::Verification));
        assert_eq!(SpawnAgentType::parse("worker"), Some(SpawnAgentType::Worker));
        assert_eq!(SpawnAgentType::parse("unknown"), None);
    }

    #[test]
    fn agent_type_defaults() {
        assert!(!SpawnAgentType::Coder.default_background());
        assert!(SpawnAgentType::Explorer.default_background());
        assert!(!SpawnAgentType::Verification.default_background());
        assert!(SpawnAgentType::Worker.default_background());
    }

    #[test]
    fn agent_type_max_turns() {
        assert_eq!(SpawnAgentType::Coder.max_turns(), 30);
        assert_eq!(SpawnAgentType::Explorer.max_turns(), 15);
        assert_eq!(SpawnAgentType::Verification.max_turns(), 10);
        assert_eq!(SpawnAgentType::Worker.max_turns(), 50);
    }

    #[test]
    fn agent_type_tools() {
        let explorer_tools = SpawnAgentType::Explorer.allowed_tools();
        assert!(explorer_tools.contains(&"Read"));
        assert!(explorer_tools.contains(&"Grep"));
        assert!(!explorer_tools.contains(&"Edit"));
        assert!(!explorer_tools.contains(&"Write"));

        let coder_tools = SpawnAgentType::Coder.allowed_tools();
        assert!(coder_tools.contains(&"Edit"));
        assert!(coder_tools.contains(&"Write"));
    }
}
