//! Shared presentation logic for the inline background-task gallery.
//!
//! This is the background-task counterpart to [`crate::swarm_gallery`]: a
//! strip that lives above the status line, listing the tasks jcode is running
//! for you, and expanding the selected one into a live tail of its combined
//! stdout/stderr.
//!
//! Before this existed, a running background task surfaced as a bare
//! "N running" line in the info widget: you could tell that *something* was
//! happening but not what, and the only way to read the output was to ask the
//! agent to call the `bg` tool. The data was always there (status files carry
//! status, exit code, duration, progress and an output capture path); it just
//! had no user-facing view.
//!
//! Rendering deliberately mirrors the swarm strip so the two feel like one
//! system: same spinner cadence, same chip/accordion structure, same
//! degradation order as width runs out (drop the hint, then the tally, then
//! truncate).

use ratatui::prelude::*;

use jcode_tui_style::color::rgb;

use crate::gallery_text::{clamp_line_to_width, disp_w, single_line, truncate_label};
use crate::swarm_gallery::{STRIP_SPINNER_FRAMES, SwarmStripHint};

/// Lifecycle of a background task, as far as the panel is concerned.
///
/// This mirrors `jcode_background_types::BackgroundTaskStatus` but adds
/// `Orphaned`, which is not a stored state: it is a `Running` status file
/// whose owning process is gone (server crash or exec reload). Showing those
/// as "running" is how phantom tasks used to accumulate, so the panel names
/// them explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgStatus {
    Running,
    Completed,
    Failed,
    Superseded,
    Orphaned,
}

impl BgStatus {
    pub fn is_active(self) -> bool {
        matches!(self, BgStatus::Running)
    }

    /// Short lowercase label used in rows and headers.
    pub fn label(self) -> &'static str {
        match self {
            BgStatus::Running => "running",
            BgStatus::Completed => "completed",
            BgStatus::Failed => "failed",
            BgStatus::Superseded => "superseded",
            BgStatus::Orphaned => "orphaned",
        }
    }
}

/// Accent color for a task status. Chosen to match the swarm strip's palette
/// so a screen showing both panels reads as one system.
pub fn status_accent(status: BgStatus) -> Color {
    match status {
        BgStatus::Running => rgb(255, 200, 100),
        BgStatus::Completed => rgb(100, 200, 100),
        BgStatus::Failed => rgb(255, 100, 100),
        BgStatus::Superseded => rgb(140, 140, 150),
        BgStatus::Orphaned => rgb(255, 170, 80),
    }
}

/// Status glyph. Running tasks animate on the shared spinner cadence; terminal
/// states get a fixed glyph.
pub fn status_glyph(status: BgStatus, spinner_frame: usize) -> &'static str {
    match status {
        BgStatus::Running => STRIP_SPINNER_FRAMES[spinner_frame % STRIP_SPINNER_FRAMES.len()],
        BgStatus::Completed => "✓",
        BgStatus::Failed => "✗",
        BgStatus::Superseded => "⊘",
        BgStatus::Orphaned => "?",
    }
}

/// One background task as the panel sees it. The TUI adapter maps status files
/// into this; the renderer never touches disk or `jcode-base` types, which is
/// what lets the whole module be unit-tested from plain values.
#[derive(Clone, Debug)]
pub struct BgTask {
    /// Short task id, e.g. "186941p01w". Doubles as the stable sort key.
    pub id: String,
    /// Human label: the command or display name, already collapsed to one line.
    pub label: String,
    /// Full command line that spawned the task, when the tool recorded one.
    /// The `label` is often a collapsed display name ("bash", a title), so the
    /// detail view shows this to answer "what is this task actually running?".
    pub command: Option<String>,
    /// Tool that spawned it ("bash", "selfdev", ...).
    pub tool: String,
    pub status: BgStatus,
    /// Exit code, when the task has finished.
    pub exit_code: Option<i32>,
    /// Wall-clock seconds: duration for finished tasks, elapsed for running.
    pub elapsed_secs: Option<f64>,
    /// Latest progress line, pre-formatted by the adapter.
    pub progress: Option<String>,
    /// Error text for failed/orphaned tasks.
    pub error: Option<String>,
    /// Tail of the combined stdout/stderr capture, oldest first. Only
    /// populated for the selected task: reading every task's output every
    /// frame would stat and read the whole task directory.
    pub output_tail: Vec<String>,
    /// Session that owns the task, for the "all sessions" filter toggle.
    pub session_id: String,
    /// Whether the task belongs to the session this TUI is attached to.
    pub is_current_session: bool,
}

