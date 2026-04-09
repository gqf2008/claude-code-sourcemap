//! Coordinator mode — multi-agent orchestration.
//!
//! In coordinator mode the engine spawns background workers via `dispatch_agent`
//! (always async), and delivers their results as `<task-notification>` XML
//! injected into the coordinator's message stream as user-role messages.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use claude_core::message::{ContentBlock, Message, UserMessage};
use claude_core::tool::{Tool, ToolContext, ToolResult};

use crate::dispatch_agent::{AgentChannelMap, CancelTokenMap};

// ── Background agent tracking ────────────────────────────────────────────────

/// Status of a background worker agent.
#[derive(Debug, Clone)]
pub enum AgentStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Killed => write!(f, "killed"),
        }
    }
}

/// Tracks the state of a background agent.
#[derive(Debug, Clone)]
pub struct AgentTask {
    pub agent_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub prompt: String,
    pub status: AgentStatus,
    pub result: Option<String>,
    pub tool_use_count: u32,
    pub total_tokens: u64,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    /// Most recent tool activity (for real-time progress display).
    pub last_activity: Option<String>,
}

impl AgentTask {
    pub fn duration_ms(&self) -> u64 {
        let end = self.finished_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started_at).as_millis() as u64
    }
}

/// Shared registry of all background agents. Thread-safe.
#[derive(Clone)]
pub struct AgentTracker {
    agents: Arc<RwLock<HashMap<String, AgentTask>>>,
    /// Channel to send task-notification messages back to the coordinator loop.
    notification_tx: mpsc::UnboundedSender<TaskNotification>,
}

/// A task notification delivered to the coordinator's message queue.
#[derive(Debug, Clone)]
pub struct TaskNotification {
    pub agent_id: String,
    pub status: AgentStatus,
    pub summary: String,
    pub result: String,
    pub total_tokens: u64,
    pub tool_uses: u32,
    pub duration_ms: u64,
}

impl TaskNotification {
    /// Build the XML representation aligned with the TS coordinator protocol.
    pub fn to_xml(&self) -> String {
        format!(
            "<task-notification>\n\
             <task-id>{}</task-id>\n\
             <status>{}</status>\n\
             <summary>{}</summary>\n\
             <result>{}</result>\n\
             <usage>\n  \
               <total_tokens>{}</total_tokens>\n  \
               <tool_uses>{}</tool_uses>\n  \
               <duration_ms>{}</duration_ms>\n\
             </usage>\n\
             </task-notification>",
            xml_escape(&self.agent_id),
            self.status,
            xml_escape(&self.summary),
            xml_escape(&self.result),
            self.total_tokens,
            self.tool_uses,
            self.duration_ms,
        )
    }

    /// Convert to a user-role message for injection into the coordinator's conversation.
    pub fn to_message(&self) -> Message {
        Message::User(UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text {
                text: self.to_xml(),
            }],
        })
    }
}

