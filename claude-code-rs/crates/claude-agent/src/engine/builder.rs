//! QueryEngineBuilder — fluent builder for constructing a QueryEngine.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use claude_api::client::ApiClient;
use claude_core::claude_md::load_claude_md;
use claude_core::config::HooksConfig;
use claude_core::memory::load_memories_for_prompt;
use claude_core::permissions::PermissionMode;
use claude_core::tool::AbortSignal;
use claude_tools::ToolRegistry;
use tokio::sync::RwLock;

use super::ThinkingOverride;
use crate::compact::AutoCompactState;
use crate::compact::AUTO_COMPACT_THRESHOLD;
use crate::coordinator::{AgentTracker, SendMessageTool, TaskStopTool};
use crate::cost::CostTracker;
use crate::dispatch_agent::{AgentChannelMap, CancelTokenMap, DispatchAgentTool, SubAgentConfig};
use crate::executor::ToolExecutor;
use crate::hooks::HookRegistry;
use crate::permissions::PermissionChecker;
use crate::query::QueryConfig;
use crate::state::new_shared_state_with_model;
use crate::system_prompt::{build_system_prompt_ext, coordinator_system_prompt, sections, DynamicSections};

use super::QueryEngine;

pub struct QueryEngineBuilder {
    pub(crate) api_key: String,
    pub(crate) model: Option<String>,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) system_prompt: String,
    pub(crate) max_turns: u32,
    pub(crate) max_tokens: u32,
    pub(crate) permission_checker: PermissionChecker,
    pub(crate) hooks_config: HooksConfig,
    pub(crate) load_claude_md: bool,
    pub(crate) load_memory: bool,
    pub(crate) compact_threshold: u64,
    pub(crate) coordinator_mode: bool,
    pub(crate) allowed_tools: Vec<String>,
    pub(crate) thinking: Option<claude_api::types::ThinkingConfig>,
    pub(crate) append_system_prompt: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) output_style: Option<(String, String)>,
    pub(crate) mcp_instructions: Vec<(String, String)>,
    pub(crate) scratchpad_dir: Option<String>,
    /// API provider name (anthropic, openai, deepseek, ollama, etc.)
    pub(crate) provider: Option<String>,
    /// Override API base URL
    pub(crate) base_url: Option<String>,
}

