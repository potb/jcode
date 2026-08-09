use super::*;
use crate::background::BackgroundTaskManager;

/// A minimal task for cache tests. Field-by-field so adding a field to
/// `BgTask` is a compile error here rather than a silently untested default.
fn sample_task(id: &str) -> BgTask {
    BgTask {
        id: id.to_string(),
        label: "sample".to_string(),
        tool: "bash".to_string(),
        status: BgStatus::Running,
        exit_code: None,
        elapsed_secs: Some(1.0),
        progress: None,
        error: None,
        output_tail: Vec::new(),
        session_id: "s".to_string(),
        is_current_session: true,
    }
}

#[test]
fn output_tail_returns_empty_for_unknown_task() {
    assert!(output_tail("definitely-not-a-task-id", 10).is_empty());
}

#[test]
fn status_maps_to_panel_states_including_orphans() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut status = crate::background::TaskStatusFile {
        task_id: "111111aaaa".to_string(),
        tool_name: "bash".to_string(),
        display_name: Some("cargo test".to_string()),
        session_id: "session-a".to_string(),
        status: crate::bus::BackgroundTaskStatus::Completed,
        exit_code: Some(0),
        error: None,
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        duration_secs: Some(3.0),
        pid: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: true,
        wake: false,
        progress: None,
        event_history: Vec::new(),
    };

    let task = to_bg_task(&manager, &status, Some("session-a"));
    assert_eq!(task.status, BgStatus::Completed);
    assert_eq!(task.label, "cargo test");
    assert!(task.is_current_session);

    // A Running file whose owner PID is dead is a phantom, not live work.
    status.status = crate::bus::BackgroundTaskStatus::Running;
    status.owner_pid = Some(4_000_000);
    status.duration_secs = None;
    let task = to_bg_task(&manager, &status, Some("session-b"));
    assert_eq!(task.status, BgStatus::Orphaned);
    assert!(
        !task.is_current_session,
        "task from another session must not be marked current"
    );

    // No owner metadata means "assume live": that is how tasks owned by the
    // separate server daemon look from a TUI client process.
    status.owner_pid = None;
    let task = to_bg_task(&manager, &status, Some("session-a"));
    assert_eq!(task.status, BgStatus::Running);

    // Terminal states must map to terminal panel states.
    //
    // Scope, measured rather than assumed: this catches a wrong mapping
    // (Completed -> Running fails it). It does NOT catch a liveness guard
    // added to the terminal arms, because `task_looks_live` returns false for
    // non-Running files, so such a guard is inert for exactly these inputs.
    // Covering that would need a status file the liveness check calls live
    // while the file says finished, which the writer never produces.
    for (file_status, expected) in [
        (
            crate::bus::BackgroundTaskStatus::Completed,
            BgStatus::Completed,
        ),
        (crate::bus::BackgroundTaskStatus::Failed, BgStatus::Failed),
    ] {
        status.status = file_status.clone();
        // Deliberately hostile metadata: a live-looking owner must not
        // resurrect a finished task.
        status.owner_pid = Some(std::process::id());
        status.duration_secs = Some(2.0);
        let task = to_bg_task(&manager, &status, Some("session-a"));
        assert_eq!(
            task.status, expected,
            "a {file_status:?} task must stay terminal, not be shown as running"
        );
    }
}

#[test]
fn elapsed_falls_back_to_wall_clock_for_running_tasks() {
    let status = crate::background::TaskStatusFile {
        task_id: "111111aaaa".to_string(),
        tool_name: "bash".to_string(),
        display_name: None,
        session_id: "s".to_string(),
        status: crate::bus::BackgroundTaskStatus::Running,
        exit_code: None,
        error: None,
        started_at: (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339(),
        completed_at: None,
        duration_secs: None,
        pid: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: true,
        wake: false,
        progress: None,
        event_history: Vec::new(),
    };
    let elapsed = elapsed_secs(&status).expect("running task should report elapsed time");
    assert!(
        (25.0..40.0).contains(&elapsed),
        "unexpected elapsed: {elapsed}"
    );
}

/// Weeks of history would bury the running work the panel exists to show.
///
/// This drives `build_tasks`, not just the `finished_recently` predicate.
/// The predicate-only version of this test was vacuous: deleting the `retain`
/// call in the fetch pipeline left it green, because nothing asserted the
/// filter was actually wired in. Found by mutation testing.
#[test]
fn old_finished_tasks_are_dropped_from_the_panel() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let status = |id: &str, completed: Option<chrono::DateTime<chrono::Utc>>| {
        crate::background::TaskStatusFile {
            task_id: id.to_string(),
            tool_name: "bash".to_string(),
            display_name: None,
            session_id: "s".to_string(),
            status: crate::bus::BackgroundTaskStatus::Completed,
            exit_code: Some(0),
            error: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: completed.map(|at| at.to_rfc3339()),
            duration_secs: Some(1.0),
            pid: None,
            owner_pid: None,
            owner_instance: None,
            detached: false,
            notify: true,
            wake: false,
            progress: None,
            event_history: Vec::new(),
        }
    };

    let now = chrono::Utc::now();
    let statuses = vec![
        status("111111aaaa", Some(now - chrono::Duration::hours(6))),
        status("222222bbbb", Some(now - chrono::Duration::minutes(2))),
        // Still running: no completion timestamp, always kept.
        status("333333cccc", None),
    ];

    let ids: Vec<String> = build_tasks(&manager, statuses, Some("s"))
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert!(
        !ids.contains(&"111111aaaa".to_string()),
        "a 6h-old finished task should be dropped: {ids:?}"
    );
    assert!(
        ids.contains(&"222222bbbb".to_string()),
        "a recently finished task should be kept: {ids:?}"
    );
    assert!(
        ids.contains(&"333333cccc".to_string()),
        "an unfinished task should always be kept: {ids:?}"
    );
}

