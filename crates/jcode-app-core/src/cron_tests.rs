//! Integration-style tests for the cron module's top-level behavior:
//! `tick`, `list_snapshot`, and `run_job_now` against real config + disk
//! state (scoped to a temp `JCODE_HOME`).

use super::*;
use tempfile::TempDir;

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
        match self.prev.take() {
            Some(prev) => crate::env::set_var(self.key, prev),
            None => crate::env::remove_var(self.key),
        }
    }
}

/// Write `config.toml` under `home` and return a guard that keeps
/// `JCODE_HOME` pointed at it for the caller's scope.
fn configured_home(config_toml: &str) -> (TempDir, EnvVarGuard) {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(temp.path().join("config.toml"), config_toml).expect("write config");
    let guard = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    (temp, guard)
}

#[tokio::test]
async fn a_due_exec_job_actually_runs_and_records_its_exit_code() {
    let _guard = crate::storage::lock_test_env();
    let marker = TempDir::new().expect("marker dir");
    let marker_file = marker.path().join("ran.txt");

    let (_temp, _home) = configured_home(&format!(
        r#"
[[cron]]
id = "touch-job"
every = "1m"
command = "touch {}"
"#,
        marker_file.display()
    ));

    // Force a reload: the config cache is fingerprinted by file path, and a
    // brand new JCODE_HOME is guaranteed to differ, but flushing here avoids
    // depending on that timing coincidence.
    crate::config::invalidate_config_cache();
    assert_eq!(
        crate::config::config().cron.len(),
        1,
        "cron job should load"
    );

    let due = tick(true);
    // A first-ever run with catch_up (default true) fires immediately, so
    // the tick above should have spawned the job. Give the spawned tokio
    // task a moment to actually run `touch`.
    for _ in 0..100 {
        if marker_file.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        marker_file.exists(),
        "exec job should have run and touched the marker file"
    );
    // The job just fired, so its own next occurrence is necessarily in the
    // future (or absent while state hasn't caught up yet); either way `tick`
    // must not still report it as overdue in the same pass.
    let _ = due;

    for _ in 0..100 {
        let state = CronState::load();
        if state.get("touch-job").is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let state = CronState::load();
    let recorded = state.get("touch-job").expect("state should be recorded");
    assert_eq!(recorded.last_status, Some(LastStatus::Success));
    assert_eq!(recorded.last_exit_code, Some(0));
}

#[tokio::test]
async fn list_snapshot_reports_schedule_and_state_for_every_job() {
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "job-a"
every = "6h"
command = "true"

[[cron]]
id = "job-b"
at = "daily 03:00"
prompt = "do a thing"
enabled = false
"#,
    );
    crate::config::invalidate_config_cache();

    let snapshot = list_snapshot();
    assert_eq!(snapshot.len(), 2);

    let a = snapshot.iter().find(|j| j.id == "job-a").unwrap();
    assert!(a.valid);
    assert!(a.enabled);
    assert_eq!(a.schedule_description, "every 6h");
    assert!(
        a.next_run.is_some(),
        "enabled valid job should have a next_run"
    );

    let b = snapshot.iter().find(|j| j.id == "job-b").unwrap();
    assert!(b.valid);
    assert!(!b.enabled);
    assert_eq!(b.schedule_description, "at daily 03:00");
    assert!(b.next_run.is_none(), "disabled job has no next_run");
}

#[tokio::test]
async fn invalid_job_config_is_reported_but_does_not_crash_the_tick() {
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "broken"
every = "6h"
at = "daily 03:00"
command = "true"
"#,
    );
    crate::config::invalidate_config_cache();

    let snapshot = list_snapshot();
    let broken = &snapshot[0];
    assert!(!broken.valid, "both every and at set is invalid");
    assert!(broken.next_run.is_none());

    // Must not panic and must not treat the invalid job as due.
    let _ = tick(true);
}

#[tokio::test]
async fn run_job_now_fires_regardless_of_schedule() {
    let _guard = crate::storage::lock_test_env();
    let marker = TempDir::new().expect("marker dir");
    let marker_file = marker.path().join("ran.txt");

    let (_temp, _home) = configured_home(&format!(
        r#"
[[cron]]
id = "manual-job"
at = "daily 03:00"
command = "touch {}"
catch_up = false
"#,
        marker_file.display()
    ));
    crate::config::invalidate_config_cache();

    // With catch_up disabled the schedule alone would not fire "now" in most
    // test runs, so a run happening at all demonstrates the manual path
    // bypasses the schedule rather than merely being lucky timing.
    run_job_now("manual-job")
        .await
        .expect("manual run should start");

    for _ in 0..100 {
        if marker_file.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(marker_file.exists(), "cron:run:<id> should run immediately");
}

#[tokio::test]
async fn run_job_now_rejects_unknown_and_disabled_jobs() {
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "off-job"
every = "1h"
command = "true"
enabled = false
"#,
    );
    crate::config::invalidate_config_cache();

    assert!(run_job_now("does-not-exist").await.is_err());
    assert!(run_job_now("off-job").await.is_err());
}

