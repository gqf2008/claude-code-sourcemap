use std::sync::Arc;
use claude_core::tool::ToolContext;
use claude_core::message::{ContentBlock, ToolResultContent};
use claude_core::permissions::PermissionBehavior;
use claude_tools::ToolRegistry;
use serde_json::Value;
use tracing::{debug, warn};
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::permissions::PermissionChecker;

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
            Some(t) => t,
            None => {
                return ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text { text: format!("Unknown tool: {}", tool_name) }],
                    is_error: true,
                };
            }
        };

        // ── PreToolUse hook ──────────────────────────────────────────────────
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
                    // Recursively call with modified input (one level only)
                    return self.execute_inner(tool_use_id, tool_name, new_input, context, tool.clone()).await;
                }
                _ => {}
            }
        }

        let result = self.execute_inner(tool_use_id, tool_name, input.clone(), context, tool.clone()).await;

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
                Some(input),
                Some(output_text),
                Some(is_err),
            );
            match self.hooks.run(HookEvent::PostToolUse, ctx).await {
                HookDecision::Block { reason } => {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = &result {
                        return ContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: vec![ToolResultContent::Text { text: format!("[PostHook override] {}", reason) }],
                            is_error: true,
                        };
                    }
                }
                _ => {}
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
                if !PermissionChecker::prompt_user(tool_name, &desc) {
                    return ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        content: vec![ToolResultContent::Text { text: "User denied permission".into() }],
                        is_error: true,
                    };
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

                // ── PostToolUseFailure hook ─────────────────────────────────
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

    pub async fn execute_many(
        &self,
        tool_uses: Vec<(String, String, Value)>,
        context: &ToolContext,
    ) -> Vec<ContentBlock> {
        let mut results = Vec::new();
        for (id, name, input) in tool_uses {
            results.push(self.execute(&id, &name, input, context).await);
        }
        results
    }
}

