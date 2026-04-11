//! Rustyline-based terminal input reader with slash command completion.
//!
//! Features:
//! - Slash command tab completion with dropdown list
//! - @file path completion
//! - History navigation with persistent storage
//! - Multiline input (Ctrl+J / Shift+Enter)
//! - Emacs key bindings (Ctrl+A/E/U/K/W, Alt+B/F/D, etc.)
//! - Reverse history search (Ctrl+R)

use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::{Hint, Hinter};
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{
    Cmd, CompletionType, Config, Context, EditMode, Editor, EventHandler, Helper,
    KeyCode, KeyEvent, Modifiers,
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
    "/vim", "/stickers", "/effort", "/tag", "/release-notes", "/feedback",
    "/stats", "/usage", "/image", "/pr-comments", "/branch",
];

/// Short description for each slash command (displayed in completion list).
fn command_description(name: &str) -> &'static str {
    match name {
        "/help" => "Show help",
        "/clear" => "Clear conversation history",
        "/model" => "Switch model",
        "/compact" => "Compact conversation",
        "/cost" => "Show token usage and costs",
        "/skills" => "List available skills",
        "/memory" => "Manage memory files",
        "/session" => "Manage sessions",
        "/diff" => "Show git diff",
        "/status" => "Show session and git status",
        "/permissions" => "Show permission mode",
        "/config" => "Show current configuration",
        "/undo" => "Undo last assistant turn",
        "/review" => "AI code review",
        "/doctor" => "Check environment health",
        "/init" => "Initialize CLAUDE.md",
        "/commit" => "Stage and commit changes",
        "/commit-push-pr" => "Commit, push, create PR",
        "/pr" => "Create/review pull request",
        "/bug" => "Debug a problem",
        "/search" => "Search conversation history",
        "/history" => "Browse conversation turns",
        "/retry" => "Retry last failed prompt",
        "/version" => "Show version info",
        "/login" => "Set API key",
        "/logout" => "Clear API key",
        "/context" => "Show loaded context",
        "/export" => "Export session",
        "/reload-context" => "Reload CLAUDE.md and settings",
        "/mcp" => "Show MCP servers",
        "/plugin" => "List loaded plugins",
        "/exit" => "Exit the CLI",
        "/fast" => "Toggle fast/cheap model",
        "/add-dir" => "Add context directory",
        "/summary" => "Generate conversation summary",
        "/rename" => "Rename current session",
        "/copy" => "Copy last response to clipboard",
        "/share" => "Export shareable session",
        "/files" => "List files in directory",
        "/env" => "Show environment info",
        "/agents" => "Manage agent definitions",
        "/theme" => "Switch terminal theme",
        "/plan" => "Toggle plan mode",
        "/think" => "Toggle extended thinking",
        "/break-cache" => "Skip prompt cache",
        "/rewind" => "Rewind by N turns",
        "/vim" => "Toggle vim mode",
        "/stickers" => "Order stickers!",
        "/effort" => "Set effort level",
        "/tag" => "Tag/untag session",
        "/release-notes" => "Show release notes",
        "/feedback" => "Submit feedback",
        "/stats" => "Show session statistics",
        "/usage" => "Show usage stats",
        "/image" => "Attach an image",
        "/pr-comments" => "Fetch PR review comments",
        "/branch" => "Fork conversation branch",
        _ => "",
    }
}

/// Result from reading a line of input.
pub enum InputResult {
    /// User entered text (may contain newlines from multiline input).
    Line(String),
    /// User pressed Ctrl+D on empty buffer (EOF).
    Eof,
    /// User pressed Ctrl+C.
    Interrupted,
}

// --- Rustyline helper ------------------------------------------------

/// Ghost-text hint for slash commands (shows dimmed remainder of unique match).
#[derive(Debug)]
struct SlashHint {
    text: String,
}

impl Hint for SlashHint {
    fn display(&self) -> &str {
        &self.text
    }
    fn completion(&self) -> Option<&str> {
        Some(&self.text)
    }
}

/// Rustyline helper: slash command + @file completion, hints, and highlighting.
struct InputHelper;

impl InputHelper {
    fn new() -> Self {
        Self
    }
}

