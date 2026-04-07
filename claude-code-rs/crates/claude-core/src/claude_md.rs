//! CLAUDE.md loader — scans standard locations and concatenates their content.
//!
//! Files are loaded in priority order and joined with `---` separators:
//!   1. `~/.claude/CLAUDE.md`              (user-level defaults)
//!   2. `~/.claude/rules/*.md`             (user-level rules)
//!   3. Ancestor dirs root→cwd:
//!      - `CLAUDE.md`
//!      - `.claude/CLAUDE.md`
//!      - `.claude/rules/*.md`
//!   4. `$CWD/CLAUDE.md`, `$CWD/.claude/CLAUDE.md`, `$CWD/.claude/rules/*.md`
//!   5. Ancestor dirs root→cwd: `CLAUDE.local.md` (per-user private, not committed)
//!
//! Each file supports `@path` include directives for recursive inclusion.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Maximum depth for recursive `@include` resolution.
const MAX_INCLUDE_DEPTH: usize = 5;

/// Maximum number of lines per memory file (TS: 200).
const MAX_LINES_PER_FILE: usize = 200;

/// Maximum byte size per memory file (TS: 25KB — handles files with very long lines).
const MAX_BYTES_PER_FILE: usize = 25 * 1024;

// ── Discovery ────────────────────────────────────────────────────────────────

