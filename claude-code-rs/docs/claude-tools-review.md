# Deep Review: clawed-tools

## Overview

28+ tool source files across 10 categories. Reviewed ~5000 lines of Rust code.

---

## Critical Issues (P0 — Fix Immediately)

### C-1: `grep.rs` bypasses path boundary checks (lines 93-98)

**File:** `crates/clawed-tools/src/grep.rs`

```rust
let search_path: PathBuf = match input["path"].as_str() {
    Some(p) => {
        let pa = std::path::Path::new(p);
        if pa.is_absolute() { pa.to_path_buf() } else { context.cwd.join(pa) }
    }
    None => context.cwd.clone(),
};
```

Unlike **every other file tool** (Read, Edit, Write, LS, Glob, Bash working_directory), Grep does **NOT** call `path_util::resolve_path()`. This means an absolute path like `/etc/passwd` or a relative path like `../../secret` can search the entire filesystem.

**Fix:** Replace with `path_util::resolve_path(p, &context.cwd)?`.

---

### C-2: `repl.rs` UTF-8 truncation panic (line 163)

**File:** `crates/clawed-tools/src/repl.rs`

```rust
if text.len() > 50_000 {
    text.truncate(50_000);  // PANICS if 50000 splits a multi-byte UTF-8 char
```

Compare with `bash.rs:truncate_output` which correctly uses `is_char_boundary()`. If REPL output contains multi-byte characters at position 50,000, this **panics**.

**Fix:**
```rust
let mut end = 50_000;
while end > 0 && !text.is_char_boundary(end) { end -= 1; }
text.truncate(end);
```

---

### C-3: `multi_edit.rs` data corruption via sequential edits (lines 75-122)

**File:** `crates/clawed-tools/src/multi_edit.rs`

Edits are applied sequentially with `replacen(old_str, new_str, 1)`. If Edit 0's `new_string` contains Edit 1's `old_string`, Edit 1 matches in the **wrong location**.

```
Original: "aaa bbb ccc"
Edit 0: "aaa" → "bbb_new"   → "bbb_new bbb ccc"
Edit 1: "bbb" → "XXX"       → matches FIRST "bbb" in "bbb_new", not original position
```

Result: `XXX_new XXX ccc` instead of `bbb_new XXX ccc`.

**Fix:** Apply edits in reverse order (by position) to avoid position shifting, or track exact character offsets and use slice-based replacement instead of string search.

---

### C-4: Consistent symlink bypass across ALL file-writing tools

**Files:** `file_edit.rs`, `file_write.rs`, `multi_edit.rs`

`resolve_path` normalizes paths but does **not** resolve symlinks. A symlink inside the project boundary pointing to `/etc/passwd` passes the boundary check. The `resolve_symlink_safe()` function exists in `path_util.rs` but is **never called** by any file tool.

**Fix:** After `resolve_path`, call `resolve_symlink_safe` or make `resolve_path` symlink-aware by default.

---

## High Severity (P1 — Fix Soon)

### H-1: `grep.rs` double-counts matches in count mode (lines 244-249)

In count mode, lines are scanned once in the main loop (incrementing `total_matches`) and then **re-scanned** with `regex.is_match()` to compute `file_match_count`. The summary line reports `total_matches` (global) but individual file counts are also displayed — these numbers can diverge if results are truncated.

**Fix:** Accumulate per-file counts during the first pass instead of re-scanning.

---

### H-2: `path_util.rs` UTF-8 split panic in byte truncation (lines 103-106)

```rust
if line_truncated.len() > MAX_TOOL_OUTPUT_SIZE {
    let truncated = &line_truncated[..MAX_TOOL_OUTPUT_SIZE];
    let cut_point = truncated.rfind('\n').unwrap_or(MAX_TOOL_OUTPUT_SIZE);
```

If a single line exceeds 30KB and position 30720 falls in the middle of a multi-byte UTF-8 character, `&line_truncated[..MAX_TOOL_OUTPUT_SIZE]` **panics**. The `rfind('\n')` fallback only helps if there's a newline before the cut point.

**Fix:** Use `is_char_boundary()` to find a safe cut point.

---

### H-3: `path_util.rs` `..` pops above root silently on Windows (lines 47-60)

```rust
Component::ParentDir => {
    result.pop();
}
```

On Windows, `C:\..` would `pop()` the drive component entirely, producing an empty path. This is a **silent path traversal bypass** — `normalize_path` accepts it without error.

**Fix:** Refuse to `pop()` when the result would be empty or a drive letter only.

---

### H-4: `powershell.rs` process error returns `Ok` instead of `Err` (line 115)

```rust
Ok(Err(e)) => Ok(ToolResult::error(format!("Process error: {e}"))),
```

Compare with `bash.rs:331` which correctly returns `Err(anyhow::anyhow!("Process error: {e}"))`. This inconsistency means the agent loop can't distinguish process spawn failures from normal errors.

