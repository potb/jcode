//! Adapter between the background-task status files on disk and the inline
//! background-task panel renderer.
//!
//! Two concerns live here that the renderer deliberately does not know about:
//!
//! 1. **Where the data comes from.** Tasks are spawned by the server process,
//!    not the TUI, so the in-process task map (`running_rows`) is empty in a
//!    remote client. The only source that works everywhere is the shared
//!    status-file directory, so this reads that.
//! 2. **How often.** Listing the directory stats every file, and the TUI
//!    redraws far more often than tasks change state, so the snapshot is
//!    cached and refreshed on a timer.

use jcode_tui_render::background_gallery::{BgStatus, BgTask};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How stale the task snapshot is allowed to get. Tasks change state on human
/// timescales, so a quarter second is imperceptible while cutting directory
/// scans from once per frame to a handful per second.
const REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// How many trailing output lines are kept for the selected task. The strip
/// shows at most a handful; the page can show more.
const OUTPUT_TAIL_LINES: usize = 64;

/// Only tasks that finished recently stay in the panel. Without this the list
/// is dominated by weeks of history, which buries the running work the panel
/// exists to show. The status files themselves remain the archive; the page's
/// "all sessions" toggle plus the status files themselves cover it now.
const FINISHED_RETENTION: Duration = Duration::from_secs(30 * 60);

/// Process-global, not thread-local.
///
/// The TUI event loop is an async `select!` on a multi-threaded tokio runtime,
/// so it migrates between worker threads at await points. With a thread-local
/// cache each worker kept its own copy: the refresh timer effectively reset
/// whenever work was stolen, and worse, `invalidate_cache()` on the thread that
/// handled a key press did not clear the copy the next render happened to read.
/// Pressing the focus chord right after starting a task could then still show
/// the pre-task snapshot.
fn cache() -> &'static Mutex<Option<Snapshot>> {
    static CACHE: OnceLock<Mutex<Option<Snapshot>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

struct Snapshot {
    fetched_at: Instant,
    /// Session the snapshot was mapped for. `is_current_session` is baked into
    /// each task, so a snapshot taken for another session is not reusable.
    session: Option<String>,
    tasks: Vec<BgTask>,
    /// A background refresh is already in flight for this snapshot.
    ///
    /// Without this, every frame during the refresh window would spawn another
    /// scanning thread, which is worse than the synchronous version it
    /// replaced. An explicitly invalidated cache is `None`, not a stale
    /// snapshot, so [`invalidate_cache`] still forces a synchronous refetch and
    /// keeps the "acts immediately after starting a task" guarantee.
    refreshing: bool,
}

impl Snapshot {
    /// Whether this snapshot can be served for `current_session`.
    ///
    /// Both read paths need this rule, and both had it inline and duplicated.
    /// Sharing it means the session scoping cannot drift between them, and it
    /// makes the rule testable without touching the process-global cache.
    fn reusable_for(&self, current_session: Option<&str>) -> bool {
        self.fetched_at.elapsed() < REFRESH_INTERVAL && self.session.as_deref() == current_session
    }

    /// Count matching tasks without cloning them.
    ///
    /// Visibility and selection clamping run several times per frame, and each
    /// `BgTask` carries a label plus an output tail, so cloning the vector to
    /// call `.len()` would copy megabytes per frame for nothing. Split out so
    /// the no-clone property is testable without the process-global cache.
    fn count(&self, only_current_session: bool) -> usize {
        if only_current_session {
            self.tasks
                .iter()
                .filter(|task| task.is_current_session)
                .count()
        } else {
            self.tasks.len()
        }
    }
}

/// Map a persisted status file into the renderer's view model.
fn to_bg_task(
    manager: &crate::background::BackgroundTaskManager,
    status: &crate::background::TaskStatusFile,
    current_session: Option<&str>,
) -> BgTask {
    use crate::bus::BackgroundTaskStatus as Raw;

    let live = manager.task_looks_live(status);
    let mapped = match status.status {
        // A `Running` file whose owning process is gone is a phantom, not
        // work in progress. Naming it keeps stale entries from being counted
        // as running forever.
        Raw::Running if live => BgStatus::Running,
        Raw::Running => BgStatus::Orphaned,
        Raw::Completed => BgStatus::Completed,
        Raw::Failed => BgStatus::Failed,
        Raw::Superseded => BgStatus::Superseded,
    };

    let label = crate::message::background_task_display_label(
        &status.tool_name,
        status.display_name.as_deref(),
    );

    BgTask {
        id: status.task_id.clone(),
        label,
        command: status.command.clone(),
        tool: status.tool_name.clone(),
        status: mapped,
        exit_code: status.exit_code,
        elapsed_secs: elapsed_secs(status),
        progress: status
            .progress
            .as_ref()
            .map(|progress| crate::background::format_progress_display(progress, 14)),
        error: status.error.clone(),
        // Filled in later, and only for the selected task.
        output_tail: Vec::new(),
        session_id: status.session_id.clone(),
        is_current_session: current_session
            .map(|id| id == status.session_id)
            .unwrap_or(false),
    }
}

