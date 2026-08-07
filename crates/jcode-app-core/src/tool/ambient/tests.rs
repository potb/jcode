use super::*;

#[test]
fn test_parse_priority() {
    assert_eq!(parse_priority(Some("low")), Priority::Low);
    assert_eq!(parse_priority(Some("normal")), Priority::Normal);
    assert_eq!(parse_priority(Some("high")), Priority::High);
    assert_eq!(parse_priority(None), Priority::Normal);
    assert_eq!(parse_priority(Some("unknown")), Priority::Normal);
}

#[test]
fn test_cycle_result_store_and_take() {
    let result = AmbientCycleResult {
        summary: "test".to_string(),
        memories_modified: 1,
        compactions: 0,
        proactive_work: None,
        significance: None,
        next_schedule: None,
        started_at: Utc::now(),
        ended_at: Utc::now(),
        status: CycleStatus::Complete,
        conversation: None,
    };

    store_cycle_result(result);
    let taken = take_cycle_result();
    assert!(taken.is_some());
    assert_eq!(taken.unwrap().summary, "test");

    // Second take should be None
    assert!(take_cycle_result().is_none());
}

#[test]
fn test_end_cycle_input_deserialization() {
    let input = json!({
        "summary": "Merged 3 duplicates",
        "memories_modified": 5,
        "compactions": 1,
        "proactive_work": "Fixed typo in README",
        "next_schedule": {
            "wake_in_minutes": 20,
            "context": "Verify stale facts",
            "priority": "high"
        }
    });

    let parsed: EndCycleInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.summary, "Merged 3 duplicates");
    assert_eq!(parsed.memories_modified, 5);
    assert_eq!(parsed.compactions, 1);
    assert_eq!(
        parsed.proactive_work.as_deref(),
        Some("Fixed typo in README")
    );
    let ns = parsed.next_schedule.unwrap();
    assert_eq!(ns.wake_in_minutes, Some(20));
    assert_eq!(ns.context.as_deref(), Some("Verify stale facts"));
    assert_eq!(ns.priority.as_deref(), Some("high"));
}

#[test]
fn test_end_cycle_input_minimal() {
    let input = json!({
        "summary": "Nothing to do",
        "memories_modified": 0,
        "compactions": 0
    });

    let parsed: EndCycleInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.summary, "Nothing to do");
    assert!(parsed.proactive_work.is_none());
    assert!(parsed.next_schedule.is_none());
}

/// Regression for #106: Claude's tool calling emits numeric arguments as JSON
/// *strings* (e.g. `{"compactions": "0"}`). Before the fix, this failed with
/// `invalid type: string "0", expected u32`, breaking every ambient cycle.
#[test]
fn test_end_cycle_input_accepts_string_numbers() {
    let input = json!({
        "summary": "Stringified counts",
        "memories_modified": "5",
        "compactions": "0",
        "next_schedule": {
            "wake_in_minutes": "20",
            "context": "later",
            "priority": "high"
        }
    });

    let parsed: EndCycleInput = serde_json::from_value(input)
        .expect("string-encoded numbers must deserialize (regression #106)");
    assert_eq!(parsed.memories_modified, 5);
    assert_eq!(parsed.compactions, 0);
    assert_eq!(parsed.next_schedule.unwrap().wake_in_minutes, Some(20));
}

/// The `schedule_ambient` and `schedule` tools and the permission `wait` flag
/// must survive the same stringified-argument quirk.
#[test]
fn test_ambient_inputs_accept_string_numbers_and_bools() {
    let sched: ScheduleInput = serde_json::from_value(json!({
        "wake_in_minutes": "15",
        "context": "ctx"
    }))
    .expect("schedule_ambient must accept string wake_in_minutes (#106)");
    assert_eq!(sched.wake_in_minutes, Some(15));

    let perm: RequestPermissionInput = serde_json::from_value(json!({
        "action": "delete",
        "description": "remove file",
        "rationale": "cleanup",
        "wait": "true"
    }))
    .expect("request_permission must accept string wait flag (#106)");
    assert!(perm.wait);

    let tool: ScheduleToolInput = serde_json::from_value(json!({
        "task": "do thing",
        "wake_in_minutes": "30"
    }))
    .expect("schedule tool must accept string wake_in_minutes (#106)");
    assert_eq!(tool.wake_in_minutes, Some(30));
}

