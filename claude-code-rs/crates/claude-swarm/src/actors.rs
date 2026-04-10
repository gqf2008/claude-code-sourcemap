//! Kameo actor definitions for swarm agents and coordinator.
//!
//! Uses kameo 0.20 derive macros for actor definitions.

use std::collections::HashMap;

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::message::{Context, Message};
use kameo::Reply;
use tracing::{debug, info, warn};

use crate::messages::*;
use crate::types::format_agent_id;

// ── Reply types ──────────────────────────────────────────────────────────

/// Result of spawning an agent.
#[derive(Debug, Clone, Reply, serde::Serialize, serde::Deserialize)]
pub struct SpawnResult {
    pub success: bool,
    pub agent_id: String,
    pub message: String,
}

/// Result of terminating an agent.
#[derive(Debug, Clone, Reply, serde::Serialize, serde::Deserialize)]
pub struct TerminateResult {
    pub success: bool,
    pub message: String,
}

/// Result of routing a message to an agent.
#[derive(Debug, Clone, Reply, serde::Serialize, serde::Deserialize)]
pub struct RouteResult {
    pub success: bool,
    pub response: Option<AgentResponse>,
    pub error: Option<String>,
}

/// Wrapper for broadcast results (Vec<RouteResult> needs Reply impl).
#[derive(Debug, Clone, Reply)]
pub struct BroadcastResults(pub Vec<RouteResult>);

/// Message routed to a specific agent within a team.
#[derive(Debug, Clone)]
pub struct RouteMessage {
    pub target_agent_id: String,
    pub query: AgentQuery,
}

// ── AgentActor ───────────────────────────────────────────────────────────

/// A single AI agent in the swarm. Holds conversation state and processes queries.
///
/// In this stub implementation the agent returns a synthetic response;
/// future versions will wrap a real `QueryEngine` session.
#[derive(Actor)]
pub struct AgentActor {
    pub agent_id: String,
    pub team_name: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub cwd: String,
    pub state: AgentState,
    pub turn_count: u32,
    pub total_tokens: u64,
}

impl AgentActor {
    pub fn new(
        name: &str,
        team_name: &str,
        model: String,
        system_prompt: Option<String>,
        cwd: String,
    ) -> Self {
        Self {
            agent_id: format_agent_id(name, team_name),
            team_name: team_name.to_string(),
            model,
            system_prompt,
            cwd,
            state: AgentState::Idle,
            turn_count: 0,
            total_tokens: 0,
        }
    }
}

// Handle AgentQuery → AgentResponse
impl Message<AgentQuery> for AgentActor {
    type Reply = AgentResponse;

    async fn handle(
        &mut self,
        msg: AgentQuery,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state = AgentState::Processing;
        self.turn_count += 1;
        debug!(agent = %self.agent_id, turn = self.turn_count, "Processing query");

        // Stub: echo the prompt back. A real implementation would call
        // the Claude API through a QueryEngine.
        let response_text = format!(
            "[{}] Processed: {}",
            self.agent_id,
            if msg.prompt.len() > 100 {
                format!("{}...", &msg.prompt[..100])
            } else {
                msg.prompt.clone()
            }
        );

        // Simulate some token usage
        self.total_tokens += msg.prompt.len() as u64 * 4;

        self.state = AgentState::Idle;
        AgentResponse {
            text: response_text,
            is_error: false,
            tool_uses: vec![],
        }
    }
}

// Handle GetStatus → AgentStatus
impl Message<GetStatus> for AgentActor {
    type Reply = AgentStatus;

    async fn handle(
        &mut self,
        _msg: GetStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        AgentStatus {
            agent_id: self.agent_id.clone(),
            team_name: self.team_name.clone(),
            model: self.model.clone(),
            state: self.state,
            turn_count: self.turn_count,
            total_tokens: self.total_tokens,
        }
    }
}

// ── SwarmCoordinator ─────────────────────────────────────────────────────

/// Manages a team of agents. Handles spawn, terminate, routing, broadcast.
#[derive(Actor)]
pub struct SwarmCoordinator {
    pub team_name: String,
    pub default_model: String,
    pub default_cwd: String,
    agents: HashMap<String, ActorRef<AgentActor>>,
}

impl SwarmCoordinator {
    pub fn new(team_name: String, default_model: String, default_cwd: String) -> Self {
        Self {
            team_name,
            default_model,
            default_cwd,
            agents: HashMap::new(),
        }
    }
}

// Handle SpawnAgent → SpawnResult
impl Message<SpawnAgent> for SwarmCoordinator {
    type Reply = SpawnResult;

    async fn handle(
        &mut self,
        msg: SpawnAgent,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let agent_id = format_agent_id(&msg.name, &self.team_name);
        if self.agents.contains_key(&agent_id) {
            return SpawnResult {
                success: false,
                agent_id: agent_id.clone(),
                message: format!("Agent '{agent_id}' already exists"),
            };
        }

        let model = msg.model.unwrap_or_else(|| self.default_model.clone());
        let cwd = msg.cwd.unwrap_or_else(|| self.default_cwd.clone());

        let actor = AgentActor::new(
            &msg.name,
            &self.team_name,
            model,
            msg.prompt,
            cwd,
        );
        let actor_ref = AgentActor::spawn(actor);
        self.agents.insert(agent_id.clone(), actor_ref);

        info!(team = %self.team_name, agent = %agent_id, "Agent spawned");
        SpawnResult {
            success: true,
            agent_id,
            message: "Agent spawned successfully".into(),
        }
    }
}

// Handle TerminateAgent → TerminateResult
impl Message<TerminateAgent> for SwarmCoordinator {
    type Reply = TerminateResult;

