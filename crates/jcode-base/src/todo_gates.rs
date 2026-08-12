//! The one todo gate decision, shared by every surface that enforces it.
//!
//! Three separate implementations of this decision used to exist: the TUI
//! (`tui/app/input.rs`), headless `jcode run`
//! (`build_run_auto_poke_follow_up_from_todos`), and the ambient runner
//! (`ambient/gates.rs`). They agreed on the intent and disagreed in detail --
//! ambient checked end-to-end ownership and `jcode run` did not, so the same
//! todo list could pass one surface and fail another. That is the failure mode
//! this module exists to remove: the *order* and the *criteria* live here once,
//! and each surface keeps only its own side effects (display messages, queued
//! turns, attempt budgets, telemetry).
//!
//! The decision is deliberately pure. It takes the todos, the goals, and an
//! already-built digest, and returns what to ask for next -- no session I/O, no
//! provider, no terminal -- so all three surfaces can be tested against it
//! without standing up their own runtime.

use crate::todo::{TodoGoal, TodoItem};

/// The next thing a surface should ask the agent for, or `None` when the todo
/// state is good enough to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoGateFollowUp {
    /// Work is still open; keep going.
    Incomplete { count: usize, message: String },
    /// Deferred quality-review points recorded during the turn.
    GateDigest { message: String },
    /// A completed goal group was not carried far enough to be owned
    /// end-to-end.
    Ownership { message: String },
    /// Completed todos whose completion confidence does not clear the bar.
    CompletionConfidence { message: String },
    /// A completed todo whose confidence jumped levels rather than climbing.
    ConfidenceSpike { message: String },
}

impl TodoGateFollowUp {
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

    /// Consume the follow-up for its message.
    pub fn into_message(self) -> String {
        match self {
            Self::Incomplete { message, .. }
            | Self::GateDigest { message }
            | Self::Ownership { message }
            | Self::CompletionConfidence { message }
            | Self::ConfidenceSpike { message } => message,
        }
    }

    /// Short stable label, so a stalled surface can log which gate stalled.
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

/// What this cycle/turn has already delivered, so a gate is not raised twice.
#[derive(Debug, Default, Clone, Copy)]
pub struct TodoGateState {
    /// The deferred digest has already been delivered for this cycle.
    pub gate_digest_delivered: bool,
    /// The confidence-spike challenge has already been issued once.
    pub confidence_spike_challenged: bool,
}

/// Whether a todo still counts as open work.
pub fn todo_is_open(todo: &TodoItem) -> bool {
    !crate::todo::todo_status_is_completed(&todo.status)
        && !crate::todo::todo_status_is_cancelled(&todo.status)
}

/// Whether any work is still open.
pub fn work_remains(todos: &[TodoItem]) -> bool {
    todos.iter().any(todo_is_open)
}

/// Whether the deferred gate digest may be consumed yet.
///
/// Building the digest clears the observation log, and the incomplete-todo
/// follow-up outranks it, so consuming it while work is still open would empty
/// the log and drop the reminder on the floor.
pub fn digest_is_due(todos: &[TodoItem], state: TodoGateState) -> bool {
    !state.gate_digest_delivered && !work_remains(todos)
}

/// Decide the next gate follow-up, or `None` when the turn may end.
///
/// The order is the contract: finish the work, then raise the recorded weak
/// points (they can change the very assessments checked next), then end-to-end
/// ownership, then completion confidence, then the confidence spike.
///
/// `gate_digest` is the already-built digest for this turn, or `None` when
/// nothing was recorded or it is not due yet (see [`digest_is_due`]); building
/// it is a side effect and therefore stays with the caller.
pub fn next_todo_gate_follow_up(
    todos: &[TodoItem],
    goals: &[TodoGoal],
    gate_digest: Option<String>,
    state: TodoGateState,
) -> Option<TodoGateFollowUp> {
    let incomplete = todos.iter().filter(|todo| todo_is_open(todo)).count();
    if incomplete > 0 {
        return Some(TodoGateFollowUp::Incomplete {
            count: incomplete,
            message: crate::todo::build_auto_poke_message(incomplete),
        });
    }

    if let Some(message) = gate_digest {
        return Some(TodoGateFollowUp::GateDigest { message });
    }

    // No todo list at all gets no gates. A turn or cycle that only reads state
    // legitimately keeps none, and manufacturing a follow-up there would spend
    // an API call on every quiet turn.
    if todos.is_empty() {
        return None;
    }

    if !crate::todo::completed_groups_have_sufficient_delivery(todos, goals) {
        return Some(TodoGateFollowUp::Ownership {
            message: crate::todo::build_todo_ownership_continuation_message(todos, goals),
        });
    }

    let completed: Vec<&TodoItem> = todos
        .iter()
        .filter(|todo| crate::todo::todo_status_is_completed(&todo.status))
        .collect();
    if completed.is_empty() {
        // Everything was cancelled. There is nothing whose completion could be
        // over- or under-claimed, so there is nothing to validate.
        return None;
    }

    if completed
        .iter()
        .any(|todo| !crate::todo::completion_confidence_passes(todo.completion_confidence))
    {
        return Some(TodoGateFollowUp::CompletionConfidence {
            message: crate::todo::build_todo_completion_continuation_message(todos),
        });
    }

    if !state.confidence_spike_challenged && !crate::todo::spike_completed_todos(todos).is_empty() {
        return Some(TodoGateFollowUp::ConfidenceSpike {
            message: crate::todo::build_todo_confidence_spike_continuation_message(todos),
        });
    }

    None
}

#[cfg(test)]
#[path = "todo_gates_tests.rs"]
mod todo_gates_tests;