/// Ordering is by recorded start time, never by task id: ids carry only the
/// last six digits of a millisecond timestamp, so they wrap roughly every 17
/// minutes and sort wrong across the wrap.
#[test]
fn tasks_are_ordered_by_start_time_not_by_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mk = |id: &str, started: chrono::DateTime<chrono::Utc>| crate::background::TaskStatusFile {
        task_id: id.to_string(),
        tool_name: "bash".to_string(),
        display_name: None,
        session_id: "s".to_string(),
        status: crate::bus::BackgroundTaskStatus::Running,
        exit_code: None,
        error: None,
        started_at: started.to_rfc3339(),
        completed_at: None,
        duration_secs: None,
        pid: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: true,
        wake: false,
        progress: None,
        event_history: Vec::new(),
    };
    let now = chrono::Utc::now();
    // Lexically "100000aaaa" < "999999bbbb", but the "100000" task is newer.
    let statuses = vec![
        mk("999999bbbb", now - chrono::Duration::minutes(30)),
        mk("100000aaaa", now),
    ];
    let ids: Vec<String> = build_tasks(&manager, statuses, Some("s"))
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert_eq!(
        ids,
        vec!["100000aaaa".to_string(), "999999bbbb".to_string()],
        "newest task must come first"
    );
}

//// The snapshot cache is process-global (shared across threads), and its
/// reuse rule is scoped by session.
///
/// The TUI event loop is an async `select!` on a multi-threaded tokio runtime,
/// so it hops worker threads at await points. A thread-local cache meant each
/// worker held its own copy, so `invalidate_cache()` after a key press could
/// fail to clear the copy the next render read.
///
/// Deliberately asserts on cache *identity* and on the pure reuse predicate,
/// never on cache *contents*. Anything that renders a real `App` or reads the
/// panel repopulates this global via `ui::draw`, so a test that asserted on
/// ambient contents was flaky under the full suite no matter how many of the
/// panel's own tests were serialized around it. Identity and the predicate are
/// the actual claims, and neither can be perturbed by a concurrent writer.
#[test]
fn snapshot_cache_is_shared_across_threads() {
    let here = cache() as *const _;
    let there = std::thread::spawn(|| cache() as *const _ as usize)
        .join()
        .expect("thread joined");
    assert_eq!(
        here as usize, there,
        "every thread must see one and the same cache cell, not a per-thread copy"
    );
}

/// The reuse rule itself: fresh enough AND mapped for the same session.
///
/// A pure predicate, so this needs no global state and cannot race.
#[test]
fn a_snapshot_is_only_reused_for_the_session_it_was_mapped_for() {
    let fresh = Snapshot {
        fetched_at: std::time::Instant::now(),
        session: Some("session-a".to_string()),
        tasks: vec![sample_task("t1")],
    };
    assert!(
        fresh.reusable_for(Some("session-a")),
        "a fresh snapshot must be reused for the session it was mapped for"
    );
    assert!(
        !fresh.reusable_for(Some("session-b")),
        "a snapshot mapped for session-a must not be served to session-b:          is_current_session is baked into each task"
    );
    assert!(
        !fresh.reusable_for(None),
        "a session-scoped snapshot must not be served to an unscoped caller"
    );

    let stale = Snapshot {
        fetched_at: std::time::Instant::now() - REFRESH_INTERVAL * 2,
        session: Some("session-a".to_string()),
        tasks: vec![sample_task("t1")],
    };
    assert!(
        !stale.reusable_for(Some("session-a")),
        "a snapshot older than the refresh interval must not be reused"
    );
}

