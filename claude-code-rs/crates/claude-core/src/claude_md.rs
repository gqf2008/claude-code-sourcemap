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
                        Err(e) => debug!("Cannot read @include {}: {}", resolved.display(), e),
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
    // Must start with @ and be the only thing on the line (trimmed)
    if !line.starts_with('@') {
        return None;
    }
    let path = line[1..].trim();
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

                sections.push(resolved);
            }
            Ok(_) => {}
            Err(e) => debug!("Could not read {}: {}", path.display(), e),
        }
    }

    sections.join("\n\n---\n\n")
}
