use super::AmbientRunnerHandle;
use crate::ambient::{Priority, ScheduleTarget, ScheduledItem};
use crate::message::{Message, Role, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::session::Session;
use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            crate::env::set_var(self.key, prev);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

struct TestProvider;

#[derive(Clone, Default)]
struct StreamingTestProvider {
    responses: Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>,
}

impl StreamingTestProvider {
    fn queue_response(&self, events: Vec<StreamEvent>) {
        self.responses.lock().unwrap().push_back(events);
    }
}

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "TestProvider should not be used for streaming completions in ambient runner tests"
        ))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(TestProvider)
    }
}

#[async_trait]
impl Provider for StreamingTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let stream = stream! {
            for event in events {
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn runner_stays_alive_to_service_schedules_when_ambient_disabled() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let task = tokio::spawn(runner.clone().run_loop(provider));

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        runner.is_running().await,
        "runner should remain active for scheduled tasks even with ambient disabled"
    );

    task.abort();
    let _ = task.await;
}

/// The user's actual ask: a `[[cron]]` job must fire from the runner loop
/// even with ambient mode off entirely, since the loop is the only thing
/// keeping `crate::cron::tick` on a live clock (see `server.rs`'s comment
/// on spawning this loop unconditionally).
#[tokio::test]
async fn runner_loop_fires_a_cron_job_with_ambient_disabled() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let marker = temp.path().join("cron-ran.txt");

    std::fs::write(
        temp.path().join("config.toml"),
        format!(
            "[[cron]]\nid = \"loop-test-job\"\nevery = \"1m\"\ncommand = \"touch {}\"\n",
            marker.display()
        ),
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let task = tokio::spawn(runner.clone().run_loop(provider));

    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        marker.exists(),
        "cron job should have fired from the runner loop despite ambient being disabled"
    );

    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn spawn_target_creates_one_child_session_and_runs_task() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let provider = StreamingTestProvider::default();
    provider.queue_response(vec![
        StreamEvent::TextDelta("Spawned session handled task.".to_string()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let provider: Arc<dyn Provider> = Arc::new(provider);

    let mut parent = Session::create_with_id(
        "session_parent_spawn_test".to_string(),
        None,
        Some("Parent".to_string()),
    );
    parent.working_dir = Some(temp.path().display().to_string());
    parent.save().expect("save parent session");

    let item = ScheduledItem {
        id: "sched_spawn_test".to_string(),
        scheduled_for: chrono::Utc::now(),
        context: "Follow up later".to_string(),
        priority: Priority::Normal,
        target: ScheduleTarget::Spawn {
            parent_session_id: parent.id.clone(),
        },
        created_by_session: parent.id.clone(),
        created_at: chrono::Utc::now(),
        working_dir: parent.working_dir.clone(),
        task_description: Some("Follow up later".to_string()),
        relevant_files: vec!["src/lib.rs".to_string()],
        git_branch: None,
        additional_context: Some("Background: spawned schedule test".to_string()),
    };

    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let child_session_id = runner
        .spawn_session_for_scheduled_item(&provider, &item, &parent.id)
        .await
        .expect("spawned scheduled task should succeed");

    assert_ne!(child_session_id, parent.id);

    let child = Session::load(&child_session_id).expect("load spawned child session");
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(child.working_dir, parent.working_dir);
    assert!(child.messages.iter().any(|message| {
        message.role == Role::User
            && message.content_preview().contains("[Scheduled task]")
            && message.content_preview().contains("Follow up later")
    }));
    assert!(child.messages.iter().any(|message| {
        message.role == Role::Assistant
            && message
                .content_preview()
                .contains("Spawned session handled task.")
    }));
}

#[test]
fn earliest_deadline_picks_the_nearer_of_the_two() {
    use super::earliest_deadline;
    let now = chrono::Utc::now();
    let soon = now + chrono::Duration::minutes(5);
    let later = now + chrono::Duration::hours(2);

    assert_eq!(earliest_deadline(Some(soon), Some(later)), Some(soon));
    assert_eq!(earliest_deadline(Some(later), Some(soon)), Some(soon));
    assert_eq!(earliest_deadline(Some(soon), None), Some(soon));
    assert_eq!(earliest_deadline(None, Some(later)), Some(later));
    assert_eq!(earliest_deadline(None, None), None);
}

/// An ambient-targeted queue item that is already due must make the runner
/// want to run, even while `state.status` says it is Scheduled far in the
/// future. Before this, `should_run` looked only at `next_wake`, so an item
/// scheduled 45 minutes out sat unrun until the (up to 2 hour) maintenance
/// interval elapsed.
#[test]
fn due_ambient_queue_item_triggers_a_cycle_despite_a_distant_next_wake() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let _enabled = EnvVarGuard::set_path("JCODE_AMBIENT_ENABLED", std::path::Path::new("true"));
    crate::config::invalidate_config_cache();

    let mut state = crate::ambient::AmbientState::load().expect("load state");
    state.status = crate::ambient::AmbientStatus::Scheduled {
        next_wake: chrono::Utc::now() + chrono::Duration::hours(2),
    };
    state.save().expect("save state");

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    assert!(
        !mgr.should_run(),
        "an empty queue with a distant next_wake must not run"
    );

    mgr.schedule(crate::ambient::ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
        context: "overdue ambient work".to_string(),
        priority: Priority::Normal,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".to_string(),
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    })
    .expect("schedule item");

    assert!(
        mgr.has_due_ambient_item(),
        "the overdue item should be reported as due"
    );
    assert!(
        mgr.should_run(),
        "a due ambient queue item must trigger a cycle on its own"
    );

    crate::config::invalidate_config_cache();
}

