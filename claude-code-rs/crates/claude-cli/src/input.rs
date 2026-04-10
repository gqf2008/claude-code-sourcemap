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
    "/fast", "/add-dir", "/summary", "/rename", "/copy", "/share", "/files",
    "/env", "/agents", "/theme", "/plan", "/think", "/break-cache", "/rewind",
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
        // Cursor position (char index) within the last line
        let mut cursor: usize = 0;

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
                    cursor = 0;
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
                    // Backslash continuation: if line ends with `\`, remove it
                    // and continue on the next line instead of submitting.
                    if let Some(last) = lines.last_mut() {
                        if last.ends_with('\\') {
                            last.pop(); // remove trailing backslash
                            lines.push(String::new());
                            cursor = 0;
                            write!(stdout, "\r\n{CONT_PROMPT}")?;
                            stdout.flush()?;
                            continue;
                        }
                    }
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
                    cursor = 0;
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Ctrl+K: delete from cursor to end of line ────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('k'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(last) = lines.last_mut() {
                        let byte_pos = last.char_indices().nth(cursor).map(|(i, _)| i).unwrap_or(last.len());
                        last.truncate(byte_pos);
                    }
                    // Also remove any continuation lines after current
                    if lines.len() > 1 {
                        lines.truncate(1);
                    }
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Ctrl+W: delete last word ─────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('w'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(last) = lines.last_mut() {
                        // Delete word before cursor
                        let chars: Vec<char> = last.chars().collect();
                        let before = &chars[..cursor];
                        // Skip trailing whitespace
                        let end = before.len();
                        let mut pos = end;
                        while pos > 0 && before[pos - 1].is_whitespace() {
                            pos -= 1;
                        }
                        // Skip word chars
                        while pos > 0 && !before[pos - 1].is_whitespace() {
                            pos -= 1;
                        }
                        // Reconstruct: [0..pos] + [cursor..]
                        let new_line: String = chars[..pos].iter().chain(chars[cursor..].iter()).collect();
                        *last = new_line;
                        cursor = pos;
                    }
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Ctrl+L: clear screen ─────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('l'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    execute!(stdout, terminal::Clear(terminal::ClearType::All))?;
                    execute!(stdout, crossterm::cursor::MoveTo(0, 0))?;
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Ctrl+R: reverse history search ───────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('r'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(found) = self.reverse_search(&mut stdout)? {
                        lines = found.split('\n').map(String::from).collect();
                        hist_idx = self.history.iter().position(|h| h == &found)
                            .unwrap_or(self.history.len());
                    }
                    cursor = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Ctrl+A: move cursor to start of line ─────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('a'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    cursor = 0;
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Home key ─────────────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Home, ..
                }) => {
                    cursor = 0;
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Ctrl+E / End: move to end of line ────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::End, ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::Char('e'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => {
                    cursor = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Alt+Left: move cursor one word left ──────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Left,
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::ALT) => {
                    if cursor > 0 {
                        let chars: Vec<char> = lines.last().map(|l| l.chars().collect()).unwrap_or_default();
                        let mut pos = cursor;
                        while pos > 0 && chars[pos - 1].is_whitespace() {
                            pos -= 1;
                        }
                        while pos > 0 && !chars[pos - 1].is_whitespace() {
                            pos -= 1;
                        }
                        cursor = pos;
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                    }
                }

                // ── Alt+Right: move cursor one word right ─────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Right,
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::ALT) => {
                    let chars: Vec<char> = lines.last().map(|l| l.chars().collect()).unwrap_or_default();
                    let len = chars.len();
                    if cursor < len {
                        let mut pos = cursor;
                        while pos < len && !chars[pos].is_whitespace() {
                            pos += 1;
                        }
                        while pos < len && chars[pos].is_whitespace() {
                            pos += 1;
                        }
                        cursor = pos;
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                    }
                }

                // ── Left arrow / Ctrl+B: move cursor left ────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Left, ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::Char('b'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => {
                    if cursor > 0 {
                        cursor -= 1;
                        write!(stdout, "\x1b[D")?;
                        stdout.flush()?;
                    }
                }

                // ── Right arrow / Ctrl+F: move cursor right ──────────
                Event::Key(KeyEvent {
                    code: KeyCode::Right, ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::Char('f'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => {
                    let line_len = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                    if cursor < line_len {
                        cursor += 1;
                        write!(stdout, "\x1b[C")?;
                        stdout.flush()?;
                    }
                }

                // ── Alt+Backspace: delete word before cursor ──────────
                Event::Key(KeyEvent {
                    code: KeyCode::Backspace,
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::ALT) => {
                    if cursor > 0 {
                        if let Some(last) = lines.last_mut() {
                            let chars: Vec<char> = last.chars().collect();
                            let mut pos = cursor;
                            while pos > 0 && chars[pos - 1].is_whitespace() {
                                pos -= 1;
                            }
                            while pos > 0 && !chars[pos - 1].is_whitespace() {
                                pos -= 1;
                            }
                            let new_line: String = chars[..pos].iter().chain(chars[cursor..].iter()).collect();
                            *last = new_line;
                            cursor = pos;
                        }
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                    }
                }

                // ── Alt+D: delete word after cursor ───────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::ALT) => {
                    if let Some(last) = lines.last_mut() {
                        let chars: Vec<char> = last.chars().collect();
                        let len = chars.len();
                        if cursor < len {
                            let mut pos = cursor;
                            while pos < len && !chars[pos].is_whitespace() {
                                pos += 1;
                            }
                            while pos < len && chars[pos].is_whitespace() {
                                pos += 1;
                            }
                            let new_line: String = chars[..cursor].iter().chain(chars[pos..].iter()).collect();
                            *last = new_line;
                        }
                    }
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Delete key: delete char at cursor ────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Delete, ..
                }) => {
                    if let Some(last) = lines.last_mut() {
                        let line_len = last.chars().count();
                        if cursor < line_len {
                            str_remove_char(last, cursor);
                            redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                        }
                    }
                }

                // ── Backspace ────────────────────────────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                }) => {
                    let last_len = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                    if cursor > 0 && last_len > 0 {
                        if let Some(last) = lines.last_mut() {
                            str_remove_char(last, cursor - 1);
                        }
                        cursor -= 1;
                        let new_len = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                        if lines.len() == 1 && cursor == new_len {
                            // At end of single line: simple erase
                            write!(stdout, "\x08 \x08")?;
                            stdout.flush()?;
                        } else {
                            redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                        }
                    } else if cursor == 0 && lines.len() > 1 {
                        // At start of continuation line: merge up
                        lines.pop();
                        cursor = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
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
                        str_insert_char(last, cursor, c);
                        cursor += 1;
                    }
                    let line_len = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                    let at_end = cursor == line_len;
                    // Single-char optimization: just write the char if at end of single line
                    if lines.len() == 1 && at_end {
                        let line = &lines[0];
                        if line.starts_with('/') {
                            if let Some(hint) = get_slash_hint(line) {
                                let remaining = &hint[line.len()..];
                                write!(stdout, "{c}\x1b[2m{remaining}\x1b[0m")?;
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
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
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
                                cursor = lines[0].chars().count();
                                redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                            } else if matches.len() > 1 {
                                write!(stdout, "\r\n")?;
                                for m in &matches {
                                    write!(stdout, "  \x1b[36m{m}\x1b[0m\r\n")?;
                                }
                                redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                            }
                        } else if let Some(at_pos) = buf.rfind('@') {
                            // @file path completion
                            let partial = &buf[at_pos + 1..];
                            if let Some(completions) = complete_file_path(partial) {
                                if completions.len() == 1 {
                                    let completed = format!("{}@{}", &buf[..at_pos], completions[0]);
                                    lines[0] = completed;
                                    cursor = lines[0].chars().count();
                                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                                } else if completions.len() > 1 {
                                    write!(stdout, "\r\n")?;
                                    for c in &completions {
                                        write!(stdout, "  \x1b[33m@{c}\x1b[0m\r\n")?;
                                    }
                                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                                }
                            }
                        }
                    }
                }

                // ── Up / Ctrl+P: history previous ─────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Up, ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::Char('p'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
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
                        cursor = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                    }
                }

                // ── Down / Ctrl+N: history next ───────────────────
                Event::Key(KeyEvent {
                    code: KeyCode::Down, ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::Char('n'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
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
                        cursor = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
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
                    cursor = lines.last().map(|l| l.chars().count()).unwrap_or(0);
                    if paste_lines.len() > 1 {
                        let count = lines.len();
                        write!(
                            stdout,
                            "\r\n\x1b[36m[Pasted {count} lines — Enter to submit, Shift+Enter to add more]\x1b[0m\r\n"
                        )?;
                    }
                    redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                }

                // ── Escape: cancel multiline (revert to single empty line) ──
                Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => {
                    if lines.len() > 1 || !lines[0].is_empty() {
                        lines = vec![String::new()];
                        cursor = 0;
                        redraw_buffer(&mut stdout, prompt, &lines, cursor)?;
                    }
                }

                _ => {} // Ignore resize, mouse, focus events
            }
        }
    }

    /// Interactive reverse history search (Ctrl+R).
    ///
    /// Displays `(reverse-i-search)`query`: result` and incrementally filters
    /// history. Returns `Some(entry)` if user selects, `None` if cancelled.
    fn reverse_search(&self, stdout: &mut io::Stdout) -> io::Result<Option<String>> {
        let mut query = String::new();
        let mut match_idx: Option<usize> = None;

        self.draw_search_prompt(stdout, &query, match_idx)?;

        loop {
            let evt = event::read()?;
            match evt {
                // Typing narrows the search
                Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    modifiers,
                    ..
                }) if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    query.push(c);
                    match_idx = self.find_reverse_match(&query, match_idx);
                    self.draw_search_prompt(stdout, &query, match_idx)?;
                }

                // Backspace narrows less
                Event::Key(KeyEvent { code: KeyCode::Backspace, .. }) => {
                    query.pop();
                    match_idx = if query.is_empty() {
                        None
                    } else {
                        self.find_reverse_match(&query, None)
                    };
                    self.draw_search_prompt(stdout, &query, match_idx)?;
                }

                // Ctrl+R again: find next (older) match
                Event::Key(KeyEvent {
                    code: KeyCode::Char('r'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if !query.is_empty() {
                        let next_start = match_idx.and_then(|i| if i > 0 { Some(i - 1) } else { None });
                        if let Some(start) = next_start {
                            match_idx = self.find_reverse_match_from(&query, start);
                        }
                        self.draw_search_prompt(stdout, &query, match_idx)?;
                    }
                }

                // Enter: accept the match
                Event::Key(KeyEvent { code: KeyCode::Enter, .. }) => {
                    // Clear search line
                    write!(stdout, "\r\x1b[K")?;
                    stdout.flush()?;
                    return Ok(match_idx.map(|i| self.history[i].clone()));
                }

                // Escape / Ctrl+C / Ctrl+G: cancel
                Event::Key(KeyEvent { code: KeyCode::Esc, .. })
                | Event::Key(KeyEvent {
                    code: KeyCode::Char('c') | KeyCode::Char('g'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => {
                    write!(stdout, "\r\x1b[K")?;
                    stdout.flush()?;
                    return Ok(None);
                }

                _ => {}
            }
        }
    }

    /// Draw the reverse search prompt line.
    fn draw_search_prompt(
        &self,
        stdout: &mut io::Stdout,
        query: &str,
        match_idx: Option<usize>,
    ) -> io::Result<()> {
        let display = match match_idx {
            Some(i) => {
                let entry = &self.history[i];
                // Show first line only for multi-line entries
                entry.lines().next().unwrap_or("")
            }
            None if query.is_empty() => "",
            None => "(no match)",
        };
        write!(
            stdout,
            "\r\x1b[K\x1b[33m(reverse-i-search)\x1b[0m`\x1b[1m{query}\x1b[0m': {display}"
        )?;
        stdout.flush()
    }

    /// Find the most recent history entry containing `query`, searching backwards.
    fn find_reverse_match(&self, query: &str, _current: Option<usize>) -> Option<usize> {
        let q = query.to_lowercase();
        self.history.iter().rposition(|entry| entry.to_lowercase().contains(&q))
    }

    /// Find match starting from a specific index (for Ctrl+R repeat).
    fn find_reverse_match_from(&self, query: &str, start: usize) -> Option<usize> {
        let q = query.to_lowercase();
        (0..=start).rev().find(|&i| self.history[i].to_lowercase().contains(&q))
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

/// Insert a char at a character position in a string.
fn str_insert_char(s: &mut String, pos: usize, c: char) {
    let byte_pos = s.char_indices().nth(pos).map(|(i, _)| i).unwrap_or(s.len());
    s.insert(byte_pos, c);
}

/// Remove a char at a character position from a string and return it.
fn str_remove_char(s: &mut String, pos: usize) -> Option<char> {
    let byte_pos = s.char_indices().nth(pos).map(|(i, _)| i)?;
    Some(s.remove(byte_pos))
}

/// Redraw the entire multiline buffer.
fn redraw_buffer(stdout: &mut io::Stdout, prompt: &str, lines: &[String], cursor: usize) -> io::Result<()> {
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

    // Position cursor: it's on the last line now. Move it to the right column.
    let last_len = lines.last().map(|l| l.chars().count()).unwrap_or(0);
    if cursor < last_len {
        let back = last_len - cursor;
        write!(stdout, "\x1b[{back}D")?;
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
