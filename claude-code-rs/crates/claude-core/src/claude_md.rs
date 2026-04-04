//! CLAUDE.md loader — scans standard locations and concatenates their content.
//!
//! Files are loaded in this order and joined with `---` separators:
//!   1. `~/.claude/CLAUDE.md`          (user-level defaults)
//!   2. `$CWD/CLAUDE.md`               (project root)
//!   3. `$CWD/.claude/CLAUDE.md`       (project sub-directory)

use std::path::{Path, PathBuf};
use tracing::debug;

/// Return all candidate CLAUDE.md paths in ascending priority order.
fn candidate_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".claude").join("CLAUDE.md"));
    }
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
