//! Subscription headroom: how much of the current provider quota window is
//! left, and when it resets.
//!
//! The adaptive scheduler was written against provider rate-limit *headers*
//! (`RateLimitInfo`), but nothing ever populated them, so every call site
//! passed `None` and the interval collapsed to a constant `max_interval`.
//! Subscription auth does not send those headers at all — the quota lives in
//! the OAuth usage endpoint that already backs the TUI info widget.
//!
//! So headroom is expressed the way a subscription actually meters: a
//! utilization fraction for a rolling window plus the instant it resets. Both
//! providers report that shape, and a user with both subscriptions is bound by
//! whichever is closer to exhaustion.

use chrono::{DateTime, Utc};

/// Remaining share of a provider quota window, and when the window resets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubscriptionHeadroom {
    /// Fraction of the window still available, in `[0.0, 1.0]`.
    pub remaining_fraction: f32,
    /// When the window rolls over, if the provider said.
    pub resets_at: Option<DateTime<Utc>>,
}

impl SubscriptionHeadroom {
    /// Seconds until the window resets, or `None` when unknown.
    ///
    /// A reset in the past is unknown rather than zero: quota data is cached
    /// and a lapsed timestamp means the snapshot is stale, not that the window
    /// is about to roll over this instant.
    pub fn seconds_until_reset(&self, now: DateTime<Utc>) -> Option<f64> {
        self.resets_at
            .map(|reset| (reset - now).num_seconds())
            .filter(|secs| *secs > 0)
            .map(|secs| secs as f64)
    }
}

