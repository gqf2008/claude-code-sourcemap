//! Prompt cache break detection — tracks prompt/tool schema changes and
//! detects cache invalidation events.
//!
//! Aligned with TS `services/api/promptCacheBreakDetection.ts`:
//! - Hash-based change detection for system prompt, tool schemas, model, betas
//! - Per-tool schema hashing for granular change identification
//! - Two-phase: record_prompt_state() before API call, check_response() after
//! - TTL-aware: distinguishes client-side changes from 5min/1hour cache expiry
//! - Per-source state tracking with LRU eviction (max 10 sources)

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of tracked sources (prevents unbounded memory growth).
const MAX_TRACKED_SOURCES: usize = 10;

/// Minimum absolute token drop to trigger cache break warning.
const MIN_CACHE_MISS_TOKENS: i64 = 2_000;

/// 5-minute TTL threshold.
const CACHE_TTL_5MIN: Duration = Duration::from_secs(5 * 60);

/// 1-hour TTL threshold.
pub const CACHE_TTL_1HOUR: Duration = Duration::from_secs(60 * 60);

/// Source prefixes that are tracked for cache break detection.
const TRACKED_SOURCE_PREFIXES: &[&str] = &[
    "repl_main_thread",
    "sdk",
    "agent:custom",
    "agent:default",
    "agent:builtin",
];

// ── Types ────────────────────────────────────────────────────────────────────

/// Detailed change information between two consecutive prompt states.
#[derive(Debug, Clone)]
pub struct PendingChanges {
    pub system_prompt_changed: bool,
    pub tool_schemas_changed: bool,
    pub model_changed: bool,
    pub fast_mode_changed: bool,
    pub cache_control_changed: bool,
    pub betas_changed: bool,
    pub auto_mode_changed: bool,
    pub effort_changed: bool,
    pub extra_body_changed: bool,
    pub added_tool_count: usize,
    pub removed_tool_count: usize,
    pub system_char_delta: i64,
    pub added_tools: Vec<String>,
    pub removed_tools: Vec<String>,
    pub changed_tool_schemas: Vec<String>,
    pub previous_model: String,
    pub new_model: String,
    pub added_betas: Vec<String>,
    pub removed_betas: Vec<String>,
    pub prev_effort_value: String,
    pub new_effort_value: String,
}

/// Snapshot of the current prompt state for change detection.
#[derive(Debug, Clone)]
pub struct PromptStateSnapshot {
    /// Serialized system prompt text (for hashing).
    pub system_text: String,
    /// Tool schemas as JSON strings (for hashing).
    pub tool_schemas_json: Vec<String>,
    /// Tool names in order.
    pub tool_names: Vec<String>,
    /// Query source identifier.
    pub query_source: String,
    /// Model name.
    pub model: String,
    /// Optional agent ID for isolation.
    pub agent_id: Option<String>,
    /// Fast mode flag.
    pub fast_mode: bool,
    /// Beta header list.
    pub betas: Vec<String>,
    /// Auto-mode active flag.
    pub auto_mode_active: bool,
    /// Effort value.
    pub effort_value: String,
    /// Extra body params hash input.
    pub extra_body_json: Option<String>,
    /// System prompt character count.
    pub system_char_count: usize,
    /// Cache control scope identifier (e.g. "global", "org", "none").
    pub cache_control_scope: Option<String>,
    /// Whether using usage overage.
    pub is_using_overage: bool,
}

/// Internal tracking state per source.
#[derive(Debug)]
struct PreviousState {
    system_hash: u64,
    tools_hash: u64,
    tool_names: Vec<String>,
    per_tool_hashes: HashMap<String, u64>,
    system_char_count: usize,
    model: String,
    fast_mode: bool,
    betas: Vec<String>,
    auto_mode_active: bool,
    effort_value: String,
    extra_body_hash: u64,
    cache_control_hash: u64,
    is_using_overage: bool,
    call_count: u64,
    pending_changes: Option<PendingChanges>,
    prev_cache_read_tokens: Option<i64>,
    cache_deletions_pending: bool,
    last_call_time: Instant,
}

