//! The pinned provider-usage block (`display.pin_usage`).
//!
//! Unlike the margin `UsageLimits` info widget - which only docks when the
//! transcript happens to leave free margin width, and so silently disappears on
//! narrow terminals or beside long lines - this block owns permanently reserved
//! rows at the very bottom of the terminal. That makes "always pinned" literally
//! true at every terminal size, which is the whole point of the feature.
//!
//! The block is right-aligned and stacked one window per line, mirroring the
//! session facts above it:
//!
//! ```text
//!                                                            Anthropic
//!                                    5-hour ▰▰▰▱▱ 60% · 3h 31m
//!                                    Weekly ▰▱▱▱▱ 13% · 6d 14h
//! ```
//!
//! Line count follows whatever the provider actually exposes, so a provider with
//! three windows gets four rows and a cost-billed provider gets one. Height is
//! capped so the block can never crowd out the transcript on a short terminal.
//!
//! Each line still degrades by width rather than being truncated blindly, widest
//! form first:
//!
//! * `Bars`     - `5-hour ▰▰▰▱▱ 60% · 3h 31m`
//! * `Percents` - `5h 60%` (label abbreviated, bar and countdown dropped)
//! * `Minimal`  - `60%`
//!
//! A form is used only when it actually fits, so a line never wraps or gets
//! chopped mid-glyph.

use crate::tui::color_support::rgb;
use crate::tui::info_widget::{UsageInfo, UsageProvider};
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

/// Width of the mini bar drawn in the `Bars` tier.
const BAR_CELLS: usize = 5;

/// Minimum width below which no footer content is drawn at all. Anything
/// narrower cannot hold even `100%` plus a space of breathing room.
const MIN_FOOTER_WIDTH: u16 = 5;

/// One quota window as the footer needs it: a long label for wide terminals, a
/// short one for narrow terminals, the utilization, and an optional countdown.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FooterWindow {
    pub long_label: String,
    pub short_label: String,
    pub used_pct: u8,
    pub reset: Option<String>,
}

/// Which rendering tier the footer chose for a given width. Exposed so tests
/// (and the debug socket) can assert the degradation ladder directly rather
/// than by string-matching rendered output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FooterTier {
    Bars,
    Percents,
    Minimal,
    Hidden,
}

impl FooterTier {
    /// How rich this form is; higher means more detail survived.
    fn richness(self) -> u8 {
        match self {
            FooterTier::Hidden => 0,
            FooterTier::Minimal => 1,
            FooterTier::Percents => 2,
            FooterTier::Bars => 3,
        }
    }

    /// The narrower of two forms. The block reports the least detailed form any
    /// of its lines had to fall back to.
    fn min_form(self, other: Self) -> Self {
        if other.richness() < self.richness() {
            other
        } else {
            self
        }
    }
}

/// Colour for a utilization value, matching the info-widget usage bars so the
/// footer and the margin widget never disagree about what "nearly out" looks
/// like. Keyed on *remaining* percent.
fn usage_color(used_pct: u8) -> Color {
    let left = 100u8.saturating_sub(used_pct);
    if left <= 20 {
        rgb(255, 100, 100)
    } else if left <= 50 {
        rgb(255, 200, 100)
    } else {
        rgb(100, 200, 100)
    }
}

fn label_style() -> Style {
    Style::default().fg(rgb(140, 140, 150))
}

fn dim_style() -> Style {
    Style::default().fg(rgb(100, 100, 110))
}

/// Abbreviate a provider-reported window label for the narrow tiers.
/// `5-hour`/`5h window` become `5h`, `Weekly`/`7-day` become `wk`, `Monthly`
/// becomes `mo`. Unknown labels keep their first two characters so a provider
/// inventing a new window name still renders something meaningful.
pub(super) fn short_window_label(label: &str) -> String {
    let lower = label.trim().to_ascii_lowercase();
    if lower.contains("5-hour") || lower.contains("5 hour") || lower.starts_with("5h") {
        return "5h".to_string();
    }
    if lower.contains("week") || lower.contains("7-day") || lower.contains("7 day") {
        return "wk".to_string();
    }
    if lower.contains("month") {
        return "mo".to_string();
    }
    if lower.contains("spark") {
        return "sp".to_string();
    }
    if lower.contains("hour") {
        return "hr".to_string();
    }
    if lower.contains("day") {
        return "dy".to_string();
    }
    let short: String = lower.chars().take(2).collect();
    if short.is_empty() {
        "usage".to_string()
    } else {
        short
    }
}

