use super::paths::project_lock_path;
use super::project_schedule::ProjectWakeLedger;
use super::test_env::EnvVarGuard;
use crate::ambient::AmbientLock;
use chrono::{Duration as ChronoDuration, Utc};
use std::time::Duration;

fn key(path: &str) -> Option<String> {
    Some(path.to_string())
}

#[test]
fn a_new_project_is_due_immediately_rather_than_waiting_out_an_interval() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();

    ledger.register(key("/a"), now);

    assert_eq!(ledger.due_project(now), Some(key("/a")));
}

#[test]
fn a_project_that_just_ran_is_not_due_again_until_its_interval_elapses() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/a"), now);

    ledger.record_cycle(key("/a"), now, Duration::from_secs(600));

    assert_eq!(
        ledger.due_project(now),
        None,
        "a project must not run twice back to back"
    );
    assert_eq!(
        ledger.due_project(now + ChronoDuration::minutes(11)),
        Some(key("/a"))
    );
}

/// The starvation #126 is about: one project running repeatedly must not keep
/// another from ever getting a turn.
#[test]
fn a_busy_project_cannot_starve_a_quiet_one() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/busy"), now);
    ledger.register(key("/quiet"), now);
    let interval = Duration::from_secs(600);

    let first = ledger.due_project(now).expect("someone is due");
    ledger.record_cycle(first.clone(), now, interval);

    let second = ledger
        .due_project(now)
        .expect("the other project is still due");
    assert_ne!(
        second, first,
        "the project that just ran must not be picked again while another waits"
    );
}

#[test]
fn the_project_that_has_waited_longest_goes_first() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/recent"), now);
    ledger.register(key("/waiting"), now - ChronoDuration::hours(2));

    assert_eq!(ledger.due_project(now), Some(key("/waiting")));
}

#[test]
fn a_cycle_belonging_to_no_project_is_scheduled_like_any_other() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(None, now);
    ledger.register(key("/a"), now + ChronoDuration::hours(1));

    assert_eq!(ledger.due_project(now), Some(None));
    assert_eq!(
        ledger.earliest_wake(),
        Some(now),
        "the sleep must be sized by the soonest project, not the last registered"
    );
}

#[test]
fn registering_an_already_scheduled_project_does_not_reset_its_wake() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/a"), now);
    ledger.record_cycle(key("/a"), now, Duration::from_secs(600));

    ledger.register(key("/a"), now);

    assert_eq!(
        ledger.due_project(now),
        None,
        "re-registering on every pass would let a project run continuously"
    );
}

#[test]
fn two_projects_take_different_lock_files_but_share_the_global_one() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let a = project_lock_path(Some("/home/potb/jcode")).expect("path");
    let b = project_lock_path(Some("/home/potb/projects/costo/beakon")).expect("path");
    let global = project_lock_path(None).expect("path");

    assert_ne!(a, b, "distinct projects must not contend on one lock");
    assert_ne!(a, global);
    assert_eq!(
        global,
        temp.path().join("ambient").join("ambient.lock"),
        "the global lock path must stay where older builds contend"
    );
    assert_eq!(
        project_lock_path(Some("/home/potb/jcode")).expect("path"),
        a,
        "the same project must resolve to the same lock every time"
    );
}

/// Sanitizing a path into a file name is lossy, so two projects differing only
/// in characters it collapses would otherwise share a lock and serialize.
#[test]
fn projects_that_sanitize_alike_still_get_distinct_locks() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let slash = project_lock_path(Some("/a/b")).expect("path");
    let dot = project_lock_path(Some("/a.b")).expect("path");

    assert_ne!(slash, dot);
}

/// The point of a per-project lock: a cycle running in one project must not
/// block a cycle in another.
///
/// Acquiring both is not on its own proof, because a lock naming our own PID is
/// reclaimed as stale, so a single shared lock would also let both calls
/// succeed. What distinguishes them is that both lock files exist at once, and
/// the first project's lock is still intact after the second acquires.
#[test]
fn a_cycle_in_one_project_does_not_exclude_a_cycle_in_another() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let first = AmbientLock::try_acquire_for(Some("/home/potb/jcode"))
        .expect("try_acquire")
        .expect("first project acquires");
    let first_path = project_lock_path(Some("/home/potb/jcode")).expect("path");
    let second = AmbientLock::try_acquire_for(Some("/home/potb/projects/costo/beakon"))
        .expect("try_acquire")
        .expect("a second project must acquire while the first is held");
    let second_path = project_lock_path(Some("/home/potb/projects/costo/beakon")).expect("path");

    assert!(
        first_path.exists() && second_path.exists(),
        "both cycles must hold their own lock at the same time"
    );
    assert_ne!(first_path, second_path);

    drop(second);
    assert!(
        first_path.exists(),
        "releasing one project's lock must not release another's"
    );
    drop(first);
    assert!(!first_path.exists());
}

#[test]
fn a_project_lock_still_excludes_a_foreign_holder_of_the_same_project() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let path = project_lock_path(Some("/home/potb/jcode")).expect("path");
    let live_foreign_pid = "1";
    std::fs::write(&path, live_foreign_pid).expect("write foreign lock");

    assert!(
        AmbientLock::try_acquire_for(Some("/home/potb/jcode"))
            .expect("try_acquire")
            .is_none(),
        "per-project locking must not weaken single-instance protection"
    );
    assert!(
        crate::ambient::is_locked_by_another_process_for(Some("/home/potb/jcode")),
        "the held project must report as locked"
    );
    assert!(
        !crate::ambient::is_locked_by_another_process_for(Some("/other")),
        "an unrelated project must not report as locked"
    );
}

#[test]
fn a_due_queue_item_decides_the_cycle_project_over_the_rotation() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/a"), now - ChronoDuration::hours(3));
    ledger.register(key("/b"), now - ChronoDuration::hours(1));

    assert_eq!(
        super::project_schedule::select_cycle_project(&ledger, &[key("/b")], now),
        key("/b"),
        "an item explicitly queued for a project must run as that project's \
         cycle even when another project has waited longer"
    );
}

#[test]
fn without_a_due_item_the_rotation_decides_the_cycle_project() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/a"), now - ChronoDuration::hours(3));
    ledger.register(key("/b"), now - ChronoDuration::hours(1));

    assert_eq!(
        super::project_schedule::select_cycle_project(&ledger, &[], now),
        key("/a"),
        "the longest-waiting project takes the turn"
    );
}

#[test]
fn a_project_less_due_item_does_not_hijack_the_rotation() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.register(key("/a"), now - ChronoDuration::hours(3));

    assert_eq!(
        super::project_schedule::select_cycle_project(&ledger, &[None], now),
        key("/a"),
        "an unscoped item (gardening, a memory task) names no project, so it \
         must not force an unfocused cycle while a project is due"
    );
}

#[test]
fn nothing_due_falls_back_to_an_unfocused_cycle() {
    let now = Utc::now();
    let mut ledger = ProjectWakeLedger::new();
    ledger.record_cycle(key("/a"), now, Duration::from_secs(3600));

    assert_eq!(
        super::project_schedule::select_cycle_project(&ledger, &[], now),
        None,
        "a runner that got this far is starting a cycle regardless, so with no \
         project due it must be an unfocused one rather than an arbitrary project"
    );
}
