//! Cost tracking for Claude API usage.
//!
//! Pricing is sourced from [`claude_core::model::model_pricing`] — the single
//! source of truth for per-model pricing tiers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use claude_core::message::Usage;
use claude_core::model;

/// Calculate USD cost from a `Usage` struct and model name.
///
/// Delegates to [`claude_core::model::model_pricing`] for per-model rates.
/// Returns 0.0 for unknown models.
pub fn calculate_cost(model_name: &str, usage: &Usage) -> f64 {
    model::estimate_cost(
        model_name,
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_input_tokens.unwrap_or(0),
        usage.cache_creation_input_tokens.unwrap_or(0),
    )
}

// ---------------------------------------------------------------------------
// Per-model usage accumulator (uses state::ModelUsage)
// ---------------------------------------------------------------------------

use crate::state::ModelUsage;

/// Time window for filtering cost data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostWindow {
    All,
    Today,
    Week,
    Month,
}

impl CostWindow {
    /// Parse from user input string.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "today" | "day" => Self::Today,
            "week" | "7d" => Self::Week,
            "month" | "30d" => Self::Month,
            _ => Self::All,
        }
    }

    /// Duration from now for this window.
    fn duration(&self) -> Option<Duration> {
        match self {
            Self::All => None,
            Self::Today => Some(Duration::from_secs(24 * 3600)),
            Self::Week => Some(Duration::from_secs(7 * 24 * 3600)),
            Self::Month => Some(Duration::from_secs(30 * 24 * 3600)),
        }
    }
}

/// A single usage record with timestamp.
#[derive(Debug, Clone)]
struct UsageRecord {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cost_usd: f64,
    timestamp: SystemTime,
}

/// Thread-safe cost tracker that accumulates usage across turns.
#[derive(Debug, Clone)]
pub struct CostTracker {
    inner: Arc<Mutex<CostTrackerInner>>,
}

#[derive(Debug, Default)]
struct CostTrackerInner {
    total_cost_usd: f64,
    by_model: HashMap<String, ModelUsage>,
    records: Vec<UsageRecord>,
}

