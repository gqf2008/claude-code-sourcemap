//! Crossterm-based terminal input reader with multiline and paste support.
//!
//! Replaces rustyline for the main REPL prompt. Features:
//! - Bracketed paste: multiline paste auto-detected and inserted
//! - Shift+Enter / Alt+Enter: explicit newline insertion
//! - Multiline editing with visual `│` line prefixes
//! - History navigation (up/down arrows) with persistent storage
//! - Tab completion for slash commands and @file paths
//! - Basic editing (backspace, Ctrl+U/W/L/A/K)
//! - Slash command hint display

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal,
    execute,
};

/// Slash commands for tab completion.
pub const SLASH_COMMANDS: &[&str] = &[
    "/help", "/clear", "/model", "/compact", "/cost", "/skills", "/memory",
    "/session", "/diff", "/status", "/permissions", "/config", "/undo",
    "/review", "/doctor", "/init", "/commit", "/commit-push-pr", "/pr",
    "/bug", "/search", "/history", "/retry", "/version", "/login", "/logout",
    "/context", "/export", "/reload-context", "/mcp", "/plugin", "/exit",
];

/// Continuation prompt for multiline input.
const CONT_PROMPT: &str = "\x1b[2m│ \x1b[0m";

/// Result from reading a line of input.
pub enum InputResult {
    /// User entered text (may contain newlines from paste or Shift+Enter).
    Line(String),
    /// User pressed Ctrl+D on empty buffer (EOF).
    Eof,
    /// User pressed Ctrl+C.
    Interrupted,
}

/// Crossterm-based input reader with multiline and paste support.
pub struct InputReader {
    history: Vec<String>,
    max_history: usize,
}

