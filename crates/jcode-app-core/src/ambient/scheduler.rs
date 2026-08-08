//! Adaptive usage calculator for ambient mode scheduling.
//!
//! Tracks per-call token usage (user vs ambient), maintains a rolling usage log,
//! and computes adaptive intervals for ambient cycles based on rate limit headroom.
use super::headroom::{SubscriptionHeadroom, current_subscription_headroom};
use crate::storage;
use chrono::{Duration as ChronoDuration, Utc};
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Usage record types
// ---------------------------------------------------------------------------

pub use jcode_ambient_types::{RateLimitInfo, UsageRecord, UsageSource};

// ---------------------------------------------------------------------------
// Usage log — rolling, persisted to disk
// ---------------------------------------------------------------------------

/// How often to auto-save (every N records added).
const SAVE_INTERVAL: usize = 10;

/// Records older than this are pruned on save.
const PRUNE_AGE_HOURS: i64 = 24;

/// Ambient's share of quota under the default reserve, used as the reference
/// point that maps "quota left" onto "interval".
///
/// Pinning the scale here means a full window under the default reserve lands
/// exactly on `min_interval_minutes`, so the configured floor is the pace the
/// user actually gets when there is room, rather than an arbitrary fraction of
/// it. Raising `user_budget_reserve` above the default lengthens intervals
/// proportionally; lowering it is capped by the floor.
const DEFAULT_AMBIENT_SHARE: f64 = 1.0 - DEFAULT_USER_BUDGET_RESERVE as f64;

/// Fraction of remaining quota reserved for the user by default.
const DEFAULT_USER_BUDGET_RESERVE: f32 = 0.8;

pub struct UsageLog {
    records: Vec<UsageRecord>,
    path: PathBuf,
    unsaved_count: usize,
}

impl UsageLog {
    /// Load (or create) the usage log from the default path.
    pub fn load() -> Self {
        let path = Self::default_path();
        let records: Vec<UsageRecord> = if path.exists() {
            storage::read_json(&path).unwrap_or_default()
        } else {
            Vec::new()
        };
        UsageLog {
            records,
            path,
            unsaved_count: 0,
        }
    }

    fn default_path() -> PathBuf {
        storage::jcode_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("ambient")
            .join("usage.json")
    }

    /// Add a record and periodically save.
    pub fn record(&mut self, record: UsageRecord) {
        self.records.push(record);
        self.unsaved_count += 1;
        if self.unsaved_count >= SAVE_INTERVAL
            && let Err(err) = self.save()
        {
            crate::logging::warn(&format!(
                "Failed to persist ambient usage log '{}': {}",
                self.path.display(),
                err
            ));
        }
    }

    /// Rolling average of *user* token usage per minute over `window`.
    pub fn user_rate_per_minute(&self, window: Duration) -> f32 {
        self.rate_per_minute(UsageSource::User, window)
    }

    /// Rolling average of *ambient* token usage per minute over `window`.
    pub fn ambient_rate_per_minute(&self, window: Duration) -> f32 {
        self.rate_per_minute(UsageSource::Ambient, window)
    }

    /// Total tokens for a given source within a window.
    pub fn total_tokens_in_window(&self, source: &UsageSource, window: Duration) -> u64 {
        let cutoff = Utc::now() - ChronoDuration::from_std(window).unwrap_or_default();
        self.records
            .iter()
            .filter(|r| r.source == *source && r.timestamp >= cutoff)
            .map(|r| r.total_tokens())
            .sum()
    }

    /// Average tokens per ambient cycle (last N cycles).
    pub fn avg_tokens_per_ambient_cycle(&self, last_n: usize) -> Option<f64> {
        let ambient: Vec<u64> = self
            .records
            .iter()
            .rev()
            .filter(|r| r.source == UsageSource::Ambient)
            .take(last_n)
            .map(|r| r.total_tokens())
            .collect();
        if ambient.is_empty() {
            return None;
        }
        let sum: u64 = ambient.iter().sum();
        Some(sum as f64 / ambient.len() as f64)
    }

