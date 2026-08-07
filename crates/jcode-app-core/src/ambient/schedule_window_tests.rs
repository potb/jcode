//! Tests for wall-clock ambient scheduling windows.

use super::schedule_window::*;
use chrono::{Local, NaiveDate, TimeZone, Weekday};

/// Build a local datetime for a given date and time.
fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Local> {
    let naive = NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(h, min, 0)
        .unwrap();
    Local.from_local_datetime(&naive).unwrap()
}

// 2026-08-03 is a Monday, 2026-08-08 a Saturday, 2026-08-09 a Sunday.

#[test]
fn parses_weekday_range_and_time() {
    let w = parse_window("mon-fri 09:00-19:00").expect("should parse");
    assert_eq!(w.days.len(), 5);
    assert!(w.days.contains(&Weekday::Mon));
    assert!(w.days.contains(&Weekday::Fri));
    assert!(!w.days.contains(&Weekday::Sat));
}

#[test]
fn parses_aliases() {
    assert_eq!(
        parse_window("weekdays 09:00-19:00").unwrap().days.len(),
        5,
        "`weekdays` should expand to Mon-Fri"
    );
    assert_eq!(
        parse_window("weekends 10:00-14:00").unwrap().days.len(),
        2,
        "`weekends` should expand to Sat-Sun"
    );
    assert_eq!(
        parse_window("daily 08:00-20:00").unwrap().days.len(),
        7,
        "`daily` should expand to all seven days"
    );
}

#[test]
fn parses_comma_list() {
    let w = parse_window("mon,wed,fri 09:00-17:00").unwrap();
    assert_eq!(w.days, vec![Weekday::Mon, Weekday::Wed, Weekday::Fri]);
}

#[test]
fn rejects_garbage() {
    assert!(parse_window("").is_none());
    assert!(parse_window("notaday 09:00-17:00").is_none());
    assert!(parse_window("mon-fri").is_none(), "missing time range");
    assert!(parse_window("mon-fri 25:00-26:00").is_none(), "bad hour");
    assert!(parse_window("mon-fri 09:00").is_none(), "missing end");
}

/// The core ask: no work on weekends.
#[test]
fn weekday_window_excludes_weekend() {
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];

    // Tuesday midday: open.
    assert!(evaluate(&windows, &local(2026, 8, 4, 12, 0)).is_open());
    // Saturday midday: closed, despite being within the time-of-day range.
    assert!(!evaluate(&windows, &local(2026, 8, 8, 12, 0)).is_open());
    // Sunday midday: closed.
    assert!(!evaluate(&windows, &local(2026, 8, 9, 12, 0)).is_open());
}

/// The other half of the ask: no work at night.
#[test]
fn weekday_window_excludes_nights() {
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];

    assert!(
        !evaluate(&windows, &local(2026, 8, 4, 3, 0)).is_open(),
        "3am Tuesday must be closed"
    );
    assert!(
        !evaluate(&windows, &local(2026, 8, 4, 22, 0)).is_open(),
        "10pm Tuesday must be closed"
    );
    assert!(
        evaluate(&windows, &local(2026, 8, 4, 9, 0)).is_open(),
        "start is inclusive"
    );
    assert!(
        !evaluate(&windows, &local(2026, 8, 4, 19, 0)).is_open(),
        "end is exclusive"
    );
}

/// A window crossing midnight keeps its tail attached to the day it opened.
/// Getting this wrong makes "friday night" mean the wrong small hours.
#[test]
fn wrapping_window_covers_small_hours_of_next_day() {
    let windows = vec![parse_window("fri 22:00-02:00").unwrap()];

    // Friday 23:00: inside the head.
    assert!(evaluate(&windows, &local(2026, 8, 7, 23, 0)).is_open());
    // Saturday 01:00: inside the tail of Friday's window.
    assert!(evaluate(&windows, &local(2026, 8, 8, 1, 0)).is_open());
    // Saturday 03:00: past the tail.
    assert!(!evaluate(&windows, &local(2026, 8, 8, 3, 0)).is_open());
    // Friday 21:00: before the head.
    assert!(!evaluate(&windows, &local(2026, 8, 7, 21, 0)).is_open());
    // Thursday 01:00 is the tail of *Wednesday*, which is not configured.
    assert!(!evaluate(&windows, &local(2026, 8, 6, 1, 0)).is_open());
}

/// Empty config must not mean "never run" — that would silently disable
/// ambient for every user who never asked for a constraint.
#[test]
fn no_windows_means_unrestricted() {
    assert!(
        evaluate(&[], &local(2026, 8, 9, 3, 0)).is_open(),
        "3am Sunday with no windows configured must still be open"
    );
}

#[test]
fn multiple_windows_union() {
    let windows = vec![
        parse_window("weekdays 09:00-19:00").unwrap(),
        parse_window("sat 10:00-14:00").unwrap(),
    ];
    assert!(evaluate(&windows, &local(2026, 8, 4, 12, 0)).is_open());
    assert!(
        evaluate(&windows, &local(2026, 8, 8, 11, 0)).is_open(),
        "Saturday morning is covered by the second window"
    );
    assert!(
        !evaluate(&windows, &local(2026, 8, 8, 16, 0)).is_open(),
        "Saturday afternoon is outside both"
    );
}

