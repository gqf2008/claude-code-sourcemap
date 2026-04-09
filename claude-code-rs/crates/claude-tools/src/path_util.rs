//! Shared path resolution, validation, and output safety utilities.
//!
//! All file-accessing tools should use `resolve_path()` to prevent path
//! traversal attacks (e.g. `../../../etc/passwd`).

use std::path::{Path, PathBuf};

/// Maximum tool output size in bytes (30 KB).
pub const MAX_TOOL_OUTPUT_SIZE: usize = 30 * 1024;

/// Maximum lines to return from a tool output.
pub const MAX_TOOL_OUTPUT_LINES: usize = 2000;

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
                // Don't pop the root component (e.g. `C:\` or `/`)
                if result.parent().is_some() && result != result.ancestors().last().unwrap_or(Path::new("")) {
                    result.pop();
                }
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

// ── Tool output truncation ──────────────────────────────────────────────────

/// Truncate tool output to fit within size limits.
///
/// If the output exceeds `MAX_TOOL_OUTPUT_SIZE` bytes or `MAX_TOOL_OUTPUT_LINES` lines,
/// it is truncated with a warning appended. Returns the (possibly truncated) string.
#[must_use] 
pub fn truncate_tool_output(output: &str) -> String {
    // Line limit first (cheaper check)
    let lines: Vec<&str> = output.lines().collect();
    let line_truncated = if lines.len() > MAX_TOOL_OUTPUT_LINES {
        let kept: String = lines[..MAX_TOOL_OUTPUT_LINES].join("\n");
        format!(
            "{}\n\n… ({} lines truncated, {} total)",
            kept,
            lines.len() - MAX_TOOL_OUTPUT_LINES,
            lines.len()
        )
    } else {
        output.to_string()
    };

    // Byte limit
    if line_truncated.len() > MAX_TOOL_OUTPUT_SIZE {
        let truncated = &line_truncated[..MAX_TOOL_OUTPUT_SIZE];
        // Find last newline to avoid cutting mid-line
        let cut_point = truncated.rfind('\n').unwrap_or(MAX_TOOL_OUTPUT_SIZE);
        format!(
            "{}\n\n… (output truncated at {} bytes, {} total)",
            &line_truncated[..cut_point],
            cut_point,
            line_truncated.len()
        )
    } else {
        line_truncated
    }
}

// ── Binary file detection ───────────────────────────────────────────────────

/// Magic bytes for common binary formats.
const BINARY_SIGNATURES: &[(&[u8], &str)] = &[
    (b"\x89PNG", "PNG image"),
    (b"\xFF\xD8\xFF", "JPEG image"),
    (b"GIF8", "GIF image"),
    (b"PK\x03\x04", "ZIP archive"),
    (b"\x1F\x8B", "gzip archive"),
    (b"\x7FELF", "ELF binary"),
    (b"MZ", "Windows executable"),
    (b"\xCA\xFE\xBA\xBE", "Mach-O binary"),
    (b"%PDF", "PDF document"),
    (b"RIFF", "RIFF media"),
    (b"\x00\x00\x01\x00", "ICO image"),
    (b"SQLite format 3", "SQLite database"),
];

/// Check if file content appears to be binary.
///
/// Uses magic byte detection and NUL-byte heuristic (first 8KB).
#[must_use] 
pub fn is_binary_content(data: &[u8]) -> bool {
    // Check magic bytes
    for (sig, _) in BINARY_SIGNATURES {
        if data.len() >= sig.len() && &data[..sig.len()] == *sig {
            return true;
        }
    }

    // NUL-byte heuristic: check first 8KB for NUL bytes
    let check_len = data.len().min(8192);
    data[..check_len].contains(&0)
}

/// Get a human-readable description of a binary file.
#[must_use] 
pub fn binary_file_type(data: &[u8]) -> &'static str {
    for (sig, name) in BINARY_SIGNATURES {
        if data.len() >= sig.len() && &data[..sig.len()] == *sig {
            return name;
        }
    }
    "binary file"
}

/// Check if a file extension suggests a binary file.
#[must_use] 
pub fn is_binary_extension(path: &Path) -> bool {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);

    matches!(ext.as_deref(), Some(
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp" | "svg" |
        "mp3" | "mp4" | "wav" | "avi" | "mov" | "mkv" | "flac" | "ogg" |
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" |
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" |
        "exe" | "dll" | "so" | "dylib" | "o" | "a" | "lib" |
        "wasm" | "class" | "pyc" | "pyo" |
        "sqlite" | "db" | "sqlite3" |
        "ttf" | "otf" | "woff" | "woff2" | "eot"
    ))
}

// ── Symlink safety ──────────────────────────────────────────────────────────

/// Maximum depth for symlink resolution (prevents infinite loops).
const MAX_SYMLINK_DEPTH: u32 = 10;