/// A future ambient item must still be visible as a deadline (so the sleep can
/// be shortened to meet it) without claiming to be due yet.
#[test]
fn future_ambient_queue_item_is_a_deadline_but_not_yet_due() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    let due_at = chrono::Utc::now() + chrono::Duration::minutes(45);
    mgr.schedule(crate::ambient::ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(due_at),
        context: "later ambient work".to_string(),
        priority: Priority::Normal,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".to_string(),
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    })
    .expect("schedule item");

    assert!(
        !mgr.has_due_ambient_item(),
        "not due for another 45 minutes"
    );
    let next = mgr.next_ambient_item_due().expect("a deadline is known");
    assert_eq!(next, due_at);

    crate::config::invalidate_config_cache();
}

/// The post-cycle sleep is bounded by the next queued item whatever its target.
///
/// The idle path already honoured deadlines, but the sleep taken *after* a
/// cycle used the bare adaptive interval. That is the likelier path to matter:
/// scheduling follow-up work is exactly what a cycle does on its way out, so
/// the item it just queued was the one at risk of waiting a full interval.
/// `next_item_due` must therefore see direct-delivery items too, not only the
/// ambient-targeted ones `next_ambient_item_due` reports.
#[test]
fn the_next_deadline_spans_every_target_kind() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    assert!(
        mgr.next_item_due().is_none(),
        "an empty queue imposes no deadline"
    );

    let ambient_at = chrono::Utc::now() + chrono::Duration::minutes(45);
    let direct_at = chrono::Utc::now() + chrono::Duration::minutes(10);
    let mut add = |when: chrono::DateTime<chrono::Utc>, target: ScheduleTarget| {
        mgr.schedule(crate::ambient::ScheduleRequest {
            wake_in_minutes: None,
            wake_at: Some(when),
            context: "queued work".to_string(),
            priority: Priority::Normal,
            target,
            created_by_session: "test".to_string(),
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        })
        .expect("schedule item");
    };

    add(ambient_at, ScheduleTarget::Ambient);
    add(
        direct_at,
        ScheduleTarget::Session {
            session_id: "session_test".to_string(),
        },
    );

    assert_eq!(
        mgr.next_item_due(),
        Some(direct_at),
        "the soonest item wins regardless of target"
    );
    assert_eq!(
        mgr.next_ambient_item_due(),
        Some(ambient_at),
        "the ambient-only view still excludes direct deliveries"
    );

    // And the sleep actually shortens to it, rather than taking the interval.
    let interval = 15 * 60;
    assert_eq!(
        super::idle_sleep_secs(chrono::Utc::now(), interval, mgr.next_item_due()),
        10 * 60,
        "a 10-minute deadline must not wait out a 15-minute interval"
    );

    crate::config::invalidate_config_cache();
}