impl AgentTracker {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<TaskNotification>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                agents: Arc::new(RwLock::new(HashMap::new())),
                notification_tx: tx,
            },
            rx,
        )
    }

    /// Register a new agent as running.
    pub async fn register(
        &self,
        agent_id: &str,
        prompt: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) {
        let task = AgentTask {
            agent_id: agent_id.to_string(),
            name: name.map(|s| s.to_string()),
            description: description.map(|s| s.to_string()),
            prompt: prompt.to_string(),
            status: AgentStatus::Running,
            result: None,
            tool_use_count: 0,
            total_tokens: 0,
            started_at: Instant::now(),
            finished_at: None,
            last_activity: None,
        };
        self.agents.write().await.insert(agent_id.to_string(), task);
    }

    /// Mark an agent as completed with its result and send notification.
    pub async fn complete(&self, agent_id: &str, result: String, tokens: u64, tool_uses: u32) {
        let duration_ms = {
            let mut agents = self.agents.write().await;
            if let Some(task) = agents.get_mut(agent_id) {
                task.status = AgentStatus::Completed;
                task.result = Some(result.clone());
                task.total_tokens = tokens;
                task.tool_use_count = tool_uses;
                task.finished_at = Some(Instant::now());
                task.duration_ms()
            } else {
                0
            }
        };

        let summary = if result.len() > 200 {
            let truncated: String = result.chars().take(200).collect();
            format!("{}...", truncated)
        } else {
            result.clone()
        };

        if let Err(e) = self.notification_tx.send(TaskNotification {
            agent_id: agent_id.to_string(),
            status: AgentStatus::Completed,
            summary,
            result,
            total_tokens: tokens,
            tool_uses,
            duration_ms,
        }) {
            tracing::warn!("Failed to send task notification for {}: {}", agent_id, e);
        }
    }

    /// Mark an agent as failed.
    pub async fn fail(&self, agent_id: &str, error: String) {
        let duration_ms = {
            let mut agents = self.agents.write().await;
            if let Some(task) = agents.get_mut(agent_id) {
                task.status = AgentStatus::Failed;
                task.result = Some(error.clone());
                task.finished_at = Some(Instant::now());
                task.duration_ms()
            } else {
                0
            }
        };

        if let Err(e) = self.notification_tx.send(TaskNotification {
            agent_id: agent_id.to_string(),
            status: AgentStatus::Failed,
            summary: error.clone(),
            result: error,
            total_tokens: 0,
            tool_uses: 0,
            duration_ms,
        }) {
            tracing::warn!("Failed to send task notification for {}: {}", agent_id, e);
        }
    }

    /// Mark an agent as killed.
    pub async fn kill(&self, agent_id: &str) {
        let duration_ms = {
            let mut agents = self.agents.write().await;
            if let Some(task) = agents.get_mut(agent_id) {
                task.status = AgentStatus::Killed;
                task.finished_at = Some(Instant::now());
                task.duration_ms()
            } else {
                0
            }
        };

        if let Err(e) = self.notification_tx.send(TaskNotification {
            agent_id: agent_id.to_string(),
            status: AgentStatus::Killed,
            summary: "Agent was stopped by coordinator".to_string(),
            result: String::new(),
            total_tokens: 0,
            tool_uses: 0,
            duration_ms,
        }) {
            tracing::warn!("Failed to send task notification for {}: {}", agent_id, e);
        }
    }

    /// Get all agent statuses.
    pub async fn list(&self) -> Vec<AgentTask> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Get a specific agent's task info.
    pub async fn get(&self, agent_id: &str) -> Option<AgentTask> {
        self.agents.read().await.get(agent_id).cloned()
    }

    /// Check if an agent is still running.
    pub async fn is_running(&self, agent_id: &str) -> bool {
        self.agents
            .read()
            .await
            .get(agent_id)
            .map(|t| matches!(t.status, AgentStatus::Running))
            .unwrap_or(false)
    }

    /// Look up an agent_id by its human-readable name.
    /// Used by SendMessage to resolve the `to` field.
    pub async fn lookup_by_name(&self, name: &str) -> Option<String> {
        self.agents
            .read()
            .await
            .values()
            .find(|t| t.name.as_deref() == Some(name))
            .map(|t| t.agent_id.clone())
    }

    /// Record real-time progress for a running agent (tool use, tokens, activity).
    pub async fn record_progress(
        &self,
        agent_id: &str,
        tool_use_count: u32,
        total_tokens: u64,
        last_activity: Option<String>,
    ) {
        let mut agents = self.agents.write().await;
        if let Some(task) = agents.get_mut(agent_id) {
            task.tool_use_count = tool_use_count;
            task.total_tokens = total_tokens;
            if let Some(activity) = last_activity {
                task.last_activity = Some(activity);
            }
        }
    }

    /// Remove an agent entry from the tracker (cleanup after notification sent).
    pub async fn remove(&self, agent_id: &str) {
        self.agents.write().await.remove(agent_id);
    }
}

// ── SendMessage tool ─────────────────────────────────────────────────────────

