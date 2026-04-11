use claude_core::permissions::PermissionResponse;
use claude_core::permissions::PermissionSuggestion;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use std::io::{self, Write};

/// RAII guard: only disables raw mode if we enabled it.
struct RawModeGuard {
    should_restore: bool,
}

impl RawModeGuard {
    fn acquire() -> io::Result<Self> {
        let already = terminal::is_raw_mode_enabled()?;
        if !already {
            terminal::enable_raw_mode()?;
        }
        Ok(Self {
            should_restore: !already,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.should_restore {
            let _ = terminal::disable_raw_mode();
        }
    }
}

/// Interactive terminal permission prompt using crossterm.
/// Returns a `PermissionResponse` with the user's choice.
pub fn prompt_user(
    tool_name: &str,
    description: &str,
    suggestions: &[PermissionSuggestion],
) -> PermissionResponse {
    // If not a terminal, fall back to simple stdin
    if !io::IsTerminal::is_terminal(&io::stdin()) {
        eprint!("   Allow? [y/N]: ");
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        return match input.trim().to_lowercase().as_str() {
            "y" | "yes" => PermissionResponse::allow_once(),
            _ => PermissionResponse::deny(),
        };
    }

    // Build menu items: (label, hint)
    let mut labels: Vec<(String, String)> = Vec::new();
    labels.push(("Allow once".into(), "This invocation only".into()));
    labels.push((
        "Allow always (this session)".into(),
        "Remember until exit".into(),
    ));
    for s in suggestions {
        labels.push((s.label.clone(), "Add permission rule".into()));
    }
    labels.push(("Deny".into(), "Block this action".into()));

    let deny_idx = labels.len() - 1;

    match select_menu(
        &format!("⚠  {} wants to: {}", tool_name, description),
        &labels,
    ) {
        Ok(Some(idx)) => {
            if idx == 0 {
                PermissionResponse::allow_once()
            } else if idx == 1 {
                PermissionResponse::allow_always()
            } else if idx == deny_idx {
                PermissionResponse::deny()
            } else {
                // Suggestion selected (idx - 2)
                let suggestion_idx = idx - 2;
                if let Some(s) = suggestions.get(suggestion_idx) {
                    PermissionResponse {
                        allowed: true,
                        persist: true,
                        feedback: None,
                        selected_suggestion: Some(suggestion_idx),
                        destination: Some(s.destination),
                    }
                } else {
                    PermissionResponse::deny()
                }
            }
        }
        _ => {
            // User pressed Esc/Ctrl-C or terminal error → deny
            PermissionResponse::deny()
        }
    }
}

/// Minimal crossterm-based arrow-key select menu.
/// Returns the selected index, or None if cancelled.
fn select_menu(prompt: &str, items: &[(String, String)]) -> io::Result<Option<usize>> {
    if items.is_empty() {
        return Ok(None);
    }

    let _guard = RawModeGuard::acquire()?;
    let mut out = io::stderr();
    let mut cursor: usize = 0;
    let n = items.len();

    write!(out, "\r\n\x1b[36m◆\x1b[0m  {}\r\n", prompt)?;
    draw_items(&mut out, items, cursor)?;

    loop {
        match event::read()? {
            Event::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) => {
                if kind != KeyEventKind::Press && kind != KeyEventKind::Repeat {
                    continue;
                }
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        cursor = cursor.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if cursor + 1 < n {
                            cursor += 1;
                        }
                    }
                    KeyCode::Enter => {
                        write!(out, "\x1b[{}F\x1b[J", n)?;
                        write!(out, "   \x1b[32m✓\x1b[0m {}\r\n", items[cursor].0)?;
                        out.flush()?;
                        return Ok(Some(cursor));
                    }
                    KeyCode::Esc => {
                        write!(out, "\x1b[{}F\x1b[J", n)?;
                        write!(out, "   \x1b[2m✗ cancelled\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(None);
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        write!(out, "\x1b[{}F\x1b[J", n)?;
                        write!(out, "   \x1b[2m✗ cancelled\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(None);
                    }
                    _ => continue,
                }
                write!(out, "\x1b[{}F\x1b[J", n)?;
                draw_items(&mut out, items, cursor)?;
            }
            _ => {}
        }
    }
}

fn draw_items(out: &mut io::Stderr, items: &[(String, String)], cursor: usize) -> io::Result<()> {
    for (i, (label, hint)) in items.iter().enumerate() {
        if i == cursor {
            write!(out, "   \x1b[36m●\x1b[0m \x1b[1m{}\x1b[0m", label)?;
        } else {
            write!(out, "   \x1b[2m○\x1b[0m {}", label)?;
        }
        if !hint.is_empty() {
            write!(out, "  \x1b[2m{}\x1b[0m", hint)?;
        }
        write!(out, "\r\n")?;
    }
    out.flush()
}
