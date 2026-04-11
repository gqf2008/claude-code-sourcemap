//! CronDelete tool — delete one or more scheduled cron jobs.

use async_trait::async_trait;
use serde_json::{json, Value};

use clawed_core::cron_tasks::{read_cron_tasks, remove_cron_tasks};
use clawed_core::tool::{Tool, ToolContext, ToolResult};

/// Delete a scheduled cron job by ID.
pub struct CronDeleteTool;

#[async_trait]
impl Tool for CronDeleteTool {
    fn name(&self) -> &'static str {
        "CronDelete"
    }

    fn description(&self) -> &'static str {
        "Delete a scheduled cron job by ID. Use CronList first to look up the ID."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The ID of the job to delete."
                }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let id = input["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'id'"))?;

        let tasks = read_cron_tasks(&context.cwd).await;
        if !tasks.iter().any(|t| t.id == id) {
            return Ok(ToolResult::error(format!(
                "No scheduled job with id '{}'. Use CronList to see all jobs.",
                id
            )));
        }

        remove_cron_tasks(&[id.to_string()], &context.cwd)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to remove task: {}", e))?;

        Ok(ToolResult::text(format!("Deleted scheduled job {}.", id)))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::cron_tasks::add_cron_task;
    use clawed_core::permissions::PermissionMode;
    use clawed_core::tool::AbortSignal;
    use tempfile::TempDir;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: dir.to_path_buf(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    #[tokio::test]
    async fn test_delete_existing() {
        let dir = TempDir::new().unwrap();
        let id = add_cron_task("*/5 * * * *", "check", true, dir.path())
            .await
            .unwrap();

        let tool = CronDeleteTool;
        let result = tool
            .call(json!({ "id": id }), &ctx(dir.path()))
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.to_text().contains("Deleted"));

        // Verify deleted
        let tasks = read_cron_tasks(dir.path()).await;
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let dir = TempDir::new().unwrap();
        let tool = CronDeleteTool;

        let result = tool
            .call(json!({ "id": "nonexistent" }), &ctx(dir.path()))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.to_text().contains("No scheduled job"));
    }
}
