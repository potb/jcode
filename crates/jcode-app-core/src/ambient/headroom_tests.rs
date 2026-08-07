//! Tests for subscription headroom selection.

use super::*;

fn window(utilization: f32, resets_at: Option<DateTime<Utc>>) -> WindowUtilization {
    WindowUtilization {
        utilization,
        resets_at,
    }
}

#[test]
fn no_reported_windows_is_no_information() {
    // Distinct from "no quota left": the scheduler must not read an absent
    // signal as an exhausted one, nor as a full one.
    assert!(binding_headroom(Vec::new()).is_none());
}

#[test]
fn the_most_consumed_window_binds() {
    // A fresh 5-hour window must not mask a nearly-spent weekly one, which is
    // exactly the shape a heavy week produces.
    let headroom = binding_headroom(vec![window(0.10, None), window(0.90, None)]).unwrap();

    assert!(
        (headroom.remaining_fraction - 0.10).abs() < 1e-6,
        "expected the 90%-consumed window to bind, got {}",
        headroom.remaining_fraction
    );
}

#[test]
fn windows_from_either_provider_compete_on_equal_terms() {
    // With two subscriptions the binding constraint can come from either, so
    // selection must not privilege the order they were collected in.
    let anthropic_first = binding_headroom(vec![window(0.20, None), window(0.75, None)]).unwrap();
    let openai_first = binding_headroom(vec![window(0.75, None), window(0.20, None)]).unwrap();

    assert_eq!(anthropic_first, openai_first);
}

#[test]
fn an_exhausted_window_leaves_no_headroom() {
    let headroom = binding_headroom(vec![window(1.0, None)]).unwrap();

    assert_eq!(headroom.remaining_fraction, 0.0);
}

#[test]
fn out_of_range_utilization_is_clamped_not_trusted() {
    // A malformed reading must not produce a negative remaining fraction, which
    // would flow into the interval arithmetic as a negative cycle count.
    let over = binding_headroom(vec![window(1.4, None)]).unwrap();
    let under = binding_headroom(vec![window(-0.3, None)]).unwrap();

    assert_eq!(over.remaining_fraction, 0.0);
    assert_eq!(under.remaining_fraction, 1.0);
}

#[test]
fn a_non_finite_reading_is_discarded_rather_than_compared() {
    // NaN has no ordering, so leaving it in the comparison could win `max_by`
    // and poison every downstream multiplication.
    let headroom = binding_headroom(vec![window(f32::NAN, None), window(0.5, None)]).unwrap();

    assert!((headroom.remaining_fraction - 0.5).abs() < 1e-6);
}

#[test]
fn only_non_finite_readings_are_no_information() {
    assert!(binding_headroom(vec![window(f32::NAN, None)]).is_none());
}

#[test]
fn seconds_until_reset_reports_the_remaining_window() {
    let now = Utc::now();
    let headroom = SubscriptionHeadroom {
        remaining_fraction: 0.5,
        resets_at: Some(now + chrono::Duration::minutes(30)),
    };

    let secs = headroom.seconds_until_reset(now).unwrap();
    assert!((secs - 1800.0).abs() < 2.0, "got {}", secs);
}

#[test]
fn a_lapsed_reset_is_unknown_not_zero() {
    // Quota snapshots are cached, so a reset in the past means the reading is
    // stale. Reporting 0 would divide the window down to nothing and pin the
    // interval at the ceiling on data we simply have not refreshed yet.
    let now = Utc::now();
    let headroom = SubscriptionHeadroom {
        remaining_fraction: 0.5,
        resets_at: Some(now - chrono::Duration::minutes(5)),
    };

    assert!(headroom.seconds_until_reset(now).is_none());
}

#[test]
fn a_missing_reset_is_unknown() {
    let headroom = SubscriptionHeadroom {
        remaining_fraction: 0.5,
        resets_at: None,
    };

    assert!(headroom.seconds_until_reset(Utc::now()).is_none());
}

