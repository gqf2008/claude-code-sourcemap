//! /session, /undo, /export command handlers.

use claude_agent::engine::QueryEngine;

/// Handle /session subcommands.
pub(crate) async fn handle_session_command(sub: &str, engine: &QueryEngine) {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => {
            let sessions = claude_core::session::list_sessions();
            if sessions.is_empty() {
                println!("No saved sessions.");
            } else {
                println!("Saved sessions:");
                for s in &sessions {
                    let age = claude_core::session::format_age(&s.updated_at);
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
                    println!("\x1b[32m✓ Session saved ({})\x1b[0m", &engine.session_id()[..8]);
                }
                Err(e) => eprintln!("\x1b[31mFailed to save session: {}\x1b[0m", e),
            }
        }
        "load" | "resume" => {
            let id = parts.get(1).copied().unwrap_or("").trim();
            if id.is_empty() {
                // Auto-resume latest session
                let sessions = claude_core::session::list_sessions();
                if sessions.is_empty() {
                    println!("No sessions to resume. Use /session list first.");
                    return;
                }
                let latest = &sessions[0];
                match engine.restore_session(&latest.id).await {
                    Ok(title) => {
                        println!("\x1b[32m✓ Resumed session: {}\x1b[0m", title);
                        println!("  ({} messages restored)", latest.message_count);
                    }
                    Err(e) => eprintln!("\x1b[31mFailed to resume: {}\x1b[0m", e),
                }
            } else {
                // Find session by prefix match
                let sessions = claude_core::session::list_sessions();
                let found = sessions.iter().find(|s| s.id.starts_with(id));
                match found {
                    Some(meta) => {
                        match engine.restore_session(&meta.id).await {
                            Ok(title) => {
                                println!("\x1b[32m✓ Resumed session: {}\x1b[0m", title);
                                println!("  ({} messages restored)", meta.message_count);
                            }
                            Err(e) => eprintln!("\x1b[31mFailed to resume: {}\x1b[0m", e),
                        }
                    }
                    None => println!("No session found matching '{}'. Use /session list.", id),
                }
            }
        }
        "delete" | "rm" => {
            let id = parts.get(1).copied().unwrap_or("").trim();
            if id.is_empty() {
                println!("Usage: /session delete <id>");
                return;
            }
            let sessions = claude_core::session::list_sessions();
            let found = sessions.iter().find(|s| s.id.starts_with(id));
            match found {
                Some(meta) => {
                    match claude_core::session::delete_session(&meta.id) {
                        Ok(()) => println!("\x1b[32m✓ Deleted session {:.8} ({})\x1b[0m", meta.id, meta.title),
                        Err(e) => eprintln!("\x1b[31mFailed to delete: {}\x1b[0m", e),
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
        let is_assistant = matches!(last, claude_core::message::Message::Assistant(_));
        s.messages.pop();
        if is_assistant {
            removed_assistant = true;
            break;
        }
    }

    if removed_assistant {
        if let Some(last) = s.messages.last() {
            if matches!(last, claude_core::message::Message::User(_)) {
                s.messages.pop();
            }
        }
    }

    if removed_assistant {
        let new_len = s.messages.len();
        println!("\x1b[32m✓ Undone (removed {} message(s), {} remaining)\x1b[0m", len - new_len, new_len);
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
                    claude_core::message::Message::User(u) => serde_json::json!({
                        "role": "user",
                        "content": u.content.iter().filter_map(|b| match b {
                            claude_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    claude_core::message::Message::Assistant(a) => serde_json::json!({
                        "role": "assistant",
                        "content": a.content.iter().filter_map(|b| match b {
                            claude_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            claude_core::message::ContentBlock::ToolUse { name, input, .. } =>
                                Some(serde_json::json!({"tool": name, "input": input})),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    claude_core::message::Message::System(s) => serde_json::json!({
                        "role": "system",
                        "content": s.message,
                    }),
                }).collect::<Vec<_>>(),
            });
            let json = serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".into());
            match std::fs::write(&path, json) {
                Ok(_) => println!("\x1b[32m✓ Exported to {}\x1b[0m", path.display()),
                Err(e) => eprintln!("\x1b[31mExport failed: {}\x1b[0m", e),
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
                    claude_core::message::Message::User(u) => {
                        md.push_str("## User\n\n");
                        for block in &u.content {
                            if let claude_core::message::ContentBlock::Text { text } = block {
                                md.push_str(text);
                                md.push_str("\n\n");
                            }
                        }
                    }
                    claude_core::message::Message::Assistant(a) => {
                        md.push_str("## Assistant\n\n");
                        for block in &a.content {
                            match block {
                                claude_core::message::ContentBlock::Text { text } => {
                                    md.push_str(text);
                                    md.push_str("\n\n");
                                }
                                claude_core::message::ContentBlock::ToolUse { name, .. } => {
                                    md.push_str(&format!("*Used tool: {}*\n\n", name));
                                }
                                _ => {}
                            }
                        }
                    }
                    claude_core::message::Message::System(_) => {}
                }
                md.push_str("---\n\n");
            }

            match std::fs::write(&path, &md) {
                Ok(_) => println!("\x1b[32m✓ Exported to {}\x1b[0m", path.display()),
                Err(e) => eprintln!("\x1b[31mExport failed: {}\x1b[0m", e),
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
                println!("\x1b[31mInvalid regex: {}\x1b[0m", e);
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
            claude_core::message::Message::User(u) => (
                "user",
                u.content.iter().filter_map(|b| match b {
                    claude_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect(),
            ),
            claude_core::message::Message::Assistant(a) => (
                "assistant",
                a.content.iter().filter_map(|b| match b {
                    claude_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect(),
            ),
            claude_core::message::Message::System(s) => ("system", vec![s.message.as_str()]),
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
