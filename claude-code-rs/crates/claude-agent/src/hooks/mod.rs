//! External shell-command hook system.
//!
//! Hooks let users run arbitrary shell scripts at lifecycle events:
//!
//! | Event                | When                              | exit 2 behaviour                  |
//! |----------------------|-----------------------------------|-----------------------------------|
//! | `PreToolUse`         | Before a tool runs                | block tool, return message        |
//! | `PostToolUse`        | After a tool runs successfully    | override result with stdout       |
//! | `PostToolUseFailure` | After a tool fails                | inject feedback immediately       |
//! | `Stop`               | After `end_turn`                  | inject feedback, loop again       |
//! | `StopFailure`        | When turn ends due to API error   | fire-and-forget (exit ignored)    |
//! | `UserPromptSubmit`   | Before user msg is sent           | append extra context              |
//! | `SessionStart`       | Once at session start             | append to system prompt           |
//! | `SessionEnd`         | When session ends                 | no blocking effect                |
//! | `Setup`              | On first use                      | one-time initialisation           |
//! | `PreCompact`         | Before conversation compaction    | append custom compact instructions |
//! | `PostCompact`        | After compaction                  | show to user                      |
//! | `SubagentStart`      | When a sub-agent is spawned       | append context to sub-agent       |
//! | `SubagentStop`       | Before sub-agent ends             | inject feedback, loop sub-agent   |
//! | `Notification`       | Desktop/terminal notifications    | fire-and-forget                   |
//!
//! Hook config lives in `settings.json` under the `hooks` key — see
//! `claude_core::config::HooksConfig` for the format.

mod execution;
mod types;

// Re-export public types
pub use types::{HookDecision, HookEvent};
pub(crate) use types::HookContext;

use std::path::PathBuf;

use serde_json::Value;
use tracing::{debug, warn};

use claude_core::config::{HooksConfig, HookRule};

use execution::{interpret_output, run_shell_hook, tool_matches};

// ── HookRegistry ─────────────────────────────────────────────────────────────

pub struct HookRegistry {
    config: HooksConfig,
    cwd: PathBuf,
    session_id: String,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            config: HooksConfig::default(),
            cwd: std::env::current_dir().unwrap_or_default(),
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Build a registry from user settings.
    pub fn from_config(config: HooksConfig, cwd: impl Into<PathBuf>, session_id: impl Into<String>) -> Self {
        Self {
            config,
            cwd: cwd.into(),
            session_id: session_id.into(),
        }
    }

    fn rules_for(&self, event: HookEvent) -> &[HookRule] {
        match event {
            HookEvent::PreToolUse => &self.config.pre_tool_use,
            HookEvent::PostToolUse => &self.config.post_tool_use,
            HookEvent::PostToolUseFailure => &self.config.post_tool_use_failure,
            HookEvent::Stop => &self.config.stop,
            HookEvent::StopFailure => &self.config.stop_failure,
            HookEvent::UserPromptSubmit => &self.config.user_prompt_submit,
            HookEvent::SessionStart => &self.config.session_start,
            HookEvent::SessionEnd => &self.config.session_end,
            HookEvent::Setup => &self.config.setup,
            HookEvent::PreCompact => &self.config.pre_compact,
            HookEvent::PostCompact => &self.config.post_compact,
            HookEvent::SubagentStart => &self.config.subagent_start,
            HookEvent::SubagentStop => &self.config.subagent_stop,
            HookEvent::Notification => &self.config.notification,
            HookEvent::PostSampling => &self.config.post_sampling,
            HookEvent::PermissionRequest => &self.config.permission_request,
            HookEvent::PermissionDenied => &self.config.permission_denied,
            HookEvent::InstructionsLoaded => &self.config.instructions_loaded,
            HookEvent::CwdChanged => &self.config.cwd_changed,
            HookEvent::FileChanged => &self.config.file_changed,
            HookEvent::ConfigChange => &self.config.config_change,
            HookEvent::TaskCreated => &self.config.task_created,
            HookEvent::TaskCompleted => &self.config.task_completed,
        }
    }

