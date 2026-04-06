use std::sync::Arc;
use futures::future::join_all;
use claude_core::tool::ToolContext;
use claude_core::message::{ContentBlock, ToolResultContent};
use claude_core::permissions::PermissionBehavior;
use claude_tools::ToolRegistry;
use serde_json::Value;
use tracing::{debug, warn};
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::permissions::PermissionChecker;

/// Max number of tools that may run concurrently (mirrors TS default of 10).
const MAX_TOOL_CONCURRENCY: usize = 10;

pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    permission_checker: Arc<PermissionChecker>,
    hooks: Arc<HookRegistry>,
}

impl ToolExecutor {
    pub fn new(registry: Arc<ToolRegistry>, permission_checker: Arc<PermissionChecker>) -> Self {
        Self {
            registry,
            permission_checker,
            hooks: Arc::new(HookRegistry::new()),
        }
    }

    pub fn with_hooks(
        registry: Arc<ToolRegistry>,
        permission_checker: Arc<PermissionChecker>,
        hooks: Arc<HookRegistry>,
    ) -> Self {
        Self { registry, permission_checker, hooks }
    }

    pub async fn execute(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: Value,
        context: &ToolContext,
    ) -> ContentBlock {
        let tool = match self.registry.get(tool_name) {
            Some(t) => t.clone(),
            None => {
                return ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text { text: format!("Unknown tool: {}", tool_name) }],
                    is_error: true,
                };
            }
        };

        // ── PreToolUse hook ──────────────────────────────────────────────────
        let mut actual_input = input.clone();
        if self.hooks.has_hooks(HookEvent::PreToolUse) {
            let ctx = self.hooks.tool_ctx(
                HookEvent::PreToolUse,
                tool_name,
                Some(input.clone()),
                None,
                None,
            );
            match self.hooks.run(HookEvent::PreToolUse, ctx).await {
                HookDecision::Block { reason } => {
                    return ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        content: vec![ToolResultContent::Text { text: format!("[Hook blocked] {}", reason) }],
                        is_error: true,
                    };
                }
                HookDecision::ModifyInput { new_input } => {
                    actual_input = new_input;
                }
                _ => {}
            }
        }

        // Execute with the (possibly hook-modified) input
        let result = self.execute_inner(tool_use_id, tool_name, actual_input.clone(), context, tool).await;

        // ── PostToolUse hook ─────────────────────────────────────────────────
        if self.hooks.has_hooks(HookEvent::PostToolUse) {
            let (output_text, is_err) = match &result {
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let text = content.iter().filter_map(|c| {
                        if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }
                    }).collect::<Vec<_>>().join("\n");
                    (text, *is_error)
                }
                _ => (String::new(), false),
            };
            let ctx = self.hooks.tool_ctx(
                HookEvent::PostToolUse,
                tool_name,
                Some(actual_input),
                Some(output_text),
                Some(is_err),
            );
            if let HookDecision::Block { reason } = self.hooks.run(HookEvent::PostToolUse, ctx).await {
                if let ContentBlock::ToolResult { tool_use_id, .. } = &result {
                    return ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContent::Text { text: format!("[PostHook override] {}", reason) }],
                        is_error: true,
                    };
                }
            }
        }

        result
    }

    async fn execute_inner(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: Value,
        context: &ToolContext,
        tool: claude_core::tool::DynTool,
    ) -> ContentBlock {
        // Check abort signal
        if context.abort_signal.is_aborted() {
            return ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: vec![ToolResultContent::Text { text: "Interrupted by user".into() }],
                is_error: true,
            };
        }

        // Permission check uses the (possibly hook-modified) input
        let perm = self.permission_checker.check(tool.as_ref(), &input).await;
        match perm.behavior {
            PermissionBehavior::Deny => {
                // Fire PermissionDenied hook
                if self.hooks.has_hooks(HookEvent::PermissionDenied) {
                    let ctx = self.hooks.permission_ctx(
                        HookEvent::PermissionDenied,
                        tool_name,
                        &input,
                        "denied_by_rule",
                    );
                    let _ = self.hooks.run(HookEvent::PermissionDenied, ctx).await;
                }
                return ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text {
                        text: perm.reason.unwrap_or_else(|| "Permission denied".into()),
                    }],
                    is_error: true,
                };
            }
            PermissionBehavior::Ask => {
                // Fire PermissionRequest hook
                if self.hooks.has_hooks(HookEvent::PermissionRequest) {
                    let ctx = self.hooks.permission_ctx(
                        HookEvent::PermissionRequest,
                        tool_name,
                        &input,
                        "ask",
                    );
                    let _ = self.hooks.run(HookEvent::PermissionRequest, ctx).await;
                }

                let desc = format!("{}: {}", tool_name, serde_json::to_string(&input).unwrap_or_default());
                let response = PermissionChecker::prompt_user(tool_name, &desc, &perm.suggestions);
                if !response.allowed {
                    // Fire PermissionDenied hook
                    if self.hooks.has_hooks(HookEvent::PermissionDenied) {
                        let ctx = self.hooks.permission_ctx(
                            HookEvent::PermissionDenied,
                            tool_name,
                            &input,
                            "denied_by_user",
                        );
                        let _ = self.hooks.run(HookEvent::PermissionDenied, ctx).await;
                    }
                    return ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        content: vec![ToolResultContent::Text { text: "User denied permission".into() }],
                        is_error: true,
                    };
                }
                // Apply the response (session allow, suggestion rule, etc.)
                self.permission_checker.apply_response(tool_name, &response, &perm, &context.cwd);
            }
            PermissionBehavior::Allow => {}
        }

        debug!("Executing tool: {}", tool_name);
        match tool.call(input.clone(), context).await {
            Ok(result) => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: result.content,
                is_error: result.is_error,
            },
            Err(e) => {
                warn!("Tool {} failed: {}", tool_name, e);
                let error_msg = format!("Tool error: {}", e);

                if self.hooks.has_hooks(HookEvent::PostToolUseFailure) {
                    let ctx = self.hooks.tool_failure_ctx(tool_name, Some(input), &error_msg);
                    let _ = self.hooks.run(HookEvent::PostToolUseFailure, ctx).await;
                }

                ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text { text: error_msg }],
                    is_error: true,
                }
            }
        }
    }

    /// Execute multiple tools with smart parallelism:
    /// - Read-only (concurrency-safe) tools in a batch run in parallel (up to MAX_TOOL_CONCURRENCY)
    /// - Write tools run sequentially
    /// - Batches run in order: [safe, safe] → [write] → [safe, safe] → …
    pub async fn execute_many(
        &self,
        tool_uses: Vec<(String, String, Value)>,
        context: &ToolContext,
    ) -> Vec<ContentBlock> {
        // Partition into batches of consecutive safe/unsafe tools
        let batches = partition_tool_calls(&self.registry, &tool_uses);

        let mut results: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());

        for batch in batches {
            if batch.concurrency_safe {
                // Parallel execution with concurrency cap
                let chunk_results = self.run_batch_parallel(batch.items, context).await;
                results.extend(chunk_results);
            } else {
                // Sequential execution for writes
                for (id, name, input) in batch.items {
                    results.push(self.execute(&id, &name, input, context).await);
                }
            }
        }

        results
    }

    async fn run_batch_parallel(
        &self,
        items: Vec<(String, String, Value)>,
        context: &ToolContext,
    ) -> Vec<ContentBlock> {
        // Process in chunks of MAX_TOOL_CONCURRENCY
        let mut results = Vec::with_capacity(items.len());
        for chunk in items.chunks(MAX_TOOL_CONCURRENCY) {
            let futs: Vec<_> = chunk.iter().map(|(id, name, input)| {
                self.execute(id, name, input.clone(), context)
            }).collect();
            let chunk_results = join_all(futs).await;
            results.extend(chunk_results);
        }
        results
    }
}

