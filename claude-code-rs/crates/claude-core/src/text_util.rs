//! Shared text utilities used across crates.

use std::sync::OnceLock;

/// Cached regex for collapsing 3+ consecutive newlines into 2.
fn blank_line_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\n{3,}").expect("blank_line_regex is valid"))
}

/// Collapse runs of 3+ consecutive newlines into exactly 2 (`\n\n`).
pub fn collapse_blank_lines(text: &str) -> String {
    blank_line_regex().replace_all(text.trim(), "\n\n").to_string()
}

/// UTF-8 safe truncation: truncate to at most `max_chars` bytes on a valid
/// char boundary, appending a suffix if truncated.
pub fn truncate_utf8(text: &str, max_bytes: usize, suffix: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}{}", &text[..end], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collapse_blank_lines() {
        assert_eq!(collapse_blank_lines("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(collapse_blank_lines("a\n\nb"), "a\n\nb");
        assert_eq!(collapse_blank_lines("a\nb"), "a\nb");
    }

    #[test]
    fn test_truncate_utf8_ascii() {
        assert_eq!(truncate_utf8("hello world", 5, "..."), "hello...");
        assert_eq!(truncate_utf8("hi", 10, "..."), "hi");
    }

    #[test]
    fn test_truncate_utf8_multibyte() {
        let s = "你好世界"; // 12 bytes (3 per char)
        let result = truncate_utf8(s, 7, "…");
        // Should truncate to 6 bytes (2 chars) + suffix
        assert_eq!(result, "你好…");
    }
}