impl CostTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CostTrackerInner::default())),
        }
    }

    /// Add a single API response usage to the running totals.
    pub fn add(&self, model: &str, usage: &Usage) {
        let cost = calculate_cost(model, usage);
        let Ok(mut inner) = self.inner.lock() else {
            tracing::warn!("CostTracker lock poisoned, skipping add");
            return;
        };
        inner.total_cost_usd += cost;

        let entry = inner.by_model.entry(canonical_model(model).to_string()).or_default();
        entry.input_tokens += usage.input_tokens;
        entry.output_tokens += usage.output_tokens;
        entry.cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
        entry.cache_creation_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        entry.api_calls += 1;
        entry.cost_usd += cost;

        inner.records.push(UsageRecord {
            model: canonical_model(model).to_string(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_creation_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            cost_usd: cost,
            timestamp: SystemTime::now(),
        });
    }

    /// Get the total accumulated USD cost.
    pub fn total_usd(&self) -> f64 {
        self.inner.lock().map(|g| g.total_cost_usd).unwrap_or(0.0)
    }

    /// Format a human-readable cost summary (aligned with TS `formatTotalCost`).
    pub fn format_summary(&self, total_input: u64, total_output: u64, turn_count: u32) -> String {
        self.format_summary_window(total_input, total_output, turn_count, CostWindow::All)
    }

    /// Format a cost summary filtered by time window.
    pub fn format_summary_window(&self, total_input: u64, total_output: u64, turn_count: u32, window: CostWindow) -> String {
        let Ok(inner) = self.inner.lock() else {
            return "  (cost data unavailable)".to_string();
        };

        if matches!(window, CostWindow::All) {
            return Self::format_inner(&inner.by_model, inner.total_cost_usd, total_input, total_output, turn_count, "all time");
        }

        // Filter records by time window
        let cutoff = window.duration().and_then(|d| SystemTime::now().checked_sub(d));
        let filtered: Vec<&UsageRecord> = if let Some(cutoff) = cutoff {
            inner.records.iter().filter(|r| r.timestamp >= cutoff).collect()
        } else {
            inner.records.iter().collect()
        };

        // Aggregate filtered records
        let mut by_model: HashMap<String, ModelUsage> = HashMap::new();
        let mut total_cost = 0.0f64;
        let mut filt_input = 0u64;
        let mut filt_output = 0u64;
        let mut filt_turns = 0u32;
        for rec in &filtered {
            total_cost += rec.cost_usd;
            filt_input += rec.input_tokens;
            filt_output += rec.output_tokens;
            filt_turns += 1;
            let entry = by_model.entry(rec.model.clone()).or_default();
            entry.input_tokens += rec.input_tokens;
            entry.output_tokens += rec.output_tokens;
            entry.cache_read_tokens += rec.cache_read_tokens;
            entry.cache_creation_tokens += rec.cache_creation_tokens;
            entry.api_calls += 1;
            entry.cost_usd += rec.cost_usd;
        }

        let label = match window {
            CostWindow::Today => "today",
            CostWindow::Week => "past 7 days",
            CostWindow::Month => "past 30 days",
            CostWindow::All => "all time",
        };

        Self::format_inner(&by_model, total_cost, filt_input, filt_output, filt_turns, label)
    }

    fn format_inner(by_model: &HashMap<String, ModelUsage>, total_cost: f64, total_input: u64, total_output: u64, turn_count: u32, period: &str) -> String {
        let mut lines = Vec::new();

        lines.push(format!("  Period:       {}", period));
        lines.push(format!("  Total cost:   {}", format_usd(total_cost)));
        lines.push(format!("  Total tokens: {} input, {} output", 
            format_number(total_input), format_number(total_output)));
        lines.push(format!("  API calls:    {}", turn_count));

        let total_cache_read: u64 = by_model.values().map(|u| u.cache_read_tokens).sum();
        let total_cache_write: u64 = by_model.values().map(|u| u.cache_creation_tokens).sum();
        if total_cache_read > 0 || total_cache_write > 0 {
            let total_cache = total_cache_read + total_cache_write;
            let hit_rate = if total_cache > 0 {
                total_cache_read as f64 / total_cache as f64 * 100.0
            } else { 0.0 };
            lines.push(format!("  Cache:        {} read, {} write ({:.0}% hit rate)",
                format_number(total_cache_read), format_number(total_cache_write), hit_rate));
        }

        if !by_model.is_empty() {
            lines.push(String::new());
            lines.push("  Usage by model:".to_string());
            let mut models: Vec<_> = by_model.iter().collect();
            models.sort_by(|a, b| b.1.cost_usd.partial_cmp(&a.1.cost_usd).unwrap_or(std::cmp::Ordering::Equal));
            for (model, usage) in models {
                lines.push(format!(
                    "    {}: {} in, {} out, {} cache_read, {} cache_write ({})",
                    model,
                    format_number(usage.input_tokens),
                    format_number(usage.output_tokens),
                    format_number(usage.cache_read_tokens),
                    format_number(usage.cache_creation_tokens),
                    format_usd(usage.cost_usd),
                ));
            }
        }

        lines.join("\n")
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Shorten model names for display.
fn canonical_model(model: &str) -> String {
    claude_core::model::display_name_any(model)
}

fn format_usd(cost: f64) -> String {
    if cost >= 0.5 {
        format!("${:.2}", cost)
    } else if cost >= 0.0001 {
        format!("${:.4}", cost)
    } else {
        "$0.00".to_string()
    }
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_cost_sonnet() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_creation_input_tokens: Some(100_000),
            cache_read_input_tokens: Some(200_000),
        };
        // Sonnet: 1M * $3 + 0.5M * $15 + 0.1M * $3.75 + 0.2M * $0.30
        // = $3.00 + $7.50 + $0.375 + $0.06 = $10.935
        let cost = calculate_cost("claude-sonnet-4-20250514", &usage);
        assert!((cost - 10.935).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost_opus_45() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        // Opus 4.5: 1M * $5 = $5.00
        let cost = calculate_cost("claude-opus-4-5-20250601", &usage);
        assert!((cost - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost_opus_4() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        // Opus 4: 1M * $15 = $15.00
        let cost = calculate_cost("claude-opus-4-20250514", &usage);
        assert!((cost - 15.0).abs() < 0.001);
    }

    #[test]
    fn test_cost_tracker() {
        let tracker = CostTracker::new();
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 5_000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        tracker.add("claude-sonnet-4-20250514", &usage);
        tracker.add("claude-sonnet-4-20250514", &usage);
        // 2 × (10K * $3/M + 5K * $15/M) = 2 × ($0.03 + $0.075) = $0.21
        assert!((tracker.total_usd() - 0.21).abs() < 0.001);
    }

    #[test]
    fn test_format_usd() {
        assert_eq!(format_usd(12.345), "$12.35");
        assert_eq!(format_usd(0.1234), "$0.1234");
        assert_eq!(format_usd(0.00001), "$0.00");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(500), "500");
        assert_eq!(format_number(1_500), "1.5K");
        assert_eq!(format_number(2_500_000), "2.5M");
    }

    #[test]
    fn test_cost_window_parse() {
        assert_eq!(CostWindow::parse("today"), CostWindow::Today);
        assert_eq!(CostWindow::parse("week"), CostWindow::Week);
        assert_eq!(CostWindow::parse("month"), CostWindow::Month);
        assert_eq!(CostWindow::parse(""), CostWindow::All);
        assert_eq!(CostWindow::parse("7d"), CostWindow::Week);
        assert_eq!(CostWindow::parse("30d"), CostWindow::Month);
    }

    #[test]
    fn test_cost_tracker_records_timestamps() {
        let tracker = CostTracker::new();
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 5_000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        tracker.add("claude-sonnet-4-20250514", &usage);
        // "today" window should include the record we just added
        let summary = tracker.format_summary_window(10_000, 5_000, 1, CostWindow::Today);
        assert!(summary.contains("today"));
        assert!(summary.contains("Sonnet"));
    }

    #[test]
    fn test_cost_window_all_shows_all_time() {
        let tracker = CostTracker::new();
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        tracker.add("claude-sonnet-4-20250514", &usage);
        let summary = tracker.format_summary_window(1_000, 500, 1, CostWindow::All);
        assert!(summary.contains("all time"));
    }
}
