//! Micro-compaction strategies — lightweight message trimming without calling Claude.
//!
//! Three strategies in ascending aggressiveness:
//! 1. `clear_old_tool_results` — replace stale tool results with placeholders
//! 2. `truncate_large_tool_results` — trim oversized tool output in-place
//! 3. `snip_old_messages` — remove entire message pairs from conversation start

use claude_core::message::{ContentBlock, Message, ToolResultContent};

// ── Constants ────────────────────────────────────────────────────────────────

/// Marker text that replaces cleared tool result content.
pub const TOOL_RESULT_CLEARED: &str = "[Old tool result content cleared]";

/// Maximum size (in chars) for a single tool result before truncation.
pub const MAX_TOOL_RESULT_CHARS: usize = 50_000;

/// Tools whose results are compactable (safe to clear after they've been consumed).
const COMPACTABLE_TOOLS: &[&str] = &[
    "Read", "Bash", "PowerShell", "Grep", "Glob",
    "WebSearch", "WebFetch", "Edit", "Write", "MultiEdit",
    "ListDir", "FileRead", "FileEdit", "FileWrite",
    "GlobTool", "GrepTool", "BashTool", "WebSearchTool", "WebFetchTool",
];

// ── Clear old tool results ───────────────────────────────────────────────────

/// Clear tool results from older messages, keeping the `keep_recent` most recent.
///
/// This is the Rust equivalent of TS `microcompactMessages()` time-based path.
/// Tool results from compactable tools are replaced with `TOOL_RESULT_CLEARED`.
///
/// Returns the number of tool results cleared.
pub fn clear_old_tool_results(messages: &mut [Message], keep_recent: usize) -> usize {
    // Collect all compactable tool result IDs, newest first.
    // We need to know which tool_use_id maps to which tool name.
    let mut tool_use_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for msg in messages.iter() {
        if let Message::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    tool_use_names.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // Find all compactable tool_result blocks (by index in message array)
    let mut compactable_ids: Vec<String> = Vec::new();
    for msg in messages.iter() {
        if let Message::User(u) = msg {
            for block in &u.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    if let Some(name) = tool_use_names.get(tool_use_id) {
                        if COMPACTABLE_TOOLS.iter().any(|t| t.eq_ignore_ascii_case(name)) {
                            compactable_ids.push(tool_use_id.clone());
                        }
                    }
                }
            }
        }
    }

    if compactable_ids.len() <= keep_recent {
        return 0;
    }

    // IDs to clear = all except the last `keep_recent`
    let clear_count = compactable_ids.len() - keep_recent;
    let clear_set: std::collections::HashSet<&str> = compactable_ids[..clear_count]
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut cleared = 0;
    for msg in messages.iter_mut() {
        if let Message::User(u) = msg {
            for block in u.content.iter_mut() {
                if let ContentBlock::ToolResult { tool_use_id, content, .. } = block {
                    if clear_set.contains(tool_use_id.as_str()) {
                        // Check if already cleared
                        let already_cleared = content.len() == 1
                            && matches!(&content[0], ToolResultContent::Text { text } if text == TOOL_RESULT_CLEARED);
                        if !already_cleared {
                            *content = vec![ToolResultContent::Text {
                                text: TOOL_RESULT_CLEARED.to_string(),
                            }];
                            cleared += 1;
                        }
                    }
                }
            }
        }
    }

    cleared
}

// ── Truncate large tool results ──────────────────────────────────────────────

