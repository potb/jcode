//! Cross-process fetch lease, shared through `~/.jcode/state/usage-fetch.lease`.
//!
//! The snapshot in [`super::snapshot`] removes *steady-state* duplicate polling:
//! a process whose own copy went stale adopts a still-fresh shared snapshot
//! instead of issuing a request. That leaves the harder half of the problem
//! untouched, and it is the half the original report calls worse.
//!
//! `UsageData::is_stale()` returns true unconditionally once a window's reset
//! timestamp passes — before it ever looks at `fetched_at`. So at a window
//! rollover the shared snapshot is stale for *every* process at the same
//! instant. Every process therefore misses the adopt path and fetches
//! together: a synchronized burst against a limiter that punishes exactly
//! that, which is the `429` storm that takes the usage readout down for
//! everyone. Adopting a stale snapshot instead is not an option — it is stale
//! precisely because the quota numbers in it now describe a window that no
//! longer exists.
//!
//! So the herd is thinned at the other end: one process wins the right to
//! make the request, and the others skip this round and keep serving what they
//! have until the winner publishes. The result is one fetch per machine per
//! reset boundary rather than one per process.
//!
//! Deliberately narrow:
//!
//! - **A lost race is a skip, never a wait.** Blocking would hold a refresh
//!   task open across another process's network request, and a caller that
//!   crashed mid-fetch would stall every other process on the machine. A
//!   skipping process keeps its previous data and retries on its next tick.
//! - **The lease expires on its own.** A holder that is `SIGKILL`ed cannot run
//!   a release, so a lease older than [`LEASE_TTL`] is treated as abandoned and
//!   may be taken over. The TTL is a bound on a single HTTP request, not on the
//!   cache duration: it only has to outlive a fetch.
//! - **Each provider gets its own lease file**, because the two have
//!   independent accounts, endpoints and cache durations. An Anthropic fetch
//!   must never block a Codex one.
//! - **Every failure is treated as "the lease is free".** If the state
//!   directory is read-only or the file is corrupt, the worst case is the
//!   behaviour we already have today (each process fetches), whereas failing
//!   closed would silence usage refreshes machine-wide.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

/// How long a lease stays valid before another process may take it over.
///
/// This bounds one usage HTTP request, not the poll interval: the holder
/// releases as soon as its fetch resolves, so the TTL only matters when a
/// process dies mid-fetch. Long enough that a slow request does not get its
/// lease stolen (which would reintroduce the double fetch), short enough that
/// a crash costs at most one poll cycle.
const LEASE_TTL_MS: i64 = 60_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredLease {
    /// Uniquely identifies one acquisition attempt. A process id alone is not
    /// enough: the same process may acquire, release and re-acquire, and two
    /// machines sharing a home directory can collide on pid.
    token: String,
    acquired_at_ms: i64,
}

/// A held lease. Release is explicit rather than `Drop`-based because the
/// holder is an async task whose fetch may be cancelled; see [`release`].
#[derive(Debug, Clone)]
pub(super) struct FetchLease {
    path: PathBuf,
    token: String,
}

fn lease_path(file_name: &str) -> PathBuf {
    jcode_storage::durable_state_dir().join(file_name)
}

fn anthropic_lease_path() -> PathBuf {
    lease_path("usage-fetch.lease")
}

fn openai_lease_path() -> PathBuf {
    lease_path("usage-fetch-openai.lease")
}

