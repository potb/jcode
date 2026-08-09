//! Tests for `cron:` debug socket commands.

use super::*;

#[tokio::test]
async fn unknown_prefix_returns_none_so_the_dispatch_chain_falls_through() {
    let result = maybe_handle_cron_command("ambient:status").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn cron_run_requires_a_non_empty_id() {
    let output = maybe_handle_cron_command("cron:run:")
        .await
        .unwrap()
        .unwrap();
    assert!(output.contains("Usage: cron:run"));
}

#[tokio::test]
async fn cron_run_answers_an_unknown_id_instead_of_dropping_the_connection() {
    // Returning `Err` propagates out of the debug-client handler and closes
    // the socket, so the caller gets an empty reply with no explanation. This
    // was observed live: `cron:run:nope` returned literally nothing.
    let _guard = crate::storage::lock_test_env();
    let output = maybe_handle_cron_command("cron:run:definitely-not-a-job")
        .await
        .expect("an unknown id is user error, not a transport failure")
        .expect("the cron namespace must claim its own command");
    assert!(
        output.contains("definitely-not-a-job"),
        "the reply should name the job that could not be found, got: {output}"
    );
}

#[tokio::test]
async fn cron_help_lists_the_documented_commands() {
    let help = maybe_handle_cron_command("cron:help")
        .await
        .unwrap()
        .unwrap();
    assert!(help.contains("cron:list"));
    assert!(help.contains("cron:run:<id>"));
}

#[tokio::test]
async fn cron_list_is_valid_json_and_empty_with_no_configured_jobs() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let output = maybe_handle_cron_command("cron:list")
        .await
        .unwrap()
        .unwrap();

    match prev {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }

    let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(parsed, serde_json::json!([]));
}

#[tokio::test]
async fn cron_list_reports_a_configured_job() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("config.toml"),
        "[[cron]]\nid = \"nightly\"\nat = \"daily 03:00\"\ncommand = \"true\"\n",
    )
    .expect("write config");
    let prev = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    let output = maybe_handle_cron_command("cron:list")
        .await
        .unwrap()
        .unwrap();

    match prev {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }

    let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
    let jobs = parsed.as_array().expect("array");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["id"], "nightly");
    assert_eq!(jobs[0]["schedule"], "at daily 03:00");
    assert_eq!(jobs[0]["enabled"], true);
    assert_eq!(jobs[0]["valid"], true);
    assert!(jobs[0]["next_run"].is_string());
    assert!(jobs[0]["next_run_in"].is_string());
}
