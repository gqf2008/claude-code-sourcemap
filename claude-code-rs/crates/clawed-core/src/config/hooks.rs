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

/// Merge overlay hooks into base, extending each event's rule list.
pub(super) fn merge_hooks(mut base: HooksConfig, overlay: &HooksConfig) -> HooksConfig {
    base.pre_tool_use.extend(overlay.pre_tool_use.clone());
    base.post_tool_use.extend(overlay.post_tool_use.clone());
    base.post_tool_use_failure.extend(overlay.post_tool_use_failure.clone());
    base.stop.extend(overlay.stop.clone());
    base.stop_failure.extend(overlay.stop_failure.clone());
    base.user_prompt_submit.extend(overlay.user_prompt_submit.clone());
    base.session_start.extend(overlay.session_start.clone());
    base.session_end.extend(overlay.session_end.clone());
    base.setup.extend(overlay.setup.clone());
    base.pre_compact.extend(overlay.pre_compact.clone());
    base.post_compact.extend(overlay.post_compact.clone());
    base.subagent_start.extend(overlay.subagent_start.clone());
    base.subagent_stop.extend(overlay.subagent_stop.clone());
    base.notification.extend(overlay.notification.clone());
    base.post_sampling.extend(overlay.post_sampling.clone());
    base.permission_request.extend(overlay.permission_request.clone());
    base.permission_denied.extend(overlay.permission_denied.clone());
    base.instructions_loaded.extend(overlay.instructions_loaded.clone());
    base.cwd_changed.extend(overlay.cwd_changed.clone());
    base.file_changed.extend(overlay.file_changed.clone());
    base.config_change.extend(overlay.config_change.clone());
    base.task_created.extend(overlay.task_created.clone());
    base.task_completed.extend(overlay.task_completed.clone());
    base.teammate_idle.extend(overlay.teammate_idle.clone());
    base.elicitation.extend(overlay.elicitation.clone());
    base.elicitation_result.extend(overlay.elicitation_result.clone());
    base.worktree_create.extend(overlay.worktree_create.clone());
    base.worktree_remove.extend(overlay.worktree_remove.clone());
    base
}
