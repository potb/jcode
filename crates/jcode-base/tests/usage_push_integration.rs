//! End-to-end behaviour of the server-owned usage push (issue #24).
//!
//! These live in their own integration binary on purpose. The push-fed marker
//! that `adopt_pushed_snapshot` sets is a process-global latch, and it is
//! deliberately never cleared (a client that loses its server keeps serving the
//! last pushed snapshot rather than resuming its own polling). A unit test that
//! set it would therefore change the behaviour of every later test sharing the
//! `jcode-base` test binary. Here the process belongs to this file, so the real
//! public functions can be exercised instead of the pure helpers the unit tests
//! have to settle for.

use jcode_base::usage;
use jcode_protocol::{UsageSnapshot, UsageSnapshotWindow};

fn snapshot() -> UsageSnapshot {
    UsageSnapshot {
        // No label, so the snapshot applies whatever account this machine has
        // configured. Account mismatch is covered by a unit test; hard-coding a
        // label here would make the outcome depend on the developer's auth.json.
        account_label: None,
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        five_hour: 0.5,
        five_hour_resets_at: Some("2099-01-01T00:00:00Z".to_string()),
        seven_day: 0.25,
        seven_day_resets_at: Some("2099-01-02T00:00:00Z".to_string()),
        seven_day_opus: Some(0.125),
        model_scoped: vec![UsageSnapshotWindow {
            model_name: "Fable".to_string(),
            utilization: 0.75,
            resets_at: Some("2099-01-03T00:00:00Z".to_string()),
        }],
        extra_usage_enabled: true,
    }
}

/// The whole point of the feature: a pushed snapshot becomes what the render
/// path reads, and the process stops fetching for itself.
///
/// One test rather than several, because the latch is one-way: the
/// before-adoption assertion is only observable once per process, so splitting
/// this up would make the order between test binaries significant.
/// The "a process with no push must still refresh itself" half of this lives in
/// `usage_no_push_refreshes.rs`, not here. The latch is process-global and
/// one-way, so once any test in this binary adopts a snapshot that precondition
/// is gone for every test after it; asserting it here would only pass while cargo
/// happens to order tests alphabetically. Do not move it back.
#[test]
fn a_pushed_snapshot_is_adopted_and_ends_self_polling() {
    assert!(
        usage::adopt_pushed_snapshot(&snapshot()),
        "an unlabelled snapshot should be adopted"
    );

    // The render path (`tui_state.rs` calls `get_sync`) must now see the pushed
    // values. This is the assertion that would have caught a push that arrived,
    // was accepted, and then never reached the cell the UI reads.
    let visible = usage::get_sync();
    assert_eq!(visible.five_hour, 0.5);
    assert_eq!(visible.seven_day, 0.25);
    assert_eq!(visible.seven_day_opus, Some(0.125));
    assert!(visible.extra_usage_enabled);
    assert_eq!(visible.model_scoped.len(), 1);
    assert_eq!(visible.model_scoped[0].model_name, "Fable");
    assert_eq!(visible.model_scoped[0].utilization, 0.75);

    // And the process is now push-fed, which is what suppresses the refresh
    // spawn inside `get_sync`/`try_spawn_refresh`. Without this, every connected
    // client resumes polling once the snapshot ages past CACHE_DURATION and the
    // 429 storm returns.
    assert!(
        usage::is_push_fed(),
        "adopting a push must latch the process out of self-refreshing"
    );

    // A second push must still land: the poller republishes every round, and a
    // client that could only ever adopt once would freeze on its first snapshot.
    let mut newer = snapshot();
    newer.five_hour = 0.9;
    assert!(usage::adopt_pushed_snapshot(&newer));
    assert_eq!(usage::get_sync().five_hour, 0.9);

    // `get_sync` is what the render loop calls every frame. Calling it while
    // push-fed must stay side-effect free and must not blank the adopted values.
    for _ in 0..5 {
        assert_eq!(usage::get_sync().five_hour, 0.9);
    }
}

/// The Anthropic push must not silence the OpenAI/Codex refresh.
///
/// Only Anthropic is pushed today: `poller::poll_once` publishes an Anthropic
/// snapshot, and the Codex side still relies on the shared on-disk snapshot and
/// the fetch lease. So the latch that stops *Anthropic* self-refreshing must not
/// leak across providers, or Codex usage would freeze at whatever the client last
/// saw and never recover -- a silent failure, since a frozen readout looks exactly
/// like a working one.
///
/// This holds today because the Codex path has its own in-flight flag and never
/// consults `is_push_fed`, but that is a property of two separate code paths
/// rather than anything enforced. Pinned here so a future refactor that unifies
/// them has to notice.
#[test]
fn the_anthropic_push_does_not_freeze_codex_usage() {
    assert!(
        usage::adopt_pushed_snapshot(&snapshot()),
        "snapshot should be adopted"
    );
    assert!(usage::is_push_fed(), "process should now be push-fed");

    // Must not panic and must not be routed through the Anthropic cache. Without
    // credentials this is a default snapshot; the assertion is about isolation,
    // so it deliberately checks that the Anthropic values did NOT bleed across
    // rather than asserting real quota numbers.
    let codex = usage::get_openai_usage_sync();
    let codex_five_hour = codex.five_hour.as_ref().map(|window| window.usage_ratio);
    assert_ne!(
        codex_five_hour,
        Some(0.5),
        "Codex usage must not report the Anthropic snapshot's five-hour value"
    );
}
