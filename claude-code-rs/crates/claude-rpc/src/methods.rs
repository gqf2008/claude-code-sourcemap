//! Method routing — maps JSON-RPC method strings to bus events.
//!
//! Converts between the wire protocol (JSON-RPC methods + params) and
//! the internal bus protocol (`AgentRequest` / `AgentNotification`).
//!
//! # Method naming convention
//!
//! ```text
//! agent.submit          — Submit user message
//! agent.abort           — Abort current operation
//! agent.compact         — Trigger compaction
//! agent.setModel        — Switch model
//! agent.clearHistory    — Clear conversation
//! agent.permission      — Respond to permission request
//! agent.sendMessage     — Message to sub-agent
//! agent.stopAgent       — Cancel sub-agent
//! session.save          — Save session to disk
//! session.status        — Query session status
//! session.shutdown      — Graceful shutdown
//! mcp.connect           — Connect MCP server
//! mcp.disconnect        — Disconnect MCP server
//! mcp.listServers       — List MCP servers
//! ```

use serde_json::Value;

use claude_bus::events::{AgentNotification, AgentRequest};

use crate::protocol::{error_codes, Notification, RpcError};

// ── Inbound: JSON-RPC method → AgentRequest ──────────────────────────────────

/// Parse a JSON-RPC method + params into an `AgentRequest`.
pub fn parse_request(method: &str, params: Option<Value>) -> Result<AgentRequest, RpcError> {
    match method {
        "agent.submit" => {
            let p = params.unwrap_or(Value::Null);
            let text = p.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                return Err(RpcError::new(
                    error_codes::INVALID_PARAMS,
                    "Missing or empty 'text' parameter for agent.submit",
                ));
            }
            Ok(AgentRequest::Submit { text, images: vec![] })
        }

        "agent.abort" => Ok(AgentRequest::Abort),

        "agent.compact" => {
            let instructions = params
                .as_ref()
                .and_then(|p| p.get("instructions"))
                .and_then(|v| v.as_str())
                .map(String::from);
            Ok(AgentRequest::Compact { instructions })
        }

        "agent.setModel" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params for agent.setModel")
            })?;
            let model = p.get("model")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    RpcError::new(error_codes::INVALID_PARAMS, "Missing 'model' parameter")
                })?
                .to_string();
            Ok(AgentRequest::SetModel { model })
        }

        "agent.clearHistory" => Ok(AgentRequest::ClearHistory),

        "agent.permission" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params for agent.permission")
            })?;
            let request_id = p.get("request_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'request_id'"))?
                .to_string();
            let granted = p.get("granted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let remember = p.get("remember")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(AgentRequest::PermissionResponse { request_id, granted, remember })
        }

        "agent.sendMessage" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params")
            })?;
            let agent_id = p.get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'agent_id'"))?
                .to_string();
            let message = p.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(AgentRequest::SendAgentMessage { agent_id, message })
        }

        "agent.stopAgent" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params")
            })?;
            let agent_id = p.get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'agent_id'"))?
                .to_string();
            Ok(AgentRequest::StopAgent { agent_id })
        }

        "session.save" => Ok(AgentRequest::SaveSession),
        "session.status" => Ok(AgentRequest::GetStatus),
        "session.shutdown" => Ok(AgentRequest::Shutdown),

        "session.load" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params for session.load")
            })?;
            let session_id = p.get("session_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'session_id'"))?
                .to_string();
            Ok(AgentRequest::LoadSession { session_id })
        }

        "agent.listModels" => Ok(AgentRequest::ListModels),
        "agent.listTools" => Ok(AgentRequest::ListTools),

        "mcp.connect" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params for mcp.connect")
            })?;
            let name = p.get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'name'"))?
                .to_string();
            let command = p.get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'command'"))?
                .to_string();

            // Security: validate MCP command against allowlist
            validate_mcp_command(&command)?;

            let args: Vec<String> = p.get("args")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let env: std::collections::HashMap<String, String> = p.get("env")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            Ok(AgentRequest::McpConnect { name, command, args, env })
        }

        "mcp.disconnect" => {
            let p = params.ok_or_else(|| {
                RpcError::new(error_codes::INVALID_PARAMS, "Missing params")
            })?;
            let name = p.get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "Missing 'name'"))?
                .to_string();
            Ok(AgentRequest::McpDisconnect { name })
        }

        "mcp.listServers" => Ok(AgentRequest::McpListServers),

        _ => Err(RpcError::new(
            error_codes::METHOD_NOT_FOUND,
            format!("Unknown method: {}", method),
        )),
    }
}