/// Due ambient items must leave the queue when a cycle claims them. They are
/// delivered by being written into the cycle's prompt, so if they survive that
/// hand-off every later cycle is told to do the same work again and
/// `overdue_queue_count` never returns to zero however much work is done.
#[test]
fn claiming_ambient_items_removes_them_but_keeps_future_and_direct_ones() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    let mut add = |ctx: &str, when: chrono::DateTime<chrono::Utc>, target: ScheduleTarget| {
        mgr.schedule(crate::ambient::ScheduleRequest {
            wake_in_minutes: None,
            wake_at: Some(when),
            context: ctx.to_string(),
            priority: Priority::Normal,
            target,
            created_by_session: "test".to_string(),
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        })
        .expect("schedule");
    };
    let past = chrono::Utc::now() - chrono::Duration::minutes(10);
    let future = chrono::Utc::now() + chrono::Duration::hours(1);
    add("due ambient", past, ScheduleTarget::Ambient);
    add("future ambient", future, ScheduleTarget::Ambient);
    add(
        "due direct",
        past,
        ScheduleTarget::Session {
            session_id: "session_other".to_string(),
        },
    );

    let claimed = mgr.take_ready_ambient_items();
    assert_eq!(claimed.len(), 1, "only the due ambient item is claimed");
    assert_eq!(claimed[0].context, "due ambient");

    let left: Vec<_> = mgr
        .queue()
        .items()
        .iter()
        .map(|i| i.context.clone())
        .collect();
    assert!(
        !left.contains(&"due ambient".to_string()),
        "the claimed item must not be replayed to the next cycle"
    );
    assert!(
        left.contains(&"future ambient".to_string()),
        "items that are not due yet must stay queued"
    );
    assert!(
        left.contains(&"due direct".to_string()),
        "direct-delivery items are serviced by their own path"
    );

    // The claim must survive a reload: the queue is shared across processes.
    let reloaded = crate::ambient::AmbientManager::new().expect("reload manager");
    assert!(
        !reloaded
            .queue()
            .items()
            .iter()
            .any(|i| i.context == "due ambient"),
        "the claim must be persisted, not just dropped in memory"
    );
    assert!(!reloaded.has_due_ambient_item());

    crate::config::invalidate_config_cache();
}

/// The idle sleep must be shortened by a future deadline but never by a past
/// one. A past deadline means the run is blocked by something else (a cycle in
/// flight, ambient paused); waking sooner cannot help, and a sub-second sleep
/// re-armed every pass turns the loop into a busy-wait. This was observed live
/// as a steady stream of "not time to run, sleeping 1s".
#[test]
fn idle_sleep_is_bounded_by_future_deadlines_and_never_busy_loops() {
    use super::idle_sleep_secs;
    let now = chrono::Utc::now();
    let interval = 7200u64;

    assert_eq!(
        idle_sleep_secs(now, interval, None),
        interval,
        "no deadline means the full maintenance interval"
    );
    assert_eq!(
        idle_sleep_secs(now, interval, Some(now + chrono::Duration::minutes(45))),
        45 * 60,
        "a nearer deadline shortens the sleep"
    );
    assert_eq!(
        idle_sleep_secs(now, interval, Some(now + chrono::Duration::hours(5))),
        interval,
        "a deadline beyond the interval does not extend the sleep"
    );

    for past in [
        chrono::Duration::seconds(1),
        chrono::Duration::hours(4),
        chrono::Duration::days(3),
    ] {
        assert_eq!(
            idle_sleep_secs(now, interval, Some(now - past)),
            interval,
            "an overdue deadline must not shorten the sleep ({past})"
        );
    }
    assert_eq!(
        idle_sleep_secs(now, interval, Some(now)),
        interval,
        "a deadline exactly now must not produce a zero-length sleep"
    );

    // Regression: whole-second truncation collapsed any sub-second deadline to
    // 0, which the "must be in the future" filter then read as overdue, so the
    // loop fell back to the full interval. Observed live as
    // "not time to run, sleeping 7200s" logged 0.87s before an item was due;
    // the item then sat overdue for two hours.
    for ms in [1i64, 100, 500, 871, 999] {
        assert_eq!(
            idle_sleep_secs(
                now,
                interval,
                Some(now + chrono::Duration::milliseconds(ms))
            ),
            1,
            "a deadline {ms}ms away must wake in ~1s, not sleep the full interval"
        );
    }
    assert_eq!(
        idle_sleep_secs(
            now,
            interval,
            Some(now + chrono::Duration::milliseconds(1500))
        ),
        2,
        "partial seconds round up so the wake lands at or after the deadline"
    );
}

