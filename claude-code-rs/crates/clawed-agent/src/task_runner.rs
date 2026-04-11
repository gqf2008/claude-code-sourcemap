//! Multi-turn autonomous task runner.
//!
//! Drives `query_stream` to completion, collecting live progress via a callback
//! and returning a structured `TaskResult`.  This is the Rust equivalent of the
//! TypeScript `runAgent()` / `query()` generator consumer.
//!
//! # Architecture (aligned with TS runAgent.ts + query.ts)
//!
//! ```text
//!   run_task()
//!     │  submits prompt to engine
//!     │  polls AgentEvent stream
//!     │    TextDelta     → accumulate output, fire on_progress(Text)
//!     │    ToolUseStart  → fire on_progress(ToolUse)
//!     │    ToolResult    → fire on_progress(ToolDone)
//!     │    TurnComplete  → fire on_progress(Turn), check abort
//!     │    Error         → mark failed, fire on_progress(Done)
//!     └──► returns TaskResult { output, turns, tool_uses, success, … }
//! ```

use std::time::{Duration, Instant};

use tokio_stream::StreamExt;

use crate::engine::QueryEngine;
use crate::hooks::HookEvent;
use crate::query::AgentEvent;
use clawed_core::message::StopReason;

// ── Public types ──────────────────────────────────────────────────────────────

/// Structured result of a completed task.
#[derive(Debug, Clone)]
pub struct TaskResult {
    /// Final text output produced by the model.
    pub output: String,
    /// Total number of tool invocations.
    pub tool_uses: u32,
    /// Number of agent turns (model calls).
    pub turns: u32,
    /// Wall-clock duration.
    pub elapsed: Duration,
    /// Token usage.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Completion reason.
    pub reason: CompletionReason,
}

impl TaskResult {
    pub fn success(&self) -> bool {
        matches!(self.reason, CompletionReason::Completed | CompletionReason::EndTurn)
    }
}

/// Why the task stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionReason {
    /// Claude finished normally (end_turn).
    Completed,
    /// stop_reason was end_turn (same as Completed).
    EndTurn,
    /// Claude hit the max-tokens limit.
    MaxTokens,
    /// stop_reason was a stop sequence.
    StopSequence,
    /// Task was aborted by the user or caller.
    Aborted,
    /// Max turns limit was reached.
    MaxTurns,
    /// API or stream error.
    Error(String),
}

impl std::fmt::Display for CompletionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed | Self::EndTurn => write!(f, "completed"),
            Self::MaxTokens    => write!(f, "max_tokens"),
            Self::StopSequence => write!(f, "stop_sequence"),
            Self::Aborted      => write!(f, "aborted"),
            Self::MaxTurns     => write!(f, "max_turns"),
            Self::Error(e)     => write!(f, "error: {}", e),
        }
    }
}

/// Live progress events emitted during task execution.
#[derive(Debug, Clone)]
pub enum TaskProgress {
    /// Agent turn started.
    TurnStart { turn: u32 },
    /// Text token from the model.
    Text(String),
    /// A tool invocation has started.
    ToolUse { name: String, turn: u32 },
    /// A tool invocation completed.
    ToolDone { name: String, is_error: bool, text: Option<String> },
    /// Token usage update.
    Tokens { input: u64, output: u64 },
    /// Task fully completed.
    Done(TaskResult),
}

// ── Core runner ───────────────────────────────────────────────────────────────

