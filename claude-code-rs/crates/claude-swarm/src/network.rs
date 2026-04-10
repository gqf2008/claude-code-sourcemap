//! `SwarmNetwork` — high-level API for managing the swarm.
//!
//! Wraps the coordinator actor and provides a simple async interface
//! for creating teams, spawning agents, and sending messages.

use std::collections::HashMap;
use std::sync::Arc;

use kameo::actor::{ActorRef, Spawn};
use tokio::sync::RwLock;
use tracing::info;

use crate::actors::{SwarmCoordinator, SpawnResult, TerminateResult, RouteResult};
use crate::messages::*;

/// Top-level manager for the agent swarm.
///
/// Holds references to team coordinators and provides the primary API
/// consumed by `SwarmMcpServer`.
pub struct SwarmNetwork {
    teams: Arc<RwLock<HashMap<String, ActorRef<SwarmCoordinator>>>>,
    default_model: String,
    default_cwd: String,
}

impl SwarmNetwork {
    /// Create a new empty swarm network.
    pub fn new(default_model: String, default_cwd: String) -> Self {
        Self {
            teams: Arc::new(RwLock::new(HashMap::new())),
            default_model,
            default_cwd,
        }
    }

    /// Create a new team with the given name.
    pub async fn create_team(&self, name: &str) -> anyhow::Result<String> {
        let mut teams = self.teams.write().await;
        if teams.contains_key(name) {
            anyhow::bail!("Team '{}' already exists", name);
        }

        let coordinator = SwarmCoordinator::new(
            name.to_string(),
            self.default_model.clone(),
            self.default_cwd.clone(),
        );
        let coord_ref = SwarmCoordinator::spawn(coordinator);
        teams.insert(name.to_string(), coord_ref);
        info!(team = %name, "Team created");
        Ok(name.to_string())
    }

    /// Delete a team and terminate all its agents.
    pub async fn delete_team(&self, name: &str) -> anyhow::Result<()> {
        let mut teams = self.teams.write().await;
        if let Some(coord_ref) = teams.remove(name) {
            info!(team = %name, "Deleting team");
            coord_ref.kill();
            Ok(())
        } else {
            anyhow::bail!("Team '{}' not found", name);
        }
    }

    /// Spawn a new agent in the given team.
    pub async fn spawn_agent(
        &self,
        team_name: &str,
        agent_name: &str,
        model: Option<String>,
        prompt: Option<String>,
        cwd: Option<String>,
    ) -> anyhow::Result<SpawnResult> {
        let teams = self.teams.read().await;
        let coord = teams.get(team_name)
            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

        let result = coord.ask(SpawnAgent {
            name: agent_name.to_string(),
            model,
            prompt,
            cwd,
        }).await.map_err(|e| anyhow::anyhow!("Spawn failed: {}", e))?;

        Ok(result)
    }

    /// Terminate an agent by ID.
    pub async fn terminate_agent(&self, team_name: &str, agent_id: &str) -> anyhow::Result<TerminateResult> {
        let teams = self.teams.read().await;
        let coord = teams.get(team_name)
            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

        let result = coord.ask(TerminateAgent {
            agent_id: agent_id.to_string(),
        }).await.map_err(|e| anyhow::anyhow!("Terminate failed: {}", e))?;

        Ok(result)
    }

    /// Send a message to a specific agent.
    pub async fn send_message(
        &self,
        team_name: &str,
        target_agent_id: &str,
        prompt: &str,
        from: Option<&str>,
    ) -> anyhow::Result<RouteResult> {
        let teams = self.teams.read().await;
        let coord = teams.get(team_name)
            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

        let result = coord.ask(crate::actors::RouteMessage {
            target_agent_id: target_agent_id.to_string(),
            query: AgentQuery {
                prompt: prompt.to_string(),
                from: from.map(|s| s.to_string()),
            },
        }).await.map_err(|e| anyhow::anyhow!("Route failed: {}", e))?;

        Ok(result)
    }