/// A cycle that dies mid-flight (crash, daemon restart, `server reload`)
/// leaves `Running` persisted in the state file. Nothing else clears it and
/// `should_run` reads Running as "already busy", so ambient would never run
/// again. Startup must reclaim it. Observed live: a reload during a cycle left
/// status Running with no such session alive, and ambient stopped entirely.
#[tokio::test]
async fn startup_reclaims_a_cycle_interrupted_by_a_previous_process() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let mut state = crate::ambient::AmbientState::load().expect("load state");
    state.status = crate::ambient::AmbientStatus::Running {
        detail: "running agent".to_string(),
    };
    state.save().expect("persist a wedged Running status");

    assert!(
        !crate::ambient::AmbientManager::new()
            .expect("manager")
            .should_run(),
        "a Running status blocks every future cycle while it persists"
    );

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let task = tokio::spawn(runner.clone().run_loop(provider));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let recovered = crate::ambient::AmbientState::load().expect("reload state");
    assert!(
        !matches!(
            recovered.status,
            crate::ambient::AmbientStatus::Running { .. }
        ),
        "startup must clear a stale Running status, got {:?}",
        recovered.status
    );

    task.abort();
    let _ = task.await;
}

/// `jcode server reload` re-execs the daemon in place, so the PID survives. A
/// lock file left behind by the pre-exec image therefore passes the liveness
/// check and the new runner waits forever for itself. Observed live as
/// "another instance holds the lock, waiting" where the recorded PID was the
/// daemon's own.
#[test]
fn lock_left_by_our_own_pre_exec_image_is_not_mistaken_for_a_live_holder() {
    use crate::ambient::AmbientLock;
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let lock_path = temp.path().join("ambient").join("ambient.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).expect("mkdir");
    std::fs::write(&lock_path, std::process::id().to_string()).expect("write stale lock");

    let acquired = AmbientLock::try_acquire().expect("try_acquire");
    assert!(
        acquired.is_some(),
        "a lock naming our own PID is stale by definition and must be reclaimed"
    );

    // A lock held by a genuinely different live process is still respected.
    // PID 1 always exists and is never us.
    drop(acquired);
    std::fs::write(&lock_path, "1").expect("write foreign lock");
    assert!(
        AmbientLock::try_acquire().expect("try_acquire").is_none(),
        "a live foreign holder must still block acquisition"
    );
}

/// Claiming is optimistic, so it needs an undo. If a cycle is handed items and
/// then cannot act on them (provider outage, auth failure, crash before the
/// first turn), dropping them would silently destroy scheduled work: strictly
/// worse than the re-delivery that claiming was introduced to stop. Requeuing
/// must restore them, and must persist so the next process sees them.
#[test]
fn requeued_items_are_restored_and_persisted_so_failed_cycles_lose_nothing() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    mgr.schedule(crate::ambient::ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
        context: "important scheduled work".to_string(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".to_string(),
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    })
    .expect("schedule");

    let claimed = mgr.take_ready_ambient_items();
    assert_eq!(claimed.len(), 1);
    assert!(
        !mgr.has_due_ambient_item(),
        "precondition: the claim emptied the due set"
    );

    // The cycle failed; hand the work back.
    mgr.requeue_items(claimed);

    assert!(
        mgr.has_due_ambient_item(),
        "restored work must be due again so the next wake picks it up"
    );

    let reloaded = crate::ambient::AmbientManager::new().expect("reload");
    let restored: Vec<_> = reloaded
        .queue()
        .items()
        .iter()
        .filter(|i| i.context == "important scheduled work")
        .collect();
    assert_eq!(
        restored.len(),
        1,
        "the restore must be persisted exactly once, not duplicated or lost"
    );
    assert_eq!(
        restored[0].priority,
        Priority::High,
        "priority must survive the round trip"
    );

    crate::config::invalidate_config_cache();
}

