//! Time-of-week windows constraining when ambient may run.
//!
//! The user thinks in terms of "not during weekends, not at night", so windows
//! are expressed as *allowed* ranges rather than forbidden ones. Allow-lists
//! fail closed: a range someone forgot to write is quiet time, whereas a
//! forgotten deny-rule is the machine waking you at 3am. The cost of that
//! polarity is that an empty list would mean "never run", which would silently
//! disable ambient for every existing config, so empty means *unrestricted*
//! and the constraint only exists once you ask for one.
//!
//! Everything here is pure and evaluated against a `DateTime<Local>`: a window
//! is a wall-clock statement about the user's week, so it must follow them
//! across DST rather than drifting an hour twice a year as a fixed UTC offset
//! would.

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike, Weekday};

/// A parsed window: a set of weekdays plus a daily time range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleWindow {
    /// Days this window applies to. Never empty after parsing.
    pub days: Vec<Weekday>,
    /// Inclusive start time-of-day.
    pub start: NaiveTime,
    /// Exclusive end time-of-day. When `end <= start` the window wraps past
    /// midnight onto the following day (`22:00-02:00`).
    pub end: NaiveTime,
}

impl ScheduleWindow {
    /// Whether this window wraps past midnight.
    fn wraps(&self) -> bool {
        self.end <= self.start
    }

    /// Whether `dt` falls inside this window.
    ///
    /// For a wrapping window the day-of-week is matched against the day the
    /// window *started*, not the day the instant falls on: `sat 22:00-02:00`
    /// covers Sunday 00:30, because that is the tail of Saturday night. Doing
    /// it the other way would make the small hours belong to a day the user
    /// never listed.
    pub fn contains(&self, dt: &DateTime<Local>) -> bool {
        let time = dt.time();
        let day = dt.weekday();
        if self.wraps() {
            // Head: same day, at or after start.
            if time >= self.start && self.days.contains(&day) {
                return true;
            }
            // Tail: before end, belonging to the previous day's window.
            if time < self.end && self.days.contains(&day.pred()) {
                return true;
            }
            false
        } else {
            self.days.contains(&day) && time >= self.start && time < self.end
        }
    }

    /// The next instant at or after `from` that falls inside this window.
    ///
    /// Returns `from` unchanged when already inside. Scans day by day over a
    /// bounded horizon so a pathological window can never spin forever.
    fn next_open(&self, from: &DateTime<Local>) -> Option<DateTime<Local>> {
        if self.contains(from) {
            return Some(*from);
        }
        // 8 days covers a full week plus the wrap tail.
        for day_offset in 0..8 {
            let date = (*from + Duration::days(day_offset)).date_naive();
            if !self.days.contains(&date.weekday()) {
                continue;
            }
            // Resolving a local time can be ambiguous or nonexistent across a
            // DST boundary. `.latest()` keeps us inside the window on both
            // sides rather than returning a time that never happened.
            let Some(candidate) = Local
                .from_local_datetime(&date.and_time(self.start))
                .latest()
            else {
                continue;
            };
            if candidate >= *from {
                return Some(candidate);
            }
        }
        None
    }
}

/// Parse a single window spec, e.g. `"mon-fri 09:00-19:00"` or `"sat 10:00-14:00"`.
///
/// Returns `None` for anything unparseable. The caller decides what an invalid
/// entry means; this function does not guess.
pub fn parse_window(spec: &str) -> Option<ScheduleWindow> {
    let spec = spec.trim().to_lowercase();
    let (days_part, time_part) = spec.split_once(char::is_whitespace)?;

    let days = parse_days(days_part)?;
    if days.is_empty() {
        return None;
    }

    let (start_s, end_s) = time_part.trim().split_once('-')?;
    let start = parse_time(start_s)?;
    let end = parse_time(end_s)?;

    Some(ScheduleWindow { days, start, end })
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    // 24:00 is a common way to write "end of day" and is worth accepting,
    // since the alternative (23:59) leaves a one-minute hole.
    if h == 24 && m == 0 {
        return NaiveTime::from_hms_opt(23, 59, 59);
    }
    NaiveTime::from_hms_opt(h, m, 0)
}

