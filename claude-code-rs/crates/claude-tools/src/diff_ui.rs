//! Terminal-rendered colored diff output for file edit operations.
//!
//! Uses the `similar` crate for unified diff computation and prints ANSI-colored
//! output to stderr so it does not interfere with Claude's response stream.

use similar::{ChangeTag, TextDiff};

// ANSI color codes
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

const CONTEXT_LINES: usize = 3;

/// Print a colored unified diff between `old` and `new` to stderr.
/// `label` is typically the file path shown in the diff header.
pub fn print_diff(label: &str, old: &str, new: &str) {
    let diff = TextDiff::from_lines(old, new);

    // Don't print anything if there are no changes
    if diff.ratio() == 1.0 {
        return;
    }

    // Header
    eprintln!("{}--- a/{}{}", RED, label, RESET);
    eprintln!("{}+++ b/{}{}", GREEN, label, RESET);

    for group in diff.grouped_ops(CONTEXT_LINES) {
        // Hunk header: @@ -old_start,old_len +new_start,new_len @@
        let Some(first) = group.first() else { continue };
        let old_start = first.old_range().start + 1;
        let old_len: usize = group.iter().map(|op| op.old_range().len()).sum();
        let new_start = first.new_range().start + 1;
        let new_len: usize = group.iter().map(|op| op.new_range().len()).sum();
        eprintln!(
            "{}@@ -{},{} +{},{} @@{}",
            CYAN, old_start, old_len, new_start, new_len, RESET
        );

        for op in &group {
            for change in diff.iter_changes(op) {
                let (prefix, color) = match change.tag() {
                    ChangeTag::Delete => ("-", RED),
                    ChangeTag::Insert => ("+", GREEN),
                    ChangeTag::Equal => (" ", DIM),
                };
                let line = change.value();
                // Suppress final newline on the last line to avoid double blank lines
                let line = line.trim_end_matches('\n');
                eprintln!("{}{}{}{}", color, prefix, line, RESET);
            }
        }
    }
}

/// Print a "created file" diff (all lines added).
pub fn print_create_diff(label: &str, content: &str) {
    eprintln!("{}+++ b/{}{}", GREEN, label, RESET);
    eprintln!("{}@@ -0,0 +1,{} @@{}", CYAN, content.lines().count(), RESET);
    for line in content.lines() {
        eprintln!("{}+{}{}", GREEN, line, RESET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_diff_no_changes() {
        print_diff("test.rs", "same", "same");
    }

    #[test]
    fn test_print_diff_simple_change() {
        print_diff("test.rs", "old\n", "new\n");
    }

    #[test]
    fn test_print_diff_multiline() {
        let old = "line1\nline2\nline3\nline4\nline5\n";
        let new = "line1\nchanged\nline3\nadded\nline4\nline5\n";
        print_diff("complex.rs", old, new);
    }

    #[test]
    fn test_print_create_diff() {
        print_create_diff("new.rs", "fn main() {}\n");
    }
}