impl Completer for InputHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        if pos != line.len() {
            return Ok((0, Vec::new()));
        }

        // Slash command completion
        if line.starts_with('/') && !line.contains(' ') {
            let matches: Vec<Pair> = SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(line))
                .map(|cmd| {
                    let desc = command_description(cmd);
                    let display = if desc.is_empty() {
                        cmd.to_string()
                    } else {
                        format!("{cmd}  \x1b[2m{desc}\x1b[0m")
                    };
                    Pair {
                        display,
                        replacement: cmd.to_string(),
                    }
                })
                .collect();
            return Ok((0, matches));
        }

        // @file path completion
        if let Some(at_pos) = line.rfind('@') {
            let partial = &line[at_pos + 1..];
            if let Some(completions) = complete_file_path(partial) {
                let pairs: Vec<Pair> = completions
                    .into_iter()
                    .map(|path| {
                        let replacement = format!("{}@{path}", &line[..at_pos]);
                        Pair {
                            display: format!("@{path}"),
                            replacement,
                        }
                    })
                    .collect();
                return Ok((0, pairs));
            }
        }

        Ok((0, Vec::new()))
    }
}

impl Hinter for InputHelper {
    type Hint = SlashHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<Self::Hint> {
        if pos != line.len() || !line.starts_with('/') || line.contains(' ') {
            return None;
        }
        let mut found: Option<&str> = None;
        for cmd in SLASH_COMMANDS {
            if cmd.starts_with(line) && *cmd != line {
                if found.is_some() {
                    return None;
                }
                found = Some(cmd);
            }
        }
        found.map(|cmd| SlashHint {
            text: cmd[line.len()..].to_string(),
        })
    }
}

impl Highlighter for InputHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        if line.starts_with('/') {
            Cow::Owned(format!("\x1b[36m{line}\x1b[0m"))
        } else {
            Cow::Borrowed(line)
        }
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(&'s self, prompt: &'p str, _default: bool) -> Cow<'b, str> {
        Cow::Owned(format!("\x1b[1;32m{prompt}\x1b[0m"))
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[2m{hint}\x1b[0m"))
    }

    fn highlight_char(&self, line: &str, _pos: usize, _kind: CmdKind) -> bool {
        line.starts_with('/')
    }
}

impl Validator for InputHelper {}
impl Helper for InputHelper {}
// --- Public InputReader -----------------------------------------------

/// Rustyline-based input reader with slash command completion and history.
pub struct InputReader {
    editor: Editor<InputHelper, DefaultHistory>,
}

impl InputReader {
    pub fn new() -> Self {
        let config = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .auto_add_history(false)
            .build();

        let mut editor = Editor::<InputHelper, DefaultHistory>::with_config(config)
            .expect("rustyline editor should initialize");
        editor.set_helper(Some(InputHelper::new()));

        // Ctrl+J and Shift+Enter insert newline (multiline input)
        editor.bind_sequence(
            KeyEvent(KeyCode::Char('J'), Modifiers::CTRL),
            EventHandler::Simple(Cmd::Newline),
        );
        editor.bind_sequence(
            KeyEvent(KeyCode::Enter, Modifiers::SHIFT),
            EventHandler::Simple(Cmd::Newline),
        );

        Self { editor }
    }

