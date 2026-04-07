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

mod pricing;
mod third_party;

#[cfg(test)]
mod tests;

// Re-export everything for backwards compatibility
pub use pricing::*;
pub use third_party::*;

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

    if let Some(resolved) = resolve_alias(trimmed) {
        return resolved.to_string();
    }

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

    let canonical = canonical_name(&resolved);
    if canonical != "unknown" || resolved.starts_with("claude-") {
        return Ok(resolved);
    }

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
        _ => "Unknown",
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
