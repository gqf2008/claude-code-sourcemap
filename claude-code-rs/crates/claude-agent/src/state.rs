use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use claude_core::permissions::PermissionMode;
use claude_core::message::Message;

#[derive(Debug, Clone)]
pub struct AppState {
    pub model: String,
    pub permission_mode: PermissionMode,
    pub verbose: bool,
    pub messages: Vec<Message>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub turn_count: u32,
    /// Cumulative error tracking for diagnostics and circuit breaking.
    pub error_counts: HashMap<String, u32>,
    pub total_errors: u32,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
}

impl AppState {
    /// Record an error by category (e.g., "rate_limit", "overloaded", "timeout").
    pub fn record_error(&mut self, category: &str) {
        *self.error_counts.entry(category.to_string()).or_insert(0) += 1;
        self.total_errors += 1;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            permission_mode: PermissionMode::Default,
            verbose: false,
            messages: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_count: 0,
            error_counts: HashMap::new(),
            total_errors: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
        }
    }
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn new_shared_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}
