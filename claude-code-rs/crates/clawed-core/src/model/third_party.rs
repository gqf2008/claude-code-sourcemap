//! Third-party (non-Claude) model registry.
//!
//! Context windows, display names, and validation for OpenAI, DeepSeek,
//! Llama, Mistral, Qwen, Gemini and other models used via openai-compatible.

use super::{canonical_name, defaults, display_name, validate_model};

/// Default model ID for a given CLI provider name.
pub fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-4o",
        "deepseek" => "deepseek-chat",
        "ollama" => "llama3.1",
        "together" => "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        "groq" => "llama-3.1-70b-versatile",
        "openai-compatible" => "gpt-4o",
        _ => defaults::SONNET,
    }
}

/// Context window for known third-party models (tokens).
pub fn third_party_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    // OpenAI models
    if m.contains("gpt-4o") || m.contains("gpt-4-turbo") { return 128_000; }
    if m.contains("gpt-4.1") { return 1_047_576; }
    if m.contains("gpt-5") { return 256_000; }
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") { return 200_000; }
    if m.contains("gpt-3.5") { return 16_385; }
    // DeepSeek
    if m.contains("deepseek") { return 64_000; }
    // Meta Llama
    if m.contains("llama-3.1") || m.contains("llama-3.2") || m.contains("llama3.1") { return 128_000; }
    if m.contains("llama") { return 8_192; }
    // Mistral
    if m.contains("mixtral") { return 32_768; }
    if m.contains("mistral") { return 32_768; }
    // Qwen
    if m.contains("qwen") { return 32_768; }
    // Google Gemini (if used via openai-compatible)
    if m.contains("gemini") { return 1_048_576; }
    // Default for unknown models
    128_000
}

/// Validate a model string for a specific provider. For Anthropic, applies full
/// Claude model validation. For other providers, accepts any non-empty string.
pub fn validate_model_for_provider(input: &str, provider: &str) -> Result<String, String> {
    if input.trim().is_empty() {
        return Err("Model name cannot be empty".into());
    }

    match provider {
        "anthropic" | "bedrock" | "vertex" => validate_model(input),
        _ => {
            // Non-Anthropic: accept any model string, just trim it
            Ok(input.trim().to_string())
        }
    }
}

/// Human-readable display name for any model (Claude or third-party).
pub fn display_name_any(model: &str) -> String {
    let c = canonical_name(model);
    if c != "unknown" {
        return display_name(model).to_string();
    }
    // Third-party: capitalize and clean up
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") { return "GPT-4o Mini".into(); }
    if m.contains("gpt-4o") { return "GPT-4o".into(); }
    if m.contains("gpt-4-turbo") { return "GPT-4 Turbo".into(); }
    if m.contains("gpt-4.1") { return "GPT-4.1".into(); }
    if m.contains("gpt-5") { return "GPT-5".into(); }
    if m.starts_with("o1") { return "OpenAI o1".into(); }
    if m.starts_with("o3") { return "OpenAI o3".into(); }
    if m.contains("deepseek-chat") { return "DeepSeek Chat".into(); }
    if m.contains("deepseek-coder") { return "DeepSeek Coder".into(); }
    if m.contains("llama-3.1") || m.contains("llama3.1") { return "Llama 3.1".into(); }
    if m.contains("mixtral") { return "Mixtral".into(); }
    if m.contains("qwen") { return "Qwen".into(); }
    if m.contains("gemini") { return "Gemini".into(); }
    // Fallback: return as-is
    model.to_string()
}

/// Multi-provider model registry entry.
pub struct ProviderModelIds {
    pub first_party: &'static str,
    pub bedrock: &'static str,
    pub vertex: &'static str,
    pub foundry: &'static str,
}

/// Get provider-specific model IDs for the current defaults.
pub fn provider_model_ids(canonical: &str) -> Option<ProviderModelIds> {
    match canonical {
        "claude-sonnet-4-6" => Some(ProviderModelIds {
            first_party: "claude-sonnet-4-6",
            bedrock: "us.anthropic.claude-sonnet-4-6",
            vertex: "claude-sonnet-4-6",
            foundry: "claude-sonnet-4-6",
        }),
        "claude-opus-4-6" => Some(ProviderModelIds {
            first_party: "claude-opus-4-6",
            bedrock: "us.anthropic.claude-opus-4-6-v1",
            vertex: "claude-opus-4-6",
            foundry: "claude-opus-4-6",
        }),
        "claude-sonnet-4" => Some(ProviderModelIds {
            first_party: "claude-sonnet-4-20250514",
            bedrock: "us.anthropic.claude-sonnet-4-20250514-v1:0",
            vertex: "claude-sonnet-4@20250514",
            foundry: "claude-sonnet-4",
        }),
        "claude-opus-4" => Some(ProviderModelIds {
            first_party: "claude-opus-4-20250514",
            bedrock: "us.anthropic.claude-opus-4-20250514-v1:0",
            vertex: "claude-opus-4@20250514",
            foundry: "claude-opus-4",
        }),
        "claude-haiku-4-5" => Some(ProviderModelIds {
            first_party: "claude-haiku-4-5-20251001",
            bedrock: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            vertex: "claude-haiku-4-5@20251001",
            foundry: "claude-haiku-4-5",
        }),
        "claude-opus-4-5" => Some(ProviderModelIds {
            first_party: "claude-opus-4-5-20251101",
            bedrock: "us.anthropic.claude-opus-4-5-20251101-v1:0",
            vertex: "claude-opus-4-5@20251101",
            foundry: "claude-opus-4-5",
        }),
        "claude-sonnet-4-5" => Some(ProviderModelIds {
            first_party: "claude-sonnet-4-5-20250929",
            bedrock: "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            vertex: "claude-sonnet-4-5@20250929",
            foundry: "claude-sonnet-4-5",
        }),
        _ => None,
    }
}

/// Get the model ID for the detected API provider.
pub fn model_for_provider(canonical: &str, provider: super::ApiProvider) -> String {
    if let Some(ids) = provider_model_ids(canonical) {
        match provider {
            super::ApiProvider::FirstParty => ids.first_party.to_string(),
            super::ApiProvider::Bedrock => ids.bedrock.to_string(),
            super::ApiProvider::Vertex => ids.vertex.to_string(),
            super::ApiProvider::Foundry => ids.foundry.to_string(),
        }
    } else {
        // Unknown model — pass through as-is
        canonical.to_string()
    }
}
