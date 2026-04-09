//! Skills — reusable prompt templates loaded from `.claude/skills/*.md`.
//!
//! A skill file is a Markdown document with an optional YAML frontmatter block:
//!
//! ```markdown
//! ---
//! description: "Security code reviewer"
//! allowed_tools: [FileRead, Grep, Bash]
//! model: "claude-opus-4-20250514"
//! ---
//! You are an expert security reviewer.  Analyse the provided code for
//! vulnerabilities and suggest fixes.
//! ```
//!
//! Skills are loaded from `.claude/skills/` at every directory level from
//! `$CWD` up to `$HOME` (matching the TS `getProjectDirsUpToHome` behavior).
//! Results are memoized per `cwd`; call [`clear_skill_cache`] to refresh.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tracing::debug;

/// Lock a std::sync::Mutex, recovering gracefully from poisoning.
/// Global caches use simple data (HashMap/HashSet) that remain valid
/// even after a panic, so recovering via `into_inner()` is safe here.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Identifier derived from filename (lowercase, spaces → `-`).
    pub name: String,
    /// Optional display name override (from `name:` frontmatter).
    pub display_name: Option<String>,
    /// Human-readable description (from frontmatter or filename).
    pub description: String,
    /// System-prompt body (everything after the frontmatter).
    pub system_prompt: String,
    /// Tool whitelist — empty means "all tools allowed".
    pub allowed_tools: Vec<String>,
    /// Optional model override.
    pub model: Option<String>,
    /// Hint shown when the model wants to use the skill (e.g. "when_to_use").
    pub when_to_use: Option<String>,
    /// Glob patterns that trigger conditional activation.
    pub paths: Vec<String>,
    /// Named argument placeholders (e.g. `["file", "language"]`).
    pub argument_names: Vec<String>,
    /// Argument hint shown in help (e.g. `"<file> [language]"`).
    pub argument_hint: Option<String>,
    /// Version string for the skill.
    pub version: Option<String>,
    /// Execution context: `Some("fork")` runs in a forked sub-agent.
    pub context: Option<String>,
    /// Agent type hint (e.g. "explore", "task").
    pub agent: Option<String>,
    /// Effort level override.
    pub effort: Option<String>,
    /// Whether the user can invoke this skill directly (default true).
    pub user_invocable: bool,
    /// Whether model invocation is disabled (default false).
    pub disable_model_invocation: bool,
    /// Source directory for directory-format skills (for `${CLAUDE_SKILL_DIR}`).
    pub skill_root: Option<PathBuf>,
}

// ── Cache ────────────────────────────────────────────────────────────────────

fn cache() -> &'static Mutex<HashMap<PathBuf, Vec<SkillEntry>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<SkillEntry>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Conditional skills (have `paths` frontmatter) waiting for activation.
fn conditional_cache() -> &'static Mutex<HashMap<String, SkillEntry>> {
    static COND: OnceLock<Mutex<HashMap<String, SkillEntry>>> = OnceLock::new();
    COND.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Names of skills that have been activated (survives cache clears within a session).
fn activated_names() -> &'static Mutex<HashSet<String>> {
    static ACTIVATED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVATED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Dynamic skills discovered at runtime from nested `.claude/skills/` dirs.
fn dynamic_skills() -> &'static Mutex<HashMap<String, SkillEntry>> {
    static DYN: OnceLock<Mutex<HashMap<String, SkillEntry>>> = OnceLock::new();
    DYN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Directories already scanned for dynamic skill discovery (avoids re-stat).
fn discovered_dirs() -> &'static Mutex<HashSet<PathBuf>> {
    static DIRS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    DIRS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Get skills with memoization (cached by canonical `cwd`).
/// First call per `cwd` scans disk; subsequent calls return the cached list.
/// Includes dynamically activated conditional skills.
/// Use [`clear_skill_cache`] (e.g. on `/reload-context`) to force a rescan.
pub fn get_skills(cwd: &Path) -> Vec<SkillEntry> {
    let key = cwd.to_path_buf();
    let mut result;
    {
        let map = lock_or_recover(cache());
        if let Some(cached) = map.get(&key) {
            result = cached.clone();
        } else {
            drop(map); // Release lock during I/O
            let skills = load_skills(cwd);
            let mut map = lock_or_recover(cache());
            result = map.entry(key).or_insert(skills).clone();
        }
    }

    // Merge in dynamic skills (discovered from nested dirs + activated conditional)
    let dyn_snapshot: Vec<SkillEntry> = lock_or_recover(dynamic_skills()).values().cloned().collect();
    let mut seen: HashSet<String> = result.iter().map(|s| s.name.clone()).collect();
    for skill in dyn_snapshot {
        if !seen.contains(&skill.name) {
            seen.insert(skill.name.clone());
            result.push(skill);
        }
    }

    result
}

/// Invalidate the skill cache so the next [`get_skills`] call rescans disk.
/// Does NOT clear activated conditional skills (they persist across reloads, like TS).
pub fn clear_skill_cache() {
    lock_or_recover(cache()).clear();
}

/// Full reset: clears all caches including conditional/dynamic (for testing).
pub fn clear_all_skill_state() {
    lock_or_recover(cache()).clear();
    lock_or_recover(conditional_cache()).clear();
    lock_or_recover(activated_names()).clear();
    lock_or_recover(dynamic_skills()).clear();
    lock_or_recover(discovered_dirs()).clear();
}

