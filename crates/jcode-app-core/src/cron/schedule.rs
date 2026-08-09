//! Pure scheduling math for `[[cron]]` jobs: parsing `every`/`at` and
//! computing the next fire time.
//!
//! Kept free of I/O on purpose. `next_fire` is the one function the runner
//! loop actually needs, and it is cheap to get subtly wrong (off-by-one
//! periods, DST, "what does no history mean"), so it is worth being able to
//! unit-test it against a fixed clock without touching disk or config.

use crate::ambient::schedule_window::{parse_days, parse_time};
use crate::config::CronJobConfig;
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Utc, Weekday};

/// A parsed `at` schedule: fire at `time` on each of `days`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AtSchedule {
    days: Vec<Weekday>,
    time: NaiveTime,
}

/// Parse an `every` duration spec: a number followed by `s`/`m`/`h`/`d`
/// (`"30m"`, `"6h"`, `"1d"`). Returns the interval in milliseconds so callers
/// can do integer arithmetic without pulling in `chrono::Duration`'s
/// `i32`-only `Mul` impl, which would overflow on long gaps.
pub(super) fn parse_every(spec: &str) -> Option<i64> {
    let spec = spec.trim();
    let split_at = spec.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = spec.split_at(split_at);
    let n: i64 = num.parse().ok()?;
    if n <= 0 {
        return None;
    }
    let ms_per_unit: i64 = match unit.trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1_000,
        "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000,
        "d" | "day" | "days" => 86_400_000,
        _ => return None,
    };
    n.checked_mul(ms_per_unit)
}

/// Parse an `at` wall-clock spec: `"<days> HH:MM"`, reusing the same day-spec
/// grammar as `[ambient] active_windows` (`daily`, `weekdays`, `mon,thu`,
/// ...) so the two scheduling surfaces read the same way to the user.
pub(super) fn parse_at(spec: &str) -> Option<AtSchedule> {
    let spec = spec.trim().to_lowercase();
    let (days_part, time_part) = spec.split_once(char::is_whitespace)?;
    let days = parse_days(days_part)?;
    let time = parse_time(time_part.trim())?;
    Some(AtSchedule { days, time })
}

/// The next instant at or after `from` that matches `days`/`time`.
///
/// Scans a bounded 8-day horizon (a week plus one, to always find a match
/// even when `from` lands the day after the last configured day) so a
/// pathological spec can never loop forever. Mirrors
/// `schedule_window::ScheduleWindow::next_open`, which solves the same
/// "next matching wall-clock instant" problem for a time *range* instead of
/// a single point.
fn next_at_occurrence(
    days: &[Weekday],
    time: NaiveTime,
    from: DateTime<Local>,
) -> Option<DateTime<Local>> {
    for day_offset in 0..8 {
        let date = (from + Duration::days(day_offset)).date_naive();
        if !days.contains(&date.weekday()) {
            continue;
        }
        // `.latest()` keeps the occurrence on the sane side of a DST jump
        // rather than returning a local time that never happened.
        let candidate = Local.from_local_datetime(&date.and_time(time)).latest()?;
        if candidate >= from {
            return Some(candidate);
        }
    }
    None
}

/// The next fire time for a fixed-interval (`every`) job.
///
/// `None` last_run means the job has never run in recorded state (first-ever
/// run, or state lost to a wipe): `catch_up` decides whether that counts as
/// "overdue since the beginning of time" (fire now) or "start the clock from
/// here" (wait one full interval). Once there is a `last_run`, a due
/// occurrence in the past is either fired as-is (`catch_up`) or walked
/// forward past every missed period to the next one after `now`, so a job
/// that was down for three days does not fire twelve times in a row.
fn next_fire_every(
    interval_ms: i64,
    last_run: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    catch_up: bool,
) -> Option<DateTime<Utc>> {
    if interval_ms <= 0 {
        return None;
    }
    match last_run {
        None => {
            if catch_up {
                Some(now)
            } else {
                Some(now + Duration::milliseconds(interval_ms))
            }
        }
        Some(last) => {
            let due = last + Duration::milliseconds(interval_ms);
            if due > now {
                return Some(due);
            }
            if catch_up {
                return Some(due);
            }
            let elapsed_ms = (now - last).num_milliseconds();
            let periods_elapsed = elapsed_ms / interval_ms + 1;
            Some(last + Duration::milliseconds(interval_ms * periods_elapsed))
        }
    }
}

/// The next fire time for a wall-clock (`at`) job. Same `catch_up` contract
/// as [`next_fire_every`], evaluated in LOCAL time so "daily 03:00" follows
/// the user across DST rather than drifting an hour twice a year.
fn next_fire_at(
    spec: &AtSchedule,
    last_run: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    catch_up: bool,
) -> Option<DateTime<Utc>> {
    let now_local = now.with_timezone(&Local);
    match last_run {
        None => {
            // A first-ever wall-clock job waits for its next real occurrence,
            // regardless of `catch_up`. "daily 03:00" with no history means
            // "starting tomorrow morning", not "you have missed every 03:00
            // since the epoch, run right now" — and running right now is
            // exactly what the user did NOT ask for when they named a time.
            // `catch_up` still governs the case that motivates it: a fire that
            // was genuinely missed while the daemon was down, which requires a
            // `last_run` to be missed relative to in the first place.
            let occurrence = next_at_occurrence(&spec.days, spec.time, now_local)?;
            Some(occurrence.with_timezone(&Utc))
        }
        Some(last) => {
            let last_local = last.with_timezone(&Local);
            // Strictly after `last`, or a job that already ran at exactly
            // its `at` time would compute itself as due again immediately.
            let after_last =
                next_at_occurrence(&spec.days, spec.time, last_local + Duration::seconds(1))?;
            if after_last > now_local {
                return Some(after_last.with_timezone(&Utc));
            }
            if catch_up {
                return Some(after_last.with_timezone(&Utc));
            }
            let occurrence =
                next_at_occurrence(&spec.days, spec.time, now_local + Duration::seconds(1))?;
            Some(occurrence.with_timezone(&Utc))
        }
    }
}

/// The next fire time for `job`, or `None` when the job is disabled or its
/// schedule field does not parse.
///
/// A returned time at or before `now` means the job is due right now; a
/// future time is the next scheduled slot. Callers decide what "due" means
/// (run it, or hold it back for a closed window); this function only does
/// the calendar math.
pub fn next_fire(
    job: &CronJobConfig,
    last_run: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !job.enabled {
        return None;
    }
    if let Some(every) = &job.every {
        let interval_ms = parse_every(every)?;
        return next_fire_every(interval_ms, last_run, now, job.catch_up);
    }
    if let Some(at) = &job.at {
        let spec = parse_at(at)?;
        return next_fire_at(&spec, last_run, now, job.catch_up);
    }
    None
}

/// A short human description of a job's schedule, for `cron:list` output.
/// Echoes the configured spec rather than re-deriving it, since the config
/// string IS the canonical description the user wrote.
pub fn describe_schedule(job: &CronJobConfig) -> String {
    if let Some(every) = &job.every {
        return format!("every {every}");
    }
    if let Some(at) = &job.at {
        return format!("at {at}");
    }
    "invalid (no every/at)".to_string()
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod schedule_tests;
