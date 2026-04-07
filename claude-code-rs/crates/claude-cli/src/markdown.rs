//! Lightweight streaming markdown renderer for terminal output.
//!
//! Processes text deltas character-by-character and emits ANSI-colored output.
//! Handles: headers, bold, italic, inline code, fenced code blocks, bullet lists.
//! Code blocks get syntax highlighting via `syntect`.

use std::io::Write;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};

/// Lazy-initialized syntax highlighting resources (loaded once).
struct SyntaxResources {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl SyntaxResources {
    fn get() -> &'static Self {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<SyntaxResources> = OnceLock::new();
        INSTANCE.get_or_init(|| SyntaxResources {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        })
    }
}

/// Streaming markdown rendering state machine.
pub struct MarkdownRenderer {
    /// Accumulated buffer for current line (we render line-by-line).
    line_buf: String,
    /// Whether we're inside a fenced code block (```).
    in_code_block: bool,
    /// The language hint for the current code block (if any).
    code_lang: String,
    /// Whether the code block header (```lang) has been printed.
    code_header_printed: bool,
    /// Accumulated code lines for syntax highlighting (flushed on block end).
    code_lines: Vec<String>,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        Self {
            line_buf: String::new(),
            in_code_block: false,
            code_lang: String::new(),
            code_header_printed: false,
            code_lines: Vec::new(),
        }
    }

    /// Process a text delta (may contain partial lines, newlines, etc.).
    pub fn push(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                self.flush_line();
            } else {
                self.line_buf.push(ch);
            }
        }
    }

    /// Flush any remaining buffered content (call at end of stream).
    pub fn finish(&mut self) {
        if !self.line_buf.is_empty() {
            self.flush_line();
        }
        if self.in_code_block {
            // Unterminated code block — flush accumulated code with highlighting
            self.flush_code_block();
            print!("\x1b[0m");
            std::io::stdout().flush().ok();
        }
    }

    fn flush_line(&mut self) {
        let line = std::mem::take(&mut self.line_buf);

        if self.in_code_block {
            if line.trim_start().starts_with("```") {
                // End of code block — flush with syntax highlighting
                self.in_code_block = false;
                self.flush_code_block();
                self.code_lang.clear();
                self.code_header_printed = false;
                println!("\x1b[0m");
            } else {
                // Accumulate code lines
                self.code_lines.push(line);
            }
            return;
        }

        // Check for fenced code block start
        if line.trim_start().starts_with("```") {
            self.in_code_block = true;
            let lang = line.trim_start().trim_start_matches('`').trim();
            self.code_lang = lang.to_string();
            self.code_header_printed = true;
            if lang.is_empty() {
                println!("\x1b[2m───────────────────\x1b[0m");
            } else {
                println!("\x1b[2m─── {} ───\x1b[0m", lang);
            }
            return;
        }

        // Headers
        if let Some(rest) = line.strip_prefix("#### ") {
            println!("\x1b[1m{}\x1b[0m", rest);
            return;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            println!("\x1b[1m{}\x1b[0m", rest);
            return;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            println!("\x1b[1;4m{}\x1b[0m", rest);
            return;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            println!("\x1b[1;4m{}\x1b[0m", rest);
            return;
        }

        // Bullet lists: render with colored bullet
        if line.starts_with("- ") || line.starts_with("* ") {
            print!("\x1b[33m•\x1b[0m ");
            render_inline(&line[2..]);
            println!();
            return;
        }

        // Numbered lists
        if let Some(rest) = strip_numbered_list(&line) {
            let prefix_len = line.len() - rest.len();
            print!("\x1b[33m{}\x1b[0m", &line[..prefix_len]);
            render_inline(rest);
            println!();
            return;
        }

        // Horizontal rule
        if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            println!("\x1b[2m────────────────────────────\x1b[0m");
            return;
        }

        // Regular paragraph — apply inline formatting
        render_inline(&line);
        println!();
    }

    /// Flush accumulated code lines with syntax highlighting.
    fn flush_code_block(&mut self) {
        let lines = std::mem::take(&mut self.code_lines);
        if lines.is_empty() {
            return;
        }

        let res = SyntaxResources::get();
        let theme = &res.theme_set.themes["base16-ocean.dark"];

        // Map common language aliases
        let lang = match self.code_lang.as_str() {
            "js" | "jsx" => "JavaScript",
            "ts" | "tsx" => "TypeScript",
            "py" => "Python",
            "rb" => "Ruby",
            "rs" => "Rust",
            "sh" | "bash" | "zsh" | "shell" => "Bourne Again Shell (bash)",
            "yml" => "YAML",
            "md" | "markdown" => "Markdown",
            "cs" => "C#",
            "cpp" | "cc" | "cxx" => "C++",
            other => other,
        };

        // Try to find syntax by language hint
        let syntax = if lang.is_empty() {
            res.syntax_set.find_syntax_plain_text()
        } else {
            res.syntax_set
                .find_syntax_by_name(lang)
                .or_else(|| res.syntax_set.find_syntax_by_extension(lang))
                .or_else(|| res.syntax_set.find_syntax_by_extension(&self.code_lang))
                .unwrap_or_else(|| res.syntax_set.find_syntax_plain_text())
        };

        let mut highlighter = HighlightLines::new(syntax, theme);

        // Rejoin lines with newlines for syntect (it expects `\n` terminated lines)
        let code = lines.join("\n") + "\n";
        for line in LinesWithEndings::from(&code) {
            match highlighter.highlight_line(line, &res.syntax_set) {
                Ok(ranges) => {
                    let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                    print!("{}", escaped);
                }
                Err(_) => {
                    // Fallback: dim text
                    print!("\x1b[2m{}\x1b[0m", line);
                }
            }
        }
        print!("\x1b[0m");
        std::io::stdout().flush().ok();
    }
}