    /// Persist to disk, pruning old records.
    pub fn save(&mut self) -> anyhow::Result<()> {
        self.prune();
        storage::write_json(&self.path, &self.records)?;
        self.unsaved_count = 0;
        Ok(())
    }

    // -- internal helpers ---------------------------------------------------

    fn rate_per_minute(&self, source: UsageSource, window: Duration) -> f32 {
        let cutoff = Utc::now() - ChronoDuration::from_std(window).unwrap_or_default();
        let total: u64 = self
            .records
            .iter()
            .filter(|r| r.source == source && r.timestamp >= cutoff)
            .map(|r| r.total_tokens())
            .sum();
        let minutes = window.as_secs_f32() / 60.0;
        if minutes > 0.0 {
            total as f32 / minutes
        } else {
            0.0
        }
    }

    fn prune(&mut self) {
        let cutoff = Utc::now() - ChronoDuration::hours(PRUNE_AGE_HOURS);
        self.records.retain(|r| r.timestamp >= cutoff);
    }
}

// ---------------------------------------------------------------------------
// Scheduler config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AmbientSchedulerConfig {
    pub min_interval_minutes: u32,
    pub max_interval_minutes: u32,
    pub pause_on_active_session: bool,
    /// Fraction of remaining budget reserved for user. 0.8 means ambient gets
    /// at most 20% of headroom.
    pub user_budget_reserve: f32,
}

impl Default for AmbientSchedulerConfig {
    fn default() -> Self {
        AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            pause_on_active_session: true,
            user_budget_reserve: DEFAULT_USER_BUDGET_RESERVE,
        }
    }
}

// ---------------------------------------------------------------------------
// Adaptive scheduler
// ---------------------------------------------------------------------------

pub struct AdaptiveScheduler {
    pub usage_log: UsageLog,
    pub config: AmbientSchedulerConfig,
    /// Exponential backoff multiplier (doubles on rate limit hits).
    backoff_multiplier: u32,
    /// Whether a user session is currently active.
    user_active: bool,
}

impl AdaptiveScheduler {
    pub fn new(config: AmbientSchedulerConfig) -> Self {
        AdaptiveScheduler {
            usage_log: UsageLog::load(),
            config,
            backoff_multiplier: 1,
            user_active: false,
        }
    }

    /// Core interval calculation following the algorithm in AMBIENT_MODE.md.
    ///
    /// Prefers per-request rate-limit headers when a caller has them. Nothing
    /// populates those under subscription auth, so absent them this falls back
    /// to the OAuth quota snapshot (see [`headroom_interval`]) rather than
    /// straight to `max_interval`.
    pub fn calculate_interval(&self, rate_limit_info: Option<&RateLimitInfo>) -> Duration {
        self.calculate_interval_with(rate_limit_info, current_subscription_headroom())
    }

