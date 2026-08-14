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
        let data = get().await;
        publish_anthropic_snapshot(&data);
    }
    if openai_decision() == PollDecision::Poll {
        let _ = get_openai_usage().await;
    }
}

/// Fan the refreshed Anthropic snapshot out to attached clients.
///
/// Published unconditionally each round rather than only on change: a client
/// that connected mid-round, or reconnected after a server reload, needs a
/// snapshot without waiting for the quota numbers to happen to move. The bus is
/// process-local and the payload is small, so a per-minute publish costs
/// nothing, and `snapshot_from_usage` already refuses to emit error or empty
/// states.
fn publish_anthropic_snapshot(data: &UsageData) {
    let account_label =
        auth::claude::active_account_label().unwrap_or_else(auth::claude::primary_account_label);
    if let Some(snapshot) = super::push::snapshot_from_usage(data, Some(&account_label)) {
        crate::bus::Bus::global().publish(crate::bus::BusEvent::UsageSnapshotRefreshed(snapshot));
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
        run_poll_loop(SERVER_POLL_INTERVAL, poll_once).await;
    });

    true
}

/// The loop shape, with the interval and the per-tick work as parameters.
///
/// Split out of [`spawn_server_usage_poller`] so the cadence can be asserted
/// against a paused clock. Inside the spawned task it was unobservable: the
/// only ways to check it were to sleep for real minutes or to suspend the
/// machine, so the two properties that make this loop safe — an immediate
/// first tick, and missed ticks collapsing instead of bursting — were carried
/// by a comment rather than by a test.
pub(super) async fn run_poll_loop<T, Fut>(interval: Duration, mut tick: T)
where
    T: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut ticker = tokio::time::interval(interval);
    // Missed ticks must not be replayed back-to-back: if the machine
    // suspends, a burst of catch-up ticks is precisely the storm this
    // is meant to avoid.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        tick().await;
    }
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

    #[tokio::test(start_paused = true)]
    async fn the_loop_ticks_immediately_and_then_once_per_interval() {
        // The first tick matters on its own: a server that waited out a full
        // interval before its first refresh would leave the client it just
        // accepted to fetch for itself, which is the case this loop exists to
        // prevent.
        let ticks = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let counter = ticks.clone();
        let interval = Duration::from_secs(60);

        let loop_task = run_poll_loop(interval, move || {
            let counter = counter.clone();
            async move { counter.set(counter.get() + 1) }
        });
        tokio::pin!(loop_task);

        // Poll once with no time elapsed: the immediate first tick must land.
        let _ = futures::poll!(&mut loop_task);
        assert_eq!(
            ticks.get(),
            1,
            "the first tick must not wait out an interval"
        );

        tokio::time::advance(interval).await;
        let _ = futures::poll!(&mut loop_task);
        assert_eq!(ticks.get(), 2, "one tick per interval afterwards");
    }

    #[tokio::test(start_paused = true)]
    async fn missed_ticks_collapse_instead_of_bursting() {
        // Three intervals pass while nothing polls the loop — a descheduled
        // task, or a machine that suspended. `MissedTickBehavior::Delay` must
        // turn that into a single catch-up tick: replaying them back-to-back
        // would aim a burst at the very rate limiter this loop protects.
        let ticks = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let counter = ticks.clone();
        let interval = Duration::from_secs(60);

        let loop_task = run_poll_loop(interval, move || {
            let counter = counter.clone();
            async move { counter.set(counter.get() + 1) }
        });
        tokio::pin!(loop_task);

        let _ = futures::poll!(&mut loop_task);
        assert_eq!(ticks.get(), 1);

        tokio::time::advance(interval * 3).await;
        let _ = futures::poll!(&mut loop_task);
        assert_eq!(
            ticks.get(),
            2,
            "three missed intervals must produce one catch-up tick, not three"
        );
    }
}