    /// Run all matching hooks for `event`.  Returns the first non-Continue decision.
    pub(crate) async fn run(&self, event: HookEvent, ctx: HookContext) -> HookDecision {
        let rules = self.rules_for(event);
        let tool_name = ctx.tool_name.as_deref().unwrap_or("");

        for rule in rules {
            if !tool_matches(&rule.matcher, tool_name) {
                continue;
            }
            for cmd_def in &rule.hooks {
                if cmd_def.hook_type != "command" {
                    continue;
                }
                match run_shell_hook(cmd_def, &ctx, &self.cwd).await {
                    Ok((exit_code, stdout)) => {
                        debug!(
                            "Hook {:?} cmd='{}' exit={} stdout_len={}",
                            event.as_str(),
                            cmd_def.command,
                            exit_code,
                            stdout.len()
                        );
                        let decision = interpret_output(event, exit_code, stdout);
                        if !matches!(decision, HookDecision::Continue) {
                            return decision;
                        }
                    }
                    Err(e) => {
                        warn!("Hook execution error ({}): {}", cmd_def.command, e);
                    }
                }
            }
        }

        HookDecision::Continue
    }

    /// Build a `HookContext` for tool events.
    pub(crate) fn tool_ctx(&self, event: HookEvent, tool_name: &str, input: Option<Value>, output: Option<String>, is_error: Option<bool>) -> HookContext {
        HookContext {
            event: event.as_str().to_string(),
            tool_name: Some(tool_name.to_string()),
            tool_input: input,
            tool_output: output,
            tool_error: is_error,
            error: None,
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a `HookContext` for tool failure events.
    pub(crate) fn tool_failure_ctx(&self, tool_name: &str, input: Option<Value>, error_msg: &str) -> HookContext {
        HookContext {
            event: HookEvent::PostToolUseFailure.as_str().to_string(),
            tool_name: Some(tool_name.to_string()),
            tool_input: input,
            tool_output: None,
            tool_error: Some(true),
            error: Some(error_msg.to_string()),
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a `HookContext` for session / prompt events.
    pub(crate) fn prompt_ctx(&self, event: HookEvent, prompt: Option<String>) -> HookContext {
        HookContext {
            event: event.as_str().to_string(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            tool_error: None,
            error: None,
            prompt,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a `HookContext` for compaction events.
    pub(crate) fn compact_ctx(&self, event: HookEvent, trigger: &str, summary: Option<String>) -> HookContext {
        HookContext {
            event: event.as_str().to_string(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            tool_error: None,
            error: None,
            prompt: None,
            trigger: Some(trigger.to_string()),
            summary,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a `HookContext` for subagent events.
    #[allow(dead_code)] // reserved for SubagentStart/End hook events
    pub(crate) fn subagent_ctx(&self, event: HookEvent, agent_id: &str) -> HookContext {
        HookContext {
            event: event.as_str().to_string(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            tool_error: None,
            error: None,
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: Some(agent_id.to_string()),
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a `HookContext` for permission events.
    pub(crate) fn permission_ctx(&self, event: HookEvent, tool_name: &str, input: &Value, reason: &str) -> HookContext {
        HookContext {
            event: event.as_str().to_string(),
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(input.clone()),
            tool_output: None,
            tool_error: None,
            error: Some(reason.to_string()),
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a minimal `HookContext` for lifecycle events (CwdChanged, ConfigChange, etc.).
    #[allow(dead_code)] // reserved for lifecycle hook events
    pub(crate) fn lifecycle_ctx(&self, event: HookEvent) -> HookContext {
        HookContext {
            event: event.as_str().to_string(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            tool_error: None,
            error: None,
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Build a `HookContext` for task events.
    pub(crate) fn task_ctx(&self, event: HookEvent, task_desc: &str, status: Option<String>) -> HookContext {
        let mut input = serde_json::json!({"task": task_desc});
        if let Some(s) = status {
            input["status"] = serde_json::Value::String(s);
        }
        HookContext {
            event: event.as_str().to_string(),
            tool_name: None,
            tool_input: Some(input),
            tool_output: None,
            tool_error: None,
            error: None,
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
        }
    }

    /// Returns true if there are any hooks configured for the given event.
    pub(crate) fn has_hooks(&self, event: HookEvent) -> bool {
        !self.rules_for(event).is_empty()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
