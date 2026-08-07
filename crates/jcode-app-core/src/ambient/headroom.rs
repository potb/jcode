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

/// Current headroom across the user's subscriptions, or `None` when neither
/// provider has usable quota data.
///
/// Reads the same cached snapshots that feed the TUI info widget, so this adds
/// no network traffic of its own: a stale cache triggers that module's own
/// background refresh and we use the previous value meanwhile.
///
/// A snapshot that reports no limits at all is skipped rather than treated as a
/// full window. That is the API-key case (usage is billed, not metered) and the
/// not-yet-fetched case, and inventing headroom there would make ambient run at
/// full speed on exactly the accounts where the meter is unknown.
pub fn current_subscription_headroom() -> Option<SubscriptionHeadroom> {
    let mut windows: Vec<WindowUtilization> = Vec::new();

    let anthropic = jcode_base::usage::get_sync();
    // `UsageData` has no `has_limits`; an unfetched snapshot is all zeroes and
    // a missing reset timestamp, which is indistinguishable from a genuinely
    // untouched window by value alone. `fetched_at` is the discriminator.
    if anthropic.fetched_at.is_some() {
        windows.push(WindowUtilization {
            utilization: anthropic.five_hour,
            resets_at: anthropic.five_hour_resets_at.as_deref().and_then(parse_reset),
        });
        windows.push(WindowUtilization {
            utilization: anthropic.seven_day,
            resets_at: anthropic.seven_day_resets_at.as_deref().and_then(parse_reset),
        });
    }

    let openai = jcode_base::usage::get_openai_usage_sync();
    if openai.fetched_at.is_some() {
        for window in [
            openai.five_hour.as_ref(),
            openai.seven_day.as_ref(),
            openai.spark.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            windows.push(WindowUtilization {
                utilization: window.usage_ratio,
                resets_at: window.resets_at.as_deref().and_then(parse_reset),
            });
        }
    }

    binding_headroom(windows)
}

#[cfg(test)]
#[path = "headroom_tests.rs"]
mod tests;
