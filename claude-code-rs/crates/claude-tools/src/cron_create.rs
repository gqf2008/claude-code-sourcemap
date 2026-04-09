//! CronCreate tool — schedule a prompt to run at a future time.

use async_trait::async_trait;
use serde_json::{json, Value};

use claude_core::cron::{cron_to_human, parse_cron_expression};
use claude_core::cron_tasks::{
    add_cron_task, default_max_age_days, next_cron_run_ms, read_cron_tasks, MAX_CRON_JOBS,
};
use claude_core::tool::{Tool, ToolContext, ToolResult};

/// Schedule a prompt to run at a future time (one-shot or recurring).
pub struct CronCreateTool;

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &'static str {
        "CronCreate"
    }

    fn description(&self) -> &'static str {
        "Schedule a prompt to run at a future time — either recurring on a cron schedule, \
         or once at a specific time. Persists to .claude/scheduled_tasks.json."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cron": {
                    "type": "string",
                    "description": "Standard 5-field cron expression in local time: \"M H DoM Mon DoW\" (e.g. \"*/5 * * * *\" = every 5 minutes)."
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt to enqueue at each fire time."
                },
                "recurring": {
                    "type": "boolean",
                    "description": "true (default) = recurring until deleted or auto-expired. false = fire once then auto-delete."
                }
            },
            "required": ["cron", "prompt"],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let cron_expr = input["cron"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'cron'"))?;
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'prompt'"))?;
        let recurring = input["recurring"].as_bool().unwrap_or(true);

        // Validate cron expression
        if parse_cron_expression(cron_expr).is_none() {
            return Ok(ToolResult::error(format!(
                "Invalid cron expression '{}'. Expected 5 fields: M H DoM Mon DoW.",
                cron_expr
            )));
        }

        let now = chrono::Utc::now().timestamp_millis();
        if next_cron_run_ms(cron_expr, now).is_none() {
            return Ok(ToolResult::error(format!(
                "Cron expression '{}' does not match any calendar date in the next year.",
                cron_expr
            )));
        }

        let tasks = read_cron_tasks(&context.cwd).await;
        if tasks.len() >= MAX_CRON_JOBS {
            return Ok(ToolResult::error(format!(
                "Too many scheduled jobs (max {}). Cancel one first.",
                MAX_CRON_JOBS
            )));
        }

        let id = add_cron_task(cron_expr, prompt, recurring, &context.cwd)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to add cron task: {}", e))?;

        let human = cron_to_human(cron_expr);
        let days = default_max_age_days();

        let msg = if recurring {
            format!(
                "Scheduled recurring job {} ({}). Auto-expires after {} days. Use CronDelete to cancel sooner.",
                id, human, days
            )
        } else {
            format!(
                "Scheduled one-shot task {} ({}). It will fire once then auto-delete.",
                id, human
            )
        };

        Ok(ToolResult::text(msg))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    async fn test_create_task() {
        let dir = TempDir::new().unwrap();
        let tool = CronCreateTool;

        let result = tool
            .call(
                json!({
                    "cron": "*/5 * * * *",
                    "prompt": "check status",
                    "recurring": true
                }),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        let text = result.to_text();
        assert!(text.contains("recurring job"));
        assert!(text.contains("Every 5 minutes"));
    }

    #[tokio::test]
    async fn test_invalid_cron() {
        let dir = TempDir::new().unwrap();
        let tool = CronCreateTool;

        let result = tool
            .call(
                json!({
                    "cron": "bad cron",
                    "prompt": "test"
                }),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.to_text().contains("Invalid cron"));
    }
}