impl InputReader {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            max_history: 1000,
        }
    }

    /// Add an entry to history (deduplicates consecutive entries).
    pub fn add_history(&mut self, entry: &str) {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_none_or(|last| last != trimmed) {
            self.history.push(trimmed.to_string());
            if self.history.len() > self.max_history {
                self.history.remove(0);
            }
        }
    }

    /// Load history from a file. Each line is one entry; multiline entries
    /// are stored with literal `\n` escapes.
    pub fn load_history(&mut self, path: &Path) {
        let Ok(file) = std::fs::File::open(path) else { return };
        let reader = io::BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            let entry = line.replace("\\n", "\n");
            let trimmed = entry.trim().to_string();
            if !trimmed.is_empty() {
                self.history.push(trimmed);
            }
        }
        // Keep only the last max_history entries
        if self.history.len() > self.max_history {
            let skip = self.history.len() - self.max_history;
            self.history.drain(..skip);
        }
    }

    /// Save history to a file.
    pub fn save_history(&self, path: &Path) {
        let Ok(mut file) = std::fs::File::create(path) else { return };
        for entry in &self.history {
            let escaped = entry.replace('\n', "\\n");
            let _ = writeln!(file, "{escaped}");
        }
    }

    /// Check whether this reader can be used (requires a real terminal).
    #[allow(dead_code)]
    pub fn is_available() -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    /// Read user input with paste and multiline support.
    ///
    /// - Enter submits (single or multiline)
    /// - Shift+Enter or Alt+Enter inserts a newline
    /// - Bracketed paste with newlines is inserted verbatim
    /// - Up/Down navigates history
    /// - Tab completes slash commands and @file paths
    pub fn readline(&self, prompt: &str) -> io::Result<InputResult> {
        let mut stdout = io::stdout();
        // Print prompt with green highlight
        write!(stdout, "\x1b[1;32m{prompt}\x1b[0m")?;
        stdout.flush()?;

        terminal::enable_raw_mode()?;
        let paste_ok = execute!(stdout, event::EnableBracketedPaste).is_ok();

        let result = self.read_loop(prompt);

        if paste_ok {
            let _ = execute!(io::stdout(), event::DisableBracketedPaste);
        }
        let _ = terminal::disable_raw_mode();

        result
    }

    fn read_loop(&self, prompt: &str) -> io::Result<InputResult> {
        let mut stdout = io::stdout();
        let mut lines: Vec<String> = vec![String::new()];
        let mut hist_idx = self.history.len();
        let mut saved_lines: Vec<String> = vec![String::new()];

        loop {
            let evt = event::read()?;
            match evt {
                // ── Shift+Enter or Alt+Enter: insert newline ─────────
                Event::Key(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::SHIFT)
                    || modifiers.contains(KeyModifiers::ALT) =>
                {
                    lines.push(String::new());
                    // Move to next line and show continuation prompt
                    write!(stdout, "\r\n{CONT_PROMPT}")?;
                    stdout.flush()?;
                }

                // ── Enter: submit ────────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers,
                    ..
                }) if !modifiers.contains(KeyModifiers::SHIFT)
                    && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    write!(stdout, "\r\n")?;
                    stdout.flush()?;
                    let text = lines.join("\n");
                    return Ok(InputResult::Line(text));
                }

                // ── Ctrl+C: interrupt ────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    write!(stdout, "^C\r\n")?;
                    stdout.flush()?;
                    return Ok(InputResult::Interrupted);
                }

                // ── Ctrl+D on empty: EOF ─────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL)
                    && lines.len() == 1
                    && lines[0].is_empty() =>
                {
                    write!(stdout, "\r\n")?;
                    stdout.flush()?;
                    return Ok(InputResult::Eof);
                }

                // ── Ctrl+U: clear current line ───────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('u'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(last) = lines.last_mut() {
                        last.clear();
                    }
                    redraw_buffer(&mut stdout, prompt, &lines)?;
                }

                // ── Ctrl+K: delete from cursor to end of line ────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('k'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Cursor is always at end, so Ctrl+K on last line is a no-op.
                    // But if multiline, remove all lines after current.
                    if lines.len() > 1 {
                        lines.truncate(1);
                        redraw_buffer(&mut stdout, prompt, &lines)?;
                    }
                }

                // ── Ctrl+W: delete last word ─────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('w'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(last) = lines.last_mut() {
                        let trimmed = last.trim_end().len();
                        last.truncate(trimmed);
                        if let Some(pos) = last.rfind(char::is_whitespace) {
                            last.truncate(pos + 1);
                        } else {
                            last.clear();
                        }
                    }
                    redraw_buffer(&mut stdout, prompt, &lines)?;
                }

                // ── Ctrl+L: clear screen ─────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('l'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    execute!(stdout, terminal::Clear(terminal::ClearType::All))?;
                    execute!(stdout, crossterm::cursor::MoveTo(0, 0))?;
                    redraw_buffer(&mut stdout, prompt, &lines)?;
                }

                // ── Backspace ────────────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                }) => {
                    if let Some(last) = lines.last_mut() {
                        if last.pop().is_some() {
                            // Single-line optimization: just erase one char
                            if lines.len() == 1 {
                                write!(stdout, "\x08 \x08")?;
                                stdout.flush()?;
                            } else {
                                redraw_buffer(&mut stdout, prompt, &lines)?;
                            }
                        } else if lines.len() > 1 {
                            // Empty continuation line: merge up
                            lines.pop();
                            redraw_buffer(&mut stdout, prompt, &lines)?;
                        }
                    }
                }

                // ── Character input ──────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    modifiers,
                    ..
                }) if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    if let Some(last) = lines.last_mut() {
                        last.push(c);
                    }
                    // Single-char optimization: just write the char
                    if lines.len() == 1 {
                        // If typing a slash command, show hint
                        let line = &lines[0];
                        if line.starts_with('/') {
                            // Check for a unique matching command to show as hint
                            if let Some(hint) = get_slash_hint(line) {
                                let remaining = &hint[line.len()..];
                                write!(stdout, "{c}\x1b[2m{remaining}\x1b[0m")?;
                                // Move cursor back to end of actual input
                                let back = remaining.len();
                                if back > 0 {
                                    write!(stdout, "\x1b[{back}D")?;
                                }
                            } else {
                                write!(stdout, "{c}")?;
                            }
                        } else {
                            write!(stdout, "{c}")?;
                        }
                        stdout.flush()?;
                    } else {
                        write!(stdout, "{c}")?;
                        stdout.flush()?;
                    }
                }

                // ── Tab: completion ──────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Tab, ..
                }) => {
                    if lines.len() == 1 {
                        let buf = &lines[0];
                        if buf.starts_with('/') {
                            // Slash command completion
                            let matches: Vec<&str> = SLASH_COMMANDS
                                .iter()
                                .filter(|cmd| cmd.starts_with(buf.as_str()))
                                .copied()
                                .collect();
                            if matches.len() == 1 {
                                lines[0] = format!("{} ", matches[0]);
                                redraw_buffer(&mut stdout, prompt, &lines)?;
                            } else if matches.len() > 1 {
                                write!(stdout, "\r\n")?;
                                for m in &matches {
                                    write!(stdout, "  \x1b[36m{m}\x1b[0m\r\n")?;
                                }
                                redraw_buffer(&mut stdout, prompt, &lines)?;
                            }
                        } else if let Some(at_pos) = buf.rfind('@') {
                            // @file path completion
                            let partial = &buf[at_pos + 1..];
                            if let Some(completions) = complete_file_path(partial) {
                                if completions.len() == 1 {
                                    let completed = format!("{}@{}", &buf[..at_pos], completions[0]);
                                    lines[0] = completed;
                                    redraw_buffer(&mut stdout, prompt, &lines)?;
                                } else if completions.len() > 1 {
                                    write!(stdout, "\r\n")?;
                                    for c in &completions {
                                        write!(stdout, "  \x1b[33m@{c}\x1b[0m\r\n")?;
                                    }
                                    redraw_buffer(&mut stdout, prompt, &lines)?;
                                }
                            }
                        }
                    }
                }

                // ── Up: history previous ─────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Up, ..
                }) => {
                    if hist_idx > 0 {
                        if hist_idx == self.history.len() {
                            saved_lines = lines.clone();
                        }
                        hist_idx -= 1;
                        lines = self.history[hist_idx]
                            .split('\n')
                            .map(String::from)
                            .collect();
                        redraw_buffer(&mut stdout, prompt, &lines)?;
                    }
                }

                // ── Down: history next ───────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Down, ..
                }) => {
                    if hist_idx < self.history.len() {
                        hist_idx += 1;
                        if hist_idx == self.history.len() {
                            lines = saved_lines.clone();
                        } else {
                            lines = self.history[hist_idx]
                                .split('\n')
                                .map(String::from)
                                .collect();
                        }
                        redraw_buffer(&mut stdout, prompt, &lines)?;
                    }
                }

                // ── Bracketed paste ──────────────────────────────────
                Event::Paste(text) => {
                    // Split pasted text into lines and merge with current buffer
                    let paste_lines: Vec<&str> = text.split('\n').collect();
                    if let Some(last) = lines.last_mut() {
                        last.push_str(paste_lines[0]);
                    }
                    for pl in &paste_lines[1..] {
                        lines.push(pl.to_string());
                    }
                    if paste_lines.len() > 1 {
                        let count = lines.len();
                        write!(
                            stdout,
                            "\r\n\x1b[36m[Pasted {count} lines — Enter to submit, Shift+Enter to add more]\x1b[0m\r\n"
                        )?;
                    }
                    redraw_buffer(&mut stdout, prompt, &lines)?;
                }

                // ── Escape: cancel multiline (revert to single empty line) ──
                Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => {
                    if lines.len() > 1 || !lines[0].is_empty() {
                        lines = vec![String::new()];
                        redraw_buffer(&mut stdout, prompt, &lines)?;
                    }
                }

                _ => {} // Ignore resize, mouse, focus events
            }
        }
    }
}

