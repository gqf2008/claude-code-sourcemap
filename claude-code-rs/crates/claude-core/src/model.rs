//! Model routing, resolution, and capability detection.
//!
//! Aligned with the TypeScript `utils/model/model.ts`, `configs.ts`, and
//! `contextWindow.ts`.  Covers:
//!
//! - Model aliases (`sonnet`, `opus`, `haiku`, `best`)
//! - Canonical name resolution (full model ID → short canonical form)
//! - Context-window and output-token limits
//! - API provider detection (first-party, Bedrock, Vertex, Foundry)
//! - Model resolution priority chain

use std::env;

// ── Provider ────────────────────────────────────────────────────────────────

/// API backend provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiProvider {
    FirstParty,
    Bedrock,
    Vertex,
    Foundry,
}

impl ApiProvider {
    /// Detect the API provider from environment variables.
    ///
    /// Priority: Bedrock → Vertex → Foundry → FirstParty
    pub fn detect() -> Self {
        if env_truthy("CLAUDE_CODE_USE_BEDROCK") {
            Self::Bedrock
        } else if env_truthy("CLAUDE_CODE_USE_VERTEX") {
            Self::Vertex
        } else if env_truthy("CLAUDE_CODE_USE_FOUNDRY") {
            Self::Foundry
        } else {
            Self::FirstParty
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FirstParty => "firstParty",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::Foundry => "foundry",
        }
    }
}

// ── Model info ──────────────────────────────────────────────────────────────

/// Static capability info for a model family.
#[derive(Debug, Clone, Copy)]
pub struct ModelCapabilities {
    /// Default context window (tokens).
    pub context_window: u64,
    /// Whether 1M context is available.
    pub supports_1m: bool,
    /// Default max output tokens.
    pub default_max_output: u32,
    /// Upper limit for max output tokens (for recovery escalation).
    pub upper_max_output: u32,
    /// Whether the model supports extended thinking.
    pub supports_thinking: bool,
}

/// Look up capabilities by canonical model name.
pub fn model_capabilities(model: &str) -> ModelCapabilities {
    let c = canonical_name(model);
    match c {
        "claude-opus-4-6" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: true,
            default_max_output: 64_000,
            upper_max_output: 128_000,
            supports_thinking: true,
        },
        "claude-sonnet-4-6" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: true,
            default_max_output: 32_000,
            upper_max_output: 128_000,
            supports_thinking: true,
        },
        "claude-opus-4-5" | "claude-sonnet-4-5" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: false,
            default_max_output: 32_000,
            upper_max_output: 64_000,
            supports_thinking: true,
        },
        "claude-sonnet-4" | "claude-haiku-4-5" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: false,
            default_max_output: 32_000,
            upper_max_output: 64_000,
            supports_thinking: true,
        },
        "claude-opus-4" | "claude-opus-4-1" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: false,
            default_max_output: 32_000,
            upper_max_output: 32_000,
            supports_thinking: true,
        },
        "claude-3-7-sonnet" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: false,
            default_max_output: 32_000,
            upper_max_output: 64_000,
            supports_thinking: true,
        },
        "claude-3-5-sonnet" | "claude-3-5-haiku" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: false,
            default_max_output: 8_192,
            upper_max_output: 8_192,
            supports_thinking: false,
        },
        "claude-3-opus" => ModelCapabilities {
            context_window: 200_000,
            supports_1m: false,
            default_max_output: 4_096,
            upper_max_output: 4_096,
            supports_thinking: false,
        },
        _ => {
            // Third-party model: use provider-aware context window
            let ctx = third_party_context_window(model);
            ModelCapabilities {
                context_window: ctx,
                supports_1m: ctx >= 1_000_000,
                default_max_output: 16_384,
                upper_max_output: 32_000,
                supports_thinking: model.starts_with("o1") || model.starts_with("o3"),
            }
        }
    }
}

// ── Canonical name resolution ───────────────────────────────────────────────

