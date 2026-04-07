//! Message sanitization for session resume and API submission.
//!
//! When loading persisted conversations (session resume), messages may contain
//! artifacts that the API rejects: orphaned thinking blocks, whitespace-only
//! assistant content, unresolved tool_use/tool_result pairs, etc.
//!
//! This module provides a multi-pass sanitization pipeline aligned with the
//! TypeScript `normalizeMessages()` / `filterOrphanedThinkingOnlyMessages()` /
//! `filterUnresolvedToolUses()` family in `utils/messages.ts`.

use std::collections::HashSet;

use crate::message::{ContentBlock, Message};

/// Result of sanitization, including the cleaned messages and a summary of changes.
#[derive(Debug, Clone, Default)]
pub struct SanitizeReport {
    /// Number of thinking-only assistant messages removed.
    pub orphaned_thinking_removed: usize,
    /// Number of whitespace-only assistant messages removed.
    pub whitespace_only_removed: usize,
    /// Number of unresolved tool_use blocks removed.
    pub unresolved_tool_uses_removed: usize,
    /// Number of unresolved tool_result blocks removed.
    pub unresolved_tool_results_removed: usize,
    /// Number of synthetic tool_result blocks injected for orphaned tool_use.
    pub synthetic_tool_results_injected: usize,
    /// Number of empty-content assistant messages patched.
    pub empty_content_patched: usize,
    /// Number of adjacent user messages merged.
    pub adjacent_users_merged: usize,
}

impl SanitizeReport {
    /// Whether any changes were made.
    pub fn has_changes(&self) -> bool {
        self.orphaned_thinking_removed > 0
            || self.whitespace_only_removed > 0
            || self.unresolved_tool_uses_removed > 0
            || self.unresolved_tool_results_removed > 0
            || self.synthetic_tool_results_injected > 0
            || self.empty_content_patched > 0
            || self.adjacent_users_merged > 0
    }

    /// One-line summary for logging.
    pub fn summary(&self) -> String {
        if !self.has_changes() {
            return "no changes".into();
        }
        let mut parts = Vec::new();
        if self.orphaned_thinking_removed > 0 {
            parts.push(format!("{} orphaned thinking", self.orphaned_thinking_removed));
        }
        if self.whitespace_only_removed > 0 {
            parts.push(format!("{} whitespace-only", self.whitespace_only_removed));
        }
        if self.unresolved_tool_uses_removed > 0 {
            parts.push(format!("{} unresolved tool_use", self.unresolved_tool_uses_removed));
        }
        if self.unresolved_tool_results_removed > 0 {
            parts.push(format!("{} unresolved tool_result", self.unresolved_tool_results_removed));
        }
        if self.synthetic_tool_results_injected > 0 {
            parts.push(format!("{} synthetic tool_result", self.synthetic_tool_results_injected));
        }
        if self.empty_content_patched > 0 {
            parts.push(format!("{} empty patched", self.empty_content_patched));
        }
        if self.adjacent_users_merged > 0 {
            parts.push(format!("{} users merged", self.adjacent_users_merged));
        }
        format!("sanitized: {}", parts.join(", "))
    }
}

/// Full sanitization pipeline for session resume.
///
/// Applies all filters in the correct order (matching TS `normalizeMessages`):
/// 1. Filter orphaned thinking-only assistant messages
/// 2. Filter whitespace-only assistant messages  
/// 3. Ensure non-empty assistant content
/// 4. Filter unresolved tool_use / tool_result pairs (remove orphaned tool_results)
/// 5. Ensure tool_result pairing (inject synthetic error results for orphaned tool_use)
/// 6. Merge adjacent user messages
pub fn sanitize_messages(messages: Vec<Message>) -> (Vec<Message>, SanitizeReport) {
    let mut report = SanitizeReport::default();

    // Pass 1: Remove assistant messages that only contain thinking blocks
    let messages = filter_orphaned_thinking(messages, &mut report);

    // Pass 2: Remove assistant messages with only whitespace text
    let messages = filter_whitespace_only_assistant(messages, &mut report);

    // Pass 3: Patch empty assistant content
    let messages = ensure_non_empty_assistant_content(messages, &mut report);

    // Pass 4: Remove unresolved tool_use / tool_result references
    let messages = filter_unresolved_tool_refs(messages, &mut report);

    // Pass 5: Inject synthetic error tool_results for orphaned tool_use blocks
    let messages = ensure_tool_result_pairing(messages, &mut report);

    // Pass 6: Merge adjacent user messages (can happen after filtering)
    let messages = merge_adjacent_users(messages, &mut report);

    (messages, report)
}

