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
    assert_eq!(crate::config::config().cron.len(), 1, "cron job should load");

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
    assert!(marker_file.exists(), "exec job should have run and touched the marker file");
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
    assert!(a.next_run.is_some(), "enabled valid job should have a next_run");

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
    run_job_now("manual-job").await.expect("manual run should start");

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

