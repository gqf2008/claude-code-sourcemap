//! Streaming markdown renderer for terminal output.
//!
//! Processes text deltas character-by-character and emits ANSI-colored output.
//! Supports: headers, bold, italic, inline code, fenced code blocks, bullet/numbered
//! lists, nested lists, task lists, blockquotes, tables, links, horizontal rules.
//! Code blocks get syntax highlighting via `syntect`.

use std::io::Write;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};
use unicode_width::UnicodeWidthStr;

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

/// Table column alignment parsed from the separator row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
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
    /// Accumulated table lines (header + separator + rows).
    table_lines: Vec<String>,
    /// Whether we're currently accumulating table rows.
    in_table: bool,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        Self {
            line_buf: String::new(),
            in_code_block: false,
            code_lang: String::new(),
            code_header_printed: false,
            code_lines: Vec::new(),
            table_lines: Vec::new(),
            in_table: false,
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
            self.flush_code_block();
            print!("\x1b[0m");
            std::io::stdout().flush().ok();
        }
        if self.in_table {
            self.flush_table();
        }
    }

    fn flush_line(&mut self) {
        let line = std::mem::take(&mut self.line_buf);

        // === Code block state ===
        if self.in_code_block {
            if line.trim_start().starts_with("```") {
                self.in_code_block = false;
                self.flush_code_block();
                self.code_lang.clear();
                self.code_header_printed = false;
                println!("\x1b[0m");
            } else {
                self.code_lines.push(line);
            }
            return;
        }

        // === Table accumulation ===
        if self.in_table {
            if is_table_row(&line) {
                self.table_lines.push(line);
                return;
            }
            // Non-table line: flush accumulated table, then process this line
            self.flush_table();
            // fall through to process `line` normally
        }

        // Check for table start: a pipe-separated line followed by separator
        // We detect the first row of a table and start accumulating.
        if is_table_row(&line) {
            self.in_table = true;
            self.table_lines.push(line);
            return;
        }

        // === Fenced code block start ===
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

        // === Headers ===
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

        // === Blockquotes ===
        if let Some(content) = strip_blockquote(&line) {
            print!("\x1b[36m│\x1b[0m ");
            // Recursively handle nested blockquotes
            if let Some(inner) = strip_blockquote(content) {
                print!("\x1b[36m│\x1b[0m ");
                render_inline(inner);
            } else {
                render_inline(content);
            }
            println!();
            return;
        }

        // === Task lists: - [ ] or - [x] ===
        if let Some(rest) = line.strip_prefix("- [x] ").or_else(|| line.strip_prefix("- [X] ")) {
            print!("\x1b[32m☑\x1b[0m ");
            render_inline(rest);
            println!();
            return;
        }
        if let Some(rest) = line.strip_prefix("- [ ] ") {
            print!("\x1b[2m☐\x1b[0m ");
            render_inline(rest);
            println!();
            return;
        }

        // === Nested / indented lists ===
        if let Some((indent, bullet_content)) = parse_indented_list(&line) {
            let indent_str = "  ".repeat(indent);
            print!("{}\x1b[33m•\x1b[0m ", indent_str);
            render_inline(bullet_content);
            println!();
            return;
        }

        // === Bullet lists (top-level) ===
        if line.starts_with("- ") || line.starts_with("* ") {
            print!("\x1b[33m•\x1b[0m ");
            render_inline(&line[2..]);
            println!();
            return;
        }

        // === Numbered lists ===
        if let Some((indent_level, prefix, rest)) = strip_numbered_list_full(&line) {
            let indent_str = "  ".repeat(indent_level);
            print!("{}\x1b[33m{}\x1b[0m", indent_str, prefix);
            render_inline(rest);
            println!();
            return;
        }

        // === Horizontal rule ===
        if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            println!("\x1b[2m────────────────────────────\x1b[0m");
            return;
        }

        // === Regular paragraph ===
        render_inline(&line);
        println!();
    }

    /// Flush accumulated table lines and render as a formatted table.
    fn flush_table(&mut self) {
        self.in_table = false;
        let lines = std::mem::take(&mut self.table_lines);

        if lines.len() < 2 {
            // Not enough lines for a valid table (need header + separator at minimum)
            // Just render them as regular text
            for l in &lines {
                render_inline(l);
                println!();
            }
            return;
        }

        // Parse table: find separator row (contains only |, -, :, spaces)
        let sep_idx = lines.iter().position(|l| is_table_separator(l));

        let (header_lines, alignments, data_lines) = if let Some(si) = sep_idx {
            let aligns = parse_alignments(&lines[si]);
            let headers: Vec<&str> = lines[..si].iter().map(|s| s.as_str()).collect();
            let data: Vec<&str> = lines[si + 1..].iter().map(|s| s.as_str()).collect();
            (headers, aligns, data)
        } else {
            // No separator found — treat first line as header, rest as data
            let aligns = vec![Align::Left; parse_cells(&lines[0]).len()];
            let headers = vec![lines[0].as_str()];
            let data: Vec<&str> = lines[1..].iter().map(|s| s.as_str()).collect();
            (headers, aligns, data)
        };

        // Parse all cells
        let header_cells: Vec<Vec<String>> = header_lines
            .iter()
            .map(|l| parse_cells(l))
            .collect();
        let data_cells: Vec<Vec<String>> = data_lines
            .iter()
            .map(|l| parse_cells(l))
            .collect();

        // Determine number of columns
        let num_cols = alignments.len().max(
            header_cells.iter().map(|r| r.len()).max().unwrap_or(0).max(
                data_cells.iter().map(|r| r.len()).max().unwrap_or(0),
            ),
        );

        if num_cols == 0 {
            return;
        }

        // Calculate column widths
        let terminal_width = terminal_width();
        let mut col_widths: Vec<usize> = vec![0; num_cols];

        for row in header_cells.iter().chain(data_cells.iter()) {
            for (i, cell) in row.iter().enumerate() {
                if i < num_cols {
                    col_widths[i] = col_widths[i].max(display_width(cell));
                }
            }
        }

        // Ensure minimum width of 3
        for w in &mut col_widths {
            *w = (*w).max(3);
        }

        // Shrink columns if table is too wide for terminal
        // Border overhead: │ cell │ cell │ = 1 + 3*num_cols
        let border_overhead = 1 + 3 * num_cols;
        let total_content_width: usize = col_widths.iter().sum();
        let total_width = total_content_width + border_overhead;

        if total_width > terminal_width && total_content_width > 0 {
            let available = terminal_width.saturating_sub(border_overhead);
            let scale = available as f64 / total_content_width as f64;
            for w in &mut col_widths {
                *w = ((*w as f64 * scale) as usize).max(3);
            }
        }

        // Render table with Unicode box-drawing characters
        render_table_border(&col_widths, '┌', '┬', '┐');

        // Header rows
        for row in &header_cells {
            render_table_row(row, &col_widths, &alignments, true);
        }

        // Header/data separator
        render_table_border(&col_widths, '├', '┼', '┤');

        // Data rows
        for row in &data_cells {
            render_table_row(row, &col_widths, &alignments, false);
        }

        // Bottom border
        render_table_border(&col_widths, '└', '┴', '┘');
    }

    /// Flush accumulated code lines with syntax highlighting.
    fn flush_code_block(&mut self) {
        let lines = std::mem::take(&mut self.code_lines);
        if lines.is_empty() {
            return;
        }

        let res = SyntaxResources::get();
        let theme = &res.theme_set.themes["base16-ocean.dark"];

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

        let code = lines.join("\n") + "\n";
        for line in LinesWithEndings::from(&code) {
            match highlighter.highlight_line(line, &res.syntax_set) {
                Ok(ranges) => {
                    let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                    print!("{}", escaped);
                }
                Err(_) => {
                    print!("\x1b[2m{}\x1b[0m", line);
                }
            }
        }
        print!("\x1b[0m");
        std::io::stdout().flush().ok();
    }
}