    /// Broadcast a message to all agents in a team.
    pub async fn broadcast(
        &self,
        team_name: &str,
        text: &str,
        from: &str,
    ) -> anyhow::Result<Vec<RouteResult>> {
        let teams = self.teams.read().await;
        let coord = teams.get(team_name)
            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

        let results = coord.ask(BroadcastMessage {
            text: text.to_string(),
            from: from.to_string(),
        }).await.map_err(|e| anyhow::anyhow!("Broadcast failed: {}", e))?;

        Ok(results.0)
    }

    /// Get the status of a specific agent.
    pub async fn agent_status(&self, team_name: &str, agent_id: &str) -> anyhow::Result<AgentStatus> {
        let teams = self.teams.read().await;
        let coord = teams.get(team_name)
            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

        let team_status = coord.ask(GetTeamStatus)
            .await
            .map_err(|e| anyhow::anyhow!("Status query failed: {}", e))?;

        team_status.agents.into_iter()
            .find(|a| a.agent_id == agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found in team '{}'", agent_id, team_name))
    }

    /// Get the status of a team.
    pub async fn team_status(&self, team_name: &str) -> anyhow::Result<TeamStatus> {
        let teams = self.teams.read().await;
        let coord = teams.get(team_name)
            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

        let status = coord.ask(GetTeamStatus)
            .await
            .map_err(|e| anyhow::anyhow!("Status query failed: {}", e))?;

        Ok(status)
    }

    /// List all team names.
    pub async fn list_teams(&self) -> Vec<String> {
        let teams = self.teams.read().await;
        teams.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn network_create_and_delete_team() {
        let net = SwarmNetwork::new("claude-haiku".into(), "/tmp".into());

        // Create
        let name = net.create_team("alpha").await.unwrap();
        assert_eq!(name, "alpha");

        // List
        let teams = net.list_teams().await;
        assert_eq!(teams, vec!["alpha"]);

        // Duplicate should fail
        assert!(net.create_team("alpha").await.is_err());

        // Delete
        net.delete_team("alpha").await.unwrap();
        assert!(net.list_teams().await.is_empty());

        // Delete nonexistent should fail
        assert!(net.delete_team("alpha").await.is_err());
    }

    #[tokio::test]
    async fn network_full_workflow() {
        let net = SwarmNetwork::new("claude-haiku".into(), "/project".into());
        net.create_team("dev").await.unwrap();

        // Spawn agents
        let spawn = net.spawn_agent("dev", "coder", None, Some("Write code".into()), None).await.unwrap();
        assert!(spawn.success);

        let spawn2 = net.spawn_agent("dev", "reviewer", None, Some("Review code".into()), None).await.unwrap();
        assert!(spawn2.success);

        // Send message
        let result = net.send_message("dev", &spawn.agent_id, "Implement login", Some("team-lead@dev")).await.unwrap();
        assert!(result.success);

        // Agent status
        let status = net.agent_status("dev", &spawn.agent_id).await.unwrap();
        assert_eq!(status.turn_count, 1);

        // Team status
        let ts = net.team_status("dev").await.unwrap();
        assert_eq!(ts.agent_count, 2);

        // Broadcast
        let bcast = net.broadcast("dev", "All hands meeting", "team-lead@dev").await.unwrap();
        assert_eq!(bcast.len(), 2); // both coder and reviewer receive

        // Terminate
        let term = net.terminate_agent("dev", &spawn.agent_id).await.unwrap();
        assert!(term.success);

        let ts2 = net.team_status("dev").await.unwrap();
        assert_eq!(ts2.agent_count, 1);

        // Cleanup
        net.delete_team("dev").await.unwrap();
    }

    #[tokio::test]
    async fn network_missing_team_errors() {
        let net = SwarmNetwork::new("claude-haiku".into(), "/tmp".into());

        assert!(net.spawn_agent("nope", "agent", None, None, None).await.is_err());
        assert!(net.send_message("nope", "a@b", "hi", None).await.is_err());
        assert!(net.team_status("nope").await.is_err());
        assert!(net.terminate_agent("nope", "a@b").await.is_err());
    }
}
