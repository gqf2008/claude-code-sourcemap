//! Structured diff display with syntax highlighting.
//!
//! Renders unified diffs with red/green coloring and optional syntax-aware highlighting.
//! Used by `/review`, permission dialogs, and file edit previews.
use similar::{ChangeTag, TextDiff};
use std::io::Write;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

/// Lazy-initialized syntax highlighting resources.
struct SyntaxRes {
    ss: SyntaxSet,
    ts: ThemeSet,
}

impl SyntaxRes {
    fn get() -> &'static Self {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<SyntaxRes> = OnceLock::new();
        INSTANCE.get_or_init(|| SyntaxRes {
            ss: SyntaxSet::load_defaults_newlines(),
            ts: ThemeSet::load_defaults(),
        })
    }
}

/// Highlight a single line using syntect for the given syntax reference.
fn highlight_line(line: &str, hl: &mut HighlightLines, ss: &SyntaxSet) -> String {
    let line_nl = if line.ends_with('\n') {
        line.to_string()
    } else {
        format!("{}\n", line)
    };
    match hl.highlight_line(&line_nl, ss) {
        Ok(ranges) => as_24_bit_terminal_escaped(&ranges, false),
        Err(_) => line.to_string(),
    }
}

/// Render a delete/insert line pair with word-level highlighting.
/// Changed words get a bright background; unchanged parts use dim coloring.
fn render_word_diff_pair(old_line: &str, new_line: &str) {
    let word_diff = TextDiff::from_words(old_line, new_line);

    // Render deleted line with highlighted removed words
    eprint!("\x1b[31m- ");
    for change in word_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => eprint!("\x1b[41;97m{}\x1b[0m\x1b[31m", change.value()),
            ChangeTag::Equal => eprint!("{}", change.value()),
            ChangeTag::Insert => {} // skip inserts on the delete line
        }
    }
    eprintln!("\x1b[0m");

    // Render inserted line with highlighted added words
    eprint!("\x1b[32m+ ");
    for change in word_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => eprint!("\x1b[42;97m{}\x1b[0m\x1b[32m", change.value()),
            ChangeTag::Equal => eprint!("{}", change.value()),
            ChangeTag::Delete => {} // skip deletes on the insert line
        }
    }
    eprintln!("\x1b[0m");
}

/// Display a unified diff between two strings with colored + syntax-highlighted output.
///
/// Red (`-`) for removed lines, green (`+`) for added lines, dim for context.
/// When a file path is given, attempts syntax highlighting for the language.
pub fn print_diff(old: &str, new: &str, file_path: Option<&str>) {
    let diff = TextDiff::from_lines(old, new);

    if let Some(path) = file_path {
        eprintln!("\x1b[1;34m── {} ──\x1b[0m", path);
    }

    // Try to get syntax highlighter from file extension
    let res = SyntaxRes::get();
    let syntax: Option<&SyntaxReference> = file_path
        .and_then(|p| std::path::Path::new(p).extension())
        .and_then(|ext| ext.to_str())
        .and_then(|ext| res.ss.find_syntax_by_extension(ext));

    let theme = &res.ts.themes["base16-ocean.dark"];
    let mut hl_old = syntax.map(|s| HighlightLines::new(s, theme));
    let mut hl_new = syntax.map(|s| HighlightLines::new(s, theme));

    for group in diff.grouped_ops(3) {
        // Hunk header
        let first = group.first().unwrap();
        let last = group.last().unwrap();
        let old_range = first.old_range().start + 1..last.old_range().end + 1;
        let new_range = first.new_range().start + 1..last.new_range().end + 1;
        eprintln!(
            "\x1b[36m@@ -{},{} +{},{} @@\x1b[0m",
            old_range.start,
            old_range.end - old_range.start,
            new_range.start,
            new_range.end - new_range.start,
        );

        for op in &group {
            // Collect changes for word-level diff detection
            let changes: Vec<_> = diff.iter_changes(op).collect();
            let mut i = 0;
            while i < changes.len() {
                let change = &changes[i];
                let line = change.value();
                let trimmed = line.strip_suffix('\n').unwrap_or(line);

                match change.tag() {
                    ChangeTag::Delete => {
                        // Look ahead: if next change is Insert, render word-level diff
                        if i + 1 < changes.len() && changes[i + 1].tag() == ChangeTag::Insert {
                            let ins_line = changes[i + 1].value();
                            let ins_trimmed = ins_line.strip_suffix('\n').unwrap_or(ins_line);
                            render_word_diff_pair(trimmed, ins_trimmed);
                            // Advance highlighters on both lines to keep state in sync
                            if let Some(ref mut hl) = hl_old {
                                let _ = highlight_line(trimmed, hl, &res.ss);
                            }
                            if let Some(ref mut hl) = hl_new {
                                let _ = highlight_line(ins_trimmed, hl, &res.ss);
                            }
                            i += 2;
                            continue;
                        }
                        if let Some(ref mut hl) = hl_old {
                            let highlighted = highlight_line(trimmed, hl, &res.ss);
                            eprint!("\x1b[41m\x1b[97m-\x1b[0m ");
                            eprintln!("{}\x1b[0m", highlighted.trim_end());
                        } else {
                            eprintln!("\x1b[31m- {}\x1b[0m", trimmed);
                        }
                    }
                    ChangeTag::Insert => {
                        if let Some(ref mut hl) = hl_new {
                            let highlighted = highlight_line(trimmed, hl, &res.ss);
                            eprint!("\x1b[42m\x1b[97m+\x1b[0m ");
                            eprintln!("{}\x1b[0m", highlighted.trim_end());
                        } else {
                            eprintln!("\x1b[32m+ {}\x1b[0m", trimmed);
                        }
                    }
                    ChangeTag::Equal => {
                        if let Some(ref mut hl) = hl_new {
                            let highlighted = highlight_line(trimmed, hl, &res.ss);
                            // Also advance old highlighter to keep state in sync
                            if let Some(ref mut hl_o) = hl_old {
                                let _ = highlight_line(trimmed, hl_o, &res.ss);
                            }
                            eprintln!("\x1b[2m  {}\x1b[0m", highlighted.trim_end());
                        } else {
                            eprintln!("\x1b[2m  {}\x1b[0m", trimmed);
                        }
                    }
                }
                i += 1;
            }
        }
    }
    std::io::stderr().flush().ok();
}