fn elapsed_secs(status: &crate::background::TaskStatusFile) -> Option<f64> {
    if let Some(duration) = status.duration_secs {
        return Some(duration);
    }
    let started = chrono::DateTime::parse_from_rfc3339(&status.started_at).ok()?;
    (chrono::Utc::now() - started.with_timezone(&chrono::Utc))
        .to_std()
        .ok()
        .map(|duration| duration.as_secs_f64())
}

/// How far back the panel is willing to read status files from disk.
///
/// [`FINISHED_RETENTION`] is the display rule; this is the I/O bound that keeps
/// the render thread from paying for the whole archive to apply it. It is
/// deliberately much larger than the retention window: a *running* task is kept
/// regardless of age (see [`finished_recently`]), and its status file's mtime
/// only advances when the task writes progress, so a tight cutoff could hide a
/// quiet long-running task. At this width a task would have to be silent for
/// twelve hours to be missed, while the common case (an archive of hundreds of
/// long-finished tasks) is skipped without being opened.
const MAX_STATUS_FILE_AGE: Duration = Duration::from_secs(12 * 60 * 60);

fn finished_recently(status: &crate::background::TaskStatusFile) -> bool {
    let Some(completed) = status.completed_at.as_deref() else {
        return true;
    };
    let Ok(completed) = chrono::DateTime::parse_from_rfc3339(completed) else {
        return true;
    };
    (chrono::Utc::now() - completed.with_timezone(&chrono::Utc))
        .to_std()
        .map(|age| age < FINISHED_RETENTION)
        .unwrap_or(true)
}

/// Read the task directory and build the panel's view model, newest first.
///
/// Ordering is by recorded start time, never by task id: ids carry only the
/// last six digits of a millisecond timestamp, so they wrap about every 17
/// minutes and sort wrong across the wrap.
fn fetch(current_session: Option<&str>) -> Vec<BgTask> {
    let manager = crate::background::global();
    build_tasks(
        manager,
        manager.list_sync_modified_within(Some(MAX_STATUS_FILE_AGE)),
        current_session,
    )
}

/// The pure half of [`fetch`]: retention, ordering, and mapping.
///
/// Split out so the pipeline can be tested against an explicit list of status
/// files. Testing only the `finished_recently` predicate in isolation was
/// vacuous: deleting the `retain` call left that test green, because nothing
/// asserted the filter was actually applied here.
fn build_tasks(
    manager: &crate::background::BackgroundTaskManager,
    mut statuses: Vec<crate::background::TaskStatusFile>,
    current_session: Option<&str>,
) -> Vec<BgTask> {
    statuses.retain(finished_recently);
    statuses.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    statuses
        .iter()
        .map(|status| to_bg_task(manager, status, current_session))
        .collect()
}

/// Cached task snapshot, refreshed at most every [`REFRESH_INTERVAL`].
pub(crate) fn tasks_snapshot(current_session: Option<&str>) -> Vec<BgTask> {
    with_fresh_snapshot(current_session, |snapshot| snapshot.tasks.clone())
}