/// A token unique to this acquisition attempt on this machine.
fn mint_token() -> String {
    // Pid distinguishes processes; the nanosecond clock distinguishes repeated
    // attempts by the same process, including two tasks racing inside it.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

/// Shortest gap between two lease *attempts* by this process, per provider.
///
/// A process that loses the race returns immediately and clears its in-flight
/// flag, so the very next staleness check would try again. On the TUI that
/// check runs once per render, which would turn a lost race into a file read
/// every frame for as long as the winner's request takes. Losing is already a
/// skip, so throttling the retry costs nothing: the loser adopts the winner's
/// published snapshot on a later tick either way.
const ATTEMPT_THROTTLE_MS: i64 = 5_000;

static LAST_ANTHROPIC_ATTEMPT_MS: AtomicI64 = AtomicI64::new(i64::MIN);
static LAST_OPENAI_ATTEMPT_MS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Whether this process may make another lease attempt yet.
fn throttle_allows(last_attempt_ms: &AtomicI64) -> bool {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let previous = last_attempt_ms.load(Ordering::SeqCst);
    // A previous stamp in the future means the clock moved backwards; treat it
    // as due rather than blocking refreshes until the skew passes.
    if previous != i64::MIN && now_ms >= previous && now_ms - previous < ATTEMPT_THROTTLE_MS {
        return false;
    }
    last_attempt_ms.store(now_ms, Ordering::SeqCst);
    true
}

/// Try to become the one process that fetches Anthropic usage right now.
pub(super) fn try_acquire_anthropic() -> Option<FetchLease> {
    if !throttle_allows(&LAST_ANTHROPIC_ATTEMPT_MS) {
        return None;
    }
    try_acquire_at(&anthropic_lease_path())
}

/// Try to become the one process that fetches Codex usage right now.
pub(super) fn try_acquire_openai() -> Option<FetchLease> {
    if !throttle_allows(&LAST_OPENAI_ATTEMPT_MS) {
        return None;
    }
    try_acquire_at(&openai_lease_path())
}

/// Path-explicit form, so the behaviour can be exercised against a temporary
/// directory instead of the real lease other processes are using.
fn try_acquire_at(path: &PathBuf) -> Option<FetchLease> {
    if let Some(held) = read_lease(path)
        && !is_expired(&held)
    {
        // Someone else is fetching right now. Skipping is the whole point.
        return None;
    }

    let token = mint_token();
    let stored = StoredLease {
        token: token.clone(),
        acquired_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let json = serde_json::to_string(&stored).ok()?;
    write_atomic(path, &json)?;

    // Read back and confirm our own token survived. Two processes that both
    // saw the lease free will both write; `rename` makes the file itself
    // whole, so exactly one token is present afterwards and only that writer
    // proceeds. Without this check both would consider themselves the winner,
    // which is precisely the double fetch this module exists to prevent.
    let winner = read_lease(path)?;
    if winner.token != token {
        return None;
    }

    Some(FetchLease {
        path: path.clone(),
        token,
    })
}

/// Give up the lease so the next process does not have to wait out the TTL.
///
/// Only removes the file when it still holds *our* token: a lease that expired
/// mid-fetch and was taken over by another process belongs to that process
/// now, and deleting it would let a third process start a concurrent fetch.
pub(super) fn release(lease: &FetchLease) {
    let Some(current) = read_lease(&lease.path) else {
        return;
    };
    if current.token != lease.token {
        return;
    }
    let _ = std::fs::remove_file(&lease.path);
}

fn read_lease(path: &PathBuf) -> Option<StoredLease> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Whether a lease may be taken over.
///
/// A lease stamped in the future means the clock moved backwards (NTP
/// correction, suspend/resume, a shared home directory). Treating that as
/// expired is the safe answer: honouring it could block refreshes for as long
/// as the skew lasts, while taking it over costs at most one duplicate fetch.
fn is_expired(lease: &StoredLease) -> bool {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let age = now_ms - lease.acquired_at_ms;
    !(0..LEASE_TTL_MS).contains(&age)
}

/// Write through a temporary file in the same directory, then rename, so a
/// concurrent reader never observes a half-written lease and parses it as
/// free. Mirrors `snapshot::write_atomic`.
fn write_atomic(path: &PathBuf, contents: &str) -> Option<()> {
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let file_name = path.file_name()?.to_str()?;
    // Pid plus nanos keeps two publishers from clobbering each other's
    // temporary file, which would produce the torn write this prevents.
    let tmp = dir.join(format!("{}.tmp.{}", file_name, mint_token()));
    if std::fs::write(&tmp, contents).is_err() {
        return None;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary path, so tests never touch the real lease that other jcode
    /// processes on this machine are using for real.
    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jcode-usage-lease-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("usage-fetch.lease");
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn the_first_caller_acquires_and_the_second_is_refused() {
        let path = temp_path("contended");
        let first = try_acquire_at(&path).expect("first caller wins");
        assert!(
            try_acquire_at(&path).is_none(),
            "a second process must skip its fetch, not fetch alongside the first"
        );
        release(&first);
    }

    #[test]
    fn releasing_lets_the_next_caller_through() {
        let path = temp_path("release");
        let first = try_acquire_at(&path).expect("first caller wins");
        release(&first);
        assert!(
            !path.exists(),
            "release removes the lease rather than waiting out the TTL"
        );
        let second = try_acquire_at(&path).expect("the next caller acquires after a release");
        release(&second);
    }

    #[test]
    fn an_expired_lease_is_taken_over() {
        let path = temp_path("expired");
        // A process that was SIGKILLed mid-fetch leaves this behind. Without
        // takeover it would block every usage refresh on the machine forever.
        let abandoned = StoredLease {
            token: "dead-holder".to_string(),
            acquired_at_ms: chrono::Utc::now().timestamp_millis() - LEASE_TTL_MS - 1,
        };
        std::fs::write(&path, serde_json::to_string(&abandoned).expect("json")).expect("write");

        let taken = try_acquire_at(&path).expect("an abandoned lease is taken over");
        assert_ne!(taken.token, "dead-holder");
        release(&taken);
    }

    #[test]
    fn a_lease_just_under_the_ttl_is_still_honoured() {
        let path = temp_path("almost-expired");
        let held = StoredLease {
            token: "live-holder".to_string(),
            acquired_at_ms: chrono::Utc::now().timestamp_millis() - LEASE_TTL_MS + 5_000,
        };
        std::fs::write(&path, serde_json::to_string(&held).expect("json")).expect("write");

        assert!(
            try_acquire_at(&path).is_none(),
            "a slow but live fetch must keep its lease, or the double fetch returns"
        );
    }

    #[test]
    fn a_future_timestamp_is_treated_as_expired_rather_than_honoured() {
        let path = temp_path("future");
        // Clock skew, not a live holder. Honouring it could block refreshes
        // for as long as the skew lasts.
        let skewed = StoredLease {
            token: "skewed".to_string(),
            acquired_at_ms: chrono::Utc::now().timestamp_millis() + 600_000,
        };
        std::fs::write(&path, serde_json::to_string(&skewed).expect("json")).expect("write");

        let taken = try_acquire_at(&path).expect("a skewed lease is not honoured");
        release(&taken);
    }

    #[test]
    fn a_corrupt_lease_is_treated_as_free_rather_than_fatal() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{not json").expect("write");
        let taken = try_acquire_at(&path).expect("a corrupt lease never blocks fetching");
        release(&taken);
    }

    #[test]
    fn releasing_a_lease_taken_over_by_someone_else_leaves_it_alone() {
        let path = temp_path("stolen");
        let mine = try_acquire_at(&path).expect("acquire");
        // Simulate: our lease expired mid-fetch and another process took it.
        let theirs = StoredLease {
            token: "other-process".to_string(),
            acquired_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        std::fs::write(&path, serde_json::to_string(&theirs).expect("json")).expect("write");

        release(&mine);

        let still_there = read_lease(&path).expect("the new holder's lease survives");
        assert_eq!(
            still_there.token, "other-process",
            "releasing must not free a lease we no longer own, or a third process fetches too"
        );
    }

    #[test]
    fn releasing_when_the_lease_is_already_gone_is_a_no_op() {
        let path = temp_path("already-gone");
        let mine = try_acquire_at(&path).expect("acquire");
        std::fs::remove_file(&path).expect("remove");
        release(&mine); // must not panic
        assert!(!path.exists());
    }

    #[test]
    fn the_attempt_throttle_admits_the_first_call_then_holds_off_the_next() {
        let clock = AtomicI64::new(i64::MIN);
        assert!(
            throttle_allows(&clock),
            "the first attempt must always be admitted"
        );
        assert!(
            !throttle_allows(&clock),
            "an immediate retry is throttled, or a lost race becomes a file read per frame"
        );
    }

    #[test]
    fn the_attempt_throttle_admits_again_once_the_window_has_passed() {
        let clock = AtomicI64::new(chrono::Utc::now().timestamp_millis() - ATTEMPT_THROTTLE_MS - 1);
        assert!(
            throttle_allows(&clock),
            "the throttle must expire, or a process that lost one race never refreshes again"
        );
    }

    #[test]
    fn the_attempt_throttle_treats_a_future_stamp_as_due() {
        // Clock moved backwards. Honouring the stamp would block this
        // process's refreshes for as long as the skew lasts.
        let clock = AtomicI64::new(chrono::Utc::now().timestamp_millis() + 600_000);
        assert!(throttle_allows(&clock));
    }

    #[test]
    fn the_two_providers_use_separate_lease_files() {
        assert_ne!(
            anthropic_lease_path(),
            openai_lease_path(),
            "an Anthropic fetch must never block a Codex one"
        );
    }
}
