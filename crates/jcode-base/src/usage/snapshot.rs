//! Cross-process usage snapshot, shared through `~/.jcode/state/usage.json`.
//!
//! Every jcode process used to poll Anthropic's OAuth usage endpoint on its
//! own five-minute timer. With a server plus several clients attached that is
//! N independent pollers against one burst limiter, which answers `429 Too
//! Many Requests` and takes the usage readouts down for *everyone*. Worse, the
//! staleness rule also fires the instant a window's reset timestamp passes, so
//! all processes decide to refetch at the same moment: a synchronized burst,
//! which is exactly what the endpoint punishes.
//!
//! This module gives the processes a place to see each other's work. A
//! successful refresh is written here; a process about to fetch looks here
//! first and adopts a snapshot that is still fresh instead of making its own
//! request. One fetch per `CACHE_DURATION` per machine, rather than one per
//! process, without any protocol change and without a process needing to be
//! connected to the server (`menubar` and one-shot CLI paths have no socket).
//!
//! Deliberately narrow:
//!
//! - Only *successful* fetches are published. Publishing failures would let a
//!   single process with a broken token or a lost network silence refreshes
//!   for every other process on the machine.
//! - The snapshot records which account it describes and is ignored when the
//!   reader's active account differs, so account A never sees B's quota.
//! - `Instant` is process-local and meaningless in a file, so the wall-clock
//!   fetch time is persisted and the reader reconstructs the age on load.