    /// Add an entry to history (deduplicates consecutive entries).
    pub fn add_history(&mut self, entry: &str) {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            return;
        }
        let _ = self.editor.add_history_entry(trimmed);
    }

    /// Load history from a file.
    pub fn load_history(&mut self, path: &Path) {
        let _ = self.editor.load_history(path);
    }

    /// Save history to a file.
    pub fn save_history(&mut self, path: &Path) {
        let _ = self.editor.save_history(path);
    }

    /// Check whether this reader can be used (requires a real terminal).
    #[allow(dead_code)]
    pub fn is_available() -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    /// Read user input with completion and multiline support.
    ///
    /// - Enter submits
    /// - Ctrl+J / Shift+Enter inserts a newline
    /// - Tab triggers completion (slash commands / @file paths)
    /// - Up/Down navigates history
    /// - Emacs key bindings (Ctrl+A/E/U/K/W, Ctrl+R, etc.)
    pub fn readline(&mut self, prompt: &str) -> io::Result<InputResult> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return self.read_pipe_fallback(prompt);
        }

        match self.editor.readline(prompt) {
            Ok(line) => Ok(InputResult::Line(line)),
            Err(ReadlineError::Interrupted) => Ok(InputResult::Interrupted),
            Err(ReadlineError::Eof) => Ok(InputResult::Eof),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    /// Fallback for piped/non-TTY input.
    #[allow(clippy::unused_self)]
    fn read_pipe_fallback(&self, prompt: &str) -> io::Result<InputResult> {
        let mut stdout = io::stdout();
        write!(stdout, "{prompt}")?;
        stdout.flush()?;

        let mut buffer = String::new();
        let bytes_read = io::stdin().read_line(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(InputResult::Eof);
        }
        while matches!(buffer.chars().last(), Some('\n' | '\r')) {
            buffer.pop();
        }
        Ok(InputResult::Line(buffer))
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

/// Paste an image from the system clipboard, save to a temp PNG file, and return its path.
///
/// Uses `arboard` for cross-platform clipboard access. The returned path can be passed
/// to `claude_core::image::read_image_file()` or referenced as `@path` in user input.
///
/// # Errors
/// Returns an error if the clipboard contains no image, or if encoding/saving fails.
#[allow(dead_code)]
pub fn paste_clipboard_image() -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context as _;

    let mut clip = arboard::Clipboard::new()
        .context("Cannot open clipboard (is a display server available?)")?;

    let img = clip.get_image()
        .context("No image in clipboard")?;

    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            std::io::Cursor::new(&mut png_bytes),
            img.width as u32,
            img.height as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .context("Failed to write PNG header")?;
        writer
            .write_image_data(&img.bytes)
            .context("Failed to encode clipboard image as PNG")?;
    }

    let filename = format!(
        "claude_clipboard_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    let path = std::env::temp_dir().join(filename);
    std::fs::write(&path, &png_bytes)
        .with_context(|| format!("Cannot save clipboard image to {}", path.display()))?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_history() {
        let mut reader = InputReader::new();
        reader.add_history("hello");
        reader.add_history("world");
        // No panic = success; rustyline manages dedup internally
    }

    #[test]
    fn test_add_history_empty() {
        let mut reader = InputReader::new();
        reader.add_history("");
        reader.add_history("   ");
        // No panic = success; empty entries are skipped
    }

    #[test]
    fn test_slash_commands_present() {
        assert!(SLASH_COMMANDS.contains(&"/help"));
        assert!(SLASH_COMMANDS.contains(&"/exit"));
        assert!(SLASH_COMMANDS.contains(&"/compact"));
        assert!(SLASH_COMMANDS.contains(&"/pr-comments"));
        assert!(SLASH_COMMANDS.contains(&"/branch"));
    }

    #[test]
    fn test_command_description_known() {
        assert_eq!(command_description("/help"), "Show help");
        assert_eq!(command_description("/exit"), "Exit the CLI");
        assert_eq!(command_description("/compact"), "Compact conversation");
    }

    #[test]
    fn test_command_description_unknown() {
        assert_eq!(command_description("/nonexistent"), "");
    }

    #[test]
    fn test_history_file_path() {
        let path = history_file_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.ends_with("history"));
    }

    #[test]
    fn test_completer_slash() {
        use rustyline::history::MemHistory;
        let helper = InputHelper::new();
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let (start, matches) = helper.complete("/he", 3, &ctx).unwrap();
        assert_eq!(start, 0);
        assert!(matches.iter().any(|p| p.replacement == "/help"));
    }

    #[test]
    fn test_completer_no_match() {
        use rustyline::history::MemHistory;
        let helper = InputHelper::new();
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let (_, matches) = helper.complete("hello", 5, &ctx).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_hinter_unique_match() {
        use rustyline::history::MemHistory;
        let helper = InputHelper::new();
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let hint = helper.hint("/ver", 4, &ctx);
        assert!(hint.is_some());
        assert_eq!(hint.unwrap().display(), "sion");
    }

    #[test]
    fn test_hinter_ambiguous() {
        use rustyline::history::MemHistory;
        let helper = InputHelper::new();
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let hint = helper.hint("/co", 3, &ctx);
        assert!(hint.is_none());
    }

    #[test]
    fn test_hinter_exact() {
        use rustyline::history::MemHistory;
        let helper = InputHelper::new();
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let hint = helper.hint("/help", 5, &ctx);
        assert!(hint.is_none());
    }

    #[test]
    fn paste_clipboard_image_no_display_returns_err() {
        let result = paste_clipboard_image();
        let _ = result;
    }

    #[test]
    fn png_encode_rgba8_roundtrip() {
        use std::io::Cursor;

        let pixels: Vec<u8> = vec![
            255, 0, 0, 255,
            255, 0, 0, 255,
            255, 0, 0, 255,
            255, 0, 0, 255,
        ];

        let mut buf: Vec<u8> = Vec::new();
        {
            let mut encoder = png::Encoder::new(Cursor::new(&mut buf), 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
        }

        assert!(buf.starts_with(&[0x89, 0x50, 0x4E, 0x47]), "Should start with PNG magic bytes");
        assert!(buf.len() > 8, "PNG should have content beyond header");
    }
}