// ── Tool Result Pairing Validation ────────────────────────────────────────────

/// Validate that every tool_use has a corresponding tool_result in the messages.
/// Returns errors for any unpaired tool uses.
pub fn validate_tool_result_pairing(
    tool_uses: &[(String, String, Value)],
    results: &[ContentBlock],
) -> Vec<String> {
    let mut errors = Vec::new();

    for (id, name, _) in tool_uses {
        let has_result = results.iter().any(|r| {
            if let ContentBlock::ToolResult { tool_use_id, .. } = r {
                tool_use_id == id
            } else {
                false
            }
        });
        if !has_result {
            errors.push(format!("Missing tool result for {}({})", name, id));
        }
    }

    // Check for orphaned results
    for result in results {
        if let ContentBlock::ToolResult { tool_use_id, .. } = result {
            let has_use = tool_uses.iter().any(|(id, _, _)| id == tool_use_id);
            if !has_use {
                errors.push(format!("Orphaned tool result: {}", tool_use_id));
            }
        }
    }

    errors
}

struct ToolBatch {
    concurrency_safe: bool,
    items: Vec<(String, String, Value)>,
}

fn partition_tool_calls(
    registry: &ToolRegistry,
    tool_uses: &[(String, String, Value)],
) -> Vec<ToolBatch> {
    let mut batches: Vec<ToolBatch> = Vec::new();

    for (id, name, input) in tool_uses {
        let safe = registry
            .get(name)
            .map(|t| t.is_concurrency_safe())
            .unwrap_or(false);

        match batches.last_mut() {
            Some(batch) if batch.concurrency_safe == safe => {
                batch.items.push((id.clone(), name.clone(), input.clone()));
            }
            _ => {
                batches.push(ToolBatch {
                    concurrency_safe: safe,
                    items: vec![(id.clone(), name.clone(), input.clone())],
                });
            }
        }
    }

    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::message::ToolResultContent;
    use claude_core::permissions::{PermissionMode, PermissionRule};
    use serde_json::json;

    // ── validate_tool_result_pairing ──────────────────────────────────

    #[test]
    fn test_validate_tool_result_pairing_ok() {
        let uses = vec![("t1".into(), "Read".into(), json!({}))];
        let results = vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![ToolResultContent::Text { text: "ok".into() }],
            is_error: false,
        }];
        assert!(validate_tool_result_pairing(&uses, &results).is_empty());
    }

    #[test]
    fn test_validate_tool_result_pairing_missing() {
        let uses = vec![
            ("t1".into(), "Read".into(), json!({})),
            ("t2".into(), "Write".into(), json!({})),
        ];
        let results = vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![],
            is_error: false,
        }];
        let errors = validate_tool_result_pairing(&uses, &results);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("t2"));
    }

    #[test]
    fn test_validate_tool_result_pairing_orphan() {
        let uses = vec![("t1".into(), "Read".into(), json!({}))];
        let results = vec![
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![],
                is_error: false,
            },
            ContentBlock::ToolResult {
                tool_use_id: "t99".into(),
                content: vec![],
                is_error: false,
            },
        ];
        let errors = validate_tool_result_pairing(&uses, &results);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("t99"));
    }

    // ── partition_tool_calls ─────────────────────────────────────────

    #[test]
    fn test_partition_empty() {
        let registry = ToolRegistry::with_defaults();
        let result = partition_tool_calls(&registry, &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_partition_groups_by_safety() {
        let registry = ToolRegistry::with_defaults();
        // Read is concurrency-safe, Write is not
        let uses = vec![
            ("t1".into(), "Read".into(), json!({})),
            ("t2".into(), "Read".into(), json!({})),
            ("t3".into(), "Write".into(), json!({})),
            ("t4".into(), "Read".into(), json!({})),
        ];
        let batches = partition_tool_calls(&registry, &uses);
        // Should get: [safe(Read, Read), unsafe(Write), safe(Read)]
        assert!(batches.len() >= 2);
        assert!(batches[0].concurrency_safe);
        assert_eq!(batches[0].items.len(), 2);
    }

    // ── ToolExecutor with bypassed permissions ──────────────────────

    fn make_bypass_executor() -> ToolExecutor {
        let registry = Arc::new(ToolRegistry::with_defaults());
        let checker = Arc::new(PermissionChecker::new(
            PermissionMode::BypassAll,
            Vec::new(),
        ));
        ToolExecutor::new(registry.clone(), checker)
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let executor = make_bypass_executor();
        let ctx = claude_core::tool::ToolContext {
            cwd: std::path::PathBuf::from("."),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: PermissionMode::BypassAll,
            messages: Vec::new(),
        };
        let result = executor.execute("t1", "NonExistentTool", json!({}), &ctx).await;
        match result {
            ContentBlock::ToolResult { is_error, content, .. } => {
                assert!(is_error);
                let text = content.iter().find_map(|c| {
                    if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }
                }).unwrap_or("");
                assert!(text.contains("Unknown tool"));
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    fn workspace_root() -> std::path::PathBuf {
        // CARGO_MANIFEST_DIR = crates/claude-agent → parent twice = workspace root
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()
            .parent().unwrap()
            .to_path_buf()
    }

    #[tokio::test]
    async fn test_execute_read_tool() {
        let executor = make_bypass_executor();
        let cwd = workspace_root();
        let ctx = claude_core::tool::ToolContext {
            cwd: cwd.clone(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: PermissionMode::BypassAll,
            messages: Vec::new(),
        };
        let cargo_toml = cwd.join("Cargo.toml");
        let result = executor.execute(
            "t1", "Read", json!({"file_path": cargo_toml.to_string_lossy()}), &ctx
        ).await;
        match result {
            ContentBlock::ToolResult { is_error, content, .. } => {
                if is_error {
                    let text = content.iter().find_map(|c| {
                        if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }
                    }).unwrap_or("(no text)");
                    panic!("Read failed: {}", text);
                }
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[tokio::test]
    async fn test_execute_aborted() {
        let executor = make_bypass_executor();
        let abort_signal = claude_core::tool::AbortSignal::new();
        abort_signal.abort();
        let ctx = claude_core::tool::ToolContext {
            cwd: std::path::PathBuf::from("."),
            abort_signal,
            permission_mode: PermissionMode::BypassAll,
            messages: Vec::new(),
        };
        let result = executor.execute("t1", "Read", json!({"file_path": "Cargo.toml"}), &ctx).await;
        match result {
            ContentBlock::ToolResult { is_error, content, .. } => {
                assert!(is_error);
                let text = content.iter().find_map(|c| {
                    if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }
                }).unwrap_or("");
                assert!(text.contains("Interrupted"));
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[tokio::test]
    async fn test_execute_permission_denied_by_rule() {
        let registry = Arc::new(ToolRegistry::with_defaults());
        let rules = vec![PermissionRule {
            tool_name: "Read".to_string(),
            behavior: claude_core::permissions::PermissionBehavior::Deny,
            pattern: None,
        }];
        let checker = Arc::new(PermissionChecker::new(PermissionMode::Default, rules));
        let executor = ToolExecutor::new(registry, checker);
        let ctx = claude_core::tool::ToolContext {
            cwd: std::path::PathBuf::from("."),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: Vec::new(),
        };
        let result = executor.execute("t1", "Read", json!({"file_path": "Cargo.toml"}), &ctx).await;
        match result {
            ContentBlock::ToolResult { is_error, content, .. } => {
                assert!(is_error);
                let text = content.iter().find_map(|c| {
                    if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }
                }).unwrap_or("");
                assert!(text.contains("denied"));
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[tokio::test]
    async fn test_execute_plan_mode_allows_reads() {
        let registry = Arc::new(ToolRegistry::with_defaults());
        let checker = Arc::new(PermissionChecker::new(PermissionMode::Plan, Vec::new()));
        let executor = ToolExecutor::new(registry, checker);
        let cwd = workspace_root();
        let ctx = claude_core::tool::ToolContext {
            cwd: cwd.clone(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: PermissionMode::Plan,
            messages: Vec::new(),
        };
        let cargo_toml = cwd.join("Cargo.toml");
        let result = executor.execute(
            "t1", "Read", json!({"file_path": cargo_toml.to_string_lossy()}), &ctx
        ).await;
        match result {
            ContentBlock::ToolResult { is_error, content, .. } => {
                if is_error {
                    let text = content.iter().find_map(|c| {
                        if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }
                    }).unwrap_or("(no text)");
                    panic!("Plan mode read failed: {}", text);
                }
            }
            _ => panic!("Expected ToolResult"),
        }
    }
}