/// Run a task to completion, calling `on_progress` for each live event.
///
/// # Errors
/// Returns `Err` only if the engine itself panics — task-level errors
/// (API errors, max turns) are encoded in `TaskResult::reason`.
pub async fn run_task<F>(
    engine: &QueryEngine,
    task: &str,
    mut on_progress: F,
) -> TaskResult
where
    F: FnMut(TaskProgress) + Send,
{
    let started = Instant::now();
    let mut output = String::new();
    let mut tool_uses: u32 = 0;
    let mut turns: u32 = 0;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut reason = CompletionReason::Completed;
    let mut current_tool_name = String::new();

    on_progress(TaskProgress::TurnStart { turn: 0 });

    // ── TaskCreated hook ─────────────────────────────────────────────────
    if engine.hooks().has_hooks(HookEvent::TaskCreated) {
        let ctx = engine.hooks().task_ctx(HookEvent::TaskCreated, task, None);
        let _ = engine.hooks().run(HookEvent::TaskCreated, ctx).await;
    }

    let mut stream = engine.submit(task).await;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                output.push_str(&text);
                on_progress(TaskProgress::Text(text));
            }

            AgentEvent::ThinkingDelta(_) => {}

            AgentEvent::ToolUseStart { name, .. } => {
                current_tool_name = name.clone();
                on_progress(TaskProgress::ToolUse { name, turn: turns });
            }

            AgentEvent::ToolUseReady { .. } => {
                // Input is ready — task_runner doesn't need to act on this
            }

            AgentEvent::ToolResult { is_error, text, .. } => {
                tool_uses += 1;
                on_progress(TaskProgress::ToolDone {
                    name: current_tool_name.clone(),
                    is_error,
                    text,
                });
            }

            AgentEvent::AssistantMessage(_) => {}

            AgentEvent::TurnComplete { stop_reason } => {
                turns += 1;
                on_progress(TaskProgress::TurnStart { turn: turns });
                reason = match stop_reason {
                    StopReason::EndTurn        => CompletionReason::EndTurn,
                    StopReason::MaxTokens      => CompletionReason::MaxTokens,
                    StopReason::StopSequence   => CompletionReason::StopSequence,
                    StopReason::ToolUse        => continue, // more turns coming
                };
            }

            AgentEvent::UsageUpdate(u) => {
                input_tokens += u.input_tokens;
                output_tokens += u.output_tokens;
                on_progress(TaskProgress::Tokens {
                    input: input_tokens,
                    output: output_tokens,
                });
            }

            AgentEvent::Error(msg) => {
                reason = CompletionReason::Error(msg);
                break;
            }

            AgentEvent::MaxTurns { .. } => {
                reason = CompletionReason::MaxTurns;
                break;
            }

            // New event types — task_runner just ignores them
            AgentEvent::TurnTokens { .. }
            | AgentEvent::ContextWarning { .. }
            | AgentEvent::CompactStart
            | AgentEvent::CompactComplete { .. } => {}
        }
    }

    let result = TaskResult {
        output: output.trim_end().to_string(),
        tool_uses,
        turns,
        elapsed: started.elapsed(),
        input_tokens,
        output_tokens,
        reason,
    };

    // ── TaskCompleted hook ───────────────────────────────────────────────
    if engine.hooks().has_hooks(HookEvent::TaskCompleted) {
        let ctx = engine.hooks().task_ctx(
            HookEvent::TaskCompleted,
            task,
            Some(result.reason.to_string()),
        );
        let _ = engine.hooks().run(HookEvent::TaskCompleted, ctx).await;
    }

    on_progress(TaskProgress::Done(result.clone()));
    result
}

// ── Convenience wrapper: collect output silently ──────────────────────────────

