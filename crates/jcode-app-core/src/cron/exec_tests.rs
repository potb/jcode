//! Tests for exec-mode cron job execution.

use super::*;
use std::time::Duration;

#[tokio::test]
async fn successful_command_records_zero_exit_and_logs_output() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let outcome = run_job_command(
        "echo-job",
        "echo hello-from-cron",
        None,
        Duration::from_secs(5),
    )
    .await;

    match prev {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }

    assert!(outcome.spawn_error.is_none());
    assert!(!outcome.timed_out);
    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.succeeded());

    let log = std::fs::read_to_string(logs_dir_for_test(temp.path()).join("echo-job.log"))
        .expect("log should exist");
    assert!(log.contains("hello-from-cron"), "got: {log}");
}

#[tokio::test]
async fn nonzero_exit_is_recorded_and_not_treated_as_success() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let outcome = run_job_command("fail-job", "false", None, Duration::from_secs(5)).await;

    match prev {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }

    assert_eq!(outcome.exit_code, Some(1));
    assert!(!outcome.succeeded());
}

#[tokio::test]
async fn long_running_command_is_killed_at_the_timeout() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let outcome = run_job_command("hang-job", "sleep 30", None, Duration::from_millis(200)).await;

    match prev {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }

    assert!(outcome.timed_out);
    assert!(!outcome.succeeded());
    assert!(
        outcome.duration < Duration::from_secs(5),
        "should not wait out the full sleep"
    );
}

#[tokio::test]
async fn unparseable_command_reports_a_spawn_error_without_panicking() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let outcome = run_job_command("bad-job", "'unterminated", None, Duration::from_secs(5)).await;

    match prev {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }

    assert!(outcome.spawn_error.is_some());
    assert!(!outcome.succeeded());
}

/// Test-only mirror of `logs_dir()` scoped to a known temp home, so the
/// assertion does not depend on the real `JCODE_HOME` resolution timing
/// relative to the env var swap above.
fn logs_dir_for_test(home: &std::path::Path) -> std::path::PathBuf {
    home.join("cron").join("logs")
}