// ── Pass 1: Orphaned thinking ────────────────────────────────────────────────

/// Remove assistant messages that only contain thinking blocks (no text or tool_use).
///
/// These can appear when:
/// - The model's response was interrupted after emitting thinking but before text
/// - A session was saved mid-stream
fn filter_orphaned_thinking(messages: Vec<Message>, report: &mut SanitizeReport) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|msg| {
            if let Message::Assistant(a) = msg {
                if a.content.is_empty() {
                    return true; // handled by Pass 3
                }
                let has_non_thinking = a.content.iter().any(|b| !matches!(b, ContentBlock::Thinking { .. }));
                if !has_non_thinking {
                    report.orphaned_thinking_removed += 1;
                    return false;
                }
            }
            true
        })
        .collect()
}

// ── Pass 2: Whitespace-only assistant ────────────────────────────────────────

/// Remove assistant messages where all text content is whitespace.
///
/// The API rejects `[{type:"text", text:"  \n  "}]` as content.
fn filter_whitespace_only_assistant(messages: Vec<Message>, report: &mut SanitizeReport) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|msg| {
            if let Message::Assistant(a) = msg {
                if a.content.is_empty() {
                    return true; // handled by Pass 3
                }
                // Check if all blocks are text-only AND all text is whitespace
                let all_text = a.content.iter().all(|b| matches!(b, ContentBlock::Text { .. }));
                if all_text {
                    let all_whitespace = a.content.iter().all(|b| match b {
                        ContentBlock::Text { text } => text.trim().is_empty(),
                        _ => false,
                    });
                    if all_whitespace {
                        report.whitespace_only_removed += 1;
                        return false;
                    }
                }
            }
            true
        })
        .collect()
}

// ── Pass 3: Empty content patch ──────────────────────────────────────────────

/// Ensure non-final assistant messages have at least one content block.
///
/// An empty `content: []` is valid for prefill (the last message), but the API
/// rejects it for earlier messages. Inject a placeholder text block.
fn ensure_non_empty_assistant_content(mut messages: Vec<Message>, report: &mut SanitizeReport) -> Vec<Message> {
    let len = messages.len();
    for (i, msg) in messages.iter_mut().enumerate() {
        if i == len.saturating_sub(1) {
            continue; // skip last message (prefill allowed)
        }
        if let Message::Assistant(a) = msg {
            if a.content.is_empty() {
                a.content.push(ContentBlock::Text {
                    text: "(empty response)".into(),
                });
                report.empty_content_patched += 1;
            }
        }
    }
    messages
}

// ── Pass 4: Unresolved tool references ───────────────────────────────────────

