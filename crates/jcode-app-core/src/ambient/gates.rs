//! Todo quality gates for headless ambient cycles.
//!
//! Interactive sessions (`jcode-tui`) and headless `jcode run` both hold the
//! agent to the same todo gates: the deferred gate digest, the end-to-end
//! ownership check, the completion-confidence check, and the confidence-spike
//! challenge. The headless ambient runner enforced none of them (issue #22), so
//! an unattended cycle could ship a branch and a PR with unvalidated todos.
//!
//! The decision itself is pure and lives here so it can be tested without a
//! provider, an agent, or a terminal. The runner owns only the side effects:
//! sending the follow-up message and counting attempts.

use crate::todo::{GateObservation, TodoGoal, TodoItem};

/// How many gate follow-ups one cycle may send before we stop nudging.
///
/// A gate that keeps failing while the agent stops making progress on it would
/// otherwise loop for the rest of the cycle, burning one API call per turn. The
/// TUI learned this the hard way and caps at 5; ambient uses the same budget so
/// the two paths behave alike.
pub const AMBIENT_GATE_MAX_ATTEMPTS: u8 = 5;

/// A gate follow-up the ambient cycle should deliver before accepting the end
/// of the cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmbientGateFollowUp {
    /// Todos are still open; keep working.
    Incomplete { count: usize, message: String },
    /// Deferred quality-review points recorded during the cycle.
    GateDigest { message: String },
    /// A completed goal group has not been carried far enough to be owned
    /// end-to-end.
    Ownership { message: String },
    /// Completed todos whose completion confidence does not pass.
    CompletionConfidence { message: String },
    /// A completed todo whose confidence jumped levels rather than climbing.
    ConfidenceSpike { message: String },
}

impl AmbientGateFollowUp {
    /// The message to send back to the agent.
    pub fn message(&self) -> &str {
        match self {
            Self::Incomplete { message, .. }
            | Self::GateDigest { message }
            | Self::Ownership { message }
            | Self::CompletionConfidence { message }
            | Self::ConfidenceSpike { message } => message,
        }
    }

    /// Short label for the run log, so a stalled cycle says which gate stalled.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Incomplete { .. } => "incomplete-todos",
            Self::GateDigest { .. } => "gate-digest",
            Self::Ownership { .. } => "ownership",
            Self::CompletionConfidence { .. } => "completion-confidence",
            Self::ConfidenceSpike { .. } => "confidence-spike",
        }
    }
}

/// What the cycle has already delivered, so a gate is not raised twice.
#[derive(Debug, Default, Clone, Copy)]
pub struct AmbientGateState {
    pub gate_digest_delivered: bool,
    pub confidence_spike_challenged: bool,
}

fn work_remains(todos: &[TodoItem]) -> bool {
    todos.iter().any(|todo| {
        !crate::todo::todo_status_is_completed(&todo.status)
            && !crate::todo::todo_status_is_cancelled(&todo.status)
    })
}

/// Whether the gate digest may be consumed yet.
///
/// Building the digest clears the observation log, and the incomplete-todo
/// follow-up takes precedence over it, so consuming it while work is still open
/// would drop the reminder on the floor with the log already emptied. Same rule
/// the `jcode run` path follows.
pub fn digest_is_due(todos: &[TodoItem], state: AmbientGateState) -> bool {
    !state.gate_digest_delivered && !work_remains(todos)
}

/// Build the digest text from an already-loaded observation log.
pub fn build_digest(
    observations: &[GateObservation],
    plan: &crate::todo::TodoPlan,
    goals: &[TodoGoal],
) -> Option<String> {
    if observations.is_empty() {
        return None;
    }
    crate::todo::build_gate_digest(observations, plan, goals)
}

/// Decide the next gate follow-up for an ambient cycle, or `None` when the
/// cycle may end.
///
/// Order matches the interactive path deliberately: finish the work, then raise
/// the recorded weak points (they may change the very assessments checked
/// next), then ownership, then confidence. `gate_digest` is the already-built
/// digest for this turn, or `None` when there is nothing recorded or it is not
/// due yet.
pub fn next_gate_follow_up(
    todos: &[TodoItem],
    goals: &[TodoGoal],
    gate_digest: Option<String>,
    state: AmbientGateState,
) -> Option<AmbientGateFollowUp> {
    let incomplete: Vec<TodoItem> = todos
        .iter()
        .filter(|todo| {
            !crate::todo::todo_status_is_completed(&todo.status)
                && !crate::todo::todo_status_is_cancelled(&todo.status)
        })
        .cloned()
        .collect();
    if !incomplete.is_empty() {
        return Some(AmbientGateFollowUp::Incomplete {
            count: incomplete.len(),
            message: crate::todo::build_auto_poke_message(incomplete.len()),
        });
    }

    if let Some(message) = gate_digest {
        return Some(AmbientGateFollowUp::GateDigest { message });
    }

    // A cycle with no todo list at all gets no gates. Ambient cycles that only
    // read state (queue check, memory gardening) legitimately finish without
    // one, and manufacturing a follow-up there would turn every quiet cycle
    // into an extra API call.
    if todos.is_empty() {
        return None;
    }

    if !crate::todo::completed_groups_have_sufficient_delivery(todos, goals) {
        return Some(AmbientGateFollowUp::Ownership {
            message: crate::todo::build_todo_ownership_continuation_message(todos, goals),
        });
    }

    let completed: Vec<&TodoItem> = todos
        .iter()
        .filter(|todo| crate::todo::todo_status_is_completed(&todo.status))
        .collect();
    if completed.is_empty() {
        return None;
    }

    if completed
        .iter()
        .any(|todo| !crate::todo::completion_confidence_passes(todo.completion_confidence))
    {
        return Some(AmbientGateFollowUp::CompletionConfidence {
            message: crate::todo::build_todo_completion_continuation_message(todos),
        });
    }

    if !state.confidence_spike_challenged && !crate::todo::spike_completed_todos(todos).is_empty() {
        return Some(AmbientGateFollowUp::ConfidenceSpike {
            message: crate::todo::build_todo_confidence_spike_continuation_message(todos),
        });
    }

    None
}
