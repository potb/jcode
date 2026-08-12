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

use crate::todo::{GateObservation, TodoGoal};

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

/// The gate decision itself is shared with the TUI and `jcode run`.
///
/// It used to be a third copy living here, and the copies had already drifted:
/// this one checked end-to-end ownership and `jcode run` did not, so the same
/// todo list passed one surface and failed another. The decision now lives in
/// [`jcode_base::todo_gates`] and this module keeps only what is specific to an
/// unattended cycle: the prompt guidance above, the attempt budget, and the
/// runner's side effects.
pub use crate::todo_gates::{
    TodoGateFollowUp as AmbientGateFollowUp, TodoGateState as AmbientGateState, digest_is_due,
    next_todo_gate_follow_up as next_gate_follow_up,
};

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