    async fn handle(
        &mut self,
        msg: TerminateAgent,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(agent_ref) = self.agents.remove(&msg.agent_id) {
            agent_ref.kill();
            info!(team = %self.team_name, agent = %msg.agent_id, "Agent terminated");
            TerminateResult {
                success: true,
                message: format!("Agent '{}' terminated", msg.agent_id),
            }
        } else {
            TerminateResult {
                success: false,
                message: format!("Agent '{}' not found in team '{}'", msg.agent_id, self.team_name),
            }
        }
    }
}

// Handle RouteMessage → RouteResult
impl Message<RouteMessage> for SwarmCoordinator {
    type Reply = RouteResult;

    async fn handle(
        &mut self,
        msg: RouteMessage,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(agent_ref) = self.agents.get(&msg.target_agent_id) {
            match agent_ref.ask(msg.query).await {
                Ok(response) => RouteResult {
                    success: true,
                    response: Some(response),
                    error: None,
                },
                Err(e) => RouteResult {
                    success: false,
                    response: None,
                    error: Some(format!("Agent query failed: {e}")),
                },
            }
        } else {
            RouteResult {
                success: false,
                response: None,
                error: Some(format!(
                    "Agent '{}' not found in team '{}'",
                    msg.target_agent_id, self.team_name
                )),
            }
        }
    }
}

// Handle BroadcastMessage → BroadcastResults
impl Message<BroadcastMessage> for SwarmCoordinator {
    type Reply = BroadcastResults;

    async fn handle(
        &mut self,
        msg: BroadcastMessage,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut results = Vec::new();
        let agent_ids: Vec<String> = self.agents.keys().cloned().collect();

        for agent_id in &agent_ids {
            // Skip the sender
            if agent_id == &msg.from {
                continue;
            }
            if let Some(agent_ref) = self.agents.get(agent_id) {
                let query = AgentQuery {
                    prompt: msg.text.clone(),
                    from: Some(msg.from.clone()),
                };
                match agent_ref.ask(query).await {
                    Ok(response) => results.push(RouteResult {
                        success: true,
                        response: Some(response),
                        error: None,
                    }),
                    Err(e) => results.push(RouteResult {
                        success: false,
                        response: None,
                        error: Some(format!("Broadcast to '{agent_id}' failed: {e}")),
                    }),
                }
            }
        }
        BroadcastResults(results)
    }
}

// Handle GetTeamStatus → TeamStatus
impl Message<GetTeamStatus> for SwarmCoordinator {
    type Reply = TeamStatus;

    async fn handle(
        &mut self,
        _msg: GetTeamStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut agents = Vec::new();

        for (agent_id, agent_ref) in &self.agents {
            match agent_ref.ask(GetStatus).await {
                Ok(status) => agents.push(status),
                Err(e) => {
                    warn!(agent = %agent_id, error = %e, "Failed to get agent status");
                    agents.push(AgentStatus {
                        agent_id: agent_id.clone(),
                        team_name: self.team_name.clone(),
                        model: "unknown".into(),
                        state: AgentState::Stopped,
                        turn_count: 0,
                        total_tokens: 0,
                    });
                }
            }
        }

        TeamStatus {
            team_name: self.team_name.clone(),
            agent_count: agents.len(),
            agents,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_agent_id_basic() {
        assert_eq!(format_agent_id("coder", "alpha"), "coder@alpha");
        assert_eq!(format_agent_id("team-lead", "my-team"), "team-lead@my-team");
    }

    #[tokio::test]
    async fn agent_actor_query_and_status() {
        let actor = AgentActor::new("test", "team", "claude-haiku".into(), None, "/tmp".into());
        let actor_ref = AgentActor::spawn(actor);

        // Query
        let resp = actor_ref.ask(AgentQuery {
            prompt: "Hello".into(),
            from: None,
        }).await.unwrap();
        assert!(resp.text.contains("test@team"));
        assert!(!resp.is_error);

        // Status
        let status = actor_ref.ask(GetStatus).await.unwrap();
        assert_eq!(status.agent_id, "test@team");
        assert_eq!(status.turn_count, 1);
        assert!(status.total_tokens > 0);
        assert_eq!(status.state, AgentState::Idle);
    }

    #[tokio::test]
    async fn coordinator_spawn_and_terminate() {
        let coord = SwarmCoordinator::new("test-team".into(), "haiku".into(), "/tmp".into());
        let coord_ref = SwarmCoordinator::spawn(coord);

        // Spawn
        let result = coord_ref.ask(SpawnAgent {
            name: "worker".into(),
            model: None,
            prompt: Some("Work hard".into()),
            cwd: None,
        }).await.unwrap();
        assert!(result.success);
        assert_eq!(result.agent_id, "worker@test-team");

        // Duplicate should fail
        let dup = coord_ref.ask(SpawnAgent {
            name: "worker".into(),
            model: None,
            prompt: None,
            cwd: None,
        }).await.unwrap();
        assert!(!dup.success);

        // Route message
        let route = coord_ref.ask(RouteMessage {
            target_agent_id: "worker@test-team".into(),
            query: AgentQuery { prompt: "Build it".into(), from: None },
        }).await.unwrap();
        assert!(route.success);
        assert!(route.response.unwrap().text.contains("worker@test-team"));

        // Team status
        let status = coord_ref.ask(GetTeamStatus).await.unwrap();
        assert_eq!(status.agent_count, 1);
        assert_eq!(status.agents[0].turn_count, 1);

        // Terminate
        let term = coord_ref.ask(TerminateAgent {
            agent_id: "worker@test-team".into(),
        }).await.unwrap();
        assert!(term.success);

        // Status after terminate
        let status2 = coord_ref.ask(GetTeamStatus).await.unwrap();
        assert_eq!(status2.agent_count, 0);
    }
}
