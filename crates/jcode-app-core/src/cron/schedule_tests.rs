//! Tests for cron scheduling math: `next_fire` and its `every`/`at` parsers.

use super::*;
use crate::config::CronJobConfig;
use chrono::{Local, NaiveDate, TimeZone, Utc};

fn every_job(id: &str, every: &str) -> CronJobConfig {
    CronJobConfig {
        id: id.to_string(),
        every: Some(every.to_string()),
        command: Some("true".to_string()),
        ..Default::default()
    }
}

fn at_job(id: &str, at: &str) -> CronJobConfig {
    CronJobConfig {
        id: id.to_string(),
        at: Some(at.to_string()),
        command: Some("true".to_string()),
        ..Default::default()
    }
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
}

/// Local time for the `at` tests, since `at` is evaluated in local time.
fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Local> {
    let naive = NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(h, min, 0)
        .unwrap();
    Local.from_local_datetime(&naive).unwrap()
}

// ---------------------------------------------------------------------------
// parse_every
// ---------------------------------------------------------------------------

#[test]
fn parse_every_accepts_common_units() {
    assert_eq!(parse_every("30m"), Some(30 * 60_000));
    assert_eq!(parse_every("6h"), Some(6 * 3_600_000));
    assert_eq!(parse_every("1d"), Some(86_400_000));
    assert_eq!(parse_every("90s"), Some(90_000));
}

#[test]
fn parse_every_rejects_garbage() {
    assert_eq!(parse_every(""), None);
    assert_eq!(parse_every("6"), None, "missing unit");
    assert_eq!(parse_every("h6"), None, "unit before number");
    assert_eq!(parse_every("0h"), None, "zero is not a valid interval");
    assert_eq!(parse_every("-1h"), None, "negative");
    assert_eq!(parse_every("6w"), None, "unsupported unit");
}

// ---------------------------------------------------------------------------
// parse_at
// ---------------------------------------------------------------------------

#[test]
fn parse_at_accepts_day_spec_and_time() {
    assert!(parse_at("daily 03:00").is_some());
    assert!(parse_at("weekdays 09:00").is_some());
    assert!(parse_at("mon,thu 18:30").is_some());
}

#[test]
fn parse_at_rejects_garbage() {
    assert!(parse_at("").is_none());
    assert!(parse_at("daily").is_none(), "missing time");
    assert!(parse_at("notaday 03:00").is_none());
    assert!(parse_at("daily 25:00").is_none(), "bad hour");
}

// ---------------------------------------------------------------------------
// next_fire: `every`
// ---------------------------------------------------------------------------

#[test]
fn every_job_first_run_fires_now_with_catch_up() {
    let job = every_job("x", "6h");
    let now = utc(2026, 1, 1, 12, 0);
    assert_eq!(next_fire(&job, None, now), Some(now));
}

#[test]
fn every_job_first_run_waits_one_interval_without_catch_up() {
    let mut job = every_job("x", "6h");
    job.catch_up = false;
    let now = utc(2026, 1, 1, 12, 0);
    assert_eq!(
        next_fire(&job, None, now),
        Some(now + chrono::Duration::hours(6))
    );
}

#[test]
fn every_job_computes_next_slot_from_last_run() {
    let job = every_job("x", "6h");
    let last_run = utc(2026, 1, 1, 6, 0);
    let now = utc(2026, 1, 1, 7, 0);
    assert_eq!(
        next_fire(&job, Some(last_run), now),
        Some(utc(2026, 1, 1, 12, 0))
    );
}

#[test]
fn every_job_missed_fire_reruns_immediately_with_catch_up() {
    let job = every_job("x", "6h");
    // last run was 20h ago; three periods have elapsed.
    let last_run = utc(2026, 1, 1, 0, 0);
    let now = utc(2026, 1, 1, 20, 0);
    assert_eq!(
        next_fire(&job, Some(last_run), now),
        Some(utc(2026, 1, 1, 6, 0)),
        "catch_up should fire at the first missed slot, not skip to now"
    );
}

#[test]
fn every_job_missed_fire_skips_to_next_slot_without_catch_up() {
    let mut job = every_job("x", "6h");
    job.catch_up = false;
    let last_run = utc(2026, 1, 1, 0, 0);
    let now = utc(2026, 1, 1, 20, 0);
    assert_eq!(
        next_fire(&job, Some(last_run), now),
        Some(utc(2026, 1, 2, 0, 0)),
        "without catch_up, missed slots are skipped entirely, landing on the \
         next slot strictly after now"
    );
}

#[test]
fn every_job_not_yet_due_returns_future_slot() {
    let job = every_job("x", "6h");
    let last_run = utc(2026, 1, 1, 6, 0);
    let now = utc(2026, 1, 1, 8, 0);
    assert_eq!(
        next_fire(&job, Some(last_run), now),
        Some(utc(2026, 1, 1, 12, 0))
    );
}

#[test]
fn disabled_job_never_fires() {
    let mut job = every_job("x", "6h");
    job.enabled = false;
    assert_eq!(next_fire(&job, None, utc(2026, 1, 1, 0, 0)), None);
}