/// Result of cache break analysis.
#[derive(Debug, Clone)]
pub struct CacheBreakReport {
    /// Human-readable reason for the cache break.
    pub reason: String,
    /// Previous cache read tokens.
    pub prev_cache_read: i64,
    /// Current cache read tokens.
    pub cache_read: i64,
    /// Cache creation tokens.
    pub cache_creation: i64,
    /// Call number for this source.
    pub call_number: u64,
    /// Detailed changes if client-side.
    pub changes: Option<PendingChanges>,
}

// ── Global state ─────────────────────────────────────────────────────────────

/// Thread-safe global state for cache tracking.
struct CacheTracker {
    states: HashMap<String, PreviousState>,
}

impl CacheTracker {
    fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }
}

// Use a Mutex for thread-safe access
static TRACKER: std::sync::LazyLock<Mutex<CacheTracker>> =
    std::sync::LazyLock::new(|| Mutex::new(CacheTracker::new()));

// ── Hash utilities ───────────────────────────────────────────────────────────

/// Compute a hash of a string using DefaultHasher (djb2-like).
fn compute_hash(data: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Compute per-tool hashes from tool schema strings.
fn compute_per_tool_hashes(schemas: &[String], names: &[String]) -> HashMap<String, u64> {
    let mut hashes = HashMap::new();
    for (i, schema) in schemas.iter().enumerate() {
        let name = names.get(i)
            .cloned()
            .unwrap_or_else(|| format!("__idx_{}", i));
        hashes.insert(name, compute_hash(schema));
    }
    hashes
}

/// Check if a model should be excluded from cache break detection.
fn is_excluded_model(model: &str) -> bool {
    model.contains("haiku")
}

// ── Tracking key resolution ──────────────────────────────────────────────────

/// Get the tracking key for a query source + optional agent ID.
///
/// Returns None for untracked sources (speculation, session_memory, etc.).
fn get_tracking_key(query_source: &str, agent_id: Option<&str>) -> Option<String> {
    // Compact shares cache with repl_main_thread
    if query_source == "compact" {
        return Some("repl_main_thread".to_string());
    }

    for prefix in TRACKED_SOURCE_PREFIXES {
        if query_source.starts_with(prefix) {
            return Some(agent_id.map(String::from).unwrap_or_else(|| query_source.to_string()));
        }
    }

    None // Untracked source
}

// ── Core API ─────────────────────────────────────────────────────────────────

/// Record the current prompt state BEFORE making an API call.
///
/// Compares with previous state for the same source and records pending changes.
pub fn record_prompt_state(snapshot: &PromptStateSnapshot) {
    let key = match get_tracking_key(&snapshot.query_source, snapshot.agent_id.as_deref()) {
        Some(k) => k,
        None => return,
    };

    let system_hash = compute_hash(&snapshot.system_text);
    let tools_combined: String = snapshot.tool_schemas_json.join("\n---\n");
    let tools_hash = compute_hash(&tools_combined);
    let cache_control_hash = snapshot.cache_control_scope
        .as_deref()
        .map(compute_hash)
        .unwrap_or(0);

    let sorted_betas = {
        let mut b = snapshot.betas.clone();
        b.sort();
        b
    };
    let effort_str = snapshot.effort_value.clone();
    let extra_body_hash = snapshot.extra_body_json
        .as_deref()
        .map(compute_hash)
        .unwrap_or(0);

    let mut tracker = match TRACKER.lock() {
        Ok(t) => t,
        Err(e) => {
            warn!("Cache tracker lock poisoned: {}", e);
            return;
        }
    };

    if let Some(prev) = tracker.states.get_mut(&key) {
        // Compare with previous state
        prev.call_count += 1;

        let system_prompt_changed = system_hash != prev.system_hash;
        let tool_schemas_changed = tools_hash != prev.tools_hash;
        let model_changed = snapshot.model != prev.model;
        let fast_mode_changed = snapshot.fast_mode != prev.fast_mode;
        let betas_changed = sorted_betas != prev.betas;
        let auto_mode_changed = snapshot.auto_mode_active != prev.auto_mode_active;
        let effort_changed = effort_str != prev.effort_value;
        let extra_body_changed = extra_body_hash != prev.extra_body_hash;
        let cache_control_changed = cache_control_hash != prev.cache_control_hash;
        let overage_changed = snapshot.is_using_overage != prev.is_using_overage;

        let any_change = system_prompt_changed
            || tool_schemas_changed
            || model_changed
            || fast_mode_changed
            || betas_changed
            || auto_mode_changed
            || effort_changed
            || extra_body_changed
            || cache_control_changed
            || overage_changed;

        if any_change {
            let prev_tool_set: HashSet<&str> = prev.tool_names.iter().map(|s| s.as_str()).collect();
            let new_tool_set: HashSet<&str> = snapshot.tool_names.iter().map(|s| s.as_str()).collect();

            let added_tools: Vec<String> = snapshot.tool_names.iter()
                .filter(|n| !prev_tool_set.contains(n.as_str()))
                .cloned()
                .collect();
            let removed_tools: Vec<String> = prev.tool_names.iter()
                .filter(|n| !new_tool_set.contains(n.as_str()))
                .cloned()
                .collect();

            // Identify specific tool schema changes
            let mut changed_tool_schemas = Vec::new();
            if tool_schemas_changed {
                let new_hashes = compute_per_tool_hashes(
                    &snapshot.tool_schemas_json,
                    &snapshot.tool_names,
                );
                for name in &snapshot.tool_names {
                    if prev_tool_set.contains(name.as_str()) {
                        let new_h = new_hashes.get(name);
                        let prev_h = prev.per_tool_hashes.get(name);
                        if new_h != prev_h {
                            changed_tool_schemas.push(name.clone());
                        }
                    }
                }
                prev.per_tool_hashes = new_hashes;
            }

            let added_betas: Vec<String> = sorted_betas.iter()
                .filter(|b| !prev.betas.contains(b))
                .cloned()
                .collect();
            let removed_betas: Vec<String> = prev.betas.iter()
                .filter(|b| !sorted_betas.contains(b))
                .cloned()
                .collect();

            prev.pending_changes = Some(PendingChanges {
                system_prompt_changed,
                tool_schemas_changed,
                model_changed,
                fast_mode_changed,
                cache_control_changed,
                betas_changed,
                auto_mode_changed,
                effort_changed,
                extra_body_changed,
                added_tool_count: added_tools.len(),
                removed_tool_count: removed_tools.len(),
                system_char_delta: snapshot.system_char_count as i64 - prev.system_char_count as i64,
                added_tools,
                removed_tools,
                changed_tool_schemas,
                previous_model: prev.model.clone(),
                new_model: snapshot.model.clone(),
                added_betas,
                removed_betas,
                prev_effort_value: prev.effort_value.clone(),
                new_effort_value: effort_str.clone(),
            });
        } else {
            prev.pending_changes = None;
        }

        // Update state
        prev.system_hash = system_hash;
        prev.tools_hash = tools_hash;
        prev.tool_names = snapshot.tool_names.clone();
        prev.system_char_count = snapshot.system_char_count;
        prev.model = snapshot.model.clone();
        prev.fast_mode = snapshot.fast_mode;
        prev.betas = sorted_betas;
        prev.auto_mode_active = snapshot.auto_mode_active;
        prev.effort_value = effort_str;
        prev.extra_body_hash = extra_body_hash;
        prev.cache_control_hash = cache_control_hash;
        prev.is_using_overage = snapshot.is_using_overage;
        prev.last_call_time = Instant::now();
    } else {
        // First call — initialize state
        // Evict oldest if at capacity
        while tracker.states.len() >= MAX_TRACKED_SOURCES {
            let oldest_key = tracker.states.iter()
                .min_by_key(|(_, v)| v.last_call_time)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key {
                tracker.states.remove(&k);
            } else {
                break;
            }
        }

        let per_tool_hashes = compute_per_tool_hashes(
            &snapshot.tool_schemas_json,
            &snapshot.tool_names,
        );

        tracker.states.insert(key, PreviousState {
            system_hash,
            tools_hash,
            tool_names: snapshot.tool_names.clone(),
            per_tool_hashes,
            system_char_count: snapshot.system_char_count,
            model: snapshot.model.clone(),
            fast_mode: snapshot.fast_mode,
            betas: sorted_betas,
            auto_mode_active: snapshot.auto_mode_active,
            effort_value: effort_str,
            extra_body_hash,
            cache_control_hash,
            is_using_overage: snapshot.is_using_overage,
            call_count: 1,
            pending_changes: None,
            prev_cache_read_tokens: None,
            cache_deletions_pending: false,
            last_call_time: Instant::now(),
        });
    }
}

/// Check the API response for a cache break event.
///
/// Should be called AFTER receiving the API response with usage data.
/// Returns a report if a cache break was detected.
pub fn check_response_for_cache_break(
    query_source: &str,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    time_since_last_assistant_ms: Option<i64>,
    agent_id: Option<&str>,
) -> Option<CacheBreakReport> {
    let key = get_tracking_key(query_source, agent_id)?;

    let mut tracker = match TRACKER.lock() {
        Ok(t) => t,
        Err(_) => return None,
    };

    let state = tracker.states.get_mut(&key)?;

    if is_excluded_model(&state.model) {
        return None;
    }

    let prev_cache_read = state.prev_cache_read_tokens;
    state.prev_cache_read_tokens = Some(cache_read_tokens);

    // First call — no baseline
    let prev_cache_read = prev_cache_read?;

    let changes = state.pending_changes.take();

    // Handle expected cache deletions
    if state.cache_deletions_pending {
        state.cache_deletions_pending = false;
        debug!(
            "[PROMPT CACHE] cache deletion applied, cache read: {} → {} (expected drop)",
            prev_cache_read, cache_read_tokens
        );
        return None;
    }

    // Check thresholds: >5% relative drop AND >2000 absolute drop
    let token_drop = prev_cache_read - cache_read_tokens;
    if cache_read_tokens as f64 >= prev_cache_read as f64 * 0.95 || token_drop < MIN_CACHE_MISS_TOKENS {
        return None;
    }

    // Build explanation
    let mut parts = Vec::new();
    if let Some(ref c) = changes {
        if c.model_changed {
            parts.push(format!("model changed ({} → {})", c.previous_model, c.new_model));
        }
        if c.system_prompt_changed {
            let delta = c.system_char_delta;
            let info = if delta == 0 {
                String::new()
            } else if delta > 0 {
                format!(" (+{} chars)", delta)
            } else {
                format!(" ({} chars)", delta)
            };
            parts.push(format!("system prompt changed{}", info));
        }
        if c.tool_schemas_changed {
            let tool_diff = if c.added_tool_count > 0 || c.removed_tool_count > 0 {
                format!(" (+{}/-{} tools)", c.added_tool_count, c.removed_tool_count)
            } else if !c.changed_tool_schemas.is_empty() {
                format!(" (schemas changed: {})", sanitize_tool_names(&c.changed_tool_schemas))
            } else {
                " (tool prompt/schema changed, same tool set)".to_string()
            };
            parts.push(format!("tools changed{}", tool_diff));
        }
        if c.fast_mode_changed {
            parts.push("fast mode toggled".to_string());
        }
        if c.betas_changed {
            let mut diff_parts = Vec::new();
            if !c.added_betas.is_empty() {
                diff_parts.push(format!("+{}", c.added_betas.join(",")));
            }
            if !c.removed_betas.is_empty() {
                diff_parts.push(format!("-{}", c.removed_betas.join(",")));
            }
            let diff = diff_parts.join(" ");
            if diff.is_empty() {
                parts.push("betas changed".to_string());
            } else {
                parts.push(format!("betas changed ({})", diff));
            }
        }
        if c.auto_mode_changed {
            parts.push("auto mode toggled".to_string());
        }
        if c.effort_changed {
            parts.push(format!(
                "effort changed ({} → {})",
                if c.prev_effort_value.is_empty() { "default" } else { &c.prev_effort_value },
                if c.new_effort_value.is_empty() { "default" } else { &c.new_effort_value },
            ));
        }
        if c.extra_body_changed {
            parts.push("extra body params changed".to_string());
        }
        if c.cache_control_changed {
            parts.push("cache control scope changed".to_string());
        }
    }

    // Determine reason
    let reason = if !parts.is_empty() {
        parts.join(", ")
    } else if let Some(ms) = time_since_last_assistant_ms {
        if ms > CACHE_TTL_1HOUR.as_millis() as i64 {
            "possible 1h TTL expiry (prompt unchanged)".to_string()
        } else if ms > CACHE_TTL_5MIN.as_millis() as i64 {
            "possible 5min TTL expiry (prompt unchanged)".to_string()
        } else {
            "likely server-side (prompt unchanged, <5min gap)".to_string()
        }
    } else {
        "unknown cause".to_string()
    };

    let report = CacheBreakReport {
        reason: reason.clone(),
        prev_cache_read,
        cache_read: cache_read_tokens,
        cache_creation: cache_creation_tokens,
        call_number: state.call_count,
        changes,
    };

    warn!(
        "[PROMPT CACHE BREAK] {} [source={}, call #{}, cache read: {} → {}, creation: {}]",
        reason, query_source, state.call_count, prev_cache_read, cache_read_tokens, cache_creation_tokens
    );

    Some(report)
}

/// Notify that cache entries are being deleted (expected drop on next call).
pub fn notify_cache_deletion(query_source: &str, agent_id: Option<&str>) {
    let key = match get_tracking_key(query_source, agent_id) {
        Some(k) => k,
        None => return,
    };
    if let Ok(mut tracker) = TRACKER.lock() {
        if let Some(state) = tracker.states.get_mut(&key) {
            state.cache_deletions_pending = true;
        }
    }
}

/// Notify that compaction occurred (reset baseline for next comparison).
pub fn notify_compaction(query_source: &str, agent_id: Option<&str>) {
    let key = match get_tracking_key(query_source, agent_id) {
        Some(k) => k,
        None => return,
    };
    if let Ok(mut tracker) = TRACKER.lock() {
        if let Some(state) = tracker.states.get_mut(&key) {
            state.prev_cache_read_tokens = None;
        }
    }
}

/// Clean up tracking state for a specific agent.
pub fn cleanup_agent_tracking(agent_id: &str) {
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.states.remove(agent_id);
    }
}

