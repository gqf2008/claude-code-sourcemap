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

        let perm = self.permission_checker.check(tool.as_ref(), &input).await;
        match perm.behavior {
            PermissionBehavior::Deny => {
                return ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text {
                        text: perm.reason.unwrap_or_else(|| "Permission denied".into()),
                    }],
                    is_error: true,
                };
            }
            PermissionBehavior::Ask => {
                let desc = format!("{}: {}", tool_name, serde_json::to_string(&input).unwrap_or_default());
                let (allowed, always) = PermissionChecker::prompt_user(tool_name, &desc);
                if !allowed {
                    return ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        content: vec![ToolResultContent::Text { text: "User denied permission".into() }],
                        is_error: true,
                    };
                }
                if always {
                    self.permission_checker.session_allow(tool_name);
                }
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

// ── Batch partitioning ────────────────────────────────────────────────────────

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


