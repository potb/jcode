#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Background ambient mode runner.
//!
//! Spawned by the server when ambient mode is enabled. Manages the lifecycle of
//! ambient cycles: scheduling, spawning agent sessions, handling results, and
//! providing status for the TUI widget and debug socket.

use crate::agent::Agent;
use crate::ambient::{
    self, AmbientCycleResult, AmbientLock, AmbientManager, AmbientState, AmbientStatus,
    CycleStatus, ScheduleTarget, ScheduledItem, cycle_significance, schedule_window,
};
use crate::ambient_scheduler::{AdaptiveScheduler, AmbientSchedulerConfig};
use crate::config::config;
use crate::logging;
use crate::memory::MemoryManager;
use crate::notifications::NotificationDispatcher;
use crate::provider::Provider;
use crate::safety::SafetySystem;
use crate::session::Session;
use crate::tool;
use crate::tool::ambient as ambient_tools;
use chrono::{DateTime, Local, Utc};
use jcode_agent_runtime::{SoftInterruptMessage, SoftInterruptQueue, SoftInterruptSource};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

const MAX_IDLE_POLL_SECS: u64 = 30;

/// Ceiling on how long a closed window may park the runner.
///
/// A window that opens on Monday morning is legitimately days away, but
/// sleeping that whole span would ignore a config edit, a manual trigger or a
/// clock change until it elapsed. Waking hourly to re-evaluate costs nothing
/// and keeps the runner responsive to the world changing underneath it.
const MAX_CLOSED_WINDOW_SLEEP_SECS: u64 = 3600;

/// The windows configured for ambient, warning once about unparseable specs.
///
/// A typo must not silently widen an allow-list into "always on", so bad
/// entries are reported rather than dropped in silence.
fn configured_windows() -> Vec<schedule_window::ScheduleWindow> {
    let specs = config().ambient.active_windows.clone();
    let (windows, bad) = schedule_window::parse_windows(&specs);
    if !bad.is_empty() {
        logging::warn(&format!(
            "Ambient: ignoring unparseable active_windows entries: {}. \
             Expected e.g. \"weekdays 09:00-19:00\".",
            bad.join(", ")
        ));
    }
    windows
}

/// Evaluate the configured windows against the current local time.
///
/// Fails OPEN: when nothing is configured, or every entry is invalid, ambient
/// runs as it always did. A time constraint the user never expressed must not
/// be invented, and a config typo must not silently disable ambient outright.
fn current_window_state() -> schedule_window::WindowState {
    let windows = configured_windows();
    schedule_window::evaluate(&windows, &Local::now())
}

/// Whether an ambient cycle may start right now.
///
/// Extracted from the idle loop so the window constraint is testable on its
/// own. Inlined in the loop it was reachable only by running the daemon across
/// a real weekend, which is precisely how a constraint ends up wired to
/// nothing while its own unit tests stay green.
fn may_start_cycle(ambient_allowed: bool, window_open: bool, wants_to_run: bool) -> bool {
    ambient_allowed && window_open && wants_to_run
}

/// How long to idle when a wall-clock window is closed.
///
/// An ambient deadline inside the closed period must NOT shorten this: it
/// cannot be serviced before the window opens, so honouring it would spin the
/// loop. A direct-delivery deadline still counts, since those go to a session
/// the user is actively in and are not subject to the window.
fn closed_window_sleep_secs(
    now_utc: DateTime<Utc>,
    now_local: DateTime<Local>,
    next_open: Option<DateTime<Local>>,
    next_direct_due: Option<DateTime<Utc>>,
) -> u64 {
    let until_open = schedule_window::sleep_secs_until_open(
        &now_local,
        next_open,
        MAX_CLOSED_WINDOW_SLEEP_SECS,
    );
    idle_sleep_secs(now_utc, until_open, next_direct_due)
}

/// The soonest of two optional deadlines.
///
/// The runner tracks direct-delivery and ambient-cycle queue deadlines
/// separately because they are serviced differently, but when deciding how long
/// to sleep only the nearer one matters.
fn earliest_deadline(
    a: Option<DateTime<Utc>>,
    b: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (only, None) | (None, only) => only,
    }
}

/// How long the idle loop should sleep.
///
/// `interval` is the adaptive maintenance interval and acts as the ceiling.
/// A known deadline lowers it so queued work is not delayed by up to a full
/// interval, but only when that deadline is still in the future: we are in the
/// idle branch because something blocks running (a cycle in flight, ambient
/// paused), and a past deadline would otherwise re-arm a ~1s timer every pass
/// and spin the loop.
///
/// The remaining time is rounded UP to whole seconds. Truncating it instead
/// collapses any deadline under a second to zero, which reads as "already
/// past" and falls back to the full interval, sleeping hours through work due
/// imminently. Rounding up also keeps the wake at or after the deadline, so
/// the item is genuinely due on the next pass rather than one pass early.
fn idle_sleep_secs(
    now: DateTime<Utc>,
    interval_secs: u64,
    next_deadline: Option<DateTime<Utc>>,
) -> u64 {
    next_deadline
        .map(|d| (d - now).num_milliseconds())
        .filter(|ms| *ms > 0)
        .map(|ms| interval_secs.min(((ms + 999) / 1000).max(1) as u64))
        .unwrap_or(interval_secs)
}

/// The wake time to persist after a cycle, given what the cycle asked for.
///
/// A cycle can request its own next wake via `schedule_ambient`, which
/// `record_cycle` writes as a `Scheduled` status before the runner gets here.
/// That request is a preference, not an override: `computed` is what the
/// resource budget actually allows, and the documented contract is that an
/// agent asking to wake later than necessary is pulled forward.
///
/// Keeping the two in agreement matters beyond tidiness. `should_run` gates on
/// the persisted value, so letting a distant request win silently reimposed
/// that distance on the whole loop while the runner's own sleep said otherwise.
fn reconciled_next_wake(
    current: &AmbientStatus,
    computed: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match current {
        AmbientStatus::Scheduled { next_wake } => Some((*next_wake).min(computed)),
        AmbientStatus::Running { .. } | AmbientStatus::Idle => Some(computed),
        // Disabled or Paused: not ours to reschedule.
        _ => None,
    }
}