/// Reset all tracking state (for testing or session restart).
pub fn reset_all() {
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.states.clear();
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Sanitize tool names for analytics (collapse MCP tools to 'mcp').
fn sanitize_tool_names(names: &[String]) -> String {
    names.iter()
        .map(|n| if n.starts_with("mcp__") { "mcp" } else { n.as_str() })
        .collect::<Vec<_>>()
        .join(", ")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(source: &str, model: &str, tools: &[&str]) -> PromptStateSnapshot {
        PromptStateSnapshot {
            system_text: "You are a helpful assistant.".to_string(),
            tool_schemas_json: tools.iter().map(|t| format!(r#"{{"name":"{}","description":"A tool"}}"#, t)).collect(),
            tool_names: tools.iter().map(|s| s.to_string()).collect(),
            query_source: source.to_string(),
            model: model.to_string(),
            agent_id: None,
            fast_mode: false,
            betas: vec![],
            auto_mode_active: false,
            effort_value: String::new(),
            extra_body_json: None,
            system_char_count: 28,
            cache_control_scope: None,
            is_using_overage: false,
        }
    }

    // Serialize tests that share the global TRACKER state.
    static TEST_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_all();
        guard
    }

    #[test]
    fn tracking_key_for_repl() {
        assert_eq!(
            get_tracking_key("repl_main_thread", None),
            Some("repl_main_thread".to_string())
        );
    }

    #[test]
    fn tracking_key_compact_shares_repl() {
        assert_eq!(
            get_tracking_key("compact", None),
            Some("repl_main_thread".to_string())
        );
    }

    #[test]
    fn tracking_key_untracked_returns_none() {
        assert_eq!(get_tracking_key("speculation", None), None);
        assert_eq!(get_tracking_key("session_memory", None), None);
    }

    #[test]
    fn tracking_key_with_agent_id() {
        assert_eq!(
            get_tracking_key("agent:custom", Some("agent-123")),
            Some("agent-123".to_string())
        );
    }

    #[test]
    fn record_first_state_initializes() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read", "Write"]);
        record_prompt_state(&snap);
        // Should not panic, state initialized
    }

    #[test]
    fn record_unchanged_state_no_pending() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read", "Write"]);
        record_prompt_state(&snap);
        record_prompt_state(&snap);

        let tracker = TRACKER.lock().unwrap();
        let state = tracker.states.get("repl_main_thread").unwrap();
        assert!(state.pending_changes.is_none());
    }

    #[test]
    fn record_model_change_creates_pending() {
        let _guard = setup();
        let snap1 = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap1);

        let snap2 = make_snapshot("repl_main_thread", "claude-opus-4-20250514", &["Read"]);
        record_prompt_state(&snap2);

        let tracker = TRACKER.lock().unwrap();
        let state = tracker.states.get("repl_main_thread").unwrap();
        let changes = state.pending_changes.as_ref().unwrap();
        assert!(changes.model_changed);
        assert_eq!(changes.previous_model, "claude-sonnet-4-20250514");
        assert_eq!(changes.new_model, "claude-opus-4-20250514");
    }

    #[test]
    fn record_tool_added() {
        let _guard = setup();
        let snap1 = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap1);

        let snap2 = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read", "Write"]);
        record_prompt_state(&snap2);

        let tracker = TRACKER.lock().unwrap();
        let state = tracker.states.get("repl_main_thread").unwrap();
        let changes = state.pending_changes.as_ref().unwrap();
        assert!(changes.tool_schemas_changed);
        assert_eq!(changes.added_tool_count, 1);
        assert_eq!(changes.added_tools, vec!["Write"]);
    }

    #[test]
    fn record_tool_removed() {
        let _guard = setup();
        let snap1 = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read", "Write"]);
        record_prompt_state(&snap1);

        let snap2 = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap2);

        let tracker = TRACKER.lock().unwrap();
        let state = tracker.states.get("repl_main_thread").unwrap();
        let changes = state.pending_changes.as_ref().unwrap();
        assert!(changes.tool_schemas_changed);
        assert_eq!(changes.removed_tool_count, 1);
        assert_eq!(changes.removed_tools, vec!["Write"]);
    }

    #[test]
    fn check_no_break_first_call() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap);

        let result = check_response_for_cache_break(
            "repl_main_thread", 10000, 0, None, None,
        );
        assert!(result.is_none()); // First call, no baseline
    }

    #[test]
    fn check_no_break_small_drop() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap);

        // Set baseline
        check_response_for_cache_break("repl_main_thread", 10000, 0, None, None);

        // Small drop (< 5% of 10000 = 500)
        let result = check_response_for_cache_break(
            "repl_main_thread", 9800, 0, None, None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn check_break_detected_large_drop() {
        let _guard = setup();
        let snap1 = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap1);
        check_response_for_cache_break("repl_main_thread", 10000, 0, None, None);

        // Change model
        let snap2 = make_snapshot("repl_main_thread", "claude-opus-4-20250514", &["Read"]);
        record_prompt_state(&snap2);

        // Large drop: 10000 → 0
        let result = check_response_for_cache_break(
            "repl_main_thread", 0, 5000, None, None,
        );
        assert!(result.is_some());
        let report = result.unwrap();
        assert!(report.reason.contains("model changed"));
    }

    #[test]
    fn check_break_ttl_expiry() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap);
        check_response_for_cache_break("repl_main_thread", 10000, 0, None, None);

        // No changes, but large drop with >5min gap
        record_prompt_state(&snap);
        let result = check_response_for_cache_break(
            "repl_main_thread", 0, 5000, Some(6 * 60 * 1000), None,
        );
        assert!(result.is_some());
        let report = result.unwrap();
        assert!(report.reason.contains("5min TTL"));
    }

    #[test]
    fn notify_cache_deletion_suppresses_break() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap);
        check_response_for_cache_break("repl_main_thread", 10000, 0, None, None);

        // Notify deletion
        notify_cache_deletion("repl_main_thread", None);

        // Large drop should be suppressed
        record_prompt_state(&snap);
        let result = check_response_for_cache_break(
            "repl_main_thread", 0, 5000, None, None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn notify_compaction_resets_baseline() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-sonnet-4-20250514", &["Read"]);
        record_prompt_state(&snap);
        check_response_for_cache_break("repl_main_thread", 10000, 0, None, None);

        notify_compaction("repl_main_thread", None);

        // Next call should be treated as first (no baseline)
        record_prompt_state(&snap);
        let result = check_response_for_cache_break(
            "repl_main_thread", 0, 5000, None, None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn excluded_model_skipped() {
        let _guard = setup();
        let snap = make_snapshot("repl_main_thread", "claude-3-haiku-20240307", &["Read"]);
        record_prompt_state(&snap);
        check_response_for_cache_break("repl_main_thread", 10000, 0, None, None);

        let result = check_response_for_cache_break(
            "repl_main_thread", 0, 5000, None, None,
        );
        assert!(result.is_none()); // Haiku excluded
    }

    #[test]
    fn max_tracked_sources_eviction() {
        let _guard = setup();
        // Fill up to MAX_TRACKED_SOURCES
        for i in 0..MAX_TRACKED_SOURCES + 2 {
            let source = format!("agent:custom_{}", i);
            let snap = make_snapshot(&source, "claude-sonnet-4-20250514", &["Read"]);
            record_prompt_state(&snap);
        }

        let tracker = TRACKER.lock().unwrap();
        assert!(tracker.states.len() <= MAX_TRACKED_SOURCES);
    }

    #[test]
    fn sanitize_mcp_tool_names() {
        let names = vec![
            "Read".to_string(),
            "mcp__filesystem__read".to_string(),
            "Write".to_string(),
        ];
        let result = sanitize_tool_names(&names);
        assert_eq!(result, "Read, mcp, Write");
    }

    #[test]
    fn cleanup_agent_tracking_removes_state() {
        let _guard = setup();
        let snap = PromptStateSnapshot {
            query_source: "agent:custom".to_string(),
            agent_id: Some("my-agent".to_string()),
            ..make_snapshot("agent:custom", "claude-sonnet-4-20250514", &["Read"])
        };
        record_prompt_state(&snap);

        {
            let tracker = TRACKER.lock().unwrap();
            assert!(tracker.states.contains_key("my-agent"));
        }

        cleanup_agent_tracking("my-agent");

        {
            let tracker = TRACKER.lock().unwrap();
            assert!(!tracker.states.contains_key("my-agent"));
        }
    }
}