/// Resolve symlinks safely with a depth limit.
///
/// Returns `Err` if the symlink chain exceeds `MAX_SYMLINK_DEPTH` or
/// if the resolved path escapes the boundary directory.
pub fn resolve_symlink_safe(path: &Path, boundary: &Path) -> anyhow::Result<PathBuf> {
    let mut current = path.to_path_buf();
    let mut depth = 0;

    loop {
        if !current.is_symlink() {
            break;
        }
        depth += 1;
        if depth > MAX_SYMLINK_DEPTH {
            anyhow::bail!(
                "Symlink chain too deep (>{}) for '{}'",
                MAX_SYMLINK_DEPTH,
                path.display()
            );
        }
        // Save parent BEFORE resolving — for relative symlinks, the base
        // must be the directory containing the current link, not the original path
        let base = current.parent().unwrap_or(Path::new(".")).to_path_buf();
        current = std::fs::read_link(&current)?;
        if current.is_relative() {
            current = base.join(&current);
        }
    }

    // Normalize and check boundary
    let normalized = normalize_path(&current);
    let boundary_normalized = normalize_path(boundary);
    if !normalized.starts_with(&boundary_normalized) {
        anyhow::bail!(
            "Symlink '{}' resolves to '{}' which is outside boundary '{}'",
            path.display(),
            normalized.display(),
            boundary_normalized.display()
        );
    }

    Ok(normalized)
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

    // ── truncate_tool_output ────────────────────────────────────────────

    #[test]
    fn truncate_small_output_unchanged() {
        let text = "hello\nworld\n";
        assert_eq!(truncate_tool_output(text), text);
    }

    #[test]
    fn truncate_by_lines() {
        let lines: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        let result = truncate_tool_output(&lines);
        assert!(result.contains("truncated"));
        assert!(result.contains("3000 total"));
        // Result should be smaller than original
        assert!(result.len() < lines.len());
    }

    #[test]
    fn truncate_by_bytes() {
        // Single very long line
        let big = "x".repeat(50_000);
        let result = truncate_tool_output(&big);
        assert!(result.contains("truncated"));
        assert!(result.len() < big.len());
    }

    #[test]
    fn truncate_exact_limit_not_truncated() {
        let lines: String = (0..MAX_TOOL_OUTPUT_LINES).map(|i| format!("{i}\n")).collect();
        if lines.len() <= MAX_TOOL_OUTPUT_SIZE {
            let result = truncate_tool_output(&lines);
            assert!(!result.contains("truncated"));
        }
    }

    // ── binary detection ────────────────────────────────────────────────

    #[test]
    fn detect_png() {
        let data = b"\x89PNG\r\n\x1a\nrest of file";
        assert!(is_binary_content(data));
        assert_eq!(binary_file_type(data), "PNG image");
    }

    #[test]
    fn detect_jpeg() {
        let data = b"\xFF\xD8\xFFstuff";
        assert!(is_binary_content(data));
        assert_eq!(binary_file_type(data), "JPEG image");
    }

    #[test]
    fn detect_elf() {
        let data = b"\x7FELFbinary";
        assert!(is_binary_content(data));
        assert_eq!(binary_file_type(data), "ELF binary");
    }

    #[test]
    fn detect_exe() {
        let data = b"MZwindows exe";
        assert!(is_binary_content(data));
        assert_eq!(binary_file_type(data), "Windows executable");
    }

    #[test]
    fn detect_nul_bytes() {
        let data = b"hello\x00world";
        assert!(is_binary_content(data));
    }

    #[test]
    fn text_not_binary() {
        let data = b"Hello, this is a normal text file.\nWith multiple lines.\n";
        assert!(!is_binary_content(data));
        assert_eq!(binary_file_type(data), "binary file"); // no match
    }

    #[test]
    fn empty_not_binary() {
        assert!(!is_binary_content(b""));
    }

    // ── binary extension ────────────────────────────────────────────────

    #[test]
    fn binary_ext_images() {
        assert!(is_binary_extension(Path::new("photo.png")));
        assert!(is_binary_extension(Path::new("image.JPG")));
        assert!(is_binary_extension(Path::new("icon.ico")));
    }

    #[test]
    fn binary_ext_archives() {
        assert!(is_binary_extension(Path::new("data.zip")));
        assert!(is_binary_extension(Path::new("archive.tar")));
        assert!(is_binary_extension(Path::new("file.gz")));
    }

    #[test]
    fn binary_ext_executables() {
        assert!(is_binary_extension(Path::new("app.exe")));
        assert!(is_binary_extension(Path::new("lib.dll")));
        assert!(is_binary_extension(Path::new("module.wasm")));
    }

    #[test]
    fn text_ext_not_binary() {
        assert!(!is_binary_extension(Path::new("code.rs")));
        assert!(!is_binary_extension(Path::new("README.md")));
        assert!(!is_binary_extension(Path::new("config.json")));
        assert!(!is_binary_extension(Path::new("style.css")));
    }

    #[test]
    fn no_ext_not_binary() {
        assert!(!is_binary_extension(Path::new("Makefile")));
        assert!(!is_binary_extension(Path::new("LICENSE")));
    }

    // ── symlink safety ──────────────────────────────────────────────────

    #[test]
    fn resolve_symlink_non_symlink() {
        let tmp = std::env::temp_dir();
        let file = tmp.join("claude_test_not_symlink.txt");
        std::fs::write(&file, "hi").unwrap();

        let result = resolve_symlink_safe(&file, &tmp);
        assert!(result.is_ok());

        let _ = std::fs::remove_file(&file);
    }
}
