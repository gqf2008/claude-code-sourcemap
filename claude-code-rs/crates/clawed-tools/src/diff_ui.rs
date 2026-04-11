//! Terminal-rendered colored diff output for file edit operations.
//!
//! Uses `similar` for unified diff computation and `syntect` for language-aware
//! syntax highlighting within changed/context lines. Outputs ANSI-colored text
//! to stderr so it does not interfere with Claude's response stream.

use similar::{ChangeTag, TextDiff};
use std::io::Write;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

const CONTEXT_LINES: usize = 3;

/// Lazy-initialized syntax highlighting resources.
struct SyntaxRes {
    ss: SyntaxSet,
    ts: ThemeSet,
}

impl SyntaxRes {
    fn get() -> &'static Self {
        static INSTANCE: OnceLock<SyntaxRes> = OnceLock::new();
        INSTANCE.get_or_init(|| SyntaxRes {
            ss: SyntaxSet::load_defaults_newlines(),
            ts: ThemeSet::load_defaults(),
        })
    }
}

/// Highlight a single line using syntect.
fn highlight_line(line: &str, hl: &mut HighlightLines, ss: &SyntaxSet) -> String {
    let line_nl = if line.ends_with('\n') {
        line.to_string()
    } else {
        format!("{line}\n")
    };
    match hl.highlight_line(&line_nl, ss) {
        Ok(ranges) => as_24_bit_terminal_escaped(&ranges, false),
        Err(_) => line.to_string(),
    }
}

/// Render a delete/insert line pair with word-level highlighting.
fn render_word_diff_pair(old_line: &str, new_line: &str) {
    let word_diff = TextDiff::from_words(old_line, new_line);

    // Deleted line with highlighted removed words
    eprint!("\x1b[31m- ");
    for change in word_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => eprint!("\x1b[41;97m{}\x1b[0m\x1b[31m", change.value()),
            ChangeTag::Equal => eprint!("{}", change.value()),
            ChangeTag::Insert => {}
        }
    }
    eprintln!("\x1b[0m");

    // Inserted line with highlighted added words
    eprint!("\x1b[32m+ ");
    for change in word_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => eprint!("\x1b[42;97m{}\x1b[0m\x1b[32m", change.value()),
            ChangeTag::Equal => eprint!("{}", change.value()),
            ChangeTag::Delete => {}
        }
    }
    eprintln!("\x1b[0m");
}

/// Resolve the syntax for a file path.
fn syntax_for_path(label: &str) -> Option<&'static SyntaxReference> {
    let res = SyntaxRes::get();
    std::path::Path::new(label)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| res.ss.find_syntax_by_extension(ext))
}

/// Print a colored unified diff between `old` and `new` to stderr.
/// `label` is typically the file path shown in the diff header.
pub fn print_diff(label: &str, old: &str, new: &str) {
    let diff = TextDiff::from_lines(old, new);

    if diff.ratio() == 1.0 {
        return;
    }

    let res = SyntaxRes::get();
    let syntax = syntax_for_path(label);
    let theme = &res.ts.themes["base16-ocean.dark"];
    let mut hl_old = syntax.map(|s| HighlightLines::new(s, theme));
    let mut hl_new = syntax.map(|s| HighlightLines::new(s, theme));

    // Header
    eprintln!("\x1b[1;34m── {label} ──\x1b[0m");

    for group in diff.grouped_ops(CONTEXT_LINES) {
        let Some(first) = group.first() else { continue };
        let old_start = first.old_range().start + 1;
        let old_len: usize = group.iter().map(|op| op.old_range().len()).sum();
        let new_start = first.new_range().start + 1;
        let new_len: usize = group.iter().map(|op| op.new_range().len()).sum();
        eprintln!(
            "\x1b[36m@@ -{old_start},{old_len} +{new_start},{new_len} @@\x1b[0m"
        );

        for op in &group {
            let changes: Vec<_> = diff.iter_changes(op).collect();
            let mut i = 0;
            while i < changes.len() {
                let change = &changes[i];
                let line = change.value();
                let trimmed = line.strip_suffix('\n').unwrap_or(line);

                match change.tag() {
                    ChangeTag::Delete => {
                        // Word-level diff if next change is Insert
                        if i + 1 < changes.len() && changes[i + 1].tag() == ChangeTag::Insert {
                            let ins_line = changes[i + 1].value();
                            let ins_trimmed = ins_line.strip_suffix('\n').unwrap_or(ins_line);
                            render_word_diff_pair(trimmed, ins_trimmed);
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
                            eprintln!("\x1b[31m- {trimmed}\x1b[0m");
                        }
                    }
                    ChangeTag::Insert => {
                        if let Some(ref mut hl) = hl_new {
                            let highlighted = highlight_line(trimmed, hl, &res.ss);
                            eprint!("\x1b[42m\x1b[97m+\x1b[0m ");
                            eprintln!("{}\x1b[0m", highlighted.trim_end());
                        } else {
                            eprintln!("\x1b[32m+ {trimmed}\x1b[0m");
                        }
                    }
                    ChangeTag::Equal => {
                        if let Some(ref mut hl) = hl_new {
                            let highlighted = highlight_line(trimmed, hl, &res.ss);
                            if let Some(ref mut hl_o) = hl_old {
                                let _ = highlight_line(trimmed, hl_o, &res.ss);
                            }
                            eprintln!("\x1b[2m  {}\x1b[0m", highlighted.trim_end());
                        } else {
                            eprintln!("\x1b[2m  {trimmed}\x1b[0m");
                        }
                    }
                }
                i += 1;
            }
        }
    }
    std::io::stderr().flush().ok();
}

/// Print a "created file" diff (all lines added) with syntax highlighting.
pub fn print_create_diff(label: &str, content: &str) {
    let res = SyntaxRes::get();
    let syntax = syntax_for_path(label);
    let theme = &res.ts.themes["base16-ocean.dark"];
    let mut hl = syntax.map(|s| HighlightLines::new(s, theme));

    eprintln!("\x1b[1;34m── {label} (new file) ──\x1b[0m");
    eprintln!(
        "\x1b[36m@@ -0,0 +1,{} @@\x1b[0m",
        content.lines().count()
    );
    for line in content.lines() {
        if let Some(ref mut h) = hl {
            let highlighted = highlight_line(line, h, &res.ss);
            eprint!("\x1b[42m\x1b[97m+\x1b[0m ");
            eprintln!("{}\x1b[0m", highlighted.trim_end());
        } else {
            eprintln!("\x1b[32m+ {line}\x1b[0m");
        }
    }
    std::io::stderr().flush().ok();
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

    #[test]
    fn syntax_highlighting_for_known_extension() {
        for ext in ["test.rs", "test.py", "test.js", "test.go", "test.c"] {
            assert!(syntax_for_path(ext).is_some(), "no syntax for {ext}");
        }
    }

    #[test]
    fn unknown_extension_gracefully_falls_back() {
        assert!(syntax_for_path("data.xyznotreal").is_none());
        // Should still print without panic
        print_diff("data.xyznotreal", "old\n", "new\n");
    }

    #[test]
    fn word_level_diff_runs_without_panic() {
        render_word_diff_pair("let x = foo(bar);", "let x = baz(bar);");
    }

    #[test]
    fn create_diff_with_syntax() {
        print_create_diff("main.py", "def hello():\n    print('hi')\n");
    }
}