/// The in-process undo cannot run if the process itself dies between claiming
/// items and finishing the cycle, which is exactly what happens on a crash or a
/// `server reload` mid-cycle. The claim is therefore also written to disk, and
/// startup restores it. Without this, reloading during a cycle silently deletes
/// whatever that cycle had been handed.
#[test]
fn items_claimed_by_a_process_that_died_are_recovered_at_startup() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    // A cycle claims the work...
    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    mgr.schedule(crate::ambient::ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
        context: "work that must survive a crash".to_string(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".to_string(),
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    })
    .expect("schedule");
    let claimed = mgr.take_ready_ambient_items();
    assert_eq!(claimed.len(), 1);

    // ...and the process dies here: no undo runs, `mgr` is simply dropped.
    drop(mgr);
    let after_death = crate::ambient::AmbientManager::new().expect("reload");
    assert!(
        !after_death
            .queue()
            .items()
            .iter()
            .any(|i| i.context == "work that must survive a crash"),
        "precondition: the claim really did remove it from the queue"
    );

    // The next process recovers it.
    let mut fresh = crate::ambient::AmbientManager::new().expect("fresh manager");
    assert_eq!(fresh.recover_inflight_items(), 1, "the item must come back");
    assert!(
        fresh.has_due_ambient_item(),
        "recovered work must be due so the next cycle picks it up"
    );

    // Recovery is idempotent: a second startup must not duplicate the work.
    let mut third = crate::ambient::AmbientManager::new().expect("third manager");
    assert_eq!(
        third.recover_inflight_items(),
        0,
        "nothing left to recover once the record is cleared"
    );
    let count = third
        .queue()
        .items()
        .iter()
        .filter(|i| i.context == "work that must survive a crash")
        .count();
    assert_eq!(
        count, 1,
        "the item must exist exactly once, never duplicated"
    );

    crate::config::invalidate_config_cache();
}

/// A completed cycle must settle its claim, otherwise the recovery record
/// outlives the work and the next restart re-runs something already done.
#[test]
fn a_settled_claim_is_not_resurrected_by_a_later_restart() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    mgr.schedule(crate::ambient::ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
        context: "work completed normally".to_string(),
        priority: Priority::Normal,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".to_string(),
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    })
    .expect("schedule");
    let claimed = mgr.take_ready_ambient_items();
    assert_eq!(claimed.len(), 1);

    // The cycle ran to completion and settled its claim.
    mgr.clear_inflight();

    let mut restarted = crate::ambient::AmbientManager::new().expect("restart");
    assert_eq!(
        restarted.recover_inflight_items(),
        0,
        "completed work must not be resurrected as abandoned"
    );
    assert!(
        !restarted.has_due_ambient_item(),
        "the queue must stay empty after a normally completed cycle"
    );

    crate::config::invalidate_config_cache();
}