/// Display a compact inline diff for short text changes.
/// Shows old text struck through in red and new text in green on the same line.
#[allow(dead_code)]
pub fn print_inline_diff(old: &str, new: &str) {
    let diff = TextDiff::from_words(old, new);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => {
                eprint!("\x1b[31;9m{}\x1b[0m", change.value());
            }
            ChangeTag::Insert => {
                eprint!("\x1b[32m{}\x1b[0m", change.value());
            }
            ChangeTag::Equal => {
                eprint!("{}", change.value());
            }
        }
    }
    eprintln!();
    std::io::stderr().flush().ok();
}

/// Return a summary of changes: lines added, removed, and changed.
pub fn diff_stats(old: &str, new: &str) -> DiffStats {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    DiffStats { added, removed }
}

/// Summary statistics for a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
}

impl std::fmt::Display for DiffStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.added > 0 && self.removed > 0 {
            write!(
                f,
                "\x1b[32m+{}\x1b[0m / \x1b[31m-{}\x1b[0m",
                self.added, self.removed
            )
        } else if self.added > 0 {
            write!(f, "\x1b[32m+{}\x1b[0m", self.added)
        } else if self.removed > 0 {
            write!(f, "\x1b[31m-{}\x1b[0m", self.removed)
        } else {
            write!(f, "no changes")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_stats_simple() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\nnew_line\n";
        let stats = diff_stats(old, new);
        assert_eq!(stats.removed, 1);
        assert_eq!(stats.added, 2);
    }

    #[test]
    fn diff_stats_no_changes() {
        let text = "same\ntext\n";
        let stats = diff_stats(text, text);
        assert_eq!(stats.added, 0);
        assert_eq!(stats.removed, 0);
    }

    #[test]
    fn diff_stats_all_new() {
        let stats = diff_stats("", "a\nb\nc\n");
        assert_eq!(stats.added, 3);
        assert_eq!(stats.removed, 0);
    }

    #[test]
    fn diff_stats_display() {
        let stats = DiffStats { added: 5, removed: 3 };
        let s = format!("{}", stats);
        assert!(s.contains("+5"));
        assert!(s.contains("-3"));
    }

    #[test]
    fn print_diff_runs_without_panic() {
        // Smoke test: word-level diff should not panic on various inputs
        print_diff("hello world\n", "hello rust\n", Some("test.rs"));
        print_diff("a\nb\nc\n", "a\nB\nc\n", None);
        print_diff("", "new\n", None);
        print_diff("old\n", "", None);
    }

    #[test]
    fn print_inline_diff_runs_without_panic() {
        print_inline_diff("hello world", "hello rust");
    }
}
