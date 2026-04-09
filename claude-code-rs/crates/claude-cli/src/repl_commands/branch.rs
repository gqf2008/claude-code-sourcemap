//! `/branch [name]` — fork the current conversation into a new session.
//!
//! Creates a copy of the current transcript with a new session ID,
//! preserving the full conversation history while allowing divergent exploration.

use claude_agent::engine::QueryEngine;
use claude_core::session;

/// Handle `/branch [name]` command.
pub(crate) async fn handle_branch(engine: &QueryEngine, name: &str) {
    let session_id = engine.session_id();
    let fork_name = if name.is_empty() { None } else { Some(name) };

    eprintln!("\x1b[2mForking session {}...\x1b[0m", &session_id[..8]);

    match session::fork_session(session_id, fork_name) {
        Ok(new_id) => {
            println!("\x1b[32m✓\x1b[0m Forked to new session: {}", &new_id[..8]);
            println!("  Original: {}", &session_id[..8]);

            // Count messages in fork
            if let Ok(entries) = session::load_transcript(&new_id) {
                let msg_count = entries.iter().filter(|e| matches!(e,
                    session::TranscriptEntry::User { .. } |
                    session::TranscriptEntry::Assistant { .. }
                )).count();
                println!("  Messages: {}", msg_count);
            }

            if let Some(n) = fork_name {
                println!("  Name:     {}", n);
            }

            println!("\n\x1b[2mTo resume this fork later: /session load {}\x1b[0m", &new_id[..8]);
        }
        Err(e) => {
            eprintln!("\x1b[31mFailed to fork session: {}\x1b[0m", e);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn branch_module_exists() {
        // This test verifies the module compiles correctly.
        // Integration tests for /branch are in commands.rs tests.
    }
}
