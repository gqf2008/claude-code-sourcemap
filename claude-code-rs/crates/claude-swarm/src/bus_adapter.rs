//! Bus adapter — bridges swarm events to the agent bus notification system.
//!
//! `SwarmNotifier` provides a thin, cloneable wrapper around the bus broadcast
//! sender so that swarm components (network, coordinator, actor) can publish
//! `AgentNotification` events without coupling to bus internals.

use std::sync::Arc;

use claude_bus::AgentNotification;
use tokio::sync::broadcast;

/// Cloneable notifier that publishes swarm events to the agent bus.
///
/// Created from the bus notification sender channel. All swarm components
/// share clones of this notifier via `Arc`.
#[derive(Clone)]
pub struct SwarmNotifier {
    tx: broadcast::Sender<AgentNotification>,
}

/// A no-op notifier for when no bus is connected (e.g. unit tests).
impl Default for SwarmNotifier {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(1);
        Self { tx }
    }
}

impl SwarmNotifier {
    /// Create a notifier wrapping a bus broadcast sender.
    pub fn new(tx: broadcast::Sender<AgentNotification>) -> Self {
        Self { tx }
    }

    /// Publish a notification to the bus. Returns number of receivers.
    pub fn notify(&self, event: AgentNotification) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Emit a SwarmTeamCreated event.
    pub fn team_created(&self, team_name: &str, agent_count: usize) {
        self.notify(AgentNotification::SwarmTeamCreated {
            team_name: team_name.to_string(),
            agent_count,
        });
    }

    /// Emit a SwarmTeamDeleted event.
    pub fn team_deleted(&self, team_name: &str) {
        self.notify(AgentNotification::SwarmTeamDeleted {
            team_name: team_name.to_string(),
        });
    }

    /// Emit a SwarmAgentSpawned event.
    pub fn agent_spawned(&self, team_name: &str, agent_id: &str, model: &str) {
        self.notify(AgentNotification::SwarmAgentSpawned {
            team_name: team_name.to_string(),
            agent_id: agent_id.to_string(),
            model: model.to_string(),
        });
    }

    /// Emit a SwarmAgentTerminated event.
    pub fn agent_terminated(&self, team_name: &str, agent_id: &str) {
        self.notify(AgentNotification::SwarmAgentTerminated {
            team_name: team_name.to_string(),
            agent_id: agent_id.to_string(),
        });
    }

    /// Emit a SwarmAgentQuery event (truncates prompt to preview length).
    pub fn agent_query(&self, team_name: &str, agent_id: &str, prompt: &str) {
        let preview = if prompt.len() > 120 {
            format!("{}…", &prompt[..120])
        } else {
            prompt.to_string()
        };
        self.notify(AgentNotification::SwarmAgentQuery {
            team_name: team_name.to_string(),
            agent_id: agent_id.to_string(),
            prompt_preview: preview,
        });
    }

    /// Emit a SwarmAgentReply event (truncates text to preview length).
    pub fn agent_reply(&self, team_name: &str, agent_id: &str, text: &str, is_error: bool) {
        let preview = if text.len() > 200 {
            format!("{}…", &text[..200])
        } else {
            text.to_string()
        };
        self.notify(AgentNotification::SwarmAgentReply {
            team_name: team_name.to_string(),
            agent_id: agent_id.to_string(),
            text_preview: preview,
            is_error,
        });
    }
}

/// Create a `SwarmNotifier` from a `BusHandle` (extracts the broadcast sender).
///
/// Since `BusHandle` doesn't expose its sender directly, callers should
/// construct `SwarmNotifier::new(tx)` from the same sender used by the bus.
/// This is a convenience alias.
pub type SharedNotifier = Arc<SwarmNotifier>;

/// Create a shared notifier (Arc-wrapped).
pub fn shared_notifier(notifier: SwarmNotifier) -> SharedNotifier {
    Arc::new(notifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_notifier_does_not_panic() {
        let n = SwarmNotifier::default();
        n.team_created("test", 0);
        n.agent_spawned("test", "a@test", "haiku");
        n.agent_query("test", "a@test", "hello world");
        n.agent_reply("test", "a@test", "response", false);
        n.agent_terminated("test", "a@test");
        n.team_deleted("test");
    }

    #[test]
    fn notifier_broadcasts_to_receiver() {
        let (tx, mut rx) = broadcast::channel(16);
        let n = SwarmNotifier::new(tx);

        n.team_created("alpha", 3);
        let event = rx.try_recv().unwrap();
        match event {
            AgentNotification::SwarmTeamCreated { team_name, agent_count } => {
                assert_eq!(team_name, "alpha");
                assert_eq!(agent_count, 3);
            }
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn preview_truncation() {
        let n = SwarmNotifier::default();
        // long prompt gets truncated — no panic
        let long_prompt = "x".repeat(500);
        n.agent_query("t", "a@t", &long_prompt);
        n.agent_reply("t", "a@t", &long_prompt, false);
    }

    #[test]
    fn shared_notifier_is_cloneable() {
        let shared = shared_notifier(SwarmNotifier::default());
        let _clone = shared.clone();
        shared.team_created("test", 0);
    }
}
