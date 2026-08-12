use crate::ambient::gates::{
    AmbientGateFollowUp, AmbientGateState, digest_is_due, next_gate_follow_up,
};
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
        ..Default::default()
    }
}

/// A completed todo whose confidence climbed one step, so only the gate under
/// test can fire.
fn completed_clean(id: &str) -> TodoItem {
    TodoItem {
        completion_confidence: Some(ConfidenceState::Verified),
        confidence: Some(ConfidenceState::Validated),
        confidence_history: vec![ConfidenceState::Validated, ConfidenceState::Verified],
        ..todo(id, "completed")
    }
}

/// A goal whose delivery assessment passes the ownership check on every field
/// it reads, so a test about a later gate is not silently answered by this one.
fn goal_delivered(group: Option<&str>) -> TodoGoal {
    TodoGoal {
        group: group.map(str::to_string),
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
fn no_todos_at_all_means_no_gate() {
    // Read-only cycles (queue check, memory gardening) legitimately keep no
    // todo list. Manufacturing a follow-up there would cost an API call every
    // quiet cycle.
    assert!(next_gate_follow_up(&[], &[], None, AmbientGateState::default()).is_none());
}

#[test]
fn incomplete_todos_block_the_end_of_the_cycle() {
    let todos = vec![completed_clean("1"), todo("2", "in_progress")];
    match next_gate_follow_up(&todos, &[goal_delivered(None)], None, Default::default()) {
        Some(AmbientGateFollowUp::Incomplete { count, .. }) => assert_eq!(count, 1),
        other => panic!("expected the incomplete gate, got {other:?}"),
    }
}

#[test]
fn cancelled_todos_do_not_count_as_incomplete() {
    let todos = vec![completed_clean("1"), todo("2", "cancelled")];
    assert!(
        next_gate_follow_up(&todos, &[goal_delivered(None)], None, Default::default()).is_none()
    );
}

#[test]
fn digest_is_withheld_while_work_remains() {
    // Building the digest clears the observation log, and the incomplete gate
    // outranks it, so taking it early would destroy the reminder.
    let todos = vec![todo("1", "pending")];
    assert!(!digest_is_due(&todos, AmbientGateState::default()));
    assert!(digest_is_due(
        &[completed_clean("1")],
        AmbientGateState::default()
    ));
}

#[test]
fn digest_is_not_due_twice() {
    let state = AmbientGateState {
        gate_digest_delivered: true,
        ..Default::default()
    };
    assert!(!digest_is_due(&[completed_clean("1")], state));
}

#[test]
fn digest_outranks_ownership_and_confidence() {
    // The digest may prompt work that changes the very assessments the later
    // gates read, so it has to be raised first.
    let todos = vec![completed_clean("1")];
    match next_gate_follow_up(&todos, &[], Some("weak points".into()), Default::default()) {
        Some(AmbientGateFollowUp::GateDigest { message }) => assert_eq!(message, "weak points"),
        other => panic!("expected the digest, got {other:?}"),
    }
}

#[test]
fn a_completed_group_without_a_passing_delivery_assessment_is_gated() {
    let todos = vec![completed_clean("1")];
    match next_gate_follow_up(&todos, &[], None, Default::default()) {
        Some(AmbientGateFollowUp::Ownership { message }) => assert!(!message.is_empty()),
        other => panic!("expected the ownership gate, got {other:?}"),
    }
}

#[test]
fn weak_completion_confidence_is_gated() {
    let todos = vec![TodoItem {
        completion_confidence: Some(ConfidenceState::Speculative),
        ..todo("1", "completed")
    }];
    match next_gate_follow_up(&todos, &[goal_delivered(None)], None, Default::default()) {
        Some(AmbientGateFollowUp::CompletionConfidence { .. }) => {}
        other => panic!("expected the completion-confidence gate, got {other:?}"),
    }
}

#[test]
fn missing_completion_confidence_is_gated() {
    let todos = vec![todo("1", "completed")];
    match next_gate_follow_up(&todos, &[goal_delivered(None)], None, Default::default()) {
        Some(AmbientGateFollowUp::CompletionConfidence { .. }) => {}
        other => panic!("expected the completion-confidence gate, got {other:?}"),
    }
}

#[test]
fn a_confidence_jump_is_challenged_once_only() {
    let todos = vec![TodoItem {
        confidence: Some(ConfidenceState::Speculative),
        completion_confidence: Some(ConfidenceState::Verified),
        confidence_history: vec![ConfidenceState::Speculative, ConfidenceState::Verified],
        ..todo("1", "completed")
    }];
    let goals = [goal_delivered(None)];
    match next_gate_follow_up(&todos, &goals, None, Default::default()) {
        Some(AmbientGateFollowUp::ConfidenceSpike { .. }) => {}
        other => panic!("expected the spike challenge, got {other:?}"),
    }
    // Once challenged it must not fire again, or the cycle would loop on it
    // until the attempt budget runs out.
    let challenged = AmbientGateState {
        confidence_spike_challenged: true,
        ..Default::default()
    };
    assert!(next_gate_follow_up(&todos, &goals, None, challenged).is_none());
}

#[test]
fn a_fully_validated_cycle_passes_every_gate() {
    let todos = vec![completed_clean("1"), completed_clean("2")];
    assert!(
        next_gate_follow_up(&todos, &[goal_delivered(None)], None, Default::default()).is_none()
    );
}

#[test]
fn ownership_is_judged_per_group() {
    // One group carried to delivery does not vouch for another.
    let todos = vec![
        TodoItem {
            group: Some("a".into()),
            ..completed_clean("1")
        },
        TodoItem {
            group: Some("b".into()),
            ..completed_clean("2")
        },
    ];
    let goals = [goal_delivered(Some("a"))];
    match next_gate_follow_up(&todos, &goals, None, Default::default()) {
        Some(AmbientGateFollowUp::Ownership { .. }) => {}
        other => panic!("expected the ownership gate for group b, got {other:?}"),
    }
}
