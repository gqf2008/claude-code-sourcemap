//! Model capabilities registry — predefined model profiles.
//!
//! Aligned with TS `utils/model/modelOptions.ts`: context window sizes,
//! max output tokens, feature support flags, and model ID resolution.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Capabilities of a specific Claude model.
#[derive(Debug, Clone)]
pub struct ModelCapabilities {
    /// Model identifier (e.g. "claude-sonnet-4-6").
    pub model_id: &'static str,
    /// Human-readable display name.
    pub display_name: &'static str,
    /// Maximum context window size in tokens.
    pub context_window: u64,
    /// Maximum output tokens the model can generate.
    pub max_output_tokens: u32,
    /// Whether the model supports vision (image inputs).
    pub supports_vision: bool,
    /// Whether the model supports extended thinking.
    pub supports_thinking: bool,
    /// Whether the model supports tool use.
    pub supports_tools: bool,
    /// Whether the model supports prompt caching.
    pub supports_caching: bool,
    /// Cost per million input tokens (USD).
    pub cost_per_m_input: f64,
    /// Cost per million output tokens (USD).
    pub cost_per_m_output: f64,
    /// Cost per million cached-read tokens (USD).
    pub cost_per_m_cache_read: f64,
    /// Cost per million cache-write tokens (USD).
    pub cost_per_m_cache_write: f64,
}

/// Default model for interactive sessions.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
/// Fast/cheap model for background tasks, fallback.
pub const FAST_MODEL: &str = "claude-haiku-4-5";

static MODEL_REGISTRY: LazyLock<HashMap<&'static str, ModelCapabilities>> = LazyLock::new(|| {
    let models = vec![
        ModelCapabilities {
            model_id: "claude-sonnet-4-6",
            display_name: "Claude Sonnet 4.6",
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_vision: true,
            supports_thinking: true,
            supports_tools: true,
            supports_caching: true,
            cost_per_m_input: 3.0,
            cost_per_m_output: 15.0,
            cost_per_m_cache_read: 0.3,
            cost_per_m_cache_write: 3.75,
        },
        ModelCapabilities {
            model_id: "claude-sonnet-4-5",
            display_name: "Claude Sonnet 4.5",
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_vision: true,
            supports_thinking: true,
            supports_tools: true,
            supports_caching: true,
            cost_per_m_input: 3.0,
            cost_per_m_output: 15.0,
            cost_per_m_cache_read: 0.3,
            cost_per_m_cache_write: 3.75,
        },
        ModelCapabilities {
            model_id: "claude-haiku-4-5",
            display_name: "Claude Haiku 4.5",
            context_window: 200_000,
            max_output_tokens: 8_192,
            supports_vision: true,
            supports_thinking: false,
            supports_tools: true,
            supports_caching: true,
            cost_per_m_input: 0.80,
            cost_per_m_output: 4.0,
            cost_per_m_cache_read: 0.08,
            cost_per_m_cache_write: 1.0,
        },
        ModelCapabilities {
            model_id: "claude-opus-4-6",
            display_name: "Claude Opus 4.6",
            context_window: 200_000,
            max_output_tokens: 32_768,
            supports_vision: true,
            supports_thinking: true,
            supports_tools: true,
            supports_caching: true,
            cost_per_m_input: 15.0,
            cost_per_m_output: 75.0,
            cost_per_m_cache_read: 1.5,
            cost_per_m_cache_write: 18.75,
        },
        ModelCapabilities {
            model_id: "claude-opus-4-5",
            display_name: "Claude Opus 4.5",
            context_window: 200_000,
            max_output_tokens: 32_768,
            supports_vision: true,
            supports_thinking: true,
            supports_tools: true,
            supports_caching: true,
            cost_per_m_input: 15.0,
            cost_per_m_output: 75.0,
            cost_per_m_cache_read: 1.5,
            cost_per_m_cache_write: 18.75,
        },
    ];
    models.into_iter().map(|m| (m.model_id, m)).collect()
});

/// Look up capabilities for a model by ID.
///
/// Returns `None` for unknown model IDs — callers should fall back to
/// conservative defaults.
pub fn get_capabilities(model_id: &str) -> Option<&'static ModelCapabilities> {
    MODEL_REGISTRY.get(model_id)
}

