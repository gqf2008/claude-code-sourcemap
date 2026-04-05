//! Structured task management tools — TaskCreate, TaskUpdate, TaskGet, TaskList.
//!
//! Aligned with TS `TaskCreateTool.ts`, `TaskUpdateTool.ts`, `TaskGetTool.ts`,
//! `TaskListTool.ts`.  Tasks are persisted as individual JSON files under
//! `~/.claude/tasks/`.  Each task has an ID, subject, description, status,
//! owner, and dependency edges (blocks / blocked_by).
//!
//! These replace the simpler TodoRead/TodoWrite tools with a richer model
//! suitable for multi-agent coordination.

use std::path::PathBuf;

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

// ── Data model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Deleted,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
            Self::Blocked => write!(f, "blocked"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub subject: String,
    pub description: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub owner: Option<String>,
    /// IDs of tasks that this task blocks (downstream dependents).
    #[serde(default)]
    pub blocks: Vec<String>,
    /// IDs of tasks that block this task (upstream dependencies).
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Map<String, Value>,
}

// ── Persistence ──────────────────────────────────────────────────────────────

fn tasks_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("tasks")
}

fn task_path(id: &str) -> PathBuf {
    tasks_dir().join(format!("{}.json", id))
}

fn save_task(task: &Task) -> anyhow::Result<()> {
    let dir = tasks_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(task)?;
    std::fs::write(task_path(&task.id), json)?;
    Ok(())
}

fn load_task(id: &str) -> Option<Task> {
    let path = task_path(id);
    if !path.exists() {
        return None;
    }
    let json = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&json).ok()
}

fn load_all_tasks() -> Vec<Task> {
    let dir = tasks_dir();
    if !dir.exists() {
        return Vec::new();
    }
    std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
            let json = std::fs::read_to_string(&path).ok()?;
            serde_json::from_str::<Task>(&json).ok()
        })
        .filter(|t| t.status != TaskStatus::Deleted)
        .collect()
}

fn gen_task_id() -> String {
    let uuid = Uuid::new_v4().to_string();
    format!("t-{}", &uuid[..8])
}

// ── TaskCreateTool ───────────────────────────────────────────────────────────

pub struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str { "task_create" }

    fn description(&self) -> &str {
        "Create a new task for tracking progress. Use this when breaking down a complex \
         problem into steps. Each task has a subject (brief title) and description (what \
         to do). Returns the task ID for use with task_update."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subject": {
                    "type": "string",
                    "description": "Brief title of the task (1 line)"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed description of what needs to be done"
                },
                "blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that must complete before this one"
                }
            },
            "required": ["subject", "description"]
        })
    }

    fn is_read_only(&self) -> bool { false }
    fn is_concurrency_safe(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let subject = input["subject"].as_str().unwrap_or("").to_string();
        let description = input["description"].as_str().unwrap_or("").to_string();
        let blocked_by: Vec<String> = input["blocked_by"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        if subject.is_empty() {
            return Ok(ToolResult::error("subject is required"));
        }

        let status = if blocked_by.is_empty() {
            TaskStatus::Pending
        } else {
            TaskStatus::Blocked
        };

        let task = Task {
            id: gen_task_id(),
            subject,
            description,
            status,
            owner: None,
            blocks: Vec::new(),
            blocked_by,
            metadata: serde_json::Map::new(),
        };

        save_task(&task)?;

        Ok(ToolResult::text(format!(
            "Created task {} ({}) — status: {}",
            task.id, task.subject, task.status
        )))
    }
}

// ── TaskUpdateTool ───────────────────────────────────────────────────────────

pub struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str { "task_update" }

    fn description(&self) -> &str {
        "Update an existing task's status, subject, description, or dependencies. \
         Use this to mark tasks as in_progress when starting, completed when done, \
         or to add/remove blocking dependencies."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "blocked", "deleted"],
                    "description": "New status for the task"
                },
                "subject": {
                    "type": "string",
                    "description": "Updated title"
                },
                "description": {
                    "type": "string",
                    "description": "Updated description"
                },
                "add_blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs to add as upstream blockers"
                },
                "remove_blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs to remove from upstream blockers"
                }
            },
            "required": ["task_id"]
        })
    }

    fn is_read_only(&self) -> bool { false }
    fn is_concurrency_safe(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let task_id = input["task_id"].as_str().unwrap_or("");
        if task_id.is_empty() {
            return Ok(ToolResult::error("task_id is required"));
        }

        let mut task = match load_task(task_id) {
            Some(t) => t,
            None => return Ok(ToolResult::error(format!("Task not found: {}", task_id))),
        };

        let mut updated = Vec::new();

        if let Some(status) = input["status"].as_str() {
            let new_status = match status {
                "pending" => TaskStatus::Pending,
                "in_progress" => TaskStatus::InProgress,
                "completed" => TaskStatus::Completed,
                "blocked" => TaskStatus::Blocked,
                "deleted" => TaskStatus::Deleted,
                _ => return Ok(ToolResult::error(format!("Invalid status: {}", status))),
            };
            task.status = new_status;
            updated.push("status");
        }

        if let Some(subject) = input["subject"].as_str() {
            task.subject = subject.to_string();
            updated.push("subject");
        }
        if let Some(desc) = input["description"].as_str() {
            task.description = desc.to_string();
            updated.push("description");
        }

        if let Some(add) = input["add_blocked_by"].as_array() {
            for v in add {
                if let Some(id) = v.as_str() {
                    if !task.blocked_by.contains(&id.to_string()) {
                        task.blocked_by.push(id.to_string());
                    }
                }
            }
            updated.push("blocked_by");
        }
        if let Some(remove) = input["remove_blocked_by"].as_array() {
            for v in remove {
                if let Some(id) = v.as_str() {
                    task.blocked_by.retain(|b| b != id);
                }
            }
            updated.push("blocked_by");
        }

        if updated.is_empty() {
            return Ok(ToolResult::text("No fields updated. Provide at least one field to change."));
        }

        save_task(&task)?;

        // If a task just completed, unblock downstream tasks
        if task.status == TaskStatus::Completed {
            unblock_downstream(&task.id);
        }

        Ok(ToolResult::text(format!(
            "Updated task {} — fields: [{}], status: {}",
            task.id,
            updated.join(", "),
            task.status
        )))
    }
}

