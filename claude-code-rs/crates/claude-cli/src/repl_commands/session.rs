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
            let export = serde_json::json!({
                "model": state.model,
                "turn_count": state.turn_count,
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
            md.push_str(&format!("# Claude Conversation Export\n\n"));
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
