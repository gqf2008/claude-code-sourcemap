use claude_core::permissions::PermissionResponse;

use crossterm::{
    cursor, execute,
    event::{self, Event, KeyCode, KeyModifiers},
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, stdout, Write};

use claude_core::permissions::PermissionSuggestion;

/// Interactive terminal permission prompt with arrow-key navigation.
/// Returns a `PermissionResponse` with the user's choice.
pub fn prompt_user(
    tool_name: &str,
    description: &str,
    suggestions: &[PermissionSuggestion],
) -> PermissionResponse {
    // Build options list
    let mut options: Vec<(String, PermissionResponse)> = Vec::new();
    options.push(("Allow once".into(), PermissionResponse::allow_once()));
    options.push((
        "Allow always (this session)".into(),
        PermissionResponse::allow_always(),
    ));
    for (i, s) in suggestions.iter().enumerate() {
        options.push((
            s.label.clone(),
            PermissionResponse {
                allowed: true,
                persist: true,
                feedback: None,
                selected_suggestion: Some(i),
                destination: Some(s.destination),
            },
        ));
    }
    options.push(("Deny".into(), PermissionResponse::deny()));

    // Print header
    let mut out = stdout();
    let _ = execute!(
        out,
        Print("\n"),
        SetForegroundColor(Color::Yellow),
        SetAttribute(Attribute::Bold),
        Print(format!("⚠  {} ", tool_name)),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(Color::Yellow),
        Print(format!("wants to: {}", description)),
        ResetColor,
        Print("\n\n"),
    );

    // If not a terminal, fall back to simple stdin
    if !io::IsTerminal::is_terminal(&io::stdin()) {
        let _ = execute!(out, Print("   Allow? [y/N]: "));
        let _ = out.flush();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        return match input.trim().to_lowercase().as_str() {
            "y" | "yes" => PermissionResponse::allow_once(),
            _ => PermissionResponse::deny(),
        };
    }

    // Interactive arrow-key selection
    let mut selected: usize = 0;
    let _ = terminal::enable_raw_mode();

    let result = loop {
        // Render options
        let _ = execute!(out, cursor::MoveToColumn(0));
        for (i, (label, _)) in options.iter().enumerate() {
            let _ = execute!(out, terminal::Clear(ClearType::CurrentLine));
            if i == selected {
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Cyan),
                    SetAttribute(Attribute::Bold),
                    Print(format!("  ❯ {}", label)),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                );
            } else {
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::DarkGrey),
                    Print(format!("    {}", label)),
                    ResetColor,
                );
            }
            let _ = execute!(out, Print("\n"));
        }
        let _ = execute!(
            out,
            SetForegroundColor(Color::DarkGrey),
            Print("\n  ↑↓ navigate · Enter select · n deny · y allow"),
            ResetColor,
        );
        let _ = out.flush();

        // Wait for key event
        if let Ok(Event::Key(key)) = event::read() {
            match key.code {
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    if selected < options.len().saturating_sub(1) {
                        selected += 1;
                    }
                }
                KeyCode::Enter => {
                    break options[selected].1.clone();
                }
                KeyCode::Char('y') => {
                    break PermissionResponse::allow_once();
                }
                KeyCode::Char('a') => {
                    break PermissionResponse::allow_always();
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    break PermissionResponse::deny();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break PermissionResponse::deny();
                }
                _ => {}
            }
        }

        // Move cursor up to re-render (options + hint line)
        let lines_to_clear = options.len() + 2;
        let _ = execute!(out, cursor::MoveUp(lines_to_clear as u16));
    };

    let _ = terminal::disable_raw_mode();
    // Clear the menu after selection
    let _ = execute!(out, Print("\n"));

    result
}
