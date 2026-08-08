use super::*;
use super::super::{ToolContext, ToolExecutionMode};

fn ctx(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "t".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::AgentTurn,
    }
}

fn tool() -> CompactTool {
    CompactTool::new(Arc::new(RwLock::new(
        CompactionManager::new().with_budget(200_000),
    )))
}

async fn run(session: &str, action: &str) -> String {
    tool()
        .execute(json!({ "action": action }), ctx(session))
        .await
        .expect("tool should succeed")
        .output
}

#[tokio::test]
async fn status_reports_context_and_queues_nothing() {
    let out = run("sess-status", "status").await;
    assert!(out.contains("Context status:"), "{out}");
    assert!(
        !take_request("sess-status"),
        "status must not queue a compaction request"
    );
}

#[tokio::test]
async fn default_action_is_status() {
    let out = tool()
        .execute(json!({}), ctx("sess-default"))
        .await
        .unwrap()
        .output;
    assert!(out.contains("Context status:"), "{out}");
    assert!(!take_request("sess-default"));
}

#[tokio::test]
async fn now_queues_a_request_for_that_session_only() {
    let out = run("sess-a", "now").await;
    assert!(out.contains("Compaction requested"), "{out}");
    assert!(!take_request("sess-b"), "request must not leak across sessions");
    assert!(take_request("sess-a"));
}

#[tokio::test]
async fn request_is_consumed_exactly_once() {
    run("sess-once", "now").await;
    assert!(take_request("sess-once"));
    assert!(
        !take_request("sess-once"),
        "a consumed request must not fire again"
    );
}

#[tokio::test]
async fn repeated_now_calls_collapse_to_one_request() {
    run("sess-dup", "now").await;
    run("sess-dup", "now").await;
    assert!(take_request("sess-dup"));
    assert!(!take_request("sess-dup"), "requests must not stack up");
}

#[tokio::test]
async fn cancel_withdraws_a_pending_request() {
    run("sess-cancel", "now").await;
    let out = run("sess-cancel", "cancel").await;
    assert!(out.contains("cleared"), "{out}");
    assert!(!take_request("sess-cancel"));
}

#[tokio::test]
async fn cancel_without_pending_request_is_harmless() {
    let out = run("sess-nocancel", "cancel").await;
    assert!(out.contains("Context status:"), "{out}");
    assert!(!take_request("sess-nocancel"));
}

#[tokio::test]
async fn unknown_action_errors_and_queues_nothing() {
    let err = tool()
        .execute(json!({ "action": "explode" }), ctx("sess-bad"))
        .await;
    assert!(err.is_err());
    assert!(!take_request("sess-bad"));
}

#[tokio::test]
async fn action_is_case_insensitive() {
    run("sess-case", "NOW").await;
    assert!(take_request("sess-case"));
}

#[test]
fn schema_exposes_the_three_actions() {
    let schema = tool().parameters_schema();
    let variants = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action should be an enum")
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert_eq!(variants, vec!["status", "now", "cancel"]);
}

#[test]
fn definition_is_compact_enough_for_the_tool_budget() {
    let def = tool().to_definition();
    assert_eq!(def.name, "compact");
    assert!(
        def.description.len() < 600,
        "description should stay small, got {} bytes",
        def.description.len()
    );
}

#[test]
fn status_block_reports_budget_and_usage() {
    let manager = CompactionManager::new().with_budget(100_000);
    let out = format_status(&manager);
    assert!(out.contains("100k budget"), "{out}");
    assert!(out.contains("Compaction in progress: no"), "{out}");
}
