//! Cost tracking for Claude API usage.
//!
//! Pricing is sourced from [`claude_core::model::model_pricing`] — the single
//! source of truth for per-model pricing tiers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

/// Thread-safe cost tracker that accumulates usage across turns.
#[derive(Debug, Clone)]
pub struct CostTracker {
    inner: Arc<Mutex<CostTrackerInner>>,
}

#[derive(Debug, Default)]
struct CostTrackerInner {
    total_cost_usd: f64,
    by_model: HashMap<String, ModelUsage>,
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
    }

    /// Get the total accumulated USD cost.
    pub fn total_usd(&self) -> f64 {
        self.inner.lock().map(|g| g.total_cost_usd).unwrap_or(0.0)
    }

    /// Format a human-readable cost summary (aligned with TS `formatTotalCost`).
    pub fn format_summary(&self, total_input: u64, total_output: u64, turn_count: u32) -> String {
        let Ok(inner) = self.inner.lock() else {
            return "  (cost data unavailable)".to_string();
        };
        let mut lines = Vec::new();

        lines.push(format!("  Total cost:   {}", format_usd(inner.total_cost_usd)));
        lines.push(format!("  Total tokens: {} input, {} output", 
            format_number(total_input), format_number(total_output)));
        lines.push(format!("  Turns:        {}", turn_count));

        // Aggregate cache stats across all models
        let total_cache_read: u64 = inner.by_model.values().map(|u| u.cache_read_tokens).sum();
        let total_cache_write: u64 = inner.by_model.values().map(|u| u.cache_creation_tokens).sum();
        if total_cache_read > 0 || total_cache_write > 0 {
            let total_cache = total_cache_read + total_cache_write;
            let hit_rate = if total_cache > 0 {
                total_cache_read as f64 / total_cache as f64 * 100.0
            } else { 0.0 };
            lines.push(format!("  Cache:        {} read, {} write ({:.0}% hit rate)",
                format_number(total_cache_read), format_number(total_cache_write), hit_rate));
        }

        if !inner.by_model.is_empty() {
            lines.push(String::new());
            lines.push("  Usage by model:".to_string());
            let mut models: Vec<_> = inner.by_model.iter().collect();
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
fn canonical_model(model: &str) -> &'static str {
    claude_core::model::display_name(model)
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
}
