//! Tests for cron state persistence (`~/.jcode/cron/state.json`).

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

#[test]
fn round_trips_through_disk_keyed_by_job_id() {
    let _guard = crate::storage::lock_test_env();
    let temp = TempDir::new().expect("temp dir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let mut state = CronState::load();
    assert!(state.get("upstream-merge").is_none(), "fresh state is empty");

    let ended_at = chrono::Utc::now();
    state
        .record_run("upstream-merge", ended_at, LastStatus::Success, Some(0), 1200)
        .expect("record run");

    let reloaded = CronState::load();
    let entry = reloaded
        .get("upstream-merge")
        .expect("job should be persisted");
    assert_eq!(entry.last_run, Some(ended_at));
    assert_eq!(entry.last_status, Some(LastStatus::Success));
    assert_eq!(entry.last_exit_code, Some(0));
    assert_eq!(entry.last_duration_ms, Some(1200));
    assert_eq!(entry.consecutive_failures, 0);
}

#[test]
fn consecutive_failures_accumulate_and_reset_on_success() {
    let _guard = crate::storage::lock_test_env();
    let temp = TempDir::new().expect("temp dir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let mut state = CronState::load();
    let now = chrono::Utc::now();
    state
        .record_run("job", now, LastStatus::Failure, Some(1), 10)
        .unwrap();
    state
        .record_run("job", now, LastStatus::Failure, Some(1), 10)
        .unwrap();
    assert_eq!(state.get("job").unwrap().consecutive_failures, 2);

    state
        .record_run("job", now, LastStatus::Success, Some(0), 10)
        .unwrap();
    assert_eq!(state.get("job").unwrap().consecutive_failures, 0);
}

#[test]
fn timeout_status_counts_as_a_failure_for_the_streak() {
    let _guard = crate::storage::lock_test_env();
    let temp = TempDir::new().expect("temp dir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let mut state = CronState::load();
    state
        .record_run("job", chrono::Utc::now(), LastStatus::Timeout, None, 60_000)
        .unwrap();
    assert_eq!(state.get("job").unwrap().consecutive_failures, 1);
    assert_eq!(state.get("job").unwrap().last_exit_code, None);
}

#[test]
fn other_jobs_are_untouched_by_a_record_for_a_different_id() {
    let _guard = crate::storage::lock_test_env();
    let temp = TempDir::new().expect("temp dir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let mut state = CronState::load();
    state
        .record_run("job-a", chrono::Utc::now(), LastStatus::Success, Some(0), 5)
        .unwrap();
    state
        .record_run("job-b", chrono::Utc::now(), LastStatus::Failure, Some(1), 5)
        .unwrap();

    let reloaded = CronState::load();
    assert_eq!(
        reloaded.get("job-a").unwrap().last_status,
        Some(LastStatus::Success)
    );
    assert_eq!(
        reloaded.get("job-b").unwrap().last_status,
        Some(LastStatus::Failure)
    );
}

#[test]
fn missing_state_file_loads_as_empty_rather_than_erroring() {
    let _guard = crate::storage::lock_test_env();
    let temp = TempDir::new().expect("temp dir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let state = CronState::load();
    assert!(state.get("anything").is_none());
    assert!(state.last_run("anything").is_none());
}
