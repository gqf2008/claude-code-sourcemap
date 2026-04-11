//! Message types for inter-actor communication.

use kameo::Reply;
use serde::{Deserialize, Serialize};

/// Message sent to an agent requesting it to process a prompt.
#[derive(Debug, Clone)]
pub struct AgentQuery {
    /// The prompt / instruction to send to the agent.
    pub prompt: String,
    /// Optional sender agent ID (for reply routing).
    pub from: Option<String>,
}

/// Response from an agent after processing a query.
#[derive(Debug, Clone, Reply, Serialize, Deserialize)]
pub struct AgentResponse {
    /// The agent's text response.
    pub text: String,
    /// Whether the agent encountered an error.
    pub is_error: bool,
    /// Tool uses that occurred during processing.
    pub tool_uses: Vec<String>,
}

/// Request the agent's current status.
#[derive(Debug, Clone)]
pub struct GetStatus;

/// Agent status report.
#[derive(Debug, Clone, Reply, Serialize, Deserialize)]
pub struct AgentStatus {
    pub agent_id: String,
    pub team_name: String,
    pub model: String,
    pub state: AgentState,
    pub turn_count: u32,
    pub total_tokens: u64,
}

/// Agent lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Ready to receive queries.
    Idle,
    /// Currently processing a query.
    Processing,
    /// Terminated (will not accept queries).
    Stopped,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Processing => write!(f, "processing"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// Broadcast a message to all agents in the team.
#[derive(Debug, Clone)]
pub struct BroadcastMessage {
    pub text: String,
    pub from: String,
}

/// Ask the coordinator to spawn a new agent.
#[derive(Debug, Clone)]
pub struct SpawnAgent {
    pub name: String,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub cwd: Option<String>,
}

/// Ask the coordinator to terminate an agent.
#[derive(Debug, Clone)]
pub struct TerminateAgent {
    pub agent_id: String,
}

/// Request team-level status from the coordinator.
#[derive(Debug, Clone)]
pub struct GetTeamStatus;

/// Team status report.
#[derive(Debug, Clone, Reply, Serialize, Deserialize)]
pub struct TeamStatus {
    pub team_name: String,
    pub agent_count: usize,
    pub agents: Vec<AgentStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_state_display() {
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Processing.to_string(), "processing");
        assert_eq!(AgentState::Stopped.to_string(), "stopped");
    }

    #[test]
    fn agent_state_serde() {
        let json = serde_json::to_string(&AgentState::Idle).unwrap();
        assert_eq!(json, "\"idle\"");
        let state: AgentState = serde_json::from_str("\"processing\"").unwrap();
        assert_eq!(state, AgentState::Processing);
    }

    #[test]
    fn agent_status_serde_roundtrip() {
        let status = AgentStatus {
            agent_id: "researcher@alpha".into(),
            team_name: "alpha".into(),
            model: "claude-sonnet-4-20250514".into(),
            state: AgentState::Idle,
            turn_count: 5,
            total_tokens: 12345,
        };
        let json = serde_json::to_string(&status).unwrap();
        let deser: AgentStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.agent_id, "researcher@alpha");
        assert_eq!(deser.state, AgentState::Idle);
        assert_eq!(deser.total_tokens, 12345);
    }

    #[test]
    fn team_status_serde_roundtrip() {
        let ts = TeamStatus {
            team_name: "beta".into(),
            agent_count: 2,
            agents: vec![
                AgentStatus {
                    agent_id: "team-lead@beta".into(),
                    team_name: "beta".into(),
                    model: "claude-sonnet-4-20250514".into(),
                    state: AgentState::Idle,
                    turn_count: 10,
                    total_tokens: 50000,
                },
                AgentStatus {
                    agent_id: "coder@beta".into(),
                    team_name: "beta".into(),
                    model: "claude-haiku-3.5-20241022".into(),
                    state: AgentState::Processing,
                    turn_count: 3,
                    total_tokens: 8000,
                },
            ],
        };
        let json = serde_json::to_string_pretty(&ts).unwrap();
        let deser: TeamStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.team_name, "beta");
        assert_eq!(deser.agent_count, 2);
        assert_eq!(deser.agents[1].state, AgentState::Processing);
    }
}
