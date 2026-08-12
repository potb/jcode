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

/// Todo guidance for the ambient system prompt.
///
/// The ambient runner sets `system_prompt_override`, which short-circuits
/// `Agent::build_system_prompt_split` and drops the base prompt entirely, so
/// none of its todo guidance reaches an ambient cycle (issue #22, direction 4).
/// The cycle is nevertheless held to the gates in this module, which is the
/// worst of both worlds: judged against expectations it was never told about,
/// and each unmet one costs a follow-up turn.
///
/// This deliberately restates the *behaviour* the gates need rather than
/// re-inlining the base prompt, because most of that prompt is about talking to
/// a user who is not there. It just as deliberately names no field thresholds:
/// which assessment values pass is private policy (see
/// `required_delivery_state` and `completion_confidence_passes`), and printing
/// the bar into the prompt would invite a cycle to write the passing value
/// rather than do the work it stands for.
pub const AMBIENT_TODO_GUIDANCE: &str = "\
## Todo Discipline

This cycle is held to the same todo quality gates as an interactive session, \
and unmet ones are re-raised as follow-up turns before the cycle is allowed to \
end. Save yourself the round trips:

- Keep the plan current as you go. Mark work completed when it is completed, \
and cancel what you decided not to do, so the list at the end of the cycle \
reflects what actually happened.
- Record the goal-level assessments honestly, including the feedback loop: \
name the observation or check that reports back on each requirement. \"Ran the \
tests\" counts only for the requirements those tests actually enforce.
- Set `completion_confidence` on every completed todo from the evidence you \
actually gathered, and let it climb as evidence accumulates rather than \
jumping to the end state at the last write. A confidence level that appears \
fully formed reads as unverified, not as done.
- Assess honestly in both directions. A weak assessment that matches reality \
is worth more than a strong one you cannot support: the gates ask for more \
work, and an inflated score sends that work to the wrong place.
- `end_ambient_cycle` does not close the cycle on its own. If a gate is still \
unmet you will be asked to continue, so treat the plan as part of the work and \
not as paperwork filed at the end.
";

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
