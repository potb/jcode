//! Server-to-client usage push (issue #24).
//!
//! [`super::poller`] made the daemon the process that normally performs the
//! usage fetch, and [`super::snapshot`] lets other processes adopt its result
//! off disk. That still leaves the clients *polling*: a connected client whose
//! cache goes stale runs the whole staleness path again, and a client that
//! cannot adopt a fresh snapshot (because the disk copy is stale at a window
//! rollover, which is exactly when every process wakes up) goes on to contend
//! for the fetch lease. The push closes that last gap: a connected client is
//! told when a new snapshot exists and never has to look for one.
//!
//! Two things make this safe to bolt onto the existing cache:
//!
//! - **The wire type carries wall-clock time, not `Instant`.** `UsageData`
//!   records `fetched_at: Option<Instant>`, which is meaningless in another
//!   process (its zero point is that process's boot). The pushed snapshot
//!   carries `fetched_at_ms` and the receiver rebuilds a local `Instant`, so the
//!   adopted data ages out on the receiver's own clock exactly as a locally
//!   fetched one would.
//! - **Only successful fetches are pushed.** The wire type has no error field at
//!   all, so a failing server cannot overwrite a client's stale-but-useful
//!   numbers with a blank snapshot. This mirrors the on-disk rule.

use super::UsageData;
use super::model::ModelScopedUsageWindow;
use jcode_protocol::{UsageSnapshot, UsageSnapshotWindow};
use std::time::{Duration, Instant};

/// Whether a process should refresh usage itself.
///
/// Pure, and separate from the `PUSH_FED` latch that feeds it, because that
/// latch is a process-global that a test cannot set without changing the
/// behaviour of every other test sharing the binary. The rule is what matters
/// and it is pinned below.
pub(super) fn should_self_refresh(push_fed: bool) -> bool {
    !push_fed
}

/// Whether a pushed snapshot describes the account this process is using.
///
/// A snapshot with no label is accepted: the label only exists to catch a
/// *mismatch*, and refusing unlabelled snapshots would mean a server that could
/// not resolve its own account label silences the client's usage readout
/// entirely. Pure and separate from the accessor for the same reason as
/// [`should_self_refresh`]: the active account is process-global state.
pub(super) fn snapshot_matches_account(pushed: Option<&str>, active: &str) -> bool {
    match pushed {
        Some(label) => label == active,
        None => true,
    }
}

/// Build a pushable snapshot from in-process usage data.
///
/// Returns `None` when the data must not be pushed: an error state, or a
/// snapshot with no known windows. An all-zero snapshot is indistinguishable
/// from a pristine, fully unused quota, so pushing one would make every
/// receiving client render "0% used" as if it were fact.
pub fn snapshot_from_usage(data: &UsageData, account_label: Option<&str>) -> Option<UsageSnapshot> {
    if data.last_error.is_some() || !data.has_known_windows() {
        return None;
    }

    Some(UsageSnapshot {
        account_label: account_label.map(str::to_string),
        // Wall clock, because the receiver's `Instant` epoch differs from ours.
        // Derived from the age of our own `fetched_at` rather than "now", so a
        // snapshot that was already a few minutes old when we pushed it is not
        // advertised to the client as brand new.
        fetched_at_ms: fetched_at_ms(data),
        five_hour: data.five_hour,
        five_hour_resets_at: data.five_hour_resets_at.clone(),
        seven_day: data.seven_day,
        seven_day_resets_at: data.seven_day_resets_at.clone(),
        seven_day_opus: data.seven_day_opus,
        model_scoped: data
            .model_scoped
            .iter()
            .map(|window| UsageSnapshotWindow {
                model_name: window.model_name.clone(),
                utilization: window.utilization,
                resets_at: window.resets_at.clone(),
            })
            .collect(),
        extra_usage_enabled: data.extra_usage_enabled,
    })
}

/// Wall-clock fetch time for `data`, preserving how old it already is.
fn fetched_at_ms(data: &UsageData) -> i64 {
    let now_ms = chrono::Utc::now().timestamp_millis();
    match data.fetched_at {
        Some(fetched_at) => {
            let age_ms = i64::try_from(fetched_at.elapsed().as_millis()).unwrap_or(i64::MAX);
            now_ms.saturating_sub(age_ms)
        }
        // Never fetched locally, so there is no age to preserve.
        None => now_ms,
    }
}

