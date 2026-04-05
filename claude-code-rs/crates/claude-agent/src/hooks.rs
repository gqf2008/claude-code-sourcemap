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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use claude_core::config::{HookCommandDef, HooksConfig, HookRule};

// ── Regex cache for hook matchers ────────────────────────────────────────────

/// Cached compiled regexes for hook tool matchers.
/// Avoids recompiling the same pattern on every tool invocation.
static REGEX_CACHE: std::sync::OnceLock<Mutex<HashMap<String, Option<regex::Regex>>>> =
    std::sync::OnceLock::new();

fn get_cached_regex(pattern: &str) -> Option<regex::Regex> {
    let cache_mutex = REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = match cache_mutex.lock() {
        Ok(c) => c,
        Err(_) => return regex::Regex::new(pattern).ok(),
    };
    cache
        .entry(pattern.to_string())
        .or_insert_with(|| regex::Regex::new(pattern).ok())
        .clone()
}

// ── Public event enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    StopFailure,
    UserPromptSubmit,
    SessionStart,
    SessionEnd,
    Setup,
    PreCompact,
    PostCompact,
    SubagentStart,
    SubagentStop,
    Notification,
    /// Fired after model sampling, before tool execution.
    PostSampling,
    // ── New events (TS parity) ──
    /// Permission request shown to user.
    PermissionRequest,
    /// Permission denied by user or rule.
    PermissionDenied,
    /// CLAUDE.md / instructions loaded or changed.
    InstructionsLoaded,
    /// Working directory changed.
    CwdChanged,
    /// Watched file changed on disk.
    FileChanged,
    /// Configuration settings changed.
    ConfigChange,
    /// Task created (task management).
    TaskCreated,
    /// Task completed.
    TaskCompleted,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::Setup => "Setup",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::Notification => "Notification",
            Self::PostSampling => "PostSampling",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::InstructionsLoaded => "InstructionsLoaded",
            Self::CwdChanged => "CwdChanged",
            Self::FileChanged => "FileChanged",
            Self::ConfigChange => "ConfigChange",
            Self::TaskCreated => "TaskCreated",
            Self::TaskCompleted => "TaskCompleted",
        }
    }
}

// ── Context passed to every hook invocation ──────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HookContext {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Compact trigger: "manual" or "auto"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Post-compact summary text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Agent ID for subagent events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub cwd: String,
    pub session_id: String,
}

// ── Hook decision returned to caller ────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum HookDecision {
    /// Proceed normally.
    Continue,
    /// Block the action; reason shown to Claude.
    Block { reason: String },
    /// (Stop hooks only) inject `feedback` as a new user message and loop.
    FeedbackAndContinue { feedback: String },
    /// Append extra text to the current payload (prompt / system prompt).
    AppendContext { text: String },
    /// Replace tool input with a new value.
    ModifyInput { new_input: Value },
}

// ── Optional JSON response hook scripts can emit on stdout ──────────────────

#[derive(Debug, Deserialize)]
struct HookJsonResponse {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

// ── Matcher ──────────────────────────────────────────────────────────────────

fn tool_matches(matcher: &Option<String>, tool_name: &str) -> bool {
    match matcher {
        None => true,
        Some(pat) if pat.is_empty() || pat == "*" => true,
        Some(pat) => {
            let is_regex = pat.contains('|') || pat.contains('^')
                || pat.contains('$') || pat.contains('.')
                || pat.contains('*') || pat.contains('+') || pat.contains('?')
                || pat.contains('[') || pat.contains('(');
            if is_regex {
                get_cached_regex(pat)
                    .map(|re| re.is_match(tool_name))
                    .unwrap_or(false)
            } else {
                pat == tool_name
            }
        }
    }
}

// ── Shell command execution ──────────────────────────────────────────────────

const DEFAULT_TIMEOUT_MS: u64 = 60_000;

async fn run_shell_hook(
    cmd_def: &HookCommandDef,
    ctx: &HookContext,
    cwd: &Path,
) -> anyhow::Result<(i32, String)> {
    let ctx_json = serde_json::to_string(ctx)?;
    let timeout = Duration::from_millis(cmd_def.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    #[cfg(windows)]
    let mut child = tokio::process::Command::new("cmd")
        .args(["/C", &cmd_def.command])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    #[cfg(not(windows))]
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", &cmd_def.command])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    // Write context JSON to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(ctx_json.as_bytes()).await.ok();
        // Drop stdin to signal EOF
    }

    let output = tokio::time::timeout(timeout, child.wait_with_output()).await??;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((exit_code, stdout))
}

/// Interpret a hook's (exit_code, stdout) for a given event.
fn interpret_output(event: HookEvent, exit_code: i32, stdout: String) -> HookDecision {
    match exit_code {
        0 => {
            if stdout.is_empty() {
                return HookDecision::Continue;
            }
            // Try to parse a structured JSON response first
            if let Ok(resp) = serde_json::from_str::<HookJsonResponse>(&stdout) {
                match resp.decision.as_deref() {
                    Some("block") => return HookDecision::Block {
                        reason: resp.reason.unwrap_or(stdout),
                    },
                    Some("modify") if resp.input.is_some() => return HookDecision::ModifyInput {
                        // Safety: guarded by is_some() check above
                        new_input: resp.input.expect("input checked above"),
                    },
                    // Explicit "approve" or "continue" → don't treat stdout as context
                    Some("approve") | Some("continue") | Some("") => return HookDecision::Continue,
                    _ => {}
                }
            }
            // Plain-text stdout → extra context only for injection events
            if matches!(
                event,
                HookEvent::UserPromptSubmit
                    | HookEvent::SessionStart
                    | HookEvent::SubagentStart
                    | HookEvent::PreCompact
            ) {
                HookDecision::AppendContext { text: stdout }
            } else {
                HookDecision::Continue
            }
        }
        2 if matches!(event, HookEvent::Stop | HookEvent::SubagentStop) => {
            // Exit 2 on Stop/SubagentStop hook → inject feedback and keep the loop going
            HookDecision::FeedbackAndContinue {
                feedback: if stdout.is_empty() { "Continue.".into() } else { stdout },
            }
        }
        2 if matches!(event, HookEvent::PreCompact) => {
            // Exit 2 on PreCompact → block compaction
            HookDecision::Block {
                reason: if stdout.is_empty() {
                    "PreCompact hook blocked compaction".into()
                } else {
                    stdout
                },
            }
        }
        _ => {
            // StopFailure, Notification: fire-and-forget, always Continue
            if matches!(event, HookEvent::StopFailure | HookEvent::Notification | HookEvent::SessionEnd | HookEvent::PostCompact) {
                HookDecision::Continue
            } else {
                // Non-zero, non-2 → block with stdout as reason
                HookDecision::Block {
                    reason: if stdout.is_empty() {
                        format!("Hook exited with code {}", exit_code)
                    } else {
                        stdout
                    },
                }
            }
        }
    }
}

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