/// The count path must borrow the task vector, never copy it.
///
/// This is the whole reason `Snapshot::count` exists: visibility and selection
/// clamping run several times per frame, and each `BgTask` carries a label
/// plus a 64-line output tail, so cloning to call `.len()` would copy
/// megabytes and perform tens of thousands of allocations per frame.
///
/// Calibrates against a borrow-only reference pass over the same data instead
/// of using a fixed time budget or a ratio. Two earlier shapes both failed to
/// catch a `count` that clones internally:
///   - racing it against a cloning baseline, because a cloning `count` simply
///     ties the baseline;
///   - comparing a heavy payload against a light one, because clone cost is
///     dominated by allocation *count*, which payload size does not change
///     (measured: 1.5x apart on time, while absolute cost differed 1000x).
/// A machine-speed-relative bound sidesteps both: copying shows up as three
/// orders of magnitude over a borrow, on any machine.
#[test]
fn counting_borrows_the_tasks_instead_of_copying_them() {
    use std::time::Instant as StdInstant;

    let snapshot = Snapshot {
        fetched_at: StdInstant::now(),
        session: Some("s".to_string()),
        tasks: (0..200)
            .map(|i| {
                let mut task = sample_task(&format!("{i:06}aaaa"));
                task.label = "x".repeat(2_000);
                task.output_tail = (0..64)
                    .map(|n| format!("line {n} {}", "y".repeat(200)))
                    .collect();
                // Every tenth task belongs to another session, so the scope
                // filter is exercised rather than trivially matching all.
                task.is_current_session = i % 10 != 0;
                task
            })
            .collect(),
    };

    assert_eq!(
        snapshot.count(false),
        200,
        "unscoped counting must see every task"
    );
    assert_eq!(
        snapshot.count(true),
        180,
        "scoped counting must exclude other sessions' tasks"
    );

    const ROUNDS: usize = 200;
    let measure = |body: &mut dyn FnMut() -> usize| {
        let start = StdInstant::now();
        for _ in 0..ROUNDS {
            std::hint::black_box(body());
        }
        start.elapsed()
    };

    // Reference: a borrow-only pass over exactly the same data. This is what
    // counting *should* cost, measured on this machine right now, so the
    // assertion below needs no absolute time budget.
    let mut reference = || {
        snapshot
            .tasks
            .iter()
            .filter(|task| task.is_current_session)
            .count()
    };
    let mut under_test = || snapshot.count(true);

    // Warm up both so neither pays for cold caches or first-touch faults.
    std::hint::black_box(measure(&mut reference));
    std::hint::black_box(measure(&mut under_test));

    let reference_time = measure(&mut reference);
    let counting_time = measure(&mut under_test);

    // Copying the payload costs ~1000x a borrow here, so 50x is far above
    // scheduler noise yet far below any implementation that copies.
    assert!(
        counting_time.as_nanos() < reference_time.as_nanos().max(1) * 50,
        "counting ({counting_time:?}) is far slower than a borrow-only pass \
         over the same tasks ({reference_time:?}): Snapshot::count is copying \
         task payload instead of borrowing it"
    );
}

/// A stale or wrong-session cache entry must be refreshed, not served.
///
/// Both read paths share `with_fresh_snapshot`, whose whole job is "reuse if
/// still valid, otherwise refetch". Disabling the refetch served stale data
/// forever and failed no test, because the other cache tests only exercise the
/// reuse predicate, never the refresh it gates.
///
/// Uses a sentinel that cannot come from a real directory scan: if it survives
/// a read that should have refreshed, the refresh did not happen.
#[test]
fn an_unusable_cache_entry_is_refetched_rather_than_served() {
    // Stale by age, right session: must refresh.
    {
        let mut guard = cache().lock().expect("cache lock");
        *guard = Some(Snapshot {
            fetched_at: std::time::Instant::now() - REFRESH_INTERVAL * 2,
            session: Some("session-x".to_string()),
            tasks: vec![sample_task("stale-sentinel")],
        });
    }
    let after_age = tasks_snapshot(Some("session-x"));
    assert!(
        !after_age.iter().any(|t| t.id == "stale-sentinel"),
        "an entry older than the refresh interval must be refetched, not served"
    );

    // Fresh but mapped for another session: must also refresh.
    {
        let mut guard = cache().lock().expect("cache lock");
        *guard = Some(Snapshot {
            fetched_at: std::time::Instant::now(),
            session: Some("session-a".to_string()),
            tasks: vec![sample_task("wrong-session-sentinel")],
        });
    }
    let for_other = tasks_snapshot(Some("session-b"));
    assert!(
        !for_other.iter().any(|t| t.id == "wrong-session-sentinel"),
        "a snapshot mapped for another session must be refetched, not served"
    );

    // Both paths share the refresh, so the count path must honor age too.
    // The count path shares the same refresh. Seed an implausible size: a real
    // scan of this machine will not return 9999 tasks, so a count that large
    // means the stale entry was served instead of refetched.
    {
        let mut guard = cache().lock().expect("cache lock");
        *guard = Some(Snapshot {
            fetched_at: std::time::Instant::now() - REFRESH_INTERVAL * 2,
            session: Some("session-y".to_string()),
            tasks: (0..9999).map(|i| sample_task(&format!("s{i}"))).collect(),
        });
    }
    assert!(
        tasks_count(Some("session-y"), false) < 9999,
        "the count path must refetch a stale entry rather than report its size"
    );
}