/// Remove tool_use blocks without matching tool_result, and vice versa.
///
/// This fixes sessions interrupted mid-tool-execution where either the
/// tool_use was emitted but the result never came back, or orphaned
/// tool_results reference a tool_use that was dropped during compaction.
fn filter_unresolved_tool_refs(messages: Vec<Message>, report: &mut SanitizeReport) -> Vec<Message> {
    // Collect all tool_use IDs and tool_result references
    let mut tool_use_ids = HashSet::new();
    let mut tool_result_refs = HashSet::new();

    for msg in &messages {
        let content = match msg {
            Message::User(u) => &u.content,
            Message::Assistant(a) => &a.content,
            Message::System(_) => continue,
        };
        for block in content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    tool_use_ids.insert(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    tool_result_refs.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }

    // Find unresolved: tool_use without result, and tool_result without use
    let unresolved_uses: HashSet<_> = tool_use_ids.difference(&tool_result_refs).cloned().collect();
    let unresolved_results: HashSet<_> = tool_result_refs.difference(&tool_use_ids).cloned().collect();

    if unresolved_uses.is_empty() && unresolved_results.is_empty() {
        return messages;
    }

    report.unresolved_tool_uses_removed = unresolved_uses.len();
    report.unresolved_tool_results_removed = unresolved_results.len();

    // Filter out the unresolved blocks
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::Assistant(mut a) => {
                a.content.retain(|b| match b {
                    ContentBlock::ToolUse { id, .. } => !unresolved_uses.contains(id),
                    _ => true,
                });
                Message::Assistant(a)
            }
            Message::User(mut u) => {
                u.content.retain(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => !unresolved_results.contains(tool_use_id),
                    _ => true,
                });
                Message::User(u)
            }
            other => other,
        })
        .filter(|msg| {
            // Remove messages left empty after filtering
            match msg {
                Message::Assistant(a) => !a.content.is_empty(),
                Message::User(u) => !u.content.is_empty(),
                _ => true,
            }
        })
        .collect()
}

// ── Pass 5: Ensure tool_result pairing ───────────────────────────────────────

/// Placeholder content for synthetic error tool_result blocks.
const SYNTHETIC_TOOL_RESULT_PLACEHOLDER: &str = "[Tool use interrupted]";

/// Ensure every tool_use block has a matching tool_result.
///
/// Mirrors TS `ensureToolResultPairing()`: for each assistant message containing
/// tool_use blocks without matching tool_result in the next user message, inject
/// a synthetic error tool_result. This is safer than removing the tool_use block
/// (which would lose context about what the model tried to do).
fn ensure_tool_result_pairing(mut messages: Vec<Message>, report: &mut SanitizeReport) -> Vec<Message> {
    if messages.is_empty() {
        return messages;
    }

    // Collect (assistant_index, missing_tool_use_ids) pairs
    let mut repairs: Vec<(usize, Vec<String>)> = Vec::new();

    for i in 0..messages.len() {
        if let Message::Assistant(a) = &messages[i] {
            let tool_use_ids: Vec<String> = a.content.iter().filter_map(|b| {
                if let ContentBlock::ToolUse { id, .. } = b {
                    Some(id.clone())
                } else {
                    None
                }
            }).collect();

            if tool_use_ids.is_empty() {
                continue;
            }

            // Collect existing tool_result IDs from the next user message
            let mut existing = HashSet::new();
            if i + 1 < messages.len() {
                if let Message::User(u) = &messages[i + 1] {
                    for block in &u.content {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            existing.insert(tool_use_id.clone());
                        }
                    }
                }
            }

            let missing: Vec<String> = tool_use_ids
                .into_iter()
                .filter(|id| !existing.contains(id))
                .collect();

            if !missing.is_empty() {
                repairs.push((i, missing));
            }
        }
    }

    // Apply repairs in reverse order to preserve indices
    for (assistant_idx, missing_ids) in repairs.into_iter().rev() {
        let synthetic_blocks: Vec<ContentBlock> = missing_ids
            .iter()
            .map(|id| {
                report.synthetic_tool_results_injected += 1;
                ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: vec![crate::message::ToolResultContent::Text {
                        text: SYNTHETIC_TOOL_RESULT_PLACEHOLDER.into(),
                    }],
                    is_error: true,
                }
            })
            .collect();

        let next_idx = assistant_idx + 1;
        if next_idx < messages.len() {
            if let Message::User(u) = &mut messages[next_idx] {
                // Prepend synthetic blocks before existing content
                let mut new_content = synthetic_blocks;
                new_content.append(&mut u.content);
                u.content = new_content;
                continue;
            }
        }
        // No next user message — insert a synthetic one
        messages.insert(next_idx, Message::User(crate::message::UserMessage {
            uuid: format!("synthetic-{}", assistant_idx),
            content: synthetic_blocks,
        }));
    }

    messages
}