/// Discover all CLAUDE.md, rules/, and .local.md files in priority order.
fn discover_files(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. User-level: ~/.claude/CLAUDE.md + ~/.claude/rules/*.md
    if let Some(home) = dirs::home_dir() {
        let user_dir = home.join(".claude");
        paths.push(user_dir.join("CLAUDE.md"));
        collect_rules_dir(&user_dir.join("rules"), &mut paths);
    }

    // Detect git root for ancestor walk boundary
    let git_root = detect_git_root(cwd);
    let start = git_root.as_deref().unwrap_or(cwd);

    // 2. Ancestor directories from root→cwd (excluding cwd itself)
    let mut ancestor_dirs: Vec<PathBuf> = Vec::new();
    let mut dir = cwd.to_path_buf();
    loop {
        if dir != *cwd {
            ancestor_dirs.push(dir.clone());
        }
        if dir == *start || dir.parent().is_none() {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    ancestor_dirs.reverse(); // root → cwd order

    for dir in &ancestor_dirs {
        paths.push(dir.join("CLAUDE.md"));
        paths.push(dir.join(".claude").join("CLAUDE.md"));
        collect_rules_dir(&dir.join(".claude").join("rules"), &mut paths);
    }

    // 3. CWD-level: CLAUDE.md, .claude/CLAUDE.md, .claude/rules/*.md
    paths.push(cwd.join("CLAUDE.md"));
    paths.push(cwd.join(".claude").join("CLAUDE.md"));
    collect_rules_dir(&cwd.join(".claude").join("rules"), &mut paths);

    // 4. CLAUDE.local.md walk (root→cwd, higher priority)
    for dir in &ancestor_dirs {
        paths.push(dir.join("CLAUDE.local.md"));
    }
    paths.push(cwd.join("CLAUDE.local.md"));

    paths
}

/// Collect all `.md` files from a rules directory (non-recursive).
fn collect_rules_dir(rules_dir: &Path, paths: &mut Vec<PathBuf>) {
    if !rules_dir.is_dir() {
        return;
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(rules_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();
    entries.sort(); // deterministic order
    paths.extend(entries);
}

/// Detect git repository root directory.
fn detect_git_root(cwd: &Path) -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| PathBuf::from(s.trim()))
            } else {
                None
            }
        })
}

// ── @include resolution ──────────────────────────────────────────────────────

/// Resolve `@path` include directives in content.
///
/// Matches `@./relative/path`, `@~/home/path`, `@/absolute/path` at the
/// start of a line or after whitespace. Skips code blocks.
fn resolve_includes(content: &str, base_dir: &Path, depth: usize, visited: &mut HashSet<PathBuf>) -> String {
    if depth >= MAX_INCLUDE_DEPTH {
        return content.to_string();
    }

    let mut result = String::with_capacity(content.len());
    let mut in_code_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Track fenced code blocks
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if in_code_block {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // Check for @include pattern: line starts with @ or has @path after whitespace
        if let Some(include_path) = extract_include_path(trimmed) {
            let resolved = resolve_include_target(&include_path, base_dir);
            if let Some(resolved) = resolved {
                if visited.contains(&resolved) {
                    // Circular reference — skip
                    debug!("Skipping circular @include: {}", resolved.display());
                    result.push_str(line);
                    result.push('\n');
                    continue;
                }
                if resolved.exists() && resolved.is_file() {
                    match std::fs::read_to_string(&resolved) {
                        Ok(included) => {
                            visited.insert(resolved.clone());
                            let include_dir = resolved.parent().unwrap_or(base_dir);
                            let processed = resolve_includes(&included, include_dir, depth + 1, visited);
                            result.push_str(&processed);
                            if !processed.ends_with('\n') {
                                result.push('\n');
                            }
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!("Cannot read @include {}: {}", resolved.display(), e);
                        }
                    }
                }
            }
        }

        result.push_str(line);
        result.push('\n');
    }

    // Remove trailing newline added by line iteration
    if result.ends_with('\n') && !content.ends_with('\n') {
        result.pop();
    }

    result
}

/// Extract an include path from a line like `@./path` or `@~/path`.
fn extract_include_path(line: &str) -> Option<String> {
    // Must start with @ followed immediately by a path character (no space)
    if !line.starts_with('@') || line.len() < 2 {
        return None;
    }
    let after_at = &line[1..];
    // @ must be immediately followed by a path character (no space)
    if after_at.starts_with(' ') || after_at.starts_with('\t') {
        return None;
    }
    let path = after_at.trim();
    if path.is_empty() || path.contains(' ') {
        return None;
    }
    // Strip fragment identifier (#heading)
    let path = path.split('#').next().unwrap_or(path);
    Some(path.to_string())
}

/// Resolve an include target path relative to base_dir.
fn resolve_include_target(path: &str, base_dir: &Path) -> Option<PathBuf> {
    if path.starts_with("~/") || path.starts_with("~\\") {
        dirs::home_dir().map(|home| home.join(&path[2..]))
    } else if Path::new(path).is_absolute() {
        Some(PathBuf::from(path))
    } else {
        // Relative to the including file's directory
        Some(base_dir.join(path))
    }
}

// ── Content transformations ──────────────────────────────────────────────────

/// Strip block-level HTML comments (`<!-- ... -->`) from content.
/// Inline comments (on a line with other text) are left intact.
fn strip_html_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut in_comment = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if in_comment {
            // Look for end of comment
            if let Some(pos) = trimmed.find("-->") {
                // Check if there's content after the closing tag
                let after = trimmed[pos + 3..].trim();
                if !after.is_empty() {
                    result.push_str(after);
                    result.push('\n');
                }
                in_comment = false;
            }
            // Skip lines inside comment
            continue;
        }

        // Check for block-level comment start (line starts with <!--)
        if let Some(inner) = trimmed.strip_prefix("<!--") {
            if let Some(pos) = inner.find("-->") {
                // Single-line comment: <!-- ... -->
                let after = inner[pos + 3..].trim();
                if !after.is_empty() {
                    result.push_str(after);
                    result.push('\n');
                }
                continue;
            }
            // Multi-line comment starts
            in_comment = true;
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    // Remove trailing newline added by iteration
    if result.ends_with('\n') && !content.ends_with('\n') {
        result.pop();
    }

    result
}

/// Truncate content to `MAX_LINES_PER_FILE` lines and `MAX_BYTES_PER_FILE` bytes.
/// Returns the truncated content with a notice if truncation occurred.
fn truncate_content(content: &str, path: &Path) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let byte_len = content.len();

    let line_truncated = lines.len() > MAX_LINES_PER_FILE;
    let byte_truncated = byte_len > MAX_BYTES_PER_FILE;

    if !line_truncated && !byte_truncated {
        return content.to_string();
    }

    let truncated = if line_truncated {
        lines[..MAX_LINES_PER_FILE].join("\n")
    } else {
        // Byte-truncate at a char boundary
        let mut end = MAX_BYTES_PER_FILE;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        // Find last newline to avoid cutting mid-line
        if let Some(last_nl) = content[..end].rfind('\n') {
            content[..last_nl].to_string()
        } else {
            content[..end].to_string()
        }
    };

    debug!(
        "Truncated {} ({} lines, {} bytes → {} bytes)",
        path.display(),
        lines.len(),
        byte_len,
        truncated.len()
    );

    format!(
        "{}\n\n[… truncated — file exceeds {} limit]",
        truncated,
        if line_truncated { "200-line" } else { "25KB" }
    )
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Load all CLAUDE.md files and join them with a separator.
/// Returns an empty string if none exist.
///
/// Supports:
/// - Hierarchical discovery (user → ancestors → cwd → local)
/// - `.claude/rules/*.md` directories
/// - `CLAUDE.local.md` per-user private files
/// - `@path` recursive include directives
pub fn load_claude_md(cwd: &Path) -> String {
    let mut sections: Vec<String> = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();

    for path in discover_files(cwd) {
        if !path.exists() || !path.is_file() {
            continue;
        }
        // Deduplicate (same file can appear in multiple discovery passes)
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen_paths.insert(canonical) {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                let label = if path.file_name().map(|n| n == "CLAUDE.local.md").unwrap_or(false) {
                    "CLAUDE.local.md"
                } else if path.components().any(|c| c.as_os_str() == "rules") {
                    "rules"
                } else {
                    "CLAUDE.md"
                };
                debug!("Loaded {} from {}", label, path.display());

                // Resolve @includes
                let base_dir = path.parent().unwrap_or(cwd);
                let mut visited = HashSet::new();
                visited.insert(path.clone());
                let resolved = resolve_includes(content.trim(), base_dir, 0, &mut visited);

                // Strip HTML comments and truncate
                let cleaned = strip_html_comments(&resolved);
                let final_content = truncate_content(&cleaned, &path);

                sections.push(final_content);
            }
            Ok(_) => {}
            Err(e) => debug!("Could not read {}: {}", path.display(), e),
        }
    }

    sections.join("\n\n---\n\n")
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_comments_single_line() {
        let input = "before\n<!-- this is a comment -->\nafter";
        let result = strip_html_comments(input);
        assert_eq!(result, "before\nafter");
    }

    #[test]
    fn test_strip_html_comments_multi_line() {
        let input = "before\n<!--\nmulti\nline\ncomment\n-->\nafter";
        let result = strip_html_comments(input);
        assert_eq!(result, "before\nafter");
    }

    #[test]
    fn test_strip_html_comments_preserves_inline() {
        let input = "some text <!-- inline --> more text";
        let result = strip_html_comments(input);
        // Inline comment on a non-block line is preserved
        assert!(result.contains("some text"));
    }

    #[test]
    fn test_truncate_content_within_limits() {
        let content = "line 1\nline 2\nline 3\n";
        let result = truncate_content(content, Path::new("test.md"));
        assert_eq!(result, content);
    }

    #[test]
    fn test_truncate_content_exceeds_lines() {
        let lines: Vec<String> = (1..=250).map(|i| format!("line {}", i)).collect();
        let content = lines.join("\n");
        let result = truncate_content(&content, Path::new("test.md"));

        assert!(result.contains("line 1"));
        assert!(result.contains("line 200"));
        assert!(!result.contains("line 201"));
        assert!(result.contains("[… truncated — file exceeds 200-line limit]"));
    }

    #[test]
    fn test_truncate_content_exceeds_bytes() {
        // Create content within line limit but exceeding 25KB
        let long_line = "x".repeat(5000);
        let lines: Vec<String> = (0..10).map(|_| long_line.clone()).collect();
        let content = lines.join("\n"); // 10 lines, ~50KB

        let result = truncate_content(&content, Path::new("test.md"));
        assert!(result.len() < 30_000); // truncated + notice
        assert!(result.contains("[… truncated — file exceeds 25KB limit]"));
    }

    #[test]
    fn test_extract_include_path() {
        assert_eq!(extract_include_path("@./foo.md"), Some("./foo.md".into()));
        assert_eq!(extract_include_path("@~/bar.md"), Some("~/bar.md".into()));
        assert_eq!(extract_include_path("@/abs/path"), Some("/abs/path".into()));
        assert_eq!(extract_include_path("@path#heading"), Some("path".into()));
        assert_eq!(extract_include_path("not an include"), None);
        assert_eq!(extract_include_path("@ space"), None);
        assert_eq!(extract_include_path("@"), None);
    }

    #[test]
    fn test_resolve_includes_skips_code_blocks() {
        let content = "before\n```\n@./should-not-include.md\n```\nafter";
        let mut visited = HashSet::new();
        let result = resolve_includes(content, Path::new("."), 0, &mut visited);
        assert!(result.contains("@./should-not-include.md")); // preserved, not resolved
    }
}
