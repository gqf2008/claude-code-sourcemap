//! Terminal UI components using crossterm.
//!
//! Provides interactive dialogs for:
//! - Permission confirmation (tool execution approval)
//! - Model selection (from known aliases)
//! - Initialization wizard (API key + defaults)
//! - Generic confirm / select helpers
//! - Spinner (via indicatif)
//!
//! All components use [`RawModeGuard`] to safely coexist with the REPL:
//! if raw mode is already enabled (e.g. during REPL input), the guard
//! is a no-op; otherwise it enables raw mode and restores on drop.

#![allow(dead_code)]

use std::io::{self, Write};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};

// ── Raw mode guard ──────────────────────────────────────────────────────────

/// RAII guard that enables raw mode only if it was not already active.
/// On drop, restores the previous state.
///
/// NOTE: intentionally duplicated in `clawed-agent/src/permissions/tui.rs`
/// to avoid adding crossterm to `clawed-core` (which is platform-agnostic).
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

// ── Select primitive ────────────────────────────────────────────────────────

/// A single item in a [`crossterm_select`] menu.
pub struct SelectItem {
    pub label: String,
    pub hint: String,
}

impl SelectItem {
    pub fn new(label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: hint.into(),
        }
    }
}

/// Arrow-key select menu written to stderr.
///
/// Returns the selected index, or `None` if the user cancelled (Esc / Ctrl-C).
pub fn crossterm_select(
    prompt: &str,
    items: &[SelectItem],
    initial: usize,
) -> io::Result<Option<usize>> {
    if items.is_empty() {
        return Ok(None);
    }

    let _guard = RawModeGuard::acquire()?;
    let mut out = io::stderr();
    let mut cursor = initial.min(items.len() - 1);
    let n = items.len();

    // Prompt line
    write!(out, "\r\n\x1b[36m◆\x1b[0m  {}\r\n", prompt)?;
    draw_select_items(&mut out, items, cursor)?;

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
                        // Clear option lines, print result
                        write!(out, "\x1b[{}F\x1b[J", n)?;
                        write!(out, "   \x1b[32m✓\x1b[0m {}\r\n", items[cursor].label)?;
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
                // Redraw
                write!(out, "\x1b[{}F\x1b[J", n)?;
                draw_select_items(&mut out, items, cursor)?;
            }
            _ => {}
        }
    }
}

fn draw_select_items(
    out: &mut io::Stderr,
    items: &[SelectItem],
    cursor: usize,
) -> io::Result<()> {
    for (i, item) in items.iter().enumerate() {
        if i == cursor {
            write!(out, "   \x1b[36m●\x1b[0m \x1b[1m{}\x1b[0m", item.label)?;
        } else {
            write!(out, "   \x1b[2m○\x1b[0m {}", item.label)?;
        }
        if !item.hint.is_empty() {
            write!(out, "  \x1b[2m{}\x1b[0m", item.hint)?;
        }
        write!(out, "\r\n")?;
    }
    out.flush()
}

// ── Input primitive ─────────────────────────────────────────────────────────

/// Text input with optional placeholder and validation.
///
/// If `masked` is true, input is displayed as `*` characters.
/// Returns `None` if the user cancelled.
pub fn crossterm_input(
    prompt: &str,
    placeholder: &str,
    masked: bool,
    validator: Option<Box<dyn Fn(&str) -> Result<(), String>>>,
) -> io::Result<Option<String>> {
    let _guard = RawModeGuard::acquire()?;
    let mut out = io::stderr();
    let mut buffer = String::new();
    let mut error_msg: Option<String> = None;

    write!(out, "\r\n\x1b[36m◆\x1b[0m  {}\r\n", prompt)?;
    draw_input_line(&mut out, &buffer, placeholder, masked, &error_msg)?;

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

                error_msg = None;

                match code {
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        write!(out, "\r\x1b[2K   \x1b[2m✗ cancelled\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(None);
                    }
                    KeyCode::Esc => {
                        write!(out, "\r\x1b[2K   \x1b[2m✗ cancelled\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(None);
                    }
                    KeyCode::Enter => {
                        if let Some(ref v) = validator {
                            if let Err(msg) = v(&buffer) {
                                error_msg = Some(msg);
                                write!(out, "\r\x1b[2K")?;
                                draw_input_line(
                                    &mut out,
                                    &buffer,
                                    placeholder,
                                    masked,
                                    &error_msg,
                                )?;
                                continue;
                            }
                        }
                        let display = if masked && !buffer.is_empty() {
                            let first_char = buffer.chars().next().unwrap().to_string();
                            let rest = buffer.chars().count().saturating_sub(1).min(16);
                            format!("{}{}", first_char, "*".repeat(rest))
                        } else {
                            buffer.clone()
                        };
                        write!(out, "\r\x1b[2K   \x1b[32m✓\x1b[0m {}\r\n", display)?;
                        out.flush()?;
                        return Ok(Some(buffer));
                    }
                    KeyCode::Backspace
                    | KeyCode::Char('h')
                        if code == KeyCode::Backspace
                            || modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        buffer.pop();
                    }
                    KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                        buffer.clear();
                    }
                    KeyCode::Char(c) => {
                        buffer.push(c);
                    }
                    _ => continue,
                }

                write!(out, "\r\x1b[2K")?;
                draw_input_line(&mut out, &buffer, placeholder, masked, &error_msg)?;
            }
            _ => {}
        }
    }
}

