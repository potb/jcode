//! Tests for the notification-worthiness decision.
//!
//! Cases are drawn from real transcripts in ~/.jcode/ambient/transcripts, so
//! they represent what actually happens rather than what is easy to imagine.

use super::cycle_significance::*;

fn outcome(significance: CycleSignificance) -> CycleOutcome {
    CycleOutcome {
        significance,
        pending_permissions: 0,
        did_proactive_work: false,
        failed: false,
    }
}

#[test]
fn parses_declared_values() {
    assert_eq!(CycleSignificance::parse(Some("routine")), CycleSignificance::Routine);
    assert_eq!(CycleSignificance::parse(Some("notable")), CycleSignificance::Notable);
    assert_eq!(
        CycleSignificance::parse(Some("  ROUTINE  ")),
        CycleSignificance::Routine,
        "case and whitespace must not change the meaning"
    );
}

#[test]
fn accepts_plausible_synonyms() {
    // The model reaches for these; treating them as unspecified would notify
    // for cycles it explicitly called maintenance.
    assert_eq!(CycleSignificance::parse(Some("garden")), CycleSignificance::Routine);
    assert_eq!(
        CycleSignificance::parse(Some("maintenance")),
        CycleSignificance::Routine
    );
    assert_eq!(
        CycleSignificance::parse(Some("significant")),
        CycleSignificance::Notable
    );
}

#[test]
fn unknown_or_missing_is_unspecified() {
    assert_eq!(CycleSignificance::parse(None), CycleSignificance::Unspecified);
    assert_eq!(CycleSignificance::parse(Some("")), CycleSignificance::Unspecified);
    assert_eq!(
        CycleSignificance::parse(Some("kind of important")),
        CycleSignificance::Unspecified,
        "an unrecognised string must not be silently read as routine"
    );
}

/// The actual ask: garden-only cycles must not reach the phone.
#[test]
fn routine_cycle_is_silent() {
    assert!(!should_notify(&outcome(CycleSignificance::Routine)));
}

#[test]
fn notable_cycle_notifies() {
    assert!(should_notify(&outcome(CycleSignificance::Notable)));
}

/// Silence is the default, because garden cycles are the majority and none of
/// them declare anything. Every case where silence could cost the user is
/// covered by structure instead (see the tests below).
#[test]
fn unspecified_is_silent() {
    assert!(!should_notify(&outcome(CycleSignificance::Unspecified)));
}

/// A cycle must not be able to mute a request that is blocking on the user by
/// calling itself routine. This is the entire point of the channel.
#[test]
fn pending_permission_always_notifies() {
    let mut o = outcome(CycleSignificance::Routine);
    o.pending_permissions = 1;
    assert!(
        should_notify(&o),
        "a routine label must never suppress work blocked on the user"
    );
}

/// A failed cycle may not have reached its own reporting code, so its label
/// (or absence of one) cannot be trusted to mean "nothing to see".
#[test]
fn failure_always_notifies() {
    let mut o = outcome(CycleSignificance::Routine);
    o.failed = true;
    assert!(should_notify(&o), "a failed cycle is news even if labelled routine");

    let mut o = outcome(CycleSignificance::Unspecified);
    o.failed = true;
    assert!(should_notify(&o));
}

/// Changing code is never routine, whatever the cycle claims.
#[test]
fn proactive_work_always_notifies() {
    let mut o = outcome(CycleSignificance::Routine);
    o.did_proactive_work = true;
    assert!(should_notify(&o));
}

/// Real transcript 2026-08-07-192228: "Garden-only cycle, no work needed."
/// with memories_modified=0, and 2026-08-07-190730: garden-only WITH
/// memories_modified=2. Both must be silent, which is exactly why the counts
/// cannot drive this decision.
#[test]
fn real_garden_cycles_are_silent() {
    // Neither cycle's structure differs; only the declaration does.
    assert!(!should_notify(&outcome(CycleSignificance::Routine)));

    let mut with_memory_work = outcome(CycleSignificance::Routine);
    with_memory_work.did_proactive_work = false;
    assert!(
        !should_notify(&with_memory_work),
        "gardening IS memory work, so touching memories must not force a push"
    );
}

/// Real transcript 2026-08-07-184733: "#763 and #764 are both MERGED into
/// staging" with memories_modified=1 — structurally identical to a garden
/// cycle, but the user wanted this one.
#[test]
fn real_newsworthy_cycle_notifies() {
    assert!(
        should_notify(&outcome(CycleSignificance::Notable)),
        "a cycle with the same counts as gardening must still be able to reach the user"
    );
}

/// The agent has to be TOLD about `significance`, or it never sets it and
/// every cycle falls to the silent default. A gate wired to a field nobody
/// populates looks identical to a working one from the outside.
#[test]
fn prompt_instructs_the_agent_about_significance() {
    let prompt = crate::ambient::build_ambient_system_prompt(
        &crate::ambient::AmbientState::default(),
        &[],
        &crate::ambient::MemoryGraphHealth::default(),
        &[],
        &[],
        &crate::ambient::ResourceBudget {
            provider: "test".into(),
            tokens_remaining_desc: String::new(),
            window_resets_desc: String::new(),
            user_usage_rate_desc: String::new(),
            cycle_budget_desc: String::new(),
        },
        0,
    );
    assert!(
        prompt.contains("significance"),
        "the system prompt must tell the agent the field exists"
    );
    assert!(
        prompt.contains("routine"),
        "the prompt must name the routine value"
    );
    assert!(
        prompt.contains("notable"),
        "the prompt must name the notable value"
    );
}

/// The tool schema must expose the field, or the model cannot pass it even
/// when the prompt asks for it.
#[test]
fn tool_schema_exposes_significance() {
    use crate::tool::Tool;
    let tool = crate::tool::ambient::EndAmbientCycleTool;
    let schema = tool.parameters_schema();
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("schema should have properties");
    let sig = props
        .get("significance")
        .expect("end_ambient_cycle must expose `significance`");
    let variants = sig.get("enum").and_then(|e| e.as_array()).expect("enum");
    assert_eq!(variants.len(), 2, "expected exactly routine|notable");
}