**Fix:** Return `Err(anyhow::anyhow!("Process error: {e}"))` for consistency.

---

### H-5: `ls.rs` metadata failure aborts entire listing (line 68)

```rust
let meta = entry.metadata().await?;
```

If a single entry can't be read (permission denied), the **entire** listing fails. Should skip unreadable entries.

**Fix:** Use `entry.metadata().await.ok()?` or filter_map with error logging.

---

### H-6: `repl.rs` Windows `taskkill` missing `/T` flag (line 186)

```rust
let _ = StdCommand::new("taskkill").args(["/F", "/PID", &pid.to_string()]).status();
```

Compare with `bash.rs:359` which correctly uses `["/F", "/T", "/PID", ...]`. Without `/T`, child processes of the REPL (e.g., a Python subprocess) are orphaned.

**Fix:** Add `/T` flag.

---

### H-7: `repl.rs` `which_exists` blocks async executor (lines 198-205)

```rust
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new(cmd)  // BLOCKS the async executor!
```

Called on every REPL invocation. Should use `tokio::process::Command` or cache results.

---

### H-8: `path_util.rs` `find_project_root` spawns synchronous git (lines 64-78)

```rust
fn find_project_root(cwd: &Path) -> Option<PathBuf> {
    std::process::Command::new("git")
```

Called from `resolve_path` which is used in async contexts. A slow or hung `git` command blocks the async executor.

**Fix:** Use `tokio::process::Command` or cache the result.

---

### H-9: `web_search.rs` no HTTP timeout configured (line 120)

```rust
let client = reqwest::Client::new();
```

Default `reqwest::Client` has **no timeout**. A slow or unresponsive search API hangs indefinitely.

**Fix:** Use `reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap()`.

---

### H-10: `web_fetch.rs` no URL validation (SSRF risk)

The tool fetches arbitrary URLs without validating that they don't point to:
- Private/internal IP ranges (10.x, 172.16.x, 192.168.x, 127.x, 169.254.x)
- `file://` scheme URLs

**Fix:** Add URL validation to reject private/reserved IP ranges and non-HTTP schemes.

---

## Medium Severity (P2 — Should Fix)

### M-1: `ls.rs` glob matching bug for `*foo*` patterns (lines 114-123)

```rust
fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" { return true; }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);  // "*.log" → ends_with(".log") ✓
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);  // "log*" → starts_with("log") ✓
    }
    pattern == name;  // "*log*" → exact match "log*" — WRONG
}
```

Pattern `*log*` would strip `*` prefix to get `log*`, then check `name.ends_with("log*")` which never matches a real filename.

---

### M-2: `grep.rs` type vs include glob inconsistency

`type_to_globs` matches against **filename** only (lines 40-42). The `include` parameter matches against the **full path** (lines 152-157). This inconsistency means `include: "*.rs"` may not match the same files as `type: "rust"`.

---

### M-3: `bash.rs` timeout parameter name inconsistency (line 213 vs powershell.rs line 32)

BashTool uses `"timeout"` but PowerShellTool uses `"timeout_ms"`. This inconsistency confuses clients.

---

### M-4: `task.rs` no file locking on task persistence

Tasks are stored as individual JSON files under `~/.claude/tasks/`. Concurrent task operations (e.g., from parallel sub-agents) can cause **write collisions** or **partial reads**. No file locking (flock) or atomic writes (write to temp, then rename) are used.

---

### M-5: `task.rs` `unblock_downstream` can deadlock on circular deps

If task A blocks B and B blocks A, completing A triggers `unblock_downstream` which tries to save B, but B is still blocked by A... Actually the function would complete but leave inconsistent state. No cycle detection exists.

---

### M-6: `file_edit.rs` external modification check doesn't block writes (lines 139-142)

```rust
if let Some(warning) = check_external_modification(&path, &content) {
    eprintln!("{warning}");  // Goes to stderr — invisible to the LLM
}
```

The warning is invisible to the agent. The edit proceeds anyway, potentially overwriting external changes silently.

---

### M-7: `file_write.rs` doesn't update `file_state_cache`

`file_edit.rs` calls `update_file_state` after writing. `file_write.rs` does not. This means if you Write a file then later Edit it, the external modification check will always trigger (false positive).

---

### M-8: `web_search.rs` no rate limiting

No rate limiting on search requests. Rapid model calls can exhaust search API quota.

---

### M-9: `glob_tool.rs` sorts alphabetically, not by modification time

Description promises "sorted by modification time" but code uses `matches.sort()` which is lexicographic. Documentation/behavior mismatch.

---

### M-10: `glob_tool.rs` canonicalize fails silently on non-existent paths (lines 56-58)

```rust
let Ok(resolved) = path.canonicalize() else { continue };
let Ok(search_canonical) = search_dir.canonicalize() else { continue };
```

If `search_dir` doesn't exist, the entire glob returns empty results instead of an error. Misleading to users.

---

### M-11: `powershell.rs` no abort signal support