// ── Directory discovery ──────────────────────────────────────────────────────

/// Collect `.claude/skills/` directories from `cwd` up to `$HOME`.
///
/// Matches the TS `getProjectDirsUpToHome('skills', cwd)` + user dir behavior:
/// project-local skills shadow parent/user skills via the name-dedup in
/// [`load_skills_from_dirs`].
fn skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let mut dirs = Vec::new();
    let mut current = Some(cwd.to_path_buf());

    while let Some(dir) = current {
        dirs.push(dir.join(".claude").join("skills"));
        // Stop after including $HOME
        if home.as_ref() == Some(&dir) {
            break;
        }
        let parent = dir.parent().map(|p| p.to_path_buf());
        // Stop at filesystem root (parent == self)
        if parent.as_ref() == Some(&dir) || parent.is_none() {
            break;
        }
        current = parent;
    }

    // If cwd is not under $HOME, still include the user skills dir
    if let Some(ref home) = home {
        let home_skills = home.join(".claude").join("skills");
        if !dirs.contains(&home_skills) {
            dirs.push(home_skills);
        }
    }
    dirs
}

/// Load all skills from standard locations (uncached); project skills shadow user skills.
/// Skills with `paths` frontmatter are stored in [`conditional_cache`] instead.
pub fn load_skills(cwd: &Path) -> Vec<SkillEntry> {
    let all = load_skills_from_dirs(&skill_dirs(cwd));
    let mut regular = Vec::new();
    let activated_snapshot: std::collections::HashSet<String> = lock_or_recover(activated_names()).clone();

    let mut conditional_entries = Vec::new();
    let mut dynamic_entries = Vec::new();

    for skill in all {
        if !skill.paths.is_empty() {
            if activated_snapshot.contains(&skill.name) {
                dynamic_entries.push(skill);
            } else {
                conditional_entries.push(skill);
            }
        } else {
            regular.push(skill);
        }
    }

    {
        let mut cond = lock_or_recover(conditional_cache());
        for skill in conditional_entries {
            cond.insert(skill.name.clone(), skill);
        }
    }
    {
        let mut dyn_map = lock_or_recover(dynamic_skills());
        for skill in dynamic_entries {
            dyn_map.insert(skill.name.clone(), skill);
        }
    }

    regular
}

/// Load skills from an explicit list of directories (for testing).
fn load_skills_from_dirs(dirs: &[PathBuf]) -> Vec<SkillEntry> {
    let mut skills: Vec<SkillEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                debug!("Cannot read skills dir {}: {}", dir.display(), e);
                continue;
            }
        };

        for entry in rd.flatten() {
            let path = entry.path();
            let ft = entry.file_type();

            // Format 1: skill-name/SKILL.md (directory or symlink containing SKILL.md)
            if ft.map(|t| t.is_dir() || t.is_symlink()).unwrap_or(false) {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase()
                        .replace(' ', "-");
                    if !name.is_empty() && !seen.contains(&name) {
                        if let Some(skill) = parse_skill_file(&skill_md, &name) {
                            debug!("Loaded skill '{}' from {}", name, skill_md.display());
                            seen.insert(name);
                            skills.push(skill);
                        }
                    }
                }
                continue;
            }

            // Format 2: skill-name.md (legacy flat file)
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase()
                .replace(' ', "-");

            if name.is_empty() || seen.contains(&name) {
                continue;
            }

            if let Some(skill) = parse_skill_file(&path, &name) {
                debug!("Loaded skill '{}' from {}", name, path.display());
                seen.insert(name);
                skills.push(skill);
            }
        }
    }

    skills
}

fn parse_skill_file(path: &Path, name: &str) -> Option<SkillEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let (fm, body) = split_frontmatter(&content);

    let fm_str = fm.as_deref();

    let description = fm_str
        .and_then(|f| extract_string(f, "description"))
        .unwrap_or_else(|| name.replace('-', " "));

    let allowed_tools = fm_str
        .and_then(|f| {
            extract_list(f, "allowed-tools").or_else(|| extract_list(f, "allowed_tools"))
        })
        .unwrap_or_default();

    let model = fm_str.and_then(|f| extract_string(f, "model"));
    let display_name = fm_str.and_then(|f| extract_string(f, "name"));
    let when_to_use = fm_str.and_then(|f| extract_string(f, "when_to_use"));
    let paths = fm_str
        .and_then(|f| extract_list(f, "paths"))
        .unwrap_or_default();
    let argument_names = fm_str
        .and_then(|f| extract_list(f, "arguments"))
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let argument_hint = fm_str.and_then(|f| extract_string(f, "argument-hint"));
    let version = fm_str.and_then(|f| extract_string(f, "version"));
    let context = fm_str.and_then(|f| extract_string(f, "context"));
    let agent = fm_str.and_then(|f| extract_string(f, "agent"));
    let effort = fm_str.and_then(|f| extract_string(f, "effort"));

    let user_invocable = fm_str
        .and_then(|f| extract_string(f, "user-invocable"))
        .map(|v| v != "false")
        .unwrap_or(true);

    let disable_model_invocation = fm_str
        .and_then(|f| extract_string(f, "disable-model-invocation"))
        .map(|v| v == "true")
        .unwrap_or(false);

    // For directory-format skills, use the parent of SKILL.md
    let skill_root = if path.file_name().and_then(|f| f.to_str()) == Some("SKILL.md") {
        path.parent().map(|p| p.to_path_buf())
    } else {
        None
    };

    Some(SkillEntry {
        name: name.to_string(),
        display_name,
        description,
        system_prompt: body.trim().to_string(),
        allowed_tools,
        model,
        when_to_use,
        paths,
        argument_names,
        argument_hint,
        version,
        context,
        agent,
        effort,
        user_invocable,
        disable_model_invocation,
        skill_root,
    })
}

