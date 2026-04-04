use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::permissions::PermissionRule;

// ── Hook configuration types ────────────────────────────────────────────────

/// A single shell-command hook definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCommandDef {
    /// Hook type — currently only `"command"` is supported.
    #[serde(rename = "type", default = "default_hook_type")]
    pub hook_type: String,
    /// Shell command to execute (passed to `sh -c` on Unix, `cmd /C` on Windows).
    pub command: String,
    /// Optional timeout in milliseconds (default: 60 000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_hook_type() -> String {
    "command".into()
}

/// A hook rule: an optional tool-name matcher + one or more hook commands.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookRule {
    /// Optional regex / glob pattern applied to the tool name.
    /// `None` or `"*"` matches every tool.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Commands to run when this rule matches.
    #[serde(default)]
    pub hooks: Vec<HookCommandDef>,
}

/// All hook rules keyed by lifecycle event name.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookRule>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookRule>,
    #[serde(default, rename = "PostToolUseFailure")]
    pub post_tool_use_failure: Vec<HookRule>,
    #[serde(default, rename = "Stop")]
    pub stop: Vec<HookRule>,
    #[serde(default, rename = "StopFailure")]
    pub stop_failure: Vec<HookRule>,
    #[serde(default, rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<HookRule>,
    #[serde(default, rename = "SessionStart")]
    pub session_start: Vec<HookRule>,
    #[serde(default, rename = "SessionEnd")]
    pub session_end: Vec<HookRule>,
    #[serde(default, rename = "Setup")]
    pub setup: Vec<HookRule>,
    #[serde(default, rename = "PreCompact")]
    pub pre_compact: Vec<HookRule>,
    #[serde(default, rename = "PostCompact")]
    pub post_compact: Vec<HookRule>,
    #[serde(default, rename = "SubagentStart")]
    pub subagent_start: Vec<HookRule>,
    #[serde(default, rename = "SubagentStop")]
    pub subagent_stop: Vec<HookRule>,
    #[serde(default, rename = "Notification")]
    pub notification: Vec<HookRule>,
}

// ── Main settings struct ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default)]
    pub custom_system_prompt: Option<String>,
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    #[serde(default)]
    pub permission_rules: Vec<PermissionRule>,
    /// Lifecycle hook configuration.
    #[serde(default)]
    pub hooks: HooksConfig,
}

impl Settings {
    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("claude"))
    }

    pub fn load() -> anyhow::Result<Self> {
        let config_dir = Self::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
        let settings_path = config_dir.join("settings.json");
        if settings_path.exists() {
            let content = std::fs::read_to_string(&settings_path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }
}