/// Closed state must say when it reopens, so the runner sleeps instead of
/// polling every 30s for a whole weekend.
#[test]
fn closed_state_reports_next_open() {
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];
    // Saturday afternoon → next open is Monday 09:00.
    let state = evaluate(&windows, &local(2026, 8, 8, 15, 0));
    let next = state.next_open_at().expect("should know when it reopens");
    assert_eq!(next, local(2026, 8, 10, 9, 0));
}

#[test]
fn closed_at_night_reopens_same_morning() {
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];
    // Tuesday 03:00 → opens Tuesday 09:00.
    let state = evaluate(&windows, &local(2026, 8, 4, 3, 0));
    assert_eq!(state.next_open_at().unwrap(), local(2026, 8, 4, 9, 0));
}

#[test]
fn evening_close_reopens_next_morning() {
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];
    // Tuesday 20:00 → opens Wednesday 09:00.
    let state = evaluate(&windows, &local(2026, 8, 4, 20, 0));
    assert_eq!(state.next_open_at().unwrap(), local(2026, 8, 5, 9, 0));
}

#[test]
fn open_state_has_no_next_open() {
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];
    assert!(
        evaluate(&windows, &local(2026, 8, 4, 12, 0))
            .next_open_at()
            .is_none()
    );
}

/// A far-off reopening must not park the runner past config edits or manual
/// triggers.
#[test]
fn sleep_until_open_is_capped() {
    let now = local(2026, 8, 8, 15, 0);
    let monday = local(2026, 8, 10, 9, 0);
    let secs = sleep_secs_until_open(&now, Some(monday), 3600);
    assert_eq!(secs, 3600, "should clamp to the cap, not sleep for 42 hours");
}

#[test]
fn sleep_until_open_uses_real_gap_when_short() {
    let now = local(2026, 8, 4, 8, 30);
    let open = local(2026, 8, 4, 9, 0);
    assert_eq!(sleep_secs_until_open(&now, Some(open), 3600), 1800);
}

/// Never return 0: that would spin the idle loop.
#[test]
fn sleep_until_open_never_zero() {
    let now = local(2026, 8, 4, 9, 0);
    assert!(sleep_secs_until_open(&now, Some(now), 3600) >= 1);
    let past = local(2026, 8, 4, 8, 0);
    assert!(sleep_secs_until_open(&now, Some(past), 3600) >= 1);
}

#[test]
fn sleep_falls_back_to_cap_without_next_open() {
    let now = local(2026, 8, 4, 9, 0);
    assert_eq!(sleep_secs_until_open(&now, None, 900), 900);
}

/// Invalid entries are surfaced, not silently dropped: in an allow-list a
/// dropped typo widens when ambient may run.
#[test]
fn parse_windows_reports_bad_specs() {
    let specs = vec![
        "weekdays 09:00-19:00".to_string(),
        "garbage".to_string(),
        "sat 10:00-14:00".to_string(),
    ];
    let (ok, bad) = parse_windows(&specs);
    assert_eq!(ok.len(), 2);
    assert_eq!(bad, vec!["garbage".to_string()]);
}

/// If every configured window is invalid we must fail OPEN, not lock the
/// agent out of running entirely on a typo.
#[test]
fn all_invalid_specs_fail_open() {
    let specs = vec!["garbage".to_string(), "alsobad".to_string()];
    let (ok, bad) = parse_windows(&specs);
    assert!(ok.is_empty());
    assert_eq!(bad.len(), 2);
    assert!(
        evaluate(&ok, &local(2026, 8, 9, 3, 0)).is_open(),
        "no usable windows must behave like unrestricted"
    );
}

#[test]
fn describe_is_readable() {
    assert_eq!(describe(&[]), "unrestricted");
    let windows = vec![parse_window("weekdays 09:00-19:00").unwrap()];
    let d = describe(&windows);
    assert!(d.contains("09:00"), "got: {d}");
    assert!(d.contains("19:00"), "got: {d}");
    assert!(d.contains("Mon"), "got: {d}");
}

#[test]
fn case_and_whitespace_insensitive() {
    assert!(parse_window("  MON-FRI  09:00-19:00  ").is_some());
    assert!(parse_window("Weekdays 09:00-19:00").is_some());
}

/// 24:00 is a natural way to write end-of-day; rejecting it leaves a
/// one-minute hole the user did not ask for.
#[test]
fn accepts_end_of_day() {
    let w = parse_window("daily 09:00-24:00").expect("24:00 should parse");
    let windows = vec![w];
    assert!(evaluate(&windows, &local(2026, 8, 4, 23, 30)).is_open());
}

/// A day range may wrap the week, which is how a weekend gets described.
#[test]
fn day_range_wraps_the_week() {
    let w = parse_window("sat-sun 10:00-14:00").unwrap();
    assert_eq!(w.days.len(), 2);
    let w = parse_window("fri-mon 10:00-14:00").unwrap();
    assert_eq!(w.days.len(), 4, "fri,sat,sun,mon");
    assert!(w.days.contains(&Weekday::Mon));
}