#[test]
fn test_schedule_input_deserialization() {
    let input = json!({
        "wake_in_minutes": 15,
        "context": "Check CI results",
        "priority": "normal"
    });

    let parsed: ScheduleInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.wake_in_minutes, Some(15));
    assert!(parsed.wake_at.is_none());
    assert_eq!(parsed.context, "Check CI results");
    assert_eq!(parsed.priority.as_deref(), Some("normal"));
}

#[test]
fn test_permission_input_deserialization() {
    let input = json!({
        "action": "create_pull_request",
        "description": "Create PR for test fixes",
        "rationale": "Found failing tests that need attention",
        "urgency": "high",
        "wait": true
    });

    let parsed: RequestPermissionInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.action, "create_pull_request");
    assert_eq!(parsed.description, "Create PR for test fixes");
    assert_eq!(parsed.rationale, "Found failing tests that need attention");
    assert_eq!(parsed.urgency.as_deref(), Some("high"));
    assert!(parsed.wait);
}

#[test]
fn test_permission_input_defaults() {
    let input = json!({
        "action": "edit",
        "description": "Fix typo",
        "rationale": "Obvious error"
    });

    let parsed: RequestPermissionInput = serde_json::from_value(input).unwrap();
    assert!(parsed.urgency.is_none());
    assert!(!parsed.wait);
}

#[test]
fn test_build_permission_review_context_defaults() {
    let review =
        build_permission_review_context("edit", "Fix typo in docs", "Needs write permission", None);

    assert_eq!(
        review
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "Fix typo in docs"
    );
    assert_eq!(
        review
            .get("why_permission_needed")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "Needs write permission"
    );
    assert_eq!(
        review
            .get("requested_action")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "edit"
    );
}

#[test]
fn test_build_permission_review_context_uses_structured_fields() {
    let context = json!({
        "summary": "Preparing a focused refactor",
        "why_permission_needed": "Need to modify tracked files",
        "planned_steps": ["Update parser", "Run tests"],
        "files": ["src/parser.rs", "src/tests.rs"],
        "commands": ["cargo test"],
        "risks": ["Could regress parsing edge cases"],
        "rollback_plan": "Revert commit if tests fail",
        "expected_outcome": "Parser handles edge-case input",
    });
    let review =
        build_permission_review_context("edit", "fallback summary", "fallback why", Some(&context));

    assert_eq!(
        review
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "Preparing a focused refactor"
    );
    assert_eq!(
        review
            .get("why_permission_needed")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "Need to modify tracked files"
    );
    assert_eq!(
        review
            .get("rollback_plan")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "Revert commit if tests fail"
    );
    assert_eq!(
        review
            .get("planned_steps")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or_default(),
        2
    );
}

#[test]
fn test_register_unregister_ambient_session() {
    let session_id = "ambient_tool_test_session";
    unregister_ambient_session(session_id);
    assert!(!is_ambient_session_registered(session_id));

    register_ambient_session(session_id.to_string());
    assert!(is_ambient_session_registered(session_id));

    unregister_ambient_session(session_id);
    assert!(!is_ambient_session_registered(session_id));
}

#[tokio::test]
async fn test_request_permission_rejects_non_ambient_session() {
    let tool = RequestPermissionTool::new();
    let input = json!({
        "action": "edit",
        "description": "Update docs",
        "rationale": "Fix typo"
    });
    let ctx = ToolContext {
        session_id: "normal_session_test".to_string(),
        message_id: "msg_1".to_string(),
        tool_call_id: "call_1".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };

    let err = tool
        .execute(input, ctx)
        .await
        .expect_err("non-ambient session should be rejected");
    assert!(
        err.to_string()
            .contains("request_permission is only available to ambient sessions")
    );
}