impl BgTask {
    fn short_id(&self) -> &str {
        &self.id
    }
}

/// Format a duration the way the strip wants it: compact, fixed-ish width.
pub fn format_duration(secs: f64) -> String {
    if secs < 1.0 {
        "0s".to_string()
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else if secs < 3600.0 {
        format!("{}m{:02}s", (secs / 60.0) as u64, (secs % 60.0) as u64)
    } else {
        let hours = (secs / 3600.0) as u64;
        let minutes = ((secs % 3600.0) / 60.0) as u64;
        format!("{}h{:02}m", hours, minutes)
    }
}

/// Sort tasks for display: running first (newest first), then everything else
/// newest first.
///
/// Task ids embed only the last six digits of a millisecond timestamp, so they
/// wrap roughly every 17 minutes and cannot be used for recency. The adapter
/// therefore hands us tasks already ordered by start time, and this function
/// only promotes the active ones without disturbing that order.
pub fn sort_tasks_for_display(tasks: &[BgTask]) -> Vec<BgTask> {
    let mut out: Vec<BgTask> = Vec::with_capacity(tasks.len());
    out.extend(tasks.iter().filter(|t| t.status.is_active()).cloned());
    out.extend(tasks.iter().filter(|t| !t.status.is_active()).cloned());
    out
}

/// Render the background-task strip shown directly above the status line.
///
/// - Unfocused: one row per task (capped by `max_rows`), each
///   `<glyph> <id> <label>` with a right-aligned duration, plus a leading
///   marker and the `M/N running` tally with a focus hint on the first row.
/// - Focused: an accordion. The selected task gains a `▸` marker and its live
///   output tail expands in place beneath its row, closed by a hint line.
///
/// ```text
/// ⏳ ⠙ 186941p01w cargo test --all              1/3 running · alt+b controls
///  ▸ ⠹ 153982c710 selfdev build                                        1m02s
///    │ Compiling jcode-tui v0.1.0
///    │ [stderr] warning: unused variable
///    ✓ 986496xr62 sleep 30                                                30s
///    alt+↑/↓ select · alt+a all sessions · esc exit
/// ```
#[allow(clippy::too_many_arguments)]
pub fn render_bg_strip(
    tasks: &[BgTask],
    selected: usize,
    focused: bool,
    hints: &[SwarmStripHint],
    enter_hint: Option<&str>,
    spinner_frame: usize,
    width: usize,
    max_rows: usize,
    max_height: usize,
) -> Vec<Line<'static>> {
    if tasks.is_empty() || width < 8 || max_height == 0 {
        return Vec::new();
    }

    let ordered = sort_tasks_for_display(tasks);
    let selected = selected.min(ordered.len().saturating_sub(1));
    let running = ordered.iter().filter(|t| t.status.is_active()).count();

    const LEAD: &str = "⏳ ";
    const INDENT: &str = "   ";
    const SEL_MARK: &str = " ▸ ";
    let lead_w = disp_w(LEAD);
    let gap = 2usize;

    // Budget split. The task list comes first: a detail viewport that starves
    // the list would hide the very tasks the user is selecting between. The
    // detail then takes whatever is left, capped so one chatty build log
    // cannot swallow the whole strip.
    const MAX_DETAIL_ROWS: usize = 8;
    let hint_rows = usize::from(focused && !hints.is_empty());
    let available = max_height.saturating_sub(hint_rows).max(1);
    let row_budget = if focused {
        // Leave room for at least a couple of detail rows when we can, but
        // never drop below one task row.
        let wanted = ordered.len().min(max_rows);
        let reserved_for_detail = available.saturating_sub(wanted).min(MAX_DETAIL_ROWS);
        available
            .saturating_sub(reserved_for_detail.max(2.min(available.saturating_sub(1))))
            .max(1)
            .min(max_rows)
    } else {
        available.min(max_rows)
    };
    let detail_budget = if focused {
        available
            .saturating_sub(row_budget.min(ordered.len()))
            .min(MAX_DETAIL_ROWS)
    } else {
        0
    };

    let shown = if ordered.len() <= row_budget {
        ordered.len()
    } else {
        row_budget.saturating_sub(1).max(1)
    };
    let hidden = ordered.len() - shown;

    // Window the list so the selection stays visible.
    let start = if selected >= shown {
        selected + 1 - shown
    } else {
        0
    };

    let tally = format!("{running}/{} running", ordered.len());
    let tally_w = disp_w(&tally);
    let hint_text = if focused { None } else { enter_hint };
    let hint_sep = " · ";
    let hint_w = hint_text.map(|h| disp_w(h) + disp_w(hint_sep)).unwrap_or(0);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut selected_row_at: Option<usize> = None;

    for (row, task) in ordered.iter().enumerate().skip(start).take(shown) {
        let first = out.is_empty();
        let is_sel = row == selected;
        let color = status_accent(task.status);
        let glyph = status_glyph(task.status, spinner_frame);

        let mut spans: Vec<Span<'static>> = Vec::new();
        if first {
            spans.push(Span::styled(
                LEAD.to_string(),
                Style::default().fg(rgb(180, 140, 255)),
            ));
        } else if is_sel && focused {
            spans.push(Span::styled(
                SEL_MARK.to_string(),
                Style::default().fg(color),
            ));
        } else {
            spans.push(Span::raw(INDENT));
        }

        // Right tail: the tally (+ hint) on the first row, the duration on the
        // rest. Degrade by dropping the hint first, then the tally, exactly
        // like the swarm strip does.
        let duration = task.elapsed_secs.map(format_duration).unwrap_or_default();
        let (row_tail, row_tail_w) = if first {
            if lead_w + gap + tally_w + hint_w + 16 <= width && hint_w > 0 {
                (RowTail::TallyAndHint, tally_w + hint_w)
            } else if lead_w + gap + tally_w + 12 <= width {
                (RowTail::Tally, tally_w)
            } else {
                (RowTail::None, 0)
            }
        } else if !duration.is_empty() && lead_w + gap + disp_w(&duration) + 12 <= width {
            (RowTail::Duration, disp_w(&duration))
        } else {
            (RowTail::None, 0)
        };

        let body_budget = width
            .saturating_sub(lead_w)
            .saturating_sub(if row_tail_w > 0 { row_tail_w + gap } else { 0 });

        // <glyph> <id> <label>
        let mut style = Style::default().fg(color);
        if is_sel && focused {
            style = style.add_modifier(Modifier::BOLD);
        }
        let glyph_part = format!("{} ", glyph);
        spans.push(Span::styled(glyph_part.clone(), style));

        let id_part = format!("{} ", task.short_id());
        let id_w = disp_w(&id_part);
        spans.push(Span::styled(
            id_part,
            Style::default().fg(rgb(130, 130, 145)),
        ));

        let label_budget = body_budget
            .saturating_sub(disp_w(&glyph_part))
            .saturating_sub(id_w);
        let label = truncate_label(&single_line(&task.label), label_budget.max(1));
        let label_w = disp_w(&label);
        let mut label_style = Style::default().fg(rgb(200, 200, 210));
        if is_sel && focused {
            label_style = label_style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(label, label_style));

        // Right-align whichever tail this row earned.
        let consumed = lead_w + disp_w(&glyph_part) + id_w + label_w;
        match row_tail {
            RowTail::None => {}
            RowTail::Duration => {
                if consumed + gap + row_tail_w <= width {
                    spans.push(Span::raw(" ".repeat(width - consumed - row_tail_w)));
                    spans.push(Span::styled(
                        duration.clone(),
                        Style::default().fg(rgb(120, 120, 130)),
                    ));
                }
            }
            RowTail::Tally | RowTail::TallyAndHint => {
                if consumed + gap + row_tail_w <= width {
                    spans.push(Span::raw(" ".repeat(width - consumed - row_tail_w)));
                    spans.push(Span::styled(
                        tally.clone(),
                        Style::default().fg(if running > 0 {
                            rgb(180, 140, 255)
                        } else {
                            rgb(120, 120, 130)
                        }),
                    ));
                    if matches!(row_tail, RowTail::TallyAndHint)
                        && let Some(hint) = hint_text
                    {
                        spans.push(Span::styled(
                            hint_sep.to_string(),
                            Style::default().fg(rgb(80, 80, 90)),
                        ));
                        spans.push(Span::styled(
                            hint.to_string(),
                            Style::default().fg(rgb(110, 130, 170)),
                        ));
                    }
                }
            }
        }

        if is_sel {
            selected_row_at = Some(out.len());
        }
        out.push(Line::from(spans));
    }

    if hidden > 0 {
        out.push(Line::from(vec![
            Span::raw(INDENT),
            Span::styled(
                format!("+{hidden} more"),
                Style::default().fg(rgb(140, 140, 150)),
            ),
        ]));
    }

    // Focused accordion: expand the selected task's detail directly beneath
    // its row, so the eye does not have to travel to a separate pane.
    if focused && detail_budget > 0 {
        if let (Some(task), Some(at)) = (ordered.get(selected), selected_row_at) {
            let detail = render_task_detail(task, width, detail_budget);
            let insert_at = (at + 1).min(out.len());
            for (offset, line) in detail.into_iter().enumerate() {
                out.insert(insert_at + offset, line);
            }
        }
    }

    if focused && !hints.is_empty() {
        out.push(render_hint_line(hints, width));
    }

    // Hard bounds: never exceed the caller's height or width budget, whatever
    // the arithmetic above concluded. An overlong strip would shove the
    // transcript up and make the screen bounce.
    out.truncate(max_height);
    for line in &mut out {
        clamp_line_to_width(line, width);
    }
    out
}