/// Resolve a full model ID (with dates, provider prefixes, etc.) to a short
/// canonical form.  Order: most-specific first.
///
/// Examples:
/// - `"claude-sonnet-4-20250514"` → `"claude-sonnet-4"`
/// - `"us.anthropic.claude-opus-4-6-v1"` → `"claude-opus-4-6"`
/// - `"claude-3-5-haiku@20241022"` → `"claude-3-5-haiku"`
pub fn canonical_name(model: &str) -> &'static str {
    let m = model.to_lowercase();

    // Opus family (most specific first)
    if m.contains("claude-opus-4-6") {
        return "claude-opus-4-6";
    }
    if m.contains("claude-opus-4-5") || m.contains("opus-4.5") {
        return "claude-opus-4-5";
    }
    if m.contains("claude-opus-4-1") || m.contains("opus-4.1") {
        return "claude-opus-4-1";
    }
    if m.contains("claude-opus-4") || m.contains("opus4") {
        return "claude-opus-4";
    }

    // Sonnet family
    if m.contains("claude-sonnet-4-6") || m.contains("sonnet-4.6") {
        return "claude-sonnet-4-6";
    }
    if m.contains("claude-sonnet-4-5") || m.contains("sonnet-4.5") {
        return "claude-sonnet-4-5";
    }
    if m.contains("claude-sonnet-4") || m.contains("sonnet4") {
        return "claude-sonnet-4";
    }

    // Haiku family
    if m.contains("claude-haiku-4-5") || m.contains("haiku-4.5") {
        return "claude-haiku-4-5";
    }

    // Legacy 3.x
    if m.contains("claude-3-7-sonnet") {
        return "claude-3-7-sonnet";
    }
    if m.contains("claude-3-5-sonnet") {
        return "claude-3-5-sonnet";
    }
    if m.contains("claude-3-5-haiku") {
        return "claude-3-5-haiku";
    }
    if m.contains("claude-3-opus") {
        return "claude-3-opus";
    }
    if m.contains("claude-3-sonnet") {
        return "claude-3-sonnet";
    }
    if m.contains("claude-3-haiku") {
        return "claude-3-haiku";
    }

    // Unknown — return generic fallback
    "unknown"
}

// ── Alias resolution ────────────────────────────────────────────────────────

/// Current default model IDs for first-party usage.
pub mod defaults {
    pub const SONNET: &str = "claude-sonnet-4-6";
    pub const OPUS: &str = "claude-opus-4-6";
    pub const HAIKU: &str = "claude-haiku-4-5-20251001";
}

/// Resolve a model alias (e.g. `"sonnet"`, `"opus"`, `"haiku"`, `"best"`)
/// to a concrete model ID.  Returns `None` if the input is not an alias.
pub fn resolve_alias(input: &str) -> Option<&'static str> {
    let stripped = input.trim().to_lowercase();
    // Strip optional [1m] suffix for alias check
    let base = stripped.strip_suffix("[1m]").unwrap_or(&stripped);

    match base {
        "sonnet" => Some(defaults::SONNET),
        "opus" | "best" => Some(defaults::OPUS),
        "haiku" => Some(defaults::HAIKU),
        _ => None,
    }
}

/// Whether the input string contains a `[1m]` suffix requesting 1M context.
pub fn requests_1m_context(input: &str) -> bool {
    input.trim().to_lowercase().ends_with("[1m]")
}

// ── Model resolution priority chain ─────────────────────────────────────────

/// Sources for model selection, in priority order.
pub struct ModelSources<'a> {
    /// `/model` command override (session-level).
    pub session_override: Option<&'a str>,
    /// `--model` flag (startup-level).
    pub cli_flag: Option<&'a str>,
    /// `ANTHROPIC_MODEL` environment variable.
    pub env_var: Option<&'a str>,
    /// User settings file.
    pub settings: Option<&'a str>,
}

