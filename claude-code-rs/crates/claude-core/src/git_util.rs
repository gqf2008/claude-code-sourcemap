//! Git utilities shared across crates.
//!
//! Provides `find_git_root()` (walks up to find `.git`) and `sanitize_path_key()`
//! (safe directory name from arbitrary path, matching TS `sanitizePath()`).

use std::path::{Path, PathBuf};

/// Maximum length of sanitized path key before hash truncation.
const MAX_SANITIZED_LENGTH: usize = 200;

/// Find the git repository root by running `git rev-parse --show-toplevel`.
///
/// Returns `None` if `cwd` is not inside a git repo or `git` is unavailable.
pub fn find_git_root(cwd: &Path) -> Option<PathBuf> {
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

/// Convert an arbitrary filesystem path into a safe directory name.
///
/// Mirrors TS `sanitizePath()`:
/// - Replace every non-alphanumeric character with `-`
/// - If result exceeds 200 chars, truncate + append `-<hash>`
///
/// # Examples
/// ```
/// use claude_core::git_util::sanitize_path_key;
/// assert_eq!(sanitize_path_key("/home/user/my-project"), "-home-user-my-project");
/// assert_eq!(sanitize_path_key("my_project"), "my-project");
/// ```
pub fn sanitize_path_key(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        sanitized
    } else {
        let hash = simple_hash(name);
        format!("{}-{}", &sanitized[..MAX_SANITIZED_LENGTH], hash)
    }
}

/// Simple string hash for deterministic path truncation suffix.
///
/// Produces a base-36 encoded hash. Matches TS `simpleHash()` behavior
/// (not cryptographic, just a stable short identifier).
fn simple_hash(s: &str) -> String {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    radix_fmt(hash)
}

/// Format u64 in base-36 (0-9a-z).
fn radix_fmt(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::with_capacity(14);
    while n > 0 {
        buf.push(CHARS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_non_alnum() {
        assert_eq!(sanitize_path_key("/home/user/project"), "-home-user-project");
        // Backslash is a single non-alnum char → single dash
        assert_eq!(sanitize_path_key("simple"), "simple");
    }

    #[test]
    fn sanitize_preserves_alphanumeric() {
        assert_eq!(sanitize_path_key("abc123"), "abc123");
        assert_eq!(sanitize_path_key("ABCxyz"), "ABCxyz");
    }

    #[test]
    fn sanitize_handles_empty() {
        assert_eq!(sanitize_path_key(""), "");
    }

    #[test]
    fn sanitize_truncates_long_path() {
        let long = "a".repeat(300);
        let result = sanitize_path_key(&long);
        assert!(result.len() < 300);
        assert!(result.starts_with(&"a".repeat(200)));
        assert!(result.contains('-')); // hash separator
    }

    #[test]
    fn sanitize_hash_is_deterministic() {
        let a = sanitize_path_key(&"x".repeat(250));
        let b = sanitize_path_key(&"x".repeat(250));
        assert_eq!(a, b);
    }

    #[test]
    fn sanitize_different_long_paths_differ() {
        let a = sanitize_path_key(&"a".repeat(250));
        let b = sanitize_path_key(&"b".repeat(250));
        assert_ne!(a, b);
    }

    #[test]
    fn radix_fmt_zero() {
        assert_eq!(radix_fmt(0), "0");
    }

    #[test]
    fn radix_fmt_small() {
        assert_eq!(radix_fmt(36), "10"); // 36 in base-36 = "10"
        assert_eq!(radix_fmt(35), "z");
    }

    #[test]
    fn find_git_root_in_repo() {
        // This test runs inside our own repo, so it should find a root
        let cwd = std::env::current_dir().unwrap();
        let root = find_git_root(&cwd);
        // Should succeed (we're in a git repo)
        assert!(root.is_some());
    }
}
