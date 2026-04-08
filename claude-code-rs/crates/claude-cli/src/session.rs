//! Session lifecycle management via the Event Bus.
//!
//! `SessionManager` wraps a `ClientHandle` and provides high-level operations
//! for session management (save, compact, status query, model switch) that
//! communicate through the bus instead of calling `QueryEngine` directly.

use claude_bus::bus::ClientHandle;
use claude_bus::events::{AgentNotification, AgentRequest};

/// High-level session operations routed through the Event Bus.
///
/// Each method sends a request via the `ClientHandle` and waits for
/// the corresponding notification response. This keeps the CLI decoupled
/// from the Agent Core implementation.
#[allow(dead_code)]
pub struct SessionManager<'a> {
    client: &'a mut ClientHandle,
}

/// Status snapshot returned by [`SessionManager::get_status`].
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SessionStatus {
    pub session_id: String,
    pub model: String,
    pub total_turns: u32,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub context_usage_pct: f64,
}

#[allow(dead_code)]
impl<'a> SessionManager<'a> {
    pub fn new(client: &'a mut ClientHandle) -> Self {
        Self { client }
    }

    /// Save the current session to disk.
    ///
    /// Sends `SaveSession` request and waits for `SessionSaved` or `Error`.
    /// Returns the session ID on success.
    pub async fn save(&mut self) -> Result<String, String> {
        self.client
            .send_request(AgentRequest::SaveSession)
            .map_err(|e| format!("Failed to send SaveSession: {}", e))?;

        // Wait for the SessionSaved or Error notification
        while let Some(notification) = self.client.recv_notification().await {
            match notification {
                AgentNotification::SessionSaved { session_id } => {
                    return Ok(session_id);
                }
                AgentNotification::Error { message, .. } => {
                    return Err(message);
                }
                _ => {
                    // Ignore unrelated notifications
                }
            }
        }

        Err("Bus channel closed before response".into())
    }

    /// Trigger manual compaction with optional instructions.
    ///
    /// Sends `Compact` request and waits for `CompactComplete` or `Error`.
    /// Returns the summary length on success.
    pub async fn compact(&mut self, instructions: Option<String>) -> Result<usize, String> {
        self.client
            .send_request(AgentRequest::Compact { instructions })
            .map_err(|e| format!("Failed to send Compact: {}", e))?;

        while let Some(notification) = self.client.recv_notification().await {
            match notification {
                AgentNotification::CompactComplete { summary_len } => {
                    return Ok(summary_len);
                }
                AgentNotification::Error { message, .. } => {
                    return Err(message);
                }
                _ => {}
            }
        }

        Err("Bus channel closed before response".into())
    }

    /// Switch the active model.
    ///
    /// Sends `SetModel` request. This is fire-and-forget — the model change
    /// takes effect on the next submission.
    pub fn set_model(&self, model: &str) -> Result<(), String> {
        self.client
            .send_request(AgentRequest::SetModel {
                model: model.to_string(),
            })
            .map_err(|e| format!("Failed to send SetModel: {}", e))
    }

    /// Query session status (model, tokens, context usage).
    ///
    /// Sends `GetStatus` request and waits for `SessionStatus` notification.
    pub async fn get_status(&mut self) -> Result<SessionStatus, String> {
        self.client
            .send_request(AgentRequest::GetStatus)
            .map_err(|e| format!("Failed to send GetStatus: {}", e))?;

        while let Some(notification) = self.client.recv_notification().await {
            match notification {
                AgentNotification::SessionStatus {
                    session_id,
                    model,
                    total_turns,
                    total_input_tokens,
                    total_output_tokens,
                    context_usage_pct,
                } => {
                    return Ok(SessionStatus {
                        session_id,
                        model,
                        total_turns,
                        total_input_tokens,
                        total_output_tokens,
                        context_usage_pct,
                    });
                }
                AgentNotification::Error { message, .. } => {
                    return Err(message);
                }
                _ => {}
            }
        }

        Err("Bus channel closed before response".into())
    }

    /// Request graceful shutdown.
    pub fn shutdown(&self) -> Result<(), String> {
        self.client
            .send_request(AgentRequest::Shutdown)
            .map_err(|e| format!("Failed to send Shutdown: {}", e))
    }

    /// Abort the currently running operation.
    pub fn abort(&self) -> Result<(), String> {
        self.client
            .send_request(AgentRequest::Abort)
            .map_err(|e| format!("Failed to send Abort: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_fields() {
        let status = SessionStatus {
            session_id: "test-123".into(),
            model: "claude-sonnet-4-20250514".into(),
            total_turns: 5,
            total_input_tokens: 10_000,
            total_output_tokens: 2_000,
            context_usage_pct: 42.5,
        };
        assert_eq!(status.session_id, "test-123");
        assert_eq!(status.total_turns, 5);
        assert!((status.context_usage_pct - 42.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn session_manager_save_via_bus() {
        use claude_bus::bus::EventBus;

        let (bus_handle, mut client) = EventBus::new(16);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            bus_handle.notify(AgentNotification::SessionSaved {
                session_id: "sess-abc".into(),
            });
        });

        let mut mgr = SessionManager::new(&mut client);
        let result = mgr.save().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sess-abc");
    }

    #[tokio::test]
    async fn session_manager_compact_via_bus() {
        use claude_bus::bus::EventBus;

        let (bus_handle, mut client) = EventBus::new(16);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            bus_handle.notify(AgentNotification::CompactComplete { summary_len: 150 });
        });

        let mut mgr = SessionManager::new(&mut client);
        let result = mgr.compact(None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 150);
    }

    #[tokio::test]
    async fn session_manager_get_status_via_bus() {
        use claude_bus::bus::EventBus;

        let (bus_handle, mut client) = EventBus::new(16);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            bus_handle.notify(AgentNotification::SessionStatus {
                session_id: "sess-xyz".into(),
                model: "claude-sonnet-4-20250514".into(),
                total_turns: 3,
                total_input_tokens: 5000,
                total_output_tokens: 1000,
                context_usage_pct: 25.0,
            });
        });

        let mut mgr = SessionManager::new(&mut client);
        let status = mgr.get_status().await.unwrap();
        assert_eq!(status.session_id, "sess-xyz");
        assert_eq!(status.total_turns, 3);
        assert!((status.context_usage_pct - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn session_manager_set_model_sends_request() {
        use claude_bus::bus::EventBus;

        let (_bus_handle, mut client) = EventBus::new(16);

        let mgr = SessionManager::new(&mut client);
        let result = mgr.set_model("claude-sonnet-4-20250514");
        assert!(result.is_ok());
    }

    #[test]
    fn session_manager_abort_sends_request() {
        use claude_bus::bus::EventBus;

        let (_bus_handle, mut client) = EventBus::new(16);

        let mgr = SessionManager::new(&mut client);
        let result = mgr.abort();
        assert!(result.is_ok());
    }
}