/// Refresh the cache if needed, then project the live snapshot.
///
/// Both read paths need exactly this: lock, decide reuse, refetch on miss,
/// then read. Having it written twice meant the refresh half was duplicated
/// logic that no test could reach without the process-global cache, and the
/// two copies could drift apart. The projection is the only real difference.
fn with_fresh_snapshot<T>(current_session: Option<&str>, project: impl FnOnce(&Snapshot) -> T) -> T
where
    T: Default,
{
    let mut guard = match cache().lock() {
        Ok(guard) => guard,
        // A panicked writer must not take the panel down with it; the worst
        // case is one extra directory scan.
        Err(poisoned) => poisoned.into_inner(),
    };
    // The session scope is part of the mapping (`is_current_session`), so a
    // snapshot taken for a different session cannot be reused.
    let reusable = guard
        .as_ref()
        .is_some_and(|snapshot| snapshot.reusable_for(current_session));
    if !reusable {
        // A stale snapshot is refreshed off-thread. `fetch` opens and parses
        // every status file in the task directory, which is an unbounded
        // archive: this call sits on the TUI render thread, so paying for it
        // inline turned one frame in four into a ~20ms stall (visible in
        // `draw-stats` as a periodic spike on an otherwise ~3ms frame, with
        // `changed_cells: 0`).
        //
        // Serving the previous snapshot for one more refresh interval is the
        // same tradeoff the panel already makes by caching at all: task state
        // changes on human timescales, and the alternative is a stall the user
        // can see. Only the very first read has nothing to serve, and it
        // populates synchronously so the panel is never briefly empty.
        match guard.as_mut() {
            // Same session, merely stale: the old tasks are still the right
            // tasks, just possibly a quarter second out of date.
            Some(snapshot)
                if snapshot.session.as_deref() == current_session && !snapshot.refreshing =>
            {
                snapshot.refreshing = true;
                let session = current_session.map(str::to_string);
                std::thread::spawn(move || {
                    let tasks = fetch(session.as_deref());
                    let mut guard = match cache().lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    *guard = Some(Snapshot {
                        fetched_at: Instant::now(),
                        session,
                        tasks,
                        refreshing: false,
                    });
                });
            }
            Some(snapshot) if snapshot.session.as_deref() == current_session => {}
            // No snapshot, or one mapped for a different session. Serving it
            // would show another session's tasks (`is_current_session` is baked
            // into each row), so this one has to be paid for inline.
            _ => {
                *guard = Some(Snapshot {
                    fetched_at: Instant::now(),
                    session: current_session.map(str::to_string),
                    tasks: fetch(current_session),
                    refreshing: false,
                });
            }
        }
    }
    guard.as_ref().map(project).unwrap_or_default()
}

/// Number of cached tasks matching the session scope, without cloning them.
///
/// Visibility and selection clamping are checked several times per frame;
/// cloning the whole task vector (each carrying its label and output tail) just
/// to call `.len()` is pure waste on the render path.
pub(crate) fn tasks_count(current_session: Option<&str>, only_current_session: bool) -> usize {
    with_fresh_snapshot(current_session, |snapshot| {
        snapshot.count(only_current_session)
    })
}

/// Drop the cache so the next read hits disk. Used after an action that is
/// expected to change task state, where waiting out the refresh interval would
/// feel unresponsive.
pub(crate) fn invalidate_cache() {
    match cache().lock() {
        Ok(mut guard) => *guard = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

/// Read the tail of one task's captured stdout/stderr.
///
/// Only ever called for the selected task: reading every task's output on
/// every frame would turn a status panel into a disk hog.
pub(crate) fn output_tail(task_id: &str, max_lines: usize) -> Vec<String> {
    // Read from the end rather than the whole file. A task's output is
    // unbounded (this panel exists to watch builds), but the panel only ever
    // shows `OUTPUT_TAIL_LINES` trailing lines, so the read is capped at a
    // generous byte budget for that many lines instead of the file's size.
    let Some(output) =
        crate::background::global().output_tail_sync(task_id, OUTPUT_TAIL_READ_BYTES)
    else {
        return Vec::new();
    };
    tail_lines(&output, max_lines)
}

/// Byte budget for the tail read: enough that `OUTPUT_TAIL_LINES` full-width
/// terminal lines fit comfortably, with room for the leading partial line that
/// a byte-aligned cut leaves behind.
const OUTPUT_TAIL_READ_BYTES: u64 = 64 * 1024;

/// The tail rule, split from the disk read so it is testable.
///
/// Keeping it inside `output_tail` meant the only way to check it was to
/// reimplement it in the test, which proves nothing: capping the budget to a
/// single line passed the whole suite that way.
fn tail_lines(output: &str, max_lines: usize) -> Vec<String> {
    let max_lines = max_lines.min(OUTPUT_TAIL_LINES);
    let mut lines: Vec<String> = output
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .map(crate::message::strip_ansi_escape_sequences)
        .collect();
    lines.reverse();
    lines
}

#[cfg(test)]
mod tests;