// ── MCP command validation ───────────────────────────────────────────────────

/// Allowed MCP server commands. Only known-safe executables are permitted.
const MCP_ALLOWED_COMMANDS: &[&str] = &[
    "npx", "node", "python", "python3", "uvx", "uv",
    "deno", "bun", "cargo", "go", "java",
    "docker", "podman",
    "mcp-server", "mcp-proxy",
];

/// Validate that an MCP command is on the allowlist.
fn validate_mcp_command(command: &str) -> Result<(), RpcError> {
    // Extract the base command name (strip path, handle .exe on Windows)
    let base = std::path::Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(command);

    if MCP_ALLOWED_COMMANDS.iter().any(|&allowed| base.eq_ignore_ascii_case(allowed)) {
        return Ok(());
    }

    // Also allow commands that start with "mcp-" (common naming convention)
    if base.starts_with("mcp-") || base.starts_with("mcp_") {
        return Ok(());
    }

    Err(RpcError::new(
        error_codes::INVALID_PARAMS,
        format!(
            "Command '{}' is not allowed for MCP. Allowed: {:?}, or any command starting with 'mcp-'",
            command, MCP_ALLOWED_COMMANDS
        ),
    ))
}

// ── Outbound: AgentNotification → JSON-RPC notification ──────────────────────

/// Build a JSON object `Value` with pre-allocated capacity.
#[inline]
fn json_obj(capacity: usize, entries: &[(&str, Value)]) -> Option<Value> {
    let mut map = serde_json::Map::with_capacity(capacity);
    for (k, v) in entries {
        map.insert((*k).into(), v.clone());
    }
    Some(Value::Object(map))
}

