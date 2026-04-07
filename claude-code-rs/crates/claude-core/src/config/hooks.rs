//! Hook configuration types used in settings files.

use serde::{Deserialize, Serialize};

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
    #[serde(default, rename = "PostSampling")]
    pub post_sampling: Vec<HookRule>,
    #[serde(default, rename = "PermissionRequest")]
    pub permission_request: Vec<HookRule>,
    #[serde(default, rename = "PermissionDenied")]
    pub permission_denied: Vec<HookRule>,
    #[serde(default, rename = "InstructionsLoaded")]
    pub instructions_loaded: Vec<HookRule>,
    #[serde(default, rename = "CwdChanged")]
    pub cwd_changed: Vec<HookRule>,
    #[serde(default, rename = "FileChanged")]
    pub file_changed: Vec<HookRule>,
    #[serde(default, rename = "ConfigChange")]
    pub config_change: Vec<HookRule>,
    #[serde(default, rename = "TaskCreated")]
    pub task_created: Vec<HookRule>,
    #[serde(default, rename = "TaskCompleted")]
    pub task_completed: Vec<HookRule>,
    #[serde(default, rename = "TeammateIdle")]
    pub teammate_idle: Vec<HookRule>,
    #[serde(default, rename = "Elicitation")]
    pub elicitation: Vec<HookRule>,
    #[serde(default, rename = "ElicitationResult")]
    pub elicitation_result: Vec<HookRule>,
    #[serde(default, rename = "WorktreeCreate")]
    pub worktree_create: Vec<HookRule>,
    #[serde(default, rename = "WorktreeRemove")]
    pub worktree_remove: Vec<HookRule>,
}

/// Check whether a HooksConfig has any non-empty event lists.
pub(super) fn has_any_hooks(h: &HooksConfig) -> bool {
    !h.pre_tool_use.is_empty()
        || !h.post_tool_use.is_empty()
        || !h.post_tool_use_failure.is_empty()
        || !h.stop.is_empty()
        || !h.stop_failure.is_empty()
        || !h.session_start.is_empty()
        || !h.session_end.is_empty()
        || !h.setup.is_empty()
        || !h.pre_compact.is_empty()
        || !h.post_compact.is_empty()
        || !h.subagent_start.is_empty()
        || !h.subagent_stop.is_empty()
        || !h.notification.is_empty()
        || !h.post_sampling.is_empty()
        || !h.permission_request.is_empty()
        || !h.permission_denied.is_empty()
        || !h.instructions_loaded.is_empty()
        || !h.user_prompt_submit.is_empty()
        || !h.teammate_idle.is_empty()
        || !h.elicitation.is_empty()
        || !h.elicitation_result.is_empty()
        || !h.worktree_create.is_empty()
        || !h.worktree_remove.is_empty()
}
