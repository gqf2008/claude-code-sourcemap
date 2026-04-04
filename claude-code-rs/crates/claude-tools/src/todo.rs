use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub struct TodoWriteTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String, // "pending" | "in_progress" | "completed"
    pub priority: String, // "high" | "medium" | "low"
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str { "TodoWrite" }

    fn description(&self) -> &str {
        "Create and manage a structured task list for the current session. Use this to track \
         progress on complex multi-step tasks. The todo list is stored in .claude_todos.json \
         in the working directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The updated todo list. Replaces the current list entirely.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique identifier for the todo item"
                            },
                            "content": {
                                "type": "string",
                                "description": "Task description in imperative form (e.g. 'Fix the login bug')"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current status of the task"
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Task priority"
                            }
                        },
                        "required": ["id", "content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let todos_path = context.cwd.join(".claude_todos.json");

        // Parse input todos
        let new_todos: Vec<TodoItem> = match serde_json::from_value(input["todos"].clone()) {
            Ok(t) => t,
            Err(e) => return Ok(ToolResult::error(format!("Invalid todos format: {}", e))),
        };

        // Read old todos for comparison
        let old_todos: Vec<TodoItem> = if todos_path.exists() {
            tokio::fs::read_to_string(&todos_path)
                .await
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Validate: at most one in_progress
        let in_progress_count = new_todos.iter().filter(|t| t.status == "in_progress").count();
        if in_progress_count > 1 {
            return Ok(ToolResult::error(
                "At most one task can be in_progress at a time.".to_string(),
            ));
        }

        // Write updated todos
        let json_str = serde_json::to_string_pretty(&new_todos)?;
        tokio::fs::write(&todos_path, &json_str).await?;

        // Build a summary of changes
        let pending = new_todos.iter().filter(|t| t.status == "pending").count();
        let in_prog = new_todos.iter().filter(|t| t.status == "in_progress").count();
        let done = new_todos.iter().filter(|t| t.status == "completed").count();

        let summary = format!(
            "Todos updated ({} total: {} pending, {} in_progress, {} completed). \
             Previously had {} todos. Saved to .claude_todos.json.\n\
             Ensure you continue to use the todo list to track your progress.",
            new_todos.len(),
            pending,
            in_prog,
            done,
            old_todos.len()
        );

        Ok(ToolResult::text(summary))
    }
}