/// When a task completes, check if any blocked tasks become unblocked.
fn unblock_downstream(completed_id: &str) {
    let tasks = load_all_tasks();
    for mut t in tasks {
        if t.blocked_by.contains(&completed_id.to_string()) {
            t.blocked_by.retain(|id| id != completed_id);
            if t.blocked_by.is_empty() && t.status == TaskStatus::Blocked {
                t.status = TaskStatus::Pending;
            }
            if let Err(e) = save_task(&t) {
                warn!("Failed to update downstream task {}: {}", t.id, e);
            }
        }
    }
}

// ── TaskGetTool ──────────────────────────────────────────────────────────────

pub struct TaskGetTool;

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str { "task_get" }

    fn description(&self) -> &str {
        "Get details of a specific task by ID, including subject, description, \
         status, and dependencies."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID to look up"
                }
            },
            "required": ["task_id"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let task_id = input["task_id"].as_str().unwrap_or("");
        if task_id.is_empty() {
            return Ok(ToolResult::error("task_id is required"));
        }

        match load_task(task_id) {
            Some(task) => Ok(ToolResult::text(format_task_detail(&task))),
            None => Ok(ToolResult::error(format!("Task not found: {}", task_id))),
        }
    }
}

// ── TaskListTool ─────────────────────────────────────────────────────────────

pub struct TaskListTool;

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str { "task_list" }

    fn description(&self) -> &str {
        "List all tasks with their status and dependencies. Returns a summary view \
         of all non-deleted tasks. Use to review project progress and plan next steps."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, _input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let tasks = load_all_tasks();
        if tasks.is_empty() {
            return Ok(ToolResult::text("No tasks. Use task_create to create one."));
        }
        Ok(ToolResult::text(format_task_list(&tasks)))
    }
}

// ── Formatting ───────────────────────────────────────────────────────────────

fn status_icon(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "○",
        TaskStatus::InProgress => "◉",
        TaskStatus::Completed => "✓",
        TaskStatus::Blocked => "⊘",
        TaskStatus::Deleted => "✗",
    }
}

fn format_task_detail(task: &Task) -> String {
    let mut out = format!(
        "{} [{}] {}\nID: {}\nStatus: {}\n\n{}",
        status_icon(&task.status),
        task.id,
        task.subject,
        task.id,
        task.status,
        task.description
    );

    if !task.blocked_by.is_empty() {
        out.push_str(&format!("\n\nBlocked by: {}", task.blocked_by.join(", ")));
    }
    if !task.blocks.is_empty() {
        out.push_str(&format!("\nBlocks: {}", task.blocks.join(", ")));
    }
    if let Some(owner) = &task.owner {
        out.push_str(&format!("\nOwner: {}", owner));
    }

    out
}

fn format_task_list(tasks: &[Task]) -> String {
    let mut out = String::new();

    let pending: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::Pending).collect();
    let in_progress: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::InProgress).collect();
    let blocked: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::Blocked).collect();
    let completed: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::Completed).collect();

    let total = tasks.len();
    out.push_str(&format!("Tasks: {} total ({} done, {} in progress, {} pending, {} blocked)\n\n",
        total, completed.len(), in_progress.len(), pending.len(), blocked.len()));

    for task in &in_progress {
        out.push_str(&format!("  {} {} — {}\n", status_icon(&task.status), task.id, task.subject));
    }
    for task in &pending {
        out.push_str(&format!("  {} {} — {}\n", status_icon(&task.status), task.id, task.subject));
    }
    for task in &blocked {
        let deps = task.blocked_by.join(", ");
        out.push_str(&format!("  {} {} — {} (blocked by: {})\n", status_icon(&task.status), task.id, task.subject, deps));
    }
    for task in &completed {
        out.push_str(&format!("  {} {} — {}\n", status_icon(&task.status), task.id, task.subject));
    }

    out
}