use super::UsageData;
use super::model::ModelScopedUsageWindow;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Snapshot format version. A reader that does not recognise the version
/// ignores the file rather than guessing at its shape; the worst case is one
/// extra fetch, whereas misreading it would report the wrong quota.
const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredWindow {
    model_name: String,
    utilization: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSnapshot {
    version: u32,
    /// Account this snapshot describes, so it is not shown to another account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_label: Option<String>,
    /// Wall-clock fetch time in milliseconds since the Unix epoch.
    fetched_at_ms: i64,
    five_hour: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    five_hour_resets_at: Option<String>,
    seven_day: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seven_day_resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seven_day_opus: Option<f32>,
    #[serde(default)]
    model_scoped: Vec<StoredWindow>,
    #[serde(default)]
    extra_usage_enabled: bool,
}

fn snapshot_path() -> PathBuf {
    jcode_storage::durable_state_dir().join("usage.json")
}

/// Publish a successful Anthropic usage fetch for the other processes.
///
/// Failures to write are swallowed: a machine where the state directory is
/// read-only should keep working exactly as it did before this file existed,
/// just without cross-process sharing.
pub(super) fn publish(data: &UsageData, account_label: Option<&str>) {
    publish_to(&snapshot_path(), data, account_label);
}

/// Path-explicit form, so the behaviour can be exercised against a temporary
/// directory instead of the real `~/.jcode/state/usage.json` that other
/// processes are actively using.
fn publish_to(path: &PathBuf, data: &UsageData, account_label: Option<&str>) {
    // Never publish an error state, and never publish a snapshot with nothing
    // in it: an empty snapshot is indistinguishable from a pristine full
    // window, so a reader would treat "we know nothing" as "0% used".
    if data.last_error.is_some() || !data.has_known_windows() {
        return;
    }

    let stored = StoredSnapshot {
        version: SNAPSHOT_VERSION,
        account_label: account_label.map(str::to_string),
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        five_hour: data.five_hour,
        five_hour_resets_at: data.five_hour_resets_at.clone(),
        seven_day: data.seven_day,
        seven_day_resets_at: data.seven_day_resets_at.clone(),
        seven_day_opus: data.seven_day_opus,
        model_scoped: data
            .model_scoped
            .iter()
            .map(|window| StoredWindow {
                model_name: window.model_name.clone(),
                utilization: window.utilization,
                resets_at: window.resets_at.clone(),
            })
            .collect(),
        extra_usage_enabled: data.extra_usage_enabled,
    };

    let Ok(json) = serde_json::to_string(&stored) else {
        return;
    };
    write_atomic(path, &json);
}

/// Write through a temporary file in the same directory, then rename.
///
/// Several processes publish to this path concurrently. A plain truncating
/// write would let a reader observe a half-written file and discard a
/// perfectly good snapshot (or, worse, parse a truncated one); `rename` is
/// atomic on the same filesystem, so a reader always sees one whole version.
fn write_atomic(path: &PathBuf, contents: &str) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // The pid keeps concurrent publishers from clobbering each other's
    // temporary file, which would otherwise produce exactly the torn write
    // this function exists to prevent.
    let tmp = dir.join(format!("usage.json.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, contents).is_err() {
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Load the shared snapshot, if one exists and describes `account_label`.
///
/// Returns the data with `fetched_at` reconstructed from the recorded wall
/// clock, so the caller's ordinary staleness rules apply unchanged: a snapshot
/// older than `CACHE_DURATION`, or one whose window has since reset, is stale
/// here for exactly the same reasons it would be in memory.
pub(super) fn load(account_label: Option<&str>) -> Option<UsageData> {
    load_from(&snapshot_path(), account_label)
}

/// Path-explicit form; see [`publish_to`].
fn load_from(path: &PathBuf, account_label: Option<&str>) -> Option<UsageData> {
    let raw = std::fs::read_to_string(path).ok()?;
    let stored: StoredSnapshot = serde_json::from_str(&raw).ok()?;
    if stored.version != SNAPSHOT_VERSION {
        return None;
    }

    // Only compare when both sides know an account. An unlabeled snapshot is
    // from a process that could not resolve one, and is better than nothing.
    if let (Some(want), Some(have)) = (account_label, stored.account_label.as_deref())
        && want != have
    {
        return None;
    }

    let age = age_from_wall_clock(stored.fetched_at_ms)?;
    // `Instant` has no representable past before process start, so a snapshot
    // older than this process's uptime cannot be expressed as an `Instant`.
    // Treat that as absent rather than clamping to `now`, which would present
    // arbitrarily old data as freshly fetched.
    let fetched_at = Instant::now().checked_sub(age)?;

    Some(UsageData {
        five_hour: stored.five_hour,
        five_hour_resets_at: stored.five_hour_resets_at,
        seven_day: stored.seven_day,
        seven_day_resets_at: stored.seven_day_resets_at,
        seven_day_opus: stored.seven_day_opus,
        model_scoped: stored
            .model_scoped
            .into_iter()
            .map(|window| ModelScopedUsageWindow {
                model_name: window.model_name,
                utilization: window.utilization,
                resets_at: window.resets_at,
            })
            .collect(),
        extra_usage_enabled: stored.extra_usage_enabled,
        fetched_at: Some(fetched_at),
        last_error: None,
        retry_after: None,
    })
}

/// Age of a wall-clock timestamp, or `None` when it is in the future.
///
/// A future timestamp means the clock moved backwards (NTP correction,
/// suspend/resume, a snapshot written by a machine with a skewed clock on a
/// shared home directory). Rejecting it is the safe answer: pretending it was
/// fetched now would pin a bogus snapshot in place for a full cache duration.
fn age_from_wall_clock(fetched_at_ms: i64) -> Option<Duration> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let delta = now_ms.checked_sub(fetched_at_ms)?;
    if delta < 0 {
        return None;
    }
    Some(Duration::from_millis(delta as u64))
}

/// A shared snapshot worth adopting instead of making a network request.
///
/// This is the whole point of the module: the check runs on the refresh path,
/// so a process that has decided its own copy is stale still makes no request
/// when another process already fetched recently.
pub(super) fn fresh_shared_snapshot(account_label: Option<&str>) -> Option<UsageData> {
    let data = load(account_label)?;
    if data.is_stale() { None } else { Some(data) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UsageData {
        UsageData {
            five_hour: 0.42,
            five_hour_resets_at: Some("2999-01-01T00:00:00Z".to_string()),
            seven_day: 0.11,
            seven_day_resets_at: Some("2999-01-02T00:00:00Z".to_string()),
            seven_day_opus: Some(0.5),
            model_scoped: vec![ModelScopedUsageWindow {
                model_name: "Fable".to_string(),
                utilization: 0.25,
                resets_at: Some("2999-01-03T00:00:00Z".to_string()),
            }],
            extra_usage_enabled: true,
            fetched_at: Some(Instant::now()),
            last_error: None,
            retry_after: None,
        }
    }

    /// A temporary file, so tests never touch the real
    /// `~/.jcode/state/usage.json` that other processes are using for real.
    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jcode-usage-snapshot-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("usage.json")
    }

    #[test]
    fn every_window_survives_the_round_trip() {
        let path = temp_path("roundtrip");
        let original = sample();
        publish_to(&path, &original, Some("primary"));

        let restored = load_from(&path, Some("primary")).expect("restored");
        assert_eq!(restored.five_hour, original.five_hour);
        assert_eq!(restored.seven_day, original.seven_day);
        assert_eq!(restored.seven_day_opus, original.seven_day_opus);
        assert_eq!(restored.five_hour_resets_at, original.five_hour_resets_at);
        assert_eq!(restored.seven_day_resets_at, original.seven_day_resets_at);
        assert_eq!(restored.model_scoped, original.model_scoped);
        assert!(restored.extra_usage_enabled);
        // The reconstructed age is what makes the ordinary staleness rules
        // apply to a snapshot that came from another process.
        assert!(restored.fetched_at.is_some());
        assert!(!restored.is_stale());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_snapshot_from_another_account_is_ignored() {
        let path = temp_path("account");
        publish_to(&path, &sample(), Some("work"));

        assert!(
            load_from(&path, Some("personal")).is_none(),
            "another account's quota must never be adopted"
        );
        assert!(
            load_from(&path, Some("work")).is_some(),
            "the same account must be adopted"
        );
        assert!(
            load_from(&path, None).is_some(),
            "a caller that cannot resolve an account still reads the snapshot"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_error_state_is_never_written_to_disk() {
        // One process with a bad token or a lost network must not be able to
        // silence refreshes for every other process on the machine.
        let path = temp_path("error");
        let _ = std::fs::remove_file(&path);
        let mut data = sample();
        data.last_error = Some("Usage API error (429 Too Many Requests)".to_string());
        publish_to(&path, &data, Some("primary"));
        assert!(!path.exists(), "a failed refresh must not be published");
    }

    #[test]
    fn an_empty_snapshot_is_never_written_to_disk() {
        // A zeroed snapshot is byte-identical to a pristine full window, so
        // publishing it would report "0% used" as if it had been measured.
        let path = temp_path("empty");
        let _ = std::fs::remove_file(&path);
        publish_to(&path, &UsageData::default(), Some("primary"));
        assert!(!path.exists(), "an empty snapshot must not be published");
    }

    #[test]
    fn a_publish_replaces_the_previous_snapshot() {
        // The atomic rename must actually overwrite, not append or fail.
        let path = temp_path("replace");
        let mut first = sample();
        first.five_hour = 0.10;
        publish_to(&path, &first, Some("primary"));
        let mut second = sample();
        second.five_hour = 0.80;
        publish_to(&path, &second, Some("primary"));

        let restored = load_from(&path, Some("primary")).expect("restored");
        assert!((restored.five_hour - 0.80).abs() < f32::EPSILON);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_file_is_ignored_rather_than_fatal() {
        // A truncated or hand-edited file must degrade to "no shared
        // snapshot", which costs one fetch, not a panic.
        let path = temp_path("corrupt");
        std::fs::write(&path, "{not json").expect("write");
        assert!(load_from(&path, Some("primary")).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_is_ignored() {
        let path = temp_path("missing").with_file_name("nope.json");
        assert!(load_from(&path, Some("primary")).is_none());
    }

    #[test]
    fn a_fresh_shared_snapshot_is_adopted_but_a_stale_one_is_not() {
        let path = temp_path("freshness");
        publish_to(&path, &sample(), Some("primary"));
        let fresh = load_from(&path, Some("primary")).expect("restored");
        assert!(!fresh.is_stale(), "a just-written snapshot is adoptable");

        // Rewrite with an old wall clock: the reader must fall through to a
        // real fetch, otherwise the shared file would freeze usage forever.
        let raw = std::fs::read_to_string(&path).expect("read");
        let mut stored: StoredSnapshot = serde_json::from_str(&raw).expect("parse");
        stored.fetched_at_ms -= 600_000;
        std::fs::write(&path, serde_json::to_string(&stored).expect("serialize")).expect("write");

        let old = load_from(&path, Some("primary")).expect("restored");
        assert!(
            old.is_stale(),
            "a 10-minute-old snapshot must not be reused"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_future_timestamp_is_rejected_rather_than_clamped() {
        let future = chrono::Utc::now().timestamp_millis() + 60_000;
        assert!(
            age_from_wall_clock(future).is_none(),
            "a backwards clock must not pin a bogus snapshot in place"
        );
    }

    #[test]
    fn an_old_timestamp_reports_its_real_age() {
        let then = chrono::Utc::now().timestamp_millis() - 120_000;
        let age = age_from_wall_clock(then).expect("age");
        assert!(age.as_secs() >= 119 && age.as_secs() <= 125, "{age:?}");
    }

    #[test]
    fn a_snapshot_whose_window_reset_is_stale() {
        // `is_stale` also fires on a passed reset timestamp. Carrying that
        // rule over is what stops the shared file from serving quota from
        // before a window rollover.
        let mut data = sample();
        data.five_hour_resets_at = Some("2000-01-01T00:00:00Z".to_string());
        assert!(data.is_stale());
    }

    #[test]
    fn an_unknown_version_is_ignored() {
        let json = r#"{"version":99,"fetched_at_ms":0,"five_hour":0.5,"seven_day":0.5}"#;
        let stored: StoredSnapshot = serde_json::from_str(json).expect("parse");
        assert_ne!(stored.version, SNAPSHOT_VERSION);
    }

    #[test]
    fn missing_optional_fields_still_parse() {
        // Forward compatibility with a snapshot written before a field
        // existed: the reader must not throw the whole file away.
        let json = r#"{"version":1,"fetched_at_ms":0,"five_hour":0.5,"seven_day":0.25}"#;
        let stored: StoredSnapshot = serde_json::from_str(json).expect("parse");
        assert_eq!(stored.five_hour, 0.5);
        assert!(stored.model_scoped.is_empty());
        assert!(!stored.extra_usage_enabled);
        assert!(stored.account_label.is_none());
    }
}