/// Tool for sending follow-up messages to running background agents.
/// Only available in coordinator mode.
pub struct SendMessageTool {
    pub tracker: AgentTracker,
    /// Channel to deliver follow-up messages to the background agent task.
    /// Key: agent_id → sender that can push text into that agent's input queue.
    pub agent_channels: AgentChannelMap,
}

#[async_trait::async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str { "SendMessage" }

    fn description(&self) -> &str {
        "Send a follow-up message to a running background agent. The message is queued \
         for the agent. Note: messages are delivered best-effort and may not be processed \
         if the agent completes before reading them."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "The agent_id of the running worker to send the message to."
                },
                "message": {
                    "type": "string",
                    "description": "The message content to send to the worker."
                }
            },
            "required": ["to", "message"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let to = input["to"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'to' field"))?;
        let message = input["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' field"))?;

        // Resolve `to` — try agent_id first, then fall back to name-based lookup
        let agent_id = if self.tracker.get(to).await.is_some() {
            to.to_string()
        } else if let Some(id) = self.tracker.lookup_by_name(to).await {
            id
        } else {
            return Ok(ToolResult::error(format!("No agent found with id or name '{}'", to)));
        };

        // Check the agent is running
        let Some(task) = self.tracker.get(&agent_id).await else {
            return Ok(ToolResult::error(format!("Agent '{}' no longer exists", agent_id)));
        };
        if !matches!(task.status, AgentStatus::Running) {
            return Ok(ToolResult::error(format!(
                "Agent '{}' is not running (status: {})",
                agent_id, task.status
            )));
        }

        let channels = self.agent_channels.read().await;
        if let Some(tx) = channels.get(&agent_id) {
            match tx.send(message.to_string()) {
                Ok(_) => Ok(ToolResult::text(format!(
                    "Message sent to agent '{}'",
                    agent_id
                ))),
                Err(_) => Ok(ToolResult::error(format!(
                    "Failed to send message — agent '{}' channel closed",
                    agent_id
                ))),
            }
        } else {
            Ok(ToolResult::error(format!(
                "No message channel for agent '{}' — agent may not support follow-ups",
                agent_id
            )))
        }
    }
}

// ── TaskStop tool ────────────────────────────────────────────────────────────

/// Tool for stopping a running background agent.
pub struct TaskStopTool {
    pub tracker: AgentTracker,
    pub cancel_tokens: CancelTokenMap,
}

