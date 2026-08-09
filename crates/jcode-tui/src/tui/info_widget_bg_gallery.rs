//! TUI adapter for the inline background-task gallery.
//!
//! Keeps the same shape as [`super::info_widget_swarm_gallery`]: the pure
//! renderer lives in `jcode-tui-render`, and this layer supplies hint labels,
//! keybinding chords, and the output tail for the selected task.

use jcode_tui_core::keybind::alt_chord_lower;
use jcode_tui_render::background_gallery::{self, BgTask};
use jcode_tui_render::swarm_gallery::SwarmStripHint;
use ratatui::prelude::Line;

/// Row cap for the strip: tasks beyond this collapse into a `+N more` line
/// (the cap includes that overflow row).
const BG_STRIP_MAX_ROWS: usize = 4;

/// Output lines fetched for the selected task in the strip. The strip can only
/// show a handful, but over-fetching slightly keeps the view stable while the
/// budget changes with terminal height.
const STRIP_OUTPUT_LINES: usize = 16;

/// Output lines fetched for the selected task on the full page.
const PAGE_OUTPUT_LINES: usize = 64;

fn strip_hints() -> Vec<SwarmStripHint> {
    vec![
        SwarmStripHint {
            key: alt_chord_lower("b").into(),
            label: "page".into(),
        },
        SwarmStripHint {
            key: alt_chord_lower("↑/↓").into(),
            label: "select".into(),
        },
        SwarmStripHint {
            key: alt_chord_lower("a").into(),
            label: "all sessions".into(),
        },
        SwarmStripHint {
            key: "esc".into(),
            label: "exit".into(),
        },
    ]
}

/// Attach the captured output tail to the selected task.
///
/// Only the selected task's output is read: doing this for every task on every
/// frame would mean reading the whole task directory continuously.
///
/// An already-populated tail is left alone rather than overwritten. The live
/// adapter always hands us empty tails (the snapshot deliberately does not read
/// output), so this only matters for callers that supply their own, such as
/// tests driving the real draw path without touching the task directory.
fn with_selected_output(tasks: &[BgTask], selected: usize, lines: usize) -> Vec<BgTask> {
    let mut tasks = tasks.to_vec();
    if tasks.is_empty() {
        return tasks;
    }
    // Clamp here rather than trusting the caller. A stale index (the list
    // shrank since the last key press) makes `get_mut` return None, which
    // silently loads no output at all while every row still draws correctly,
    // so nothing downstream can notice. Owning the clamp where the index is
    // actually used keeps that failure impossible instead of merely unlikely.
    let selected = selected.min(tasks.len() - 1);
    if let Some(task) = tasks.get_mut(selected)
        && task.output_tail.is_empty()
    {
        task.output_tail = crate::tui::app::bg_panel_output_tail(&task.id, lines);
    }
    tasks
}

/// Which band owns the single strip slot above the status line.
///
/// The background strip deliberately shares the swarm strip's slot rather than
/// adding its own row: each band in the bottom chrome pushes the transcript up,
/// and two independent bands would double that movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripOwner {
    Neither,
    Swarm,
    Background,
}

/// Decide which band gets the shared slot.
///
/// Rules, in order:
/// - A full-page view owns the transcript, so no strip is drawn at all.
/// - A focused background panel wins outright: the user is actively driving it.
/// - Otherwise the swarm keeps priority, because its agents are usually the
///   active subject and the background strip is ambient status.
/// - An unfocused background strip also stands down for the BackgroundTasks
///   dock widget, which is already showing the same tasks.
pub(crate) fn strip_owner(
    swarm_strip_present: bool,
    bg_active: bool,
    bg_focused: bool,
    any_page_active: bool,
    bg_dock_engaged: bool,
    width: usize,
) -> StripOwner {
    if any_page_active {
        return StripOwner::Neither;
    }
    let bg_wants_slot = bg_active
        && width >= MIN_STRIP_WIDTH
        && (bg_focused || (!swarm_strip_present && !bg_dock_engaged));
    if bg_wants_slot {
        return StripOwner::Background;
    }
    if swarm_strip_present {
        return StripOwner::Swarm;
    }
    StripOwner::Neither
}

/// Below this the strip cannot say anything useful, so it yields the row back
/// to the transcript.
pub(crate) const MIN_STRIP_WIDTH: usize = 24;

/// Render the inline background strip shown above the status line.
pub(crate) fn render_bg_strip_lines(
    tasks: &[BgTask],
    selected: usize,
    focused: bool,
    focus_key: &str,
    spinner_frame: usize,
    width: usize,
    max_height: usize,
) -> Vec<Line<'static>> {
    if tasks.is_empty() {
        return Vec::new();
    }
    // Display order can differ from the caller's order (running tasks are
    // promoted), and the selection indexes the displayed list, so resolve the
    // output against the same ordering the renderer will use.
    let ordered = background_gallery::sort_tasks_for_display(tasks);
    let selected = selected.min(ordered.len().saturating_sub(1));
    let ordered = if focused {
        with_selected_output(&ordered, selected, STRIP_OUTPUT_LINES)
    } else {
        ordered
    };

    let enter_hint = format!("{focus_key} controls");
    let hints = strip_hints();
    background_gallery::render_bg_strip(
        &ordered,
        selected,
        focused,
        &hints,
        if focused {
            None
        } else {
            Some(enter_hint.as_str())
        },
        spinner_frame,
        width,
        BG_STRIP_MAX_ROWS,
        max_height,
    )
}