/// Startup recovery must distinguish "the previous process died and left work
/// behind" from "another daemon is running a cycle right now". Recovering in
/// the second case would clear a live cycle's status and duplicate the very
/// items it is working on back onto the queue. The lock is the discriminator,
/// and our own PID must not count, because `server reload` re-execs in place.
#[test]
fn a_live_foreign_lock_holder_suppresses_startup_recovery() {
    use crate::ambient::is_locked_by_another_process;
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let lock_path = temp.path().join("ambient").join("ambient.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).expect("mkdir");

    // No lock at all: nothing to defer to.
    assert!(
        !is_locked_by_another_process(),
        "absent lock must not look like a live holder"
    );

    // PID 1 always exists and is never us: a real concurrent daemon.
    std::fs::write(&lock_path, "1").expect("write foreign lock");
    assert!(
        is_locked_by_another_process(),
        "a live foreign holder must suppress recovery"
    );

    // Our own PID is the reload case: the lock is our pre-exec ghost, so it
    // must NOT suppress recovery or a reload could never recover anything.
    std::fs::write(&lock_path, std::process::id().to_string()).expect("write own lock");
    assert!(
        !is_locked_by_another_process(),
        "our own stale lock must not block us from recovering our own work"
    );

    // A dead PID and a garbage file are both "no live holder".
    std::fs::write(&lock_path, "4294967294").expect("write dead pid");
    assert!(!is_locked_by_another_process(), "a dead holder is not live");
    std::fs::write(&lock_path, "not-a-pid").expect("write junk");
    assert!(
        !is_locked_by_another_process(),
        "an unparseable lock must not wedge recovery forever"
    );
}

/// The lock check above proves the *predicate* is right. This proves the
/// startup path actually CONSULTS it.
///
/// That distinction is not academic: deleting the `is_locked_by_another_process`
/// guard from the runner left the entire ambient suite green, because every
/// test exercised the predicate directly and none covered the decision made
/// from it. A concurrent daemon's `Running` status would have been reset to
/// idle and its claimed items handed to a second cycle, duplicating live work.
#[test]
fn startup_recovery_defers_to_a_live_foreign_lock_holder() {
    use super::{reclaimed_startup_status, recover_abandoned_queue_items};
    use crate::ambient::AmbientStatus;
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let running = AmbientStatus::Running {
        detail: "mid-cycle".to_string(),
    };

    // No foreign holder: the wedged status is ours to clear.
    assert!(
        matches!(
            reclaimed_startup_status(&running, false),
            Some(AmbientStatus::Idle)
        ),
        "an interrupted cycle we own must be reset or ambient stays wedged forever"
    );
    // A live foreign holder: that Running belongs to a cycle happening NOW.
    assert!(
        reclaimed_startup_status(&running, true).is_none(),
        "resetting a live daemon's status would let two cycles run at once"
    );
    // An idle status is never "recovered" either way.
    for foreign in [true, false] {
        assert!(
            reclaimed_startup_status(&AmbientStatus::Idle, foreign).is_none(),
            "idle is not an interrupted cycle (foreign={foreign})"
        );
    }

    // Same gate on the queue items. Seed a real abandoned claim first.
    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    mgr.schedule(crate::ambient::ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
        context: "claimed by the other daemon".to_string(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".to_string(),
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    })
    .expect("schedule");
    assert_eq!(mgr.take_ready_ambient_items().len(), 1);
    drop(mgr);

    assert_eq!(
        recover_abandoned_queue_items(true),
        0,
        "items claimed by a live daemon must not be handed to a second cycle"
    );
    assert!(
        crate::storage::jcode_dir()
            .expect("jcode dir")
            .join("ambient")
            .join("inflight.json")
            .exists(),
        "deferring must LEAVE the record for its real owner, not consume it"
    );
    assert_eq!(
        recover_abandoned_queue_items(false),
        1,
        "with no foreign holder the abandoned item is ours to recover"
    );

    crate::config::invalidate_config_cache();
}

/// The inflight record is written by a process that may be killed at any
/// moment, so a truncated or garbage file is a realistic state to find at
/// startup. Recovery must treat it as "nothing to recover" and clear it, never
/// panic or wedge every future startup on the same unreadable file.
#[test]
fn a_corrupt_inflight_record_is_survivable_and_self_clearing() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let dir = temp.path().join("ambient");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let inflight = dir.join("inflight.json");

    for bad in [
        "",
        "{",
        "[{\"id\":\"trunc",
        "null",
        "{\"not\":\"an array\"}",
    ] {
        std::fs::write(&inflight, bad).expect("write corrupt record");
        let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
        assert_eq!(
            mgr.recover_inflight_items(),
            0,
            "a corrupt record ({bad:?}) must recover nothing rather than panic"
        );
        assert!(
            !inflight.exists(),
            "a corrupt record ({bad:?}) must be cleared, or every startup retries it forever"
        );
    }

    crate::config::invalidate_config_cache();
}