enum RowTail {
    None,
    Duration,
    Tally,
    TallyAndHint,
}

/// The selected task's detail: a status/progress header plus the tail of its
/// combined stdout/stderr, drawn against a rail so it reads as nested under
/// the task row.
fn render_task_detail(task: &BgTask, width: usize, budget: usize) -> Vec<Line<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    const BAR: &str = "   │ ";
    let bar_w = disp_w(BAR);
    let text_budget = width.saturating_sub(bar_w).max(4);
    let mut out: Vec<Line<'static>> = Vec::new();

    // Command line: the task row shows only the collapsed label, which for a
    // named task ("build", "bash") says nothing about what is running. Skip it
    // when it would just repeat the label.
    if let Some(command) = task
        .command
        .as_deref()
        .map(single_line)
        .filter(|command| !command.trim().is_empty() && command.trim() != task.label.trim())
    {
        out.push(Line::from(vec![
            Span::styled(BAR.to_string(), Style::default().fg(rgb(80, 80, 90))),
            Span::styled(
                truncate_label(&command, text_budget),
                Style::default().fg(rgb(170, 190, 220)),
            ),
        ]));
        if out.len() >= budget {
            out.truncate(budget);
            return out;
        }
    }

    // Meta line: status, exit code, progress or error. Only worth a row when
    // it says something the task row did not.
    let mut meta: Vec<String> = Vec::new();
    if let Some(code) = task.exit_code.filter(|code| *code != 0) {
        meta.push(format!("exit {code}"));
    }
    if let Some(progress) = task.progress.as_deref().filter(|p| !p.trim().is_empty()) {
        meta.push(single_line(progress));
    }
    if let Some(error) = task.error.as_deref().filter(|e| !e.trim().is_empty()) {
        meta.push(single_line(error));
    }
    if !meta.is_empty() {
        out.push(Line::from(vec![
            Span::styled(BAR.to_string(), Style::default().fg(rgb(80, 80, 90))),
            Span::styled(
                truncate_label(&meta.join(" · "), text_budget),
                Style::default().fg(status_accent(task.status)),
            ),
        ]));
    }

    let output_budget = budget.saturating_sub(out.len());
    if output_budget == 0 {
        out.truncate(budget);
        return out;
    }

    if task.output_tail.is_empty() {
        if out.len() < budget {
            out.push(Line::from(vec![
                Span::styled(BAR.to_string(), Style::default().fg(rgb(80, 80, 90))),
                Span::styled(
                    "no output yet".to_string(),
                    Style::default().fg(rgb(110, 110, 120)),
                ),
            ]));
        }
        return out;
    }

    // Show the newest lines: a build's interesting output is at the end.
    let start = task.output_tail.len().saturating_sub(output_budget);
    for line in &task.output_tail[start..] {
        // stderr is tagged inline by the capture layer; color it so a failing
        // command is legible at a glance.
        let is_stderr = line.trim_start().starts_with("[stderr]");
        let text = truncate_label(&single_line(line), text_budget);
        out.push(Line::from(vec![
            Span::styled(BAR.to_string(), Style::default().fg(rgb(80, 80, 90))),
            Span::styled(
                text,
                Style::default().fg(if is_stderr {
                    rgb(230, 150, 150)
                } else {
                    rgb(180, 180, 190)
                }),
            ),
        ]));
    }

    out.truncate(budget);
    out
}