/// `list_sync` must actually read the task directory from disk.
///
/// The pipeline tests inject statuses straight into `build_tasks`, which is
/// the right seam for the mapping rules but leaves the directory scan itself
/// untested: making `list_sync` return an empty vec failed no test, and it is
/// the only thing standing between a running task and an empty panel.
#[test]
fn list_sync_reads_status_files_from_the_task_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    assert!(
        manager.list_sync().is_empty(),
        "an empty directory yields no tasks"
    );

    for (id, session) in [("111111aaaa", "session-a"), ("222222bbbb", "session-b")] {
        let status = crate::background::TaskStatusFile {
            task_id: id.to_string(),
            tool_name: "bash".to_string(),
            display_name: Some("cargo test".to_string()),
            session_id: session.to_string(),
            status: crate::bus::BackgroundTaskStatus::Completed,
            exit_code: Some(0),
            error: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
            duration_secs: Some(3.0),
            pid: None,
            owner_pid: None,
            owner_instance: None,
            detached: false,
            progress: None,
            notify: true,
            wake: false,
            event_history: Vec::new(),
        };
        std::fs::write(
            tmp.path().join(format!("{id}.status.json")),
            serde_json::to_string(&status).expect("serialize"),
        )
        .expect("write status file");
    }

    // A non-status file in the same directory must be ignored, not parsed.
    // Several lines, so the tail's line budget is actually exercised: capping
    // it to one line passed the whole suite when this file held one line.
    let captured: String = (1..=12).map(|i| format!("tick-{i:02}\n")).collect();
    std::fs::write(tmp.path().join("111111aaaa.output"), &captured).expect("write output");

    let listed = manager.list_sync();
    assert_eq!(listed.len(), 2, "both status files must be read back");
    let mut ids: Vec<&str> = listed.iter().map(|s| s.task_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["111111aaaa", "222222bbbb"]);

    // And output_sync must read the captured output for a task.
    let output = manager.output_sync("111111aaaa").expect("output file read");
    assert_eq!(
        output.lines().count(),
        12,
        "output_sync must read the whole captured file"
    );
    assert!(
        output.contains("tick-12"),
        "the newest line must be present"
    );
}

/// The output tail must return many lines, and the newest ones.
///
/// Capping the budget to a single line passed the entire suite: no test read a
/// task whose capture had more than a couple of lines. A one-line tail makes
/// the panel useless for watching a build. An earlier version of this test
/// reimplemented the tail logic instead of calling it, so it passed under the
/// same mutation; it now drives `tail_lines` directly.
#[test]
fn the_output_tail_returns_many_lines_and_keeps_the_newest() {
    let captured: String = (1..=40).map(|i| format!("line-{i:02}\n")).collect();

    let tail = tail_lines(&captured, 20);
    assert_eq!(
        tail.len(),
        20,
        "the tail must not collapse to a single line"
    );
    assert_eq!(
        tail.last().map(String::as_str),
        Some("line-40"),
        "the newest line must be last"
    );
    assert_eq!(
        tail.first().map(String::as_str),
        Some("line-21"),
        "the tail must start where the budget begins, not at the file head"
    );

    // The hard cap still applies when a caller asks for more.
    let huge = tail_lines(&captured, 10_000);
    assert!(
        huge.len() <= OUTPUT_TAIL_LINES,
        "the tail must stay bounded regardless of the requested size"
    );

    // Blank lines are dropped so the panel does not waste rows on them.
    let padded = "a\n\n\n   \nb\n";
    assert_eq!(
        tail_lines(padded, 10),
        vec!["a".to_string(), "b".to_string()]
    );
}