fn draw_input_line(
    out: &mut io::Stderr,
    buffer: &str,
    placeholder: &str,
    masked: bool,
    error: &Option<String>,
) -> io::Result<()> {
    write!(out, "   ")?;
    if buffer.is_empty() {
        write!(out, "\x1b[2m{}\x1b[0m", placeholder)?;
    } else if masked {
        write!(out, "{}", "*".repeat(buffer.chars().count()))?;
    } else {
        write!(out, "{}", buffer)?;
    }
    if let Some(err) = error {
        write!(out, "  \x1b[31m⚠ {}\x1b[0m", err)?;
    }
    out.flush()
}

// ── Confirm primitive ───────────────────────────────────────────────────────

/// Simple yes/no confirmation. Returns `None` if cancelled.
pub fn crossterm_confirm(prompt: &str) -> io::Result<Option<bool>> {
    let _guard = RawModeGuard::acquire()?;
    let mut out = io::stderr();

    write!(out, "\r\n\x1b[36m◆\x1b[0m  {} \x1b[2m(y/n)\x1b[0m ", prompt)?;
    out.flush()?;

    loop {
        match event::read()? {
            Event::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) => {
                if kind != KeyEventKind::Press {
                    continue;
                }
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        write!(out, "\x1b[32mYes\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(Some(true));
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter => {
                        write!(out, "\x1b[31mNo\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(Some(false));
                    }
                    KeyCode::Esc => {
                        write!(out, "\x1b[2mcancelled\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(None);
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        write!(out, "\x1b[2mcancelled\x1b[0m\r\n")?;
                        out.flush()?;
                        return Ok(None);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ── Decorative helpers ──────────────────────────────────────────────────────

pub fn print_intro(title: &str) -> io::Result<()> {
    let mut out = io::stderr();
    write!(out, "\r\n\x1b[36m┌\x1b[0m  {}\r\n", title)?;
    out.flush()
}

pub fn print_outro(message: &str) -> io::Result<()> {
    let mut out = io::stderr();
    write!(out, "\x1b[36m└\x1b[0m  {}\r\n\r\n", message)?;
    out.flush()
}

pub fn print_note(title: &str, body: &str) -> io::Result<()> {
    let mut out = io::stderr();
    write!(out, "\x1b[36m│\x1b[0m  \x1b[1m{}\x1b[0m\r\n", title)?;
    for line in body.lines() {
        write!(out, "\x1b[36m│\x1b[0m  {}\r\n", line)?;
    }
    out.flush()
}

// ── Spinner (indicatif) ─────────────────────────────────────────────────────

/// Animated spinner for long-running operations.
pub struct Spinner {
    bar: indicatif::ProgressBar,
}

impl Spinner {
    pub fn new(message: &str) -> Self {
        let bar = indicatif::ProgressBar::new_spinner();
        bar.set_style(
            indicatif::ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(80));
        Self { bar }
    }

    pub fn set_message(&self, message: &str) {
        self.bar.set_message(message.to_string());
    }

    pub fn stop(&self, message: &str) {
        self.bar.finish_with_message(message.to_string());
    }
}

// ── High-level API ──────────────────────────────────────────────────────────

/// Result of a permission confirmation dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionChoice {
    AllowOnce,
    AllowSession,
    AllowAlways,
    Deny,
}

/// Show a permission confirmation dialog for a tool invocation.
pub fn permission_confirm(
    tool_name: &str,
    description: &str,
    risk_level: &str,
) -> io::Result<PermissionChoice> {
    print_intro(&format!("🔒 Permission required: {}", tool_name))?;
    print_note(&format!("Risk: {}", risk_level), description)?;

    let items = vec![
        SelectItem::new("Allow once", "this invocation only"),
        SelectItem::new("Allow for session", "remember until exit"),
        SelectItem::new("Allow always", "add permanent rule"),
        SelectItem::new("Deny", "block this action"),
    ];

    let result = match crossterm_select("Allow this action?", &items, 0)? {
        Some(0) => PermissionChoice::AllowOnce,
        Some(1) => PermissionChoice::AllowSession,
        Some(2) => PermissionChoice::AllowAlways,
        _ => PermissionChoice::Deny,
    };

    print_outro(match &result {
        PermissionChoice::AllowOnce => "✓ Allowed (once)",
        PermissionChoice::AllowSession => "✓ Allowed (session)",
        PermissionChoice::AllowAlways => "✓ Allowed (always)",
        PermissionChoice::Deny => "✗ Denied",
    })?;

    Ok(result)
}

/// Model selection entry.
struct ModelOption {
    id: &'static str,
    label: &'static str,
    hint: &'static str,
}

const MODEL_OPTIONS: &[ModelOption] = &[
    ModelOption {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        hint: "Fast, balanced (default)",
    },
    ModelOption {
        id: "claude-opus-4-6",
        label: "Claude Opus 4.6",
        hint: "Most capable, slower",
    },
    ModelOption {
        id: "claude-sonnet-4-5",
        label: "Claude Sonnet 4.5",
        hint: "Extended thinking",
    },
    ModelOption {
        id: "claude-opus-4-5",
        label: "Claude Opus 4.5",
        hint: "Highest reasoning",
    },
    ModelOption {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
        hint: "Fastest, cheapest",
    },
];

/// Show a model selection dialog. Returns the selected model ID.
pub fn model_select(current: &str) -> io::Result<String> {
    let mut items: Vec<SelectItem> = MODEL_OPTIONS
        .iter()
        .map(|opt| {
            let hint = if opt.id == current {
                format!("{} ← current", opt.hint)
            } else {
                opt.hint.to_string()
            };
            SelectItem::new(opt.label, hint)
        })
        .collect();

    items.push(SelectItem::new("Custom model ID", "enter manually"));

    let chosen = crossterm_select(&format!("Select model (current: {})", current), &items, 0)?;

    match chosen {
        Some(idx) if idx < MODEL_OPTIONS.len() => Ok(MODEL_OPTIONS[idx].id.to_string()),
        Some(_) => {
            // Custom model ID
            match crossterm_input("Enter model ID:", "claude-sonnet-4-20250514", false, None)? {
                Some(id) => Ok(id),
                None => Ok(current.to_string()),
            }
        }
        None => Ok(current.to_string()),
    }
}

/// Simple yes/no confirmation.
pub fn confirm(message: &str) -> io::Result<bool> {
    Ok(crossterm_confirm(message)?.unwrap_or(false))
}

/// Multi-step initialization wizard. Returns `(api_key, model)`.
pub fn init_wizard(default_model: &str) -> io::Result<(String, String)> {
    print_intro("🚀 Claude Code Setup")?;

    let api_key = crossterm_input(
        "Anthropic API key:",
        "sk-ant-...",
        true,
        Some(Box::new(|input: &str| {
            if input.trim().is_empty() {
                Err("API key is required".to_string())
            } else {
                Ok(())
            }
        })),
    )?;

    let api_key = match api_key {
        Some(k) => k,
        None => {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Setup cancelled"));
        }
    };

    let model_items = vec![
        SelectItem::new("Claude Sonnet 4.6", "Recommended — fast & capable"),
        SelectItem::new("Claude Opus 4.6", "Most capable, higher cost"),
        SelectItem::new("Claude Haiku 4.5", "Fastest, lowest cost"),
    ];
    let model_ids = ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"];
    let default_idx = model_ids
        .iter()
        .position(|&id| id == default_model)
        .unwrap_or(0);

    let model = match crossterm_select("Default model:", &model_items, default_idx)? {
        Some(idx) => model_ids.get(idx).unwrap_or(&"claude-sonnet-4-6").to_string(),
        None => default_model.to_string(),
    };

    let _perm_items = vec![
        SelectItem::new("Default", "Ask before file writes and commands"),
        SelectItem::new("Accept edits", "Auto-allow file writes, ask for commands"),
        SelectItem::new("Bypass all", "Auto-allow everything (risky!)"),
    ];
    let _perm = crossterm_select("Permission mode:", &_perm_items, 0)?;

    print_outro(&format!(
        "✓ Setup complete! Using {} with key {}...{}",
        model,
        &api_key[..6.min(api_key.len())],
        &api_key[api_key.len().saturating_sub(4)..]
    ))?;

    Ok((api_key, model))
}

/// Show a spinner. Returns a handle — call `.stop()` when done.
pub fn spinner(message: &str) -> io::Result<Spinner> {
    Ok(Spinner::new(message))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_choice_variants() {
        assert_eq!(PermissionChoice::AllowOnce, PermissionChoice::AllowOnce);
        assert_ne!(PermissionChoice::AllowOnce, PermissionChoice::Deny);
        assert_ne!(PermissionChoice::AllowSession, PermissionChoice::AllowAlways);
    }

    #[test]
    fn model_options_list() {
        assert!(MODEL_OPTIONS.len() >= 3);
        assert_eq!(MODEL_OPTIONS[0].id, "claude-sonnet-4-6");
    }

    #[test]
    fn select_item_construction() {
        let item = SelectItem::new("Label", "Hint");
        assert_eq!(item.label, "Label");
        assert_eq!(item.hint, "Hint");
    }

    #[test]
    fn select_empty_items_returns_none() {
        // crossterm_select with empty items should return None without blocking
        let result = crossterm_select("prompt", &[], 0).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn spinner_creates_and_stops() {
        let s = Spinner::new("Loading...");
        s.set_message("Still loading...");
        s.stop("Done!");
    }
}
