//! /session, /undo, /export command handlers.

use crate::theme;
use clawed_agent::engine::QueryEngine;

/// Handle /session subcommands.
pub(crate) async fn handle_session_command(sub: &str, engine: &QueryEngine) {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => {
            let sessions = clawed_core::session::list_sessions();
            if sessions.is_empty() {
                println!("No saved sessions.");
            } else {
                println!("Saved sessions:");
                for s in &sessions {
                    let age = clawed_core::session::format_age(&s.updated_at);
                    println!(
                        "  \x1b[36m{:.8}\x1b[0m  {:<50} ({} msgs, {} turns, {})",
                        s.id, s.title, s.message_count, s.turn_count, age,
                    );
                }
            }
        }
        "save" => {
            match engine.save_session().await {
                Ok(()) => {
                    println!("{}✓ Session saved ({})\x1b[0m", theme::c_ok(), &engine.session_id()[..8]);
                }
                Err(e) => eprintln!("{}Failed to save session: {}\x1b[0m", theme::c_err(), e),
            }
        }
        "load" | "resume" => {
            let query = parts.get(1).copied().unwrap_or("").trim();
            if query.is_empty() {
                // Auto-resume latest session
                let sessions = clawed_core::session::list_sessions();
                if sessions.is_empty() {
                    println!("No sessions to resume. Use /session list first.");
                    return;
                }
                let latest = &sessions[0];
                match engine.restore_session(&latest.id).await {
                    Ok(title) => {
                        println!("{}✓ Resumed session: {}\x1b[0m", theme::c_ok(), title);
                        println!("  ({} messages restored)", latest.message_count);
                    }
                    Err(e) => eprintln!("{}Failed to resume: {}\x1b[0m", theme::c_err(), e),
                }
            } else {
                let sessions = clawed_core::session::list_sessions();
                // 1. Try exact ID prefix match first
                if let Some(meta) = sessions.iter().find(|s| s.id.starts_with(query)) {
                    match engine.restore_session(&meta.id).await {
                        Ok(title) => {
                            println!("{}✓ Resumed session: {}\x1b[0m", theme::c_ok(), title);
                            println!("  ({} messages restored)", meta.message_count);
                        }
                        Err(e) => eprintln!("{}Failed to resume: {}\x1b[0m", theme::c_err(), e),
                    }
                    return;
                }
                // 2. Fuzzy substring match on title, custom_title, summary, last_prompt
                let matches: Vec<_> = sessions.iter().filter(|s| {
                    session_matches_query(s, query)
                }).collect();

                match matches.len() {
                    0 => println!("No session found matching '{}'. Use /session list.", query),
                    1 => {
                        let meta = matches[0];
                        match engine.restore_session(&meta.id).await {
                            Ok(title) => {
                                println!("{}✓ Resumed session: {}\x1b[0m", theme::c_ok(), title);
                                println!("  ({} messages restored)", meta.message_count);
                            }
                            Err(e) => eprintln!("{}Failed to resume: {}\x1b[0m", theme::c_err(), e),
                        }
                    }
                    n => {
                        println!("Found {} sessions matching '{}':", n, query);
                        for s in &matches {
                            let age = clawed_core::session::format_age(&s.updated_at);
                            println!(
                                "  \x1b[36m{:.8}\x1b[0m  {:<50} ({} msgs, {})",
                                s.id, s.title, s.message_count, age,
                            );
                        }
                        println!("\nUse /session load <id> with a more specific prefix.");
                    }
                }
            }
        }
        "delete" | "rm" => {
            let id = parts.get(1).copied().unwrap_or("").trim();
            if id.is_empty() {
                println!("Usage: /session delete <id>");
                return;
            }
            let sessions = clawed_core::session::list_sessions();
            let found = sessions.iter().find(|s| s.id.starts_with(id));
            match found {
                Some(meta) => {
                    match clawed_core::session::delete_session(&meta.id) {
                        Ok(()) => println!("{}✓ Deleted session {:.8} ({})\x1b[0m", theme::c_ok(), meta.id, meta.title),
                        Err(e) => eprintln!("{}Failed to delete: {}\x1b[0m", theme::c_err(), e),
                    }
                }
                None => println!("No session found matching '{}'. Use /session list.", id),
            }
        }
        other => {
            println!("Unknown session subcommand: '{}'. Use save, list, load <id>, or delete <id>.", other);
        }
    }
}

