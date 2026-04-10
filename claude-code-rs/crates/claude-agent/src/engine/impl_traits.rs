//! `AgentEngine` trait implementation for `QueryEngine`.

use std::pin::Pin;

use async_trait::async_trait;
use claude_api::types::ThinkingConfig;
use claude_core::message::{ContentBlock, Message};
use claude_core::tool::AbortSignal;
use futures::Stream;

use crate::cost::CostTracker;
use crate::query::AgentEvent;
use crate::task_runner::{TaskProgress, TaskResult};
use crate::traits::AgentEngine;
use super::QueryEngine;

#[async_trait]
impl AgentEngine for QueryEngine {
    async fn submit(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        QueryEngine::submit(self, prompt.to_string()).await
    }

    async fn submit_with_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        QueryEngine::submit_with_content(self, content).await
    }

    fn abort(&self) {
        QueryEngine::abort(self)
    }

    fn abort_signal(&self) -> AbortSignal {
        QueryEngine::abort_signal(self)
    }

    fn session_id(&self) -> &str {
        QueryEngine::session_id(self)
    }

    fn cwd(&self) -> &std::path::Path {
        QueryEngine::cwd(self)
    }

    async fn model(&self) -> String {
        let s = self.state().read().await;
        s.model.clone()
    }

    async fn set_model(&self, model: &str) {
        let mut s = self.state().write().await;
        s.model = model.to_string();
    }

    fn is_coordinator(&self) -> bool {
        QueryEngine::is_coordinator(self)
    }

    fn tool_count(&self) -> usize {
        QueryEngine::tool_count(self)
    }

    fn tool_list(&self) -> Vec<(String, String, bool)> {
        QueryEngine::tool_list(self)
    }

    fn cost_tracker(&self) -> &CostTracker {
        QueryEngine::cost_tracker(self)
    }

    async fn context_usage_percent(&self) -> Option<u8> {
        QueryEngine::context_usage_percent(self).await
    }

    async fn should_auto_compact(&self) -> bool {
        QueryEngine::should_auto_compact(self).await
    }

    async fn compact(
        &self,
        trigger: &str,
        custom_instructions: Option<&str>,
    ) -> anyhow::Result<String> {
        QueryEngine::compact(self, trigger, custom_instructions).await
    }

    async fn record_compact_success(&self) {
        QueryEngine::record_compact_success(self).await
    }

    async fn record_compact_failure(&self) {
        QueryEngine::record_compact_failure(self).await
    }

    async fn clear_history(&self) {
        QueryEngine::clear_history(self).await
    }

    async fn rewind_turns(&self, n: usize) -> (usize, usize) {
        QueryEngine::rewind_turns(self, n).await
    }

    async fn last_user_prompt(&self) -> Option<String> {
        QueryEngine::last_user_prompt(self).await
    }

    async fn pop_last_turn(&self) -> Option<String> {
        QueryEngine::pop_last_turn(self).await
    }

    async fn save_session(&self) -> anyhow::Result<()> {
        QueryEngine::save_session(self).await
    }

    async fn restore_session(&self, session_id: &str) -> anyhow::Result<String> {
        QueryEngine::restore_session(self, session_id).await
    }

    async fn rename_session(&self, name: &str) -> anyhow::Result<()> {
        QueryEngine::rename_session(self, name).await
    }

    fn thinking_config(&self) -> Option<ThinkingConfig> {
        QueryEngine::thinking_config(self)
    }

    fn set_thinking(&self, config: Option<ThinkingConfig>) {
        QueryEngine::set_thinking(self, config)
    }

    fn set_break_cache(&self) {
        QueryEngine::set_break_cache(self)
    }

    async fn drain_notifications(&self) -> Vec<Message> {
        QueryEngine::drain_notifications(self).await
    }

    async fn send_to_agent(&self, agent_id: &str, message: &str) -> anyhow::Result<()> {
        QueryEngine::send_to_agent(self, agent_id, message).await
    }

    async fn cancel_agent(&self, agent_id: &str) -> anyhow::Result<()> {
        QueryEngine::cancel_agent(self, agent_id).await
    }

    async fn update_system_prompt_context(&self, claude_md: &str) {
        QueryEngine::update_system_prompt_context(self, claude_md).await
    }

    async fn run_session_start(&self) -> Option<String> {
        QueryEngine::run_session_start(self).await
    }

    async fn run_task(
        &self,
        task: &str,
        on_progress: Box<dyn FnMut(TaskProgress) + Send>,
    ) -> TaskResult {
        crate::task_runner::run_task(self, task, on_progress).await
    }
}