/// Convert an `AgentNotification` into a JSON-RPC `Notification`.
#[inline]
pub fn notification_to_jsonrpc(notif: &AgentNotification) -> Notification {
    match notif {
        AgentNotification::TextDelta { text } => {
            Notification::new("agent.textDelta", json_obj(1, &[("text", Value::String(text.clone()))]))
        }
        AgentNotification::ThinkingDelta { text } => {
            Notification::new("agent.thinkingDelta", json_obj(1, &[("text", Value::String(text.clone()))]))
        }
        AgentNotification::ToolUseStart { id, tool_name } => {
            Notification::new("agent.toolStart", json_obj(2, &[
                ("id", Value::String(id.clone())),
                ("tool_name", Value::String(tool_name.clone())),
            ]))
        }
        AgentNotification::ToolUseReady { id, tool_name, input } => {
            Notification::new("agent.toolReady", json_obj(3, &[
                ("id", Value::String(id.clone())),
                ("tool_name", Value::String(tool_name.clone())),
                ("input", input.clone()),
            ]))
        }
        AgentNotification::ToolUseComplete { id, tool_name, is_error, result_preview } => {
            let mut map = serde_json::Map::with_capacity(4);
            map.insert("id".into(), Value::String(id.clone()));
            map.insert("tool_name".into(), Value::String(tool_name.clone()));
            map.insert("is_error".into(), Value::Bool(*is_error));
            map.insert("result_preview".into(), match result_preview {
                Some(s) => Value::String(s.clone()),
                None => Value::Null,
            });
            Notification::new("agent.toolComplete", Some(Value::Object(map)))
        }
        AgentNotification::TurnStart { turn } => {
            Notification::new("agent.turnStart", json_obj(1, &[("turn", serde_json::json!(turn))]))
        }
        AgentNotification::TurnComplete { turn, stop_reason, usage } => {
            let mut map = serde_json::Map::with_capacity(3);
            map.insert("turn".into(), serde_json::json!(turn));
            map.insert("stop_reason".into(), Value::String(stop_reason.clone()));
            let mut umap = serde_json::Map::with_capacity(4);
            umap.insert("input_tokens".into(), serde_json::json!(usage.input_tokens));
            umap.insert("output_tokens".into(), serde_json::json!(usage.output_tokens));
            umap.insert("cache_read_tokens".into(), serde_json::json!(usage.cache_read_tokens));
            umap.insert("cache_creation_tokens".into(), serde_json::json!(usage.cache_creation_tokens));
            map.insert("usage".into(), Value::Object(umap));
            Notification::new("agent.turnComplete", Some(Value::Object(map)))
        }
        AgentNotification::AssistantMessage { turn, text_blocks } => {
            Notification::new("agent.assistantMessage", Some(serde_json::json!({
                "turn": turn, "text_blocks": text_blocks
            })))
        }
        AgentNotification::SessionStart { session_id, model } => {
            Notification::new("session.start", json_obj(2, &[
                ("session_id", Value::String(session_id.clone())),
                ("model", Value::String(model.clone())),
            ]))
        }
        AgentNotification::SessionEnd { reason } => {
            Notification::new("session.end", json_obj(1, &[("reason", Value::String(reason.clone()))]))
        }
        AgentNotification::SessionSaved { session_id } => {
            Notification::new("session.saved", json_obj(1, &[("session_id", Value::String(session_id.clone()))]))
        }
        AgentNotification::SessionStatus {
            session_id, model, total_turns,
            total_input_tokens, total_output_tokens, context_usage_pct,
        } => {
            let mut map = serde_json::Map::with_capacity(6);
            map.insert("session_id".into(), Value::String(session_id.clone()));
            map.insert("model".into(), Value::String(model.clone()));
            map.insert("total_turns".into(), serde_json::json!(total_turns));
            map.insert("total_input_tokens".into(), serde_json::json!(total_input_tokens));
            map.insert("total_output_tokens".into(), serde_json::json!(total_output_tokens));
            map.insert("context_usage_pct".into(), serde_json::json!(context_usage_pct));
            Notification::new("session.status", Some(Value::Object(map)))
        }
        AgentNotification::HistoryCleared => {
            Notification::new("agent.historyCleared", None)
        }
        AgentNotification::ModelChanged { model, display_name } => {
            Notification::new("agent.modelChanged", json_obj(2, &[
                ("model", Value::String(model.clone())),
                ("display_name", Value::String(display_name.clone())),
            ]))
        }
        AgentNotification::ContextWarning { usage_pct, message } => {
            Notification::new("agent.contextWarning", Some(serde_json::json!({
                "usage_pct": usage_pct, "message": message
            })))
        }
        AgentNotification::CompactStart => {
            Notification::new("agent.compactStart", None)
        }
        AgentNotification::CompactComplete { summary_len } => {
            Notification::new("agent.compactComplete", json_obj(1, &[("summary_len", serde_json::json!(summary_len))]))
        }
        AgentNotification::AgentSpawned { agent_id, name, agent_type, background } => {
            let mut map = serde_json::Map::with_capacity(4);
            map.insert("agent_id".into(), Value::String(agent_id.clone()));
            map.insert("name".into(), match name {
                Some(s) => Value::String(s.clone()),
                None => Value::Null,
            });
            map.insert("agent_type".into(), Value::String(agent_type.clone()));
            map.insert("background".into(), Value::Bool(*background));
            Notification::new("agent.spawned", Some(Value::Object(map)))
        }
        AgentNotification::AgentProgress { agent_id, text } => {
            Notification::new("agent.progress", json_obj(2, &[
                ("agent_id", Value::String(agent_id.clone())),
                ("text", Value::String(text.clone())),
            ]))
        }
        AgentNotification::AgentComplete { agent_id, result, is_error } => {
            Notification::new("agent.complete", json_obj(3, &[
                ("agent_id", Value::String(agent_id.clone())),
                ("result", Value::String(result.clone())),
                ("is_error", Value::Bool(*is_error)),
            ]))
        }
        AgentNotification::McpServerConnected { name, tool_count } => {
            Notification::new("mcp.connected", json_obj(2, &[
                ("name", Value::String(name.clone())),
                ("tool_count", serde_json::json!(tool_count)),
            ]))
        }
        AgentNotification::McpServerDisconnected { name } => {
            Notification::new("mcp.disconnected", json_obj(1, &[("name", Value::String(name.clone()))]))
        }
        AgentNotification::McpServerError { name, error } => {
            Notification::new("mcp.error", json_obj(2, &[
                ("name", Value::String(name.clone())),
                ("error", Value::String(error.clone())),
            ]))
        }
        AgentNotification::McpServerList { servers } => {
            let list: Vec<Value> = servers.iter().map(|s| serde_json::json!({
                "name": s.name, "tool_count": s.tool_count, "connected": s.connected
            })).collect();
            Notification::new("mcp.serverList", json_obj(1, &[("servers", Value::Array(list))]))
        }
        AgentNotification::MemoryExtracted { facts } => {
            Notification::new("agent.memoryExtracted", Some(serde_json::json!({ "facts": facts })))
        }
        AgentNotification::ModelList { models } => {
            let list: Vec<Value> = models.iter().map(|m| serde_json::json!({
                "id": m.id, "display_name": m.display_name
            })).collect();
            Notification::new("agent.modelList", json_obj(1, &[("models", Value::Array(list))]))
        }
        AgentNotification::ToolList { tools } => {
            let list: Vec<Value> = tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "enabled": t.enabled
            })).collect();
            Notification::new("agent.toolList", json_obj(1, &[("tools", Value::Array(list))]))
        }
        AgentNotification::Error { code, message } => {
            Notification::new("agent.error", json_obj(2, &[
                ("code", Value::String(code.to_string())),
                ("message", Value::String(message.clone())),
            ]))
        }
        AgentNotification::ThinkingChanged { enabled, budget } => {
            Notification::new("agent.thinking_changed", json_obj(2, &[
                ("enabled", Value::Bool(*enabled)),
                ("budget", budget.map(|b| Value::Number(b.into())).unwrap_or(Value::Null)),
            ]))
        }
        AgentNotification::CacheBreakSet => {
            Notification::new("agent.cache_break_set", Some(Value::Object(serde_json::Map::new())))
        }

        // ── Swarm lifecycle ──
        AgentNotification::SwarmTeamCreated { team_name, agent_count } => {
            Notification::new("swarm.team_created", json_obj(2, &[
                ("team_name", Value::String(team_name.clone())),
                ("agent_count", Value::Number((*agent_count).into())),
            ]))
        }
        AgentNotification::SwarmTeamDeleted { team_name } => {
            Notification::new("swarm.team_deleted", json_obj(1, &[
                ("team_name", Value::String(team_name.clone())),
            ]))
        }
        AgentNotification::SwarmAgentSpawned { team_name, agent_id, model } => {
            Notification::new("swarm.agent_spawned", json_obj(3, &[
                ("team_name", Value::String(team_name.clone())),
                ("agent_id", Value::String(agent_id.clone())),
                ("model", Value::String(model.clone())),
            ]))
        }
        AgentNotification::SwarmAgentTerminated { team_name, agent_id } => {
            Notification::new("swarm.agent_terminated", json_obj(2, &[
                ("team_name", Value::String(team_name.clone())),
                ("agent_id", Value::String(agent_id.clone())),
            ]))
        }
        AgentNotification::SwarmAgentQuery { team_name, agent_id, prompt_preview } => {
            Notification::new("swarm.agent_query", json_obj(3, &[
                ("team_name", Value::String(team_name.clone())),
                ("agent_id", Value::String(agent_id.clone())),
                ("prompt_preview", Value::String(prompt_preview.clone())),
            ]))
        }
        AgentNotification::SwarmAgentReply { team_name, agent_id, text_preview, is_error } => {
            Notification::new("swarm.agent_reply", json_obj(4, &[
                ("team_name", Value::String(team_name.clone())),
                ("agent_id", Value::String(agent_id.clone())),
                ("text_preview", Value::String(text_preview.clone())),
                ("is_error", Value::Bool(*is_error)),
            ]))
        }

        // ── Extended lifecycle ──

        AgentNotification::AgentTerminated { agent_id, reason } => {
            Notification::new("agent.terminated", json_obj(2, &[
                ("agent_id", Value::String(agent_id.clone())),
                ("reason", Value::String(reason.clone())),
            ]))
        }
        AgentNotification::ToolSelected { tool_name } => {
            Notification::new("agent.tool_selected", json_obj(1, &[
                ("tool_name", Value::String(tool_name.clone())),
            ]))
        }
        AgentNotification::ConflictDetected { file_path, agents } => {
            Notification::new("agent.conflict_detected", json_obj(2, &[
                ("file_path", Value::String(file_path.clone())),
                ("agents", Value::Array(agents.iter().map(|a| Value::String(a.clone())).collect())),
            ]))
        }
    }
}

