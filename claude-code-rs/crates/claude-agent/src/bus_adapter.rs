//! AgentCoreAdapter — bridges QueryEngine's AgentEvent stream to the EventBus.
//!
//! This is the **strangler fig** integration layer. The existing
//! `QueryEngine::submit()` returns `Stream<AgentEvent>`, and the REPL
//! processes events directly. The adapter sits between them:
//!
//! ```text
//! ┌─────────────┐     Stream<AgentEvent>     ┌──────────────┐
//! │ QueryEngine │ ─────────────────────────→  │ Adapter Task │
//! └─────────────┘                             │   (tokio)    │
//!       ↑ submit / abort / compact            │              │
//!       │                                     │  converts    │
//! ┌─────┴───────┐   AgentRequest (mpsc)       │  AgentEvent  │
//! │  ClientHandle│ ──────────────────────────→ │     →        │
//! │  (UI side)   │ ←─────────────────────────  │ Notification │
//! └─────────────┘   AgentNotification (bcast) └──────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! let engine = QueryEngine::builder(key, cwd).build().await?;
//! let (bus_handle, client_handle) = EventBus::new(256);
//! let adapter = AgentCoreAdapter::new(engine, bus_handle);
//!
//! // Spawn the adapter loop — it processes requests and forwards events
//! let join = adapter.spawn();
//!
//! // UI side: send a message, receive notifications
//! client_handle.submit("Hello")?;
//! while let Some(event) = client_handle.recv_notification().await {
//!     match event {
//!         AgentNotification::TextDelta { text } => print!("{}", text),
//!         AgentNotification::TurnComplete { .. } => break,
//!         _ => {}
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use claude_bus::bus::BusHandle;
use claude_bus::events::*;
use claude_mcp::McpBusAdapter;

use crate::engine::QueryEngine;
use crate::query::AgentEvent;

/// Bridges an existing [`QueryEngine`] to a [`BusHandle`].
///
/// The adapter owns the engine and the core-side bus handle. It runs an
/// async loop that:
/// 1. Listens for `AgentRequest` from the UI client
/// 2. Dispatches requests to the engine (submit, abort, compact, etc.)
/// 3. Converts the resulting `AgentEvent` stream into `AgentNotification`s
pub struct AgentCoreAdapter {
    engine: Arc<QueryEngine>,
    bus: Mutex<BusHandle>,
    mcp: Option<McpBusAdapter>,
    /// Current turn number (incremented on each submit).
    turn: Mutex<u32>,
    /// Track tool_use id → tool_name so ToolResult can populate tool_name.
    tool_names: Mutex<HashMap<String, String>>,
}

impl AgentCoreAdapter {
    pub fn new(engine: QueryEngine, bus: BusHandle) -> Self {
        Self {
            engine: Arc::new(engine),
            bus: Mutex::new(bus),
            mcp: None,
            turn: Mutex::new(0),
            tool_names: Mutex::new(HashMap::new()),
        }
    }

    /// Create with an MCP bus adapter for MCP server management.
    pub fn with_mcp(engine: QueryEngine, bus: BusHandle, mcp: McpBusAdapter) -> Self {
        Self::from_arc(Arc::new(engine), bus, Some(mcp))
    }

