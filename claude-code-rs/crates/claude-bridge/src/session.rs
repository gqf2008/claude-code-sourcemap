//! SessionRouter — maps platform channels to Agent sessions.
//!
//! Each platform channel (e.g., a Feishu group chat, a Telegram private chat)
//! is mapped to a unique Agent session with its own `ClientHandle`. The router
//! creates sessions on demand and cleans up idle ones.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tracing::{debug, info};

use claude_bus::bus::{BusHandle, ClientHandle};

use crate::message::ChannelId;

/// A session bound to a platform channel.
struct ChannelSession {
    /// Bus client handle for this session.
    client: ClientHandle,
    /// When the session was last active.
    last_active: Instant,
    /// Unique session ID.
    session_id: String,
}

/// Routes platform channels to Agent sessions via the Event Bus.
///
/// Thread-safe: wrap in `Arc<Mutex<SessionRouter>>` for shared access.
pub struct SessionRouter {
    /// Active sessions keyed by channel ID.
    sessions: HashMap<ChannelId, ChannelSession>,
    /// Bus handle for creating new client handles.
    bus: BusHandle,
    /// Session counter for generating unique IDs.
    next_id: u64,
    /// Idle timeout for session cleanup.
    idle_timeout: Duration,
}

impl SessionRouter {
    /// Create a new router bound to a bus handle.
    pub fn new(bus: BusHandle, idle_timeout: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            bus,
            next_id: 0,
            idle_timeout,
        }
    }

    /// Get or create a session for a channel.
    ///
    /// Returns a reference to the `ClientHandle` and the session ID.
    pub fn get_or_create(&mut self, channel_id: &ChannelId) -> (&ClientHandle, &str) {
        if !self.sessions.contains_key(channel_id) {
            let client = self.bus.new_client();
            self.next_id += 1;
            let session_id = format!("bridge-{}-{}", channel_id, self.next_id);
            info!("New session: {} for channel {}", session_id, channel_id);
            self.sessions.insert(channel_id.clone(), ChannelSession {
                client,
                last_active: Instant::now(),
                session_id,
            });
        }

        let session = self.sessions.get_mut(channel_id).unwrap();
        session.last_active = Instant::now();
        (&session.client, &session.session_id)
    }

    /// Get a mutable reference to the ClientHandle for a channel.
    ///
    /// Returns None if no session exists for this channel.
    pub fn get_client_mut(&mut self, channel_id: &ChannelId) -> Option<&mut ClientHandle> {
        self.sessions.get_mut(channel_id).map(|s| {
            s.last_active = Instant::now();
            &mut s.client
        })
    }

    /// Destroy a session for a channel (e.g., user sends /new).
    pub fn destroy(&mut self, channel_id: &ChannelId) -> bool {
        if let Some(session) = self.sessions.remove(channel_id) {
            info!("Destroyed session: {} for channel {}", session.session_id, channel_id);
            true
        } else {
            false
        }
    }

    /// Check if a session exists for a channel.
    pub fn has_session(&self, channel_id: &ChannelId) -> bool {
        self.sessions.contains_key(channel_id)
    }

    /// Get the session ID for a channel (if it exists).
    pub fn session_id(&self, channel_id: &ChannelId) -> Option<&str> {
        self.sessions.get(channel_id).map(|s| s.session_id.as_str())
    }

    /// Get a notification subscriber for a channel's session.
    ///
    /// Returns a new `broadcast::Receiver` that receives `AgentNotification`s
    /// from the bus. Returns `None` if no session exists for this channel.
    pub fn get_client_subscriber(
        &self,
        channel_id: &ChannelId,
    ) -> Option<tokio::sync::broadcast::Receiver<claude_bus::events::AgentNotification>> {
        self.sessions.get(channel_id).map(|s| s.client.subscribe_notifications())
    }

    /// Get the number of active sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Clean up sessions that have been idle longer than `idle_timeout`.
    ///
    /// Returns the number of sessions cleaned up.
    pub fn cleanup_idle(&mut self) -> usize {
        let threshold = Instant::now() - self.idle_timeout;
        let before = self.sessions.len();

        self.sessions.retain(|channel_id, session| {
            let keep = session.last_active >= threshold;
            if !keep {
                debug!("Cleaning up idle session: {} for {}", session.session_id, channel_id);
            }
            keep
        });

        before - self.sessions.len()
    }

    /// List all active channel IDs.
    pub fn active_channels(&self) -> Vec<&ChannelId> {
        self.sessions.keys().collect()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use claude_bus::bus::EventBus;

    fn make_channel(platform: &str, ch: &str) -> ChannelId {
        ChannelId::new(platform, ch)
    }

    #[test]
    fn create_and_get_session() {
        let (bus, _client) = EventBus::new(64);
        let mut router = SessionRouter::new(bus, Duration::from_secs(3600));

        let ch = make_channel("feishu", "oc_abc");

        // First creation
        router.get_or_create(&ch);
        assert_eq!(router.session_count(), 1);
        let sid = router.session_id(&ch).unwrap().to_string();
        assert!(sid.contains("bridge-"));

        // Second call should return the same session
        router.get_or_create(&ch);
        assert_eq!(router.session_count(), 1);
        let sid2 = router.session_id(&ch).unwrap().to_string();
        assert_eq!(sid, sid2);
    }

    #[test]
    fn different_channels_get_different_sessions() {
        let (bus, _client) = EventBus::new(64);
        let mut router = SessionRouter::new(bus, Duration::from_secs(3600));

        let ch1 = make_channel("feishu", "oc_1");
        let ch2 = make_channel("feishu", "oc_2");

        router.get_or_create(&ch1);
        router.get_or_create(&ch2);
        assert_eq!(router.session_count(), 2);
    }

    #[test]
    fn destroy_session() {
        let (bus, _client) = EventBus::new(64);
        let mut router = SessionRouter::new(bus, Duration::from_secs(3600));

        let ch = make_channel("telegram", "123");
        router.get_or_create(&ch);
        assert!(router.has_session(&ch));

        assert!(router.destroy(&ch));
        assert!(!router.has_session(&ch));
        assert_eq!(router.session_count(), 0);

        // Destroying non-existent session
        assert!(!router.destroy(&ch));
    }

    #[test]
    fn cleanup_idle_sessions() {
        let (bus, _client) = EventBus::new(64);
        // Use 0 timeout so everything is idle immediately
        let mut router = SessionRouter::new(bus, Duration::from_millis(0));

        let ch1 = make_channel("feishu", "a");
        let ch2 = make_channel("feishu", "b");
        router.get_or_create(&ch1);
        router.get_or_create(&ch2);
        assert_eq!(router.session_count(), 2);

        // Sleep a tiny bit to ensure they're past the 0ms threshold
        std::thread::sleep(Duration::from_millis(10));
        let cleaned = router.cleanup_idle();
        assert_eq!(cleaned, 2);
        assert_eq!(router.session_count(), 0);
    }

    #[test]
    fn active_channels_list() {
        let (bus, _client) = EventBus::new(64);
        let mut router = SessionRouter::new(bus, Duration::from_secs(3600));

        let ch1 = make_channel("feishu", "a");
        let ch2 = make_channel("telegram", "b");
        router.get_or_create(&ch1);
        router.get_or_create(&ch2);

        let channels = router.active_channels();
        assert_eq!(channels.len(), 2);
    }

    #[test]
    fn get_client_mut() {
        let (bus, _client) = EventBus::new(64);
        let mut router = SessionRouter::new(bus, Duration::from_secs(3600));

        let ch = make_channel("feishu", "a");
        assert!(router.get_client_mut(&ch).is_none());

        router.get_or_create(&ch);
        assert!(router.get_client_mut(&ch).is_some());
    }
}