fn render_hint_line(hints: &[SwarmStripHint], width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::raw("   ")];
    for (i, hint) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(rgb(80, 80, 90))));
        }
        spans.push(Span::styled(
            hint.key.clone(),
            Style::default().fg(rgb(150, 170, 210)),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            hint.label.clone(),
            Style::default().fg(rgb(120, 120, 130)),
        ));
    }
    let mut total = 0usize;
    let mut trimmed: Vec<Span<'static>> = Vec::new();
    for span in spans {
        let w = disp_w(&span.content);
        if total + w > width {
            break;
        }
        total += w;
        trimmed.push(span);
    }
    Line::from(trimmed)
}

/// Full-page background task view: every task as a row, with the selected
/// task's output filling the remaining height.
///
/// This is the third state of the view cycle (chat → strip controls → page),
/// for when you want to actually read a build log rather than glance at it.
pub fn render_bg_page(
    tasks: &[BgTask],
    selected: usize,
    show_all_sessions: bool,
    spinner_frame: usize,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if height == 0 || width < 8 {
        return Vec::new();
    }
    let ordered = sort_tasks_for_display(tasks);
    let running = ordered.iter().filter(|t| t.status.is_active()).count();

    let mut out: Vec<Line<'static>> = Vec::new();
    let scope = if show_all_sessions {
        "all sessions"
    } else {
        "this session"
    };
    out.push(Line::from(vec![
        Span::styled("⏳ ", Style::default().fg(rgb(180, 140, 255))),
        Span::styled(
            "Background tasks".to_string(),
            Style::default()
                .fg(rgb(220, 220, 230))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {running}/{} running · {scope}", ordered.len()),
            Style::default().fg(rgb(140, 140, 150)),
        ),
    ]));

    if ordered.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(vec![Span::styled(
            "  No background tasks.".to_string(),
            Style::default().fg(rgb(130, 130, 140)),
        )]));
        for line in &mut out {
            clamp_line_to_width(line, width);
        }
        return out;
    }

    let selected = selected.min(ordered.len() - 1);

    // Split the page: list on top (at most half), output below.
    let body_height = height.saturating_sub(out.len());
    let list_height = (ordered.len() + 1).min(body_height.div_ceil(2)).max(1);
    let start = if selected >= list_height {
        selected + 1 - list_height
    } else {
        0
    };

    for (idx, task) in ordered.iter().enumerate().skip(start).take(list_height) {
        let is_sel = idx == selected;
        let color = status_accent(task.status);
        let marker = if is_sel { " ▸ " } else { "   " };
        let duration = task.elapsed_secs.map(format_duration).unwrap_or_default();

        let mut style = Style::default().fg(color);
        if is_sel {
            style = style.add_modifier(Modifier::BOLD);
        }
        let head = format!(
            "{}{} {} ",
            marker,
            status_glyph(task.status, spinner_frame),
            task.short_id()
        );
        let head_w = disp_w(&head);
        let tail_w = if duration.is_empty() {
            0
        } else {
            disp_w(&duration) + 2
        };
        let label = truncate_label(
            &single_line(&task.label),
            width.saturating_sub(head_w + tail_w).max(1),
        );

        let mut spans = vec![
            Span::styled(head, style),
            Span::styled(
                label.clone(),
                Style::default().fg(if is_sel {
                    rgb(220, 220, 230)
                } else {
                    rgb(180, 180, 190)
                }),
            ),
        ];
        let consumed = head_w + disp_w(&label);
        if tail_w > 0 && consumed + tail_w <= width {
            spans.push(Span::raw(" ".repeat(width - consumed - disp_w(&duration))));
            spans.push(Span::styled(
                duration,
                Style::default().fg(rgb(120, 120, 130)),
            ));
        }
        out.push(Line::from(spans));
    }

    let remaining = height.saturating_sub(out.len());
    if remaining > 1 {
        out.push(Line::from(""));
        if let Some(task) = ordered.get(selected) {
            out.extend(render_task_detail(task, width, remaining - 1));
        }
    }

    out.truncate(height);
    for line in &mut out {
        clamp_line_to_width(line, width);
    }
    out
}

#[cfg(test)]
mod tests;