/// Split `---\n<yaml>\n---\n<body>` → `(Some(yaml), body)`.
/// Handles both LF and CRLF line endings.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    // Normalize CRLF → LF for reliable parsing on Windows
    let normalized = content.replace("\r\n", "\n");
    let s = normalized.trim_start();
    if !s.starts_with("---") {
        return (None, s.to_string());
    }
    let rest = s[3..].trim_start_matches('\n');
    if let Some(end) = rest.find("\n---") {
        let yaml = rest[..end].to_string();
        let body = rest[end + 4..].trim_start_matches('\n').to_string();
        (Some(yaml), body)
    } else {
        (None, s.to_string())
    }
}

/// Extract a scalar string from simplistic YAML (`key: value`).
fn extract_string(yaml: &str, key: &str) -> Option<String> {
    for line in yaml.lines() {
        if let Some(rest) = line.trim().strip_prefix(&format!("{}:", key)) {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Extract a list from simplistic YAML — supports inline `[A, B]` and block `- A` styles.
fn extract_list(yaml: &str, key: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = yaml.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.trim().strip_prefix(&format!("{}:", key)) {
            let rest = rest.trim();
            // Inline: [A, B, C]
            if rest.starts_with('[') {
                let inner = rest.trim_matches(|c| c == '[' || c == ']');
                let items = inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Some(items);
            }
            // Block list
            if rest.is_empty() {
                let items: Vec<String> = lines[i + 1..]
                    .iter()
                    .take_while(|l| l.trim().starts_with("- "))
                    .filter_map(|l| {
                        l.trim()
                            .strip_prefix("- ")
                            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    })
                    .collect();
                if !items.is_empty() {
                    return Some(items);
                }
            }
        }
    }
    None
}

// ── Conditional skill activation ─────────────────────────────────────────────

/// Activate conditional skills whose `paths` patterns match the given file paths.
/// Uses gitignore-style glob matching. Activated skills are moved to
/// the dynamic skills map, making them available to the model.
///
/// Returns the names of newly activated skills.
pub fn activate_conditional_skills(file_paths: &[&str], cwd: &Path) -> Vec<String> {
    let mut cond = lock_or_recover(conditional_cache());
    if cond.is_empty() {
        return vec![];
    }

    let mut activated = Vec::new();
    let mut to_remove = Vec::new();

    for (name, skill) in cond.iter() {
        if skill.paths.is_empty() {
            continue;
        }
        for fp in file_paths {
            let rel = make_relative(fp, cwd);
            if rel.is_empty() || rel.starts_with("..") {
                continue;
            }
            if matches_any_pattern(&rel, &skill.paths) {
                to_remove.push(name.clone());
                activated.push(name.clone());
                debug!("[skills] Activated conditional skill '{}' (matched: {})", name, rel);
                break;
            }
        }
    }

    let removed: Vec<(String, SkillEntry)> = to_remove
        .iter()
        .filter_map(|name| cond.remove(name).map(|s| (name.clone(), s)))
        .collect();
    drop(cond);

    let mut dyn_skills = lock_or_recover(dynamic_skills());
    let mut act_names = lock_or_recover(activated_names());
    for (name, skill) in removed {
        dyn_skills.insert(name.clone(), skill);
        act_names.insert(name);
    }

    activated
}

/// Number of pending conditional skills (for debugging/testing).
pub fn conditional_skill_count() -> usize {
    lock_or_recover(conditional_cache()).len()
}

/// Compute a relative path from `cwd` to `path`, using forward slashes.
fn make_relative(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        if let Ok(rel) = p.strip_prefix(cwd) {
            return rel.to_string_lossy().replace('\\', "/");
        }
        return String::new(); // Outside cwd
    }
    path.replace('\\', "/")
}

/// Check if `rel_path` matches any of the given glob patterns.
/// Uses simple gitignore-style matching: `*.rs` matches any `.rs` file,
/// `src/**` matches anything under `src/`.
fn matches_any_pattern(rel_path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_match(pattern, rel_path) {
            return true;
        }
    }
    false
}

/// Minimal glob matching (supports `*`, `**`, `?`).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let txt = text.replace('\\', "/");
    let pat_parts: Vec<&str> = pat.split('/').collect();
    let txt_parts: Vec<&str> = txt.split('/').collect();
    glob_match_parts(&pat_parts, &txt_parts)
}

/// Segment-level glob matching. `**` matches zero or more path segments.
fn glob_match_parts(pat: &[&str], txt: &[&str]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0);

    while ti < txt.len() {
        if pi < pat.len() && pat[pi] == "**" {
            star_pi = pi;
            star_ti = ti;
            pi += 1; // try matching zero segments
            continue;
        }

        if pi < pat.len() && segment_match(pat[pi], txt[ti]) {
            pi += 1;
            ti += 1;
            continue;
        }

        // Backtrack: `**` consumes one more segment
        if star_pi != usize::MAX {
            star_ti += 1;
            ti = star_ti;
            pi = star_pi + 1;
            continue;
        }

        return false;
    }

    // Remaining pattern must be all `**`
    while pi < pat.len() && pat[pi] == "**" {
        pi += 1;
    }
    pi == pat.len()
}

