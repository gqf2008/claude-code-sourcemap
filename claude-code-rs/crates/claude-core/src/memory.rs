//! Memory system — mirrors claude-code's `~/.claude/memory/` file-based memory.
//!
//! # Design (aligned with original TypeScript)
//!
//! Memory files are plain `.md` files living under:
//!   - `~/.claude/memory/`           (user-global private memories)
//!   - `<project>/.claude/memory/`   (project-scoped memories)
//!
//! Each file **may** start with a YAML frontmatter block (between `---` markers)
//! containing:
//!   - `type:` one of `user | feedback | project | reference`
//!   - `description:` short one-liner shown in the manifest
//!
//! ## Injection strategy
//!
//! `load_memories_for_prompt()` returns a formatted block that is prepended to
//! the system prompt (same approach as CLAUDE.md injection).  For context
//! efficiency we include:
//!   1. A compact manifest (one line per file) so Claude knows what's available.
//!   2. The full content of each file (up to `MAX_MEMORY_BYTES` per file,
//!      `MAX_TOTAL_BYTES` total).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tracing::warn;

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_MEMORY_FILES: usize = 200;
const MAX_MEMORY_BYTES_PER_FILE: usize = 10_000;
const MAX_TOTAL_MEMORY_BYTES: usize = 100_000;

// ── Memory types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }
}

// ── Memory header (frontmatter metadata) ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryHeader {
    pub filename: String,
    pub file_path: PathBuf,
    pub mtime: SystemTime,
    pub description: Option<String>,
    pub memory_type: Option<MemoryType>,
}

// ── Memory entry (header + content) ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub header: MemoryHeader,
    /// Body text after the frontmatter (possibly truncated).
    pub content: String,
    pub truncated: bool,
}

// ── Frontmatter parsing ──────────────────────────────────────────────────────

/// Extract YAML frontmatter from `---\n...\n---` at the start of a file.
/// Returns `(frontmatter_lines, body)`.
fn parse_frontmatter(text: &str) -> (Vec<String>, &str) {
    let Some(rest) = text.strip_prefix("---") else {
        return (Vec::new(), text);
    };
    // Accept `---\n` or `---\r\n`
    let rest = rest.trim_start_matches('\n').trim_start_matches('\r');
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), text);
    };
    let fm = &rest[..end];
    let body_start = end + 4; // skip `\n---`
    let body = if body_start <= rest.len() {
        rest[body_start..].trim_start_matches('\n').trim_start_matches('\r')
    } else {
        ""
    };
    let lines: Vec<String> = fm.lines().map(|l| l.to_string()).collect();
    (lines, body)
}

/// Parse a simple YAML key: value line.
fn parse_yaml_kv(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once(':')?;
    Some((k.trim(), v.trim()))
}

fn parse_header_from_frontmatter(lines: &[String]) -> (Option<MemoryType>, Option<String>) {
    let mut mem_type = None;
    let mut description = None;
    for line in lines {
        if let Some((k, v)) = parse_yaml_kv(line) {
            match k {
                "type" => mem_type = MemoryType::from_str(v),
                "description" => description = Some(v.to_string()),
                _ => {}
            }
        }
    }
    (mem_type, description)
}

// ── Directory scanning ───────────────────────────────────────────────────────

/// Scan a directory for `*.md` files (excluding `MEMORY.md` index files).
/// Returns headers sorted newest-first, capped at `MAX_MEMORY_FILES`.
pub fn scan_memory_dir(dir: &Path) -> Vec<MemoryHeader> {
    let mut headers = Vec::new();

    let walk = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|x| x == "md")
                && e.file_name() != "MEMORY.md"
        });

    for entry in walk {
        let path = entry.path().to_path_buf();
        let filename = path
            .strip_prefix(dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.file_name().unwrap_or_default().to_string_lossy().to_string());

        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        // Read first 30 lines for frontmatter only
        let preview = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let first_30: String = preview.lines().take(30).collect::<Vec<_>>().join("\n");
        let (fm_lines, _) = parse_frontmatter(&first_30);
        let (mem_type, description) = parse_header_from_frontmatter(&fm_lines);

        headers.push(MemoryHeader {
            filename,
            file_path: path,
            mtime,
            description,
            memory_type: mem_type,
        });
    }

    headers.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    headers.truncate(MAX_MEMORY_FILES);
    headers
}

