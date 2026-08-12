use super::*;
use crate::todo::{
    Autonomy, ConfidenceState, DeliveryState, FeedbackLoopCoverage, FeedbackLoopRelevance,
    FeedbackLoopTraceability, IterationMaturity, TodoGoal, TodoItem,
};

fn todo(id: &str, status: &str) -> TodoItem {
    TodoItem {
        content: format!("task {id}"),
        status: status.to_string(),
        priority: "high".to_string(),
        id: id.to_string(),
        group: Some("g".to_string()),
        confidence: Some(ConfidenceState::Validated),
        completion_confidence: Some(ConfidenceState::Verified),
        confidence_history: vec![ConfidenceState::Validated, ConfidenceState::Verified],
        ..Default::default()
    }
}

/// A goal that passes the ownership check on every field it reads, so a test
/// about a later gate is not silently answered by this one.
fn passing_goal() -> TodoGoal {
    TodoGoal {
        group: Some("g".to_string()),
        delivery_state: Some(DeliveryState::OutcomeDelivered),
        autonomy: Some(Autonomy::NecessaryFollowthrough),
        iteration_maturity: Some(IterationMaturity::OutcomeReached),
        feedback_loop_relevance: Some(FeedbackLoopRelevance::AcceptanceAligned),
        feedback_loop_coverage: Some(FeedbackLoopCoverage::EdgeAndIntegrationPaths),
        feedback_loop_traceability: Some(FeedbackLoopTraceability::Complete),
        ..Default::default()
    }
}

#[test]
fn open_work_outranks_every_other_gate() {
    // Even with a digest ready and no goal assessment at all, the only sane
    // thing to ask for while work is open is the work.
    let todos = vec![todo("a", "completed"), todo("b", "in_progress")];
    match next_todo_gate_follow_up(
        &todos,
        &[],
        Some("digest".to_string()),
        TodoGateState::default(),
    ) {
        Some(TodoGateFollowUp::Incomplete { count, .. }) => assert_eq!(count, 1),
        other => panic!("expected incomplete, got {other:?}"),
    }
}

#[test]
fn digest_is_withheld_while_work_remains() {
    // Building the digest clears the observation log, so it must not be
    // consumed on a turn whose follow-up is going to be Incomplete anyway.
    let open = vec![todo("a", "pending")];
    assert!(!digest_is_due(&open, TodoGateState::default()));
    let done = vec![todo("a", "completed")];
    assert!(digest_is_due(&done, TodoGateState::default()));
    assert!(!digest_is_due(
        &done,
        TodoGateState {
            gate_digest_delivered: true,
            ..Default::default()
        }
    ));
}

#[test]
fn digest_precedes_ownership_and_confidence() {
    // The digest can prompt work that changes the assessments checked after
    // it, so taking it first is the whole point of the ordering. Both later
    // gates would fire here: no goal, and no completion confidence.
    let todos = vec![TodoItem {
        completion_confidence: None,
        ..todo("a", "completed")
    }];
    match next_todo_gate_follow_up(
        &todos,
        &[],
        Some("digest".to_string()),
        TodoGateState::default(),
    ) {
        Some(TodoGateFollowUp::GateDigest { message }) => assert_eq!(message, "digest"),
        other => panic!("expected digest, got {other:?}"),
    }
}

#[test]
fn no_todo_list_at_all_gets_no_gates() {
    // Read-only turns and quiet ambient cycles legitimately keep no list;
    // manufacturing a follow-up would cost an API call every quiet turn.
    assert!(next_todo_gate_follow_up(&[], &[], None, TodoGateState::default()).is_none());
}

#[test]
fn ownership_is_checked_before_completion_confidence() {
    // Confidence passes on this todo, so an ownership result here pins the
    // order rather than just proving "something fired".
    let todos = vec![TodoItem {
        completion_confidence: Some(ConfidenceState::Speculative),
        ..todo("a", "completed")
    }];
    match next_todo_gate_follow_up(&todos, &[], None, TodoGateState::default()) {
        Some(TodoGateFollowUp::Ownership { .. }) => {}
        other => panic!("expected ownership, got {other:?}"),
    }
}

#[test]
fn completion_confidence_gate_fires_for_unset_and_weak_states() {
    for state in [None, Some(ConfidenceState::Speculative)] {
        let todos = vec![TodoItem {
            completion_confidence: state,
            ..todo("a", "completed")
        }];
        match next_todo_gate_follow_up(&todos, &[passing_goal()], None, TodoGateState::default()) {
            Some(TodoGateFollowUp::CompletionConfidence { .. }) => {}
            other => panic!("expected completion confidence for {state:?}, got {other:?}"),
        }
    }
}