fn parse_day(s: &str) -> Option<Weekday> {
    match s.trim() {
        "mon" | "monday" => Some(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Some(Weekday::Tue),
        "wed" | "weds" | "wednesday" => Some(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Some(Weekday::Thu),
        "fri" | "friday" => Some(Weekday::Fri),
        "sat" | "saturday" => Some(Weekday::Sat),
        "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

/// Parse a day spec: `mon`, `mon-fri`, `mon,wed,fri`, `weekdays`, `weekends`, `daily`.
fn parse_days(s: &str) -> Option<Vec<Weekday>> {
    let s = s.trim();
    match s {
        "daily" | "everyday" | "all" => {
            return Some(vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ]);
        }
        "weekdays" => {
            return Some(vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ]);
        }
        "weekends" => return Some(vec![Weekday::Sat, Weekday::Sun]),
        _ => {}
    }

    let mut days = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some((from, to)) = part.split_once('-') {
            let from = parse_day(from)?;
            let to = parse_day(to)?;
            // Walk forward from `from` to `to` so ranges may wrap the week
            // (`fri-mon`), which is exactly how a weekend is described.
            let mut d = from;
            loop {
                if !days.contains(&d) {
                    days.push(d);
                }
                if d == to {
                    break;
                }
                d = d.succ();
                // Guard against a malformed range looping the week forever.
                if days.len() > 7 {
                    return None;
                }
            }
        } else {
            let d = parse_day(part)?;
            if !days.contains(&d) {
                days.push(d);
            }
        }
    }
    if days.is_empty() { None } else { Some(days) }
}

/// The outcome of checking the clock against the configured windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowState {
    /// No windows configured, or now is inside one.
    Open,
    /// Outside every window. Carries the next opening instant when one is
    /// known, so the caller can sleep exactly that long instead of polling.
    Closed { next_open: Option<DateTime<Local>> },
}

impl WindowState {
    pub fn is_open(&self) -> bool {
        matches!(self, WindowState::Open)
    }

    /// The next opening instant, when known and currently closed.
    pub fn next_open_at(&self) -> Option<DateTime<Local>> {
        match self {
            WindowState::Open => None,
            WindowState::Closed { next_open } => *next_open,
        }
    }
}

/// Evaluate `now` against the configured window specs.
///
/// An empty list means unrestricted. Unparseable entries are skipped by the
/// caller-facing [`parse_windows`], which reports them, so a typo cannot
/// silently narrow (or widen) when the agent is allowed to run.
pub fn evaluate(windows: &[ScheduleWindow], now: &DateTime<Local>) -> WindowState {
    if windows.is_empty() {
        return WindowState::Open;
    }
    if windows.iter().any(|w| w.contains(now)) {
        return WindowState::Open;
    }
    let next_open = windows.iter().filter_map(|w| w.next_open(now)).min();
    WindowState::Closed { next_open }
}

/// Parse every spec, returning the valid windows and the rejected specs.
///
/// Invalid entries are returned rather than dropped so the caller can warn.
/// Silently ignoring a typo in an *allow*-list would leave the agent running
/// at times the user believed they had excluded.
pub fn parse_windows(specs: &[String]) -> (Vec<ScheduleWindow>, Vec<String>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for spec in specs {
        match parse_window(spec) {
            Some(w) => ok.push(w),
            None => bad.push(spec.clone()),
        }
    }
    (ok, bad)
}

/// Seconds to sleep until the next window opens, clamped to `[1, max]`.
///
/// Clamping matters in both directions: a next-open far in the future would
/// otherwise park the runner for days past config reloads and manual triggers,
/// and a zero would spin the loop.
pub fn sleep_secs_until_open(
    now: &DateTime<Local>,
    next_open: Option<DateTime<Local>>,
    max: u64,
) -> u64 {
    match next_open {
        Some(open) => {
            let secs = (open.timestamp() - now.timestamp()).max(1) as u64;
            secs.min(max)
        }
        None => max,
    }
}

/// Human-readable summary of the configured windows, for status output.
pub fn describe(windows: &[ScheduleWindow]) -> String {
    if windows.is_empty() {
        return "unrestricted".to_string();
    }
    windows
        .iter()
        .map(|w| {
            let days: Vec<String> = w.days.iter().map(|d| format!("{:?}", d)).collect();
            format!(
                "{} {:02}:{:02}-{:02}:{:02}",
                days.join(","),
                w.start.hour(),
                w.start.minute(),
                w.end.hour(),
                w.end.minute()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