fn pct(ratio: f32) -> u8 {
    (ratio * 100.0).round().clamp(0.0, 100.0) as u8
}

/// Extract the quota windows the footer should show for this provider.
///
/// Subscription providers (Anthropic/OpenAI OAuth) report real windows. Cost-
/// and token-based providers have no quota at all, so the footer shows their
/// running spend/token totals instead of pretending a percentage exists - see
/// `cost_line`.
pub(super) fn footer_windows(info: &UsageInfo) -> Vec<FooterWindow> {
    let mut windows = Vec::new();
    if let Some(label) = info.primary_limit_label.as_deref() {
        windows.push(FooterWindow {
            long_label: label.to_string(),
            short_label: short_window_label(label),
            used_pct: pct(info.five_hour),
            reset: info
                .five_hour_resets_at
                .as_deref()
                .map(crate::usage::format_reset_time),
        });
    }
    if let Some(label) = info.secondary_limit_label.as_deref() {
        windows.push(FooterWindow {
            long_label: label.to_string(),
            short_label: short_window_label(label),
            used_pct: pct(info.seven_day),
            reset: info
                .seven_day_resets_at
                .as_deref()
                .map(crate::usage::format_reset_time),
        });
    }
    if let Some(spark) = info.spark {
        windows.push(FooterWindow {
            long_label: "Spark".to_string(),
            short_label: "sp".to_string(),
            used_pct: pct(spark),
            reset: info
                .spark_resets_at
                .as_deref()
                .map(crate::usage::format_reset_time),
        });
    }
    windows
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn mini_bar(used_pct: u8) -> Vec<Span<'static>> {
    let filled = ((used_pct as f32 / 100.0) * BAR_CELLS as f32).round() as usize;
    let filled = filled.min(BAR_CELLS);
    let empty = BAR_CELLS - filled;
    let mut spans = Vec::new();
    if filled > 0 {
        spans.push(Span::styled(
            "▰".repeat(filled),
            Style::default().fg(usage_color(used_pct)),
        ));
    }
    if empty > 0 {
        spans.push(Span::styled(
            "▱".repeat(empty),
            Style::default().fg(rgb(50, 50, 60)),
        ));
    }
    spans
}

/// Widest form for one window: `label bar pct · reset`.
fn window_bars_spans(window: &FooterWindow) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!("{} ", window.long_label),
        label_style(),
    )];
    spans.extend(mini_bar(window.used_pct));
    spans.push(Span::styled(
        format!(" {}%", window.used_pct),
        Style::default().fg(usage_color(window.used_pct)),
    ));
    if let Some(reset) = window.reset.as_deref() {
        spans.push(Span::styled(format!(" · {reset}"), dim_style()));
    }
    spans
}

/// Middle form for one window: `5h 60%`. Abbreviated label, no bar, no reset.
fn window_percents_spans(window: &FooterWindow) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{} ", window.short_label), label_style()),
        Span::styled(
            format!("{}%", window.used_pct),
            Style::default().fg(usage_color(window.used_pct)),
        ),
    ]
}

/// Narrowest form for one window: just the number.
fn window_minimal_spans(window: &FooterWindow) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("{}%", window.used_pct),
        Style::default().fg(usage_color(window.used_pct)),
    )]
}

/// Pick the widest form of `window` that fits `width`, or `None` when even the
/// bare percentage does not.
fn window_line_spans(window: &FooterWindow, width: usize) -> Option<Vec<Span<'static>>> {
    for build in [
        window_bars_spans as fn(&FooterWindow) -> Vec<Span<'static>>,
        window_percents_spans,
        window_minimal_spans,
    ] {
        let spans = build(window);
        if spans_width(&spans) <= width {
            return Some(spans);
        }
    }
    None
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