// ── Table helpers ──────────────────────────────────────────────────────────

/// Check if a line looks like a table row (contains at least one `|`).
fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('|') && !trimmed.starts_with("```")
}

/// Check if a line is a table separator (e.g., `| --- | :---: | ---: |`).
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return false;
    }
    // Remove leading/trailing pipes and split
    let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
    let cells: Vec<&str> = inner.split('|').collect();
    if cells.is_empty() {
        return false;
    }
    cells.iter().all(|cell| {
        let c = cell.trim();
        if c.is_empty() {
            return true;
        }
        // Must be all dashes, optionally with leading/trailing colons
        let stripped = c.trim_start_matches(':').trim_end_matches(':');
        !stripped.is_empty() && stripped.chars().all(|ch| ch == '-')
    })
}

/// Parse column alignments from separator row.
fn parse_alignments(sep_line: &str) -> Vec<Align> {
    let inner = sep_line.trim().trim_start_matches('|').trim_end_matches('|');
    inner
        .split('|')
        .map(|cell| {
            let c = cell.trim();
            let left = c.starts_with(':');
            let right = c.ends_with(':');
            match (left, right) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

/// Parse cells from a table row, trimming whitespace.
fn parse_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// Get display width of a string (accounting for wide characters).
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Get terminal width (fallback to 80).
fn terminal_width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// Render a table border line: e.g., ┌───┬───┐
fn render_table_border(widths: &[usize], left: char, mid: char, right: char) {
    print!("\x1b[2m{}", left);
    for (i, &w) in widths.iter().enumerate() {
        for _ in 0..w + 2 {
            print!("─");
        }
        if i < widths.len() - 1 {
            print!("{}", mid);
        }
    }
    println!("{}\x1b[0m", right);
}

/// Render a table row with cell content.
fn render_table_row(cells: &[String], widths: &[usize], aligns: &[Align], is_header: bool) {
    print!("\x1b[2m│\x1b[0m");
    for (i, width) in widths.iter().enumerate() {
        let cell = cells.get(i).map(|s| s.as_str()).unwrap_or("");
        let cell_width = display_width(cell);
        let w = *width;
        let align = aligns.get(i).copied().unwrap_or(Align::Left);

        // Truncate if cell is wider than column
        let display_cell = if cell_width > w {
            truncate_to_width(cell, w.saturating_sub(1))
        } else {
            cell.to_string()
        };
        let display_cell_width = display_width(&display_cell);
        let padding = w.saturating_sub(display_cell_width);

        let (left_pad, right_pad) = match align {
            Align::Left => (0, padding),
            Align::Right => (padding, 0),
            Align::Center => {
                let lp = padding / 2;
                (lp, padding - lp)
            }
        };

        print!(" ");
        for _ in 0..left_pad {
            print!(" ");
        }
        if is_header {
            print!("\x1b[1m");
            render_inline(&display_cell);
            print!("\x1b[0m");
        } else {
            render_inline(&display_cell);
        }
        for _ in 0..right_pad {
            print!(" ");
        }
        print!(" \x1b[2m│\x1b[0m");
    }
    println!();
}

/// Truncate a string to fit within a given display width, adding "…".
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return "…".to_string();
    }
    let mut result = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            result.push('…');
            return result;
        }
        result.push(ch);
        w += cw;
    }
    result
}

