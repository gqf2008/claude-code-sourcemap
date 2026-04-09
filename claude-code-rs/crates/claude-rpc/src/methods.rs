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

/// Convert an `AgentNotification` into a JSON-RPC `Notification`.
pub fn notification_to_jsonrpc(notif: &AgentNotification) -> Notification {
    match notif {
        AgentNotification::TextDelta { text } => {
            Notification::new("agent.textDelta", Some(serde_json::json!({ "text": text })))
        }
        AgentNotification::ThinkingDelta { text } => {
            Notification::new("agent.thinkingDelta", Some(serde_json::json!({ "text": text })))
        }
        AgentNotification::ToolUseStart { id, tool_name } => {
            Notification::new("agent.toolStart", Some(serde_json::json!({
                "id": id, "tool_name": tool_name
            })))
        }
        AgentNotification::ToolUseReady { id, tool_name, input } => {
            Notification::new("agent.toolReady", Some(serde_json::json!({
                "id": id, "tool_name": tool_name, "input": input
            })))
        }
        AgentNotification::ToolUseComplete { id, tool_name, is_error, result_preview } => {
            Notification::new("agent.toolComplete", Some(serde_json::json!({
                "id": id, "tool_name": tool_name, "is_error": is_error,
                "result_preview": result_preview
            })))
        }
        AgentNotification::TurnStart { turn } => {
            Notification::new("agent.turnStart", Some(serde_json::json!({ "turn": turn })))
        }
        AgentNotification::TurnComplete { turn, stop_reason, usage } => {
            Notification::new("agent.turnComplete", Some(serde_json::json!({
                "turn": turn, "stop_reason": stop_reason,
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_creation_tokens": usage.cache_creation_tokens,
                }
            })))
        }
        AgentNotification::AssistantMessage { turn, text_blocks } => {
            Notification::new("agent.assistantMessage", Some(serde_json::json!({
                "turn": turn, "text_blocks": text_blocks
            })))
        }
        AgentNotification::SessionStart { session_id, model } => {
            Notification::new("session.start", Some(serde_json::json!({
                "session_id": session_id, "model": model
            })))
        }
        AgentNotification::SessionEnd { reason } => {
            Notification::new("session.end", Some(serde_json::json!({ "reason": reason })))
        }
        AgentNotification::SessionSaved { session_id } => {
            Notification::new("session.saved", Some(serde_json::json!({ "session_id": session_id })))
        }
        AgentNotification::SessionStatus {
            session_id, model, total_turns,
            total_input_tokens, total_output_tokens, context_usage_pct,
        } => {
            Notification::new("session.status", Some(serde_json::json!({
                "session_id": session_id, "model": model,
                "total_turns": total_turns,
                "total_input_tokens": total_input_tokens,
                "total_output_tokens": total_output_tokens,
                "context_usage_pct": context_usage_pct,
            })))
        }
        AgentNotification::HistoryCleared => {
            Notification::new("agent.historyCleared", None)
        }
        AgentNotification::ModelChanged { model, display_name } => {
            Notification::new("agent.modelChanged", Some(serde_json::json!({
                "model": model, "display_name": display_name
            })))
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
            Notification::new("agent.compactComplete", Some(serde_json::json!({
                "summary_len": summary_len
            })))
        }
        AgentNotification::AgentSpawned { agent_id, name, agent_type, background } => {
            Notification::new("agent.spawned", Some(serde_json::json!({
                "agent_id": agent_id, "name": name,
                "agent_type": agent_type, "background": background
            })))
        }
        AgentNotification::AgentProgress { agent_id, text } => {
            Notification::new("agent.progress", Some(serde_json::json!({
                "agent_id": agent_id, "text": text
            })))
        }
        AgentNotification::AgentComplete { agent_id, result, is_error } => {
            Notification::new("agent.complete", Some(serde_json::json!({
                "agent_id": agent_id, "result": result, "is_error": is_error
            })))
        }
        AgentNotification::McpServerConnected { name, tool_count } => {
            Notification::new("mcp.connected", Some(serde_json::json!({
                "name": name, "tool_count": tool_count
            })))
        }
        AgentNotification::McpServerDisconnected { name } => {
            Notification::new("mcp.disconnected", Some(serde_json::json!({ "name": name })))
        }
        AgentNotification::McpServerError { name, error } => {
            Notification::new("mcp.error", Some(serde_json::json!({
                "name": name, "error": error
            })))
        }
        AgentNotification::McpServerList { servers } => {
            let list: Vec<Value> = servers.iter().map(|s| serde_json::json!({
                "name": s.name, "tool_count": s.tool_count, "connected": s.connected
            })).collect();
            Notification::new("mcp.serverList", Some(serde_json::json!({ "servers": list })))
        }
        AgentNotification::MemoryExtracted { facts } => {
            Notification::new("agent.memoryExtracted", Some(serde_json::json!({ "facts": facts })))
        }
        AgentNotification::ModelList { models } => {
            let list: Vec<Value> = models.iter().map(|m| serde_json::json!({
                "id": m.id, "display_name": m.display_name
            })).collect();
            Notification::new("agent.modelList", Some(serde_json::json!({ "models": list })))
        }
        AgentNotification::ToolList { tools } => {
            let list: Vec<Value> = tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "enabled": t.enabled
            })).collect();
            Notification::new("agent.toolList", Some(serde_json::json!({ "tools": list })))
        }
        AgentNotification::Error { code, message } => {
            Notification::new("agent.error", Some(serde_json::json!({
                "code": code.to_string(), "message": message
            })))
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
        let req = parse_request("agent.submit", None).unwrap();
        assert!(matches!(req, AgentRequest::Submit { text, .. } if text.is_empty()));
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
}