/// Parse a provider reset timestamp.
///
/// Mirrors the accepted shapes in `jcode-base`'s usage display (RFC 3339, plus
/// the trailing-`Z` naive form some responses use). That helper is private to
/// its module, and this is a two-branch parse, so it is duplicated rather than
/// widening another crate's API for one caller.
fn parse_reset(timestamp: &str) -> Option<DateTime<Utc>> {
    if let Ok(reset) = DateTime::parse_from_rfc3339(timestamp) {
        return Some(reset.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.fZ")
        .ok()
        .map(|naive| naive.and_utc())
}

/// One window's utilization, as reported by a provider.
///
/// Kept separate from the provider snapshots so the selection rule below is a
/// pure function that tests can drive without touching global usage caches.
#[derive(Debug, Clone, Copy)]
pub struct WindowUtilization {
    /// Share of the window consumed, in `[0.0, 1.0]`.
    pub utilization: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

/// The binding constraint across every reported window.
///
/// Returns `None` when no provider reported anything, which the scheduler must
/// treat as "no information" — not as "no quota" and not as "unlimited".
///
/// Selection is by *highest utilization*: with two subscriptions and several
/// windows each (5-hour, weekly), ambient is limited by whichever is closest to
/// exhaustion. Picking the roomiest window would let a spent weekly quota hide
/// behind a fresh 5-hour one.
pub fn binding_headroom(
    windows: impl IntoIterator<Item = WindowUtilization>,
) -> Option<SubscriptionHeadroom> {
    windows
        .into_iter()
        .filter(|w| w.utilization.is_finite())
        .map(|w| WindowUtilization {
            utilization: w.utilization.clamp(0.0, 1.0),
            resets_at: w.resets_at,
        })
        .max_by(|a, b| {
            a.utilization
                .partial_cmp(&b.utilization)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|w| SubscriptionHeadroom {
            remaining_fraction: (1.0 - w.utilization).clamp(0.0, 1.0),
            resets_at: w.resets_at,
        })
}

/// Whether an Anthropic usage snapshot carries a reading worth acting on.
///
/// Split out from [`current_subscription_headroom`] because that function
/// reads process-global caches and so cannot be exercised by a unit test. This
/// rule is the one that actually decides whether ambient trusts the meter, and
/// a mutation-test showed the inline version could be reverted with every test
/// still green.
///
/// The failure mode being guarded is subtle: on a failed fetch the usage module
/// stamps `fetched_at` and records `last_error` while leaving every utilization
/// at its zero default, which is byte-identical to a pristine full window. So
/// "has it been fetched?" is not sufficient; require no error, plus a reset
/// timestamp, which a real response always carries.
pub(crate) fn anthropic_snapshot_is_usable(
    fetched: bool,
    has_error: bool,
    has_five_hour_reset: bool,
    has_seven_day_reset: bool,
) -> bool {
    fetched && !has_error && (has_five_hour_reset || has_seven_day_reset)
}

/// Whether an OpenAI/Codex usage snapshot carries a reading worth acting on.
///
/// `OpenAIUsageData` models absent windows as `None` rather than zero, so
/// `has_limits` already separates "nothing reported" from "nothing used"; the
/// error check still matters because a failed fetch stamps `fetched_at` too.
pub(crate) fn openai_snapshot_is_usable(fetched: bool, has_error: bool, has_limits: bool) -> bool {
    fetched && !has_error && has_limits
}

/// A provider's reported quota windows, reduced to the shape this module needs.
///
/// Both providers are normalized into this before selection so the "which
/// windows count" rule is one pure function rather than two inline blocks
/// welded to process-global caches.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderWindows {
    /// Whether this provider's snapshot is usable at all.
    pub usable: bool,
    /// Utilization plus reset time for each window the provider reported.
    pub windows: Vec<WindowUtilization>,
}

/// Every window that should be considered, across both providers.
///
/// Exists as its own function because a mutation test showed that deleting the
/// entire OpenAI branch from [`current_subscription_headroom`] left all tests
/// green: that function reads global caches, so nothing could assert both
/// providers were consulted. A user with two subscriptions must be paced by
/// whichever runs out first, so silently dropping one is a real failure that
/// simply looks like "quota is fine".
pub(crate) fn collect_windows(
    anthropic: &ProviderWindows,
    openai: &ProviderWindows,
) -> Vec<WindowUtilization> {
    let mut windows = Vec::new();
    for provider in [anthropic, openai] {
        if provider.usable {
            windows.extend(provider.windows.iter().copied());
        }
    }
    windows
}

/// Current headroom across the user's subscriptions, or `None` when neither
/// provider has usable quota data.
///
/// Reads the same cached snapshots that feed the TUI info widget, so this adds
/// no network traffic of its own: a stale cache triggers that module's own
/// background refresh and we use the previous value meanwhile.
///
/// A snapshot without usable limits is skipped rather than treated as a full
/// window. That covers the API-key case (usage is billed, not metered), the
/// not-yet-fetched case, and a *failed* fetch. The last one is the trap: on
/// error the usage module stamps `fetched_at` and records `last_error` while
/// leaving every utilization at its zero default, which is byte-identical to a
/// pristine untouched window. Reading that as full headroom would make ambient
/// run flat out precisely when the meter is broken, so an errored snapshot is
/// treated as no information and the caller falls back to `max_interval`.
pub fn current_subscription_headroom() -> Option<SubscriptionHeadroom> {
    let anthropic = jcode_base::usage::get_sync();
    // `UsageData` has no `has_limits`, and an unfetched or errored snapshot is
    // all zeroes, indistinguishable from a genuinely untouched window by value
    // alone. Require a successful fetch that actually reported a window: a real
    // response always carries reset timestamps, so their absence means there is
    // nothing to reason about.
    let anthropic_windows = ProviderWindows {
        usable: anthropic_snapshot_is_usable(
            anthropic.fetched_at.is_some(),
            anthropic.last_error.is_some(),
            anthropic.five_hour_resets_at.is_some(),
            anthropic.seven_day_resets_at.is_some(),
        ),
        windows: vec![
            WindowUtilization {
                utilization: anthropic.five_hour,
                resets_at: anthropic
                    .five_hour_resets_at
                    .as_deref()
                    .and_then(parse_reset),
            },
            WindowUtilization {
                utilization: anthropic.seven_day,
                resets_at: anthropic
                    .seven_day_resets_at
                    .as_deref()
                    .and_then(parse_reset),
            },
        ],
    };

    let openai = jcode_base::usage::get_openai_usage_sync();
    // `OpenAIUsageData` models absent windows as `None` rather than zero, so
    // `has_limits` already distinguishes "nothing reported" from "nothing used"
    // and an errored fetch simply carries no windows.
    let openai_windows = ProviderWindows {
        usable: openai_snapshot_is_usable(
            openai.fetched_at.is_some(),
            openai.last_error.is_some(),
            openai.has_limits(),
        ),
        windows: [
            openai.five_hour.as_ref(),
            openai.seven_day.as_ref(),
            openai.spark.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|window| WindowUtilization {
            utilization: window.usage_ratio,
            resets_at: window.resets_at.as_deref().and_then(parse_reset),
        })
        .collect(),
    };

    binding_headroom(collect_windows(&anthropic_windows, &openai_windows))
}

#[cfg(test)]
#[path = "headroom_tests.rs"]
mod tests;
