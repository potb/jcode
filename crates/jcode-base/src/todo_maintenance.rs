//! Retention sweep for the on-disk todo store.
//!
//! Every session that touches the `todo` tool leaves up to five files in
//! `~/.jcode/todos/`: `<id>.json`, plus `-goals`, `-plan`, `-review-state` and
//! `-gate-observations` siblings. Nothing ever removed them. Ambient makes this
//! visible fastest — one cycle per wake, each a fresh session id — but it is not
//! ambient-specific: short-lived `jcode run` invocations and swarm workers leak
//! the same way, and issue #22 lists the accumulation as a secondary problem.
//!
//! The files are only ever read by the session that owns them (keyed by session
//! id), so once that session is long gone its todos are unreachable state. This
//! prunes them by mtime, mirroring [`crate::session::maintenance`]: a
//! conservative window, a machine-wide interval marker so a burst of spawns does
//! not each walk the directory, and best-effort I/O so a failure can never
//! affect startup.

use crate::storage;
use chrono::{DateTime, Duration, Local};
use std::path::Path;

/// Todo artifacts untouched for this long belong to sessions that are over.
///
/// Deliberately generous: a resumed session rewrites its todo file on the next
/// `todo` call, so the only cost of pruning too eagerly would be losing the plan
/// of a session someone comes back to after a month away.
const TODO_RETENTION_DAYS: i64 = 30;

/// Minimum interval between sweeps across all jcode processes.
const PRUNE_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Remove todo artifacts belonging to long-dead sessions.
///
/// Best-effort and rate-limited; safe to call unconditionally at startup.
pub fn prune_old_todo_artifacts() {
    if let Ok(base) = storage::jcode_dir() {
        if !claim_prune_slot(&base) {
            return;
        }
        prune_old_todo_artifacts_in(&base.join("todos"), Local::now());
    }
}

/// Returns true when this process should sweep now, claiming the slot for the
/// interval. The marker is touched before the walk so simultaneous spawns
/// resolve to at most a couple of walkers instead of one per process.
fn claim_prune_slot(base: &Path) -> bool {
    let marker = base.join("todos-prune.stamp");
    if let Ok(metadata) = std::fs::metadata(&marker)
        && let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
        && age.as_secs() < PRUNE_INTERVAL_SECS
    {
        return false;
    }
    std::fs::write(&marker, b"").is_ok()
}

/// True for the file names the todo store owns.
///
/// Checked by extension rather than by matching the session-id prefix, because
/// the sweep must not depend on the id format staying stable. Everything the
/// todo store writes is `.json`; the interval marker deliberately lives in the
/// parent directory and is not `.json`, so it cannot sweep away its own stamp.
fn is_todo_artifact(path: &Path) -> bool {
    path.extension().map(|e| e == "json").unwrap_or(false)
}

/// Core of [`prune_old_todo_artifacts`], parameterized on directory and "now"
/// so the retention boundary can be tested without waiting a month.
fn prune_old_todo_artifacts_in(todos_dir: &Path, now: DateTime<Local>) -> usize {
    let Ok(entries) = std::fs::read_dir(todos_dir) else {
        return 0;
    };
    let cutoff = now - Duration::days(TODO_RETENTION_DAYS);
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_todo_artifact(&path)
            && let Ok(metadata) = entry.metadata()
            && metadata.is_file()
            && let Ok(modified) = metadata.modified()
        {
            let modified: DateTime<Local> = modified.into();
            if modified < cutoff && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{Duration as StdDuration, SystemTime};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jcode-todo-prune-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_aged(dir: &Path, name: &str, age_days: u64) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).expect("create");
        f.write_all(b"[]").ok();
        if age_days > 0 {
            let mtime = SystemTime::now() - StdDuration::from_secs(age_days * 24 * 60 * 60);
            f.set_modified(mtime).expect("set mtime");
        }
        path
    }

    #[test]
    fn prunes_every_artifact_kind_of_a_long_dead_session() {
        let dir = temp_dir("kinds");
        let files: Vec<_> = [
            "session_old.json",
            "session_old-goals.json",
            "session_old-plan.json",
            "session_old-review-state.json",
            "session_old-gate-observations.json",
        ]
        .iter()
        .map(|name| write_aged(&dir, name, 60))
        .collect();

        assert_eq!(prune_old_todo_artifacts_in(&dir, Local::now()), files.len());
        for path in files {
            assert!(!path.exists(), "{} should be pruned", path.display());
        }

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keeps_artifacts_inside_the_retention_window() {
        let dir = temp_dir("window");
        // A concrete age, not `TODO_RETENTION_DAYS - 1`: expressing it in terms
        // of the constant would make the test follow the window wherever it
        // moved, so shortening retention to a day would still "pass".
        let recent = write_aged(&dir, "session_recent.json", 29);
        let live = write_aged(&dir, "session_live-plan.json", 0);

        assert_eq!(prune_old_todo_artifacts_in(&dir, Local::now()), 0);
        assert!(recent.exists(), "in-window artifact must survive");
        assert!(live.exists(), "live artifact must survive");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leaves_non_json_files_alone() {
        let dir = temp_dir("scope");
        let stamp = write_aged(&dir, "todos-prune.stamp", 60);
        let other = write_aged(&dir, "notes.txt", 60);

        assert_eq!(prune_old_todo_artifacts_in(&dir, Local::now()), 0);
        assert!(stamp.exists(), "a stray stamp file is out of scope");
        assert!(other.exists(), "unrelated files are out of scope");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn claim_prune_slot_rate_limits_within_interval_and_reclaims_after() {
        let dir = temp_dir("claim");

        assert!(claim_prune_slot(&dir), "first claim should win");
        let marker = dir.join("todos-prune.stamp");
        assert!(marker.exists(), "marker should be created");
        assert!(
            !claim_prune_slot(&dir),
            "second claim within interval should be skipped"
        );

        let old = SystemTime::now() - StdDuration::from_secs(PRUNE_INTERVAL_SECS + 60);
        File::options()
            .write(true)
            .open(&marker)
            .and_then(|f| f.set_modified(old))
            .expect("age the marker");
        assert!(
            claim_prune_slot(&dir),
            "claim should succeed after the interval elapses"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_todos_directory_is_not_an_error() {
        let dir = temp_dir("missing");
        let absent = dir.join("nope");
        assert_eq!(prune_old_todo_artifacts_in(&absent, Local::now()), 0);
        fs::remove_dir_all(&dir).ok();
    }
}