/// Providers billed per token have no quota window, so the footer reports what
/// it actually knows: spend (cost-based) or token counts (Copilot). Same
/// widest-that-fits ladder as the quota tiers.
fn cost_spans(info: &UsageInfo, width: usize) -> Vec<Span<'static>> {
    let tokens = format!(
        "{} in + {} out",
        format_tokens(info.input_tokens),
        format_tokens(info.output_tokens)
    );

    if matches!(info.provider, UsageProvider::Copilot) {
        let full = vec![Span::styled(tokens.clone(), label_style())];
        if spans_width(&full) <= width {
            return full;
        }
        let compact = format!(
            "{}/{}",
            format_tokens(info.input_tokens),
            format_tokens(info.output_tokens)
        );
        let compact_spans = vec![Span::styled(compact, label_style())];
        if spans_width(&compact_spans) <= width {
            return compact_spans;
        }
        return Vec::new();
    }

    let cost = format!("${:.4}", info.total_cost);
    let full = vec![
        Span::styled(cost.clone(), Style::default().fg(rgb(180, 180, 190))),
        Span::styled(format!(" · {tokens}"), label_style()),
    ];
    if spans_width(&full) <= width {
        return full;
    }
    let just_cost = vec![Span::styled(cost, Style::default().fg(rgb(180, 180, 190)))];
    if spans_width(&just_cost) <= width {
        return just_cost;
    }
    Vec::new()
}

/// Whether there is anything worth pinning for this provider.
///
/// `available` alone is too strict for a pinned line: when the provider's usage
/// API rate-limits a refresh, `available` flips false while the last-known
/// window values are still in hand. The margin widget is happy to disappear in
/// that case, but a line the user deliberately pinned should not blink out of
/// existence over a transient 429, so stale-but-known readings still render
/// (marked with a `~`).
pub(super) fn has_showable_usage(info: &UsageInfo) -> bool {
    info.available || info.stale
}

/// Maximum rows the pinned block may occupy, regardless of how many windows a
/// provider reports. Keeps a chatty provider from eating the transcript.
const MAX_FOOTER_LINES: u16 = 6;

