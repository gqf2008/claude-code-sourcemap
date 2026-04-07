use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// ContextInspectTool — inspect the current conversation context.
///
/// Returns token counts, message count, tool list, and other metadata.
/// Useful for the model to understand its own context limits and state.
pub struct ContextInspectTool;

#[async_trait]
impl Tool for ContextInspectTool {
    fn name(&self) -> &str { "ContextInspect" }

    fn description(&self) -> &str {
        "Inspect the current conversation context: message count, estimated tokens, \
         available tools, and working directory. Use when you need to understand your \
         context size or debug tool availability."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, _input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let msg_count = context.messages.len();
        let cwd = context.cwd.display().to_string();
        let permission_mode = format!("{:?}", context.permission_mode);

        // Use hybrid counting: real API usage + rough estimation for tail
        let estimated_tokens = claude_core::token_estimation::token_count_with_estimation(
            &context.messages,
        );

        let result = json!({
            "messages": msg_count,
            "estimated_tokens": estimated_tokens,
            "cwd": cwd,
            "permission_mode": permission_mode,
            "aborted": context.abort_signal.is_aborted(),
        });

        Ok(ToolResult::text(serde_json::to_string_pretty(&result)?))
    }
}

/// VerifyChecksumTool — verify that file content matches expectations.
///
/// After editing a file, the model can use this tool to verify the file
/// was written correctly by checking its content against expected snippets.
pub struct VerifyTool;

#[async_trait]
impl Tool for VerifyTool {
    fn name(&self) -> &str { "Verify" }

    fn description(&self) -> &str {
        "Verify that a file contains expected content after an edit. \
         Returns whether all expected snippets are found in the file. \
         Use after FileEdit or FileWrite to confirm changes were applied correctly."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to verify."
                },
                "expected_snippets": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of text snippets that should be present in the file."
                },
                "unexpected_snippets": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of text snippets that should NOT be present in the file."
                }
            },
            "required": ["path", "expected_snippets"]
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;

        let full_path = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            context.cwd.join(path)
        };

        let content = match tokio::fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Cannot read file: {}", e))),
        };

        let expected: Vec<&str> = input["expected_snippets"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let unexpected: Vec<&str> = input["unexpected_snippets"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let mut issues = Vec::new();
        for snippet in &expected {
            if !content.contains(snippet) {
                issues.push(format!("MISSING: \"{}\"", snippet));
            }
        }
        for snippet in &unexpected {
            if content.contains(snippet) {
                issues.push(format!("UNEXPECTED: \"{}\"", snippet));
            }
        }

        if issues.is_empty() {
            Ok(ToolResult::text(format!(
                "✓ Verified: all {} expected snippets found, {} unexpected snippets absent.",
                expected.len(), unexpected.len()
            )))
        } else {
            Ok(ToolResult::error(format!(
                "Verification failed:\n{}",
                issues.join("\n")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::AbortSignal;
    use claude_core::permissions::PermissionMode;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: dir.to_path_buf(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    fn result_text(r: &ToolResult) -> String {
        match &r.content[0] {
            claude_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    // --- ContextInspectTool tests ---

    #[tokio::test]
    async fn context_inspect_returns_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = ContextInspectTool;
        let result = tool.call(json!({}), &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        let text = result_text(&result);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["messages"], 0);
        assert!(parsed["cwd"].as_str().is_some());
        assert_eq!(parsed["aborted"], false);
    }

    // --- VerifyTool tests ---

    #[tokio::test]
    async fn verify_expected_snippets_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world foo bar").unwrap();
        let tool = VerifyTool;
        let input = json!({
            "path": "test.txt",
            "expected_snippets": ["hello", "foo bar"]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        assert!(result_text(&result).contains("Verified"));
    }

    #[tokio::test]
    async fn verify_missing_snippet_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world").unwrap();
        let tool = VerifyTool;
        let input = json!({
            "path": "test.txt",
            "expected_snippets": ["missing"]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("MISSING"));
    }

    #[tokio::test]
    async fn verify_unexpected_snippet_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "should not have password").unwrap();
        let tool = VerifyTool;
        let input = json!({
            "path": "test.txt",
            "expected_snippets": ["should"],
            "unexpected_snippets": ["password"]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("UNEXPECTED"));
    }

    #[tokio::test]
    async fn verify_nonexistent_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = VerifyTool;
        let input = json!({
            "path": "nonexistent.txt",
            "expected_snippets": ["anything"]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("Cannot read file"));
    }
}