/// All supported method names (for introspection / help).
pub const METHODS: &[&str] = &[
    "agent.submit",
    "agent.abort",
    "agent.compact",
    "agent.setModel",
    "agent.clearHistory",
    "agent.permission",
    "agent.sendMessage",
    "agent.stopAgent",
    "agent.listModels",
    "agent.listTools",
    "session.save",
    "session.status",
    "session.shutdown",
    "session.load",
    "mcp.connect",
    "mcp.disconnect",
    "mcp.listServers",
];

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_submit() {
        let req = parse_request("agent.submit", Some(serde_json::json!({"text": "hello"}))).unwrap();
        assert!(matches!(req, AgentRequest::Submit { text, .. } if text == "hello"));
    }

    #[test]
    fn parse_submit_no_params() {
        // Empty text is now rejected
        let err = parse_request("agent.submit", None).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_submit_empty_text() {
        let err = parse_request("agent.submit", Some(serde_json::json!({"text": ""}))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_submit_missing_text_field() {
        let err = parse_request("agent.submit", Some(serde_json::json!({"other": "val"}))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_abort() {
        let req = parse_request("agent.abort", None).unwrap();
        assert!(matches!(req, AgentRequest::Abort));
    }

    #[test]
    fn parse_compact_with_instructions() {
        let req = parse_request(
            "agent.compact",
            Some(serde_json::json!({"instructions": "Keep API calls"})),
        ).unwrap();
        assert!(matches!(req, AgentRequest::Compact { instructions: Some(i) } if i == "Keep API calls"));
    }

    #[test]
    fn parse_compact_no_instructions() {
        let req = parse_request("agent.compact", None).unwrap();
        assert!(matches!(req, AgentRequest::Compact { instructions: None }));
    }

    #[test]
    fn parse_set_model() {
        let req = parse_request("agent.setModel", Some(serde_json::json!({"model": "opus"}))).unwrap();
        assert!(matches!(req, AgentRequest::SetModel { model } if model == "opus"));
    }

    #[test]
    fn parse_set_model_missing_param() {
        let err = parse_request("agent.setModel", None).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_clear_history() {
        let req = parse_request("agent.clearHistory", None).unwrap();
        assert!(matches!(req, AgentRequest::ClearHistory));
    }

    #[test]
    fn parse_permission() {
        let req = parse_request("agent.permission", Some(serde_json::json!({
            "request_id": "perm-1", "granted": true, "remember": true
        }))).unwrap();
        assert!(matches!(req, AgentRequest::PermissionResponse { granted: true, remember: true, .. }));
    }

    #[test]
    fn parse_session_commands() {
        assert!(matches!(parse_request("session.save", None).unwrap(), AgentRequest::SaveSession));
        assert!(matches!(parse_request("session.status", None).unwrap(), AgentRequest::GetStatus));
        assert!(matches!(parse_request("session.shutdown", None).unwrap(), AgentRequest::Shutdown));
    }

    #[test]
    fn parse_mcp_list() {
        assert!(matches!(parse_request("mcp.listServers", None).unwrap(), AgentRequest::McpListServers));
    }

    #[test]
    fn parse_unknown_method() {
        let err = parse_request("unknown.method", None).unwrap_err();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn notification_text_delta() {
        let notif = AgentNotification::TextDelta { text: "hi".into() };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "agent.textDelta");
        let text = jsonrpc.params.unwrap()["text"].as_str().unwrap().to_string();
        assert_eq!(text, "hi");
    }

    #[test]
    fn notification_turn_complete() {
        let notif = AgentNotification::TurnComplete {
            turn: 1,
            stop_reason: "end_turn".into(),
            usage: claude_bus::events::UsageInfo {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "agent.turnComplete");
        let params = jsonrpc.params.unwrap();
        assert_eq!(params["turn"], 1);
        assert_eq!(params["usage"]["input_tokens"], 100);
    }

    #[test]
    fn notification_history_cleared() {
        let notif = AgentNotification::HistoryCleared;
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "agent.historyCleared");
        assert!(jsonrpc.params.is_none());
    }

    #[test]
    fn notification_model_changed() {
        let notif = AgentNotification::ModelChanged {
            model: "claude-opus-4-20250514".into(),
            display_name: "Claude Opus 4".into(),
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "agent.modelChanged");
        let params = jsonrpc.params.unwrap();
        assert_eq!(params["model"], "claude-opus-4-20250514");
    }

    #[test]
    fn notification_error() {
        let notif = AgentNotification::Error {
            code: claude_bus::events::ErrorCode::ApiError,
            message: "Rate limited".into(),
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "agent.error");
    }

    #[test]
    fn notification_mcp_server_list() {
        let notif = AgentNotification::McpServerList {
            servers: vec![claude_bus::events::McpServerInfo {
                name: "test".into(),
                tool_count: 3,
                connected: true,
            }],
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "mcp.serverList");
    }

    #[test]
    fn all_methods_are_parseable() {
        // Verify every method in METHODS list can be called (even if params are missing)
        for method in METHODS {
            let result = parse_request(method, None);
            // Some require params (will error with INVALID_PARAMS), but none should be METHOD_NOT_FOUND
            if let Err(e) = &result {
                assert_ne!(e.code, error_codes::METHOD_NOT_FOUND,
                    "Method '{}' returned METHOD_NOT_FOUND", method);
            }
        }
    }

    // ── Additional parse_request edge case tests ─────────────────────────────

    #[test]
    fn parse_set_model_missing_model_field() {
        let err = parse_request("agent.setModel", Some(serde_json::json!({}))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_permission_missing_request_id() {
        let err = parse_request("agent.permission", Some(serde_json::json!({
            "granted": true
        }))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_permission_defaults() {
        // granted and remember default to false when missing
        let req = parse_request("agent.permission", Some(serde_json::json!({
            "request_id": "perm-x"
        }))).unwrap();
        assert!(matches!(req, AgentRequest::PermissionResponse { granted: false, remember: false, .. }));
    }

    #[test]
    fn parse_session_load_missing_id() {
        let err = parse_request("session.load", None).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_session_load_valid() {
        let req = parse_request("session.load", Some(serde_json::json!({
            "session_id": "sess-123"
        }))).unwrap();
        assert!(matches!(req, AgentRequest::LoadSession { session_id } if session_id == "sess-123"));
    }

    #[test]
    fn parse_mcp_connect_valid() {
        let req = parse_request("mcp.connect", Some(serde_json::json!({
            "name": "fs-server",
            "command": "npx",
            "args": ["-y", "@mcp/fs-server"],
            "env": {"NODE_ENV": "production"}
        }))).unwrap();
        if let AgentRequest::McpConnect { name, command, args, env } = req {
            assert_eq!(name, "fs-server");
            assert_eq!(command, "npx");
            assert_eq!(args, vec!["-y", "@mcp/fs-server"]);
            assert_eq!(env.get("NODE_ENV").unwrap(), "production");
        } else {
            panic!("Expected McpConnect");
        }
    }

    #[test]
    fn parse_mcp_connect_missing_command() {
        let err = parse_request("mcp.connect", Some(serde_json::json!({
            "name": "test"
        }))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_mcp_connect_blocked_command() {
        let err = parse_request("mcp.connect", Some(serde_json::json!({
            "name": "test", "command": "rm"
        }))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("not allowed"));
    }

    #[test]
    fn parse_mcp_connect_allowed_mcp_prefix() {
        let req = parse_request("mcp.connect", Some(serde_json::json!({
            "name": "test", "command": "mcp-my-server"
        }))).unwrap();
        assert!(matches!(req, AgentRequest::McpConnect { .. }));
    }

    #[test]
    fn parse_send_message_valid() {
        let req = parse_request("agent.sendMessage", Some(serde_json::json!({
            "agent_id": "a1", "message": "hello"
        }))).unwrap();
        assert!(matches!(req, AgentRequest::SendAgentMessage { agent_id, message }
            if agent_id == "a1" && message == "hello"));
    }

    #[test]
    fn parse_stop_agent_missing_id() {
        let err = parse_request("agent.stopAgent", None).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    // ── Additional notification tests ────────────────────────────────────────

    #[test]
    fn notification_tool_use_start() {
        let notif = AgentNotification::ToolUseStart {
            id: "tu-1".into(),
            tool_name: "Bash".into(),
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "agent.toolStart");
        let p = jsonrpc.params.unwrap();
        assert_eq!(p["id"], "tu-1");
        assert_eq!(p["tool_name"], "Bash");
    }

    #[test]
    fn notification_tool_complete_with_none_preview() {
        let notif = AgentNotification::ToolUseComplete {
            id: "tu-2".into(),
            tool_name: "Read".into(),
            is_error: false,
            result_preview: None,
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        let p = jsonrpc.params.unwrap();
        assert!(p["result_preview"].is_null());
    }

    #[test]
    fn notification_agent_spawned_optional_name() {
        let notif = AgentNotification::AgentSpawned {
            agent_id: "ag-1".into(),
            name: None,
            agent_type: "explore".into(),
            background: true,
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        let p = jsonrpc.params.unwrap();
        assert!(p["name"].is_null());
        assert_eq!(p["background"], true);
    }

    #[test]
    fn notification_session_status() {
        let notif = AgentNotification::SessionStatus {
            session_id: "s1".into(),
            model: "opus".into(),
            total_turns: 5,
            total_input_tokens: 1000,
            total_output_tokens: 500,
            context_usage_pct: 42.5,
        };
        let jsonrpc = notification_to_jsonrpc(&notif);
        assert_eq!(jsonrpc.method, "session.status");
        let p = jsonrpc.params.unwrap();
        assert_eq!(p["total_turns"], 5);
        assert_eq!(p["context_usage_pct"], 42.5);
    }

    #[test]
    fn notification_compact_events() {
        let start = notification_to_jsonrpc(&AgentNotification::CompactStart);
        assert_eq!(start.method, "agent.compactStart");
        assert!(start.params.is_none());

        let complete = notification_to_jsonrpc(&AgentNotification::CompactComplete { summary_len: 200 });
        assert_eq!(complete.method, "agent.compactComplete");
        assert_eq!(complete.params.unwrap()["summary_len"], 200);
    }

    // ── MCP command validation tests ─────────────────────────────────────────

    #[test]
    fn validate_mcp_allowed_commands() {
        for cmd in MCP_ALLOWED_COMMANDS {
            assert!(validate_mcp_command(cmd).is_ok(), "Should allow: {}", cmd);
        }
    }

    #[test]
    fn validate_mcp_command_with_path() {
        assert!(validate_mcp_command("/usr/bin/node").is_ok());
        assert!(validate_mcp_command("C:\\Program Files\\node.exe").is_ok());
    }

    #[test]
    fn validate_mcp_command_blocked() {
        assert!(validate_mcp_command("rm").is_err());
        assert!(validate_mcp_command("bash").is_err());
        assert!(validate_mcp_command("curl").is_err());
    }

    #[test]
    fn validate_mcp_command_prefix_allowed() {
        assert!(validate_mcp_command("mcp-filesystem").is_ok());
        assert!(validate_mcp_command("mcp_custom_tool").is_ok());
    }
}
