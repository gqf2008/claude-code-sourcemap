//! Cost tracking for Claude API usage.
//!
//! Pricing table aligned with the TypeScript `modelCost.ts` source.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use claude_core::message::Usage;

// ---------------------------------------------------------------------------
// Pricing tiers (USD per 1 million tokens)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

/// Sonnet tier: claude-sonnet-4, 3.5v2, 3.7, 4.5, 4.6
const TIER_SONNET: ModelPricing = ModelPricing {
    input: 3.0,
    output: 15.0,
    cache_write: 3.75,
    cache_read: 0.3,
};

/// Opus 4 / 4.1 tier
const TIER_OPUS: ModelPricing = ModelPricing {
    input: 15.0,
    output: 75.0,
    cache_write: 18.75,
    cache_read: 1.5,
};

/// Opus 4.5 / 4.6 tier
const TIER_OPUS_45: ModelPricing = ModelPricing {
    input: 5.0,
    output: 25.0,
    cache_write: 6.25,
    cache_read: 0.5,
};

/// Haiku 3.5 tier
const TIER_HAIKU_35: ModelPricing = ModelPricing {
    input: 0.8,
    output: 4.0,
    cache_write: 1.0,
    cache_read: 0.08,
};

/// Haiku 4.5 tier
const TIER_HAIKU_45: ModelPricing = ModelPricing {
    input: 1.0,
    output: 5.0,
    cache_write: 1.25,
    cache_read: 0.1,
};

/// Look up the pricing tier for a model name.
pub fn pricing_for_model(model: &str) -> ModelPricing {
    match claude_core::model::canonical_name(model) {
        "claude-opus-4-5" | "claude-opus-4-6" => TIER_OPUS_45,
        "claude-opus-4" | "claude-opus-4-1" => TIER_OPUS,
        "claude-haiku-4-5" => TIER_HAIKU_45,
        "claude-3-5-haiku" => TIER_HAIKU_35,
        _ => TIER_SONNET, // sonnet family is the default
    }
}

/// Calculate USD cost from a `Usage` struct and model name.
pub fn calculate_cost(model: &str, usage: &Usage) -> f64 {
    let p = pricing_for_model(model);
    let input = usage.input_tokens as f64 / 1_000_000.0 * p.input;
    let output = usage.output_tokens as f64 / 1_000_000.0 * p.output;
    let cache_write = usage.cache_creation_input_tokens.unwrap_or(0) as f64 / 1_000_000.0 * p.cache_write;
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0) as f64 / 1_000_000.0 * p.cache_read;
    input + output + cache_write + cache_read
}

// ---------------------------------------------------------------------------
// Per-model usage accumulator
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
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
        let mut inner = self.inner.lock().unwrap();
        inner.total_cost_usd += cost;

        let entry = inner.by_model.entry(canonical_model(model).to_string()).or_default();
        entry.input_tokens += usage.input_tokens;
        entry.output_tokens += usage.output_tokens;
        entry.cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
        entry.cache_write_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        entry.cost_usd += cost;
    }

    /// Get the total accumulated USD cost.
    pub fn total_usd(&self) -> f64 {
        self.inner.lock().unwrap().total_cost_usd
    }

    /// Format a human-readable cost summary (aligned with TS `formatTotalCost`).
    pub fn format_summary(&self, total_input: u64, total_output: u64, turn_count: u32) -> String {
        let inner = self.inner.lock().unwrap();
        let mut lines = Vec::new();

        lines.push(format!("  Total cost:   {}", format_usd(inner.total_cost_usd)));
        lines.push(format!("  Total tokens: {} input, {} output", 
            format_number(total_input), format_number(total_output)));
        lines.push(format!("  Turns:        {}", turn_count));

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
                    format_number(usage.cache_write_tokens),
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
    fn test_pricing_lookup() {
        let sonnet = pricing_for_model("claude-sonnet-4-20250514");
        assert!((sonnet.input - 3.0).abs() < 0.001);
        assert!((sonnet.output - 15.0).abs() < 0.001);

        let opus = pricing_for_model("claude-opus-4-20250514");
        assert!((opus.input - 15.0).abs() < 0.001);

        let opus45 = pricing_for_model("claude-opus-4-5-20250601");
        assert!((opus45.input - 5.0).abs() < 0.001);

        let haiku = pricing_for_model("claude-3-5-haiku-20241022");
        assert!((haiku.input - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost() {
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