    /// [`calculate_interval`] with the quota reading supplied explicitly.
    ///
    /// The production entry point reads process-global usage caches, which no
    /// unit test can populate. A mutation test exploited exactly that: severing
    /// the headroom call so the interval always returned `max_interval` (the
    /// original dead-code bug this feature exists to fix) left every test
    /// green. Taking the reading as a parameter makes the wiring itself
    /// assertable.
    ///
    /// [`calculate_interval`]: Self::calculate_interval
    pub fn calculate_interval_with(
        &self,
        rate_limit_info: Option<&RateLimitInfo>,
        headroom: Option<SubscriptionHeadroom>,
    ) -> Duration {
        let max = Duration::from_secs(self.config.max_interval_minutes as u64 * 60);
        let min = Duration::from_secs(self.config.min_interval_minutes as u64 * 60);

        let info = match rate_limit_info {
            Some(i) => i,
            None => {
                return match headroom {
                    Some(headroom) => self.headroom_interval(&headroom, Utc::now()),
                    // No quota data at all (API key, or usage not yet fetched):
                    // stay at the configured ceiling, which is the pre-existing
                    // conservative behaviour.
                    None => self.apply_backoff(max),
                };
            }
        };

        // window_remaining = reset_time - now
        let window_remaining_secs = info
            .reset_at
            .map(|r| {
                let diff = r - Utc::now();
                diff.num_seconds().max(0) as f64
            })
            .unwrap_or(3600.0); // default 1 hour if unknown

        let tokens_remaining = info.remaining_tokens.unwrap_or(0) as f64;

        if tokens_remaining <= 0.0 || window_remaining_secs <= 0.0 {
            return self.apply_backoff(max);
        }

        // Estimate user consumption from rolling history (last hour).
        let user_rate = self
            .usage_log
            .user_rate_per_minute(Duration::from_secs(3600)) as f64;

        // Project user usage for rest of window.
        let window_remaining_minutes = window_remaining_secs / 60.0;
        let user_projected = user_rate * window_remaining_minutes;

        // Ambient budget = (remaining - user_projected) * (1 - reserve)
        let ambient_fraction = 1.0 - self.config.user_budget_reserve as f64;
        let ambient_budget = (tokens_remaining - user_projected) * ambient_fraction;

        if ambient_budget <= 0.0 {
            // No headroom — wait until window resets.
            return self.apply_backoff(max);
        }

        // Estimate cost per ambient cycle from recent cycles.
        let tokens_per_cycle = self
            .usage_log
            .avg_tokens_per_ambient_cycle(5)
            .unwrap_or(10_000.0); // conservative default

        let cycles_available = ambient_budget / tokens_per_cycle;

        let interval_secs = if cycles_available > 0.0 {
            window_remaining_secs / cycles_available
        } else {
            window_remaining_secs
        };

        let interval = Duration::from_secs_f64(interval_secs);
        self.apply_backoff(interval.clamp(min, max))
    }

    /// Interval implied by how much of the subscription window is left.
    ///
    /// Pace is inversely proportional to remaining quota: a fresh window runs
    /// at the configured floor, and the interval stretches toward the ceiling
    /// as the window is consumed. Scaled so the default reserve puts a full
    /// window exactly at `min_interval_minutes`, and a higher reserve (more of
    /// the quota held back for the user) lengthens every interval.
    ///
    /// The window's *duration* deliberately does not enter the arithmetic. An
    /// earlier version spread the remaining quota across the time left before
    /// reset, which inverted the intended behaviour: a fresh 7-day window paced
    /// slower than a fresh 5-hour one, because the same "cost per cycle" was
    /// being charged as a fraction of a much longer window. What a subscription
    /// actually reports is the fraction left right now, and this recomputes
    /// every cycle, so the fraction alone is the signal. Duration only tells us
    /// how soon it refills, which is used as a cap below.
    ///
    /// Monotonic in remaining quota by construction: less headroom never yields
    /// a shorter interval. Clamped to the configured bounds, so
    /// `max_interval_minutes` stays a hard ceiling and `min_interval_minutes` a
    /// hard floor that no quota reading can breach.
    pub fn headroom_interval(
        &self,
        headroom: &SubscriptionHeadroom,
        now: chrono::DateTime<Utc>,
    ) -> Duration {
        let max = Duration::from_secs(self.config.max_interval_minutes as u64 * 60);
        let min = Duration::from_secs(self.config.min_interval_minutes as u64 * 60);

        // An exhausted window backs off to the ceiling and waits for the reset
        // (or for the user's own consumption to roll out of the window).
        if headroom.remaining_fraction <= 0.0 {
            return self.apply_backoff(max);
        }

        let ambient_share = (1.0 - self.config.user_budget_reserve as f64).clamp(0.0, 1.0);
        if ambient_share <= 0.0 {
            // Every last token is reserved for the user.
            return self.apply_backoff(max);
        }

        let usable = headroom.remaining_fraction as f64 * ambient_share;
        if usable <= 0.0 {
            return self.apply_backoff(max);
        }

        let interval_secs = min.as_secs_f64() * (DEFAULT_AMBIENT_SHARE / usable);
        let mut interval = Duration::from_secs_f64(interval_secs.max(0.0));

        // Never idle past a refill. If the window resets in less time than the
        // backed-off interval, waking at the reset is strictly better than
        // sleeping through it and discovering the fresh quota an interval late.
        if let Some(until_reset) = headroom.seconds_until_reset(now) {
            interval = interval.min(Duration::from_secs_f64(until_reset));
        }

        self.apply_backoff(interval.clamp(min, max))
    }