/// Resolve the model to use, applying alias expansion and the priority chain.
///
/// Returns the concrete model ID string.
pub fn resolve_model(sources: &ModelSources) -> String {
    let raw = sources
        .session_override
        .or(sources.cli_flag)
        .or(sources.env_var)
        .or(sources.settings)
        .unwrap_or(defaults::SONNET);

    resolve_model_string(raw)
}

/// Resolve a single model string: expand aliases, strip `[1m]` suffix.
pub fn resolve_model_string(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return defaults::SONNET.to_string();
    }

    // Check alias
    if let Some(resolved) = resolve_alias(trimmed) {
        return resolved.to_string();
    }

    // Strip [1m] suffix (context window hint, not part of model ID)
    let base = trimmed
        .strip_suffix("[1m]")
        .or_else(|| trimmed.strip_suffix("[1M]"))
        .unwrap_or(trimmed);

    base.to_string()
}

/// Validate and resolve a model string. Returns `Ok(resolved_model)` if valid,
/// or `Err` with a helpful message listing available aliases and known models.
pub fn validate_model(input: &str) -> Result<String, String> {
    if input.trim().is_empty() {
        return Err("Model name cannot be empty".into());
    }

    let resolved = resolve_model_string(input);

    // If canonical_name maps to a known model, it's valid
    let canonical = canonical_name(&resolved);
    if canonical != "unknown" || resolved.starts_with("claude-") {
        return Ok(resolved);
    }

    // Unknown model — build helpful error
    let aliases = list_aliases();
    let alias_list: Vec<String> = aliases
        .iter()
        .map(|(name, model)| format!("  {} → {}", name, model))
        .collect();

    Err(format!(
        "Unknown model: '{}'\n\nAvailable aliases:\n{}\n\nOr use a full model ID like 'claude-sonnet-4-20250514'",
        input,
        alias_list.join("\n"),
    ))
}

// ── Small/fast model for cheap tasks ────────────────────────────────────────

/// Return the small/fast model for cheap operations (compaction, token counting).
///
/// Priority: `ANTHROPIC_SMALL_FAST_MODEL` env → default Haiku.
/// Matches TS `getSmallFastModel()`.
pub fn small_fast_model() -> String {
    if let Ok(m) = env::var("ANTHROPIC_SMALL_FAST_MODEL") {
        if !m.is_empty() {
            return resolve_model_string(&m);
        }
    }
    default_haiku_model()
}