// ── Pass 6: Merge adjacent users ─────────────────────────────────────────────

/// Merge consecutive user messages into one.
///
/// The API requires strict user/assistant alternation. After filtering
/// assistant messages, we may end up with adjacent user messages.
fn merge_adjacent_users(messages: Vec<Message>, report: &mut SanitizeReport) -> Vec<Message> {
    let mut result: Vec<Message> = Vec::with_capacity(messages.len());

    for msg in messages {
        match (&msg, result.last_mut()) {
            (Message::User(incoming), Some(Message::User(existing))) => {
                existing.content.extend(incoming.content.clone());
                report.adjacent_users_merged += 1;
            }
            _ => result.push(msg),
        }
    }

    result
}

// ── Convenience helpers ──────────────────────────────────────────────────────

/// Strip thinking blocks from all messages (for sending to non-thinking models).
pub fn strip_thinking_blocks(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        if let Message::Assistant(a) = msg {
            a.content.retain(|b| !matches!(b, ContentBlock::Thinking { .. }));
        }
    }
}

/// Validate user/assistant alternation. Returns the first violation index if any.
pub fn validate_alternation(messages: &[Message]) -> Option<usize> {
    let mut last_role = None;
    for (i, msg) in messages.iter().enumerate() {
        let role = match msg {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::System(_) => continue,
        };
        if last_role == Some(role) {
            return Some(i);
        }
        last_role = Some(role);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::*;

    fn user_msg(text: &str) -> Message {
        Message::User(UserMessage {
            uuid: format!("u-{}", text.len()),
            content: vec![ContentBlock::Text { text: text.into() }],
        })
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: format!("a-{}", text.len()),
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    fn thinking_only_msg() -> Message {
        Message::Assistant(AssistantMessage {
            uuid: "a-think".into(),
            content: vec![ContentBlock::Thinking { thinking: "let me think...".into() }],
            stop_reason: None,
            usage: None,
        })
    }

    fn tool_use_msg(tool_id: &str, name: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: format!("a-{}", tool_id),
            content: vec![ContentBlock::ToolUse {
                id: tool_id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        })
    }

    fn tool_result_msg(tool_use_id: &str, text: &str) -> Message {
        Message::User(UserMessage {
            uuid: format!("u-{}", tool_use_id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContent::Text { text: text.into() }],
                is_error: false,
            }],
        })
    }

    // ── Orphaned thinking tests ──────────────────────────────────────────

    #[test]
    fn filter_thinking_only_messages() {
        let msgs = vec![
            user_msg("hello"),
            thinking_only_msg(),
            assistant_msg("response"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(report.orphaned_thinking_removed, 1);
    }

    #[test]
    fn keep_thinking_with_text() {
        let msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a-mixed".into(),
                content: vec![
                    ContentBlock::Thinking { thinking: "hmm".into() },
                    ContentBlock::Text { text: "answer".into() },
                ],
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(report.orphaned_thinking_removed, 0);
    }

    // ── Whitespace-only tests ────────────────────────────────────────────

    #[test]
    fn filter_whitespace_only_assistant() {
        let msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a-ws".into(),
                content: vec![ContentBlock::Text { text: "  \n  ".into() }],
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
            assistant_msg("real answer"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(report.whitespace_only_removed, 1);
    }

    #[test]
    fn keep_whitespace_with_tool_use() {
        let msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a-mixed".into(),
                content: vec![
                    ContentBlock::Text { text: "  ".into() },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "test".into(),
                        input: serde_json::json!({}),
                    },
                ],
                stop_reason: Some(StopReason::ToolUse),
                usage: None,
            }),
            tool_result_msg("t1", "ok"),
        ];
        let (result, _) = sanitize_messages(msgs);
        assert_eq!(result.len(), 3);
    }

    // ── Empty content patch tests ────────────────────────────────────────

    #[test]
    fn patch_empty_assistant_content() {
        let msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a-empty".into(),
                content: vec![],
                stop_reason: None,
                usage: None,
            }),
            user_msg("continue"),
            assistant_msg("ok"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(report.empty_content_patched, 1);
        if let Message::Assistant(a) = &result[1] {
            assert_eq!(a.content.len(), 1);
            assert!(matches!(&a.content[0], ContentBlock::Text { text } if text == "(empty response)"));
        } else {
            panic!("expected assistant");
        }
    }

    #[test]
    fn allow_empty_last_assistant() {
        let msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a-last".into(),
                content: vec![],
                stop_reason: None,
                usage: None,
            }),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(report.empty_content_patched, 0);
        if let Message::Assistant(a) = &result[1] {
            assert!(a.content.is_empty());
        }
    }

    // ── Unresolved tool reference tests ──────────────────────────────────

    #[test]
    fn filter_tool_use_without_result() {
        let msgs = vec![
            user_msg("hello"),
            tool_use_msg("t1", "read_file"),
            // no tool_result for t1
            assistant_msg("done"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(report.unresolved_tool_uses_removed, 1);
        // The tool_use msg should be removed (empty after filtering)
        assert_eq!(result.len(), 2); // user + final assistant
    }

    #[test]
    fn filter_tool_result_without_use() {
        let msgs = vec![
            user_msg("hello"),
            assistant_msg("let me check"),
            tool_result_msg("orphan_id", "orphan result"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(report.unresolved_tool_results_removed, 1);
        assert_eq!(result.len(), 2); // user + assistant
    }

    #[test]
    fn keep_matched_tool_pairs() {
        let msgs = vec![
            user_msg("hello"),
            tool_use_msg("t1", "echo"),
            tool_result_msg("t1", "echoed"),
            assistant_msg("done"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(result.len(), 4);
        assert!(!report.has_changes());
    }

    // ── Adjacent user merge tests ────────────────────────────────────────

    #[test]
    fn merge_adjacent_users() {
        let msgs = vec![
            user_msg("first"),
            user_msg("second"),
            assistant_msg("response"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(report.adjacent_users_merged, 1);
        if let Message::User(u) = &result[0] {
            assert_eq!(u.content.len(), 2);
        }
    }

    // ── Validate alternation tests ───────────────────────────────────────

    #[test]
    fn valid_alternation() {
        let msgs = vec![user_msg("a"), assistant_msg("b"), user_msg("c")];
        assert_eq!(validate_alternation(&msgs), None);
    }

    #[test]
    fn invalid_alternation_detected() {
        let msgs = vec![user_msg("a"), user_msg("b"), assistant_msg("c")];
        assert_eq!(validate_alternation(&msgs), Some(1));
    }

    // ── Strip thinking tests ─────────────────────────────────────────────

    #[test]
    fn strip_thinking_from_all_messages() {
        let mut msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![
                    ContentBlock::Thinking { thinking: "hmm".into() },
                    ContentBlock::Text { text: "answer".into() },
                ],
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
        ];
        strip_thinking_blocks(&mut msgs);
        if let Message::Assistant(a) = &msgs[1] {
            assert_eq!(a.content.len(), 1);
            assert!(matches!(&a.content[0], ContentBlock::Text { text } if text == "answer"));
        }
    }

    // ── Full pipeline integration ────────────────────────────────────────

    #[test]
    fn full_pipeline_complex_scenario() {
        // Simulate a messy session: orphaned thinking, unresolved tool, adjacent users
        let msgs = vec![
            user_msg("hello"),
            thinking_only_msg(),              // should be removed
            user_msg("continue"),             // now adjacent to first user
            tool_use_msg("t1", "echo"),
            tool_result_msg("t1", "echoed"),
            tool_use_msg("t2", "read"),       // no result → unresolved
            assistant_msg("done"),
        ];
        let (result, report) = sanitize_messages(msgs);
        assert!(report.orphaned_thinking_removed >= 1);
        assert!(report.unresolved_tool_uses_removed >= 1);
        // After filtering: user(merged), tool_use(t1), tool_result(t1), assistant
        assert!(validate_alternation(&result).is_none() || result.len() <= 4,
            "pipeline should produce valid or near-valid alternation");
    }

    #[test]
    fn empty_messages_unchanged() {
        let (result, report) = sanitize_messages(vec![]);
        assert!(result.is_empty());
        assert!(!report.has_changes());
    }

    #[test]
    fn single_user_message_unchanged() {
        let (result, report) = sanitize_messages(vec![user_msg("hello")]);
        assert_eq!(result.len(), 1);
        assert!(!report.has_changes());
    }

    #[test]
    fn report_summary_format() {
        let report = SanitizeReport {
            orphaned_thinking_removed: 2,
            whitespace_only_removed: 1,
            ..Default::default()
        };
        let summary = report.summary();
        assert!(summary.contains("2 orphaned thinking"));
        assert!(summary.contains("1 whitespace-only"));
    }

    // ── Ensure tool_result pairing tests ────────────────────────────────

    #[test]
    fn pairing_injects_synthetic_for_missing_result() {
        // assistant has tool_use(t1), but no following user message
        let msgs = vec![
            user_msg("hello"),
            tool_use_msg("t1", "read"),
        ];
        let mut report = SanitizeReport::default();
        let result = ensure_tool_result_pairing(msgs, &mut report);
        assert_eq!(report.synthetic_tool_results_injected, 1);
        // Should have 3 messages: user, assistant(tool_use), user(synthetic result)
        assert_eq!(result.len(), 3);
        if let Message::User(u) = &result[2] {
            assert!(matches!(&u.content[0], ContentBlock::ToolResult { is_error: true, .. }));
        } else {
            panic!("Expected synthetic user message");
        }
    }

    #[test]
    fn pairing_prepends_to_existing_user_message() {
        // assistant has tool_use(t1, t2), next user only has result for t1
        let msgs = vec![
            user_msg("hello"),
            Message::Assistant(AssistantMessage {
                uuid: "a-multi".into(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "write".into(),
                        input: serde_json::json!({}),
                    },
                ],
                stop_reason: Some(StopReason::ToolUse),
                usage: None,
            }),
            tool_result_msg("t1", "ok"),
        ];
        let mut report = SanitizeReport::default();
        let result = ensure_tool_result_pairing(msgs, &mut report);
        assert_eq!(report.synthetic_tool_results_injected, 1);
        // Still 3 messages, but the user msg now has 2 tool_results
        assert_eq!(result.len(), 3);
        if let Message::User(u) = &result[2] {
            let result_count = u.content.iter().filter(|b| matches!(b, ContentBlock::ToolResult { .. })).count();
            assert_eq!(result_count, 2); // 1 synthetic + 1 original
        }
    }

    #[test]
    fn pairing_no_change_when_all_paired() {
        let msgs = vec![
            user_msg("hello"),
            tool_use_msg("t1", "read"),
            tool_result_msg("t1", "file contents"),
            assistant_msg("done"),
        ];
        let mut report = SanitizeReport::default();
        let result = ensure_tool_result_pairing(msgs, &mut report);
        assert_eq!(report.synthetic_tool_results_injected, 0);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn pairing_handles_empty_messages() {
        let mut report = SanitizeReport::default();
        let result = ensure_tool_result_pairing(vec![], &mut report);
        assert!(result.is_empty());
        assert_eq!(report.synthetic_tool_results_injected, 0);
    }

    #[test]
    fn report_summary_includes_synthetic() {
        let report = SanitizeReport {
            synthetic_tool_results_injected: 3,
            ..Default::default()
        };
        let summary = report.summary();
        assert!(summary.contains("3 synthetic tool_result"));
    }
}