Unlike BashTool (which uses `tokio::select!` to monitor abort), PowerShellTool has no interrupt mechanism.

---

### M-12: `powershell.rs` missing working_directory and environment support

PowerShellTool ignores `input["working_directory"]` and `input["environment"]`, unlike BashTool which supports both.

---

### M-13: `file_read.rs` binary detection only checks first 8KB

```rust
let check_len = data.len().min(8192);
```

A polyglot file with valid text at the start and binary data after 8KB would pass the check and expose binary content.

---

### M-14: `web_search.rs` domain filtering uses search syntax, not enforcement

```rust
for domain in allowed_domains {
    search_query.push_str(&format!(" site:{domain}"));
}
```

Relies entirely on the search engine to respect `site:` syntax. If the API doesn't support it, restrictions are silently ignored.

---

### M-15: `bash.rs` `extract_base_command` fragile with quoted strings

```rust
for part in &mut parts {
    if part.contains('=') && !part.starts_with('-') {
        continue;  // FOO="bar=baz" would be skipped correctly
    }
    return first_cmd[first_cmd.find(part).unwrap_or(0)..].trim();  // find() matches FIRST occurrence
}
```

`first_cmd.find(part)` finds the first occurrence of the string, not necessarily the current word. If an env var value contains the same substring as the command name, it could match incorrectly.

---

## Low Severity (P3 — Nice to Have)

### L-1: `path_util.rs` trailing whitespace on `#[must_use]` attributes (lines 86, 139, 154)

Style: `#[must_use] ` has trailing space.

### L-2: `web_search.rs` duplicated UTF-8 truncation code (lines 157-164, 182-188)

Same 8-line block appears twice. Should extract to `truncate_utf8(s: &str, max_len: usize) -> &str`.

### L-3: `grep.rs` duplicate test: `type_to_globs_unknown` (line 316) and `type_to_globs_unknown_returns_none` (line 457)

### L-4: `grep.rs` empty pattern matches every line (DoS vector)

An empty regex matches every line in every file. Should be rejected or warned about.

### L-5: `file_read.rs` `offset`/`limit` edge cases not documented

If `offset` > file length, returns empty string. If `limit` is 0, returns nothing. Reasonable but undocumented.

### L-6: `web_search.rs` `expect("checked above")` unnecessary (line 89)

Safe due to early return guard, but `as_deref().unwrap()` or pattern matching would be more idiomatic.

### L-7: `task.rs` tests write to disk without cleanup

Tests like `test_file_state_cache` write to `/tmp/` without cleanup, polluting the test environment.

### L-8: `file_edit.rs` `count_line_changes` is not a real diff

Line-by-line comparison at same index. If one line is inserted at position 0, every subsequent line is counted as changed. Function name implies accuracy.

### L-9: `file_read.rs` reads directory via `Read` tool — should suggest `LS`

When passed a directory path, `file_read.rs` reads directory entries but returns a plain text list. Should suggest using `LS` tool instead.

### L-10: `todo.rs` no validation of task status transitions

`TodoWriteTool` accepts any status string without validation against the allowed enum values.

---

## Summary Table

| Severity | Count | Key Issues |
|----------|-------|------------|
| **P0 Critical** | 4 | Grep path traversal, REPL UTF-8 panic, multi-edit data corruption, symlink bypass |
| **P1 High** | 10 | Grep double-count, path_util UTF-8 panic, Windows `..` traversal, PS error handling, LS metadata abort, REPL taskkill, REPL blocking, git blocking, web no-timeout, web SSRF |
| **P2 Medium** | 15 | LS glob bug, grep type/include inconsistency, timeout param naming, task file locking, task circular deps, file edit external mod, file_write cache, web rate limit, glob sort, glob canonicalize, PS abort signal, PS missing features, binary detection, domain filtering, bash base command |
| **P3 Low** | 10 | Style nits, duplicate code/tests, empty pattern DoS, edge case docs, test cleanup, diff accuracy |

## Recommended Fix Order

1. **`grep.rs:93-98`** — Add `resolve_path` call (path traversal vulnerability)
2. **`repl.rs:163`** — Fix UTF-8 truncation panic
3. **`multi_edit.rs:75-122`** — Fix data corruption from sequential edits
4. **`path_util.rs:103-106`** — Fix UTF-8 byte truncation panic
5. **All file-write tools** — Add symlink resolution after `resolve_path`
6. **`powershell.rs:115`** — Return `Err` instead of `Ok(ToolResult::error(...))`
7. **`ls.rs:68`** — Handle metadata errors gracefully
8. **`bash.rs`/`powershell.rs`** — Align timeout parameter names
9. **`path_util.rs:47-60`** — Refuse `..` above root on Windows
10. **`repl.rs:186`** — Add `/T` flag to taskkill
11. **`web_search.rs:120`** — Add HTTP timeout
12. **`web_fetch.rs`** — Add URL validation for SSRF prevention