/// The status a runner should adopt at startup, or `None` to leave it alone.
///
/// A persisted `Running` status means the previous process died mid-cycle
/// (crash, restart, or a reload that swapped the binary underneath it).
/// Nothing else ever clears it, and `should_run` treats `Running` as "already
/// busy", so ambient would stay wedged forever.
///
/// It is only ours to reclaim when no OTHER live daemon holds the lock. If one
/// does, that `Running` belongs to a cycle happening right now and resetting it
/// to idle would let two cycles run at once.
fn reclaimed_startup_status(
    status: &AmbientStatus,
    lock_is_foreign: bool,
) -> Option<AmbientStatus> {
    if lock_is_foreign {
        return None;
    }
    matches!(status, AmbientStatus::Running { .. }).then_some(AmbientStatus::Idle)
}

/// Restore queue items abandoned by a cycle that died before finishing them.
///
/// Skipped entirely when another live process holds the lock: those items are
/// claimed by a cycle running right now, and "recovering" them would hand the
/// same work to a second cycle concurrently.
fn recover_abandoned_queue_items(lock_is_foreign: bool) -> usize {
    if lock_is_foreign {
        return 0;
    }
    match AmbientManager::new() {
        Ok(mut mgr) => mgr.recover_inflight_items(),
        Err(_) => 0,
    }
}

/// Shared ambient runner state, accessible from the server, debug socket, and TUI.
#[derive(Clone)]
pub struct AmbientRunnerHandle {
    inner: Arc<AmbientRunnerInner>,
}

struct AmbientRunnerInner {
    /// Current snapshot of ambient state (for queries)
    state: RwLock<AmbientState>,
    /// Queue item count for widget
    queue_count: RwLock<usize>,
    /// Next queue item context preview
    next_queue_preview: RwLock<Option<String>>,
    /// Wake notify (nudge the loop to re-check sooner)
    wake_notify: Notify,
    /// Whether the runner loop is active
    running: RwLock<bool>,
    /// Safety system shared with ambient tools
    safety: Arc<SafetySystem>,
    /// Notification dispatcher for push/email/desktop alerts
    notifier: NotificationDispatcher,
    /// Number of active user sessions (for pause logic)
    active_user_sessions: RwLock<usize>,
    /// Soft interrupt queue for the currently-running ambient agent (if any).
    /// Telegram replies push messages here so they arrive mid-cycle.
    active_cycle_queue: RwLock<Option<SoftInterruptQueue>>,
    /// A human asked for a cycle right now, so wall-clock windows do not apply.
    ///
    /// `active_windows` exists to stop the agent waking ITSELF at night or on
    /// weekends. Someone typing `jcode ambient trigger` at 2am is not the
    /// scheduled work that constraint holds back, and silently ignoring them
    /// is the same "reported success and did nothing" failure the trigger path
    /// already had to fix once.
    manual_override: RwLock<bool>,
}

impl AmbientRunnerHandle {
    pub fn new(safety: Arc<SafetySystem>) -> Self {
        let state = AmbientState::load().unwrap_or_default();
        Self {
            inner: Arc::new(AmbientRunnerInner {
                state: RwLock::new(state),
                queue_count: RwLock::new(0),
                next_queue_preview: RwLock::new(None),
                wake_notify: Notify::new(),
                running: RwLock::new(false),
                safety,
                notifier: NotificationDispatcher::new(),
                active_user_sessions: RwLock::new(0),
                active_cycle_queue: RwLock::new(None),
                manual_override: RwLock::new(false),
            }),
        }
    }

    /// Nudge the ambient loop to check sooner (e.g., after session close/crash).
    pub fn nudge(&self) {
        self.inner.wake_notify.notify_one();
    }

    /// Check if the runner loop is active.
    pub async fn is_running(&self) -> bool {
        *self.inner.running.read().await
    }

    /// Get current ambient state snapshot.
    pub async fn state(&self) -> AmbientState {
        self.inner.state.read().await.clone()
    }

    /// Get a reference to the safety system (for debug socket permission commands).
    pub fn safety(&self) -> &Arc<SafetySystem> {
        &self.inner.safety
    }

    /// Inject a message from an external channel (Telegram, Discord, etc.)
    /// into the active ambient cycle as a user message.
    /// If a cycle is running, the message goes in via soft interrupt (immediate).
    /// If no cycle is running, the message is saved as a directive and a cycle is triggered.
    /// Returns true if injected into active cycle, false if queued as directive.
    pub async fn inject_message(&self, text: &str, source: &str) -> bool {
        let queue = self.inner.active_cycle_queue.read().await;
        if let Some(ref q) = *queue
            && let Ok(mut q) = q.lock()
        {
            q.push(SoftInterruptMessage {
                content: format!("[{} message from user]\n{}", source, text),
                images: Vec::new(),
                urgent: false,
                source: SoftInterruptSource::User,
            });
            logging::info(&format!(
                "{} message injected into active ambient cycle: {}",
                source,
                crate::util::truncate_str(text, 60)
            ));
            return true;
        }
        drop(queue);

        // No active cycle — save as directive and trigger a wake
        let source_id = format!("{}_{}", source, chrono::Utc::now().timestamp());
        if let Err(e) = ambient::add_directive(text.to_string(), source_id) {
            logging::error(&format!("Failed to save {} directive: {}", source, e));
        }
        self.trigger().await;
        false
    }

    /// Manually trigger an ambient cycle (returns immediately, cycle runs async).
    pub async fn trigger(&self) {
        // Set status to Idle so `should_run` returns true.
        //
        // This MUST be persisted. `should_run` is evaluated on a freshly
        // loaded `AmbientManager`, which reads state from disk, so an
        // in-memory-only change is invisible to it: the loop woke, re-read the
        // old `Scheduled` status, decided it was not time yet and went back to
        // sleep. `jcode ambient trigger` reported success and did nothing,
        // which is the worst shape a manual override can take.
        {
            let mut state = self.inner.state.write().await;
            if matches!(
                state.status,
                AmbientStatus::Scheduled { .. } | AmbientStatus::Idle
            ) {
                state.status = AmbientStatus::Idle;
                if let Err(e) = state.save() {
                    logging::error(&format!(
                        "Ambient runner: failed to persist manual trigger: {}",
                        e
                    ));
                }
            }
        }
        // Bypass wall-clock windows for this cycle only.
        {
            let mut over = self.inner.manual_override.write().await;
            *over = true;
        }
        self.inner.wake_notify.notify_one();
    }

    /// Stop the ambient loop.
    pub async fn stop(&self) {
        let mut state = self.inner.state.write().await;
        state.status = AmbientStatus::Disabled;
        let _ = state.save();
        drop(state);
        self.inner.wake_notify.notify_one();
    }

