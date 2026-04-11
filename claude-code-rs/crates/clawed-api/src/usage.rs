//! Usage tracker — per-model token aggregation and cost calculation.
//!
//! Aligned with TS `cost-tracker.ts`: accumulates input/output/cache tokens
//! per model, calculates running cost, and formats summary for display.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::model::calculate_cost;
use crate::types::ApiUsage;

/// Aggregated token usage for one model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub api_calls: u32,
    pub cost_usd: f64,
}

/// Tracks token usage and cost across models for a session.
#[derive(Debug, Clone, Default)]
pub struct UsageTracker {
    /// Per-model aggregated usage.
    per_model: HashMap<String, ModelUsageStats>,
    /// Total API calls across all models.
    total_calls: u32,
}

impl UsageTracker {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    /// Record usage from a single API response.
    pub fn record(&mut self, model: &str, usage: &ApiUsage) {
        let entry = self.per_model.entry(model.to_string()).or_default();
        entry.input_tokens += usage.input_tokens;
        entry.output_tokens += usage.output_tokens;
        entry.cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
        entry.cache_write_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        entry.api_calls += 1;
        entry.cost_usd = calculate_cost(
            model,
            entry.input_tokens,
            entry.output_tokens,
            entry.cache_read_tokens,
            entry.cache_write_tokens,
        );
        self.total_calls += 1;
    }

    /// Total cost across all models.
    #[must_use] 
    pub fn total_cost(&self) -> f64 {
        self.per_model.values().map(|s| s.cost_usd).sum()
    }

    /// Total input tokens across all models.
    #[must_use] 
    pub fn total_input_tokens(&self) -> u64 {
        self.per_model.values().map(|s| s.input_tokens).sum()
    }

    /// Total output tokens across all models.
    #[must_use] 
    pub fn total_output_tokens(&self) -> u64 {
        self.per_model.values().map(|s| s.output_tokens).sum()
    }

    /// Total API calls.
    #[must_use] 
    pub const fn total_calls(&self) -> u32 {
        self.total_calls
    }

    /// Get stats for a specific model.
    #[must_use] 
    pub fn model_stats(&self, model: &str) -> Option<&ModelUsageStats> {
        self.per_model.get(model)
    }

    /// Get all per-model stats.
    #[must_use] 
    pub const fn all_model_stats(&self) -> &HashMap<String, ModelUsageStats> {
        &self.per_model
    }

    /// Format a compact usage summary for display.
    ///
    /// Example: "📊 3 calls | 12.5K in / 3.2K out | $0.0523"
    #[must_use] 
    pub fn format_summary(&self) -> String {
        let total_in = self.total_input_tokens();
        let total_out = self.total_output_tokens();
        let cost = self.total_cost();

        format!(
            "📊 {} call{} | {} in / {} out | ${:.4}",
            self.total_calls,
            if self.total_calls == 1 { "" } else { "s" },
            format_token_count(total_in),
            format_token_count(total_out),
            cost,
        )
    }

    /// Format a detailed per-model breakdown.
    #[must_use] 
    pub fn format_detailed(&self) -> String {
        if self.per_model.is_empty() {
            return "No API usage recorded.".to_string();
        }

        let mut lines = vec![format!("📊 Usage Summary ({} total calls)\n", self.total_calls)];

        let mut models: Vec<_> = self.per_model.iter().collect();
        models.sort_by(|a, b| b.1.cost_usd.partial_cmp(&a.1.cost_usd).unwrap_or(std::cmp::Ordering::Equal));

        for (model, stats) in &models {
            lines.push(format!(
                "  {} — {} calls, {} in / {} out, ${:.4}",
                model,
                stats.api_calls,
                format_token_count(stats.input_tokens),
                format_token_count(stats.output_tokens),
                stats.cost_usd,
            ));
            if stats.cache_read_tokens > 0 || stats.cache_write_tokens > 0 {
                lines.push(format!(
                    "    cache: {} read / {} write",
                    format_token_count(stats.cache_read_tokens),
                    format_token_count(stats.cache_write_tokens),
                ));
            }
        }

        lines.push(format!("\n  Total: ${:.4}", self.total_cost()));
        lines.join("\n")
    }

    /// Export per-model stats as a serializable map (for session persistence).
    #[must_use] 
    pub fn to_session_usage(&self) -> HashMap<String, ModelUsageStats> {
        self.per_model.clone()
    }