#[test]
fn invalid_every_spec_yields_no_fire_time() {
    let job = every_job("x", "not-a-duration");
    assert_eq!(next_fire(&job, None, utc(2026, 1, 1, 0, 0)), None);
}

// ---------------------------------------------------------------------------
// next_fire: `at`
// ---------------------------------------------------------------------------

#[test]
fn at_job_first_run_waits_for_the_next_occurrence_even_with_catch_up() {
    // Naming a time of day is a statement about WHEN, so a job with no history
    // must not fire the moment the daemon first sees it. This was a real bug:
    // a `daily 03:00` upstream-merge job ran on the spot at 01:53 the first
    // time the config was loaded.
    let job = at_job("x", "daily 03:00");
    assert!(job.catch_up, "catch_up defaults on; that is the point here");
    // 2026-01-01 is a Thursday, local time.
    let now = local(2026, 1, 1, 12, 0).with_timezone(&Utc);
    let expected = local(2026, 1, 2, 3, 0).with_timezone(&Utc);
    assert_eq!(next_fire(&job, None, now), Some(expected));
}

#[test]
fn at_job_first_run_without_catch_up_waits_for_next_occurrence() {
    let mut job = at_job("x", "daily 03:00");
    job.catch_up = false;
    // 2026-01-01 is a Thursday, local time.
    let now = local(2026, 1, 1, 12, 0).with_timezone(&Utc);
    let expected = local(2026, 1, 2, 3, 0).with_timezone(&Utc);
    assert_eq!(next_fire(&job, None, now), Some(expected));
}

#[test]
fn at_job_after_running_today_waits_for_tomorrow() {
    let job = at_job("x", "daily 03:00");
    let last_run = local(2026, 1, 1, 3, 0).with_timezone(&Utc);
    let now = local(2026, 1, 1, 10, 0).with_timezone(&Utc);
    let expected = local(2026, 1, 2, 3, 0).with_timezone(&Utc);
    assert_eq!(next_fire(&job, Some(last_run), now), Some(expected));
}

#[test]
fn at_job_weekday_only_skips_weekend() {
    let job = at_job("x", "weekdays 09:00");
    // 2026-01-02 is a Friday; the next weekday occurrence after running
    // Friday morning is Monday, not Saturday.
    let last_run = local(2026, 1, 2, 9, 0).with_timezone(&Utc);
    let now = local(2026, 1, 2, 12, 0).with_timezone(&Utc);
    let expected = local(2026, 1, 5, 9, 0).with_timezone(&Utc);
    assert_eq!(next_fire(&job, Some(last_run), now), Some(expected));
}

#[test]
fn at_job_missed_fire_without_catch_up_skips_to_next_slot() {
    let mut job = at_job("x", "daily 03:00");
    job.catch_up = false;
    // Job was due yesterday at 03:00 but the daemon was down; without
    // catch_up it must not fire retroactively, only at the *next* 03:00.
    let last_run = local(2025, 12, 30, 3, 0).with_timezone(&Utc);
    let now = local(2026, 1, 1, 12, 0).with_timezone(&Utc);
    let expected = local(2026, 1, 2, 3, 0).with_timezone(&Utc);
    assert_eq!(next_fire(&job, Some(last_run), now), Some(expected));
}

#[test]
fn at_job_missed_fire_with_catch_up_fires_at_the_missed_slot() {
    let job = at_job("x", "daily 03:00");
    let last_run = local(2025, 12, 30, 3, 0).with_timezone(&Utc);
    let now = local(2026, 1, 1, 12, 0).with_timezone(&Utc);
    let expected = local(2025, 12, 31, 3, 0).with_timezone(&Utc);
    assert_eq!(
        next_fire(&job, Some(last_run), now),
        Some(expected),
        "catch_up fires at the first missed occurrence, not the most recent"
    );
}

/// DST boundary: US spring-forward 2026-03-08 02:00 -> 03:00 local. A job
/// scheduled inside the skipped hour must still resolve to a real instant
/// (`ScheduleWindow` handles this the same way via `.latest()`).
#[test]
fn at_job_handles_dst_spring_forward_gap() {
    // This test only exercises the local system's own DST rules, which in
    // CI/sandbox environments is typically UTC (no DST). The assertion is
    // structural: next_fire must always return Some for a valid spec rather
    // than silently swallowing the ambiguous/nonexistent local time.
    let job = at_job("x", "daily 02:30");
    let now = utc(2026, 3, 8, 0, 0);
    assert!(
        next_fire(&job, None, now).is_some(),
        "a valid `at` spec must always resolve to a real instant, even across a DST gap"
    );
}

// ---------------------------------------------------------------------------
// mode validity
// ---------------------------------------------------------------------------

#[test]
fn next_fire_returns_none_for_a_structurally_invalid_job() {
    // Neither every nor at set.
    let job = CronJobConfig {
        id: "x".to_string(),
        command: Some("true".to_string()),
        ..Default::default()
    };
    assert_eq!(next_fire(&job, None, utc(2026, 1, 1, 0, 0)), None);
}

#[test]
fn describe_schedule_echoes_the_configured_spec() {
    assert_eq!(describe_schedule(&every_job("x", "6h")), "every 6h");
    assert_eq!(
        describe_schedule(&at_job("x", "daily 03:00")),
        "at daily 03:00"
    );
}