/// Rebuild in-process usage data from a pushed snapshot.
///
/// The wall-clock fetch time is converted back into a local `Instant` so the
/// adopted snapshot ages on the receiver's clock. A snapshot timestamped in the
/// future (clock skew, or a corrected clock) is clamped to "just now" rather
/// than producing an `Instant` this process cannot represent.
pub fn usage_from_snapshot(snapshot: &UsageSnapshot) -> UsageData {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let age = Duration::from_millis(
        u64::try_from(now_ms.saturating_sub(snapshot.fetched_at_ms)).unwrap_or(0),
    );
    let fetched_at = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);

    UsageData {
        five_hour: snapshot.five_hour,
        five_hour_resets_at: snapshot.five_hour_resets_at.clone(),
        seven_day: snapshot.seven_day,
        seven_day_resets_at: snapshot.seven_day_resets_at.clone(),
        seven_day_opus: snapshot.seven_day_opus,
        model_scoped: snapshot
            .model_scoped
            .iter()
            .map(|window| ModelScopedUsageWindow {
                model_name: window.model_name.clone(),
                utilization: window.utilization,
                resets_at: window.resets_at.clone(),
            })
            .collect(),
        extra_usage_enabled: snapshot.extra_usage_enabled,
        fetched_at: Some(fetched_at),
        last_error: None,
        retry_after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated() -> UsageData {
        UsageData {
            five_hour: 0.42,
            five_hour_resets_at: Some("2099-01-01T00:00:00Z".to_string()),
            seven_day: 0.13,
            seven_day_resets_at: Some("2099-01-02T00:00:00Z".to_string()),
            seven_day_opus: Some(0.5),
            model_scoped: vec![ModelScopedUsageWindow {
                model_name: "Fable".to_string(),
                utilization: 0.25,
                resets_at: Some("2099-01-03T00:00:00Z".to_string()),
            }],
            extra_usage_enabled: true,
            fetched_at: Some(Instant::now()),
            last_error: None,
            retry_after: None,
        }
    }

    #[test]
    fn round_trip_preserves_every_window() {
        let snapshot = snapshot_from_usage(&populated(), Some("work")).expect("should be pushable");
        assert_eq!(snapshot.account_label.as_deref(), Some("work"));

        let restored = usage_from_snapshot(&snapshot);
        let original = populated();
        assert_eq!(restored.five_hour, original.five_hour);
        assert_eq!(restored.five_hour_resets_at, original.five_hour_resets_at);
        assert_eq!(restored.seven_day, original.seven_day);
        assert_eq!(restored.seven_day_resets_at, original.seven_day_resets_at);
        assert_eq!(restored.seven_day_opus, original.seven_day_opus);
        assert_eq!(restored.extra_usage_enabled, original.extra_usage_enabled);
        assert_eq!(restored.model_scoped.len(), 1);
        assert_eq!(restored.model_scoped[0].model_name, "Fable");
        assert_eq!(restored.model_scoped[0].utilization, 0.25);
    }

    #[test]
    fn an_error_snapshot_is_never_pushed() {
        let mut data = populated();
        data.last_error = Some("429 Too Many Requests".to_string());
        assert!(
            snapshot_from_usage(&data, None).is_none(),
            "a failed refresh must not overwrite a client's stale-but-useful values"
        );
    }

    #[test]
    fn an_empty_snapshot_is_never_pushed() {
        assert!(
            snapshot_from_usage(&UsageData::default(), None).is_none(),
            "an all-zero snapshot would render as a pristine 0%-used quota"
        );
    }

    #[test]
    fn a_restored_snapshot_carries_no_error_or_retry_state() {
        let snapshot = snapshot_from_usage(&populated(), None).expect("should be pushable");
        let restored = usage_from_snapshot(&snapshot);
        assert!(restored.last_error.is_none());
        assert!(
            restored.retry_after.is_none(),
            "a pushed snapshot is a success, so it must not import backoff state"
        );
    }

    #[test]
    fn existing_age_is_preserved_across_the_wire() {
        // A snapshot the server fetched a while ago must not arrive looking new,
        // or the client would keep serving it past its real lifetime.
        let mut data = populated();
        data.fetched_at = Some(Instant::now() - Duration::from_secs(120));

        let snapshot = snapshot_from_usage(&data, None).expect("should be pushable");
        let restored = usage_from_snapshot(&snapshot);

        let age = restored
            .fetched_at
            .expect("restored snapshot has a fetch time")
            .elapsed();
        assert!(
            age >= Duration::from_secs(115),
            "expected the ~120s age to survive the round trip, got {age:?}"
        );
    }

    #[test]
    fn a_push_fed_process_never_refreshes_for_itself() {
        // The central design point of issue #24: if a client keeps refreshing
        // once it is being pushed to, the push achieves nothing and N clients
        // are back on the burst limiter.
        assert!(
            !should_self_refresh(true),
            "a push-fed process must never fetch usage itself"
        );
        assert!(
            should_self_refresh(false),
            "a process with no push (menubar, one-shot CLI, pre-Subscribe \
             client) still has to refresh for itself"
        );
    }

    #[test]
    fn a_snapshot_for_another_account_is_rejected() {
        // Account labels exist so a client on account B is never shown account
        // A's quota. Adopting the wrong one would misreport headroom and, via
        // the exhaustion checks, could reroute or block requests.
        assert!(
            !snapshot_matches_account(Some("personal"), "work"),
            "a snapshot for another account must never be adopted"
        );
        assert!(snapshot_matches_account(Some("work"), "work"));
    }

    #[test]
    fn an_unlabelled_snapshot_is_still_adopted() {
        // The label catches a mismatch; it is not a requirement. Refusing
        // unlabelled snapshots would let a server that cannot resolve its own
        // account label silence the client's usage readout entirely.
        assert!(snapshot_matches_account(None, "work"));
    }

    #[test]
    fn a_future_timestamp_does_not_panic() {
        // Clock skew between machines, or a clock correction, can put the
        // server's timestamp ahead of ours. `Instant::checked_sub` guards the
        // subtraction; this pins that it stays guarded.
        let snapshot = UsageSnapshot {
            fetched_at_ms: chrono::Utc::now().timestamp_millis() + 60_000,
            five_hour: 0.1,
            ..Default::default()
        };

        let restored = usage_from_snapshot(&snapshot);
        assert!(restored.fetched_at.is_some());
    }
}