#[test]
fn reset_timestamps_parse_in_both_shapes_providers_send() {
    let rfc3339 = parse_reset("2026-08-07T16:30:00+00:00").expect("rfc3339");
    let trailing_z = parse_reset("2026-08-07T16:30:00Z").expect("trailing Z");
    let fractional = parse_reset("2026-08-07T16:30:00.123Z").expect("fractional seconds");

    assert_eq!(rfc3339, trailing_z);
    assert_eq!(fractional.timestamp(), trailing_z.timestamp());
}

#[test]
fn an_unparseable_reset_is_dropped_rather_than_defaulted() {
    // Defaulting to "now" would look like an expiring window and throttle
    // ambient on a formatting change.
    assert!(parse_reset("not a timestamp").is_none());
    assert!(parse_reset("").is_none());
}

#[test]
fn the_binding_window_carries_its_own_reset_time() {
    // The horizon must come from the window that actually binds; pairing a
    // spent window's fraction with a roomy window's reset would spread the
    // remaining quota over the wrong span.
    let now = Utc::now();
    let soon = now + chrono::Duration::minutes(10);
    let later = now + chrono::Duration::hours(20);

    let headroom = binding_headroom(vec![window(0.2, Some(later)), window(0.95, Some(soon))])
        .expect("a binding window");

    assert_eq!(headroom.resets_at, Some(soon));
}

/// A failed usage fetch must never look like a fresh window.
///
/// The usage module stamps `fetched_at` and sets `last_error` on failure while
/// leaving every utilization at its zero default. By value that is identical to
/// a pristine untouched window, so a naive "has it been fetched?" gate reads a
/// broken meter as 100% headroom and runs ambient flat out at exactly the
/// moment it cannot see the quota. Found by probing live credentials: the
/// OpenAI snapshot came back `fetched=true, last_error=No OpenAI/Codex OAuth
/// credentials found` with all windows unset.
#[test]
fn an_errored_snapshot_is_no_information_not_full_headroom() {
    // The shape the usage module produces on error: fetched, zeroed, no windows.
    let errored: Vec<WindowUtilization> = Vec::new();
    assert!(
        binding_headroom(errored).is_none(),
        "an errored fetch reports no windows, which must stay 'no information'"
    );

    // And the value that would be read if the zeroes leaked through: full
    // headroom, i.e. the fastest possible pace. This asserts the difference is
    // real and worth guarding, not that the guard is in this function.
    let leaked = binding_headroom(vec![window(0.0, None)]).unwrap();
    assert_eq!(
        leaked.remaining_fraction, 1.0,
        "zeroed utilization means a full window, which is why it must not leak"
    );
}

// -- snapshot usability -----------------------------------------------------
//
// These pin the rule that decides whether ambient trusts the quota meter at
// all. It lives in its own function because `current_subscription_headroom`
// reads process-global caches; a mutation test showed the previously-inline
// version could be reverted to a bare `fetched_at.is_some()` with all 94 tests
// still green, i.e. the most dangerous branch in this module was uncovered.

/// The exact shape a failed Anthropic fetch produces: stamped, errored, and
/// otherwise all zeroes. Trusting it reads as a pristine full window and runs
/// ambient at its fastest pace exactly when the meter is broken.
#[test]
fn an_errored_anthropic_snapshot_is_not_usable() {
    assert!(!anthropic_snapshot_is_usable(true, true, true, true));
}

/// Never fetched: no information, not a full window.
#[test]
fn an_unfetched_anthropic_snapshot_is_not_usable() {
    assert!(!anthropic_snapshot_is_usable(false, false, true, true));
}

/// A successful fetch that reported no window at all. Real responses always
/// carry a reset timestamp, so their absence means there is nothing to read,
/// which is what an API-key (billed, not metered) account looks like.
#[test]
fn an_anthropic_snapshot_without_any_reset_timestamp_is_not_usable() {
    assert!(!anthropic_snapshot_is_usable(true, false, false, false));
}