// ── Blockquote / list helpers ──────────────────────────────────────────────

/// Strip blockquote prefix: `> text` → `text`, `>text` → `text`.
fn strip_blockquote(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("> ") {
        Some(rest)
    } else if let Some(rest) = trimmed.strip_prefix('>') {
        // `>text` without space
        Some(rest)
    } else {
        None
    }
}

/// Parse an indented list item. Returns (indent_level, content).
/// Handles: "  - item", "    - item", "  * item", etc.
fn parse_indented_list(line: &str) -> Option<(usize, &str)> {
    let stripped = line.trim_end();
    if stripped.is_empty() {
        return None;
    }

    // Count leading whitespace
    let indent_chars = stripped.len() - stripped.trim_start().len();
    if indent_chars < 2 {
        return None; // need at least 2 spaces for nesting
    }

    let trimmed = stripped.trim_start();

    // Check for bullet markers
    if let Some(rest) = trimmed.strip_prefix("- ") {
        Some((indent_chars / 2, rest))
    } else if let Some(rest) = trimmed.strip_prefix("* ") {
        Some((indent_chars / 2, rest))
    } else {
        None
    }
}

/// Strip numbered list prefix, supporting indentation.
/// Returns (indent_level, prefix_str, rest_of_line).
fn strip_numbered_list_full(line: &str) -> Option<(usize, &str, &str)> {
    let indent_chars = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let digit_end = trimmed.find(|c: char| !c.is_ascii_digit())?;
    if digit_end == 0 {
        return None;
    }
    let rest = &trimmed[digit_end..];
    if let Some(after_dot) = rest.strip_prefix(". ") {
        let prefix = &trimmed[..digit_end + 2]; // "N. "
        Some((indent_chars / 2, prefix, after_dot))
    } else {
        None
    }
}

// ── Inline formatting ──────────────────────────────────────────────────────

/// Render a line of text with inline markdown formatting.
/// Handles: **bold**, *italic*, `code`, ~~strikethrough~~, [links](url).
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

        // Link: [text](url)
        if chars[i] == '[' {
            if let Some((text_end, url, total_end)) = parse_link(&chars, i) {
                // Render link text with underline
                print!("\x1b[4m");
                for c in &chars[i + 1..text_end] {
                    print!("{}", c);
                }
                print!("\x1b[0m");
                // Show URL in dim
                print!("\x1b[2m ({})\x1b[0m", url);
                i = total_end;
                continue;
            }
        }

        // Bold: **...**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_double_closing(&chars, i + 2, '*') {
                print!("\x1b[1m");
                // Recurse for nested formatting inside bold
                let inner: String = chars[i + 2..end].iter().collect();
                render_inline(&inner);
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