/// Run a task and return its output text, discarding progress events.
/// Useful for sub-agents or testing.
pub async fn run_task_silent(engine: &QueryEngine, task: &str) -> TaskResult {
    run_task(engine, task, |_| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TaskResult ───────────────────────────────────────────────────────

    #[test]
    fn task_result_success_completed() {
        let r = TaskResult {
            output: "done".into(),
            tool_uses: 0,
            turns: 1,
            elapsed: Duration::from_secs(1),
            input_tokens: 100,
            output_tokens: 50,
            reason: CompletionReason::Completed,
        };
        assert!(r.success());
    }

    #[test]
    fn task_result_success_end_turn() {
        let r = TaskResult {
            output: "".into(),
            tool_uses: 0,
            turns: 1,
            elapsed: Duration::ZERO,
            input_tokens: 0,
            output_tokens: 0,
            reason: CompletionReason::EndTurn,
        };
        assert!(r.success());
    }

    #[test]
    fn task_result_failure_max_tokens() {
        let r = TaskResult {
            output: "partial".into(),
            tool_uses: 3,
            turns: 5,
            elapsed: Duration::from_secs(30),
            input_tokens: 5000,
            output_tokens: 2000,
            reason: CompletionReason::MaxTokens,
        };
        assert!(!r.success());
    }

    #[test]
    fn task_result_failure_error() {
        let r = TaskResult {
            output: "".into(),
            tool_uses: 0,
            turns: 0,
            elapsed: Duration::ZERO,
            input_tokens: 0,
            output_tokens: 0,
            reason: CompletionReason::Error("API error".into()),
        };
        assert!(!r.success());
    }

    #[test]
    fn task_result_failure_aborted() {
        let r = TaskResult {
            output: "".into(),
            tool_uses: 0,
            turns: 0,
            elapsed: Duration::ZERO,
            input_tokens: 0,
            output_tokens: 0,
            reason: CompletionReason::Aborted,
        };
        assert!(!r.success());
    }

    #[test]
    fn task_result_failure_max_turns() {
        let r = TaskResult {
            output: "".into(),
            tool_uses: 0,
            turns: 0,
            elapsed: Duration::ZERO,
            input_tokens: 0,
            output_tokens: 0,
            reason: CompletionReason::MaxTurns,
        };
        assert!(!r.success());
    }

    // ── CompletionReason Display ─────────────────────────────────────────

    #[test]
    fn completion_reason_display() {
        assert_eq!(CompletionReason::Completed.to_string(), "completed");
        assert_eq!(CompletionReason::EndTurn.to_string(), "completed");
        assert_eq!(CompletionReason::MaxTokens.to_string(), "max_tokens");
        assert_eq!(CompletionReason::StopSequence.to_string(), "stop_sequence");
        assert_eq!(CompletionReason::Aborted.to_string(), "aborted");
        assert_eq!(CompletionReason::MaxTurns.to_string(), "max_turns");
        assert_eq!(
            CompletionReason::Error("oops".into()).to_string(),
            "error: oops"
        );
    }

    #[test]
    fn completion_reason_equality() {
        assert_eq!(CompletionReason::Completed, CompletionReason::Completed);
        assert_ne!(CompletionReason::Completed, CompletionReason::EndTurn);
        assert_eq!(
            CompletionReason::Error("a".into()),
            CompletionReason::Error("a".into())
        );
        assert_ne!(
            CompletionReason::Error("a".into()),
            CompletionReason::Error("b".into())
        );
    }

    // ── TaskProgress variants ────────────────────────────────────────────

    #[test]
    fn task_progress_turn_start() {
        let p = TaskProgress::TurnStart { turn: 3 };
        assert!(matches!(p, TaskProgress::TurnStart { turn: 3 }));
    }

    #[test]
    fn task_progress_text() {
        let p = TaskProgress::Text("hello".into());
        if let TaskProgress::Text(t) = p {
            assert_eq!(t, "hello");
        } else {
            panic!("Expected Text");
        }
    }

    #[test]
    fn task_progress_tool_use() {
        let p = TaskProgress::ToolUse { name: "Bash".into(), turn: 1 };
        if let TaskProgress::ToolUse { name, turn } = p {
            assert_eq!(name, "Bash");
            assert_eq!(turn, 1);
        } else {
            panic!("Expected ToolUse");
        }
    }

    #[test]
    fn task_progress_tool_done() {
        let p = TaskProgress::ToolDone {
            name: "FileRead".into(),
            is_error: false,
            text: Some("contents".into()),
        };
        if let TaskProgress::ToolDone { name, is_error, text } = p {
            assert_eq!(name, "FileRead");
            assert!(!is_error);
            assert_eq!(text.unwrap(), "contents");
        } else {
            panic!("Expected ToolDone");
        }
    }

    #[test]
    fn task_progress_tokens() {
        let p = TaskProgress::Tokens { input: 100, output: 50 };
        assert!(matches!(p, TaskProgress::Tokens { input: 100, output: 50 }));
    }

    #[test]
    fn task_progress_done_carries_result() {
        let result = TaskResult {
            output: "ok".into(),
            tool_uses: 2,
            turns: 1,
            elapsed: Duration::from_millis(500),
            input_tokens: 200,
            output_tokens: 100,
            reason: CompletionReason::Completed,
        };
        let p = TaskProgress::Done(result);
        if let TaskProgress::Done(r) = p {
            assert!(r.success());
            assert_eq!(r.tool_uses, 2);
        } else {
            panic!("Expected Done");
        }
    }
}
