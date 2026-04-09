//! Non-React cron scheduler core.
//!
//! Lifecycle: start() → poll for tasks → load + watch → 1s check timer → fire.
//! Shared between REPL and headless modes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use claude_core::cron_lock::{release_scheduler_lock, try_acquire_scheduler_lock};
use claude_core::cron_tasks::{
    has_cron_tasks_sync, is_recurring_task_aged, jittered_next_cron_run_ms,
    mark_cron_tasks_fired, next_cron_run_ms, one_shot_jittered_next_cron_run_ms,
    read_cron_tasks, remove_cron_tasks, CronJitterConfig, CronTask,
};

const CHECK_INTERVAL_MS: u64 = 1000;
const LOCK_PROBE_INTERVAL_MS: u64 = 5000;

/// Callback when a task fires.
pub type OnFire = Arc<dyn Fn(&CronTask) + Send + Sync>;
/// Callback to check if the engine is busy.
pub type IsLoading = Arc<dyn Fn() -> bool + Send + Sync>;

/// Options for creating a cron scheduler.
pub struct CronSchedulerOptions {
    /// Project directory containing .claude/scheduled_tasks.json.
    pub dir: PathBuf,
    /// Stable session identifier for the lock file.
    pub session_id: String,
    /// Called when a task fires (receives the full CronTask).
    pub on_fire: OnFire,
    /// While true, firing is deferred to the next tick.
    pub is_loading: IsLoading,
}

/// Handle for the running cron scheduler.
pub struct CronScheduler {
    /// Signal to stop the scheduler.
    stop_tx: tokio::sync::watch::Sender<bool>,
    /// Join handle for the scheduler task.
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl CronScheduler {
    /// Start the cron scheduler. Returns a handle to stop it.
    pub fn start(opts: CronSchedulerOptions) -> Self {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

        let handle = tokio::spawn(scheduler_loop(opts, stop_rx));

        Self {
            stop_tx,
            handle: Some(handle),
        }
    }

    /// Get the next fire time across all loaded tasks.
    /// Note: this is approximate — the actual state is inside the spawned task.
    pub fn stop(&mut self) {
        let _ = self.stop_tx.send(true);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for CronScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

struct SchedulerState {
    /// File-backed tasks loaded from disk.
    tasks: Vec<CronTask>,
    /// Per-task next-fire times (epoch ms).
    next_fire_at: HashMap<String, i64>,
    /// Tasks currently in-flight (being removed from disk).
    in_flight: HashSet<String>,
    /// Whether we own the scheduler lock.
    is_owner: bool,
    /// Ids of missed tasks already surfaced.
    missed_asked: HashSet<String>,
}

async fn scheduler_loop(
    opts: CronSchedulerOptions,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    let dir = opts.dir.clone();
    let session_id = opts.session_id.clone();

    // Wait until tasks exist or stop is signaled
    if !has_cron_tasks_sync(&dir) {
        let mut poll = tokio::time::interval(tokio::time::Duration::from_millis(CHECK_INTERVAL_MS));
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    if has_cron_tasks_sync(&dir) {
                        break;
                    }
                }
                _ = stop_rx.changed() => return,
            }
        }
    }

    // Try to acquire the scheduler lock
    let is_owner = try_acquire_scheduler_lock(&dir, &session_id)
        .await
        .unwrap_or(false);

    let state = Arc::new(Mutex::new(SchedulerState {
        tasks: Vec::new(),
        next_fire_at: HashMap::new(),
        in_flight: HashSet::new(),
        is_owner,
        missed_asked: HashSet::new(),
    }));

    // Initial load
    {
        let tasks = read_cron_tasks(&dir).await;
        let mut s = state.lock().await;
        s.tasks = tasks;
        // Surface missed one-shot tasks on initial load
        if s.is_owner {
            surface_missed_tasks(&mut s, &opts);
        }
    }

    // Start check timer
    let mut check_interval =
        tokio::time::interval(tokio::time::Duration::from_millis(CHECK_INTERVAL_MS));
    let mut lock_probe_interval =
        tokio::time::interval(tokio::time::Duration::from_millis(LOCK_PROBE_INTERVAL_MS));
    // Reload tasks periodically (poor man's file watcher)
    let mut reload_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = check_interval.tick() => {
                let mut s = state.lock().await;
                if s.is_owner {
                    check(&mut s, &opts).await;
                }
            }
            _ = lock_probe_interval.tick() => {
                let mut s = state.lock().await;
                if !s.is_owner && matches!(try_acquire_scheduler_lock(&dir, &session_id).await, Ok(true)) {
                    s.is_owner = true;
                    tracing::debug!("acquired scheduler lock (probe)");
                }
            }
            _ = reload_interval.tick() => {
                let tasks = read_cron_tasks(&dir).await;
                let mut s = state.lock().await;
                s.tasks = tasks;
            }
            _ = stop_rx.changed() => break,
        }
    }

    // Cleanup
    let s = state.lock().await;
    if s.is_owner {
        let _ = release_scheduler_lock(&dir, &session_id).await;
    }
}

