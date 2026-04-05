//! CLAUDE.md loader — scans standard locations and concatenates their content.
//!
//! Files are loaded in this order and joined with `---` separators:
//!   1. `~/.claude/CLAUDE.md`          (user-level defaults)
//!   2. `$CWD/CLAUDE.md`               (project root)
//!   3. `$CWD/.claude/CLAUDE.md`       (project sub-directory)

use std::path::{Path, PathBuf};
use tracing::debug;

/// Return all candidate CLAUDE.md paths in ascending priority order.
/// Walks from cwd up to git root (or filesystem root) checking each level.
fn candidate_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. User-level: ~/.claude/CLAUDE.md
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".claude").join("CLAUDE.md"));
    }

    // 2. Walk from git root (or filesystem root) down to cwd
    //    This finds CLAUDE.md at every ancestor level.
    let git_root = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok().map(|s| PathBuf::from(s.trim()))
            } else {
                None
            }
        });

    let start = git_root.as_deref().unwrap_or(cwd);

    // Collect ancestor CLAUDE.md files from root to cwd
    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut dir = cwd.to_path_buf();
    loop {
        if dir != cwd.to_path_buf() {
            // Ancestor (not cwd itself) — only CLAUDE.md
            ancestors.push(dir.join("CLAUDE.md"));
        }
        if dir == start.to_path_buf() || dir.parent().is_none() {
            break;
        }
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        } else {
            break;
        }
    }
    // Add ancestors in root→cwd order
    ancestors.reverse();
    paths.extend(ancestors);

    // 3. CWD-level: both CLAUDE.md and .claude/CLAUDE.md
    paths.push(cwd.join("CLAUDE.md"));
    paths.push(cwd.join(".claude").join("CLAUDE.md"));

    paths
}

/// Load all CLAUDE.md files and join them with a separator.
/// Returns an empty string if none exist.
pub fn load_claude_md(cwd: &Path) -> String {
    let mut sections: Vec<String> = Vec::new();

    for path in candidate_paths(cwd) {
        if !path.exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                debug!("Loaded CLAUDE.md from {}", path.display());
                sections.push(content.trim().to_string());
            }
            Ok(_) => {}
            Err(e) => debug!("Could not read {}: {}", path.display(), e),
        }
    }

    sections.join("\n\n---\n\n")
}
