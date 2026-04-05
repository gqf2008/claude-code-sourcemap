use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ── Shared data type ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,   // "pending" | "in_progress" | "completed"
    pub priority: String, // "high" | "medium" | "low"
}

fn todos_path(cwd: &std::path::Path) -> std::path::PathBuf {
    cwd.join(".claude_todos.json")
}

async fn read_todos(cwd: &std::path::Path) -> Vec<TodoItem> {
    let path = todos_path(cwd);
    if !path.exists() {
        return Vec::new();
    }
    tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn format_todos(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "No todos. Use TodoWrite to create a task plan.".into();
    }
    let mut out = format!("Todo list ({} items):\n", todos.len());
    for t in todos {
        let icon = match t.status.as_str() {
            "completed"  => "✓",
            "in_progress" => "→",
            _            => "○",
        };
        let pri = match t.priority.as_str() {
            "high"   => "❗",
            "medium" => "·",
            _        => " ",
        };
        out.push_str(&format!("  {} {} [{}] {}\n", icon, pri, t.id, t.content));
    }
    let pending  = todos.iter().filter(|t| t.status == "pending").count();
    let in_prog  = todos.iter().filter(|t| t.status == "in_progress").count();
    let done     = todos.iter().filter(|t| t.status == "completed").count();
    out.push_str(&format!("\nSummary: {} pending, {} in_progress, {} completed", pending, in_prog, done));
    out
}

// ── TodoWrite ─────────────────────────────────────────────────────────────────

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str { "TodoWrite" }

    fn description(&self) -> &str {
        "Create or update the structured task list for this session. The list is the \
         single source of truth for what needs to be done. Always call TodoRead first to \
         understand the current state before calling TodoWrite. Replace the entire list on \
         each write. Allowed statuses: pending | in_progress | completed. \
         Only one task should be in_progress at a time."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Full updated todo list (replaces the current list).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id":       { "type": "string",  "description": "Short unique ID (e.g. 'setup-db')" },
                            "content":  { "type": "string",  "description": "Task description in imperative form" },
                            "status":   { "type": "string",  "enum": ["pending", "in_progress", "completed"] },
                            "priority": { "type": "string",  "enum": ["high", "medium", "low"] }
                        },
                        "required": ["id", "content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let path = todos_path(&context.cwd);

        let new_todos: Vec<TodoItem> = match serde_json::from_value(input["todos"].clone()) {
            Ok(t)  => t,
            Err(e) => return Ok(ToolResult::error(format!("Invalid todos format: {}", e))),
        };

        // Validate: at most one in_progress at a time
        let in_progress_count = new_todos.iter().filter(|t| t.status == "in_progress").count();
        if in_progress_count > 1 {
            return Ok(ToolResult::error(
                "At most one task can be in_progress at a time. \
                 Mark only the task you are currently working on as in_progress.".to_string(),
            ));
        }

        let old_len = read_todos(&context.cwd).await.len();
        let json_str = serde_json::to_string_pretty(&new_todos)?;
        tokio::fs::write(&path, &json_str).await?;

        let pending  = new_todos.iter().filter(|t| t.status == "pending").count();
        let in_prog  = new_todos.iter().filter(|t| t.status == "in_progress").count();
        let done     = new_todos.iter().filter(|t| t.status == "completed").count();

        Ok(ToolResult::text(format!(
            "Todos updated ({} total: {} pending, {} in_progress, {} completed). \
             Previously had {} todos.\n\n{}",
            new_todos.len(), pending, in_prog, done, old_len,
            format_todos(&new_todos)
        )))
    }
}

// ── TodoRead ──────────────────────────────────────────────────────────────────

pub struct TodoReadTool;

#[async_trait]
impl Tool for TodoReadTool {
    fn name(&self) -> &str { "TodoRead" }

    fn description(&self) -> &str {
        "Read the current task list to check progress. Returns all todos with their \
         current status. Call this at the start of each turn to understand what still \
         needs to be done, and before calling TodoWrite to avoid overwriting unseen changes."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, _input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let todos = read_todos(&context.cwd).await;
        Ok(ToolResult::text(format_todos(&todos)))
    }
}

