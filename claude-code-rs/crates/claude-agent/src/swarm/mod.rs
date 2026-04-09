//! Swarm team management — multi-agent team orchestration.
//!
//! Implements the swarm pattern where a coordinator (team lead) spawns
//! named teammates, tracks their state via on-disk TeamFile, and manages
//! team lifecycle (create / delete / status).

pub mod types;
pub mod helpers;
pub mod team_create;
pub mod team_delete;
pub mod team_status;
pub mod conflict;

pub use types::*;
pub use helpers::*;
pub use team_create::TeamCreateTool;
pub use team_delete::TeamDeleteTool;
pub use team_status::TeamStatusTool;
pub use conflict::FileConflictTracker;