/// A manual trigger must survive the trip to disk.
///
/// `should_run` is evaluated on a freshly loaded `AmbientManager`, so an
/// in-memory-only status change is invisible to it. Observed live: `jcode
/// ambient trigger` printed "Ambient cycle triggered", the loop woke, re-read
/// the old `Scheduled` status from disk, logged "not time to run" and slept
/// again. The command reported success and did nothing.
#[tokio::test]
async fn a_manual_trigger_is_persisted_so_the_loop_can_see_it() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    // `should_run` short-circuits on `is_enabled()`, which the temp JCODE_HOME
    // isolates away, so the assertion below would pass vacuously without this.
    let _enabled = EnvVarGuard::set_path("JCODE_AMBIENT_ENABLED", std::path::Path::new("true"));
    crate::config::invalidate_config_cache();

    let mut state = crate::ambient::AmbientState::load().expect("load state");
    state.status = crate::ambient::AmbientStatus::Scheduled {
        next_wake: chrono::Utc::now() + chrono::Duration::hours(3),
    };
    state.save().expect("persist a distant scheduled wake");

    assert!(
        !crate::ambient::AmbientManager::new()
            .expect("manager")
            .should_run(),
        "a distant scheduled wake must not run on its own"
    );

    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    runner.trigger().await;

    let reloaded = crate::ambient::AmbientState::load().expect("reload state");
    assert!(
        matches!(reloaded.status, crate::ambient::AmbientStatus::Idle),
        "trigger must persist Idle, got {:?}",
        reloaded.status
    );
    assert!(
        crate::ambient::AmbientManager::new()
            .expect("manager")
            .should_run(),
        "a triggered cycle must be runnable from a freshly loaded manager"
    );

    crate::config::invalidate_config_cache();
}

/// A cycle's own `schedule_ambient` request is a preference, not an override.
///
/// `record_cycle` writes the agent's requested wake as a `Scheduled` status
/// before the runner computes its interval. The old guard only matched
/// `Running | Idle`, so a cycle that requested a distant wake kept it and the
/// computed interval was discarded. That mattered because `should_run` gates on
/// the persisted value, not the runner's sleep: observed live, the runner
/// logged "next cycle in 789s" while state.json said the next wake was three
/// hours out, and the three hours won.
#[test]
fn an_agents_requested_wake_never_outlasts_the_budgeted_interval() {
    use super::reconciled_next_wake;
    let now = chrono::Utc::now();
    let computed = now + chrono::Duration::minutes(13);

    let distant_request = crate::ambient::AmbientStatus::Scheduled {
        next_wake: now + chrono::Duration::hours(3),
    };
    assert_eq!(
        reconciled_next_wake(&distant_request, computed),
        Some(computed),
        "a request beyond the budgeted interval is pulled forward"
    );

    // The reverse is honoured: asking to wake sooner is always allowed, since
    // the interval is a budget ceiling and not a minimum spacing.
    let soon = now + chrono::Duration::minutes(2);
    let eager_request = crate::ambient::AmbientStatus::Scheduled { next_wake: soon };
    assert_eq!(
        reconciled_next_wake(&eager_request, computed),
        Some(soon),
        "a nearer request is kept"
    );

    // No request at all: the computed interval stands.
    assert_eq!(
        reconciled_next_wake(&crate::ambient::AmbientStatus::Idle, computed),
        Some(computed)
    );
    assert_eq!(
        reconciled_next_wake(
            &crate::ambient::AmbientStatus::Running {
                detail: "running agent".to_string()
            },
            computed
        ),
        Some(computed)
    );
}