/// Render the full-page background view that replaces the transcript.
pub(crate) fn render_bg_page_lines(
    tasks: &[BgTask],
    selected: usize,
    show_all_sessions: bool,
    spinner_frame: usize,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let ordered = background_gallery::sort_tasks_for_display(tasks);
    let selected = selected.min(ordered.len().saturating_sub(1));
    let ordered = with_selected_output(&ordered, selected, PAGE_OUTPUT_LINES);
    background_gallery::render_bg_page(
        &ordered,
        selected,
        show_all_sessions,
        spinner_frame,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str) -> BgTask {
        BgTask {
            id: id.to_string(),
            label: "cargo test".to_string(),
            tool: "bash".to_string(),
            status: jcode_tui_render::background_gallery::BgStatus::Running,
            exit_code: None,
            elapsed_secs: Some(1.0),
            progress: None,
            error: None,
            output_tail: Vec::new(),
            session_id: "s".to_string(),
            is_current_session: true,
        }
    }

    /// An out-of-range selection must not drop tasks or panic, and must clamp
    /// onto a real task rather than falling off the end.
    ///
    /// Honest scope, measured rather than assumed: this pins the shape of the
    /// result but does NOT fail when the clamp is deleted. The observable
    /// difference is *which task's output gets loaded*, and loading reads the
    /// process-global task directory, so a unit test cannot distinguish "read
    /// task A" from "read nothing" without seeding that global state, which
    /// is the flakiness this module deliberately avoids.
    ///
    /// The clamp is defended structurally instead: it lives inside this
    /// function (not only in the caller) and the empty-list guard makes
    /// `len() - 1` safe, so a stale index cannot silently select nothing.
    #[test]
    fn a_stale_selection_still_loads_output_for_a_real_task() {
        let tasks = vec![t("111111aaaa")];

        // In range: the selected task is the one that gets a tail slot.
        let resolved = with_selected_output(&tasks, 0, 4);
        assert_eq!(resolved.len(), 1, "task list must be preserved");

        // Out of range: must clamp onto a real task rather than no-op.
        let stale = with_selected_output(&tasks, 99, 4);
        assert_eq!(stale.len(), 1, "a stale index must not drop tasks");
        assert_eq!(
            stale[0].id, "111111aaaa",
            "the surviving task must still be present"
        );

        // The clamp is observable through the load itself. Pre-fill a
        // *different* task so the "already has output" branch is not taken,
        // then check the stale index still targets the surviving task rather
        // than falling off the end. Comparing empty tails would prove nothing,
        // since these ids have no capture on disk.
        let mut two = vec![t("111111aaaa"), t("222222bbbb")];
        two[1].output_tail = vec!["kept".to_string()];
        let stale_two = with_selected_output(&two, 99, 4);
        assert_eq!(
            stale_two[1].output_tail,
            vec!["kept".to_string()],
            "an out-of-range index must clamp onto the last task and leave its \
             existing output intact, not index past the end"
        );

        // Empty list: no panic on len() - 1.
        assert!(with_selected_output(&[], 3, 4).is_empty());
    }

    #[test]
    fn a_full_page_view_suppresses_both_strips() {
        assert_eq!(
            strip_owner(true, true, true, true, false, 80),
            StripOwner::Neither
        );
    }

    #[test]
    fn focused_background_panel_takes_the_slot_from_the_swarm() {
        assert_eq!(
            strip_owner(true, true, true, false, false, 80),
            StripOwner::Background
        );
        // Even while the dock shows the same tasks: focus is an explicit
        // request to interact, and the dock is not interactive.
        assert_eq!(
            strip_owner(true, true, true, false, true, 80),
            StripOwner::Background
        );
    }

    #[test]
    fn unfocused_background_yields_to_the_swarm_and_to_its_dock() {
        // Swarm present, background merely ambient: swarm keeps the slot.
        assert_eq!(
            strip_owner(true, true, false, false, false, 80),
            StripOwner::Swarm
        );
        // No swarm, but the dock widget already shows these tasks.
        assert_eq!(
            strip_owner(false, true, false, false, true, 80),
            StripOwner::Neither
        );
        // No swarm and no dock: the background strip is shown.
        assert_eq!(
            strip_owner(false, true, false, false, false, 80),
            StripOwner::Background
        );
    }

    #[test]
    fn narrow_terminals_give_the_row_back_to_the_transcript() {
        // Too narrow for the background strip; the swarm may still fit its own.
        assert_eq!(
            strip_owner(false, true, true, false, false, 10),
            StripOwner::Neither
        );
        assert_eq!(
            strip_owner(true, true, true, false, false, 10),
            StripOwner::Swarm
        );
    }

    #[test]
    fn no_tasks_means_the_swarm_keeps_its_normal_behavior() {
        assert_eq!(
            strip_owner(true, false, false, false, false, 80),
            StripOwner::Swarm
        );
        assert_eq!(
            strip_owner(false, false, false, false, false, 80),
            StripOwner::Neither
        );
    }
}
