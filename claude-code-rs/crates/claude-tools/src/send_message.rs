use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

/// SendUserMessageTool — send a message or notification to the user.
///
/// In the TS codebase this is called BriefTool / SendUserMessage.  It lets
/// the model proactively push messages (e.g. status updates, completion
/// notices, or "FYI" messages) outside the normal assistant response flow.
///
/// In CLI mode this simply prints to stderr.  In a future IDE/GUI mode it
/// could trigger a notification or chat bubble.
pub struct SendUserMessageTool;

#[async_trait]
impl Tool for SendUserMessageTool {
    fn name(&self) -> &str { "SendUserMessage" }
    fn category(&self) -> ToolCategory { ToolCategory::Agent }

    fn description(&self) -> &str {
        "Send a message to the user. Use for proactive status updates, completion notices, \
         or important information the user should see immediately. Supports markdown formatting."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to send. Supports markdown formatting."
                },
                "status": {
                    "type": "string",
                    "enum": ["normal", "proactive"],
                    "description": "Use 'proactive' when surfacing something the user hasn't asked for (e.g. task completion, blockers). Use 'normal' for direct replies."
                }
            },
            "required": ["message"]
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let message = input["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'message'"))?;

        let status = input["status"].as_str().unwrap_or("normal");

        let timestamp = chrono::Utc::now().to_rfc3339();

        // In CLI mode, print the message to stderr (separate from the response stream)
        let prefix = if status == "proactive" { "📢 " } else { "💬 " };
        eprintln!("\n\x1b[36m{}{}\x1b[0m", prefix, message);

        let result = json!({
            "message": message,
            "sentAt": timestamp,
            "delivered": true
        });

        Ok(ToolResult::text(serde_json::to_string(&result)?))
    }
}
