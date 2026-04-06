//! Shared path resolution and validation utilities.
//!
//! All file-accessing tools should use `resolve_path()` to prevent path
//! traversal attacks (e.g. `../../../etc/passwd`).

use std::path::{Path, PathBuf};

/// Resolve a user-supplied file path relative to cwd.
///
/// Returns the resolved path. Does NOT require the file to exist (for write/create operations).
/// Validates that the resolved path does not escape the project root (git root or cwd).
pub fn resolve_path(file_path: &str, cwd: &Path) -> anyhow::Result<PathBuf> {
    let p = Path::new(file_path);
    let path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };

    // Normalize the path by resolving `..` and `.` components logically
    // (without requiring the file to exist, unlike canonicalize())
    let normalized = normalize_path(&path);

    // Determine project boundary (git root or cwd)
    let boundary = find_project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let boundary_normalized = normalize_path(&boundary);

    // Check that the resolved path is within the boundary
    if !normalized.starts_with(&boundary_normalized) {
        anyhow::bail!(
            "Path '{}' is outside the project directory '{}'",
            file_path,
            boundary_normalized.display()
        );
    }

    Ok(normalized)
}

/// Normalize a path by resolving `.` and `..` components without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {} // skip `.`
            Component::ParentDir => {
                result.pop(); // go up one level
            }
            other => result.push(other),
        }
    }
    result
}

/// Find the git root directory (if inside a git repo).
fn find_project_root(cwd: &Path) -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        let p = normalize_path(Path::new("/a/b/../c/./d"));
        assert_eq!(p, PathBuf::from("/a/c/d"));
    }

    #[test]
    fn test_normalize_parent_at_root() {
        let p = normalize_path(Path::new("/a/../../b"));
        assert_eq!(p, PathBuf::from("/b"));
    }

    #[test]
    fn test_normalize_path_identity() {
        let p = normalize_path(Path::new("/a/b/c"));
        assert_eq!(p, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn test_normalize_path_current_dir() {
        let p = normalize_path(Path::new("/a/./b"));
        assert_eq!(p, PathBuf::from("/a/b"));
    }

    #[test]
    fn test_normalize_path_multiple_parents() {
        let p = normalize_path(Path::new("/a/b/c/../../d"));
        assert_eq!(p, PathBuf::from("/a/d"));
    }
}