/// Get the default model capabilities (Sonnet).
pub fn default_model() -> &'static ModelCapabilities {
    MODEL_REGISTRY.get(DEFAULT_MODEL).expect("default model must exist")
}

/// Get the fast/fallback model capabilities (Haiku).
pub fn fallback_model() -> &'static ModelCapabilities {
    MODEL_REGISTRY.get(FAST_MODEL).expect("fallback model must exist")
}

/// List all registered model IDs.
pub fn all_model_ids() -> Vec<&'static str> {
    MODEL_REGISTRY.keys().copied().collect()
}

/// Calculate cost in USD for a given token usage on a specific model.
#[must_use] 
pub fn calculate_cost(
    model_id: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> f64 {
    let caps = match get_capabilities(model_id) {
        Some(c) => c,
        None => return 0.0,
    };
    let input = input_tokens as f64 * caps.cost_per_m_input / 1_000_000.0;
    let output = output_tokens as f64 * caps.cost_per_m_output / 1_000_000.0;
    let cache_read = cache_read_tokens as f64 * caps.cost_per_m_cache_read / 1_000_000.0;
    let cache_write = cache_write_tokens as f64 * caps.cost_per_m_cache_write / 1_000_000.0;
    input + output + cache_read + cache_write
}

/// Resolve a model ID, accepting common abbreviations.
///
/// E.g. "sonnet" → "claude-sonnet-4-6", "haiku" → "claude-haiku-4-5".
#[must_use] 
pub fn resolve_model_id(input: &str) -> &str {
    match input.to_lowercase().as_str() {
        "sonnet" | "sonnet-4" | "sonnet-4-6" => "claude-sonnet-4-6",
        "sonnet-4-5" => "claude-sonnet-4-5",
        "haiku" | "haiku-4" | "haiku-4-5" => "claude-haiku-4-5",
        "opus" | "opus-4" | "opus-4-6" => "claude-opus-4-6",
        "opus-4-5" => "claude-opus-4-5",
        _ => input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_default_model() {
        let caps = default_model();
        assert_eq!(caps.model_id, "claude-sonnet-4-6");
        assert_eq!(caps.context_window, 200_000);
        assert!(caps.supports_vision);
        assert!(caps.supports_thinking);
    }

    #[test]
    fn get_fallback_model() {
        let caps = fallback_model();
        assert_eq!(caps.model_id, "claude-haiku-4-5");
        assert!(!caps.supports_thinking);
        assert!(caps.cost_per_m_input < 1.0);
    }

    #[test]
    fn lookup_known_model() {
        let caps = get_capabilities("claude-opus-4-6").unwrap();
        assert_eq!(caps.display_name, "Claude Opus 4.6");
        assert_eq!(caps.max_output_tokens, 32_768);
    }

    #[test]
    fn lookup_unknown_model_returns_none() {
        assert!(get_capabilities("gpt-4o").is_none());
    }

    #[test]
    fn all_models_listed() {
        let ids = all_model_ids();
        assert!(ids.len() >= 5);
        assert!(ids.contains(&"claude-sonnet-4-6"));
        assert!(ids.contains(&"claude-haiku-4-5"));
    }

    #[test]
    fn cost_calculation_sonnet() {
        let cost = calculate_cost("claude-sonnet-4-6", 1_000_000, 500_000, 200_000, 100_000);
        // input: 3.0, output: 7.5, cache_read: 0.06, cache_write: 0.375
        let expected = 3.0 + 7.5 + 0.06 + 0.375;
        assert!((cost - expected).abs() < 0.001);
    }

    #[test]
    fn cost_unknown_model_is_zero() {
        let cost = calculate_cost("unknown-model", 1_000_000, 1_000_000, 0, 0);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn resolve_abbreviations() {
        assert_eq!(resolve_model_id("sonnet"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_id("haiku"), "claude-haiku-4-5");
        assert_eq!(resolve_model_id("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_id("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_id("custom-model"), "custom-model");
    }

    #[test]
    fn all_models_have_valid_pricing() {
        for id in all_model_ids() {
            let caps = get_capabilities(id).unwrap();
            assert!(caps.cost_per_m_input > 0.0, "{} has zero input cost", id);
            assert!(caps.cost_per_m_output > 0.0, "{} has zero output cost", id);
            assert!(caps.context_window > 0, "{} has zero context", id);
            assert!(caps.max_output_tokens > 0, "{} has zero max output", id);
        }
    }
}