/// Match a single path segment against a pattern segment (supports `*` and `?`).
fn segment_match(pat: &str, txt: &str) -> bool {
    let (pb, tb) = (pat.as_bytes(), txt.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0);

    while ti < tb.len() {
        if pi < pb.len() && pb[pi] == b'*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
            continue;
        }
        if pi < pb.len() && (pb[pi] == b'?' || pb[pi] == tb[ti]) {
            pi += 1;
            ti += 1;
            continue;
        }
        if star_pi != usize::MAX {
            star_ti += 1;
            ti = star_ti;
            pi = star_pi + 1;
            continue;
        }
        return false;
    }
    while pi < pb.len() && pb[pi] == b'*' {
        pi += 1;
    }
    pi == pb.len()
}

// ── Dynamic discovery ────────────────────────────────────────────────────────

/// Discover and load skills from nested `.claude/skills/` directories between
/// `file_paths` and `cwd`. Only directories strictly below `cwd` are considered
/// (cwd-level skills are loaded at startup).
///
/// Returns the list of newly discovered skill directory paths.
pub fn discover_and_load_skills_for_paths(file_paths: &[&str], cwd: &Path) -> Vec<PathBuf> {
    let cwd_str = cwd.to_string_lossy().replace('\\', "/");
    let cwd_prefix = if cwd_str.ends_with('/') {
        cwd_str.clone()
    } else {
        format!("{}/", cwd_str)
    };

    let mut new_dirs = Vec::new();

    {
        let mut disc = lock_or_recover(discovered_dirs());

        for fp in file_paths {
            let mut current = Path::new(fp)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();

            // Walk up to cwd, NOT including cwd itself
            loop {
                let cur_str = current.to_string_lossy().replace('\\', "/");
                if !cur_str.starts_with(&cwd_prefix) {
                    break;
                }

                let skill_dir = current.join(".claude").join("skills");
                if !disc.contains(&skill_dir) {
                    disc.insert(skill_dir.clone());
                    if skill_dir.is_dir() {
                        new_dirs.push(skill_dir);
                    }
                }

                let parent = current.parent().map(|p| p.to_path_buf());
                match parent {
                    Some(p) if p != current => current = p,
                    _ => break,
                }
            }
        }
    }

    // Sort deepest first (more specific skills take precedence)
    new_dirs.sort_by(|a, b| {
        let a_depth = a.components().count();
        let b_depth = b.components().count();
        b_depth.cmp(&a_depth)
    });

    // Load skills from discovered directories
    if !new_dirs.is_empty() {
        let skills = load_skills_from_dirs(&new_dirs);
        let mut dyn_skills = lock_or_recover(dynamic_skills());
        for skill in skills {
            if !skill.paths.is_empty() {
                // Conditional → conditional cache
                conditional_cache()
                    .lock()
                    .unwrap()
                    .insert(skill.name.clone(), skill);
            } else {
                dyn_skills.entry(skill.name.clone()).or_insert(skill);
            }
        }
        debug!(
            "[skills] Dynamically discovered skills from {} directories",
            new_dirs.len()
        );
    }

    new_dirs
}

// ── Argument substitution ────────────────────────────────────────────────────

