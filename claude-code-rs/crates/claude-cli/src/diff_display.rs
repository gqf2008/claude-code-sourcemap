//! Structured diff display with syntax highlighting.
//!
//! Renders unified diffs with red/green coloring and optional word-level highlighting.
//! Used by `/review`, permission dialogs, and file edit previews.
use similar::{ChangeTag, TextDiff};
use std::io::Write;

/// Display a unified diff between two strings with colored output.
///
/// Red (`-`) for removed lines, green (`+`) for added lines, dim for context.
/// Includes a header with the file path if provided.
pub fn print_diff(old: &str, new: &str, file_path: Option<&str>) {
    let diff = TextDiff::from_lines(old, new);

    if let Some(path) = file_path {
        eprintln!("\x1b[1;34m── {} ──\x1b[0m", path);
    }

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
            for change in diff.iter_changes(op) {
                let (sign, color) = match change.tag() {
                    ChangeTag::Delete => ('-', "\x1b[31m"),   // red
                    ChangeTag::Insert => ('+', "\x1b[32m"),   // green
                    ChangeTag::Equal => (' ', "\x1b[2m"),     // dim
                };
                let line = change.value();
                // Print without trailing newline from the value itself
                let trimmed = line.strip_suffix('\n').unwrap_or(line);
                eprintln!("{}{} {}\x1b[0m", color, sign, trimmed);
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
}