/// Either window alone is enough to act on.
#[test]
fn one_anthropic_reset_timestamp_is_enough() {
    assert!(anthropic_snapshot_is_usable(true, false, true, false));
    assert!(anthropic_snapshot_is_usable(true, false, false, true));
}

/// The live-probe case: `fetched=true, last_error=No OpenAI/Codex OAuth
/// credentials found`, every window unset.
#[test]
fn an_errored_openai_snapshot_is_not_usable() {
    assert!(!openai_snapshot_is_usable(true, true, false));
    assert!(!openai_snapshot_is_usable(true, true, true));
}

#[test]
fn an_openai_snapshot_without_limits_is_not_usable() {
    assert!(!openai_snapshot_is_usable(true, false, false));
}

#[test]
fn an_unfetched_openai_snapshot_is_not_usable() {
    assert!(!openai_snapshot_is_usable(false, false, true));
}

/// A clean reading from either provider is usable, so the guards above cannot
/// be satisfied by a function that simply always returns false.
#[test]
fn a_clean_snapshot_from_either_provider_is_usable() {
    assert!(anthropic_snapshot_is_usable(true, false, true, true));
    assert!(openai_snapshot_is_usable(true, false, true));
}

// -- window collection across providers -------------------------------------
//
// A mutation test showed the entire OpenAI branch could be deleted from
// `current_subscription_headroom` with all 102 tests still green, because that
// function reads process-global caches and nothing could observe it. The
// collection rule therefore lives in `collect_windows`, and these pin it.

fn provider(usable: bool, utilizations: &[f32]) -> ProviderWindows {
    ProviderWindows {
        usable,
        windows: utilizations.iter().map(|u| window(*u, None)).collect(),
    }
}

/// Both subscriptions must reach the selection step. Dropping either one is
/// invisible in the result (it just looks like more headroom) but silently
/// removes the constraint the user is actually bound by.
#[test]
fn windows_from_both_providers_are_collected() {
    let collected = collect_windows(&provider(true, &[0.1, 0.2]), &provider(true, &[0.3, 0.4]));

    assert_eq!(collected.len(), 4, "every window from both providers counts");
}

/// The specific mutation: OpenAI dropped entirely. Its window is the binding
/// one here, so losing it changes the answer rather than merely the count.
#[test]
fn dropping_a_provider_loses_the_binding_constraint() {
    let anthropic = provider(true, &[0.10]);
    let openai = provider(true, &[0.95]);

    let both = binding_headroom(collect_windows(&anthropic, &openai)).expect("a window");
    let anthropic_only =
        binding_headroom(collect_windows(&anthropic, &provider(false, &[]))).expect("a window");

    assert!(
        (both.remaining_fraction - 0.05).abs() < 1e-6,
        "with both providers the 95%-spent window binds, got {}",
        both.remaining_fraction
    );
    assert!(
        (anthropic_only.remaining_fraction - 0.90).abs() < 1e-6,
        "dropping OpenAI wrongly reports 90% headroom, got {}",
        anthropic_only.remaining_fraction
    );
}

/// An unusable snapshot contributes nothing, even when it carries windows.
#[test]
fn an_unusable_provider_contributes_no_windows() {
    let collected = collect_windows(&provider(false, &[0.9]), &provider(true, &[0.2]));

    assert_eq!(collected.len(), 1);
    assert!((collected[0].utilization - 0.2).abs() < 1e-6);
}

/// Neither provider usable is "no information", which the caller turns into the
/// conservative ceiling rather than a full window.
#[test]
fn no_usable_provider_yields_no_windows() {
    assert!(collect_windows(&provider(false, &[0.9]), &provider(false, &[0.1])).is_empty());
    assert!(
        binding_headroom(collect_windows(
            &provider(false, &[0.9]),
            &provider(false, &[0.1])
        ))
        .is_none()
    );
}