/// Parse arguments using simple shell-like splitting (respects quoted strings).
fn parse_arguments(args: &str) -> Vec<String> {
    let args = args.trim();
    if args.is_empty() {
        return vec![];
    }
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let chars = args.chars().peekable();

    for ch in chars {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// Substitute argument placeholders in a skill's system prompt.
/// Matches the TS `substituteArguments()` behavior:
///
/// 1. `$name` — named arguments from `arguments` frontmatter (word-boundary aware)
/// 2. `$ARGUMENTS[N]` — indexed argument access (0-based)
/// 3. `$N` — shorthand positional (0-based, word-boundary aware)
/// 4. `$ARGUMENTS` — replaced with the full argument string
/// 5. If no placeholders were found and args is non-empty, appends `\n\nARGUMENTS: {args}`
/// 6. `${CLAUDE_SKILL_DIR}` — path to the skill's root directory
pub fn substitute_arguments(skill: &SkillEntry, args: &str) -> String {
    let mut result = skill.system_prompt.clone();
    let original = result.clone();
    let parsed = parse_arguments(args);

    // 1. Named arguments: $name (no braces, word-boundary aware like TS)
    //    Also support ${name} as an extension.
    //    Numeric names are already filtered out at load time (parse_skill_file).
    for (i, arg_name) in skill.argument_names.iter().enumerate() {
        let value = parsed.get(i).map(|s| s.as_str()).unwrap_or("");

        // $name — must not be followed by word chars or `[`
        let dollar_name = format!("${}", arg_name);
        let mut new_result = String::with_capacity(result.len());
        let mut idx = 0;
        while idx < result.len() {
            if result[idx..].starts_with(&dollar_name) {
                let after = idx + dollar_name.len();
                let next_ch = result[after..].chars().next();
                // Word boundary: next char must NOT be alphanumeric, `_`, or `[`
                if next_ch.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '[') {
                    new_result.push_str(value);
                    idx = after;
                    continue;
                }
            }
            // Advance by full UTF-8 character (safe for non-ASCII)
            let ch = result[idx..].chars().next().unwrap();
            new_result.push(ch);
            idx += ch.len_utf8();
        }
        result = new_result;

        // Also handle ${name} for compatibility
        let braced = format!("${{{}}}", arg_name);
        result = result.replace(&braced, value);
    }

    // 2. $ARGUMENTS[N] — indexed access (0-based)
    let mut new_result = String::with_capacity(result.len());
    let mut idx = 0;
    while idx < result.len() {
        if result[idx..].starts_with("$ARGUMENTS[") {
            let start = idx + "$ARGUMENTS[".len();
            if let Some(bracket_end) = result[start..].find(']') {
                let index_str = &result[start..start + bracket_end];
                if let Ok(index) = index_str.parse::<usize>() {
                    new_result.push_str(parsed.get(index).map(|s| s.as_str()).unwrap_or(""));
                    idx = start + bracket_end + 1;
                    continue;
                }
            }
        }
        let ch = result[idx..].chars().next().unwrap();
        new_result.push(ch);
        idx += ch.len_utf8();
    }
    result = new_result;

    // 3. $N shorthand (0-based, word-boundary: not followed by \w)
    //    Process from highest index to lowest to avoid $1 matching inside $10
    let max_index = parsed.len().max(10); // handle at least $0..$9
    for i in (0..max_index).rev() {
        let placeholder = format!("${}", i);
        let mut new_result = String::with_capacity(result.len());
        let mut idx = 0;
        while idx < result.len() {
            if result[idx..].starts_with(&placeholder) {
                let after = idx + placeholder.len();
                let next_ch = result[after..].chars().next();
                // Word boundary: next char must NOT be alphanumeric or `_`
                if next_ch.is_none_or(|c| !c.is_alphanumeric() && c != '_') {
                    new_result.push_str(parsed.get(i).map(|s| s.as_str()).unwrap_or(""));
                    idx = after;
                    continue;
                }
            }
            let ch = result[idx..].chars().next().unwrap();
            new_result.push(ch);
            idx += ch.len_utf8();
        }
        result = new_result;
    }

    // 4. $ARGUMENTS — full argument string
    result = result.replace("$ARGUMENTS", args);

    // 5. If no placeholders were substituted and args is non-empty, append
    if result == original && !args.is_empty() {
        result.push_str(&format!("\n\nARGUMENTS: {}", args));
    }

    // 6. ${CLAUDE_SKILL_DIR}
    if let Some(ref root) = skill.skill_root {
        let root_str = root.to_string_lossy();
        result = result.replace("${CLAUDE_SKILL_DIR}", &root_str);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_fm_valid() {
        let content = "---\ndescription: test\n---\nBody text here";
        let (fm, body) = split_frontmatter(content);
        assert_eq!(fm.unwrap(), "description: test");
        assert_eq!(body, "Body text here");
    }

    #[test]
    fn split_fm_no_frontmatter() {
        let (fm, body) = split_frontmatter("Just plain body");
        assert!(fm.is_none());
        assert_eq!(body, "Just plain body");
    }

    #[test]
    fn split_fm_unclosed() {
        let (fm, _body) = split_frontmatter("---\nkey: val\nno end marker");
        assert!(fm.is_none());
    }

    #[test]
    fn extract_string_plain() {
        assert_eq!(extract_string("description: hello world", "description"), Some("hello world".into()));
    }

    #[test]
    fn extract_string_quoted() {
        assert_eq!(extract_string("description: \"quoted value\"", "description"), Some("quoted value".into()));
    }

    #[test]
    fn extract_string_missing() {
        assert_eq!(extract_string("other: value", "description"), None);
    }

    #[test]
    fn extract_string_empty_value() {
        assert_eq!(extract_string("description:", "description"), None);
    }

    #[test]
    fn extract_list_inline() {
        let yaml = "allowed_tools: [FileRead, Grep, Bash]";
        let list = extract_list(yaml, "allowed_tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn extract_list_block_style() {
        let yaml = "allowed_tools:\n- FileRead\n- Grep\n- Bash";
        let list = extract_list(yaml, "allowed_tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn extract_list_missing_key() {
        assert!(extract_list("other: value", "allowed_tools").is_none());
    }

    #[test]
    fn extract_list_inline_quoted() {
        let yaml = "allowed_tools: [\"FileRead\", 'Grep']";
        let list = extract_list(yaml, "allowed_tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep"]);
    }

    #[test]
    fn parse_skill_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.md");
        std::fs::write(&path, "---\ndescription: Security reviewer\nallowed_tools: [FileRead, Grep]\nmodel: claude-opus-4-20250514\n---\nYou are an expert.").unwrap();

        let skill = parse_skill_file(&path, "reviewer").unwrap();
        assert_eq!(skill.name, "reviewer");
        assert_eq!(skill.description, "Security reviewer");
        assert_eq!(skill.allowed_tools, vec!["FileRead", "Grep"]);
        assert_eq!(skill.model.as_deref(), Some("claude-opus-4-20250514"));
        assert_eq!(skill.system_prompt, "You are an expert.");
    }

    #[test]
    fn parse_skill_no_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper.md");
        std::fs::write(&path, "Just a prompt body.").unwrap();

        let skill = parse_skill_file(&path, "helper").unwrap();
        assert_eq!(skill.description, "helper");
        assert!(skill.allowed_tools.is_empty());
        assert!(skill.model.is_none());
        assert_eq!(skill.system_prompt, "Just a prompt body.");
    }

    #[test]
    fn load_skills_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        let skills = load_skills_from_dirs(&[skills_dir]);
        assert!(skills.is_empty());
    }

    #[test]
    fn load_skills_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("test.md"), "---\ndescription: Test skill\n---\nDo testing.").unwrap();
        std::fs::write(skills_dir.join("review.md"), "Review code.").unwrap();
        std::fs::write(skills_dir.join("readme.txt"), "Not a skill").unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"test"));
        assert!(names.contains(&"review"));
    }

    #[test]
    fn load_skills_dedup_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("test.md"), "First").unwrap();
        let skills = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn load_skills_directory_format() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");

        // Create directory-format skill: my-skill/SKILL.md
        let skill_dir = skills_dir.join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: My directory skill\n---\nDo something.",
        )
        .unwrap();

        // Also create a references file (should be ignored)
        let refs_dir = skill_dir.join("references");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(refs_dir.join("guide.md"), "Reference content").unwrap();

        // Create a flat-file skill too
        std::fs::write(skills_dir.join("flat.md"), "Flat skill body.").unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"my-skill"));
        assert!(names.contains(&"flat"));

        let dir_skill = skills.iter().find(|s| s.name == "my-skill").unwrap();
        assert_eq!(dir_skill.description, "My directory skill");
        assert_eq!(dir_skill.system_prompt, "Do something.");
    }

    #[test]
    fn load_skills_directory_without_skill_md_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        let empty_skill = skills_dir.join("no-skill-md");
        std::fs::create_dir_all(&empty_skill).unwrap();
        std::fs::write(empty_skill.join("readme.md"), "Not a SKILL.md").unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]);
        assert!(skills.is_empty(), "dir without SKILL.md should be ignored");
    }

    // ── Directory walking tests ──────────────────────────────────────────

    #[test]
    fn skill_dirs_walks_parents_up_to_home() {
        // Create a nested project under a temp "home" dir
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("projects").join("my-app");
        std::fs::create_dir_all(&project).unwrap();

        let dirs = skill_dirs(&project);
        // Should include: project/.claude/skills, projects/.claude/skills, home/.claude/skills
        assert!(dirs.len() >= 2, "Should walk at least 2 levels, got {}", dirs.len());
        assert_eq!(dirs[0], project.join(".claude").join("skills"));
    }

    #[test]
    fn skill_dirs_dedup_parent_skills_by_name() {
        let root = tempfile::tempdir().unwrap();
        let parent_skills = root.path().join(".claude").join("skills");
        let child = root.path().join("sub");
        let child_skills = child.join(".claude").join("skills");
        std::fs::create_dir_all(&parent_skills).unwrap();
        std::fs::create_dir_all(&child_skills).unwrap();

        // Same skill name in both parent and child
        std::fs::write(parent_skills.join("review.md"), "Parent review").unwrap();
        std::fs::write(child_skills.join("review.md"), "Child review").unwrap();
        // Unique to parent
        std::fs::write(parent_skills.join("deploy.md"), "Deploy").unwrap();

        let skills = load_skills_from_dirs(&[child_skills, parent_skills]);
        // "review" should only appear once (child wins), "deploy" from parent
        assert_eq!(skills.len(), 2);
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(review.system_prompt, "Child review", "child should shadow parent");
    }

    // ── Cache tests ─────────────────────────────────────────────────────

    #[test]
    fn get_skills_caches_results() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("cached.md"), "Prompt").unwrap();

        // Clear all state to avoid pollution from parallel tests
        clear_all_skill_state();

        let first = get_skills(dir.path());
        assert!(first.iter().any(|s| s.name == "cached"), "should contain our skill");

        // Remove the file — cached result should still have the skill
        std::fs::remove_file(skills_dir.join("cached.md")).unwrap();
        let second = get_skills(dir.path());
        assert!(
            second.iter().any(|s| s.name == "cached"),
            "cached result should still contain removed skill"
        );

        // After clearing cache, rescan should no longer find it
        clear_all_skill_state();
        let third = get_skills(dir.path());
        assert!(
            !third.iter().any(|s| s.name == "cached"),
            "removed skill should be gone after full clear"
        );

        // Cleanup
        clear_all_skill_state();
    }

    // ── Rich frontmatter tests ──────────────────────────────────────────

    #[test]
    fn parse_all_frontmatter_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.md");
        std::fs::write(
            &path,
            r#"---
description: Full featured skill
name: My Full Skill
allowed-tools: [FileRead, Grep]
model: claude-sonnet-4-20250514
when_to_use: Use for comprehensive reviews
paths: [src/**/*.rs, tests/**]
arguments: [file, language]
argument-hint: <file> [language]
version: 1.2.3
context: fork
agent: explore
effort: high
user-invocable: false
disable-model-invocation: true
---
Analyze $ARGUMENTS in ${file} for ${language}.
"#,
        )
        .unwrap();

        let skill = parse_skill_file(&path, "full").unwrap();
        assert_eq!(skill.description, "Full featured skill");
        assert_eq!(skill.display_name.as_deref(), Some("My Full Skill"));
        assert_eq!(skill.allowed_tools, vec!["FileRead", "Grep"]);
        assert_eq!(skill.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(
            skill.when_to_use.as_deref(),
            Some("Use for comprehensive reviews")
        );
        assert_eq!(skill.paths, vec!["src/**/*.rs", "tests/**"]);
        assert_eq!(skill.argument_names, vec!["file", "language"]);
        assert_eq!(skill.argument_hint.as_deref(), Some("<file> [language]"));
        assert_eq!(skill.version.as_deref(), Some("1.2.3"));
        assert_eq!(skill.context.as_deref(), Some("fork"));
        assert_eq!(skill.agent.as_deref(), Some("explore"));
        assert_eq!(skill.effort.as_deref(), Some("high"));
        assert!(!skill.user_invocable);
        assert!(skill.disable_model_invocation);
        assert!(skill.skill_root.is_none()); // flat file, not directory
    }

    #[test]
    fn parse_skill_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minimal.md");
        std::fs::write(&path, "Just prompt.").unwrap();

        let skill = parse_skill_file(&path, "minimal").unwrap();
        assert!(skill.user_invocable); // default true
        assert!(!skill.disable_model_invocation); // default false
        assert!(skill.paths.is_empty());
        assert!(skill.argument_names.is_empty());
        assert!(skill.display_name.is_none());
    }

    #[test]
    fn parse_directory_skill_has_skill_root() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        std::fs::write(&path, "---\ndescription: dir skill\n---\nPrompt with ${CLAUDE_SKILL_DIR}").unwrap();

        let skill = parse_skill_file(&path, "my-skill").unwrap();
        assert_eq!(skill.skill_root.as_deref(), Some(skill_dir.as_path()));
    }

    #[test]
    fn allowed_tools_hyphen_variant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool-test.md");
        std::fs::write(&path, "---\nallowed-tools: [Bash, FileRead]\n---\nPrompt").unwrap();

        let skill = parse_skill_file(&path, "tool-test").unwrap();
        assert_eq!(skill.allowed_tools, vec!["Bash", "FileRead"]);
    }

    // ── Glob matching tests ─────────────────────────────────────────────

    #[test]
    fn glob_match_star() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
    }

    #[test]
    fn glob_match_double_star() {
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "src/deep/nested/main.rs"));
        assert!(glob_match("src/**", "src/main.rs"));
        assert!(glob_match("src/**", "src/a/b/c.rs"));
    }

    #[test]
    fn glob_match_question_mark() {
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn glob_no_match() {
        assert!(!glob_match("*.py", "main.rs"));
        assert!(!glob_match("docs/**", "src/main.rs"));
    }

    // ── Conditional skill activation tests ───────────────────────────────

    #[test]
    fn conditional_skill_activation() {
        // Test conditional skill parsing + activation logic without global cache
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        std::fs::write(
            skills_dir.join("rust-review.md"),
            "---\ndescription: Rust reviewer\npaths: [src/**/*.rs]\n---\nReview Rust code.",
        )
        .unwrap();
        std::fs::write(
            skills_dir.join("general.md"),
            "---\ndescription: General helper\n---\nGeneral help.",
        )
        .unwrap();

        // Test that load_skills_from_dirs parses paths correctly
        let all = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(all.len(), 2);
        let conditional: Vec<_> = all.iter().filter(|s| !s.paths.is_empty()).collect();
        let regular: Vec<_> = all.iter().filter(|s| s.paths.is_empty()).collect();
        assert_eq!(conditional.len(), 1);
        assert_eq!(conditional[0].name, "rust-review");
        assert_eq!(conditional[0].paths, vec!["src/**/*.rs"]);
        assert_eq!(regular.len(), 1);
        assert_eq!(regular[0].name, "general");

        // Test glob matching used by activation
        assert!(!matches_any_pattern("docs/readme.md", &conditional[0].paths));
        assert!(matches_any_pattern("src/main.rs", &conditional[0].paths));
        assert!(matches_any_pattern("src/deep/nested/lib.rs", &conditional[0].paths));
    }

    // ── Argument substitution tests ─────────────────────────────────────

    /// Helper to create a minimal SkillEntry for argument substitution tests.
    fn test_skill(prompt: &str, arg_names: &[&str]) -> SkillEntry {
        SkillEntry {
            name: "test".into(),
            display_name: None,
            description: "test".into(),
            system_prompt: prompt.into(),
            allowed_tools: vec![],
            model: None,
            when_to_use: None,
            paths: vec![],
            argument_names: arg_names.iter().map(|s| s.to_string()).collect(),
            argument_hint: None,
            version: None,
            context: None,
            agent: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            skill_root: None,
        }
    }

    #[test]
    fn substitute_arguments_basic() {
        let skill = test_skill("Review $ARGUMENTS carefully.", &[]);
        let result = substitute_arguments(&skill, "main.rs");
        assert_eq!(result, "Review main.rs carefully.");
    }

    #[test]
    fn substitute_named_arguments_dollar_name() {
        // TS uses $name (no braces) for named args
        let skill = test_skill("Analyze $file in $language.", &["file", "language"]);
        let result = substitute_arguments(&skill, "main.rs Rust");
        assert_eq!(result, "Analyze main.rs in Rust.");
    }

    #[test]
    fn substitute_named_arguments_braced() {
        // Also support ${name} as extension
        let skill = test_skill("Analyze ${file} in ${language}.", &["file", "language"]);
        let result = substitute_arguments(&skill, "main.rs Rust");
        assert_eq!(result, "Analyze main.rs in Rust.");
    }

    #[test]
    fn substitute_positional_zero_based() {
        // TS uses $0, $1 (0-based)
        let skill = test_skill("First: $0, second: $1.", &[]);
        let result = substitute_arguments(&skill, "foo bar");
        assert_eq!(result, "First: foo, second: bar.");
    }

    #[test]
    fn substitute_positional_word_boundary() {
        // $1 must not match inside $10
        let skill = test_skill("$0 $1 $10", &[]);
        let result = substitute_arguments(&skill, "a b c d e f g h i j k");
        assert_eq!(result, "a b k");
    }

    #[test]
    fn substitute_indexed_arguments() {
        // $ARGUMENTS[N] syntax
        let skill = test_skill("First: $ARGUMENTS[0], last: $ARGUMENTS[2].", &[]);
        let result = substitute_arguments(&skill, "foo bar baz");
        assert_eq!(result, "First: foo, last: baz.");
    }

    #[test]
    fn substitute_no_placeholder_appends() {
        // When no placeholders found, append ARGUMENTS: ...
        let skill = test_skill("Just a prompt.", &[]);
        let result = substitute_arguments(&skill, "some args");
        assert_eq!(result, "Just a prompt.\n\nARGUMENTS: some args");
    }

    #[test]
    fn substitute_no_placeholder_empty_args() {
        // Empty args should NOT append
        let skill = test_skill("Just a prompt.", &[]);
        let result = substitute_arguments(&skill, "");
        assert_eq!(result, "Just a prompt.");
    }

    #[test]
    fn substitute_named_word_boundary() {
        // $file should not match $filename
        let skill = test_skill("$file and $filename", &["file"]);
        let result = substitute_arguments(&skill, "test.rs");
        assert_eq!(result, "test.rs and $filename");
    }

    #[test]
    fn substitute_quoted_args() {
        let skill = test_skill("$0 and $1", &[]);
        let result = substitute_arguments(&skill, r#"hello "world peace""#);
        assert_eq!(result, "hello and world peace");
    }

    #[test]
    fn substitute_skill_dir() {
        let mut skill = test_skill("Read ${CLAUDE_SKILL_DIR}/ref.md", &[]);
        skill.skill_root = Some(PathBuf::from("/skills/my-skill"));
        let result = substitute_arguments(&skill, "");
        assert!(result.contains("/skills/my-skill/ref.md"));
    }

    // ── CRLF frontmatter test ───────────────────────────────────────────

    #[test]
    fn split_frontmatter_crlf() {
        let content = "---\r\ndescription: test\r\n---\r\nBody text here";
        let (fm, body) = split_frontmatter(content);
        assert_eq!(fm.unwrap(), "description: test");
        assert_eq!(body, "Body text here");
    }

    // ── parse_arguments tests ───────────────────────────────────────────

    #[test]
    fn parse_args_simple() {
        assert_eq!(parse_arguments("foo bar baz"), vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn parse_args_quoted() {
        assert_eq!(
            parse_arguments(r#"foo "hello world" baz"#),
            vec!["foo", "hello world", "baz"]
        );
    }

    #[test]
    fn parse_args_single_quoted() {
        assert_eq!(
            parse_arguments("foo 'hello world' baz"),
            vec!["foo", "hello world", "baz"]
        );
    }

    #[test]
    fn parse_args_empty() {
        assert!(parse_arguments("").is_empty());
        assert!(parse_arguments("   ").is_empty());
    }

    // ── UTF-8 safety tests ──────────────────────────────────────────────

    #[test]
    fn substitute_cjk_before_placeholder() {
        let skill = test_skill("分析 $file 的结果", &["file"]);
        let result = substitute_arguments(&skill, "main.rs");
        assert_eq!(result, "分析 main.rs 的结果");
    }

    #[test]
    fn substitute_cjk_no_placeholder() {
        let skill = test_skill("这是一个中文提示词", &[]);
        let result = substitute_arguments(&skill, "args");
        assert_eq!(result, "这是一个中文提示词\n\nARGUMENTS: args");
    }

    #[test]
    fn substitute_emoji_in_prompt() {
        let skill = test_skill("🔍 Review $0 📝", &[]);
        let result = substitute_arguments(&skill, "code.rs");
        assert_eq!(result, "🔍 Review code.rs 📝");
    }

    #[test]
    fn substitute_mixed_unicode_indexed() {
        let skill = test_skill("审查 $ARGUMENTS[0] 中的 $ARGUMENTS[1]", &[]);
        let result = substitute_arguments(&skill, "main.rs bugs");
        assert_eq!(result, "审查 main.rs 中的 bugs");
    }

    // ── Numeric argument name filter tests ───────────────────────────────

    #[test]
    fn substitute_numeric_arg_name_skipped() {
        // Numeric arg names are filtered at load time (TS parseArgumentNames behavior)
        // so $0 is treated as positional, not named.
        // With arguments: ["0", "file"], after filtering → ["file"]
        // "file" gets i=0 → parsedArgs[0] (matching TS behavior)
        let skill = test_skill("$file and $0", &["file"]);
        let result = substitute_arguments(&skill, "a b");
        // $file → parsedArgs[0] = "a" (named), $0 → parsedArgs[0] = "a" (positional)
        assert_eq!(result, "a and a");
    }

    #[test]
    fn parse_skill_filters_numeric_arg_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("numeric-args.md");
        std::fs::write(&path, "---\narguments: [0, file, 1, language]\n---\nPrompt").unwrap();

        let skill = parse_skill_file(&path, "numeric-args").unwrap();
        assert_eq!(skill.argument_names, vec!["file", "language"]);
    }
}