    /// Start (or restart) the ambient loop. If the loop exited due to Disabled
    /// status, this resets the state to Idle and spawns a new loop task.
    pub async fn start(&self, provider: Arc<dyn Provider>) -> bool {
        let already_running = *self.inner.running.read().await;
        if already_running {
            return false;
        }
        {
            let mut state = self.inner.state.write().await;
            state.status = AmbientStatus::Idle;
            let _ = state.save();
        }
        let handle = self.clone();
        tokio::spawn(async move {
            handle.run_loop(provider).await;
        });
        true
    }

    /// Get status JSON for debug socket.
    pub async fn status_json(&self) -> String {
        let state = self.state().await;
        let running = self.is_running().await;
        let active_sessions = *self.inner.active_user_sessions.read().await;

        let (
            queue_count,
            next_preview,
            next_due,
            overdue_queue_count,
            reminder_count,
            next_reminder_preview,
            next_reminder_due,
            overdue_reminder_count,
        ) = match AmbientManager::new() {
            Ok(mgr) => {
                let now = Utc::now();
                let items = mgr.queue().items();
                let queue_count = items.len();
                let next_item = items.iter().min_by_key(|item| item.scheduled_for);
                let overdue_queue_count = items
                    .iter()
                    .filter(|item| item.scheduled_for <= now)
                    .count();
                let reminder_items: Vec<_> = items
                    .iter()
                    .filter(|item| item.target.is_direct_delivery())
                    .collect();
                let reminder_count = reminder_items.len();
                let next_reminder = reminder_items
                    .iter()
                    .min_by_key(|item| item.scheduled_for)
                    .copied();
                let overdue_reminder_count = reminder_items
                    .iter()
                    .filter(|item| item.scheduled_for <= now)
                    .count();

                (
                    queue_count,
                    next_item.map(|item| {
                        item.task_description
                            .as_deref()
                            .unwrap_or(&item.context)
                            .to_string()
                    }),
                    next_item.map(|item| item.scheduled_for.to_rfc3339()),
                    overdue_queue_count,
                    reminder_count,
                    next_reminder.map(|item| {
                        item.task_description
                            .as_deref()
                            .unwrap_or(&item.context)
                            .to_string()
                    }),
                    next_reminder.map(|item| item.scheduled_for.to_rfc3339()),
                    overdue_reminder_count,
                )
            }
            Err(_) => (0, None, None, 0, 0, None, None, 0),
        };

        let status_str = match &state.status {
            AmbientStatus::Idle => "idle".to_string(),
            AmbientStatus::Running { detail } => format!("running: {}", detail),
            AmbientStatus::Scheduled { next_wake } => {
                let until = *next_wake - Utc::now();
                let mins = until.num_minutes().max(0) as u32;
                format!(
                    "scheduled (in {})",
                    crate::ambient::format_minutes_human(mins)
                )
            }
            AmbientStatus::Paused { reason } => format!("paused: {}", reason),
            AmbientStatus::Disabled => "disabled".to_string(),
        };

        // Surface the wall-clock window so "why is it not running?" is
        // answerable from `ambient:status` rather than only from the logs.
        let windows = configured_windows();
        let window_state = schedule_window::evaluate(&windows, &Local::now());

        serde_json::json!({
            "enabled": config().ambient.enabled,
            "status": status_str,
            "active_windows": schedule_window::describe(&windows),
            "window_open": window_state.is_open(),
            "window_next_open": window_state.next_open_at().map(|t| t.to_rfc3339()),
            "loop_running": running,
            "total_cycles": state.total_cycles,
            "last_run": state.last_run.map(|t| t.to_rfc3339()),
            "last_summary": state.last_summary,
            "last_memories_modified": state.last_memories_modified,
            "last_compactions": state.last_compactions,
            "queue_count": queue_count,
            "next_queue_preview": next_preview,
            "next_queue_due": next_due,
            "overdue_queue_count": overdue_queue_count,
            "reminder_count": reminder_count,
            "next_reminder_preview": next_reminder_preview,
            "next_reminder_due": next_reminder_due,
            "overdue_reminder_count": overdue_reminder_count,
            "scheduled_task_count": reminder_count,
            "next_scheduled_task_preview": next_reminder_preview,
            "next_scheduled_task_due": next_reminder_due,
            "overdue_scheduled_task_count": overdue_reminder_count,
            "active_user_sessions": active_sessions,
        })
        .to_string()
    }