/// Undo the last assistant turn — remove trailing assistant+user message pair.
pub(crate) async fn handle_undo(engine: &QueryEngine) {
    let mut s = engine.state().write().await;
    let len = s.messages.len();
    if len < 2 {
        println!("Nothing to undo.");
        return;
    }

    let mut removed_assistant = false;
    while let Some(last) = s.messages.last() {
        let is_assistant = matches!(last, clawed_core::message::Message::Assistant(_));
        s.messages.pop();
        if is_assistant {
            removed_assistant = true;
            break;
        }
    }

    if removed_assistant {
        if let Some(last) = s.messages.last() {
            if matches!(last, clawed_core::message::Message::User(_)) {
                s.messages.pop();
            }
        }
    }

    if removed_assistant {
        let new_len = s.messages.len();
        println!("{}✓ Undone (removed {} message(s), {} remaining)\x1b[0m", theme::c_ok(), len - new_len, new_len);
    } else {
        println!("Nothing to undo.");
    }
}

/// Export conversation to file.
pub(crate) async fn handle_export(engine: &QueryEngine, cwd: &std::path::Path, format: &str) {
    let state = engine.state().read().await;
    if state.messages.is_empty() {
        println!("No conversation to export.");
        return;
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");

    match format {
        "json" => {
            let filename = format!("claude_export_{}.json", timestamp);
            let path = cwd.join(&filename);

            // Build per-model usage stats
            let model_stats: serde_json::Value = state.model_usage.iter()
                .map(|(model, usage)| {
                    (model.clone(), serde_json::json!({
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_read_tokens": usage.cache_read_tokens,
                        "cache_creation_tokens": usage.cache_creation_tokens,
                        "api_calls": usage.api_calls,
                        "cost_usd": usage.cost_usd,
                    }))
                })
                .collect::<serde_json::Map<_, _>>()
                .into();

            let export = serde_json::json!({
                "model": state.model,
                "turn_count": state.turn_count,
                "total_input_tokens": state.total_input_tokens,
                "total_output_tokens": state.total_output_tokens,
                "total_cost_usd": state.total_cost(),
                "total_errors": state.total_errors,
                "lines_added": state.total_lines_added,
                "lines_removed": state.total_lines_removed,
                "model_usage": model_stats,
                "messages": state.messages.iter().map(|m| match m {
                    clawed_core::message::Message::User(u) => serde_json::json!({
                        "role": "user",
                        "content": u.content.iter().filter_map(|b| match b {
                            clawed_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    clawed_core::message::Message::Assistant(a) => serde_json::json!({
                        "role": "assistant",
                        "content": a.content.iter().filter_map(|b| match b {
                            clawed_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            clawed_core::message::ContentBlock::ToolUse { name, input, .. } =>
                                Some(serde_json::json!({"tool": name, "input": input})),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    clawed_core::message::Message::System(s) => serde_json::json!({
                        "role": "system",
                        "content": s.message,
                    }),
                }).collect::<Vec<_>>(),
            });
            let json = serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".into());
            match std::fs::write(&path, json) {
                Ok(_) => println!("{}✓ Exported to {}\x1b[0m", theme::c_ok(), path.display()),
                Err(e) => eprintln!("{}Export failed: {}\x1b[0m", theme::c_err(), e),
            }
        }
        _ => {
            // Default: markdown export
            let filename = format!("claude_export_{}.md", timestamp);
            let path = cwd.join(&filename);
            let mut md = String::new();
            md.push_str("# Claude Conversation Export\n\n");
            md.push_str(&format!("Model: {}\n\n", state.model));

            for msg in &state.messages {
                match msg {
                    clawed_core::message::Message::User(u) => {
                        md.push_str("## User\n\n");
                        for block in &u.content {
                            if let clawed_core::message::ContentBlock::Text { text } = block {
                                md.push_str(text);
                                md.push_str("\n\n");
                            }
                        }
                    }
                    clawed_core::message::Message::Assistant(a) => {
                        md.push_str("## Assistant\n\n");
                        for block in &a.content {
                            match block {
                                clawed_core::message::ContentBlock::Text { text } => {
                                    md.push_str(text);
                                    md.push_str("\n\n");
                                }
                                clawed_core::message::ContentBlock::ToolUse { name, .. } => {
                                    md.push_str(&format!("*Used tool: {}*\n\n", name));
                                }
                                _ => {}
                            }
                        }
                    }
                    clawed_core::message::Message::System(_) => {}
                }
                md.push_str("---\n\n");
            }

            match std::fs::write(&path, &md) {
                Ok(_) => println!("{}✓ Exported to {}\x1b[0m", theme::c_ok(), path.display()),
                Err(e) => eprintln!("{}Export failed: {}\x1b[0m", theme::c_err(), e),
            }
        }
    }
}

/// Search conversation history for a query string (case-insensitive).
pub(crate) async fn handle_search(engine: &QueryEngine, query: &str) {
    if query.is_empty() {
        println!("Usage: /search <query>  (prefix with r/ for regex, e.g. /search r/fn\\s+main)");
        return;
    }
    let state = engine.state().read().await;
    if state.messages.is_empty() {
        println!("No conversation to search.");
        return;
    }

    // Support regex: if query starts with "r/", treat the rest as a regex pattern
    let is_regex = query.starts_with("r/");
    let re = if is_regex {
        let pattern = &query[2..];
        match regex::RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(r) => Some(r),
            Err(e) => {
                println!("{}Invalid regex: {}\x1b[0m", theme::c_err(), e);
                return;
            }
        }
    } else {
        None
    };

    let query_lower = query.to_lowercase();
    let mut hits: Vec<(usize, &str, String)> = Vec::new();

    for (idx, msg) in state.messages.iter().enumerate() {
        let (role, texts): (&str, Vec<&str>) = match msg {
            clawed_core::message::Message::User(u) => (
                "user",
                u.content.iter().filter_map(|b| match b {
                    clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect(),
            ),
            clawed_core::message::Message::Assistant(a) => (
                "assistant",
                a.content.iter().filter_map(|b| match b {
                    clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect(),
            ),
            clawed_core::message::Message::System(s) => ("system", vec![s.message.as_str()]),
        };

        for text in texts {
            let found = if let Some(ref re) = re {
                re.find(text).map(|m| (m.start(), m.end()))
            } else {
                let lower = text.to_lowercase();
                lower.find(&query_lower).map(|pos| {
                    let byte_end = pos + query_lower.len();
                    (pos, byte_end)
                })
            };

            if let Some((byte_start, byte_end)) = found {
                let char_pos = text[..byte_start].chars().count();
                let match_char_len = text[byte_start..byte_end].chars().count();
                let total_chars = text.chars().count();
                let ctx = 40;
                let start_char = char_pos.saturating_sub(ctx);
                let end_char = (char_pos + match_char_len + ctx).min(total_chars);
                let snippet: String = text.chars().skip(start_char).take(end_char - start_char).collect();
                let snippet = snippet.replace('\n', " ");
                let prefix = if start_char > 0 { "…" } else { "" };
                let suffix = if end_char < total_chars { "…" } else { "" };
                hits.push((idx, role, format!("{}{}{}", prefix, snippet, suffix)));
                break;
            }
        }
    }

    let display_query = if is_regex { &query[2..] } else { query };
    if hits.is_empty() {
        println!("No matches for \"{}\".", display_query);
    } else {
        println!("\x1b[1m{} match(es) for \"{}\":\x1b[0m\n", hits.len(), display_query);
        for (idx, role, snippet) in &hits {
            let role_color = match *role {
                "user" => "\x1b[36m",
                "assistant" => "\x1b[33m",
                _ => "\x1b[2m",
            };
            println!("  #{:<3} {}[{}]\x1b[0m {}", idx + 1, role_color, role, snippet);
        }
    }
}

/// Browse conversation turns with pagination.
///
/// Shows 10 messages per page with role labels and truncated content.
pub(crate) async fn handle_history(engine: &QueryEngine, page: usize) {
    let state = engine.state().read().await;
    if state.messages.is_empty() {
        println!("No conversation history.");
        return;
    }

    let per_page = 10;
    let total = state.messages.len();
    let total_pages = total.div_ceil(per_page);
    let page = page.clamp(1, total_pages);
    let start = (page - 1) * per_page;
    let end = (start + per_page).min(total);

    println!(
        "\x1b[1mConversation History\x1b[0m — page {}/{} ({} messages total)\n",
        page, total_pages, total
    );

    for idx in start..end {
        let msg = &state.messages[idx];
        let (role, role_color, preview) = match msg {
            clawed_core::message::Message::User(u) => {
                let text = u.content.iter().find_map(|b| match b {
                    clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).unwrap_or("");
                ("user", "\x1b[36m", truncate_preview(text, 80))
            }
            clawed_core::message::Message::Assistant(a) => {
                let text_blocks: Vec<&str> = a.content.iter().filter_map(|b| match b {
                    clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect();
                let tool_count = a.content.iter().filter(|b| matches!(b, clawed_core::message::ContentBlock::ToolUse { .. })).count();
                let preview = if text_blocks.is_empty() && tool_count > 0 {
                    format!("[{} tool call(s)]", tool_count)
                } else {
                    let combined = text_blocks.join(" ");
                    let suffix = if tool_count > 0 {
                        format!(" [+{} tool(s)]", tool_count)
                    } else {
                        String::new()
                    };
                    format!("{}{}", truncate_preview(&combined, 70), suffix)
                };
                ("assistant", "\x1b[33m", preview)
            }
            clawed_core::message::Message::System(s) => {
                ("system", "\x1b[2m", truncate_preview(&s.message, 80))
            }
        };

        println!(
            "  \x1b[2m#{:<3}\x1b[0m {}[{}]\x1b[0m {}",
            idx + 1,
            role_color,
            role,
            preview,
        );
    }

    if total_pages > 1 {
        println!("\n\x1b[2mUse /history {} for next page\x1b[0m", if page < total_pages { page + 1 } else { 1 });
    }
}

/// Truncate a string to `max_chars` and add ellipsis if needed.
fn truncate_preview(text: &str, max_chars: usize) -> String {
    let clean = text.replace('\n', " ").replace('\r', "");
    let clean = clean.trim();
    if clean.chars().count() <= max_chars {
        clean.to_string()
    } else {
        let truncated: String = clean.chars().take(max_chars).collect();
        format!("{}…", truncated)
    }
}

/// Fuzzy match a query against session metadata (case-insensitive substring).
fn session_matches_query(meta: &clawed_core::session::SessionMeta, query: &str) -> bool {
    let q = query.to_lowercase();
    meta.title.to_lowercase().contains(&q)
        || meta.custom_title.as_deref().unwrap_or("").to_lowercase().contains(&q)
        || meta.summary.as_deref().unwrap_or("").to_lowercase().contains(&q)
        || meta.last_prompt.as_deref().unwrap_or("").to_lowercase().contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_preview_short() {
        assert_eq!(truncate_preview("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_preview_exact() {
        assert_eq!(truncate_preview("12345", 5), "12345");
    }

    #[test]
    fn test_truncate_preview_long() {
        let result = truncate_preview("hello world this is a long string", 10);
        assert_eq!(result, "hello worl…");
    }

    #[test]
    fn test_truncate_preview_newlines() {
        assert_eq!(truncate_preview("line1\nline2\nline3", 20), "line1 line2 line3");
    }

    #[test]
    fn test_truncate_preview_whitespace_trim() {
        assert_eq!(truncate_preview("  hello  ", 10), "hello");
    }

    fn make_meta(title: &str, custom_title: Option<&str>, summary: Option<&str>, last_prompt: Option<&str>) -> clawed_core::session::SessionMeta {
        clawed_core::session::SessionMeta {
            id: "test-id-123".into(),
            title: title.into(),
            model: "claude-sonnet".into(),
            cwd: "/tmp".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            turn_count: 5,
            message_count: 10,
            total_cost_usd: 0.01,
            git_branch: None,
            custom_title: custom_title.map(|s| s.into()),
            summary: summary.map(|s| s.into()),
            last_prompt: last_prompt.map(|s| s.into()),
        }
    }

    #[test]
    fn test_session_fuzzy_match_title() {
        let meta = make_meta("Fix authentication bug", None, None, None);
        assert!(session_matches_query(&meta, "auth"));
        assert!(session_matches_query(&meta, "AUTH")); // case-insensitive
        assert!(!session_matches_query(&meta, "deploy"));
    }

    #[test]
    fn test_session_fuzzy_match_custom_title() {
        let meta = make_meta("Some title", Some("My deploy script"), None, None);
        assert!(session_matches_query(&meta, "deploy"));
        assert!(!session_matches_query(&meta, "migration"));
    }

    #[test]
    fn test_session_fuzzy_match_summary() {
        let meta = make_meta("Title", None, Some("Implemented database migration"), None);
        assert!(session_matches_query(&meta, "migration"));
    }

    #[test]
    fn test_session_fuzzy_match_last_prompt() {
        let meta = make_meta("Title", None, None, Some("add unit tests for parser"));
        assert!(session_matches_query(&meta, "parser"));
        assert!(session_matches_query(&meta, "unit test"));
    }

    #[test]
    fn test_session_fuzzy_no_match() {
        let meta = make_meta("Title", None, None, None);
        assert!(!session_matches_query(&meta, "nonexistent"));
    }
}
