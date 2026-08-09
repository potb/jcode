//! Pure scheduling math for `[[cron]]` jobs: parsing `every`/`at` and
//! computing the next fire time.
//!
//! Kept free of I/O on purpose. `next_fire` is the one function the runner
//! loop actually needs, and it is cheap to get subtly wrong (off-by-one
//! periods, DST, "what does no history mean"), so it is worth being able to
//! unit-test it against a fixed clock without touching disk or config.

use crate::ambient::schedule_window::{parse_days, parse_time};
use crate::config::CronJobConfig;
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike, Utc, Weekday};

/// A parsed `at` schedule: fire at `time` on each of `days`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AtSchedule {
    days: Vec<Weekday>,
    time: NaiveTime,
}

/// Parse an `every` duration spec: a number followed by `s`/`m`/`h`/`d`
/// (`"30m"`, `"6h"`, `"1d"`). Each unit also accepts its longer spellings
/// (`sec`/`secs`/`second`/`seconds`, and likewise for minutes, hours, days).
/// Returns the interval in milliseconds so callers can do integer arithmetic
/// without pulling in `chrono::Duration`'s `i32`-only `Mul` impl, which would
/// overflow on long gaps.
///
/// Whitespace is forgiven around the spec and between the number and its unit,
/// so `"6h"`, `" 6h "` and `"6 h"` are the same interval. Both readings are
/// unambiguous, and being strict here would turn a harmless typo into a job
/// that is silently invalid and never fires. Anything genuinely ambiguous or
/// unsupported (a bare number, a fractional value, an unknown unit, zero or
/// negative) is still rejected so the job is reported invalid instead.
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
        let naive = date.and_time(time);
        let candidate = match Local.from_local_datetime(&naive) {
            // Unambiguous, or the fall-back repeat where the clock passes the
            // same wall time twice. `.latest()` picks the second pass, so the
            // job fires once that day rather than twice.
            chrono::LocalResult::Single(t) => t,
            chrono::LocalResult::Ambiguous(_, latest) => latest,
            // Spring forward: this wall time never happens. Do NOT abort the
            // search. `.latest()?` used to return None for the whole
            // function, so a `daily 02:30` job in a DST zone had no next fire
            // at all on the transition day, was skipped by the runner as if it
            // had no schedule, and stayed silent for that day. Advance to the
            // first real instant after the gap instead, which is what a user
            // who asked for "02:30" means on the one day 02:30 does not exist.
            chrono::LocalResult::None => {
                let mut probe = naive;
                let mut resolved = None;
                // The largest DST jump in the tz database is an hour, but step
                // minute by minute for two so an unusual rule cannot fall
                // through, and stop at the first instant that exists.
                for _ in 0..120 {
                    probe += Duration::minutes(1);
                    if let Some(t) = Local.from_local_datetime(&probe).latest() {
                        resolved = Some(t);
                        break;
                    }
                }
                resolved?
            }
        };
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
            //
            // Search from the start of the current minute, not from `now`. The
            // runner wakes at or *after* a deadline, never before, so at the
            // moment an occurrence comes due `now` is already a fraction of a
            // second past it. Searching from `now` skips that occurrence and
            // returns the following one, which is filed as a future deadline
            // and then skipped in turn: a `daily 03:00` job rescheduled itself
            // a day ahead every day and never fired at all. Rounding down to
            // the minute makes the occurrence that is currently happening
            // count as due, which is the resolution `at` specs are written in.
            let search_from = now_local
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now_local);
            let occurrence = next_at_occurrence(&spec.days, spec.time, search_from)?;
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

/// The newest scheduled slot at or before `now`, starting from `due`.
///
/// Used to anchor a catch-up fire. `due` is the slot that came up while the
/// daemon was down; recording it verbatim would leave the job overdue and make
/// the next pass fire the following missed slot, draining the whole backlog.
/// Walking forward to the last slot that is not in the future collapses an
/// outage of any length into a single rerun, then resumes on the grid.
///
/// Only interval (`every`) jobs have a grid to walk. A wall-clock (`at`) job
/// fires at most once per occurrence anyway, so its `due` is returned as-is.
/// A `due` already at or after `now` is returned unchanged: that is an
/// on-schedule fire, not a catch-up.
pub fn latest_slot_not_after(
    job: &CronJobConfig,
    due: DateTime<Utc>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    if due >= now {
        return due;
    }
    let Some(every) = &job.every else {
        return due;
    };
    let Some(interval_ms) = parse_every(every) else {
        return due;
    };
    if interval_ms <= 0 {
        return due;
    }
    let behind_ms = (now - due).num_milliseconds();
    let periods = behind_ms / interval_ms;
    due + Duration::milliseconds(interval_ms * periods)
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