    /// Get queue items JSON for debug socket.
    pub async fn queue_json(&self) -> String {
        match AmbientManager::new() {
            Ok(mgr) => {
                let items: Vec<serde_json::Value> = mgr
                    .queue()
                    .items()
                    .iter()
                    .map(|item| {
                        let (target_kind, target_session_id, target_parent_session_id) =
                            match &item.target {
                                ScheduleTarget::Ambient => ("ambient", None, None),
                                ScheduleTarget::Session { session_id } => {
                                    ("session", Some(session_id.clone()), None)
                                }
                                ScheduleTarget::Spawn { parent_session_id } => {
                                    ("spawn", None, Some(parent_session_id.clone()))
                                }
                            };
                        let overdue_seconds =
                            (Utc::now() - item.scheduled_for).num_seconds().max(0);
                        serde_json::json!({
                            "id": item.id,
                            "scheduled_for": item.scheduled_for.to_rfc3339(),
                            "context": item.context,
                            "task_description": item.task_description,
                            "priority": format!("{:?}", item.priority),
                            "created_at": item.created_at.to_rfc3339(),
                            "target_kind": target_kind,
                            "target_session_id": target_session_id,
                            "target_parent_session_id": target_parent_session_id,
                            "working_dir": item.working_dir,
                            "relevant_files": item.relevant_files,
                            "git_branch": item.git_branch,
                            "overdue": item.scheduled_for <= Utc::now(),
                            "overdue_seconds": overdue_seconds,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
            }
            Err(e) => format!("{{\"error\": \"{}\"}}", e),
        }
    }

    /// Get recent transcript log summaries.
    pub async fn log_json(&self) -> String {
        let transcripts_dir = match crate::storage::jcode_dir() {
            Ok(d) => d.join("ambient").join("transcripts"),
            Err(e) => return format!("{{\"error\": \"{}\"}}", e),
        };

        if !transcripts_dir.exists() {
            return "[]".to_string();
        }

        let mut entries: Vec<serde_json::Value> = Vec::new();
        if let Ok(dir) = std::fs::read_dir(&transcripts_dir) {
            let mut files: Vec<_> = dir.flatten().collect();
            files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
            files.truncate(20);

            for entry in files {
                if let Ok(content) = std::fs::read_to_string(entry.path())
                    && let Ok(transcript) =
                        serde_json::from_str::<crate::safety::AmbientTranscript>(&content)
                {
                    entries.push(serde_json::json!({
                        "session_id": transcript.session_id,
                        "started_at": transcript.started_at.to_rfc3339(),
                        "ended_at": transcript.ended_at.map(|t| t.to_rfc3339()),
                        "status": format!("{:?}", transcript.status),
                        "summary": transcript.summary,
                        "memories_modified": transcript.memories_modified,
                        "compactions": transcript.compactions,
                    }));
                }
            }
        }

        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
    }

    async fn wait_for_request_done(
        client: &mut crate::server::Client,
        request_id: u64,
    ) -> anyhow::Result<()> {
        loop {
            match client.read_event().await? {
                crate::protocol::ServerEvent::Done { id } if id == request_id => return Ok(()),
                crate::protocol::ServerEvent::Error { id, message, .. } if id == request_id => {
                    anyhow::bail!(message)
                }
                _ => continue,
            }
        }
    }

    async fn notify_live_session(&self, session_id: &str, message: &str) -> anyhow::Result<()> {
        let mut client = crate::server::Client::connect().await?;
        let request_id = client.notify_session(session_id, message).await?;
        Self::wait_for_request_done(&mut client, request_id).await
    }

    async fn resume_dead_session_with_reminder(
        &self,
        provider: &Arc<dyn Provider>,
        item: &ScheduledItem,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let session = Session::load(session_id)?;
        let cycle_provider = provider.fork();
        let registry = tool::Registry::new(cycle_provider.clone()).await;
        if session.is_canary {
            registry.register_selfdev_tools().await;
        }

        let mut agent = Agent::new(cycle_provider, registry);
        agent.set_debug(session.is_debug);
        agent.restore_session(session_id)?;

        let reminder = ambient::format_scheduled_session_message(item);
        let _ = agent.run_once_capture(&reminder).await?;
        agent.mark_closed();
        Ok(())
    }

    async fn spawn_session_for_scheduled_item(
        &self,
        provider: &Arc<dyn Provider>,
        item: &ScheduledItem,
        parent_session_id: &str,
    ) -> anyhow::Result<String> {
        let mut child = match Session::load(parent_session_id) {
            Ok(parent) => {
                let mut child = Session::create(
                    Some(parent_session_id.to_string()),
                    Some(
                        item.task_description
                            .clone()
                            .unwrap_or_else(|| "Scheduled task".to_string()),
                    ),
                );
                child.replace_messages(parent.messages.clone());
                child.compaction = parent.compaction.clone();
                child.provider_key = parent.provider_key.clone();
                child.route_api_method = parent.route_api_method.clone();
                child.model = parent.model.clone();
                child.subagent_model = parent.subagent_model.clone();
                child.improve_mode = parent.improve_mode;
                child.autoreview_enabled = parent.autoreview_enabled;
                child.autojudge_enabled = parent.autojudge_enabled;
                child.is_canary = parent.is_canary;
                child.testing_build = parent.testing_build.clone();
                child.is_debug = parent.is_debug;
                child.memory_injections = parent.memory_injections.clone();
                child.replay_events = parent.replay_events.clone();
                child.working_dir = item.working_dir.clone().or(parent.working_dir.clone());
                child
            }
            Err(err) => {
                logging::warn(&format!(
                    "Ambient runner: failed to load parent session {} for spawned scheduled task {}; creating a fresh child instead: {}",
                    parent_session_id, item.id, err
                ));
                let mut child = Session::create(
                    Some(parent_session_id.to_string()),
                    Some(
                        item.task_description
                            .clone()
                            .unwrap_or_else(|| "Scheduled task".to_string()),
                    ),
                );
                child.working_dir = item.working_dir.clone();
                child
            }
        };
        child.status = crate::session::SessionStatus::Closed;
        child.save()?;

        let child_session_id = child.id.clone();
        let child_is_canary = child.is_canary;
        let child_is_debug = child.is_debug;
        let cycle_provider = provider.fork();
        let registry = tool::Registry::new(cycle_provider.clone()).await;
        if child_is_canary {
            registry.register_selfdev_tools().await;
        }

        let mut agent = Agent::new_with_session(cycle_provider, registry, child, None);
        agent.set_debug(child_is_debug);
        if item.working_dir.is_some() {
            agent.set_working_dir_for_pending_context(item.working_dir.clone());
        }

        let reminder = ambient::format_scheduled_session_message(item);
        let _ = agent.run_once_capture(&reminder).await?;
        agent.mark_closed();
        Ok(child_session_id)
    }

    async fn deliver_scheduled_direct_item(
        &self,
        provider: &Arc<dyn Provider>,
        item: &ScheduledItem,
    ) -> anyhow::Result<()> {
        match &item.target {
            ScheduleTarget::Ambient => Ok(()),
            ScheduleTarget::Session { session_id } => {
                let reminder = ambient::format_scheduled_session_message(item);
                match self.notify_live_session(session_id, &reminder).await {
                    Ok(()) => {
                        logging::info(&format!(
                            "Ambient runner: delivered scheduled task {} to live session {}",
                            item.id, session_id
                        ));
                        Ok(())
                    }
                    Err(err) => {
                        logging::info(&format!(
                            "Ambient runner: live delivery for {} fell back to headless resume: {}",
                            session_id, err
                        ));
                        self.resume_dead_session_with_reminder(provider, item, session_id)
                            .await
                    }
                }
            }
            ScheduleTarget::Spawn { parent_session_id } => {
                let spawned_session_id = self
                    .spawn_session_for_scheduled_item(provider, item, parent_session_id)
                    .await?;
                logging::info(&format!(
                    "Ambient runner: spawned scheduled task {} into child session {} from {}",
                    item.id, spawned_session_id, parent_session_id
                ));
                Ok(())
            }
        }
    }

    async fn deliver_ready_direct_items(
        &self,
        provider: &Arc<dyn Provider>,
        items: Vec<ScheduledItem>,
    ) {
        for item in items {
            if let Err(e) = self.deliver_scheduled_direct_item(provider, &item).await {
                logging::error(&format!(
                    "Ambient runner: failed to deliver scheduled direct item {}: {}",
                    item.id, e
                ));
            }
        }
    }

    /// Start the background ambient loop. Call from a tokio::spawn.
    pub async fn run_loop(self, provider: Arc<dyn Provider>) {
        {
            let mut running = self.inner.running.write().await;
            *running = true;
        }
        logging::info("Ambient runner: starting background loop");

        // Recover after a process that died mid-cycle. Both the wedged status
        // and any claimed-but-unfinished queue items are only *ours* to reclaim
        // when no other live daemon holds the lock: otherwise that daemon is
        // mid-cycle right now, and "recovering" would clear its status and
        // duplicate the items it is actively working on back onto the queue.
        // NOTE: both decisions below are unit-tested via
        // `startup_recovery_defers_to_a_live_foreign_lock_holder`, but THIS
        // line is not: proving it reads the real lock rather than a constant
        // needs a genuine second daemon holding it. Keep the call trivial so
        // inspection is enough.
        let lock_is_foreign = crate::ambient::is_locked_by_another_process();
        if lock_is_foreign {
            logging::info(
                "Ambient runner: another process holds the ambient lock; \
                 leaving its in-flight cycle alone",
            );
        }

        {
            let mut s = self.inner.state.write().await;
            if let Some(reclaimed) = reclaimed_startup_status(&s.status, lock_is_foreign) {
                logging::warn(
                    "Ambient runner: found an interrupted cycle from a previous process; \
                     resetting status to idle",
                );
                s.status = reclaimed;
                let _ = s.save();
            }
        }

        // That dead cycle may also have been holding claimed queue items.
        // No in-process undo could have run, so recover them here or the
        // work is gone for good.
        let recovered = recover_abandoned_queue_items(lock_is_foreign);
        if recovered > 0 {
            logging::warn(&format!(
                "Ambient runner: recovered {} queue item(s) abandoned by an interrupted cycle",
                recovered
            ));
        }

        let ambient_enabled = config().ambient.enabled;

        // Spawn reply pollers only when ambient mode is enabled; scheduled
        // session-targeted scheduled tasks should still work without the ambient-only reply
        // infrastructure.
        if ambient_enabled {
            let safety_config = config().safety.clone();
            if safety_config.email_reply_enabled
                && safety_config.email_imap_host.is_some()
                && safety_config.email_enabled
            {
                let imap_config = safety_config.clone();
                tokio::spawn(async move {
                    crate::notifications::imap_reply_loop(imap_config).await;
                });
                logging::info("Ambient runner: IMAP reply poller spawned");
            }

            // Spawn reply pollers for all configured message channels
            // (Telegram, Discord, etc.)
            let channel_registry = crate::channel::ChannelRegistry::from_config(&safety_config);
            channel_registry.spawn_reply_loops(&self);
        }

        let amb_config = &config().ambient;
        let scheduler_config = AmbientSchedulerConfig {
            min_interval_minutes: amb_config.min_interval_minutes,
            max_interval_minutes: amb_config.max_interval_minutes,
            pause_on_active_session: amb_config.pause_on_active_session,
            ..Default::default()
        };
        let mut scheduler = AdaptiveScheduler::new(scheduler_config);

        // Initialize safety system for ambient tools
        ambient_tools::init_safety_system(Arc::clone(&self.inner.safety));

        loop {
            // Check state
            let state = { self.inner.state.read().await.clone() };

            let ambient_allowed =
                ambient_enabled && !matches!(state.status, AmbientStatus::Disabled);

            // Wall-clock windows: the user's "not at night, not on weekends".
            //
            // This gates ambient *cycles* only. Direct deliveries into a
            // session the user is sitting in are excluded on purpose: the user
            // is present by definition, so refusing to hand them their own
            // scheduled item would be the constraint working against them.
            let window_state = current_window_state();
            // A pending manual trigger opens the window for this pass. Read it
            // without consuming: it is only spent once a cycle actually starts,
            // so a trigger arriving while a previous cycle still holds the lock
            // is not silently swallowed.
            let manual_override = *self.inner.manual_override.read().await;
            let window_open = window_state.is_open() || manual_override;
            if ambient_allowed && !window_open {
                logging::info(&format!(
                    "Ambient runner: outside active windows ({}), holding",
                    schedule_window::describe(&configured_windows())
                ));
            }

            if ambient_allowed {
                // Update scheduler's user-active state
                let active_sessions = *self.inner.active_user_sessions.read().await;
                scheduler.set_user_active(active_sessions > 0);

                // Check if we should pause
                if scheduler.should_pause() {
                    let mut s = self.inner.state.write().await;
                    s.status = AmbientStatus::Paused {
                        reason: "user session active".to_string(),
                    };
                    drop(s);

                    // Sleep until nudged or 60s
                    tokio::select! {
                        _ = self.inner.wake_notify.notified() => {},
                        _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {},
                    }
                    continue;
                }

                // Drop stale permission requests whose originating session is no longer active.
                match self
                    .inner
                    .safety
                    .expire_dead_session_requests("ambient_runner_gc")
                {
                    Ok(expired) if !expired.is_empty() => {
                        logging::info(&format!(
                            "Ambient runner: expired {} stale permission request(s)",
                            expired.len()
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        logging::warn(&format!(
                            "Ambient runner: failed to expire stale permission requests: {}",
                            e
                        ));
                    }
                }
            }

            // Load manager to check should_run and update queue info
            let (should_run, ready_direct_items, next_direct_due, next_ambient_due) =
                match AmbientManager::new() {
                    Ok(mut mgr) => {
                        let ready_direct_items = mgr.take_ready_direct_items();
                        let next_direct_due = mgr
                            .queue()
                            .items()
                            .iter()
                            .filter(|item| item.target.is_direct_delivery())
                            .map(|item| item.scheduled_for)
                            .min();
                        let next_ambient_due = mgr.next_ambient_item_due();
                        // Update queue info for widget
                        {
                            let mut qc = self.inner.queue_count.write().await;
                            *qc = mgr.queue().len();
                        }
                        {
                            let mut qp = self.inner.next_queue_preview.write().await;
                            *qp = mgr.queue().peek_next().map(|i| i.context.clone());
                        }
                        // Also run if there are pending email reply directives
                        (
                            may_start_cycle(
                                ambient_allowed,
                                window_open,
                                mgr.should_run() || ambient::has_pending_directives(),
                            ),
                            ready_direct_items,
                            next_direct_due,
                            next_ambient_due,
                        )
                    }
                    Err(e) => {
                        logging::error(&format!("Ambient runner: failed to load manager: {}", e));
                        (false, Vec::new(), None, None)
                    }
                };

            if !ready_direct_items.is_empty() {
                self.deliver_ready_direct_items(&provider, ready_direct_items)
                    .await;
            }

            if !should_run {
                // Never sleep past a deadline we already know about. The
                // adaptive interval describes routine maintenance; a queued
                // item has an explicit time the user or a previous cycle asked
                // for, and sleeping through it silently delays the work by up
                // to a full interval.
                //
                // Only *future* deadlines shorten the sleep. A deadline in the
                // past cannot be met by waking sooner, and we are here because
                // something else (a cycle already running, ambient paused)
                // blocks the run. Re-arming a ~1s timer for it would spin the
                // loop until that condition clears.
                let sleep_secs = if ambient_allowed && !window_open {
                    // Closed window: sleep until it opens rather than polling.
                    // An ambient deadline falling inside the closed period must
                    // NOT shorten this; it cannot be serviced before the window
                    // opens, and honouring it would spin the loop. The item is
                    // not lost, it simply runs when the window opens.
                    //
                    // A direct-delivery deadline still counts, since those are
                    // handed to a session the user is actively in and are not
                    // subject to the window.
                    closed_window_sleep_secs(
                        Utc::now(),
                        Local::now(),
                        window_state.next_open_at(),
                        next_direct_due,
                    )
                } else if ambient_allowed {
                    let interval = scheduler
                        .calculate_interval(None)
                        .as_secs()
                        .max(MAX_IDLE_POLL_SECS);
                    idle_sleep_secs(
                        Utc::now(),
                        interval,
                        earliest_deadline(next_direct_due, next_ambient_due),
                    )
                } else {
                    // With ambient disabled only direct deliveries still need
                    // servicing, so an ambient-only deadline must not shorten
                    // the idle poll.
                    idle_sleep_secs(Utc::now(), MAX_IDLE_POLL_SECS, next_direct_due)
                };

                logging::info(&format!(
                    "Ambient runner: not time to run, sleeping {}s",
                    sleep_secs
                ));

                tokio::select! {
                    _ = self.inner.wake_notify.notified() => {
                        logging::info("Ambient runner: nudged awake");
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)) => {},
                }
                continue;
            }

            // Try to acquire lock
            let lock = match AmbientLock::try_acquire() {
                Ok(Some(lock)) => lock,
                Ok(None) => {
                    logging::info("Ambient runner: another instance holds the lock, waiting");
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    continue;
                }
                Err(e) => {
                    logging::error(&format!("Ambient runner: lock error: {}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    continue;
                }
            };

            // The manual override is spent here, not at read time: a cycle is
            // genuinely starting, so the one-shot bypass has done its job.
            // Clearing it earlier would drop a trigger that arrived while the
            // lock was held; clearing it never would leave windows disabled
            // for good after a single 2am trigger.
            if manual_override {
                let mut over = self.inner.manual_override.write().await;
                *over = false;
            }

            // Run a cycle
            logging::info("Ambient runner: starting ambient cycle");
            self.set_running_detail("starting cycle").await;

            let cycle_result = self.run_cycle(&provider).await;

            // Clear the soft interrupt queue — cycle is done
            {
                let mut aq = self.inner.active_cycle_queue.write().await;
                *aq = None;
            }

            match cycle_result {
                Ok(result) => {
                    logging::info(&format!(
                        "Ambient cycle complete: {} memories modified, {} compactions",
                        result.memories_modified, result.compactions
                    ));

                    // Update state
                    if let Ok(mut mgr) = AmbientManager::new() {
                        let _ = mgr.record_cycle_result(result.clone());
                        // The cycle finished, so its claim is settled. Drop the
                        // crash-recovery record or the next startup would treat
                        // completed work as abandoned and run it again.
                        mgr.clear_inflight();
                    }
                    let mut s = self.inner.state.write().await;
                    s.record_cycle(&result);
                    let _ = s.save();

                    scheduler.on_successful_cycle();

                    // Save transcript
                    let transcript = crate::safety::AmbientTranscript {
                        session_id: format!("ambient_{}", Utc::now().format("%Y%m%d_%H%M%S")),
                        started_at: result.started_at,
                        ended_at: Some(result.ended_at),
                        status: match result.status {
                            CycleStatus::Complete => crate::safety::TranscriptStatus::Complete,
                            CycleStatus::Interrupted => {
                                crate::safety::TranscriptStatus::Interrupted
                            }
                            CycleStatus::Incomplete => crate::safety::TranscriptStatus::Incomplete,
                        },
                        provider: provider.name().to_string(),
                        model: provider.model(),
                        actions: Vec::new(),
                        pending_permissions: self.inner.safety.pending_requests().len(),
                        summary: Some(result.summary.clone()),
                        compactions: result.compactions,
                        memories_modified: result.memories_modified,
                        conversation: result.conversation.clone(),
                    };
                    let _ = self.inner.safety.save_transcript(&transcript);

                    // Send notifications (fire-and-forget), but only when the
                    // cycle earned one. Garden-only cycles are the vast
                    // majority; pushing them to a phone trains the user to
                    // ignore the channel, which costs the alerts that matter.
                    let outcome = cycle_significance::CycleOutcome {
                        significance: cycle_significance::CycleSignificance::parse(
                            result.significance.as_deref(),
                        ),
                        pending_permissions: transcript.pending_permissions,
                        did_proactive_work: result
                            .proactive_work
                            .as_deref()
                            .is_some_and(|w| !w.trim().is_empty()),
                        failed: !matches!(result.status, CycleStatus::Complete),
                    };
                    if cycle_significance::should_notify(&outcome) {
                        self.inner.notifier.dispatch_cycle_summary(&transcript);
                    } else {
                        // Name WHICH arm silenced it. "routine" (the agent
                        // said so) and "unspecified" (it never set the field)
                        // are operationally identical but mean very different
                        // things: the second says the prompt is not landing,
                        // which is invisible if both log the same line.
                        logging::info(&format!(
                            "Ambient runner: {} cycle, no notification sent",
                            match outcome.significance {
                                cycle_significance::CycleSignificance::Routine => "routine",
                                cycle_significance::CycleSignificance::Notable => "notable",
                                cycle_significance::CycleSignificance::Unspecified =>
                                    "unspecified-significance",
                            }
                        ));
                    }

                    // Post-cycle memory consolidation (fire-and-forget)
                    tokio::spawn(async move {
                        let manager = MemoryManager::new();
                        match manager.backfill_embeddings() {
                            Ok((backfilled, _failed)) => {
                                if backfilled > 0 {
                                    logging::info(&format!(
                                        "Ambient: backfilled {} embeddings",
                                        backfilled
                                    ));
                                }
                            }
                            Err(e) => {
                                logging::error(&format!(
                                    "Ambient: embedding backfill failed: {}",
                                    e
                                ));
                            }
                        }
                    });
                }
                Err(e) => {
                    logging::error(&format!("Ambient cycle failed: {}", e));
                    scheduler.on_rate_limit_hit();

                    let mut s = self.inner.state.write().await;
                    s.status = AmbientStatus::Idle;
                    let _ = s.save();
                }
            }

            // Release lock
            let _ = lock.release();

            // Calculate next sleep interval.
            //
            // The same rule as the idle path applies here: the adaptive
            // interval describes routine maintenance, so it must not sleep past
            // a deadline the user or a previous cycle explicitly asked for. A
            // cycle that finishes just before a queued item is due would
            // otherwise leave it waiting a full interval, and this is the more
            // likely path of the two, since scheduling an item is exactly the
            // kind of thing a cycle does on its way out.
            let interval = scheduler.calculate_interval(None);
            let next_due = match AmbientManager::new() {
                Ok(mgr) => mgr.next_item_due(),
                Err(e) => {
                    logging::error(&format!(
                        "Ambient runner: failed to load manager for next wake: {}",
                        e
                    ));
                    None
                }
            };
            let sleep_secs = idle_sleep_secs(Utc::now(), interval.as_secs().max(30), next_due);

            // Update state with scheduled wake.
            //
            // The cycle may have asked for its own next wake via
            // `schedule_ambient`, which `record_cycle` already wrote as a
            // `Scheduled` status. That request is a *preference*, not an
            // override: the interval computed above is what the resource
            // budget allows, and AMBIENT_MODE.md's stated contract is that an
            // agent asking to wake later than necessary gets pulled forward.
            // Taking the sooner of the two also keeps the runner's own sleep
            // and the persisted `next_wake` in agreement, which they were not
            // when the agent's request won: `should_run` gates on the
            // persisted value, so a cycle that requested a distant wake
            // silently reimposed that distance on the whole loop.
            {
                let computed = Utc::now() + chrono::Duration::seconds(sleep_secs as i64);
                let mut s = self.inner.state.write().await;
                if let Some(next_wake) = reconciled_next_wake(&s.status, computed) {
                    s.status = AmbientStatus::Scheduled { next_wake };
                    let _ = s.save();
                }
            }

            logging::info(&format!("Ambient runner: next cycle in {}s", sleep_secs));

            tokio::select! {
                _ = self.inner.wake_notify.notified() => {
                    logging::info("Ambient runner: nudged awake after cycle");
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)) => {},
            }
        }
    }

    /// Update the running status detail and persist to disk for waybar.
    async fn set_running_detail(&self, detail: &str) {
        let mut s = self.inner.state.write().await;
        s.status = AmbientStatus::Running {
            detail: detail.to_string(),
        };
        let _ = s.save();
    }

    /// Build the ambient system prompt and initial message for a cycle.
    ///
    /// Also returns the queue items this cycle has claimed, so the caller can
    /// restore them if the cycle never gets to run them.
    async fn build_cycle_context(
        &self,
        provider: &Arc<dyn Provider>,
    ) -> anyhow::Result<(String, String, Vec<ScheduledItem>)> {
        let state = self.inner.state.read().await.clone();

        let mut mgr = AmbientManager::new()?;
        // Claim the due items as they are handed to the cycle. The prompt below
        // is the delivery mechanism, so anything left in the queue afterwards
        // would be re-delivered to every subsequent cycle. Items not yet due
        // stay queued and still appear, so the agent can see what is coming.
        let claimed = mgr.take_ready_ambient_items();
        let queue_items: Vec<_> = claimed
            .iter()
            .cloned()
            .chain(mgr.queue().items().iter().cloned())
            .collect();

        let memory_manager = MemoryManager::new();
        let graph_health = ambient::gather_memory_graph_health(&memory_manager);
        let recent_sessions = ambient::gather_recent_sessions(state.last_run);
        let feedback_memories = ambient::gather_feedback_memories(&memory_manager);

        let budget = ambient::ResourceBudget {
            provider: provider.name().to_string(),
            tokens_remaining_desc: "unknown (adaptive)".to_string(),
            window_resets_desc: "unknown".to_string(),
            user_usage_rate_desc: "estimated from history".to_string(),
            cycle_budget_desc: "stay under 50k tokens".to_string(),
        };

        let active_sessions = *self.inner.active_user_sessions.read().await;

        let system_prompt = ambient::build_ambient_system_prompt(
            &state,
            &queue_items,
            &graph_health,
            &recent_sessions,
            &feedback_memories,
            &budget,
            active_sessions,
        );

        let initial_message = "Begin your ambient cycle. Check the scheduled queue, assess memory graph health, and plan your work using the `todo` tool.".to_string();

        Ok((system_prompt, initial_message, claimed))
    }

    /// Run a single ambient cycle. Returns the cycle result.
    async fn run_cycle(&self, provider: &Arc<dyn Provider>) -> anyhow::Result<AmbientCycleResult> {
        let started_at = Utc::now();
        let visible = config().ambient.visible;

        self.set_running_detail("gathering context").await;
        let (system_prompt, initial_message, claimed) =
            self.build_cycle_context(provider).await?;

        // A claimed item is only safely consumed once the cycle has actually
        // had a chance to act on it. If the cycle cannot run at all, put the
        // items back: losing scheduled work outright is worse than the replay
        // this claiming was introduced to stop.
        let restore_claimed = |reason: &str| {
            if claimed.is_empty() {
                return;
            }
            match AmbientManager::new() {
                Ok(mut mgr) => {
                    let count = claimed.len();
                    mgr.requeue_items(claimed.clone());
                    logging::warn(&format!(
                        "Ambient cycle: returned {} claimed queue item(s) after {}",
                        count, reason
                    ));
                }
                Err(e) => logging::error(&format!(
                    "Ambient cycle: could not return {} claimed queue item(s) after {}: {}",
                    claimed.len(),
                    reason,
                    e
                )),
            }
        };

        // Visible mode: spawn a full TUI instead of running headlessly
        if visible {
            let outcome = self
                .run_cycle_visible(started_at, system_prompt, initial_message)
                .await;
            if outcome.is_err() {
                restore_claimed("the visible cycle failed to start");
            }
            return outcome;
        }

        // Headless mode: run agent directly
        self.set_running_detail("setting up tools").await;

        let cycle_provider = provider.fork();
        let registry = tool::Registry::new(cycle_provider.clone()).await;
        registry.register_ambient_tools().await;

        let mut agent = Agent::new(cycle_provider.clone(), registry);
        agent.set_debug(true);
        agent.set_system_prompt(&system_prompt);
        let ambient_session_id = agent.session_id().to_string();
        ambient_tools::register_ambient_session(ambient_session_id.clone());

        // Clear any previous cycle result
        ambient_tools::take_cycle_result();

        // Expose the agent's soft interrupt queue so Telegram replies can be injected mid-cycle
        {
            let mut aq = self.inner.active_cycle_queue.write().await;
            *aq = Some(agent.soft_interrupt_queue());
        }

        self.set_running_detail("running agent").await;

        let run_result = agent.run_once_capture(&initial_message).await;

        // Check if end_ambient_cycle was called
        if let Some(result) = ambient_tools::take_cycle_result() {
            ambient_tools::unregister_ambient_session(&ambient_session_id);
            let conversation = agent.export_conversation_markdown();
            agent.mark_closed();
            return Ok(AmbientCycleResult {
                started_at,
                ended_at: Utc::now(),
                conversation: Some(conversation),
                ..result
            });
        }

        // Agent didn't call end_ambient_cycle - try continuation
        if run_result.is_err() {
            logging::warn("Ambient cycle: agent error without calling end_ambient_cycle");
        }

        self.set_running_detail("continuation turn").await;
        logging::info("Ambient cycle: sending continuation message (no end_ambient_cycle called)");
        let continuation = "You stopped unexpectedly without calling end_ambient_cycle. \
            If you are done with your work, call end_ambient_cycle with a summary of \
            what you accomplished and schedule your next wake. \
            If you are not done, continue what you were doing.";

        let continuation_result = agent.run_once_capture(continuation).await;

        // Check again
        if let Some(result) = ambient_tools::take_cycle_result() {
            ambient_tools::unregister_ambient_session(&ambient_session_id);
            let conversation = agent.export_conversation_markdown();
            agent.mark_closed();
            return Ok(AmbientCycleResult {
                started_at,
                ended_at: Utc::now(),
                conversation: Some(conversation),
                ..result
            });
        }

        // Forced end
        ambient_tools::unregister_ambient_session(&ambient_session_id);
        logging::warn("Ambient cycle: forced end after 2 attempts without end_ambient_cycle");
        // Both turns errored, so the agent never got to act on its instructions
        // (provider outage, auth failure, immediate crash). Treat the claim as
        // unconsumed rather than dropping the scheduled work on the floor. If at
        // least one turn ran, the agent did see the items and we let the claim
        // stand, since re-delivering work it may have half-finished is worse.
        if run_result.is_err() && continuation_result.is_err() {
            restore_claimed("both cycle turns failed before the agent could act");
        }
        let forced = AmbientCycleResult {
            summary: "Cycle ended without calling end_ambient_cycle (forced end after 2 attempts)"
                .to_string(),
            memories_modified: 0,
            compactions: 0,
            proactive_work: None,
            // Left unset deliberately. This is an Incomplete cycle, and the
            // notification decision treats a failure as notable on structure
            // alone, so it must not be labelled routine here.
            significance: None,
            next_schedule: None,
            started_at,
            ended_at: Utc::now(),
            status: CycleStatus::Incomplete,
            conversation: Some(agent.export_conversation_markdown()),
        };
        agent.mark_closed();
        Ok(forced)
    }

    /// Run a visible ambient cycle by spawning a full TUI in a kitty window.
    async fn run_cycle_visible(
        &self,
        started_at: chrono::DateTime<Utc>,
        system_prompt: String,
        initial_message: String,
    ) -> anyhow::Result<AmbientCycleResult> {
        use crate::ambient::VisibleCycleContext;

        self.set_running_detail("launching visible TUI").await;

        // Save context for the spawned process
        let context = VisibleCycleContext {
            system_prompt,
            initial_message,
        };
        context.save()?;

        // Clear any previous result file
        if let Ok(result_path) = VisibleCycleContext::result_path() {
            let _ = std::fs::remove_file(&result_path);
        }

        // Find the jcode binary
        let jcode_bin =
            std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("jcode"));

        // Spawn kitty with `jcode ambient run-visible`
        logging::info("Ambient visible: spawning kitty with jcode TUI");
        let child = std::process::Command::new("kitty")
            .args([
                "--title",
                "🤖 jcode ambient cycle",
                "-e",
                &jcode_bin.to_string_lossy(),
                "ambient",
                "run-visible",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        match child {
            Ok(mut child) => {
                self.set_running_detail("waiting for TUI cycle").await;

                // Wait for the kitty process to exit (user closes window or cycle completes)
                let status = tokio::task::spawn_blocking(move || child.wait()).await?;
                match status {
                    Ok(s) => logging::info(&format!("Ambient visible: TUI exited with {}", s)),
                    Err(e) => logging::warn(&format!("Ambient visible: wait error: {}", e)),
                }

                // Try to read the cycle result from the file
                if let Ok(result_path) = VisibleCycleContext::result_path()
                    && result_path.exists()
                    && let Ok(result) =
                        crate::storage::read_json::<AmbientCycleResult>(&result_path)
                {
                    let _ = std::fs::remove_file(&result_path);
                    return Ok(AmbientCycleResult {
                        started_at,
                        ended_at: Utc::now(),
                        ..result
                    });
                }

                // No result file — user closed the window without end_ambient_cycle
                Ok(AmbientCycleResult {
                    summary: "Visible cycle ended (user closed window)".to_string(),
                    memories_modified: 0,
                    compactions: 0,
                    proactive_work: None,
                    significance: None,
                    next_schedule: None,
                    started_at,
                    ended_at: Utc::now(),
                    status: CycleStatus::Incomplete,
                    conversation: None,
                })
            }
            Err(e) => {
                logging::warn(&format!(
                    "Ambient visible: failed to spawn kitty ({}), falling back to headless",
                    e
                ));
                // Fall back to headless mode
                Err(anyhow::anyhow!("Failed to spawn visible TUI: {}", e))
            }
        }
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "runner_tests.rs"]
mod runner_tests;