/// Get the default history file path (~/.claude/history).
pub fn history_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        let dir = home.join(".claude");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("history")
    })
}

/// Get the slash command hint for partial input.
fn get_slash_hint(input: &str) -> Option<&'static str> {
    let mut found: Option<&str> = None;
    for cmd in SLASH_COMMANDS {
        if cmd.starts_with(input) && *cmd != input {
            if found.is_some() {
                return None; // Multiple matches — no unique hint
            }
            found = Some(cmd);
        }
    }
    found
}

/// Complete @file paths relative to current directory.
fn complete_file_path(partial: &str) -> Option<Vec<String>> {
    let (dir, prefix) = if partial.contains('/') || partial.contains('\\') {
        let p = Path::new(partial);
        let parent = p.parent().unwrap_or(Path::new("."));
        let file_prefix = p.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        (parent.to_path_buf(), file_prefix)
    } else {
        (PathBuf::from("."), partial.to_string())
    };

    // Prevent path traversal
    let project_root = Path::new(".").canonicalize().ok()?;
    if let Ok(canonical_dir) = dir.canonicalize() {
        if !canonical_dir.starts_with(&project_root) {
            return Some(vec![]);
        }
    }

    let mut results = Vec::new();
    let prefix_lower = prefix.to_lowercase();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if !name.to_lowercase().starts_with(&prefix_lower) {
                continue;
            }
            let full = if dir == Path::new(".") {
                name.clone()
            } else {
                format!("{}/{}", dir.display(), name).replace('\\', "/")
            };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                results.push(format!("{full}/"));
            } else {
                results.push(full);
            }
        }
    }

    results.sort();
    if results.len() > 20 {
        results.truncate(20);
    }
    Some(results)
}