    /// Returns `true` if the scheduler thinks ambient should pause (user active).
    pub fn should_pause(&self) -> bool {
        self.config.pause_on_active_session && self.user_active
    }

    /// Mark user session state.
    pub fn set_user_active(&mut self, active: bool) {
        self.user_active = active;
    }

    /// Called when a provider rate limit error occurs.
    pub fn on_rate_limit_hit(&mut self) {
        self.backoff_multiplier = self.backoff_multiplier.saturating_mul(2).min(64);
    }

    /// Called after a successful ambient cycle.
    pub fn on_successful_cycle(&mut self) {
        self.backoff_multiplier = 1;
    }

    // -- internal --

    fn apply_backoff(&self, interval: Duration) -> Duration {
        let min = Duration::from_secs(self.config.min_interval_minutes as u64 * 60);
        let max = Duration::from_secs(self.config.max_interval_minutes as u64 * 60);
        let adjusted = interval.saturating_mul(self.backoff_multiplier);
        adjusted.clamp(min, max)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(source: UsageSource, tokens: u32, mins_ago: i64) -> UsageRecord {
        UsageRecord {
            timestamp: Utc::now() - ChronoDuration::minutes(mins_ago),
            source,
            tokens_input: tokens / 2,
            tokens_output: tokens / 2,
            provider: "test".to_string(),
        }
    }

    #[test]
    fn test_usage_log_rate_per_minute() {
        let mut log = UsageLog {
            records: Vec::new(),
            path: PathBuf::from("/tmp/test_usage.json"),
            unsaved_count: 0,
        };

        // Add 3 user records in the last 30 minutes, 1000 tokens each.
        for i in 0..3 {
            log.records
                .push(make_record(UsageSource::User, 1000, i * 10));
        }

        let rate = log.user_rate_per_minute(Duration::from_secs(3600));
        // 3000 tokens over 60 minutes = 50 tokens/min
        assert!((rate - 50.0).abs() < 1.0, "got {}", rate);
    }

    #[test]
    fn test_total_tokens_in_window() {
        let mut log = UsageLog {
            records: Vec::new(),
            path: PathBuf::from("/tmp/test_usage2.json"),
            unsaved_count: 0,
        };

        log.records.push(make_record(UsageSource::User, 500, 10));
        log.records.push(make_record(UsageSource::Ambient, 300, 5));
        log.records.push(make_record(UsageSource::User, 200, 2));

        let user_total = log.total_tokens_in_window(&UsageSource::User, Duration::from_secs(3600));
        assert_eq!(user_total, 700);

        let ambient_total =
            log.total_tokens_in_window(&UsageSource::Ambient, Duration::from_secs(3600));
        assert_eq!(ambient_total, 300);
    }

    #[test]
    fn test_avg_tokens_per_ambient_cycle() {
        let mut log = UsageLog {
            records: Vec::new(),
            path: PathBuf::from("/tmp/test_usage3.json"),
            unsaved_count: 0,
        };

        // No ambient records => None.
        assert!(log.avg_tokens_per_ambient_cycle(5).is_none());

        log.records
            .push(make_record(UsageSource::Ambient, 1000, 30));
        log.records
            .push(make_record(UsageSource::Ambient, 2000, 20));
        log.records
            .push(make_record(UsageSource::Ambient, 3000, 10));

        let avg = log.avg_tokens_per_ambient_cycle(5).unwrap();
        assert!((avg - 2000.0).abs() < 1.0, "got {}", avg);
    }

    #[test]
    fn test_scheduler_no_rate_limit_returns_max() {
        let config = AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            ..Default::default()
        };
        let scheduler = AdaptiveScheduler::new(config);
        // No headers AND no quota reading: the conservative ceiling. Passing
        // the reading explicitly matters here — this used to call
        // `calculate_interval(None)`, which consults process-global usage
        // caches, so it asserted the ceiling only because those caches happen
        // to be empty under `cargo test`. That made it a test of the
        // environment rather than of the code, and it would have kept passing
        // if the headroom path were deleted outright.
        let interval = scheduler.calculate_interval_with(None, None);
        assert_eq!(interval, Duration::from_secs(120 * 60));
    }