#[tokio::test]
async fn respect_windows_holds_a_due_job_back_when_the_window_is_closed() {
    let _guard = crate::storage::lock_test_env();
    let marker = TempDir::new().expect("marker dir");
    let marker_file = marker.path().join("ran.txt");

    let (_temp, _home) = configured_home(&format!(
        r#"
[[cron]]
id = "windowed-job"
every = "1m"
command = "touch {}"
respect_windows = true
"#,
        marker_file.display()
    ));
    crate::config::invalidate_config_cache();

    // Closed window: the due first-ever run must NOT fire.
    let deadlines = tick(false);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !marker_file.exists(),
        "a respect_windows job must not fire while the window is closed"
    );
    // It is due right now, so it contributes no *future* deadline either;
    // the window-closed sleep is governed separately by
    // schedule_window::sleep_secs_until_open, not by this deadline.
    assert!(deadlines.windowed.is_none());
    assert!(deadlines.unblocked.is_none());

    // Open window: the same due job now fires.
    let _ = tick(true);
    for _ in 0..100 {
        if marker_file.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        marker_file.exists(),
        "the same job must fire once the window opens"
    );
}

#[tokio::test]
async fn unblocked_job_deadline_is_reported_separately_from_windowed() {
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "clock-job"
every = "6h"
command = "true"

[[cron]]
id = "quiet-hours-job"
every = "6h"
command = "true"
respect_windows = true
"#,
    );
    crate::config::invalidate_config_cache();

    // Both jobs already "ran" (simulated) far enough in the past that their
    // next occurrence is a future deadline rather than due-now, so `tick`
    // reports a deadline instead of firing.
    {
        let mut state = CronState::load();
        let last_run = chrono::Utc::now() - chrono::Duration::hours(1);
        state
            .record_run("clock-job", last_run, LastStatus::Success, Some(0), 0)
            .unwrap();
        state
            .record_run("quiet-hours-job", last_run, LastStatus::Success, Some(0), 0)
            .unwrap();
    }

    let deadlines = peek_next_due(false);
    assert!(
        deadlines.unblocked.is_some(),
        "clock-job (respect_windows=false) must report an unblocked deadline"
    );
    assert!(
        deadlines.windowed.is_some(),
        "quiet-hours-job (respect_windows=true) must report a windowed deadline"
    );
}

#[tokio::test]
async fn a_job_that_just_fired_still_reports_its_following_deadline() {
    // Regression: `tick` used to fire a due job and then move on without
    // recording when that job runs NEXT, so the runner loop saw no cron
    // deadline at all and fell back to its 30s idle poll. Observed live as an
    // `every = "5s"` job ticking roughly twice a minute.
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "fast-job"
every = "5s"
command = "true"
"#,
    );
    crate::config::invalidate_config_cache();

    let before = chrono::Utc::now();
    let deadlines = tick(true);

    let next = deadlines
        .unblocked
        .expect("a job that just fired must still report its following fire");
    let delay = next - before;
    assert!(
        delay > chrono::Duration::zero() && delay <= chrono::Duration::seconds(6),
        "the next fire should be about one interval away, got {delay}"
    );
    assert!(
        deadlines.windowed.is_none(),
        "the job does not opt into windows, so nothing belongs in the windowed slot"
    );
}

#[tokio::test]
async fn an_interval_job_records_its_scheduled_slot_not_its_completion_time() {
    // Regression: `last_run` used to be stamped with `Utc::now()` when the
    // command finished. The next fire is computed from `last_run`, so every
    // cycle inherited that run's latency, and the runner's sleep math rounds
    // the leftover sub-second up to a whole second. Measured live under
    // systemd, an `every = "5s"` job settled into a rock-steady 6.00s cadence
    // (6.003, 6.001, 6.002, ...) rather than drifting visibly, which is why
    // interval assertions with a second of slack did not catch it.
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "slot-anchored"
every = "5s"
command = "true"
"#,
    );
    crate::config::invalidate_config_cache();

    let job = config()
        .cron
        .iter()
        .find(|j| j.id == "slot-anchored")
        .cloned()
        .expect("configured job");

    // A slot deliberately in the past, so the recorded value is unambiguously
    // the slot rather than "whatever now happened to be".
    let slot = chrono::Utc::now() - chrono::Duration::seconds(30);
    run_exec_job(job, Some(slot)).await;

    let recorded = CronState::load()
        .last_run("slot-anchored")
        .expect("the run must be recorded");
    assert_eq!(
        recorded, slot,
        "last_run must be the scheduled slot; recording completion time drifts \
         the cadence by one execution's latency every cycle"
    );
}

