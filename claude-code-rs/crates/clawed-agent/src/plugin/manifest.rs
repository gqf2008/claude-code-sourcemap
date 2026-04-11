//! Plugin manifest — `plugin.json` schema types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Root manifest for a plugin, loaded from `plugin.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin name (must be unique, used as namespace).
    pub name: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: String,

    /// Plugin version (semver string).
    #[serde(default = "default_version")]
    pub version: String,

    /// Minimum claude-code-rs version required (semver).
    #[serde(default, rename = "minVersion")]
    pub min_version: Option<String>,

    /// Commands provided by this plugin.
    #[serde(default)]
    pub commands: Vec<PluginCommand>,

    /// Skills provided by this plugin.
    #[serde(default)]
    pub skills: Vec<PluginSkill>,

    /// Hooks registered by this plugin.
    #[serde(default)]
    pub hooks: Vec<PluginHook>,

    /// MCP server configurations to add.
    #[serde(default)]
    pub mcp: HashMap<String, serde_json::Value>,

    /// Whether the plugin is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_version() -> String { "0.1.0".into() }
fn default_true() -> bool { true }

/// A command registered by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommand {
    /// Command name (without `/` prefix).
    pub name: String,

    /// Command description shown in `/help`.
    #[serde(default)]
    pub description: String,

    /// Path to a `.md` file (relative to plugin dir) containing the prompt template.
    /// The file content is used as the system prompt when the command is invoked.
    #[serde(default, rename = "promptFile")]
    pub prompt_file: Option<String>,

    /// Inline prompt text (used if `prompt_file` is not set).
    #[serde(default)]
    pub prompt: Option<String>,

    /// Tools allowed for this command (empty = all tools).
    #[serde(default, rename = "allowedTools")]
    pub allowed_tools: Vec<String>,
}

/// A skill registered by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSkill {
    /// Skill name (used as `/skill_name` command).
    pub name: String,

    /// Skill description.
    #[serde(default)]
    pub description: String,

    /// Path to a `.md` file containing the skill prompt.
    #[serde(rename = "promptFile")]
    pub prompt_file: String,
}

/// A hook registered by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHook {
    /// Which event triggers this hook.
    pub event: HookEvent,

    /// Shell command to execute.
    pub command: String,

    /// Optional timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_timeout() -> u64 { 30 }

/// Events that can trigger hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Before a user message is processed.
    PreMessage,
    /// After a response is generated.
    PostMessage,
    /// Before a tool is executed.
    PreTool,
    /// After a tool execution completes.
    PostTool,
    /// When a session starts.
    SessionStart,
    /// When a session ends.
    SessionEnd,
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreMessage => write!(f, "pre_message"),
            Self::PostMessage => write!(f, "post_message"),
            Self::PreTool => write!(f, "pre_tool"),
            Self::PostTool => write!(f, "post_tool"),
            Self::SessionStart => write!(f, "session_start"),
            Self::SessionEnd => write!(f, "session_end"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal_manifest() {
        let json = r#"{"name": "my-plugin"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        assert!(manifest.description.is_empty());
        assert_eq!(manifest.version, "0.1.0");
        assert!(manifest.commands.is_empty());
        assert!(manifest.skills.is_empty());
        assert!(manifest.hooks.is_empty());
        assert!(manifest.enabled);
    }

    #[test]
    fn test_deserialize_full_manifest() {
        let json = r#"{
            "name": "git-helper",
            "description": "Git workflow helpers",
            "version": "1.0.0",
            "commands": [
                {
                    "name": "squash",
                    "description": "Squash recent commits",
                    "promptFile": "commands/squash.md",
                    "allowedTools": ["Bash"]
                }
            ],
            "skills": [
                {
                    "name": "git-expert",
                    "description": "Git expertise",
                    "promptFile": "skills/git.md"
                }
            ],
            "hooks": [
                {
                    "event": "pre_tool",
                    "command": "echo pre-tool check",
                    "timeout": 10
                }
            ],
            "mcp": {
                "git-mcp": {
                    "command": "git-mcp-server",
                    "args": []
                }
            },
            "enabled": true
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "git-helper");
        assert_eq!(manifest.commands.len(), 1);
        assert_eq!(manifest.commands[0].name, "squash");
        assert_eq!(manifest.commands[0].allowed_tools, vec!["Bash"]);
        assert_eq!(manifest.skills.len(), 1);
        assert_eq!(manifest.hooks.len(), 1);
        assert_eq!(manifest.hooks[0].event, HookEvent::PreTool);
        assert_eq!(manifest.hooks[0].timeout, 10);
        assert!(manifest.mcp.contains_key("git-mcp"));
    }

    #[test]
    fn test_deserialize_disabled_plugin() {
        let json = r#"{"name": "disabled", "enabled": false}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(!manifest.enabled);
    }

    #[test]
    fn test_hook_event_display() {
        assert_eq!(HookEvent::PreMessage.to_string(), "pre_message");
        assert_eq!(HookEvent::PostTool.to_string(), "post_tool");
        assert_eq!(HookEvent::SessionStart.to_string(), "session_start");
    }

    #[test]
    fn test_command_inline_prompt() {
        let json = r#"{
            "name": "greet",
            "description": "Say hello",
            "prompt": "Say hello to the user in a friendly way"
        }"#;
        let cmd: PluginCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.prompt.as_deref(), Some("Say hello to the user in a friendly way"));
        assert!(cmd.prompt_file.is_none());
    }
}