    /// The wiring itself: a quota reading must reach the interval.
    ///
    /// Severing the headroom call so `None` headers always returned
    /// `max_interval` is precisely the dead-code bug this feature fixes, and a
    /// mutation test showed it passed every other test in the suite.
    #[test]
    fn a_quota_reading_reaches_the_interval_rather_than_the_ceiling() {
        let scheduler = AdaptiveScheduler::new(AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            user_budget_reserve: 0.8,
            ..Default::default()
        });

        let fresh = SubscriptionHeadroom {
            remaining_fraction: 1.0,
            resets_at: Some(Utc::now() + ChronoDuration::hours(5)),
        };
        let interval = scheduler.calculate_interval_with(None, Some(fresh));

        assert_eq!(
            interval,
            Duration::from_secs(5 * 60),
            "a full window must pace at the floor, not the ceiling"
        );
        assert_ne!(
            interval,
            Duration::from_secs(120 * 60),
            "returning max_interval here is the original dead-code bug"
        );
    }

    /// Different readings must yield different intervals, so the reading is
    /// genuinely consumed rather than merely accepted and discarded.
    #[test]
    fn the_interval_tracks_the_quota_reading() {
        let scheduler = AdaptiveScheduler::new(AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            user_budget_reserve: 0.8,
            ..Default::default()
        });
        let window = ChronoDuration::hours(5);
        let at = |remaining: f32| {
            scheduler.calculate_interval_with(
                None,
                Some(SubscriptionHeadroom {
                    remaining_fraction: remaining,
                    resets_at: Some(Utc::now() + window),
                }),
            )
        };

        let plenty = at(1.0);
        let half = at(0.5);
        let scarce = at(0.1);

        assert!(
            plenty < half && half < scarce,
            "interval must lengthen as quota is consumed: {:?} {:?} {:?}",
            plenty,
            half,
            scarce
        );
    }

    /// Explicit rate-limit headers still win over the quota reading, so the
    /// legacy path is not accidentally bypassed.
    #[test]
    fn explicit_rate_limit_headers_take_precedence_over_quota() {
        let scheduler = AdaptiveScheduler::new(AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            user_budget_reserve: 0.8,
            ..Default::default()
        });

        // Headers say the window is spent; the quota reading says it is fresh.
        let info = RateLimitInfo {
            limit_tokens: Some(100_000),
            remaining_tokens: Some(0),
            limit_requests: None,
            remaining_requests: None,
            reset_at: Some(Utc::now() + ChronoDuration::hours(1)),
        };
        let fresh = SubscriptionHeadroom {
            remaining_fraction: 1.0,
            resets_at: Some(Utc::now() + ChronoDuration::hours(5)),
        };

        assert_eq!(
            scheduler.calculate_interval_with(Some(&info), Some(fresh)),
            Duration::from_secs(120 * 60),
            "exhausted headers must back off despite a fresh quota reading"
        );
    }

    #[test]
    fn test_scheduler_no_remaining_tokens_returns_max() {
        let config = AmbientSchedulerConfig::default();
        let scheduler = AdaptiveScheduler::new(config);

        let info = RateLimitInfo {
            limit_tokens: Some(100_000),
            remaining_tokens: Some(0),
            limit_requests: None,
            remaining_requests: None,
            reset_at: Some(Utc::now() + ChronoDuration::hours(1)),
        };
        let interval = scheduler.calculate_interval(Some(&info));
        assert_eq!(interval, Duration::from_secs(120 * 60));
    }

    #[test]
    fn test_scheduler_plenty_of_headroom() {
        let config = AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            user_budget_reserve: 0.8,
            ..Default::default()
        };
        let scheduler = AdaptiveScheduler::new(config);

        let info = RateLimitInfo {
            limit_tokens: Some(1_000_000),
            remaining_tokens: Some(500_000),
            limit_requests: None,
            remaining_requests: None,
            reset_at: Some(Utc::now() + ChronoDuration::hours(1)),
        };

        let interval = scheduler.calculate_interval(Some(&info));
        // With 500k remaining, 0 user rate, 20% for ambient = 100k budget.
        // Default 10k per cycle => 10 cycles in 60 min => 6 min per cycle.
        let mins = interval.as_secs() as f64 / 60.0;
        assert!(
            (5.0..=10.0).contains(&mins),
            "expected 5-10 min, got {:.1}",
            mins
        );
    }

    #[test]
    fn test_backoff_doubles() {
        let config = AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            ..Default::default()
        };
        let mut scheduler = AdaptiveScheduler::new(config);

        let info = RateLimitInfo {
            limit_tokens: Some(1_000_000),
            remaining_tokens: Some(500_000),
            limit_requests: None,
            remaining_requests: None,
            reset_at: Some(Utc::now() + ChronoDuration::hours(1)),
        };

        let before = scheduler.calculate_interval(Some(&info));
        scheduler.on_rate_limit_hit();
        let after = scheduler.calculate_interval(Some(&info));

        // After one hit, interval should roughly double (clamped).
        assert!(
            after >= before,
            "after backoff should be >= before: {:?} vs {:?}",
            after,
            before
        );
    }

    #[test]
    fn test_backoff_resets_on_success() {
        let config = AmbientSchedulerConfig::default();
        let mut scheduler = AdaptiveScheduler::new(config);

        scheduler.on_rate_limit_hit();
        scheduler.on_rate_limit_hit();
        assert!(scheduler.backoff_multiplier > 1);

        scheduler.on_successful_cycle();
        assert_eq!(scheduler.backoff_multiplier, 1);
    }

    #[test]
    fn test_should_pause() {
        let config = AmbientSchedulerConfig {
            pause_on_active_session: true,
            ..Default::default()
        };
        let mut scheduler = AdaptiveScheduler::new(config);

        assert!(!scheduler.should_pause());
        scheduler.set_user_active(true);
        assert!(scheduler.should_pause());
        scheduler.set_user_active(false);
        assert!(!scheduler.should_pause());
    }

    #[test]
    fn test_prune_removes_old_records() {
        let mut log = UsageLog {
            records: Vec::new(),
            path: PathBuf::from("/tmp/test_prune.json"),
            unsaved_count: 0,
        };

        // Record from 25 hours ago (should be pruned).
        log.records.push(UsageRecord {
            timestamp: Utc::now() - ChronoDuration::hours(25),
            source: UsageSource::User,
            tokens_input: 100,
            tokens_output: 100,
            provider: "test".to_string(),
        });

        // Recent record (should survive).
        log.records.push(make_record(UsageSource::User, 200, 5));

        log.prune();
        assert_eq!(log.records.len(), 1);
        assert_eq!(log.records[0].total_tokens(), 200);
    }

    // -- headroom-driven intervals ------------------------------------------

    fn headroom_scheduler() -> AdaptiveScheduler {
        AdaptiveScheduler::new(AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 15,
            user_budget_reserve: 0.8,
            ..Default::default()
        })
    }

    fn headroom(remaining: f32, window: ChronoDuration) -> SubscriptionHeadroom {
        SubscriptionHeadroom {
            remaining_fraction: remaining,
            resets_at: Some(Utc::now() + window),
        }
    }

    #[test]
    fn a_fresh_window_runs_at_the_configured_floor() {
        // The whole point of wiring quota in: with room to spare, ambient
        // should not be sitting at the ceiling.
        let scheduler = headroom_scheduler();
        let now = Utc::now();

        let interval = scheduler.headroom_interval(&headroom(1.0, ChronoDuration::hours(5)), now);

        assert_eq!(interval, Duration::from_secs(5 * 60));
    }

    #[test]
    fn a_nearly_spent_window_backs_off_to_the_ceiling() {
        let scheduler = headroom_scheduler();
        let now = Utc::now();

        let interval = scheduler.headroom_interval(&headroom(0.02, ChronoDuration::hours(5)), now);

        assert_eq!(interval, Duration::from_secs(15 * 60));
    }

    #[test]
    fn an_exhausted_window_backs_off_to_the_ceiling() {
        let scheduler = headroom_scheduler();
        let now = Utc::now();

        let interval = scheduler.headroom_interval(&headroom(0.0, ChronoDuration::hours(5)), now);

        assert_eq!(interval, Duration::from_secs(15 * 60));
    }

    #[test]
    fn less_headroom_never_means_a_shorter_interval() {
        // Monotonicity is the safety property: whatever the constants, spending
        // more quota must never make ambient run harder.
        let scheduler = headroom_scheduler();
        let now = Utc::now();
        let window = ChronoDuration::hours(5);

        let mut previous = Duration::from_secs(0);
        for step in 0..=20 {
            let remaining = 1.0 - (step as f32 / 20.0);
            let interval = scheduler.headroom_interval(&headroom(remaining, window), now);
            assert!(
                interval >= previous,
                "interval shrank at remaining={}: {:?} < {:?}",
                remaining,
                interval,
                previous
            );
            previous = interval;
        }
    }

    #[test]
    fn the_configured_ceiling_is_never_exceeded() {
        // A quota reading must not be able to stretch ambient past the interval
        // the user configured, however pessimistic the arithmetic gets.
        let scheduler = headroom_scheduler();
        let now = Utc::now();

        let interval = scheduler.headroom_interval(&headroom(0.001, ChronoDuration::days(7)), now);

        assert!(
            interval <= Duration::from_secs(15 * 60),
            "got {:?}",
            interval
        );
    }

    #[test]
    fn an_unknown_reset_time_still_produces_a_bounded_interval() {
        // Utilization without a reset timestamp is a real provider response; it
        // must degrade to a sane interval rather than dividing by nothing.
        let scheduler = headroom_scheduler();
        let now = Utc::now();

        let interval = scheduler.headroom_interval(
            &SubscriptionHeadroom {
                remaining_fraction: 0.5,
                resets_at: None,
            },
            now,
        );

        assert!(
            interval >= Duration::from_secs(5 * 60) && interval <= Duration::from_secs(15 * 60),
            "got {:?}",
            interval
        );
    }

    #[test]
    fn a_lapsed_reset_falls_back_instead_of_collapsing_the_window() {
        // A stale snapshot's reset time is in the past. Treating that as zero
        // seconds remaining would pin the interval at the ceiling on nothing
        // more than a cache that has not refreshed.
        let scheduler = headroom_scheduler();
        let now = Utc::now();

        let interval = scheduler.headroom_interval(
            &SubscriptionHeadroom {
                remaining_fraction: 1.0,
                resets_at: Some(now - ChronoDuration::minutes(5)),
            },
            now,
        );

        assert_eq!(interval, Duration::from_secs(5 * 60));
    }

    #[test]
    fn rate_limit_backoff_still_applies_to_headroom_intervals() {
        // Backoff is the response to an actual 429, so it has to survive the
        // new path; otherwise a fresh window would cancel it out.
        let mut scheduler = headroom_scheduler();
        let now = Utc::now();
        let fresh = headroom(1.0, ChronoDuration::hours(5));

        let before = scheduler.headroom_interval(&fresh, now);
        scheduler.on_rate_limit_hit();
        let after = scheduler.headroom_interval(&fresh, now);

        assert!(after > before, "{:?} should exceed {:?}", after, before);
    }

    #[test]
    fn a_full_user_reserve_leaves_ambient_at_the_ceiling() {
        // Reserving everything for the user is a coherent request, and must not
        // divide by zero on its way to the answer.
        let scheduler = AdaptiveScheduler::new(AmbientSchedulerConfig {
            min_interval_minutes: 5,
            max_interval_minutes: 15,
            user_budget_reserve: 1.0,
            ..Default::default()
        });

        let interval =
            scheduler.headroom_interval(&headroom(1.0, ChronoDuration::hours(5)), Utc::now());

        assert_eq!(interval, Duration::from_secs(15 * 60));
    }
}
