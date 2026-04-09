use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// `BriefTool` — primary visible output channel for the agent.
///
/// Sends a markdown-formatted message to the user with optional file
/// attachments.  The `status` field distinguishes reactive replies from
/// proactive notifications (task completion, blockers, unsolicited updates).
///
/// Mirrors the TS `BriefTool` from `tools/BriefTool/`.
pub struct BriefTool;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub path: String,
    pub size: u64,
    pub is_image: bool,
}

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "tiff", "tif",
];

fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn resolve_attachment(raw: &str, cwd: &std::path::Path) -> AttachmentMeta {
    let path = if PathBuf::from(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let is_image = is_image_path(&path);

    AttachmentMeta {
        path: path.to_string_lossy().into_owned(),
        size,
        is_image,
    }
}

#[async_trait]
impl Tool for BriefTool {
    fn name(&self) -> &'static str { "Brief" }
    fn category(&self) -> ToolCategory { ToolCategory::Agent }

    fn description(&self) -> &'static str {
        "Send a message to the user — your primary visible output channel. \
         Supports markdown formatting and optional file attachments (photos, \
         screenshots, diffs, logs, or any file the user should see)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message for the user. Supports markdown formatting."
                },
                "attachments": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional file paths (absolute or relative to cwd) to attach. Use for photos, screenshots, diffs, logs, or any file the user should see alongside your message."
                },
                "status": {
                    "type": "string",
                    "enum": ["normal", "proactive"],
                    "description": "'proactive' when surfacing something the user hasn't asked for and needs to see now — task completion while they're away, a blocker you hit, an unsolicited status update. Use 'normal' when replying to something the user just said."
                }
            },
            "required": ["message", "status"]
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let message = input["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'message'"))?;

        let status = input["status"].as_str().unwrap_or("normal");
        let sent_at = chrono::Utc::now().to_rfc3339();

        // Resolve attachments
        let attachments: Option<Vec<AttachmentMeta>> = input["attachments"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|p| resolve_attachment(p, &context.cwd))
                    .collect()
            });

        // Print to stderr (CLI mode)
        let prefix = if status == "proactive" { "📢 " } else { "💬 " };
        eprintln!("\n\x1b[36m{prefix}{message}\x1b[0m");

        if let Some(ref atts) = attachments {
            for att in atts {
                let icon = if att.is_image { "🖼" } else { "📎" };
                let size_kb = att.size / 1024;
                eprintln!("  {icon}  {} ({size_kb} KB)", att.path);
            }
        }

        let mut result = json!({
            "message": message,
            "sentAt": sent_at,
        });
        if let Some(atts) = attachments {
            result["attachments"] = serde_json::to_value(atts)?;
        }

        Ok(ToolResult::text(serde_json::to_string(&result)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
            ..ToolContext::default()
        }
    }

    #[test]
    fn is_image_path_recognizes_common_formats() {
        assert!(is_image_path(std::path::Path::new("photo.png")));
        assert!(is_image_path(std::path::Path::new("photo.JPG")));
        assert!(is_image_path(std::path::Path::new("photo.WebP")));
        assert!(!is_image_path(std::path::Path::new("doc.txt")));
        assert!(!is_image_path(std::path::Path::new("script.rs")));
    }

    #[test]
    fn resolve_attachment_relative_path() {
        let cwd = std::env::temp_dir();
        let meta = resolve_attachment("file.txt", &cwd);
        assert!(meta.path.contains("file.txt"));
        assert!(!meta.is_image);
    }

    #[test]
    fn resolve_attachment_image_detection() {
        let cwd = std::env::temp_dir();
        let meta = resolve_attachment("screenshot.png", &cwd);
        assert!(meta.is_image);
    }

    #[test]
    fn resolve_attachment_with_real_file() {
        let tmp = std::env::temp_dir().join("brief_test_file.txt");
        fs::write(&tmp, "hello").unwrap();
        let meta = resolve_attachment(&tmp.to_string_lossy(), std::path::Path::new("/"));
        assert_eq!(meta.size, 5);
        assert!(!meta.is_image);
        fs::remove_file(&tmp).ok();
    }

    #[tokio::test]
    async fn call_basic_message() {
        let tool = BriefTool;
        let input = json!({ "message": "Hello user!", "status": "normal" });
        let result = tool.call(input, &test_context()).await.unwrap();
        let text = result.to_text();
        assert!(text.contains("Hello user!"));
        assert!(text.contains("sentAt"));
    }

    #[tokio::test]
    async fn call_proactive_with_attachments() {
        let tool = BriefTool;
        let input = json!({
            "message": "Task complete!",
            "status": "proactive",
            "attachments": ["output.log", "screenshot.png"]
        });
        let result = tool.call(input, &test_context()).await.unwrap();
        let text = result.to_text();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["message"], "Task complete!");
        let atts = parsed["attachments"].as_array().unwrap();
        assert_eq!(atts.len(), 2);
        assert!(!atts[0]["isImage"].as_bool().unwrap());
        assert!(atts[1]["isImage"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn call_missing_message_fails() {
        let tool = BriefTool;
        let input = json!({ "status": "normal" });
        assert!(tool.call(input, &test_context()).await.is_err());
    }

    #[test]
    fn tool_metadata() {
        let tool = BriefTool;
        assert_eq!(tool.name(), "Brief");
        assert!(tool.is_read_only());
        assert!(tool.is_concurrency_safe());
        assert_eq!(tool.category(), ToolCategory::Agent);
    }
}