/// Default Opus model, overridable via `ANTHROPIC_DEFAULT_OPUS_MODEL`.
pub fn default_opus_model() -> String {
    env::var("ANTHROPIC_DEFAULT_OPUS_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| defaults::OPUS.to_string())
}

/// Default Sonnet model, overridable via `ANTHROPIC_DEFAULT_SONNET_MODEL`.
pub fn default_sonnet_model() -> String {
    env::var("ANTHROPIC_DEFAULT_SONNET_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| defaults::SONNET.to_string())
}

/// Default Haiku model, overridable via `ANTHROPIC_DEFAULT_HAIKU_MODEL`.
pub fn default_haiku_model() -> String {
    env::var("ANTHROPIC_DEFAULT_HAIKU_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| defaults::HAIKU.to_string())
}

/// List all available model aliases with their current resolved values.
pub fn list_aliases() -> Vec<(&'static str, String)> {
    vec![
        ("sonnet", default_sonnet_model()),
        ("opus", default_opus_model()),
        ("haiku", default_haiku_model()),
        ("best", default_opus_model()),
    ]
}

/// Human-readable display name for a model.
pub fn display_name(model: &str) -> &'static str {
    match canonical_name(model) {
        "claude-opus-4-6" => "Claude Opus 4.6",
        "claude-opus-4-5" => "Claude Opus 4.5",
        "claude-opus-4-1" => "Claude Opus 4.1",
        "claude-opus-4" => "Claude Opus 4",
        "claude-sonnet-4-6" => "Claude Sonnet 4.6",
        "claude-sonnet-4-5" => "Claude Sonnet 4.5",
        "claude-sonnet-4" => "Claude Sonnet 4",
        "claude-haiku-4-5" => "Claude Haiku 4.5",
        "claude-3-7-sonnet" => "Claude 3.7 Sonnet",
        "claude-3-5-sonnet" => "Claude 3.5 Sonnet",
        "claude-3-5-haiku" => "Claude 3.5 Haiku",
        "claude-3-opus" => "Claude 3 Opus",
        _ => "Unknown", // use display_name_any() for non-Claude models
    }
}

/// Knowledge cutoff date string for the given model.
pub fn knowledge_cutoff(model: &str) -> &'static str {
    match canonical_name(model) {
        "claude-sonnet-4-6" => "August 2025",
        "claude-opus-4-6" | "claude-opus-4-5" => "May 2025",
        "claude-haiku-4-5" => "February 2025",
        "claude-opus-4" | "claude-opus-4-1" | "claude-sonnet-4" | "claude-sonnet-4-5" => {
            "January 2025"
        }
        "claude-3-7-sonnet" | "claude-3-5-sonnet" | "claude-3-5-haiku" => "April 2024",
        _ => "",
    }
}

// ── Provider-specific model ID mapping ──────────────────────────────────────

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
pub fn model_for_provider(canonical: &str, provider: ApiProvider) -> String {
    if let Some(ids) = provider_model_ids(canonical) {
        match provider {
            ApiProvider::FirstParty => ids.first_party.to_string(),
            ApiProvider::Bedrock => ids.bedrock.to_string(),
            ApiProvider::Vertex => ids.vertex.to_string(),
            ApiProvider::Foundry => ids.foundry.to_string(),
        }
    } else {
        // Unknown model — pass through as-is
        canonical.to_string()
    }
}

// ── Sub-agent model selection ───────────────────────────────────────────────

/// Agent type identifiers for model routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    /// Fast research agent (uses Haiku).
    Explore,
    /// General-purpose implementation agent (inherits parent model).
    GeneralPurpose,
    /// Code review agent (uses Sonnet).
    CodeReview,
    /// Planning/architecture agent (uses Sonnet).
    Plan,
}

