//! Tests for the model module.

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
    assert_eq!(resolve_model(&sources), defaults::OPUS);

    let sources2 = ModelSources {
        session_override: Some("haiku"),
        cli_flag: Some("opus"),
        env_var: None,
        settings: None,
    };
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
    assert_eq!(id3, "custom-model");
}

// ── Cost estimation ──────────────────────────────────────────────────

#[test]
fn test_model_pricing_known_models() {
    let opus46 = model_pricing("claude-opus-4-6").unwrap();
    assert!((opus46.input_per_mtok - 5.0).abs() < f64::EPSILON);
    assert!((opus46.output_per_mtok - 25.0).abs() < f64::EPSILON);

    let opus4 = model_pricing("claude-opus-4-20250514").unwrap();
    assert!((opus4.input_per_mtok - 15.0).abs() < f64::EPSILON);

    let sonnet = model_pricing("claude-sonnet-4-20250514").unwrap();
    assert!((sonnet.input_per_mtok - 3.0).abs() < f64::EPSILON);

    let haiku45 = model_pricing("claude-haiku-4-5").unwrap();
    assert!((haiku45.input_per_mtok - 1.0).abs() < f64::EPSILON);

    let haiku35 = model_pricing("claude-3-5-haiku-20241022").unwrap();
    assert!((haiku35.input_per_mtok - 0.8).abs() < f64::EPSILON);
}

#[test]
fn test_model_pricing_unknown_returns_none() {
    assert!(model_pricing("custom-model-xyz").is_none());
}

#[test]
fn test_estimate_cost_sonnet() {
    let cost = estimate_cost(
        "claude-sonnet-4",
        10_000,
        2_000,
        5_000,
        1_000,
    );
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