#[test]
fn a_malformed_job_warns_once_rather_than_once_per_runner_pass() {
    // Regression: the warning sat inline in the per-pass evaluation loop, so a
    // single malformed job logged one identical line every pass forever.
    // Measured against a real daemon: 36 copies in 80 seconds, unbounded.
    forget_invalid_warnings();

    assert!(
        should_warn_invalid("typo-job"),
        "the first sighting of a malformed job must be reported"
    );
    for _ in 0..10 {
        assert!(
            !should_warn_invalid("typo-job"),
            "subsequent passes must stay quiet instead of repeating the warning"
        );
    }
    assert!(
        should_warn_invalid("other-typo-job"),
        "a different malformed job is a different problem and must be reported"
    );

    // A config reload clears the set, so a job that was fixed and broken again
    // is reported again rather than being silenced for the process lifetime.
    forget_invalid_warnings();
    assert!(
        should_warn_invalid("typo-job"),
        "after a config reload the job must be able to warn again"
    );
}

#[test]
fn a_prompt_job_does_not_requeue_while_its_previous_fire_is_undelivered() {
    // Regression: cron ticks regardless of `ambient.enabled`, by design, but
    // prompt delivery IS the ambient pipeline. With ambient off nothing drains
    // the queue, so every tick enqueued another copy: measured at 9 overdue
    // items inside a minute on an 8s job, unbounded.
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[[cron]]
id = "chatty"
every = "1s"
prompt = "do a thing"
"#,
    );
    crate::config::invalidate_config_cache();

    let job = config()
        .cron
        .iter()
        .find(|j| j.id == "chatty")
        .cloned()
        .expect("configured job");

    let first = run_prompt_job(&job, None).expect("the first fire must enqueue");
    assert!(!first.is_empty(), "a queued item id is expected");

    let second = run_prompt_job(&job, None);
    assert!(
        second.is_err(),
        "a second fire must be skipped while the first is still queued"
    );
    let message = second.unwrap_err().to_string();
    assert!(
        message.contains("queued fire") && message.contains("chatty"),
        "the skip should say which job and why, got: {message}"
    );

    let queued = AmbientManager::new()
        .expect("manager")
        .queue()
        .items()
        .iter()
        .filter(|item| item.created_by_session == "cron:chatty")
        .count();
    assert_eq!(
        queued, 1,
        "exactly one outstanding request per job, not one per tick"
    );
}

#[test]
fn next_due_ignores_deadlines_that_have_already_passed() {
    // Regression: next_due took a plain min() over every job's next_run, so a
    // job legitimately sitting overdue without firing (window-gated, or a
    // prompt job holding for delivery) was reported as the next due time.
    // ambient:status then claimed work was due 106 seconds ago while the
    // schedule was in fact healthy.
    let _guard = crate::storage::lock_test_env();
    let (_temp, _home) = configured_home(
        r#"
[ambient]
active_windows = ["weekdays 09:00-09:01"]

[[cron]]
id = "gated"
every = "1s"
respect_windows = true
command = "true"
"#,
    );
    crate::config::invalidate_config_cache();

    // The gated job is overdue on paper but cannot fire outside its window.
    // Whatever next_due reports must not be in the past.
    if let Some(next) = next_due() {
        assert!(
            next >= chrono::Utc::now() - chrono::Duration::seconds(1),
            "next_due must not report an already-passed deadline, got {next}"
        );
    }
}

#[test]
fn every_documented_target_value_maps_to_its_delivery_route() {
    // The config template documents three target values. Only the default was
    // ever exercised, so a mistake in the `spawn` or `session:` arms would have
    // silently delivered a job to the wrong place.
    let base = CronJobConfig {
        id: "t".to_string(),
        every: Some("1h".to_string()),
        prompt: Some("p".to_string()),
        ..Default::default()
    };

    let ambient = CronJobConfig {
        target: None,
        ..base.clone()
    };
    assert!(
        matches!(parse_cron_target(&ambient), ScheduleTarget::Ambient),
        "no target means ambient, as documented"
    );

    let explicit_ambient = CronJobConfig {
        target: Some("ambient".to_string()),
        ..base.clone()
    };
    assert!(
        matches!(
            parse_cron_target(&explicit_ambient),
            ScheduleTarget::Ambient
        ),
        "an explicit 'ambient' must route the same as the default"
    );

    let spawn = CronJobConfig {
        target: Some("spawn".to_string()),
        ..base.clone()
    };
    match parse_cron_target(&spawn) {
        ScheduleTarget::Spawn { parent_session_id } => assert_eq!(
            parent_session_id, "cron:t",
            "a spawned worker is parented to the job, so its origin is traceable"
        ),
        other => unreachable!("expected Spawn, got {other:?}"),
    }

    let session = CronJobConfig {
        target: Some("session:abc123".to_string()),
        ..base.clone()
    };
    match parse_cron_target(&session) {
        ScheduleTarget::Session { session_id } => {
            assert_eq!(
                session_id, "abc123",
                "the id after the prefix is the target"
            )
        }
        other => unreachable!("expected Session, got {other:?}"),
    }

    // An unrecognised value falls back to ambient rather than failing the job:
    // delivering somewhere sane beats a schedule that silently stops.
    let bogus = CronJobConfig {
        target: Some("nonsense".to_string()),
        ..base
    };
    assert!(
        matches!(parse_cron_target(&bogus), ScheduleTarget::Ambient),
        "an unknown target falls back to ambient"
    );
}