/// Truncate individual tool results that exceed `max_chars`.
///
/// Returns the number of tool results truncated.
pub fn truncate_large_tool_results(messages: &mut [Message], max_chars: usize) -> usize {
    let mut truncated = 0;
    for msg in messages.iter_mut() {
        if let Message::User(u) = msg {
            for block in u.content.iter_mut() {
                if let ContentBlock::ToolResult { content, .. } = block {
                    for item in content.iter_mut() {
                        if let ToolResultContent::Text { text } = item {
                            if text.len() > max_chars {
                                // UTF-8 safe truncation
                                let mut end = max_chars;
                                while !text.is_char_boundary(end) && end > 0 {
                                    end -= 1;
                                }
                                let truncated_text = format!(
                                    "{}\n\n[… truncated {} chars]",
                                    &text[..end],
                                    text.len() - end,
                                );
                                *text = truncated_text;
                                truncated += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    truncated
}

// ── Snip old messages ────────────────────────────────────────────────────────

/// Snip old message pairs from the beginning of conversation history.
///
/// Removes user+assistant pairs from the front until `target_pairs` remain.
/// Inserts a boundary message at the snip point.
/// Returns the number of messages removed.
pub fn snip_old_messages(messages: &mut Vec<Message>, keep_recent_pairs: usize) -> usize {
    // Count user+assistant pairs
    let mut pair_count = 0;
    for msg in messages.iter() {
        if matches!(msg, Message::User(_)) {
            pair_count += 1;
        }
    }

    if pair_count <= keep_recent_pairs {
        return 0;
    }

    let pairs_to_remove = pair_count - keep_recent_pairs;

    // Remove messages from front: each "pair" is one user msg + one assistant msg
    let mut removed = 0;
    let mut pairs_removed = 0;
    let mut i = 0;
    while pairs_removed < pairs_to_remove && i < messages.len() {
        match &messages[i] {
            Message::User(_) => {
                messages.remove(i);
                removed += 1;
                pairs_removed += 1;
                // Also remove the following assistant message if present
                if i < messages.len() && matches!(&messages[i], Message::Assistant(_)) {
                    messages.remove(i);
                    removed += 1;
                }
            }
            Message::Assistant(_) => {
                // Orphaned assistant without a preceding user — remove it
                messages.remove(i);
                removed += 1;
            }
            Message::System(_) => {
                i += 1; // Skip system messages
            }
        }
    }

    // Insert boundary message at the start
    if removed > 0 {
        use claude_core::message::SystemMessage;
        messages.insert(0, Message::System(SystemMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            message: format!(
                "[{} earlier messages snipped to manage context size]",
                removed
            ),
        }));
    }

    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::message::{AssistantMessage, ContentBlock, ToolResultContent, UserMessage};

    fn make_tool_use_msg(id: &str, name: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
            stop_reason: None,
            usage: None,
        })
    }

    fn make_tool_result_msg(tool_use_id: &str, text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContent::Text { text: text.into() }],
                is_error: false,
            }],
        })
    }

    #[test]
    fn test_clear_old_tool_results_basic() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "file contents 1"),
            make_tool_use_msg("t2", "Bash"),
            make_tool_result_msg("t2", "command output"),
            make_tool_use_msg("t3", "Read"),
            make_tool_result_msg("t3", "file contents 3"),
        ];

        let cleared = clear_old_tool_results(&mut msgs, 1);
        assert_eq!(cleared, 2); // t1, t2 cleared; t3 kept

        // Verify t3 still has content
        if let Message::User(u) = &msgs[5] {
            if let ContentBlock::ToolResult { content, .. } = &u.content[0] {
                if let ToolResultContent::Text { text } = &content[0] {
                    assert_eq!(text, "file contents 3");
                }
            }
        }

        // Verify t1 was cleared
        if let Message::User(u) = &msgs[1] {
            if let ContentBlock::ToolResult { content, .. } = &u.content[0] {
                if let ToolResultContent::Text { text } = &content[0] {
                    assert_eq!(text, TOOL_RESULT_CLEARED);
                }
            }
        }
    }

    #[test]
    fn test_clear_old_tool_results_non_compactable_skipped() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "AgentTool"),
            make_tool_result_msg("t1", "agent result"),
            make_tool_use_msg("t2", "Read"),
            make_tool_result_msg("t2", "file content"),
        ];

        let cleared = clear_old_tool_results(&mut msgs, 0);
        assert_eq!(cleared, 1);
    }

    #[test]
    fn test_clear_old_tool_results_idempotent() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "data"),
            make_tool_use_msg("t2", "Read"),
            make_tool_result_msg("t2", "data2"),
        ];

        let c1 = clear_old_tool_results(&mut msgs, 1);
        assert_eq!(c1, 1);

        let c2 = clear_old_tool_results(&mut msgs, 1);
        assert_eq!(c2, 0);
    }

    #[test]
    fn test_clear_old_tool_results_keep_all() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "data"),
        ];

        let cleared = clear_old_tool_results(&mut msgs, 5);
        assert_eq!(cleared, 0);
    }

    #[test]
    fn test_truncate_large_tool_results() {
        let long_text = "x".repeat(100);
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", &long_text),
        ];

        let truncated = truncate_large_tool_results(&mut msgs, 50);
        assert_eq!(truncated, 1);

        if let Message::User(u) = &msgs[1] {
            if let ContentBlock::ToolResult { content, .. } = &u.content[0] {
                if let ToolResultContent::Text { text } = &content[0] {
                    assert!(text.len() < 100);
                    assert!(text.contains("truncated"));
                }
            }
        }
    }

    #[test]
    fn test_truncate_no_change_under_limit() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "short"),
        ];

        let truncated = truncate_large_tool_results(&mut msgs, 1000);
        assert_eq!(truncated, 0);
    }

    #[test]
    fn test_snip_old_messages() {
        let mut msgs: Vec<Message> = Vec::new();
        for i in 0..5 {
            msgs.push(Message::User(UserMessage {
                uuid: format!("u{i}"),
                content: vec![ContentBlock::Text { text: format!("question {i}") }],
            }));
            msgs.push(Message::Assistant(AssistantMessage {
                uuid: format!("a{i}"),
                content: vec![ContentBlock::Text { text: format!("answer {i}") }],
                stop_reason: None,
                usage: None,
            }));
        }

        assert_eq!(msgs.len(), 10);
        let removed = snip_old_messages(&mut msgs, 2);
        assert_eq!(removed, 6);

        assert!(matches!(&msgs[0], Message::System(_)));
        assert_eq!(msgs.len(), 5);
    }

    #[test]
    fn test_snip_no_change_under_limit() {
        let mut msgs = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "q".into() }],
            }),
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![ContentBlock::Text { text: "a".into() }],
                stop_reason: None,
                usage: None,
            }),
        ];

        let removed = snip_old_messages(&mut msgs, 5);
        assert_eq!(removed, 0);
        assert_eq!(msgs.len(), 2);
    }
}