fn surface_missed_tasks(state: &mut SchedulerState, opts: &CronSchedulerOptions) {
    let now = chrono::Utc::now().timestamp_millis();
    let missed: Vec<CronTask> = state
        .tasks
        .iter()
        .filter(|t| {
            if t.recurring || state.missed_asked.contains(&t.id) {
                return false;
            }
            if let Some(next) = next_cron_run_ms(&t.cron, t.created_at) {
                next < now
            } else {
                false
            }
        })
        .cloned()
        .collect();

    for t in &missed {
        state.missed_asked.insert(t.id.clone());
        state.next_fire_at.insert(t.id.clone(), i64::MAX);
        (opts.on_fire)(t);
    }

    if !missed.is_empty() {
        let ids: Vec<String> = missed.iter().map(|t| t.id.clone()).collect();
        let dir = opts.dir.clone();
        tokio::spawn(async move {
            let _ = remove_cron_tasks(&ids, &dir).await;
        });
    }
}

async fn check(state: &mut SchedulerState, opts: &CronSchedulerOptions) {
    if (opts.is_loading)() {
        return;
    }

    let now = chrono::Utc::now().timestamp_millis();
    let cfg = CronJitterConfig::default();
    let mut seen = HashSet::new();
    let mut fired_file_recurring = Vec::new();

    // Process all tasks
    let tasks: Vec<CronTask> = state.tasks.clone();
    for t in &tasks {
        seen.insert(t.id.clone());
        if state.in_flight.contains(&t.id) {
            continue;
        }

        let next = state.next_fire_at.entry(t.id.clone()).or_insert_with(|| {
            let anchor = t.last_fired_at.unwrap_or(t.created_at);
            if t.recurring {
                jittered_next_cron_run_ms(&t.cron, anchor, &t.id, &cfg).unwrap_or(i64::MAX)
            } else {
                one_shot_jittered_next_cron_run_ms(&t.cron, t.created_at, &t.id, &cfg)
                    .unwrap_or(i64::MAX)
            }
        });

        if now < *next {
            continue;
        }

        tracing::debug!(
            task_id = %t.id,
            recurring = t.recurring,
            "firing scheduled task"
        );

        (opts.on_fire)(t);

        let aged = is_recurring_task_aged(t, now, cfg.recurring_max_age_ms);

        if t.recurring && !aged {
            // Reschedule from now
            let new_next =
                jittered_next_cron_run_ms(&t.cron, now, &t.id, &cfg).unwrap_or(i64::MAX);
            state.next_fire_at.insert(t.id.clone(), new_next);
            fired_file_recurring.push(t.id.clone());
        } else {
            // One-shot or aged recurring: delete
            state.in_flight.insert(t.id.clone());
            state.next_fire_at.remove(&t.id);
            let id = t.id.clone();
            let dir = opts.dir.clone();
            let in_flight_cleanup = id.clone();
            tokio::spawn(async move {
                let _ = remove_cron_tasks(&[id], &dir).await;
                // Note: in_flight cleanup happens on next reload
                drop(in_flight_cleanup);
            });
        }
    }

    // Batch persist lastFiredAt for recurring tasks
    if !fired_file_recurring.is_empty() {
        let dir = opts.dir.clone();
        let ids = fired_file_recurring;
        tokio::spawn(async move {
            let _ = mark_cron_tasks_fired(&ids, now, &dir).await;
        });
    }

    // Evict stale schedule entries
    state
        .next_fire_at
        .retain(|id, _| seen.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn test_scheduler_start_stop() {
        let dir = tempfile::TempDir::new().unwrap();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_clone = fired.clone();

        let mut scheduler = CronScheduler::start(CronSchedulerOptions {
            dir: dir.path().to_path_buf(),
            session_id: "test-session".to_string(),
            on_fire: Arc::new(move |_task| {
                fired_clone.fetch_add(1, Ordering::Relaxed);
            }),
            is_loading: Arc::new(|| false),
        });

        // Let it run briefly
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        scheduler.stop();

        // No tasks, so nothing should have fired
        assert_eq!(fired.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_scheduler_fires_task() {
        let dir = tempfile::TempDir::new().unwrap();

        // Write a task that should fire immediately (cron every minute, created long ago)
        let task = CronTask {
            id: "test1234".to_string(),
            cron: "* * * * *".to_string(),
            prompt: "hello".to_string(),
            created_at: chrono::Utc::now().timestamp_millis() - 120_000,
            last_fired_at: None,
            recurring: false,
            permanent: false,
        };
        claude_core::cron_tasks::write_cron_tasks(&[task], dir.path())
            .await
            .unwrap();

        let fired = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();

        let mut scheduler = CronScheduler::start(CronSchedulerOptions {
            dir: dir.path().to_path_buf(),
            session_id: "test-session".to_string(),
            on_fire: Arc::new(move |task| {
                let fired = fired_clone.clone();
                let prompt = task.prompt.clone();
                tokio::spawn(async move {
                    fired.lock().await.push(prompt);
                });
            }),
            is_loading: Arc::new(|| false),
        });

        // Wait for the scheduler to pick up and fire
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        scheduler.stop();

        let prompts = fired.lock().await;
        assert!(!prompts.is_empty(), "scheduler should have fired the task");
        assert_eq!(prompts[0], "hello");
    }
}