/// Build the pinned block for `info` at `width` columns: a provider heading
/// followed by one line per quota window, plus the chosen tier so callers (and
/// tests) can reason about width degradation.
///
/// The heading is dropped before any window line is, since the windows carry the
/// numbers the user is actually watching.
pub(super) fn usage_footer_lines(info: &UsageInfo, width: u16) -> (FooterTier, Vec<Line<'static>>) {
    if !has_showable_usage(info) || width < MIN_FOOTER_WIDTH {
        return (FooterTier::Hidden, Vec::new());
    }
    let width = width as usize;

    // Cost- and token-billed providers have no quota window, so they stay a
    // single line reporting what is actually known.
    if matches!(
        info.provider,
        UsageProvider::CostBased | UsageProvider::Copilot
    ) {
        let spans = cost_spans(info, width);
        if spans.is_empty() {
            return (FooterTier::Hidden, Vec::new());
        }
        return (FooterTier::Bars, vec![Line::from(spans)]);
    }

    let windows = footer_windows(info);
    if windows.is_empty() {
        return (FooterTier::Hidden, Vec::new());
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut tier = FooterTier::Bars;

    // Heading: `Anthropic limits`, or `~Anthropic limits` when the numbers are a
    // last-known reading kept after a failed refresh (see `has_showable_usage`).
    let label = info.provider.label();
    if !label.is_empty() {
        let heading = if info.available {
            format!("{label} limits")
        } else {
            format!("~{label} limits")
        };
        if UnicodeWidthStr::width(heading.as_str()) <= width {
            lines.push(Line::from(Span::styled(heading, dim_style())));
        }
    }

    for window in &windows {
        let Some(spans) = window_line_spans(window, width) else {
            continue;
        };
        // Track the narrowest form any window had to fall back to, so the
        // reported tier describes the block as a whole.
        let observed = if spans_width(&spans) == spans_width(&window_bars_spans(window)) {
            FooterTier::Bars
        } else if spans.len() > 1 {
            FooterTier::Percents
        } else {
            FooterTier::Minimal
        };
        tier = tier.min_form(observed);
        lines.push(Line::from(spans));
    }

    // A heading with no window under it is not worth a reserved row.
    let window_lines = lines.len() - usize::from(!label.is_empty() && !lines.is_empty());
    if window_lines == 0 {
        return (FooterTier::Hidden, Vec::new());
    }

    // Drop the heading first if the block would exceed its height budget.
    while lines.len() > MAX_FOOTER_LINES as usize {
        lines.remove(0);
    }

    (tier, lines)
}

/// How many rows the pinned block needs at `width`. Zero when nothing renders.
///
/// The layout calls this before drawing so the reservation always matches what
/// the renderer will actually paint; deriving the height from the same line
/// builder is what keeps the two from drifting apart.
pub(super) fn usage_footer_height(app: &dyn crate::tui::TuiState, width: u16) -> u16 {
    if !crate::config::config().display.pin_usage {
        return 0;
    }
    let data = app.info_widget_data();
    let Some(info) = data.usage_info.as_ref() else {
        return 0;
    };
    let (tier, lines) = usage_footer_lines(info, width);
    if tier == FooterTier::Hidden {
        return 0;
    }
    (lines.len() as u16).min(MAX_FOOTER_LINES)
}

/// Paint `info` into `area`. Split out from [`draw_usage_footer`] so tests can
/// drive the real painting path with a synthetic snapshot: the app-level test
/// harness runs on a mock provider that reports no usage at all, so going
/// through `TuiState` there would silently assert nothing.
fn draw_usage_footer_into(frame: &mut Frame, info: &UsageInfo, area: Rect, _centered: bool) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let (tier, lines) = usage_footer_lines(info, area.width);
    if tier == FooterTier::Hidden || lines.is_empty() {
        return;
    }
    // Right-aligned so the block reads as a continuation of the session facts
    // stacked above it, rather than as a stray centered banner.
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines).alignment(Alignment::Right),
        area,
    );
}