impl QueryEngineBuilder {
    pub fn new(api_key: impl Into<String>, cwd: impl Into<std::path::PathBuf>) -> Self {
        Self {
            api_key: api_key.into(),
            model: None,
            cwd: cwd.into(),
            system_prompt: String::new(),
            max_turns: 100,
            max_tokens: 16384,
            permission_checker: PermissionChecker::new(PermissionMode::Default, Vec::new()),
            hooks_config: HooksConfig::default(),
            load_claude_md: true,
            load_memory: true,
            compact_threshold: AUTO_COMPACT_THRESHOLD,
            coordinator_mode: false,
            allowed_tools: Vec::new(),
            thinking: None,
            append_system_prompt: None,
            language: None,
            output_style: None,
            mcp_instructions: Vec::new(),
            scratchpad_dir: None,
            provider: None,
            base_url: None,
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn max_turns(mut self, max: u32) -> Self {
        self.max_turns = max;
        self
    }

    #[allow(dead_code)]
    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = max;
        self
    }

    pub fn permission_checker(mut self, checker: PermissionChecker) -> Self {
        self.permission_checker = checker;
        self
    }

    pub fn hooks_config(mut self, config: HooksConfig) -> Self {
        self.hooks_config = config;
        self
    }

    pub fn load_claude_md(mut self, enable: bool) -> Self {
        self.load_claude_md = enable;
        self
    }

    pub fn load_memory(mut self, enable: bool) -> Self {
        self.load_memory = enable;
        self
    }

    pub fn compact_threshold(mut self, tokens: u64) -> Self {
        self.compact_threshold = tokens;
        self
    }

    pub fn coordinator_mode(mut self, enable: bool) -> Self {
        self.coordinator_mode = enable;
        self
    }

    pub fn allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    pub fn thinking(mut self, config: Option<claude_api::types::ThinkingConfig>) -> Self {
        self.thinking = config;
        self
    }

    pub fn append_system_prompt(mut self, text: Option<String>) -> Self {
        self.append_system_prompt = text;
        self
    }

    pub fn language(mut self, lang: Option<String>) -> Self {
        self.language = lang;
        self
    }

    pub fn output_style(mut self, name: String, prompt: String) -> Self {
        self.output_style = Some((name, prompt));
        self
    }

    pub fn mcp_instructions(mut self, instructions: Vec<(String, String)>) -> Self {
        self.mcp_instructions = instructions;
        self
    }

    pub fn scratchpad_dir(mut self, dir: Option<String>) -> Self {
        self.scratchpad_dir = dir;
        self
    }

    /// Set the API provider (anthropic, openai, deepseek, ollama, etc.)
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Override the API base URL
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn build(self) -> QueryEngine {
        let mut client = ApiClient::new(&self.api_key);
        if let Some(ref model) = self.model {
            client = client.with_model(model);
        }
        client = client.with_max_tokens(self.max_tokens);

        // Apply provider backend if not default Anthropic
        if let Some(ref provider) = self.provider {
            if provider != "anthropic" {
                let backend = claude_api::provider::create_backend(
                    provider,
                    &self.api_key,
                    self.base_url.as_deref(),
                );
                client = client.with_backend(backend);
            } else if let Some(ref url) = self.base_url {
                client = client.with_base_url(url);
            }
        } else if let Some(ref url) = self.base_url {
            client = client.with_base_url(url);
        }

        let client = Arc::new(client);
        let mut registry = ToolRegistry::with_defaults();

        // Register Computer Use tools if enabled via environment variable
        if std::env::var("CLAUDE_CODE_COMPUTER_USE").unwrap_or_default() == "1" {
            match crate::computer_use::register_cu_tools(&mut registry) {
                Ok(()) => tracing::info!("Computer Use tools registered"),
                Err(e) => tracing::warn!("Computer Use unavailable: {e}"),
            }
        }

        let permission_checker = Arc::new(self.permission_checker);

        let model_name = self.model.clone().unwrap_or_else(|| "claude-sonnet-4-20250514".into());
        let caps = claude_core::model::model_capabilities(&model_name);

        // Apply context window env var overrides:
        // - CLAUDE_CODE_MAX_CONTEXT_TOKENS: set absolute context window (for large-context providers)
        // - CLAUDE_CODE_AUTO_COMPACT_WINDOW: cap context window (can only reduce, matches TS behavior)
        let effective_context_window = {
            fn env_u64(name: &str) -> Option<u64> {
                std::env::var(name).ok()?.parse().ok().filter(|&v| v > 0)
            }
            let mut cw = caps.context_window;
            if let Some(v) = env_u64("CLAUDE_CODE_MAX_CONTEXT_TOKENS") {
                cw = v;
                tracing::info!("CLAUDE_CODE_MAX_CONTEXT_TOKENS={v} → context_window={cw}");
            }
            if let Some(v) = env_u64("CLAUDE_CODE_AUTO_COMPACT_WINDOW") {
                cw = cw.min(v);
                tracing::info!("CLAUDE_CODE_AUTO_COMPACT_WINDOW={v} → context_window={cw}");
            }
            cw
        };

        // ── Assemble system prompt via modular builder ────────────────────────
        let claude_md_content = if self.load_claude_md {
            load_claude_md(&self.cwd)
        } else {
            String::new()
        };

        let memory_content = if self.load_memory {
            load_memories_for_prompt(&self.cwd).unwrap_or_default()
        } else {
            String::new()
        };

        // Compute the memory directory path for behavioral prompt injection
        // Normalize to forward slashes for consistent display in prompt
        let memory_dir_str = if self.load_memory {
            claude_core::memory::primary_memory_dir(&self.cwd)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        } else {
            None
        };

        let enabled_tool_names: Vec<String> = registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            .map(|t| t.name().to_string())
            .collect();

        let system_prompt = if self.coordinator_mode {
            coordinator_system_prompt()
        } else if self.system_prompt.is_empty() {
            let dynamic = DynamicSections {
                language: self.language.as_deref(),
                output_style: self.output_style.as_ref().map(|(n, p)| (n.as_str(), p.as_str())),
                mcp_instructions: self.mcp_instructions.clone(),
                scratchpad_dir: self.scratchpad_dir.as_deref(),
                memory_dir: memory_dir_str.as_deref(),
                ..Default::default()
            };
            build_system_prompt_ext(
                &self.cwd,
                &model_name,
                &enabled_tool_names,
                &claude_md_content,
                &memory_content,
                &dynamic,
            )
            .text
        } else {
            let mut parts = Vec::new();
            parts.push(self.system_prompt.clone());
            if !claude_md_content.is_empty() {
                parts.push(format!(
                    "\n## Project Instructions (CLAUDE.md)\n\n<project-instructions>\n{}\n</project-instructions>",
                    claude_md_content
                ));
            }
            if let Some(ref dir) = memory_dir_str {
                parts.push(sections::section_memory_behavioral(dir));
            }
            if !memory_content.is_empty() {
                parts.push(format!(
                    "\n## Memory Contents\n\n<memory>\n{}\n</memory>",
                    memory_content
                ));
            }
            parts.join("\n")
        };

        let system_prompt = match self.append_system_prompt {
            Some(ref append) if !append.is_empty() => {
                format!("{}\n\n{}", system_prompt, append)
            }
            _ => system_prompt,
        };

        let sub_registry = Arc::new(ToolRegistry::with_defaults());

        // ── Coordinator mode setup ───────────────────────────────────────────
        let (agent_tracker, notification_rx, coord_cancel_tokens, coord_agent_channels) = if self.coordinator_mode {
            let (tracker, rx) = AgentTracker::new();
            let agent_channels: AgentChannelMap = Arc::new(RwLock::new(HashMap::new()));
            let cancel_tokens: CancelTokenMap = Arc::new(RwLock::new(HashMap::new()));

            registry.register(SendMessageTool {
                tracker: tracker.clone(),
                agent_channels: agent_channels.clone(),
            });
            registry.register(TaskStopTool {
                tracker: tracker.clone(),
                cancel_tokens: cancel_tokens.clone(),
            });

            (
                Some(tracker),
                Some(tokio::sync::Mutex::new(rx)),
                Some(cancel_tokens),
                Some(agent_channels),
            )
        } else {
            (None, None, None, None)
        };

        let dispatch_tool = DispatchAgentTool {
            client: client.clone(),
            registry: sub_registry,
            permission_checker: permission_checker.clone(),
            config: SubAgentConfig {
                model: model_name.clone(),
                max_tokens: self.max_tokens,
                cwd: self.cwd.clone(),
                system_prompt: system_prompt.clone(),
                max_turns: self.max_turns,
                context_window: effective_context_window,
            },
            agent_tracker,
            cancel_tokens: coord_cancel_tokens.clone(),
            agent_channels: coord_agent_channels.clone(),
        };
        registry.register(dispatch_tool);

        let registry = Arc::new(registry);

        let session_id = uuid::Uuid::new_v4().to_string();
        let hooks = Arc::new(HookRegistry::from_config(
            self.hooks_config,
            self.cwd.clone(),
            session_id.clone(),
        ));
        let executor = Arc::new({
            let mut exec = ToolExecutor::with_hooks(
                registry.clone(),
                permission_checker,
                hooks.clone(),
            );
            exec.set_session_id(&session_id);
            exec
        });

        let state = new_shared_state_with_model(model_name.clone());
        let abort_signal = AbortSignal::new();

        QueryEngine {
            client,
            executor,
            registry,
            state,
            config: QueryConfig {
                system_prompt,
                max_turns: self.max_turns,
                max_tokens: self.max_tokens,
                temperature: None,
                thinking: self.thinking.clone(),
                token_budget: 0,
                context_window: effective_context_window,
                auto_compact_state: None, // engine.build_query_config() creates fresh state per submit
            },
            hooks,
            cwd: self.cwd,
            session_id,
            created_at: chrono::Utc::now(),
            compact_threshold: self.compact_threshold,
            abort_signal,
            notification_rx,
            coordinator_mode: self.coordinator_mode,
            allowed_tools: self.allowed_tools,
            cost_tracker: CostTracker::new(),
            cancel_tokens: coord_cancel_tokens,
            agent_channels: coord_agent_channels,
            auto_compact: Arc::new(tokio::sync::Mutex::new(AutoCompactState::new())),
            context_window: effective_context_window,
            break_cache_next: AtomicBool::new(false),
            thinking_override: std::sync::Mutex::new(ThinkingOverride::UseDefault),
        }
    }
}