#[test]
fn confidence_spike_is_challenged_exactly_once() {
    let todos = vec![TodoItem {
        confidence: Some(ConfidenceState::Speculative),
        completion_confidence: Some(ConfidenceState::Verified),
        confidence_history: vec![ConfidenceState::Speculative, ConfidenceState::Verified],
        ..todo("a", "completed")
    }];
    let goals = vec![passing_goal()];
    match next_todo_gate_follow_up(&todos, &goals, None, TodoGateState::default()) {
        Some(TodoGateFollowUp::ConfidenceSpike { .. }) => {}
        other => panic!("expected spike, got {other:?}"),
    }
    // Already challenged: asking twice in one cycle is nagging, not gating.
    assert!(
        next_todo_gate_follow_up(
            &todos,
            &goals,
            None,
            TodoGateState {
                confidence_spike_challenged: true,
                ..Default::default()
            }
        )
        .is_none()
    );
}

#[test]
fn a_fully_validated_plan_ends_silently() {
    let todos = vec![todo("a", "completed"), todo("b", "completed")];
    assert!(
        next_todo_gate_follow_up(&todos, &[passing_goal()], None, TodoGateState::default())
            .is_none()
    );
}

#[test]
fn an_all_cancelled_plan_has_nothing_to_validate() {
    // Nothing was claimed complete, so there is no completion to over-claim.
    let todos = vec![todo("a", "cancelled")];
    assert!(!work_remains(&todos));
    assert!(
        next_todo_gate_follow_up(&todos, &[passing_goal()], None, TodoGateState::default())
            .is_none()
    );
}

#[test]
fn labels_are_distinct_so_a_stall_names_the_gate() {
    let all = [
        TodoGateFollowUp::Incomplete {
            count: 1,
            message: "m".into(),
        },
        TodoGateFollowUp::GateDigest {
            message: "m".into(),
        },
        TodoGateFollowUp::Ownership {
            message: "m".into(),
        },
        TodoGateFollowUp::CompletionConfidence {
            message: "m".into(),
        },
        TodoGateFollowUp::ConfidenceSpike {
            message: "m".into(),
        },
    ];
    let mut labels: Vec<&str> = all.iter().map(|f| f.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), all.len());
    for follow_up in all {
        assert_eq!(follow_up.message(), "m");
        assert_eq!(follow_up.into_message(), "m");
    }
}

#[test]
fn ship_actions_are_recognised_by_whole_word() {
    for action in [
        "push",
        "create_pull_request",
        "open PR",
        "git-push",
        "merge_branch",
        "deploy",
        "publish release",
    ] {
        assert!(is_ship_action(action), "expected ship action: {action}");
    }
    // Substrings must not catch unrelated actions: "pr" lives inside plenty of
    // ordinary words, and blocking an edit or a cache prune would stall the
    // very work the ownership gate is asking for.
    for action in [
        "edit",
        "prune_cache",
        "run_command",
        "write_file",
        "approve",
        "compress",
    ] {
        assert!(!is_ship_action(action), "not a ship action: {action}");
    }
}

#[test]
fn shipping_is_blocked_while_a_completed_goal_is_unowned() {
    // The cycle-end gates run too late for this: the PR is already open by the
    // time they would raise the same complaint.
    let todos = vec![todo("a", "completed")];
    let weak = TodoGoal {
        group: Some("g".to_string()),
        ..Default::default()
    };
    let reason = ship_block_reason(&todos, &[weak]).expect("expected a block");
    assert!(!reason.is_empty());
    assert!(ship_block_reason(&todos, &[passing_goal()]).is_none());
}

#[test]
fn shipping_mid_cycle_with_open_work_is_not_blocked() {
    // The branch is normally pushed before the plan is closed out, and open
    // todos are not evidence of anything wrong yet. Only a group already
    // *claimed* complete without ownership blocks.
    let todos = vec![todo("a", "in_progress")];
    assert!(ship_block_reason(&todos, &[]).is_none());
    // No plan at all also ships: a read-only cycle keeps no todos, and there is
    // no assessment to contradict.
    assert!(ship_block_reason(&[], &[]).is_none());
}

#[test]
fn shipping_is_not_blocked_by_weak_completion_confidence_alone() {
    // Completion confidence describes work already claimed done and is raised
    // at cycle end; blocking the ship path on it would stop a push for a state
    // the ownership check says is fine.
    let mut todo = todo("a", "completed");
    todo.completion_confidence = Some(ConfidenceState::Speculative);
    assert!(ship_block_reason(&[todo], &[passing_goal()]).is_none());
}