/// The plain text the footer paints at `width`, for the visual-debug capture.
/// Returns an empty string when nothing is drawn.
pub(super) fn usage_footer_debug_text(app: &dyn crate::tui::TuiState, width: u16) -> String {
    let data = app.info_widget_data();
    let Some(info) = data.usage_info.as_ref() else {
        return String::new();
    };
    let (_, lines) = usage_footer_lines(info, width);
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Draw the pinned usage footer into its reserved row.
pub(super) fn draw_usage_footer(frame: &mut Frame, app: &dyn crate::tui::TuiState, area: Rect) {
    let data = app.info_widget_data();
    let Some(info) = data.usage_info.as_ref() else {
        return;
    };
    draw_usage_footer_into(frame, info, area, app.centered_mode());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic_info() -> UsageInfo {
        UsageInfo {
            provider: UsageProvider::Anthropic,
            primary_limit_label: Some("5-hour".to_string()),
            five_hour: 0.60,
            five_hour_resets_at: None,
            secondary_limit_label: Some("Weekly".to_string()),
            seven_day: 0.13,
            seven_day_resets_at: None,
            available: true,
            ..Default::default()
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn render(info: &UsageInfo, width: u16) -> (FooterTier, Vec<String>) {
        let (tier, lines) = usage_footer_lines(info, width);
        (tier, lines.iter().map(line_text).collect())
    }

    #[test]
    fn anthropic_renders_one_line_per_window_under_a_heading() {
        let (tier, lines) = render(&anthropic_info(), 60);

        assert_eq!(tier, FooterTier::Bars);
        assert_eq!(lines.len(), 3, "heading + two windows, got: {lines:?}");
        assert_eq!(lines[0], "Anthropic limits");
        assert!(lines[1].starts_with("5-hour "), "got: {:?}", lines[1]);
        assert!(lines[1].contains("60%"), "got: {:?}", lines[1]);
        assert!(lines[2].starts_with("Weekly "), "got: {:?}", lines[2]);
        assert!(lines[2].contains("13%"), "got: {:?}", lines[2]);
    }

    #[test]
    fn a_spark_window_adds_a_fourth_line() {
        let info = UsageInfo {
            provider: UsageProvider::OpenAI,
            primary_limit_label: Some("Monthly".to_string()),
            five_hour: 0.4,
            secondary_limit_label: None,
            spark: Some(0.25),
            available: true,
            ..Default::default()
        };

        let (_, lines) = render(&info, 60);
        assert_eq!(lines.len(), 3, "heading + monthly + spark, got: {lines:?}");
        assert_eq!(lines[0], "OpenAI limits");
        assert!(lines[1].starts_with("Monthly "));
        assert!(lines[2].starts_with("Spark "));
    }

    #[test]
    fn reset_countdowns_are_shown_when_known() {
        let info = UsageInfo {
            five_hour_resets_at: Some("2099-01-01T00:00:00+00:00".to_string()),
            ..anthropic_info()
        };

        let (_, lines) = render(&info, 60);
        assert!(
            lines[1].contains(" · "),
            "the window line should carry its countdown, got: {:?}",
            lines[1]
        );
    }

    #[test]
    fn narrow_width_drops_bars_and_abbreviates_labels_per_line() {
        let (tier, lines) = render(&anthropic_info(), 8);

        assert_eq!(tier, FooterTier::Percents);
        assert_eq!(lines, vec!["5h 60%".to_string(), "wk 13%".to_string()]);
    }

    #[test]
    fn very_narrow_width_keeps_the_numbers_only() {
        let (tier, lines) = render(&anthropic_info(), 5);

        assert_eq!(tier, FooterTier::Minimal);
        assert_eq!(lines, vec!["60%".to_string(), "13%".to_string()]);
    }

    #[test]
    fn every_line_fits_within_the_width_budget() {
        let info = anthropic_info();
        for width in 0u16..=160 {
            let (_, lines) = usage_footer_lines(&info, width);
            for line in &lines {
                let rendered = line_text(line);
                assert!(
                    UnicodeWidthStr::width(rendered.as_str()) <= width as usize,
                    "line overflowed at width {width}: {rendered:?}"
                );
            }
        }
    }

    #[test]
    fn the_block_never_exceeds_its_height_cap() {
        let info = anthropic_info();
        for width in 0u16..=160 {
            let (_, lines) = usage_footer_lines(&info, width);
            assert!(lines.len() <= MAX_FOOTER_LINES as usize);
        }
    }

    #[test]
    fn unavailable_usage_with_no_prior_reading_renders_nothing() {
        let info = UsageInfo {
            available: false,
            stale: false,
            ..anthropic_info()
        };

        assert_eq!(render(&info, 120).0, FooterTier::Hidden);
    }

    #[test]
    fn stale_usage_keeps_the_block_and_marks_the_heading() {
        // A 429 on the usage refresh must not make a deliberately pinned block
        // vanish: the last-known numbers stay, flagged on the heading.
        let info = UsageInfo {
            available: false,
            stale: true,
            ..anthropic_info()
        };

        let (tier, lines) = render(&info, 60);
        assert_eq!(tier, FooterTier::Bars);
        assert_eq!(lines[0], "~Anthropic limits");
        assert!(lines[1].contains("60%"));
    }

    #[test]
    fn cost_based_provider_stays_a_single_line() {
        let info = UsageInfo {
            provider: UsageProvider::CostBased,
            total_cost: 1.2345,
            input_tokens: 120_000,
            output_tokens: 8_000,
            available: true,
            ..Default::default()
        };

        let (_, lines) = render(&info, 60);
        assert_eq!(lines.len(), 1, "no quota windows to stack, got: {lines:?}");
        assert!(lines[0].contains("$1.2345"));

        assert_eq!(render(&info, 10).1, vec!["$1.2345".to_string()]);
    }

    #[test]
    fn copilot_shows_token_counts_on_one_line() {
        let info = UsageInfo {
            provider: UsageProvider::Copilot,
            input_tokens: 1_500,
            output_tokens: 250,
            available: true,
            ..Default::default()
        };

        assert_eq!(render(&info, 40).1, vec!["1.5K in + 250 out".to_string()]);
        assert_eq!(render(&info, 12).1, vec!["1.5K/250".to_string()]);
    }

    #[test]
    fn window_labels_abbreviate_predictably() {
        assert_eq!(short_window_label("5-hour"), "5h");
        assert_eq!(short_window_label("5-hour window"), "5h");
        assert_eq!(short_window_label("Weekly"), "wk");
        assert_eq!(short_window_label("7-day window"), "wk");
        assert_eq!(short_window_label("Monthly"), "mo");
        assert_eq!(short_window_label("Spark"), "sp");
    }

    #[test]
    fn exhausted_window_is_colored_as_critical() {
        let info = UsageInfo {
            five_hour: 1.0,
            ..anthropic_info()
        };
        let (_, lines) = usage_footer_lines(&info, 60);
        let critical = lines[1]
            .spans
            .iter()
            .any(|span| span.style.fg == Some(rgb(255, 100, 100)));

        assert!(
            critical,
            "exhausted window should render in the danger color"
        );
    }

    /// The reserved height must equal the number of lines the renderer produces,
    /// per provider shape. These are the same numbers `usage_footer_height`
    /// feeds into the layout, so a provider exposing a different set of windows
    /// changes the reservation automatically instead of clipping or leaving a
    /// blank row.
    #[test]
    fn line_count_tracks_the_windows_a_provider_actually_exposes() {
        // Anthropic: heading + 5-hour + weekly.
        assert_eq!(usage_footer_lines(&anthropic_info(), 60).1.len(), 3);

        // Anthropic reporting only a primary window: heading + one line.
        let one_window = UsageInfo {
            secondary_limit_label: None,
            ..anthropic_info()
        };
        assert_eq!(usage_footer_lines(&one_window, 60).1.len(), 2);

        // OpenAI with a Spark window: heading + monthly + spark.
        let openai = UsageInfo {
            provider: UsageProvider::OpenAI,
            primary_limit_label: Some("Monthly".to_string()),
            five_hour: 0.4,
            secondary_limit_label: None,
            spark: Some(0.25),
            available: true,
            ..Default::default()
        };
        assert_eq!(usage_footer_lines(&openai, 60).1.len(), 3);

        // A third weekly window alongside the 5-hour one: heading + three.
        let three_windows = UsageInfo {
            spark: Some(0.5),
            ..anthropic_info()
        };
        assert_eq!(usage_footer_lines(&three_windows, 60).1.len(), 4);

        // Cost-billed providers have no quota window at all: a single line.
        let cost = UsageInfo {
            provider: UsageProvider::CostBased,
            total_cost: 0.5,
            input_tokens: 10,
            output_tokens: 10,
            available: true,
            ..Default::default()
        };
        assert_eq!(usage_footer_lines(&cost, 60).1.len(), 1);
    }

    /// Render the block through the real painting path and read the cells back,
    /// so the assertions cover actual terminal output and right-alignment rather
    /// than the line builders alone.
    fn painted_rows(info: &UsageInfo, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_usage_footer_into(frame, info, area, false);
            })
            .expect("draw failed");

        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn painted_block_is_right_aligned_and_stacked() {
        let rows = painted_rows(&anthropic_info(), 40, 3);

        assert!(rows[0].contains("Anthropic limits"), "got: {rows:?}");
        assert!(rows[1].contains("60%"), "got: {rows:?}");
        assert!(rows[2].contains("13%"), "got: {rows:?}");
        for row in &rows {
            assert!(
                row.starts_with(' ') && !row.ends_with(' '),
                "each row should be flush right, got: {row:?}"
            );
        }
    }
}