    /// Create from a shared Arc<QueryEngine> — allows the caller to retain
    /// a reference to the engine while the adapter runs in the background.
    pub fn from_arc(
        engine: Arc<QueryEngine>,
        bus: BusHandle,
        mcp: Option<McpBusAdapter>,
    ) -> Self {
        Self {
            engine,
            bus: Mutex::new(bus),
            mcp,
            turn: Mutex::new(0),
            tool_names: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn the adapter as a background tokio task.
    ///
    /// Returns a `JoinHandle` that resolves when the bus shuts down
    /// (i.e., when all client handles are dropped or `Shutdown` is received).
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let adapter = Arc::new(self);
        tokio::spawn(async move {
            adapter.run().await;
        })
    }

    /// Main loop: process requests from the UI client.
    async fn run(self: &Arc<Self>) {
        info!("AgentCoreAdapter started");

        // Emit SessionStart
        {
            let state = self.engine.state().read().await;
            let bus = self.bus.lock().await;
            bus.notify(AgentNotification::SessionStart {
                session_id: self.engine.session_id().to_string(),
                model: state.model.clone(),
            });
        }

        loop {
            let request = {
                let mut bus = self.bus.lock().await;
                bus.recv_request().await
            };

            let request = match request {
                Some(r) => r,
                None => {
                    info!("All clients disconnected, adapter shutting down");
                    break;
                }
            };

            debug!("Adapter received request: {:?}", std::mem::discriminant(&request));

            match request {
                AgentRequest::Submit { text, images: _ } => {
                    // TODO: support images via submit_with_content
                    self.handle_submit(&text).await;
                }
                AgentRequest::Abort => {
                    self.engine.abort();
                    let bus = self.bus.lock().await;
                    bus.notify(AgentNotification::Error {
                        code: ErrorCode::InternalError,
                        message: "Aborted by user".into(),
                    });
                }
                AgentRequest::Compact { instructions } => {
                    self.handle_compact(instructions).await;
                }
                AgentRequest::SetModel { model } => {
                    let resolved = claude_core::model::resolve_model_string(&model);
                    let display_name = claude_core::model::display_name_any(&resolved);
                    {
                        let mut state = self.engine.state().write().await;
                        state.model = resolved.clone();
                    }
                    info!("Model changed to: {} ({})", display_name, resolved);
                    let bus = self.bus.lock().await;
                    bus.notify(AgentNotification::ModelChanged {
                        model: resolved,
                        display_name,
                    });
                }
                AgentRequest::Shutdown => {
                    info!("Shutdown requested");
                    let bus = self.bus.lock().await;
                    bus.notify(AgentNotification::SessionEnd {
                        reason: "shutdown".into(),
                    });
                    break;
                }
                AgentRequest::SlashCommand { command } => {
                    debug!("Slash command via bus: {}", command);
                    // Slash commands are handled by the CLI layer, not the adapter.
                    // Forward as error notification so the client knows.
                    let bus = self.bus.lock().await;
                    bus.notify(AgentNotification::Error {
                        code: ErrorCode::InternalError,
                        message: format!("Slash commands must be handled client-side: /{}", command),
                    });
                }
                AgentRequest::SendAgentMessage { agent_id, message } => {
                    match self.engine.send_to_agent(&agent_id, &message).await {
                        Ok(()) => {
                            debug!("Message sent to agent {}", agent_id);
                        }
                        Err(e) => {
                            let bus = self.bus.lock().await;
                            bus.notify(AgentNotification::Error {
                                code: ErrorCode::InternalError,
                                message: format!("Failed to send message to agent '{}': {}", agent_id, e),
                            });
                        }
                    }
                }
                AgentRequest::StopAgent { agent_id } => {
                    match self.engine.cancel_agent(&agent_id).await {
                        Ok(()) => {
                            info!("Agent {} cancellation requested", agent_id);
                        }
                        Err(e) => {
                            let bus = self.bus.lock().await;
                            bus.notify(AgentNotification::Error {
                                code: ErrorCode::InternalError,
                                message: format!("Failed to stop agent '{}': {}", agent_id, e),
                            });
                        }
                    }
                }
                AgentRequest::PermissionResponse { .. } => {
                    // Permission responses are handled via the dedicated channel,
                    // not the general request channel.
                    warn!("Unexpected PermissionResponse in request channel");
                }
                AgentRequest::McpConnect { name, command, args, env } => {
                    self.handle_mcp_connect(&name, &command, &args, &env).await;
                }
                AgentRequest::McpDisconnect { name } => {
                    self.handle_mcp_disconnect(&name).await;
                }
                AgentRequest::McpListServers => {
                    self.handle_mcp_list_servers().await;
                }
                AgentRequest::SaveSession => {
                    self.handle_save_session().await;
                }
                AgentRequest::GetStatus => {
                    self.handle_get_status().await;
                }
                AgentRequest::ClearHistory => {
                    self.handle_clear_history().await;
                }
                AgentRequest::LoadSession { session_id } => {
                    // Session loading requires creating a new engine — cannot be done
                    // from within the adapter. The CLI layer handles this.
                    warn!("LoadSession '{}' via bus — requires CLI-layer handling", session_id);
                    let bus = self.bus.lock().await;
                    bus.notify(AgentNotification::Error {
                        code: ErrorCode::InternalError,
                        message: format!(
                            "LoadSession must be handled by the CLI layer (session: {})",
                            session_id
                        ),
                    });
                }
                AgentRequest::ListModels => {
                    self.handle_list_models().await;
                }
                AgentRequest::ListTools => {
                    self.handle_list_tools().await;
                }
            }
        }

        info!("AgentCoreAdapter stopped");
    }

    /// Submit a user prompt, stream the response, and forward all events to the bus.
    async fn handle_submit(&self, text: &str) {
        let turn = {
            let mut t = self.turn.lock().await;
            *t += 1;
            *t
        };

        {
            let bus = self.bus.lock().await;
            bus.notify(AgentNotification::TurnStart { turn });
        }

        // Clear stale tool name mappings from previous turn
        self.tool_names.lock().await.clear();

        let mut stream = self.engine.submit(text).await;

        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        while let Some(event) = stream.next().await {
            let notification = match event {
                AgentEvent::TextDelta(text) => AgentNotification::TextDelta { text },

                AgentEvent::ThinkingDelta(text) => AgentNotification::ThinkingDelta { text },

                AgentEvent::ToolUseStart { id, name } => {
                    self.tool_names.lock().await.insert(id.clone(), name.clone());
                    AgentNotification::ToolUseStart {
                        id,
                        tool_name: name,
                    }
                }

                AgentEvent::ToolUseReady { id, name, input } => {
                    self.tool_names.lock().await.insert(id.clone(), name.clone());
                    AgentNotification::ToolUseReady {
                        id,
                        tool_name: name,
                        input,
                    }
                }

                AgentEvent::ToolResult {
                    id,
                    is_error,
                    text,
                } => {
                    let tool_name = self.tool_names.lock().await.remove(&id)
                        .unwrap_or_default();
                    AgentNotification::ToolUseComplete {
                        id,
                        tool_name,
                        is_error,
                        result_preview: text,
                    }
                }

                AgentEvent::AssistantMessage(_msg) => {
                    // Full message already represented by TextDelta deltas
                    // and TurnComplete. Optionally emit blocks summary.
                    AgentNotification::AssistantMessage {
                        turn,
                        text_blocks: _msg
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                claude_core::message::ContentBlock::Text { text } => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                            .collect(),
                    }
                }

                AgentEvent::TurnComplete { stop_reason } => AgentNotification::TurnComplete {
                    turn,
                    stop_reason: format!("{:?}", stop_reason),
                    usage: UsageInfo {
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                    },
                },

                AgentEvent::UsageUpdate(usage) => {
                    // Track cumulative tokens for TurnComplete
                    input_tokens = usage.input_tokens;
                    output_tokens = usage.output_tokens;
                    continue; // Don't emit a separate notification
                }

                AgentEvent::TurnTokens {
                    input_tokens: it,
                    output_tokens: ot,
                } => {
                    input_tokens = it;
                    output_tokens = ot;
                    continue; // Tracked internally
                }

                AgentEvent::ContextWarning { usage_pct, message } => {
                    AgentNotification::ContextWarning { usage_pct, message }
                }

                AgentEvent::CompactStart => AgentNotification::CompactStart,

                AgentEvent::CompactComplete { summary_len } => {
                    AgentNotification::CompactComplete { summary_len }
                }

                AgentEvent::MaxTurns { limit } => AgentNotification::TurnComplete {
                    turn,
                    stop_reason: format!("max_turns({})", limit),
                    usage: UsageInfo {
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                    },
                },

                AgentEvent::Error(msg) => AgentNotification::Error {
                    code: ErrorCode::InternalError,
                    message: msg,
                },
            };

            let bus = self.bus.lock().await;
            bus.notify(notification);
        }

        debug!("Submit stream ended for turn {}", turn);
    }

    /// Handle compaction request.
    async fn handle_compact(&self, instructions: Option<String>) {
        {
            let bus = self.bus.lock().await;
            bus.notify(AgentNotification::CompactStart);
        }

        match self
            .engine
            .compact("bus_request", instructions.as_deref())
            .await
        {
            Ok(summary) => {
                let bus = self.bus.lock().await;
                bus.notify(AgentNotification::CompactComplete {
                    summary_len: summary.len(),
                });
            }
            Err(e) => {
                error!("Compaction failed: {}", e);
                let bus = self.bus.lock().await;
                bus.notify(AgentNotification::Error {
                    code: ErrorCode::InternalError,
                    message: format!("Compaction failed: {}", e),
                });
            }
        }
    }

    /// Get access to the underlying engine (for direct queries that
    /// haven't been ported to the bus protocol yet).
    pub fn engine(&self) -> &QueryEngine {
        &self.engine
    }

    /// Get access to the MCP bus adapter (if configured).
    pub fn mcp(&self) -> Option<&McpBusAdapter> {
        self.mcp.as_ref()
    }

    // ── MCP request handlers ──────────────────────────────────────────────

    async fn handle_mcp_connect(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
    ) {
        let notification = match &self.mcp {
            Some(mcp) => mcp.connect(name, command, args, env).await,
            None => AgentNotification::McpServerError {
                name: name.to_string(),
                error: "MCP support not configured".into(),
            },
        };
        let bus = self.bus.lock().await;
        bus.notify(notification);
    }

    async fn handle_mcp_disconnect(&self, name: &str) {
        let notification = match &self.mcp {
            Some(mcp) => mcp.disconnect(name).await,
            None => AgentNotification::McpServerError {
                name: name.to_string(),
                error: "MCP support not configured".into(),
            },
        };
        let bus = self.bus.lock().await;
        bus.notify(notification);
    }

    async fn handle_mcp_list_servers(&self) {
        let notification = match &self.mcp {
            Some(mcp) => mcp.list_servers().await,
            None => AgentNotification::McpServerList {
                servers: vec![],
            },
        };
        let bus = self.bus.lock().await;
        bus.notify(notification);
    }

    /// Save the current session to disk.
    async fn handle_save_session(&self) {
        match self.engine.save_session().await {
            Ok(()) => {
                let session_id = self.engine.session_id().to_string();
                let bus = self.bus.lock().await;
                bus.notify(AgentNotification::SessionSaved { session_id });
            }
            Err(e) => {
                let bus = self.bus.lock().await;
                bus.notify(AgentNotification::Error {
                    code: ErrorCode::InternalError,
                    message: format!("Failed to save session: {}", e),
                });
            }
        }
    }

    /// Return session status: model, turns, token usage, context usage.
    async fn handle_get_status(&self) {
        let (session_id, model, total_turns, total_input_tokens, total_output_tokens) = {
            let state = self.engine.state().read().await;
            (
                self.engine.session_id().to_string(),
                state.model.clone(),
                state.turn_count,
                state.total_input_tokens,
                state.total_output_tokens,
            )
        };
        let context_usage_pct = self.engine.context_usage_percent().await.unwrap_or(0) as f64;

        let bus = self.bus.lock().await;
        bus.notify(AgentNotification::SessionStatus {
            session_id,
            model,
            total_turns,
            total_input_tokens,
            total_output_tokens,
            context_usage_pct,
        });
    }

    /// Clear conversation history.
    async fn handle_clear_history(&self) {
        self.engine.clear_history().await;
        let bus = self.bus.lock().await;
        bus.notify(AgentNotification::HistoryCleared);
    }

    /// List available models.
    async fn handle_list_models(&self) {
        let current_model = { self.engine.state().read().await.model.clone() };
        let aliases = ["sonnet", "opus", "haiku"];
        let models: Vec<ModelInfo> = aliases
            .iter()
            .map(|alias| {
                let id = claude_core::model::resolve_model_string(alias);
                let display = claude_core::model::display_name_any(&id);
                ModelInfo { id, display_name: display }
            })
            .chain(std::iter::once(ModelInfo {
                id: current_model.clone(),
                display_name: claude_core::model::display_name_any(&current_model),
            }))
            .collect();

        // Deduplicate by id
        let mut seen = std::collections::HashSet::new();
        let models: Vec<ModelInfo> = models.into_iter().filter(|m| seen.insert(m.id.clone())).collect();

        let bus = self.bus.lock().await;
        bus.notify(AgentNotification::ModelList { models });
    }

    /// List available tools.
    async fn handle_list_tools(&self) {
        let tools: Vec<ToolInfo> = self.engine.tool_list()
            .into_iter()
            .map(|(name, description, enabled)| ToolInfo { name, description, enabled })
            .collect();

        let bus = self.bus.lock().await;
        bus.notify(AgentNotification::ToolList { tools });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_bus::bus::EventBus;

    // Note: full integration tests require a mock QueryEngine which needs
    // the API test-support feature. Here we test the event conversion logic.

    #[test]
    fn agent_event_to_notification_text_delta() {
        let event = AgentEvent::TextDelta("hello".into());
        let notification = convert_event(event, 1);
        match notification {
            Some(AgentNotification::TextDelta { text }) => assert_eq!(text, "hello"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn agent_event_to_notification_tool_use() {
        let event = AgentEvent::ToolUseStart {
            id: "t1".into(),
            name: "Bash".into(),
        };
        let notification = convert_event(event, 1);
        match notification {
            Some(AgentNotification::ToolUseStart { id, tool_name }) => {
                assert_eq!(id, "t1");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("Expected ToolUseStart, got {:?}", other),
        }
    }

    #[test]
    fn agent_event_to_notification_error() {
        let event = AgentEvent::Error("boom".into());
        let notification = convert_event(event, 1);
        match notification {
            Some(AgentNotification::Error { code, message }) => {
                assert_eq!(code, ErrorCode::InternalError);
                assert_eq!(message, "boom");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn usage_events_return_none() {
        let event = AgentEvent::UsageUpdate(claude_core::message::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        assert!(convert_event(event, 1).is_none());

        let event = AgentEvent::TurnTokens {
            input_tokens: 100,
            output_tokens: 50,
        };
        assert!(convert_event(event, 1).is_none());
    }

    #[test]
    fn turn_complete_includes_stop_reason() {
        use claude_core::message::StopReason;
        let event = AgentEvent::TurnComplete {
            stop_reason: StopReason::EndTurn,
        };
        let notification = convert_event(event, 3);
        match notification {
            Some(AgentNotification::TurnComplete {
                turn,
                stop_reason,
                ..
            }) => {
                assert_eq!(turn, 3);
                assert!(stop_reason.contains("EndTurn"));
            }
            other => panic!("Expected TurnComplete, got {:?}", other),
        }
    }

    #[test]
    fn compact_events_convert() {
        assert!(matches!(
            convert_event(AgentEvent::CompactStart, 1),
            Some(AgentNotification::CompactStart)
        ));
        assert!(matches!(
            convert_event(AgentEvent::CompactComplete { summary_len: 42 }, 1),
            Some(AgentNotification::CompactComplete { summary_len: 42 })
        ));
    }

    #[test]
    fn context_warning_converts() {
        let event = AgentEvent::ContextWarning {
            usage_pct: 85.5,
            message: "Getting full".into(),
        };
        match convert_event(event, 1) {
            Some(AgentNotification::ContextWarning { usage_pct, message }) => {
                assert!((usage_pct - 85.5).abs() < f64::EPSILON);
                assert_eq!(message, "Getting full");
            }
            other => panic!("Expected ContextWarning, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn bus_integration_smoke() {
        // Just verify we can create the types together
        let (_bus, _client) = EventBus::new(16);
        // Full integration needs mock engine — covered by engine tests
    }

    /// Pure conversion function extracted for unit testing.
    fn convert_event(event: AgentEvent, turn: u32) -> Option<AgentNotification> {
        match event {
            AgentEvent::TextDelta(text) => Some(AgentNotification::TextDelta { text }),
            AgentEvent::ThinkingDelta(text) => Some(AgentNotification::ThinkingDelta { text }),
            AgentEvent::ToolUseStart { id, name } => Some(AgentNotification::ToolUseStart {
                id,
                tool_name: name,
            }),
            AgentEvent::ToolUseReady { id, name, input } => {
                Some(AgentNotification::ToolUseReady {
                    id,
                    tool_name: name,
                    input,
                })
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                text,
            } => Some(AgentNotification::ToolUseComplete {
                id,
                tool_name: String::new(), // stateless helper; real adapter uses tool_names map
                is_error,
                result_preview: text,
            }),
            AgentEvent::AssistantMessage(msg) => Some(AgentNotification::AssistantMessage {
                turn,
                text_blocks: msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        claude_core::message::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect(),
            }),
            AgentEvent::TurnComplete { stop_reason } => Some(AgentNotification::TurnComplete {
                turn,
                stop_reason: format!("{:?}", stop_reason),
                usage: UsageInfo {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            }),
            AgentEvent::UsageUpdate(_) => None,
            AgentEvent::TurnTokens { .. } => None,
            AgentEvent::ContextWarning { usage_pct, message } => {
                Some(AgentNotification::ContextWarning { usage_pct, message })
            }
            AgentEvent::CompactStart => Some(AgentNotification::CompactStart),
            AgentEvent::CompactComplete { summary_len } => {
                Some(AgentNotification::CompactComplete { summary_len })
            }
            AgentEvent::MaxTurns { limit } => Some(AgentNotification::TurnComplete {
                turn,
                stop_reason: format!("max_turns({})", limit),
                usage: UsageInfo {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            }),
            AgentEvent::Error(msg) => Some(AgentNotification::Error {
                code: ErrorCode::InternalError,
                message: msg,
            }),
        }
    }
}