/// Strip numbered list prefix like "1. ", "12. " etc.
fn strip_numbered_list(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let digit_end = trimmed.find(|c: char| !c.is_ascii_digit())?;
    if digit_end == 0 { return None; }
    let rest = &trimmed[digit_end..];
    if let Some(after_dot) = rest.strip_prefix(". ") {
        Some(after_dot)
    } else {
        None
    }
}

/// Render a line of text with inline markdown formatting.
/// Handles: **bold**, *italic*, `code`, ~~strikethrough~~.
fn render_inline(text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Inline code: `...`
        if chars[i] == '`' {
            if let Some(end) = find_closing(&chars, i + 1, '`') {
                print!("\x1b[36m");
                for c in &chars[i + 1..end] {
                    print!("{}", c);
                }
                print!("\x1b[0m");
                i = end + 1;
                continue;
            }
        }

        // Bold: **...**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_double_closing(&chars, i + 2, '*') {
                print!("\x1b[1m");
                for c in &chars[i + 2..end] {
                    print!("{}", c);
                }
                print!("\x1b[0m");
                i = end + 2;
                continue;
            }
        }

        // Italic: *...*
        if chars[i] == '*' && (i + 1 < len && chars[i + 1] != '*') {
            if let Some(end) = find_closing(&chars, i + 1, '*') {
                print!("\x1b[3m");
                for c in &chars[i + 1..end] {
                    print!("{}", c);
                }
                print!("\x1b[0m");
                i = end + 1;
                continue;
            }
        }

        // Strikethrough: ~~...~~
        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            if let Some(end) = find_double_closing(&chars, i + 2, '~') {
                print!("\x1b[9m");
                for c in &chars[i + 2..end] {
                    print!("{}", c);
                }
                print!("\x1b[0m");
                i = end + 2;
                continue;
            }
        }

        print!("{}", chars[i]);
        i += 1;
    }
    std::io::stdout().flush().ok();
}

/// Find closing single delimiter.
fn find_closing(chars: &[char], start: usize, delim: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == delim)
}

/// Find closing double delimiter (e.g., ** or ~~).
fn find_double_closing(chars: &[char], start: usize, delim: char) -> Option<usize> {
    (start..chars.len().saturating_sub(1)).find(|&i| chars[i] == delim && chars[i + 1] == delim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_numbered_list() {
        assert_eq!(strip_numbered_list("1. Hello"), Some("Hello"));
        assert_eq!(strip_numbered_list("12. World"), Some("World"));
        assert_eq!(strip_numbered_list("Not a list"), None);
        assert_eq!(strip_numbered_list(""), None);
    }

    #[test]
    fn test_find_closing() {
        let chars: Vec<char> = "hello`world".chars().collect();
        assert_eq!(find_closing(&chars, 0, '`'), Some(5));
    }

    #[test]
    fn test_find_double_closing() {
        let chars: Vec<char> = "hello**world".chars().collect();
        assert_eq!(find_double_closing(&chars, 0, '*'), Some(5));
    }

    #[test]
    fn test_renderer_code_block_toggle() {
        let mut r = MarkdownRenderer::new();
        assert!(!r.in_code_block);
        r.push("```rust\n");
        assert!(r.in_code_block);
        assert_eq!(r.code_lang, "rust");
        r.push("let x = 1;\n");
        assert!(r.in_code_block);
        r.push("```\n");
        assert!(!r.in_code_block);
    }

    #[test]
    fn test_renderer_empty_input() {
        let mut r = MarkdownRenderer::new();
        r.push("");
        r.finish();
        // Should not panic
    }

    #[test]
    fn test_renderer_partial_line() {
        let mut r = MarkdownRenderer::new();
        r.push("hel");
        r.push("lo");
        assert_eq!(r.line_buf, "hello");
        r.finish();
    }

    #[test]
    fn test_find_double_closing_at_end() {
        // "bold**" — delimiter at very end of string
        let chars: Vec<char> = "bold**".chars().collect();
        assert_eq!(find_double_closing(&chars, 0, '*'), Some(4));
    }

    #[test]
    fn test_find_double_closing_not_found() {
        let chars: Vec<char> = "no delimiters".chars().collect();
        assert_eq!(find_double_closing(&chars, 0, '*'), None);
    }

    #[test]
    fn test_find_closing_not_found() {
        let chars: Vec<char> = "no backtick".chars().collect();
        assert_eq!(find_closing(&chars, 0, '`'), None);
    }

    #[test]
    fn test_strip_numbered_list_edge_cases() {
        assert_eq!(strip_numbered_list("0. Zero"), Some("Zero"));
        assert_eq!(strip_numbered_list("99. Ninety-nine"), Some("Ninety-nine"));
        assert_eq!(strip_numbered_list("1.No space"), None);
        assert_eq!(strip_numbered_list(". Dot"), None);
    }
}