    /// Restore from previously saved session usage.
    #[must_use] 
    pub fn from_session_usage(saved: HashMap<String, ModelUsageStats>) -> Self {
        let total_calls = saved.values().map(|s| s.api_calls).sum();
        Self {
            per_model: saved,
            total_calls,
        }
    }
}

/// Format a token count for human display: 1234 → "1.2K", 1234567 → "1.2M".
fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_usage(input: u64, output: u64) -> ApiUsage {
        ApiUsage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }
    }

    fn make_usage_cached(input: u64, output: u64, cache_read: u64, cache_write: u64) -> ApiUsage {
        ApiUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: Some(cache_read),
            cache_creation_input_tokens: Some(cache_write),
        }
    }

    #[test]
    fn new_tracker_is_empty() {
        let tracker = UsageTracker::new();
        assert_eq!(tracker.total_calls(), 0);
        assert_eq!(tracker.total_cost(), 0.0);
        assert_eq!(tracker.total_input_tokens(), 0);
    }

    #[test]
    fn record_single_call() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage(1000, 500));

        assert_eq!(tracker.total_calls(), 1);
        assert_eq!(tracker.total_input_tokens(), 1000);
        assert_eq!(tracker.total_output_tokens(), 500);

        let stats = tracker.model_stats("claude-sonnet-4-6").unwrap();
        assert_eq!(stats.api_calls, 1);
        assert_eq!(stats.input_tokens, 1000);
    }

    #[test]
    fn record_multiple_calls_same_model() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage(1000, 500));
        tracker.record("claude-sonnet-4-6", &make_usage(2000, 800));

        assert_eq!(tracker.total_calls(), 2);
        let stats = tracker.model_stats("claude-sonnet-4-6").unwrap();
        assert_eq!(stats.api_calls, 2);
        assert_eq!(stats.input_tokens, 3000);
        assert_eq!(stats.output_tokens, 1300);
    }

    #[test]
    fn record_multiple_models() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage(1000, 500));
        tracker.record("claude-haiku-4-5", &make_usage(2000, 800));

        assert_eq!(tracker.total_calls(), 2);
        assert_eq!(tracker.total_input_tokens(), 3000);

        assert!(tracker.model_stats("claude-sonnet-4-6").is_some());
        assert!(tracker.model_stats("claude-haiku-4-5").is_some());
    }

    #[test]
    fn cached_tokens_tracked() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage_cached(1000, 500, 5000, 2000));

        let stats = tracker.model_stats("claude-sonnet-4-6").unwrap();
        assert_eq!(stats.cache_read_tokens, 5000);
        assert_eq!(stats.cache_write_tokens, 2000);
        assert!(stats.cost_usd > 0.0);
    }

    #[test]
    fn format_summary_basic() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage(15000, 3200));

        let summary = tracker.format_summary();
        assert!(summary.contains("1 call"));
        assert!(summary.contains("15.0K in"));
        assert!(summary.contains("3.2K out"));
        assert!(summary.contains("$"));
    }

    #[test]
    fn format_token_count_units() {
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(1500), "1.5K");
        assert_eq!(format_token_count(1_500_000), "1.5M");
    }

    #[test]
    fn roundtrip_session_usage() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage(1000, 500));
        tracker.record("claude-haiku-4-5", &make_usage(2000, 800));

        let saved = tracker.to_session_usage();
        let restored = UsageTracker::from_session_usage(saved);

        assert_eq!(restored.total_calls(), 2);
        assert_eq!(restored.total_input_tokens(), 3000);
        assert_eq!(restored.total_output_tokens(), 1300);
    }

    #[test]
    fn unknown_model_zero_cost() {
        let mut tracker = UsageTracker::new();
        tracker.record("unknown-model", &make_usage(1000, 500));

        let stats = tracker.model_stats("unknown-model").unwrap();
        assert_eq!(stats.cost_usd, 0.0);
    }

    #[test]
    fn format_detailed_multi_model() {
        let mut tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4-6", &make_usage(10000, 5000));
        tracker.record("claude-haiku-4-5", &make_usage_cached(20000, 8000, 5000, 1000));

        let detailed = tracker.format_detailed();
        assert!(detailed.contains("Usage Summary"));
        assert!(detailed.contains("claude-sonnet-4-6"));
        assert!(detailed.contains("claude-haiku-4-5"));
        assert!(detailed.contains("cache:"));
        assert!(detailed.contains("Total:"));
    }
}
