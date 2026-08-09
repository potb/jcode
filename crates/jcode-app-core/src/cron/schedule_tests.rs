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

/// A valid `at` spec must always resolve, whatever the host timezone.
///
/// This deliberately does NOT pin `TZ`: it is the host-rules smoke check, and
/// the real DST behaviour is pinned by the `Europe/Paris` tests further down.
/// Those exist because this assertion is satisfied by any `Some`, so on a
/// UTC host (the usual CI and sandbox case) it never reaches a transition at
/// all -- it passed unchanged while `at` jobs had no next fire whatsoever on a
/// spring-forward day.
#[test]
fn at_job_always_resolves_under_host_timezone_rules() {
    let job = at_job("x", "daily 02:30");
    let now = utc(2026, 3, 8, 0, 0);
    assert!(
        next_fire(&job, None, now).is_some(),
        "a valid `at` spec must always resolve to a real instant"
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

#[test]
fn a_catch_up_anchor_collapses_a_whole_outage_into_one_rerun() {
    // Regression: anchoring a catch-up fire on the missed slot itself left the
    // job still overdue, so the next pass fired the slot after it, and so on.
    // Observed live after a 40s outage of an `every = "5s"` job: replays at
    // 30s intervals (the runner's idle poll) instead of one rerun and a
    // return to the 5s grid.
    let job = every_job("anchor", "5s");
    let missed = Utc::now() - Duration::seconds(40);
    let now = Utc::now();

    let anchor = latest_slot_not_after(&job, missed, now);

    assert!(
        anchor <= now,
        "the anchor must not be in the future, got {anchor} vs now {now}"
    );
    assert!(
        now - anchor < Duration::seconds(5),
        "the anchor must be the newest slot at or before now, so the job is no \
         longer overdue and resumes on the grid; got {} behind",
        now - anchor
    );
    // The grid is preserved: the anchor is a whole number of intervals after
    // the missed slot, not an arbitrary wall-clock instant.
    let offset_ms = (anchor - missed).num_milliseconds();
    assert_eq!(
        offset_ms % 5000,
        0,
        "the anchor must land on the schedule's grid, got {offset_ms}ms after \
         the missed slot"
    );
}

#[test]
fn an_on_time_fire_keeps_its_exact_slot_as_the_anchor() {
    // The catch-up walk must not perturb the normal path: a due-now fire is
    // anchored on its own slot, which is what keeps cadence exact.
    let job = every_job("anchor", "5s");
    let now = Utc::now();
    let due = now + Duration::milliseconds(1);

    assert_eq!(
        latest_slot_not_after(&job, due, now),
        due,
        "a slot at or after now is an on-schedule fire, not a catch-up"
    );
}

#[test]
fn a_wall_clock_job_keeps_its_occurrence_as_the_anchor() {
    // `at` jobs have no interval grid to walk, and fire at most once per
    // occurrence, so a missed occurrence is recorded as itself.
    let mut job = every_job("anchor", "5s");
    job.every = None;
    job.at = Some("daily 03:00".to_string());
    let missed = Utc::now() - Duration::hours(5);
    let now = Utc::now();

    assert_eq!(
        latest_slot_not_after(&job, missed, now),
        missed,
        "a wall-clock occurrence is its own anchor"
    );
}

#[test]
fn every_accepts_all_documented_unit_spellings_and_rejects_the_rest() {
    // parse_every accepts twenty spellings across four units. Tests covered
    // five of them, so a typo'd match arm in the other fifteen would have
    // silently turned a job into an invalid one that never fires.
    let cases: &[(&str, i64)] = &[
        ("5s", 5_000),
        ("5sec", 5_000),
        ("5secs", 5_000),
        ("5second", 5_000),
        ("5seconds", 5_000),
        ("2m", 120_000),
        ("2min", 120_000),
        ("2mins", 120_000),
        ("2minute", 120_000),
        ("2minutes", 120_000),
        ("3h", 10_800_000),
        ("3hr", 10_800_000),
        ("3hrs", 10_800_000),
        ("3hour", 10_800_000),
        ("3hours", 10_800_000),
        ("1d", 86_400_000),
        ("1day", 86_400_000),
        ("1days", 86_400_000),
        // Whitespace is forgiven, around the spec and between the number and
        // its unit. Both readings are unambiguous, so rejecting them would
        // only turn a harmless typo into a job that never fires.
        ("  4h  ", 14_400_000),
        ("5 s", 5_000),
        ("30 minutes", 1_800_000),
    ];
    for (spec, expected_ms) in cases {
        assert_eq!(
            parse_every(spec),
            Some(*expected_ms),
            "'{spec}' should parse to {expected_ms}ms"
        );
    }

    // Anything else must be rejected rather than silently coerced, so the job
    // is reported invalid instead of running on a schedule nobody asked for.
    for spec in ["", "h", "5", "5x", "-5s", "0s", "5week", "abc", "5.5h"] {
        assert_eq!(parse_every(spec), None, "'{spec}' should be rejected");
    }
}

#[test]
fn a_wall_clock_job_fires_at_its_occurrence_instead_of_skipping_a_whole_period() {
    // Regression, and the worst one in this feature: `at` jobs never fired at
    // all. The runner wakes at or *after* a deadline, so when an occurrence
    // came due `now` was already a fraction of a second past it. Searching for
    // the next occurrence strictly from `now` skipped the one happening right
    // then and returned the following period, which was filed as a future
    // deadline and skipped the same way when it arrived. A `daily 03:00` job
    // rescheduled itself a day ahead, every day, forever. Observed live: a job
    // set for "sun 04:07" reported next_run 6d 23h away at 04:07:39, having
    // silently passed over its own slot.
    let mut job = every_job("wall", "1h");
    job.every = None;
    job.at = Some("daily 12:00".to_string());

    // The instant the runner realistically observes: a hair past the slot.
    let slot_local = Local
        .with_ymd_and_hms(2026, 8, 9, 12, 0, 0)
        .single()
        .expect("unambiguous local time");
    let just_after = slot_local + Duration::milliseconds(400);

    let next = next_fire(&job, None, just_after.with_timezone(&Utc))
        .expect("a valid at-job must have a next fire");

    assert_eq!(
        next.with_timezone(&Local),
        slot_local,
        "the occurrence currently in progress must be reported as due, not \
         deferred a full period"
    );
    assert!(
        next <= just_after.with_timezone(&Utc),
        "a due-now occurrence must be at or before now so the runner fires it"
    );
}

#[test]
fn a_wall_clock_job_still_waits_when_its_occurrence_has_not_arrived() {
    // The overshoot fix must not turn into "fire on sight": a job whose time
    // has genuinely not come around yet still waits for it.
    let mut job = every_job("wall", "1h");
    job.every = None;
    job.at = Some("daily 12:00".to_string());

    let before_local = Local
        .with_ymd_and_hms(2026, 8, 9, 11, 59, 0)
        .single()
        .expect("unambiguous local time");

    let next = next_fire(&job, None, before_local.with_timezone(&Utc))
        .expect("a valid at-job must have a next fire");

    assert!(
        next > before_local.with_timezone(&Utc),
        "an occurrence that has not arrived yet must stay in the future"
    );
    assert_eq!(
        next.with_timezone(&Local).time(),
        chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        "and it must be today's 12:00, not tomorrow's"
    );
}

/// The DST tests below force a real DST timezone via the `TZ` environment
/// variable rather than trusting the host's own rules. The pre-existing
/// spring-forward test asserted only that `next_fire` returned `Some`, and it
/// passed on this machine for the wrong reason: the sandbox reports UTC, where
/// no transition exists, so the assertion never reached the code path it was
/// written for.
///
/// `chrono`'s `Local` reads `TZ` through the libc-style resolution it performs
/// per call, so setting it for the duration of the check is enough. The env
/// guard keeps that scoped, and `lock_test_env` serialises against other tests
/// that touch process-wide environment.
fn with_timezone<T>(tz: &str, f: impl FnOnce() -> T) -> T {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("TZ");
    // SAFETY-equivalent reasoning: the test-env lock serialises these mutations.
    crate::env::set_var("TZ", tz);
    let result = f();
    match previous {
        Some(value) => crate::env::set_var("TZ", value.to_string_lossy().as_ref()),
        None => crate::env::remove_var("TZ"),
    }
    result
}

#[test]
fn an_at_job_still_has_a_next_fire_on_a_spring_forward_day() {
    // Regression: `Local.from_local_datetime(..).latest()?` returns None for a
    // wall time inside the skipped hour, and the `?` aborted the entire
    // day-search rather than moving to the next day. A `daily 02:30` job in a
    // DST zone therefore had NO next fire on the transition day: the runner
    // skips a job whose next_fire is None exactly as if it had no schedule, so
    // the job went silent, and cron:list reported next=None indistinguishably
    // from an invalid spec.
    with_timezone("Europe/Paris", || {
        let job = at_job("gap", "daily 02:30");
        // 2026-03-29 is the Paris spring-forward: 02:00 jumps to 03:00, so
        // 02:30 that day does not exist.
        let day_before = Local
            .with_ymd_and_hms(2026, 3, 28, 12, 0, 0)
            .single()
            .expect("a normal midday is unambiguous");

        let next = next_fire(&job, None, day_before.with_timezone(&Utc))
            .expect("a job must keep a schedule across a DST gap");
        assert!(next > day_before.with_timezone(&Utc));

        // Advance to just before the gap and confirm the job resolves to a real
        // instant on the transition day rather than vanishing.
        let in_the_gap_day = Local
            .with_ymd_and_hms(2026, 3, 29, 1, 0, 0)
            .single()
            .expect("01:00 is before the jump and unambiguous");
        let across = next_fire(&job, None, in_the_gap_day.with_timezone(&Utc))
            .expect("the transition day must still yield a fire time");
        let across_local = across.with_timezone(&Local);
        assert_eq!(
            across_local.date_naive(),
            chrono::NaiveDate::from_ymd_opt(2026, 3, 29).unwrap(),
            "the fire should land on the transition day itself, not be deferred"
        );
        assert!(
            across_local.hour() == 3,
            "02:30 does not exist that day, so the first real instant after the \
             gap is expected, got {across_local}"
        );
    });
}

#[test]
fn an_at_job_fires_once_across_a_fall_back_repeat() {
    // The other half of the DST story: on fall-back the clock passes 02:30
    // twice. Picking the later pass means one fire that day rather than two.
    with_timezone("Europe/Paris", || {
        let job = at_job("dup", "daily 02:30");
        // 2026-10-25 is the Paris fall-back: 03:00 returns to 02:00.
        let before = Local
            .with_ymd_and_hms(2026, 10, 25, 1, 0, 0)
            .single()
            .expect("01:00 is before the repeat");

        let next = next_fire(&job, None, before.with_timezone(&Utc))
            .expect("an ambiguous local time must still resolve");
        let local = next.with_timezone(&Local);
        assert_eq!(local.hour(), 2);
        assert_eq!(local.minute(), 30);

        // Recomputing with that fire recorded must move to the NEXT day, not
        // offer the earlier pass of the same repeated hour again.
        let following =
            next_fire(&job, Some(next), next).expect("a following occurrence must exist");
        assert!(
            following > next,
            "the next fire must be strictly later, not the duplicated hour"
        );
        assert_eq!(
            following.with_timezone(&Local).date_naive(),
            chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
            "one fire per day, so the repeat must not schedule a second"
        );
    });
}

#[test]
fn a_daily_job_keeps_its_wall_clock_time_across_a_dst_change() {
    // The failure this guards against is the natural shortcut: resolve the
    // schedule to UTC once and keep adding 24h. That silently drifts an
    // `at = "daily 09:00"` job to 08:00 or 10:00 local for half the year,
    // which defeats the point of naming a wall-clock time. Each fire is
    // recomputed in local time instead, so the job tracks the clock.
    with_timezone("Europe/Paris", || {
        let job = at_job("morning", "daily 09:00");

        // A fire on the day before Paris springs forward (2026-03-29).
        let before_transition = Local
            .with_ymd_and_hms(2026, 3, 28, 9, 0, 0)
            .single()
            .expect("unambiguous");
        let next = next_fire(
            &job,
            Some(before_transition.with_timezone(&Utc)),
            before_transition.with_timezone(&Utc),
        )
        .expect("a following occurrence must exist");

        let local = next.with_timezone(&Local);
        assert_eq!(
            (local.hour(), local.minute()),
            (9, 0),
            "the job must stay at 09:00 local across the transition, got {local}"
        );
        assert_eq!(
            local.date_naive(),
            chrono::NaiveDate::from_ymd_opt(2026, 3, 29).unwrap(),
            "and it must be the very next day"
        );

        // The UTC instant genuinely shifts by an hour, which is the proof that
        // the local time was preserved rather than the offset.
        assert_ne!(
            next.with_timezone(&Utc).hour(),
            before_transition.with_timezone(&Utc).hour(),
            "holding 09:00 local across a DST change means the UTC hour moves"
        );
    });
}
