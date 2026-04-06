//! Hook execution: regex cache, tool matching, shell execution, output interpretation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use claude_core::config::HookCommandDef;

use super::types::{HookContext, HookDecision, HookEvent, HookJsonResponse};

// ── Regex cache for hook matchers ────────────────────────────────────────────

/// Cached compiled regexes for hook tool matchers.
/// Avoids recompiling the same pattern on every tool invocation.
static REGEX_CACHE: std::sync::OnceLock<Mutex<HashMap<String, Option<regex::Regex>>>> =
    std::sync::OnceLock::new();

pub(super) fn get_cached_regex(pattern: &str) -> Option<regex::Regex> {
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

// ── Matcher ──────────────────────────────────────────────────────────────────

pub(super) fn tool_matches(matcher: &Option<String>, tool_name: &str) -> bool {
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

pub(super) async fn run_shell_hook(
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
pub(super) fn interpret_output(event: HookEvent, exit_code: i32, stdout: String) -> HookDecision {
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


