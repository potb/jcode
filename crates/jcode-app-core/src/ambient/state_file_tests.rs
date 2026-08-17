use std::collections::BTreeMap;

use super::state_file::AmbientStateFile;
use super::{AmbientCycleResult, AmbientState, AmbientStatus, CycleStatus};
use crate::storage;

fn cycle_result(summary: &str) -> AmbientCycleResult {
    let now = chrono::Utc::now();
    AmbientCycleResult {
        summary: summary.to_string(),
        memories_modified: 1,
        compactions: 0,
        proactive_work: None,
        significance: None,
        next_schedule: None,
        started_at: now,
        ended_at: now,
        status: CycleStatus::Complete,
        agent_session_id: None,
        conversation: None,
    }
}

/// A `state.json` written before the envelope existed is a bare `AmbientState`.
/// #126 asks for it to be migrated, so its cycle count and summary must land in
/// the global slot rather than being defaulted away.
#[test]
fn a_pre_envelope_state_file_migrates_into_the_global_slot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("state.json");
    std::fs::write(
        &path,
        r#"{
            "status": {"Running": {"detail": "running agent"}},
            "last_run": "2026-08-15T10:00:00Z",
            "last_summary": "legacy cycle",
            "last_compactions": 2,
            "last_memories_modified": 7,
            "total_cycles": 145
        }"#,
    )
    .expect("write legacy state");

    let file = AmbientStateFile::load_from(&path).expect("load legacy state");

    assert_eq!(file.global.total_cycles, 145, "cycle count must survive");
    assert_eq!(file.global.last_summary.as_deref(), Some("legacy cycle"));
    assert_eq!(file.global.last_memories_modified, Some(7));
    assert!(
        file.projects.is_empty(),
        "a legacy file has no per-project history to invent"
    );
}

#[test]
fn an_unparseable_state_file_is_an_error_not_an_empty_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("state.json");
    std::fs::write(&path, r#"{"total_cycles": "not a number"}"#).expect("write");

    assert!(
        AmbientStateFile::load_from(&path).is_err(),
        "defaulting here would report a user's history as empty and then overwrite it"
    );
}

#[test]
fn an_envelope_state_file_round_trips_project_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("state.json");

    let mut projects = BTreeMap::new();
    projects.insert(
        "/home/potb/jcode".to_string(),
        AmbientState {
            status: AmbientStatus::Idle,
            last_run: None,
            last_summary: Some("jcode cycle".to_string()),
            last_compactions: None,
            last_memories_modified: None,
            total_cycles: 3,
        },
    );
    let written = AmbientStateFile {
        global: AmbientState {
            total_cycles: 10,
            ..Default::default()
        },
        projects,
    };
    storage::write_json(&path, &written).expect("write envelope");

    let file = AmbientStateFile::load_from(&path).expect("load envelope");

    assert_eq!(file.global.total_cycles, 10);
    assert_eq!(file.project("/home/potb/jcode").total_cycles, 3);
    assert_eq!(
        file.project("/home/potb/projects/costo/beakon")
            .total_cycles,
        0,
        "a project with no history reads as default, not as an error"
    );
}

/// Per-project history is the point of stage 2: one project's cycle must not
/// be the only thing the file remembers, and must not leak into another
/// project's counters.
#[test]
fn recording_a_cycle_updates_its_project_and_the_global_slot() {
    let mut file = AmbientStateFile::default();

    file.record_cycle(Some("/home/potb/jcode"), &cycle_result("jcode work"));
    file.record_cycle(
        Some("/home/potb/projects/costo/beakon"),
        &cycle_result("beakon work"),
    );
    file.record_cycle(Some("/home/potb/jcode"), &cycle_result("more jcode work"));

    assert_eq!(file.project("/home/potb/jcode").total_cycles, 2);
    assert_eq!(
        file.project("/home/potb/jcode").last_summary.as_deref(),
        Some("more jcode work")
    );
    assert_eq!(
        file.project("/home/potb/projects/costo/beakon")
            .total_cycles,
        1,
        "another project's cycles must not be counted here"
    );
    assert_eq!(
        file.global.total_cycles, 3,
        "the global slot stays the daemon-wide total"
    );
}

#[test]
fn a_projectless_cycle_is_recorded_globally_only() {
    let mut file = AmbientStateFile::default();

    file.record_cycle(None, &cycle_result("gardening"));

    assert_eq!(file.global.total_cycles, 1);
    assert!(
        file.projects.is_empty(),
        "a cycle owning no project must not create a project slot"
    );
}

/// `AmbientState::save` is still the daemon's write path, so it must not
/// destroy the per-project map it does not know about.
#[test]
fn saving_global_state_preserves_per_project_state() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let mut file = AmbientStateFile::default();
    file.record_cycle(Some("/home/potb/jcode"), &cycle_result("jcode work"));
    file.save().expect("save envelope");

    let mut global = AmbientState::load().expect("load global");
    global.total_cycles += 1;
    global.save().expect("save global");

    let reloaded = AmbientStateFile::load().expect("reload envelope");

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }

    assert_eq!(
        reloaded.project("/home/potb/jcode").total_cycles,
        1,
        "per-project state must survive a global-only save"
    );
    assert_eq!(reloaded.global.total_cycles, 2);
}

/// Stage 4 of #126: the runner records a finished cycle against the project it
/// belonged to as well as globally, in one load/save, so the two cannot
/// disagree about the same cycle.
#[test]
fn recording_a_cycle_for_a_project_updates_both_slots_once() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = crate::ambient::test_env::EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let mut mgr = crate::ambient::AmbientManager::new().expect("manager");
    mgr.record_cycle_result_for(Some("/work/alpha"), cycle_result("alpha work"))
        .expect("record");

    let file = AmbientStateFile::load().expect("load");
    assert_eq!(file.global.total_cycles, 1);
    assert_eq!(file.project("/work/alpha").total_cycles, 1);
    assert_eq!(
        file.project("/work/alpha").last_summary.as_deref(),
        Some("alpha work"),
        "a project's own history must be attributable to it"
    );
    assert_eq!(
        file.project("/work/beta").total_cycles,
        0,
        "another project's history must not move"
    );

    mgr.record_cycle_result_for(None, cycle_result("gardening"))
        .expect("record");
    let file = AmbientStateFile::load().expect("load");
    assert_eq!(file.global.total_cycles, 2);
    assert_eq!(
        file.project("/work/alpha").total_cycles,
        1,
        "an unfocused cycle belongs to no project, so no project slot moves"
    );
}