#[test]
fn test_schedule_tool_input_deserialization() {
    let input = json!({
        "task": "Run the full test suite and report results",
        "wake_in_minutes": 120,
        "priority": "high",
        "relevant_files": ["src/main.rs", "tests/e2e/main.rs"],
        "background_context": "We just merged PR #42 which changed the parser",
        "success_criteria": "All tests pass, or a summary of failures is stored"
    });

    let parsed: ScheduleToolInput = serde_json::from_value(input).unwrap();
    assert_eq!(
        parsed.task.as_deref(),
        Some("Run the full test suite and report results")
    );
    assert!(parsed.action.is_none());
    assert!(parsed.schedule_id.is_none());
    assert_eq!(parsed.wake_in_minutes, Some(120));
    assert!(parsed.wake_at.is_none());
    assert_eq!(parsed.priority.as_deref(), Some("high"));
    assert_eq!(parsed.relevant_files.len(), 2);
    assert_eq!(
        parsed.background_context.as_deref(),
        Some("We just merged PR #42 which changed the parser")
    );
    assert_eq!(
        parsed.success_criteria.as_deref(),
        Some("All tests pass, or a summary of failures is stored")
    );
}

#[test]
fn test_schedule_tool_input_resume_target() {
    let input = json!({
        "task": "Follow up in this chat",
        "wake_in_minutes": 10,
        "target": "resume"
    });

    let parsed: ScheduleToolInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.target.as_deref(), Some("resume"));
}

#[test]
fn test_schedule_tool_input_spawn_target() {
    let input = json!({
        "task": "Follow up in a new child session",
        "wake_in_minutes": 10,
        "target": "spawn"
    });

    let parsed: ScheduleToolInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.target.as_deref(), Some("spawn"));
}

#[test]
fn test_schedule_tool_input_minimal() {
    let input = json!({
        "task": "Check CI",
        "wake_in_minutes": 30
    });

    let parsed: ScheduleToolInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.task.as_deref(), Some("Check CI"));
    assert_eq!(parsed.wake_in_minutes, Some(30));
    assert!(parsed.relevant_files.is_empty());
    assert!(parsed.background_context.is_none());
    assert!(parsed.success_criteria.is_none());
}

#[test]
fn test_schedule_tool_input_cancel_action() {
    let input = json!({
        "action": "cancel",
        "schedule_id": "sched_abc123"
    });

    let parsed: ScheduleToolInput = serde_json::from_value(input).unwrap();
    assert_eq!(parsed.action.as_deref(), Some("cancel"));
    assert_eq!(parsed.schedule_id.as_deref(), Some("sched_abc123"));
    assert!(parsed.task.is_none());
}

#[test]
fn test_parse_schedule_target_defaults_to_resume_originating_session() {
    assert_eq!(
        parse_schedule_target(None, "session_123").unwrap(),
        ScheduleTarget::Session {
            session_id: "session_123".to_string()
        }
    );
    assert_eq!(
        parse_schedule_target(Some("resume"), "session_123").unwrap(),
        ScheduleTarget::Session {
            session_id: "session_123".to_string()
        }
    );
}

#[test]
fn test_parse_schedule_target_supports_spawn_and_ambient() {
    assert_eq!(
        parse_schedule_target(Some("spawn"), "session_123").unwrap(),
        ScheduleTarget::Spawn {
            parent_session_id: "session_123".to_string()
        }
    );
    assert_eq!(
        parse_schedule_target(Some("ambient"), "session_123").unwrap(),
        ScheduleTarget::Ambient
    );
}

