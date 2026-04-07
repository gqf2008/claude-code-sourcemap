//! Model pricing and cost estimation.
//!
//! Contains pricing data for Claude and third-party models, and functions
//! to estimate and format API costs.

use super::canonical_name;

/// Pricing per million tokens (input, output, cache_read) in USD.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
}

/// Get pricing for a model. Returns `None` for unknown models.
pub fn model_pricing(model: &str) -> Option<ModelPricing> {
    let c = canonical_name(model);
    match c {
        // Opus 4.5 / 4.6 — reduced pricing tier
        "claude-opus-4-5" | "claude-opus-4-6" => Some(ModelPricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
            cache_read_per_mtok: 0.5,
            cache_write_per_mtok: 6.25,
        }),
        // Opus 4 / 4.1 / legacy 3 — original pricing tier
        "claude-opus-4" | "claude-opus-4-1" | "claude-3-opus" => Some(ModelPricing {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_read_per_mtok: 1.5,
            cache_write_per_mtok: 18.75,
        }),
        // Sonnet family
        "claude-sonnet-4-6" | "claude-sonnet-4-5" | "claude-sonnet-4" | "claude-3-7-sonnet"
        | "claude-3-5-sonnet" => Some(ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.3,
            cache_write_per_mtok: 3.75,
        }),
        // Haiku 4.5
        "claude-haiku-4-5" => Some(ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: 0.1,
            cache_write_per_mtok: 1.25,
        }),
        // Haiku 3.5
        "claude-3-5-haiku" => Some(ModelPricing {
            input_per_mtok: 0.8,
            output_per_mtok: 4.0,
            cache_read_per_mtok: 0.08,
            cache_write_per_mtok: 1.0,
        }),
        _ => third_party_pricing(model),
    }
}

/// Pricing for known third-party models (input, output per million tokens).
pub fn third_party_pricing(model: &str) -> Option<ModelPricing> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") {
        return Some(ModelPricing { input_per_mtok: 0.15, output_per_mtok: 0.60, cache_read_per_mtok: 0.075, cache_write_per_mtok: 0.15 });
    }
    if m.contains("gpt-4o") {
        return Some(ModelPricing { input_per_mtok: 2.5, output_per_mtok: 10.0, cache_read_per_mtok: 1.25, cache_write_per_mtok: 2.5 });
    }
    if m.contains("gpt-4-turbo") {
        return Some(ModelPricing { input_per_mtok: 10.0, output_per_mtok: 30.0, cache_read_per_mtok: 5.0, cache_write_per_mtok: 10.0 });
    }
    if m.starts_with("o1") {
        return Some(ModelPricing { input_per_mtok: 15.0, output_per_mtok: 60.0, cache_read_per_mtok: 7.5, cache_write_per_mtok: 15.0 });
    }
    if m.contains("deepseek-chat") || m.contains("deepseek-coder") {
        return Some(ModelPricing { input_per_mtok: 0.27, output_per_mtok: 1.10, cache_read_per_mtok: 0.07, cache_write_per_mtok: 0.27 });
    }
    None
}

/// Estimate cost in USD for a given set of token counts and model.
pub fn estimate_cost(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
) -> f64 {
    let pricing = match model_pricing(model) {
        Some(p) => p,
        None => return 0.0,
    };

    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_mtok;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_mtok;
    let cache_read_cost = (cache_read_tokens as f64 / 1_000_000.0) * pricing.cache_read_per_mtok;
    let cache_write_cost = (cache_creation_tokens as f64 / 1_000_000.0) * pricing.cache_write_per_mtok;

    input_cost + output_cost + cache_read_cost + cache_write_cost
}

/// Format a cost value as a human-readable string (e.g., "$0.42", "$1.23").
pub fn format_cost(cost_usd: f64) -> String {
    if cost_usd < 0.01 {
        format!("${:.4}", cost_usd)
    } else {
        format!("${:.2}", cost_usd)
    }
}