#[async_trait::async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str { "TaskStop" }

    fn description(&self) -> &str {
        "Stop a running background agent. The agent will be killed and a task-notification \
         with status 'killed' will be delivered."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The ID of the agent to stop."
                }
            },
            "required": ["agent_id"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let agent_id = input["agent_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'agent_id'"))?;

        let task = self.tracker.get(agent_id).await;
        match task {
            None => Ok(ToolResult::error(format!("No agent found with id '{}'", agent_id))),
            Some(t) if !matches!(t.status, AgentStatus::Running) => {
                Ok(ToolResult::error(format!(
                    "Agent '{}' is already {} — cannot stop",
                    agent_id, t.status
                )))
            }
            Some(_) => {
                // Cancel via CancellationToken — the background loop will detect
                // cancellation and call tracker.kill() itself, so we only cancel the
                // token here and don't call kill() to avoid duplicate notifications.
                let tokens = self.cancel_tokens.read().await;
                if let Some(token) = tokens.get(agent_id) {
                    token.cancel();
                }
                Ok(ToolResult::text(format!("Agent '{}' stop requested", agent_id)))
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Build the list of tool names available to workers (excludes coordinator-only tools).
pub fn worker_tool_names(all_tools: &[&str]) -> Vec<String> {
    let excluded = [
        "Agent",
        "SendMessage",
        "TaskStop",
        "AskUserQuestion",
    ];
    all_tools
        .iter()
        .filter(|t| !excluded.contains(t))
        .map(|t| t.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: std::path::PathBuf::from("."),
            abort_signal: claude_core::tool::AbortSignal::default(),
            permission_mode: claude_core::permissions::PermissionMode::BypassAll,
            messages: vec![],
        }
    }

    fn result_text(result: &ToolResult) -> String {
        result.content.iter().filter_map(|c| {
            if let claude_core::message::ToolResultContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        }).collect::<Vec<_>>().join("")
    }

    #[test]
    fn test_notification_xml() {
        let n = TaskNotification {
            agent_id: "agent-123".into(),
            status: AgentStatus::Completed,
            summary: "Finished task".into(),
            result: "All <done> & good".into(),
            total_tokens: 1500,
            tool_uses: 5,
            duration_ms: 3200,
        };
        let xml = n.to_xml();
        assert!(xml.contains("<task-id>agent-123</task-id>"));
        assert!(xml.contains("<status>completed</status>"));
        assert!(xml.contains("&lt;done&gt; &amp; good"));
        assert!(xml.contains("<total_tokens>1500</total_tokens>"));
    }

    #[test]
    fn test_worker_tool_names() {
        let all = vec!["Bash", "Read", "Edit", "Agent", "SendMessage", "AskUserQuestion"];
        let worker = worker_tool_names(&all);
        assert_eq!(worker, vec!["Bash", "Read", "Edit"]);
    }

    #[test]
    fn agent_status_display() {
        assert_eq!(AgentStatus::Running.to_string(), "running");
        assert_eq!(AgentStatus::Completed.to_string(), "completed");
        assert_eq!(AgentStatus::Failed.to_string(), "failed");
        assert_eq!(AgentStatus::Killed.to_string(), "killed");
    }

    #[test]
    fn notification_to_message_is_user() {
        let n = TaskNotification {
            agent_id: "a1".into(),
            status: AgentStatus::Completed,
            summary: "done".into(),
            result: "ok".into(),
            total_tokens: 100,
            tool_uses: 2,
            duration_ms: 500,
        };
        let msg = n.to_message();
        match msg {
            Message::User(u) => {
                assert!(!u.uuid.is_empty());
                assert_eq!(u.content.len(), 1);
                if let ContentBlock::Text { text } = &u.content[0] {
                    assert!(text.contains("<task-notification>"));
                } else {
                    panic!("Expected text block");
                }
            }
            _ => panic!("Expected user message"),
        }
    }

    #[tokio::test]
    async fn tracker_register_and_complete() {
        let (tracker, mut rx) = AgentTracker::new();
        tracker.register("test-1", "Do something", None, None).await;
        tracker.complete("test-1", "Done!".into(), 500, 3).await;

        let notif = rx.try_recv().unwrap();
        assert_eq!(notif.agent_id, "test-1");
        assert_eq!(notif.result, "Done!");
        assert_eq!(notif.total_tokens, 500);
        assert_eq!(notif.tool_uses, 3);
        assert!(matches!(notif.status, AgentStatus::Completed));
    }

    #[tokio::test]
    async fn tracker_register_and_fail() {
        let (tracker, mut rx) = AgentTracker::new();
        tracker.register("fail-1", "Will fail", None, None).await;
        tracker.fail("fail-1", "Connection error".into()).await;

        let notif = rx.try_recv().unwrap();
        assert_eq!(notif.agent_id, "fail-1");
        assert!(matches!(notif.status, AgentStatus::Failed));
        assert_eq!(notif.result, "Connection error");
    }

    #[tokio::test]
    async fn tracker_kill() {
        let (tracker, mut rx) = AgentTracker::new();
        tracker.register("kill-1", "Long task", None, None).await;
        tracker.kill("kill-1").await;

        let notif = rx.try_recv().unwrap();
        assert!(matches!(notif.status, AgentStatus::Killed));
    }

    #[test]
    fn worker_tool_names_empty() {
        let worker = worker_tool_names(&Vec::<&str>::new());
        assert!(worker.is_empty());
    }

    #[test]
    fn worker_tool_names_no_exclusions() {
        let all = vec!["Bash", "Read", "Glob"];
        let worker = worker_tool_names(&all);
        assert_eq!(worker, vec!["Bash", "Read", "Glob"]);
    }

    #[test]
    fn notification_xml_escapes_special_chars() {
        let n = TaskNotification {
            agent_id: "a&b<c>d".into(),
            status: AgentStatus::Completed,
            summary: "sum".into(),
            result: "res".into(),
            total_tokens: 0,
            tool_uses: 0,
            duration_ms: 0,
        };
        let xml = n.to_xml();
        assert!(xml.contains("a&amp;b&lt;c&gt;d"));
    }

    // ── AgentTracker state management tests ──────────────────────────────────

    #[tokio::test]
    async fn tracker_get_returns_none_for_unknown() {
        let (tracker, _rx) = AgentTracker::new();
        assert!(tracker.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn tracker_is_running_unknown_agent() {
        let (tracker, _rx) = AgentTracker::new();
        assert!(!tracker.is_running("no-such-agent").await);
    }

    #[tokio::test]
    async fn tracker_is_running_after_complete() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("r1", "test", None, None).await;
        assert!(tracker.is_running("r1").await);

        tracker.complete("r1", "done".into(), 100, 1).await;
        assert!(!tracker.is_running("r1").await);
    }

    #[tokio::test]
    async fn tracker_lookup_by_name() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("id-1", "task", Some("my-worker"), None).await;

        assert_eq!(tracker.lookup_by_name("my-worker").await, Some("id-1".into()));
        assert_eq!(tracker.lookup_by_name("other").await, None);
    }

    #[tokio::test]
    async fn tracker_record_progress() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("p1", "task", None, None).await;

        tracker.record_progress("p1", 5, 1200, Some("Running Bash".into())).await;

        let task = tracker.get("p1").await.unwrap();
        assert_eq!(task.tool_use_count, 5);
        assert_eq!(task.total_tokens, 1200);
        assert_eq!(task.last_activity.as_deref(), Some("Running Bash"));
    }

    #[tokio::test]
    async fn tracker_record_progress_preserves_activity_on_none() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("p2", "task", None, None).await;
        tracker.record_progress("p2", 1, 100, Some("First".into())).await;
        // None should NOT overwrite existing activity
        tracker.record_progress("p2", 2, 200, None).await;

        let task = tracker.get("p2").await.unwrap();
        assert_eq!(task.tool_use_count, 2);
        assert_eq!(task.last_activity.as_deref(), Some("First"));
    }

    #[tokio::test]
    async fn tracker_remove() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("rm-1", "task", None, None).await;
        assert!(tracker.get("rm-1").await.is_some());

        tracker.remove("rm-1").await;
        assert!(tracker.get("rm-1").await.is_none());
    }

    #[tokio::test]
    async fn tracker_list_returns_all() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("a1", "t1", None, None).await;
        tracker.register("a2", "t2", None, None).await;
        tracker.register("a3", "t3", None, None).await;

        let list = tracker.list().await;
        assert_eq!(list.len(), 3);
    }

    #[tokio::test]
    async fn tracker_duplicate_register_overwrites() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("dup", "first prompt", None, None).await;
        tracker.register("dup", "second prompt", None, None).await;

        let task = tracker.get("dup").await.unwrap();
        assert_eq!(task.prompt, "second prompt");
    }

    #[tokio::test]
    async fn tracker_complete_nonexistent_still_notifies() {
        let (tracker, mut rx) = AgentTracker::new();
        // Complete an agent that was never registered
        tracker.complete("ghost", "result".into(), 0, 0).await;

        let notif = rx.try_recv().unwrap();
        assert_eq!(notif.agent_id, "ghost");
        assert_eq!(notif.duration_ms, 0); // fallback duration
    }

    #[tokio::test]
    async fn tracker_fail_nonexistent_still_notifies() {
        let (tracker, mut rx) = AgentTracker::new();
        tracker.fail("ghost", "err".into()).await;

        let notif = rx.try_recv().unwrap();
        assert!(matches!(notif.status, AgentStatus::Failed));
        assert_eq!(notif.duration_ms, 0);
    }

    #[tokio::test]
    async fn complete_truncates_long_summary() {
        let (tracker, mut rx) = AgentTracker::new();
        tracker.register("long", "task", None, None).await;

        let long_result = "x".repeat(500);
        tracker.complete("long", long_result.clone(), 0, 0).await;

        let notif = rx.try_recv().unwrap();
        assert_eq!(notif.result.len(), 500); // result is full
        assert!(notif.summary.len() <= 203); // summary is truncated (200 + "...")
        assert!(notif.summary.ends_with("..."));
    }

    #[tokio::test]
    async fn complete_short_summary_not_truncated() {
        let (tracker, mut rx) = AgentTracker::new();
        tracker.register("short", "task", None, None).await;

        tracker.complete("short", "ok".into(), 0, 0).await;

        let notif = rx.try_recv().unwrap();
        assert_eq!(notif.summary, "ok");
        assert!(!notif.summary.ends_with("..."));
    }

    // ── Tool tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn send_message_to_unknown_agent() {
        let (tracker, _rx) = AgentTracker::new();
        let channels: AgentChannelMap = Arc::new(RwLock::new(HashMap::new()));

        let tool = SendMessageTool { tracker, agent_channels: channels };
        let ctx = test_context();
        let input = json!({"to": "no-such-agent", "message": "hello"});

        let result = tool.call(input, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("No agent found"));
    }

    #[tokio::test]
    async fn send_message_to_completed_agent() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("done-agent", "task", None, None).await;
        tracker.complete("done-agent", "result".into(), 0, 0).await;

        let channels: AgentChannelMap = Arc::new(RwLock::new(HashMap::new()));
        let tool = SendMessageTool { tracker, agent_channels: channels };
        let ctx = test_context();
        let input = json!({"to": "done-agent", "message": "hello"});

        let result = tool.call(input, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("not running"));
    }

    #[tokio::test]
    async fn send_message_by_name() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("id-abc", "task", Some("my-worker"), None).await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let channels: AgentChannelMap = Arc::new(RwLock::new(HashMap::new()));
        channels.write().await.insert("id-abc".into(), tx);

        let tool = SendMessageTool { tracker, agent_channels: channels };
        let ctx = test_context();
        let input = json!({"to": "my-worker", "message": "do something"});

        let result = tool.call(input, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(rx.try_recv().unwrap(), "do something");
    }

    #[tokio::test]
    async fn task_stop_nonexistent() {
        let (tracker, _rx) = AgentTracker::new();
        let tokens: CancelTokenMap = Arc::new(RwLock::new(HashMap::new()));

        let tool = TaskStopTool { tracker, cancel_tokens: tokens };
        let ctx = test_context();
        let input = json!({"agent_id": "nope"});

        let result = tool.call(input, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("No agent found"));
    }

    #[tokio::test]
    async fn task_stop_already_completed() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("fin", "task", None, None).await;
        tracker.complete("fin", "done".into(), 0, 0).await;

        let tokens: CancelTokenMap = Arc::new(RwLock::new(HashMap::new()));
        let tool = TaskStopTool { tracker, cancel_tokens: tokens };
        let ctx = test_context();
        let input = json!({"agent_id": "fin"});

        let result = tool.call(input, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("already"));
    }

    #[tokio::test]
    async fn task_stop_cancels_token() {
        let (tracker, _rx) = AgentTracker::new();
        tracker.register("stop-me", "task", None, None).await;

        let token = tokio_util::sync::CancellationToken::new();
        let tokens: CancelTokenMap = Arc::new(RwLock::new(HashMap::new()));
        tokens.write().await.insert("stop-me".into(), token.clone());

        let tool = TaskStopTool { tracker, cancel_tokens: tokens };
        let ctx = test_context();
        let input = json!({"agent_id": "stop-me"});

        assert!(!token.is_cancelled());
        let result = tool.call(input, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(token.is_cancelled());
    }

    // ── xml_escape tests ─────────────────────────────────────────────────────

    #[test]
    fn xml_escape_all_entities() {
        assert_eq!(xml_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn xml_escape_no_special() {
        assert_eq!(xml_escape("hello world"), "hello world");
    }

    #[test]
    fn xml_escape_empty() {
        assert_eq!(xml_escape(""), "");
    }
}
