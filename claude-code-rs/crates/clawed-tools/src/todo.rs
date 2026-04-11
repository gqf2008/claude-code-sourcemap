use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolContext, ToolResult};
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
    out.push_str(&format!("\nSummary: {pending} pending, {in_prog} in_progress, {done} completed"));
    out
}

// ── TodoWrite ─────────────────────────────────────────────────────────────────

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &'static str { "TodoWrite" }

    fn description(&self) -> &'static str {
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
            Err(e) => return Ok(ToolResult::error(format!("Invalid todos format: {e}"))),
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
    fn name(&self) -> &'static str { "TodoRead" }

    fn description(&self) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::tool::Tool;

    // ── TodoItem serde ──────────────────────────────────────────────────

    #[test]
    fn todo_item_serde_roundtrip() {
        let item = TodoItem {
            id: "setup-db".into(),
            content: "Set up database".into(),
            status: "pending".into(),
            priority: "high".into(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TodoItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "setup-db");
        assert_eq!(back.content, "Set up database");
        assert_eq!(back.status, "pending");
        assert_eq!(back.priority, "high");
    }

    // ── format_todos ────────────────────────────────────────────────────

    #[test]
    fn format_todos_empty() {
        let result = format_todos(&[]);
        assert_eq!(result, "No todos. Use TodoWrite to create a task plan.");
    }

    #[test]
    fn format_todos_single_pending() {
        let todos = vec![TodoItem {
            id: "t1".into(),
            content: "Do something".into(),
            status: "pending".into(),
            priority: "medium".into(),
        }];
        let result = format_todos(&todos);
        assert!(result.contains("○"), "pending should use ○ icon");
        assert!(result.contains("·"), "medium priority should use · icon");
        assert!(result.contains("[t1]"));
        assert!(result.contains("Do something"));
        assert!(result.contains("1 pending, 0 in_progress, 0 completed"));
    }

    #[test]
    fn format_todos_mixed_statuses() {
        let todos = vec![
            TodoItem { id: "a".into(), content: "Pending task".into(), status: "pending".into(), priority: "low".into() },
            TodoItem { id: "b".into(), content: "Active task".into(), status: "in_progress".into(), priority: "medium".into() },
            TodoItem { id: "c".into(), content: "Done task".into(), status: "completed".into(), priority: "high".into() },
        ];
        let result = format_todos(&todos);
        assert!(result.contains("○"), "pending icon");
        assert!(result.contains("→"), "in_progress icon");
        assert!(result.contains("✓"), "completed icon");
        assert!(result.contains("1 pending, 1 in_progress, 1 completed"));
        assert!(result.contains("3 items"));
    }

    #[test]
    fn format_todos_priority_icons() {
        let todos = vec![
            TodoItem { id: "h".into(), content: "High".into(), status: "pending".into(), priority: "high".into() },
            TodoItem { id: "m".into(), content: "Medium".into(), status: "pending".into(), priority: "medium".into() },
            TodoItem { id: "l".into(), content: "Low".into(), status: "pending".into(), priority: "low".into() },
        ];
        let result = format_todos(&todos);
        assert!(result.contains("❗"), "high priority should use ❗ icon");
        assert!(result.contains("·"), "medium priority should use · icon");
        // low priority is a space, just ensure the line exists
        assert!(result.contains("[l] Low"));
    }

    // ── Tool metadata ───────────────────────────────────────────────────

    #[test]
    fn todo_write_tool_name() {
        let tool = TodoWriteTool;
        assert_eq!(tool.name(), "TodoWrite");
    }

    #[test]
    fn todo_read_tool_name() {
        let tool = TodoReadTool;
        assert_eq!(tool.name(), "TodoRead");
    }

    #[test]
    fn todo_read_tool_is_read_only() {
        assert!(TodoReadTool.is_read_only());
        // TodoWriteTool should NOT be read-only (default is false)
        assert!(!TodoWriteTool.is_read_only());
    }
}