/// Parse a markdown link starting at position `start` (which should be `[`).
/// Returns (text_end_idx, url_string, total_end_idx) or None.
fn parse_link(chars: &[char], start: usize) -> Option<(usize, String, usize)> {
    // Find closing ]
    let text_end = find_closing(chars, start + 1, ']')?;
    // Must be followed by (
    if text_end + 1 >= chars.len() || chars[text_end + 1] != '(' {
        return None;
    }
    // Find closing )
    let url_end = find_closing(chars, text_end + 2, ')')?;
    let url: String = chars[text_end + 2..url_end].iter().collect();
    Some((text_end, url, url_end + 1))
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
    fn test_strip_blockquote() {
        assert_eq!(strip_blockquote("> hello"), Some("hello"));
        assert_eq!(strip_blockquote(">hello"), Some("hello"));
        assert_eq!(strip_blockquote("  > indented"), Some("indented"));
        assert_eq!(strip_blockquote("not a quote"), None);
    }

    #[test]
    fn test_parse_indented_list() {
        assert_eq!(parse_indented_list("  - nested"), Some((1, "nested")));
        assert_eq!(parse_indented_list("    - deep"), Some((2, "deep")));
        assert_eq!(parse_indented_list("  * star"), Some((1, "star")));
        assert_eq!(parse_indented_list("- top"), None);
        assert_eq!(parse_indented_list("not a list"), None);
    }

    #[test]
    fn test_strip_numbered_list_full() {
        let result = strip_numbered_list_full("1. Hello");
        assert!(result.is_some());
        let (indent, prefix, rest) = result.unwrap();
        assert_eq!(indent, 0);
        assert_eq!(prefix, "1. ");
        assert_eq!(rest, "Hello");

        let result2 = strip_numbered_list_full("  2. Indented");
        assert!(result2.is_some());
        let (indent2, _, rest2) = result2.unwrap();
        assert_eq!(indent2, 1);
        assert_eq!(rest2, "Indented");
    }

    #[test]
    fn test_is_table_row() {
        assert!(is_table_row("| a | b | c |"));
        assert!(is_table_row("a | b | c"));
        assert!(!is_table_row("no pipes here"));
        assert!(!is_table_row("```|code|```"));
    }

    #[test]
    fn test_is_table_separator() {
        assert!(is_table_separator("| --- | --- | --- |"));
        assert!(is_table_separator("|---|---|"));
        assert!(is_table_separator("| :---: | ---: | :--- |"));
        assert!(!is_table_separator("| hello | world |"));
        assert!(!is_table_separator("no table"));
    }

    #[test]
    fn test_parse_alignments() {
        let aligns = parse_alignments("| :--- | :---: | ---: |");
        assert_eq!(aligns, vec![Align::Left, Align::Center, Align::Right]);

        let aligns2 = parse_alignments("| --- | --- |");
        assert_eq!(aligns2, vec![Align::Left, Align::Left]);
    }

    #[test]
    fn test_parse_cells() {
        let cells = parse_cells("| hello | world | test |");
        assert_eq!(cells, vec!["hello", "world", "test"]);

        let cells2 = parse_cells("| single |");
        assert_eq!(cells2, vec!["single"]);
    }

    #[test]
    fn test_parse_link() {
        let chars: Vec<char> = "[text](https://example.com)".chars().collect();
        let result = parse_link(&chars, 0);
        assert!(result.is_some());
        let (text_end, url, total_end) = result.unwrap();
        assert_eq!(text_end, 5); // position of ]
        assert_eq!(url, "https://example.com");
        assert_eq!(total_end, 27);
    }

    #[test]
    fn test_parse_link_no_url() {
        let chars: Vec<char> = "[text] no url".chars().collect();
        assert!(parse_link(&chars, 0).is_none());
    }

    #[test]
    fn test_truncate_to_width() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello world", 5), "hello…");
        assert_eq!(truncate_to_width("", 5), "");
    }

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
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
    fn test_renderer_table_accumulation() {
        let mut r = MarkdownRenderer::new();
        r.push("| A | B |\n");
        assert!(r.in_table);
        assert_eq!(r.table_lines.len(), 1);
        r.push("| --- | --- |\n");
        assert_eq!(r.table_lines.len(), 2);
        r.push("| 1 | 2 |\n");
        assert_eq!(r.table_lines.len(), 3);
        // Non-table line triggers flush
        r.push("Regular text\n");
        assert!(!r.in_table);
        assert!(r.table_lines.is_empty());
    }

    #[test]
    fn test_renderer_table_finish() {
        let mut r = MarkdownRenderer::new();
        r.push("| A | B |\n| --- | --- |\n| 1 | 2 |\n");
        assert!(r.in_table);
        r.finish();
        assert!(!r.in_table);
    }

    #[test]
    fn test_find_double_closing_at_end() {
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
        assert!(strip_numbered_list_full("0. Zero").is_some());
        assert!(strip_numbered_list_full("99. Ninety-nine").is_some());
        assert!(strip_numbered_list_full("1.No space").is_none());
        assert!(strip_numbered_list_full(". Dot").is_none());
    }
}
