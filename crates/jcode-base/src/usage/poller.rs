//! Server-owned usage polling.
//!
//! Every jcode process refreshes usage lazily, from its own render loop, which
//! is what made N clients hammer the same burst limiter (issue #24). The
//! cross-process snapshot and lease already cut that down to one fetch per
//! machine per round, but they are still driven by whichever process happens to
//! notice staleness first — which is usually a short-lived client.
//!
//! This module gives the daemon a steady tick of its own. The server is the one
//! process that is always up, so letting it drive the refresh means it is
//! normally the lease winner, and clients keep adopting the snapshot it
//! publishes instead of racing for the fetch themselves.
//!
//! Deliberately no protocol change here: the loop reuses the existing
//! `get`/`get_openai_usage` entry points, so it stays correct for processes
//! with no server connection (menubar, one-shot CLI) which cannot be pushed to.

use super::*;

/// How often the server re-checks staleness.
///
/// This is a *check* interval, not a fetch interval: `get()` only spawns a
/// network refresh when the cached data is actually stale. It is deliberately
/// shorter than [`CACHE_DURATION`] so the server notices a window reset within
/// a tick rather than up to a full cache duration late — that reset instant is
/// exactly when every other process decides it needs a fetch, so the server has
/// to be there first for the lease to do any good.
pub const SERVER_POLL_INTERVAL: Duration = Duration::from_secs(60);

static SERVER_POLLER_STARTED: AtomicBool = AtomicBool::new(false);

/// Whether a provider is worth polling this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollDecision {
    Poll,
    /// No credentials for this provider, so a fetch could only ever record an
    /// auth error — and that error would then sit in the cache and drive the
    /// error backoff for every reader in this process.
    SkipNoCredentials,
}

pub(super) fn decide(has_credentials: bool) -> PollDecision {
    if has_credentials {
        PollDecision::Poll
    } else {
        PollDecision::SkipNoCredentials
    }
}

fn anthropic_decision() -> PollDecision {
    decide(auth::claude::has_credentials())
}

fn openai_decision() -> PollDecision {
    decide(
        auth::codex::list_accounts()
            .map(|accounts| !accounts.is_empty())
            .unwrap_or(false),
    )
}

/// One poll round: refresh each configured provider if its cache is stale.
///
/// Failures are not propagated: `get`/`get_openai_usage` record their own error
/// state and back off, and a poll loop that gave up on the first network blip
/// would leave the machine back in the every-process-for-itself state this
/// exists to prevent.
pub async fn poll_once() {
    if anthropic_decision() == PollDecision::Poll {
        let _ = get().await;
    }
    if openai_decision() == PollDecision::Poll {
        let _ = get_openai_usage().await;
    }
}

/// Start the server-owned poll loop. Idempotent: a second call is a no-op, so a
/// reload path that re-runs server startup cannot end up with two loops racing
/// for the same lease.
///
/// Returns whether this call started the loop, which is what the idempotence
/// test observes.
pub fn spawn_server_usage_poller() -> bool {
    if SERVER_POLLER_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }

    tokio::spawn(async {
        let mut ticker = tokio::time::interval(SERVER_POLL_INTERVAL);
        // Missed ticks must not be replayed back-to-back: if the machine
        // suspends, a burst of catch-up ticks is precisely the storm this
        // is meant to avoid.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            poll_once().await;
        }
    });

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_without_credentials_is_skipped() {
        assert_eq!(decide(false), PollDecision::SkipNoCredentials);
        assert_eq!(decide(true), PollDecision::Poll);
    }

    #[test]
    fn the_check_interval_is_shorter_than_the_cache_duration() {
        assert!(
            SERVER_POLL_INTERVAL < CACHE_DURATION,
            "the server must re-check more often than the cache expires, \
             otherwise it can be up to a full cache duration late to a window \
             reset and loses the lease race it exists to win"
        );
        assert!(SERVER_POLL_INTERVAL > Duration::from_secs(0));
    }

    #[tokio::test]
    async fn the_poller_starts_exactly_once() {
        assert!(
            spawn_server_usage_poller(),
            "first call should start the loop"
        );
        assert!(
            !spawn_server_usage_poller(),
            "a second call must not spawn a second loop competing for the lease"
        );
    }
}
