//! CronList tool — list all scheduled cron jobs.

use async_trait::async_trait;
use serde_json::{json, Value};

use claude_core::cron::cron_to_human;
use claude_core::cron_tasks::read_cron_tasks;
use claude_core::tool::{Tool, ToolContext, ToolResult};

/// List scheduled cron jobs.
pub struct CronListTool;

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &'static str {
        "CronList"
    }

    fn description(&self) -> &'static str {
        "List all cron jobs scheduled via CronCreate."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn call(&self, _input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let tasks = read_cron_tasks(&context.cwd).await;

        if tasks.is_empty() {
            return Ok(ToolResult::text("No scheduled jobs."));
        }

        let mut out = format!("Scheduled jobs ({}):\n", tasks.len());
        for t in &tasks {
            let prompt_preview = if t.prompt.len() > 80 {
                format!("{}...", &t.prompt[..77])
            } else {
                t.prompt.clone()
            };
            let kind = if t.recurring { "recurring" } else { "one-shot" };
            out.push_str(&format!(
                "  [{}] {} ({}) — {}\n    prompt: {}\n",
                t.id,
                cron_to_human(&t.cron),
                kind,
                t.cron,
                prompt_preview,
            ));
        }

        Ok(ToolResult::text(out))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::cron_tasks::add_cron_task;
    use claude_core::permissions::PermissionMode;
    use claude_core::tool::AbortSignal;
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
    async fn test_list_empty() {
        let dir = TempDir::new().unwrap();
        let tool = CronListTool;

        let result = tool.call(json!({}), &ctx(dir.path())).await.unwrap();
        assert!(!result.is_error);
        assert!(result.to_text().contains("No scheduled jobs"));
    }

    #[tokio::test]
    async fn test_list_with_tasks() {
        let dir = TempDir::new().unwrap();
        add_cron_task("*/5 * * * *", "check", true, dir.path())
            .await
            .unwrap();
        add_cron_task("0 9 * * *", "morning", false, dir.path())
            .await
            .unwrap();

        let tool = CronListTool;
        let result = tool.call(json!({}), &ctx(dir.path())).await.unwrap();

        let text = result.to_text();
        assert!(text.contains("Scheduled jobs (2)"));
        assert!(text.contains("recurring"));
        assert!(text.contains("one-shot"));
    }
}
