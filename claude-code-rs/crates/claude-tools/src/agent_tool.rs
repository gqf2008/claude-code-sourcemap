//! AgentTool — spawn sub-agents for parallel or specialized work.
//!
//! Aligned with TS `tools/AgentTool.ts`:
//! - Creates isolated sub-agents with their own tool permissions
//! - Supports background (async) and foreground (sync) execution
//! - Integrates with task tracking for result collection
//!
//! This is the skeleton: actual agent execution delegates to the
//! `claude-agent` crate's dispatch mechanism.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

/// AgentTool — spawns a sub-agent to perform a task.
///
/// The agent runs with its own system prompt, tool permissions, and
/// conversation history. Results are reported back to the parent.
pub struct AgentTool;

/// Agent types that can be spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnAgentType {
    /// General-purpose coding agent (full tool access).
    Coder,
    /// Read-only exploration/research agent.
    Explorer,
    /// Verification agent (runs tests, read-only + bash).
    Verification,
    /// Worker agent (coordinator-spawned, full access, isolated worktree).
    Worker,
}

impl SpawnAgentType {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "coder" | "general" | "general-purpose" => Some(Self::Coder),
            "explorer" | "explore" | "research" => Some(Self::Explorer),
            "verification" | "verify" | "test" => Some(Self::Verification),
            "worker" => Some(Self::Worker),
            _ => None,
        }
    }

    fn default_background(&self) -> bool {
        matches!(self, Self::Worker | Self::Explorer)
    }

    fn allowed_tools(&self) -> &[&str] {
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

    fn max_turns(&self) -> u32 {
        match self {
            Self::Coder => 30,
            Self::Explorer => 15,
            Self::Verification => 10,
            Self::Worker => 50,
        }
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str { "Agent" }
    fn category(&self) -> ToolCategory { ToolCategory::Agent }

    fn description(&self) -> &str {
        "Spawn a sub-agent to perform a task. The agent runs with its own conversation \
         and tool permissions. Use for parallel work, research, verification, or \
         when isolation is needed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task for the agent to perform. Be specific and provide all necessary context."
                },
                "agent_type": {
                    "type": "string",
                    "enum": ["coder", "explorer", "verification", "worker"],
                    "description": "Type of agent to spawn. 'coder' has full tool access, 'explorer' is read-only, 'verification' runs tests, 'worker' is for coordinator-spawned work.",
                    "default": "coder"
                },
                "background": {
                    "type": "boolean",
                    "description": "Run in background (true) or wait for completion (false).",
                    "default": false
                }
            },
            "required": ["prompt"]
        })
    }

    fn is_read_only(&self) -> bool { false }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        let agent_type_str = input["agent_type"]
            .as_str()
            .unwrap_or("coder");

        let agent_type = SpawnAgentType::from_str(agent_type_str)
            .ok_or_else(|| anyhow::anyhow!(
                "Unknown agent_type '{}'. Use: coder, explorer, verification, worker",
                agent_type_str
            ))?;

        let background = input["background"]
            .as_bool()
            .unwrap_or_else(|| agent_type.default_background());

        let max_turns = agent_type.max_turns();
        let allowed = agent_type.allowed_tools();

        // Build agent descriptor for the dispatch system
        let descriptor = json!({
            "prompt": prompt,
            "agent_type": agent_type_str,
            "background": background,
            "max_turns": max_turns,
            "allowed_tools": allowed,
        });

        if background {
            // In background mode, return immediately with a task ID
            let task_id = format!("agent-{}", uuid::Uuid::new_v4().simple());
            Ok(ToolResult::text(format!(
                "Agent '{}' spawned in background (type: {}, max {} turns).\n\
                 Task ID: {}\n\
                 Use TaskGet or TaskOutput to check results.",
                &prompt.chars().take(60).collect::<String>(),
                agent_type_str,
                max_turns,
                task_id,
            )))
        } else {
            // Foreground: would normally run the agent loop here
            // For now, return the descriptor so the engine can handle dispatch
            Ok(ToolResult::text(format!(
                "Agent dispatch request (type: {}, max {} turns):\n{}",
                agent_type_str,
                max_turns,
                serde_json::to_string_pretty(&descriptor)?,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::Tool;
    use claude_core::permissions::PermissionMode;
    use std::path::PathBuf;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("."),
            abort_signal: Default::default(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    #[test]
    fn agent_type_parsing() {
        assert_eq!(SpawnAgentType::from_str("coder"), Some(SpawnAgentType::Coder));
        assert_eq!(SpawnAgentType::from_str("general"), Some(SpawnAgentType::Coder));
        assert_eq!(SpawnAgentType::from_str("explorer"), Some(SpawnAgentType::Explorer));
        assert_eq!(SpawnAgentType::from_str("explore"), Some(SpawnAgentType::Explorer));
        assert_eq!(SpawnAgentType::from_str("verification"), Some(SpawnAgentType::Verification));
        assert_eq!(SpawnAgentType::from_str("test"), Some(SpawnAgentType::Verification));
        assert_eq!(SpawnAgentType::from_str("worker"), Some(SpawnAgentType::Worker));
        assert_eq!(SpawnAgentType::from_str("unknown"), None);
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
        assert!(!explorer_tools.contains(&"Edit")); // read-only
        assert!(!explorer_tools.contains(&"Write")); // read-only

        let coder_tools = SpawnAgentType::Coder.allowed_tools();
        assert!(coder_tools.contains(&"Edit"));
        assert!(coder_tools.contains(&"Write"));
    }

    #[test]
    fn tool_metadata() {
        let tool = AgentTool;
        assert_eq!(tool.name(), "Agent");
        assert_eq!(tool.category(), ToolCategory::Agent);
        assert!(!tool.is_read_only());
        assert!(tool.is_concurrency_safe());
    }

    #[test]
    fn input_schema_has_required_fields() {
        let tool = AgentTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
    }

    #[tokio::test]
    async fn call_missing_prompt() {
        let tool = AgentTool;
        let result = tool.call(json!({}), &test_context()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn call_foreground() {
        let tool = AgentTool;
        let result = tool.call(
            json!({"prompt": "Fix the bug", "agent_type": "coder"}),
            &test_context()
        ).await.unwrap();
        let text = result.content.iter()
            .filter_map(|c| match c {
                claude_core::message::ToolResultContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("coder"));
        assert!(text.contains("30 turns"));
    }

    #[tokio::test]
    async fn call_background() {
        let tool = AgentTool;
        let result = tool.call(
            json!({"prompt": "Research something", "background": true}),
            &test_context()
        ).await.unwrap();
        let text = result.content.iter()
            .filter_map(|c| match c {
                claude_core::message::ToolResultContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("background"));
        assert!(text.contains("agent-"));
    }

    #[tokio::test]
    async fn call_unknown_agent_type() {
        let tool = AgentTool;
        let result = tool.call(
            json!({"prompt": "do stuff", "agent_type": "invalid"}),
            &test_context()
        ).await;
        assert!(result.is_err());
    }
}