#[test]
fn test_parse_schedule_target_rejects_removed_session_alias() {
    let err = parse_schedule_target(Some("session"), "session_123")
        .expect_err("removed session alias should be rejected");
    assert!(err.to_string().contains("resume, spawn, ambient"));
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "test intentionally serializes process-wide JCODE_HOME/env state across async tool execution"
)]
async fn test_schedule_tool_defaults_to_resuming_originating_session() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let tool = ScheduleTool::new();
    let input = json!({
        "task": "Follow up on this work",
        "wake_in_minutes": 5
    });
    let ctx = ToolContext {
        session_id: "origin_session".to_string(),
        message_id: "msg_1".to_string(),
        tool_call_id: "call_1".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };

    let output = tool
        .execute(input, ctx)
        .await
        .expect("schedule should succeed");
    assert!(
        output
            .output
            .contains("Target: resume session origin_session")
    );

    let manager = AmbientManager::new().expect("ambient manager");
    let scheduled = manager
        .queue()
        .items()
        .first()
        .expect("scheduled item should exist");
    assert_eq!(
        scheduled.target,
        ScheduleTarget::Session {
            session_id: "origin_session".to_string()
        }
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn test_schedule_tool_schema_avoids_top_level_combinators() {
    let tool = ScheduleTool::new();
    let schema = tool.parameters_schema();

    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert!(schema.get("anyOf").is_none());
    assert!(schema.get("oneOf").is_none());
    assert!(schema.get("allOf").is_none());
}

#[tokio::test]
async fn test_schedule_tool_requires_time() {
    let tool = ScheduleTool::new();
    let input = json!({
        "task": "Do something eventually"
    });
    let ctx = ToolContext {
        session_id: "test_session".to_string(),
        message_id: "msg_1".to_string(),
        tool_call_id: "call_1".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };

    let err = tool
        .execute(input, ctx)
        .await
        .expect_err("should require wake_in_minutes or wake_at");
    assert!(err.to_string().contains("wake_in_minutes"));
}

/// With `ambient.auto_approve_permissions` on, the tool must resolve the
/// request itself instead of parking it in the queue. A queued request is the
/// exact failure this setting exists to prevent: with no channel able to carry
/// an answer back, it sits until it is expired and the cycle's work is lost.
#[tokio::test]
async fn test_request_permission_auto_approves_when_configured() {
    let _guard = jcode_base::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_flag = std::env::var_os("JCODE_AMBIENT_AUTO_APPROVE");
    let temp = tempfile::TempDir::new().expect("create temp dir");
    jcode_base::env::set_var("JCODE_HOME", temp.path());
    jcode_base::env::set_var("JCODE_AMBIENT_AUTO_APPROVE", "true");
    jcode_base::config::invalidate_config_cache();

    let session_id = "ambient_auto_approve_test";
    register_ambient_session(session_id);

    let tool = RequestPermissionTool::new();
    let ctx = ToolContext {
        session_id: session_id.to_string(),
        message_id: "msg_1".to_string(),
        tool_call_id: "call_1".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };
    let out = tool
        .execute(
            json!({
                "action": "create_pull_request",
                "description": "Open the knip config PR",
                "rationale": "Pre-authorized by the initiative"
            }),
            ctx,
        )
        .await
        .expect("auto-approve should succeed");

    assert!(
        out.output.contains("Permission approved"),
        "expected an approval, got: {}",
        out.output
    );
    assert!(
        get_safety_system()
            .pending_requests()
            .is_empty(),
        "auto-approved requests must not be left queued for a human"
    );

    // The whole justification for auto-approving is that the audit trail still
    // shows what was done and on whose authority. An in-memory approval that
    // never reaches disk would leave the user with no record at all, so assert
    // the persisted file, not just the tool's return value.
    let history_file = temp.path().join("safety").join("history.json");
    assert!(
        history_file.exists(),
        "an auto-approval must be persisted to {}",
        history_file.display()
    );
    let decisions: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&history_file).expect("read history"))
            .expect("history is valid json");
    let entries = decisions.as_array().expect("history is an array");
    assert_eq!(entries.len(), 1, "exactly one decision: {decisions}");
    let d = &entries[0];
    assert_eq!(
        d["approved"], true,
        "the decision must record an APPROVAL, not merely that it was seen"
    );
    assert_eq!(
        d["decided_via"], "ambient_auto_approve",
        "the trail must attribute this to the auto-approve config, not a human"
    );
    assert!(
        d["request_id"].as_str().is_some_and(|s| !s.is_empty()),
        "a decision with no request id cannot be traced back: {d}"
    );
    assert!(
        d["message"]
            .as_str()
            .is_some_and(|m| m.contains("create_pull_request")),
        "the trail must name the action that was approved: {d}"
    );

    unregister_ambient_session(session_id);
    match prev_flag {
        Some(v) => jcode_base::env::set_var("JCODE_AMBIENT_AUTO_APPROVE", v),
        None => jcode_base::env::remove_var("JCODE_AMBIENT_AUTO_APPROVE"),
    }
    match prev_home {
        Some(v) => jcode_base::env::set_var("JCODE_HOME", v),
        None => jcode_base::env::remove_var("JCODE_HOME"),
    }
    jcode_base::config::invalidate_config_cache();
}