/// Find all memory directories to scan:
///   - `~/.claude/memory/`
///   - `<cwd>/.claude/memory/`
pub fn memory_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("memory");
        if p.exists() {
            dirs.push(p);
        }
    }
    let project = cwd.join(".claude").join("memory");
    if project.exists() {
        dirs.push(project);
    }
    dirs
}

// ── Reading memory content ───────────────────────────────────────────────────

/// Read the body of a memory file (after frontmatter), truncated to limit.
fn read_memory_body(path: &Path) -> (String, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            warn!("Failed to read memory file {:?}: {}", path, e);
            return (String::new(), false);
        }
    };
    let (_, body) = parse_frontmatter(&text);
    if body.len() > MAX_MEMORY_BYTES_PER_FILE {
        // Find a valid UTF-8 char boundary at or before the limit
        let mut end = MAX_MEMORY_BYTES_PER_FILE;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        (body[..end].to_string(), true)
    } else {
        (body.to_string(), false)
    }
}

// ── Human-readable age ───────────────────────────────────────────────────────

fn human_age(mtime: SystemTime) -> String {
    let elapsed = mtime.elapsed().unwrap_or_default();
    let secs = elapsed.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hr ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Load all available memories and format them as a block for injection into
/// the system prompt.  Returns `None` if no memory files are found.
pub fn load_memories_for_prompt(cwd: &Path) -> Option<String> {
    let dirs = memory_dirs(cwd);
    if dirs.is_empty() {
        return None;
    }

    let mut all_headers: Vec<MemoryHeader> = Vec::new();
    for dir in &dirs {
        all_headers.extend(scan_memory_dir(dir));
    }
    // Re-sort globally and cap
    all_headers.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all_headers.truncate(MAX_MEMORY_FILES);

    if all_headers.is_empty() {
        return None;
    }

    let mut result = String::from("<memory>\n");
    result.push_str("The following memory files provide relevant context:\n\n");

    // Manifest section
    result.push_str("## Memory Files\n");
    for h in &all_headers {
        let tag = h
            .memory_type
            .as_ref()
            .map(|t| format!("[{}] ", t.as_str()))
            .unwrap_or_default();
        let age = human_age(h.mtime);
        if let Some(ref desc) = h.description {
            result.push_str(&format!("- {}{} ({}): {}\n", tag, h.filename, age, desc));
        } else {
            result.push_str(&format!("- {}{} ({})\n", tag, h.filename, age));
        }
    }

    // Content section
    let mut total_bytes = 0usize;
    result.push_str("\n## Memory Contents\n\n");

    for h in &all_headers {
        if total_bytes >= MAX_TOTAL_MEMORY_BYTES {
            result.push_str("\n> Additional memory files were omitted (context budget exceeded).\n");
            break;
        }

        let age = human_age(h.mtime);
        let header_line = format!("### Memory (saved {}): {}\n\n", age, h.filename);
        let (body, truncated) = read_memory_body(&h.file_path);

        result.push_str(&header_line);
        result.push_str(&body);
        if truncated {
            result.push_str(&format!(
                "\n\n> This memory file was truncated (>{} bytes). Use FileRead to view the full file.\n",
                MAX_MEMORY_BYTES_PER_FILE
            ));
        }
        result.push('\n');

        total_bytes += body.len();
    }

    result.push_str("</memory>\n");
    Some(result)
}

/// List memory headers (for `/memory list` CLI command).
pub fn list_memory_files(cwd: &Path) -> Vec<MemoryHeader> {
    let dirs = memory_dirs(cwd);
    let mut all: Vec<MemoryHeader> = dirs.iter().flat_map(|d| scan_memory_dir(d)).collect();
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all
}

/// Return the primary user memory directory (creates it if missing).
pub fn ensure_user_memory_dir() -> anyhow::Result<PathBuf> {
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot locate home directory"))?
        .join(".claude")
        .join("memory");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