/// Redraw the entire multiline buffer.
fn redraw_buffer(stdout: &mut io::Stdout, prompt: &str, lines: &[String]) -> io::Result<()> {
    // Move cursor to start of first line: go up (lines.len() - 1) then to column 0
    let total = lines.len();
    if total > 1 {
        write!(stdout, "\x1b[{}A", total - 1)?;
    }
    write!(stdout, "\r")?;

    // Clear from cursor to end of screen
    write!(stdout, "\x1b[J")?;

    // Draw each line
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            // First line: colored prompt + content
            if line.starts_with('/') {
                write!(stdout, "\x1b[1;32m{prompt}\x1b[36m{line}\x1b[0m")?;
            } else {
                write!(stdout, "\x1b[1;32m{prompt}\x1b[0m{line}")?;
            }
        } else {
            // Continuation lines
            write!(stdout, "{CONT_PROMPT}{line}")?;
        }
        if i < total - 1 {
            write!(stdout, "\r\n")?;
        }
    }
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_history_dedup() {
        let mut reader = InputReader::new();
        reader.add_history("hello");
        reader.add_history("hello");
        assert_eq!(reader.history.len(), 1);
        reader.add_history("world");
        assert_eq!(reader.history.len(), 2);
    }

    #[test]
    fn test_add_history_empty() {
        let mut reader = InputReader::new();
        reader.add_history("");
        reader.add_history("   ");
        assert_eq!(reader.history.len(), 0);
    }

    #[test]
    fn test_add_history_max() {
        let mut reader = InputReader::new();
        reader.max_history = 3;
        for i in 0..5 {
            reader.add_history(&format!("cmd{i}"));
        }
        assert_eq!(reader.history.len(), 3);
        assert_eq!(reader.history[0], "cmd2");
        assert_eq!(reader.history[2], "cmd4");
    }

    #[test]
    fn test_slash_commands_present() {
        assert!(SLASH_COMMANDS.contains(&"/help"));
        assert!(SLASH_COMMANDS.contains(&"/exit"));
        assert!(SLASH_COMMANDS.contains(&"/compact"));
    }

    #[test]
    fn test_get_slash_hint_unique() {
        assert_eq!(get_slash_hint("/ver"), Some("/version"));
        assert_eq!(get_slash_hint("/doc"), Some("/doctor"));
    }

    #[test]
    fn test_get_slash_hint_ambiguous() {
        // /co matches /compact, /cost, /commit, /commit-push-pr, /config, /context
        assert_eq!(get_slash_hint("/co"), None);
    }

    #[test]
    fn test_get_slash_hint_exact() {
        assert_eq!(get_slash_hint("/help"), None); // exact match, no hint
    }

    #[test]
    fn test_history_persistence() {
        let dir = std::env::temp_dir().join("claude_test_history");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_hist");

        let mut writer = InputReader::new();
        writer.add_history("single line");
        writer.add_history("multi\nline\nentry");
        writer.save_history(&path);

        let mut reader = InputReader::new();
        reader.load_history(&path);
        assert_eq!(reader.history.len(), 2);
        assert_eq!(reader.history[0], "single line");
        assert_eq!(reader.history[1], "multi\nline\nentry");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