/// Resolve the model for a sub-agent based on its type and the parent model.
pub fn resolve_agent_model(agent_type: AgentType, parent_model: &str) -> String {
    match agent_type {
        AgentType::Explore => defaults::HAIKU.to_string(),
        AgentType::GeneralPurpose => parent_model.to_string(),
        AgentType::CodeReview => defaults::SONNET.to_string(),
        AgentType::Plan => defaults::SONNET.to_string(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

// ── Third-party provider model registry ─────────────────────────────────────

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

// ── Cost estimation ────────────────────────────────────────────────────────

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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_name() {
        assert_eq!(canonical_name("claude-sonnet-4-20250514"), "claude-sonnet-4");
        assert_eq!(canonical_name("claude-opus-4-6"), "claude-opus-4-6");
        assert_eq!(
            canonical_name("us.anthropic.claude-opus-4-5-20251101-v1:0"),
            "claude-opus-4-5"
        );
        assert_eq!(
            canonical_name("claude-haiku-4-5@20251001"),
            "claude-haiku-4-5"
        );
        assert_eq!(canonical_name("claude-3-5-sonnet-20241022"), "claude-3-5-sonnet");
        assert_eq!(canonical_name("claude-3-7-sonnet-20250219"), "claude-3-7-sonnet");
        assert_eq!(canonical_name("unknown-model"), "unknown");
    }

    #[test]
    fn test_resolve_alias() {
        assert_eq!(resolve_alias("sonnet"), Some(defaults::SONNET));
        assert_eq!(resolve_alias("opus"), Some(defaults::OPUS));
        assert_eq!(resolve_alias("haiku"), Some(defaults::HAIKU));
        assert_eq!(resolve_alias("best"), Some(defaults::OPUS));
        assert_eq!(resolve_alias("sonnet[1m]"), Some(defaults::SONNET));
        assert_eq!(resolve_alias("claude-sonnet-4"), None);
    }

    #[test]
    fn test_requests_1m() {
        assert!(requests_1m_context("sonnet[1m]"));
        assert!(requests_1m_context("opus[1M]"));
        assert!(!requests_1m_context("sonnet"));
        assert!(!requests_1m_context("claude-opus-4-6"));
    }

    #[test]
    fn test_resolve_model_string() {
        assert_eq!(resolve_model_string("sonnet"), defaults::SONNET);
        assert_eq!(resolve_model_string("opus[1m]"), defaults::OPUS);
        assert_eq!(
            resolve_model_string("claude-sonnet-4-20250514"),
            "claude-sonnet-4-20250514"
        );
        assert_eq!(resolve_model_string(""), defaults::SONNET);
    }

    #[test]
    fn test_model_capabilities() {
        let opus46 = model_capabilities("claude-opus-4-6");
        assert_eq!(opus46.default_max_output, 64_000);
        assert_eq!(opus46.upper_max_output, 128_000);
        assert!(opus46.supports_1m);
        assert!(opus46.supports_thinking);

        let sonnet4 = model_capabilities("claude-sonnet-4-20250514");
        assert_eq!(sonnet4.default_max_output, 32_000);
        assert!(!sonnet4.supports_1m);

        let legacy = model_capabilities("claude-3-5-sonnet-20241022");
        assert_eq!(legacy.default_max_output, 8_192);
        assert!(!legacy.supports_thinking);
    }

    #[test]
    fn test_resolve_model_priority() {
        let sources = ModelSources {
            session_override: None,
            cli_flag: Some("opus"),
            env_var: Some("claude-sonnet-4-20250514"),
            settings: None,
        };
        // CLI flag takes priority over env var
        assert_eq!(resolve_model(&sources), defaults::OPUS);

        let sources2 = ModelSources {
            session_override: Some("haiku"),
            cli_flag: Some("opus"),
            env_var: None,
            settings: None,
        };
        // Session override takes priority over CLI flag
        assert_eq!(resolve_model(&sources2), defaults::HAIKU);
    }

    #[test]
    fn test_display_name() {
        assert_eq!(display_name("claude-sonnet-4-20250514"), "Claude Sonnet 4");
        assert_eq!(display_name("claude-opus-4-6"), "Claude Opus 4.6");
        assert_eq!(display_name("claude-haiku-4-5-20251001"), "Claude Haiku 4.5");
    }

    #[test]
    fn test_knowledge_cutoff() {
        assert_eq!(knowledge_cutoff("claude-sonnet-4-6"), "August 2025");
        assert_eq!(knowledge_cutoff("claude-opus-4-6"), "May 2025");
        assert_eq!(knowledge_cutoff("claude-sonnet-4-20250514"), "January 2025");
    }

    #[test]
    fn test_agent_model_routing() {
        let parent = "claude-opus-4-6";
        assert_eq!(resolve_agent_model(AgentType::Explore, parent), defaults::HAIKU);
        assert_eq!(resolve_agent_model(AgentType::GeneralPurpose, parent), parent);
        assert_eq!(resolve_agent_model(AgentType::CodeReview, parent), defaults::SONNET);
    }

    #[test]
    fn test_provider_detection() {
        // Default should be FirstParty (no env vars set in tests)
        // Can't easily test env-based detection in unit tests
        let provider = ApiProvider::FirstParty;
        assert_eq!(provider.as_str(), "firstParty");
    }

    #[test]
    fn test_model_for_provider() {
        let id = model_for_provider("claude-sonnet-4", ApiProvider::Bedrock);
        assert_eq!(id, "us.anthropic.claude-sonnet-4-20250514-v1:0");

        let id2 = model_for_provider("claude-opus-4-6", ApiProvider::Vertex);
        assert_eq!(id2, "claude-opus-4-6");

        let id3 = model_for_provider("custom-model", ApiProvider::FirstParty);
        assert_eq!(id3, "custom-model"); // pass-through for unknown
    }

    // ── Cost estimation ──────────────────────────────────────────────────

    #[test]
    fn test_model_pricing_known_models() {
        // Opus 4.5/4.6 uses the reduced pricing tier
        let opus46 = model_pricing("claude-opus-4-6").unwrap();
        assert!((opus46.input_per_mtok - 5.0).abs() < f64::EPSILON);
        assert!((opus46.output_per_mtok - 25.0).abs() < f64::EPSILON);

        // Opus 4/4.1 uses the original pricing tier
        let opus4 = model_pricing("claude-opus-4-20250514").unwrap();
        assert!((opus4.input_per_mtok - 15.0).abs() < f64::EPSILON);

        let sonnet = model_pricing("claude-sonnet-4-20250514").unwrap();
        assert!((sonnet.input_per_mtok - 3.0).abs() < f64::EPSILON);

        // Haiku 4.5 pricing
        let haiku45 = model_pricing("claude-haiku-4-5").unwrap();
        assert!((haiku45.input_per_mtok - 1.0).abs() < f64::EPSILON);

        // Haiku 3.5 pricing
        let haiku35 = model_pricing("claude-3-5-haiku-20241022").unwrap();
        assert!((haiku35.input_per_mtok - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_model_pricing_unknown_returns_none() {
        assert!(model_pricing("custom-model-xyz").is_none());
    }

    #[test]
    fn test_estimate_cost_sonnet() {
        // 10K input + 2K output + 5K cache read + 1K cache write with Sonnet pricing
        let cost = estimate_cost(
            "claude-sonnet-4",
            10_000,
            2_000,
            5_000,
            1_000,
        );
        // input:  10K/1M * 3.0 = 0.030
        // output: 2K/1M * 15.0 = 0.030
        // cache_read: 5K/1M * 0.3 = 0.0015
        // cache_write: 1K/1M * 3.75 = 0.00375
        let expected = 0.030 + 0.030 + 0.0015 + 0.00375;
        assert!((cost - expected).abs() < 1e-6, "expected {expected}, got {cost}");
    }

    #[test]
    fn test_estimate_cost_unknown_model_returns_zero() {
        let cost = estimate_cost("unknown-model", 100_000, 50_000, 0, 0);
        assert!((cost - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.001), "$0.0010");
        assert_eq!(format_cost(0.42), "$0.42");
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(format_cost(12.345), "$12.35");
    }

    // ── P24 new tests ───────────────────────────────────────────────────

    #[test]
    fn test_small_fast_model_default() {
        // When env var not set, returns Haiku
        std::env::remove_var("ANTHROPIC_SMALL_FAST_MODEL");
        let model = small_fast_model();
        assert!(model.contains("haiku"), "expected haiku, got {}", model);
    }

    #[test]
    fn test_default_model_functions_return_defaults() {
        std::env::remove_var("ANTHROPIC_DEFAULT_OPUS_MODEL");
        std::env::remove_var("ANTHROPIC_DEFAULT_SONNET_MODEL");
        std::env::remove_var("ANTHROPIC_DEFAULT_HAIKU_MODEL");

        assert_eq!(default_opus_model(), defaults::OPUS);
        assert_eq!(default_sonnet_model(), defaults::SONNET);
        assert!(default_haiku_model().contains("haiku"));
    }

    #[test]
    fn test_list_aliases_has_all_entries() {
        let aliases = list_aliases();
        assert_eq!(aliases.len(), 4);
        let names: Vec<&str> = aliases.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"sonnet"));
        assert!(names.contains(&"opus"));
        assert!(names.contains(&"haiku"));
        assert!(names.contains(&"best"));
    }

    #[test]
    fn test_validate_model_alias() {
        let result = validate_model("sonnet");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), defaults::SONNET);
    }

    #[test]
    fn test_validate_model_full_id() {
        let result = validate_model("claude-sonnet-4-20250514");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_validate_model_unknown_but_claude_prefix() {
        // Future models with claude- prefix should be accepted
        let result = validate_model("claude-future-5-0");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_model_invalid() {
        let result = validate_model("gpt-4o");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Unknown model"));
        assert!(err.contains("sonnet"));
    }

    #[test]
    fn test_validate_model_empty() {
        let result = validate_model("");
        assert!(result.is_err());
    }

    // ── P32 multi-provider model tests ──────────────────────────────────

    #[test]
    fn test_validate_model_for_provider_openai() {
        // GPT models should be accepted for openai provider
        let result = validate_model_for_provider("gpt-4o", "openai");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "gpt-4o");
    }

    #[test]
    fn test_validate_model_for_provider_deepseek() {
        let result = validate_model_for_provider("deepseek-chat", "deepseek");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "deepseek-chat");
    }

    #[test]
    fn test_validate_model_for_provider_anthropic_rejects_gpt() {
        let result = validate_model_for_provider("gpt-4o", "anthropic");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_model_for_provider_anthropic_accepts_claude() {
        let result = validate_model_for_provider("sonnet", "anthropic");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_model_for_provider_empty() {
        let result = validate_model_for_provider("", "openai");
        assert!(result.is_err());
    }

    #[test]
    fn test_default_model_for_provider() {
        assert_eq!(default_model_for_provider("openai"), "gpt-4o");
        assert_eq!(default_model_for_provider("deepseek"), "deepseek-chat");
        assert_eq!(default_model_for_provider("ollama"), "llama3.1");
        assert_eq!(default_model_for_provider("anthropic"), defaults::SONNET);
    }

    #[test]
    fn test_third_party_context_window() {
        assert_eq!(third_party_context_window("gpt-4o"), 128_000);
        assert_eq!(third_party_context_window("gpt-4o-mini"), 128_000);
        assert_eq!(third_party_context_window("deepseek-chat"), 64_000);
        assert_eq!(third_party_context_window("llama-3.1-70b"), 128_000);
        assert_eq!(third_party_context_window("gpt-3.5-turbo"), 16_385);
        // Unknown defaults to 128K
        assert_eq!(third_party_context_window("custom-model"), 128_000);
    }

    #[test]
    fn test_model_capabilities_third_party() {
        let gpt4o = model_capabilities("gpt-4o");
        assert_eq!(gpt4o.context_window, 128_000);
        assert!(!gpt4o.supports_thinking);

        let o1 = model_capabilities("o1-preview");
        assert!(o1.supports_thinking);
    }

    #[test]
    fn test_third_party_pricing() {
        let gpt4o = third_party_pricing("gpt-4o").unwrap();
        assert!((gpt4o.input_per_mtok - 2.5).abs() < f64::EPSILON);

        let ds = third_party_pricing("deepseek-chat").unwrap();
        assert!((ds.input_per_mtok - 0.27).abs() < f64::EPSILON);

        assert!(third_party_pricing("unknown-model").is_none());
    }

    #[test]
    fn test_model_pricing_falls_through_to_third_party() {
        // GPT-4o should be priced via third_party_pricing fallback
        let pricing = model_pricing("gpt-4o");
        assert!(pricing.is_some());
        assert!((pricing.unwrap().input_per_mtok - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_display_name_any_claude() {
        let name = display_name_any("claude-opus-4-6");
        assert_eq!(name, "Claude Opus 4.6");
    }

    #[test]
    fn test_display_name_any_openai() {
        assert_eq!(display_name_any("gpt-4o"), "GPT-4o");
        assert_eq!(display_name_any("gpt-4o-mini"), "GPT-4o Mini");
        assert_eq!(display_name_any("o1-preview"), "OpenAI o1");
    }

    #[test]
    fn test_display_name_any_deepseek() {
        assert_eq!(display_name_any("deepseek-chat"), "DeepSeek Chat");
    }

    #[test]
    fn test_display_name_any_unknown_passthrough() {
        assert_eq!(display_name_any("my-custom-model"), "my-custom-model");
    }
}