/// Disabled and Paused are not ours to reschedule.
///
/// Overwriting either with a `Scheduled` status would silently re-enable
/// ambient, or clear the reason it was paused, on nothing more than a cycle
/// having finished.
#[test]
fn a_disabled_or_paused_runner_is_not_rescheduled() {
    use super::reconciled_next_wake;
    let computed = chrono::Utc::now() + chrono::Duration::minutes(13);

    assert_eq!(
        reconciled_next_wake(&crate::ambient::AmbientStatus::Disabled, computed),
        None
    );
    assert_eq!(
        reconciled_next_wake(
            &crate::ambient::AmbientStatus::Paused {
                reason: "user session active".to_string()
            },
            computed
        ),
        None
    );
}

// ---------------------------------------------------------------------------
// Wall-clock window gating
// ---------------------------------------------------------------------------

/// The window must actually gate cycle start. Without this the pure window
/// module can be perfectly correct while wired to nothing.
#[test]
fn closed_window_blocks_cycle_start() {
    use super::may_start_cycle;

    assert!(
        may_start_cycle(true, true, true),
        "open window and wanting to run must start a cycle"
    );
    assert!(
        !may_start_cycle(true, false, true),
        "a closed window must block a cycle even when everything else says run"
    );
    assert!(
        !may_start_cycle(false, true, true),
        "ambient disabled still blocks regardless of the window"
    );
    assert!(
        !may_start_cycle(true, true, false),
        "an open window must not force a run that was not wanted"
    );
}

/// A closed window must park the runner near the reopening instead of polling
/// every 30s, but a direct delivery still pulls it awake early.
#[test]
fn closed_window_sleeps_until_open() {
    use super::closed_window_sleep_secs;
    use chrono::{Duration, Local, Utc};

    let now_utc = Utc::now();
    let now_local = Local::now();

    // Reopens in 10 minutes, nothing else pending → sleep ~10 minutes.
    let secs = closed_window_sleep_secs(
        now_utc,
        now_local,
        Some(now_local + Duration::minutes(10)),
        None,
    );
    assert!(
        (590..=600).contains(&secs),
        "expected ~600s until the window opens, got {secs}"
    );

    // A direct delivery inside the closed period still wins: those bypass the
    // window because the user is sitting in that session.
    let secs = closed_window_sleep_secs(
        now_utc,
        now_local,
        Some(now_local + Duration::minutes(10)),
        Some(now_utc + Duration::minutes(2)),
    );
    assert!(
        (110..=120).contains(&secs),
        "a direct delivery must shorten the closed-window sleep, got {secs}"
    );

    // Far-off reopening is capped so config edits and manual triggers are seen.
    let secs = closed_window_sleep_secs(
        now_utc,
        now_local,
        Some(now_local + Duration::days(2)),
        None,
    );
    assert_eq!(
        secs, 3600,
        "a distant reopening must clamp to the hourly re-check"
    );
}

/// An explicit `jcode ambient trigger` must run even at 3am on a Sunday.
///
/// The window exists to stop the agent waking ITSELF outside working hours.
/// A human asking for a cycle right now is not that, and refusing them
/// silently is the "reported success and did nothing" failure the trigger path
/// already had to fix once.
#[tokio::test]
async fn manual_trigger_overrides_closed_window() {
    use crate::safety::SafetySystem;
    use std::sync::Arc;

    let handle = AmbientRunnerHandle::new(Arc::new(SafetySystem::new()));

    assert!(
        !*handle.inner.manual_override.read().await,
        "override must start clear, or windows would never apply"
    );

    handle.trigger().await;

    assert!(
        *handle.inner.manual_override.read().await,
        "an explicit trigger must set the one-shot window bypass"
    );

    // The bypass must actually open a closed window.
    let window_open = false;
    let manual_override = *handle.inner.manual_override.read().await;
    assert!(
        super::may_start_cycle(true, window_open || manual_override, true),
        "a triggered cycle must start despite the window being closed"
    );
}
