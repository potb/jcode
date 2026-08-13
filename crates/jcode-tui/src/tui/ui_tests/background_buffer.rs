//! Buffer-level (terminal cell) verification for the inline background-task
//! strip and its full page.
//!
//! The unit tests in `jcode-tui-render` check `Line` construction. These go
//! through ratatui's `TestBackend` and the real `ui::draw` layout path, which
//! is where the interesting failures live: the strip shares one chrome slot
//! with the swarm strip, and getting that wrong either hides a band or adds a
//! row that shoves the transcript up.

use super::*;
use crate::tui::ui::clear_flicker_frame_history_for_tests;
use jcode_tui_render::background_gallery::{BgStatus, BgTask};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn bg_task(id: &str, label: &str, status: BgStatus) -> BgTask {
    BgTask {
        id: id.to_string(),
        label: label.to_string(),
        command: None,
        tool: "bash".to_string(),
        status,
        exit_code: None,
        elapsed_secs: Some(12.0),
        progress: None,
        error: None,
        output_tail: vec!["compiling".to_string(), "[stderr] warning".to_string()],
        session_id: "s1".to_string(),
        is_current_session: true,
    }
}

fn swarm_member(id: &str, name: &str) -> crate::protocol::SwarmMemberStatus {
    crate::protocol::SwarmMemberStatus {
        session_id: id.to_string(),
        friendly_name: Some(name.to_string()),
        status: "running".to_string(),
        detail: Some("working".to_string()),
        task_label: None,
        role: None,
        is_headless: Some(true),
        live_attachments: None,
        status_age_secs: Some(5),
        output_tail: None,
        report_back_to_session_id: None,
        todo_progress: Some((1, 3)),
        todo_items: Vec::new(),
        runtime: crate::protocol::SwarmMemberRuntime::default(),
    }
}

/// Buffer contents as one string per row (not trimmed, full width).
fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buf = terminal.backend().buffer();
    let width = buf.area.width;
    let height = buf.area.height;
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn draw(state: &TestState, width: u16, height: u16) -> Terminal<TestBackend> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, state))
        .expect("full draw should not panic");
    terminal
}

#[test]
fn background_strip_renders_task_id_above_the_status_line() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("hello")],
        messages_version: 1,
        bg_tasks: vec![bg_task("111111aaaa", "cargo test --all", BgStatus::Running)],
        ..Default::default()
    };

    let terminal = draw(&state, 80, 24);
    let text = rows(&terminal).join("\n");
    assert!(
        text.contains("111111aaaa"),
        "task id should be visible so the user knows what is running:\n{text}"
    );
    assert!(
        text.contains("cargo test --all"),
        "task label should be visible:\n{text}"
    );
}

/// The two bands share one slot. A focused background panel wins; otherwise
/// the swarm keeps it. Both must never be drawn at once, or the bottom chrome
/// grows by an extra band and pushes the transcript up.
#[test]
fn background_and_swarm_strips_never_both_occupy_the_slot() {
    let _lock = viewport_snapshot_test_lock();

    for bg_focused in [false, true] {
        clear_flicker_frame_history_for_tests();
        crate::tui::info_widget::clear_widget_placements_for_tests();
        let state = TestState {
            display_messages: vec![DisplayMessage::assistant("hello")],
            messages_version: 1,
            swarm_members: vec![swarm_member("s1", "researcher")],
            bg_tasks: vec![bg_task("111111aaaa", "cargo test", BgStatus::Running)],
            bg_panel_focused: bg_focused,
            ..Default::default()
        };

        let terminal = draw(&state, 80, 24);
        let text = rows(&terminal).join("\n");
        let has_bg = text.contains("111111aaaa");
        let has_swarm = text.contains("researcher");
        assert!(
            !(has_bg && has_swarm),
            "both strips drawn at once (bg_focused={bg_focused}):\n{text}"
        );
        if bg_focused {
            assert!(
                has_bg,
                "a focused background panel must own the slot:\n{text}"
            );
        } else {
            assert!(
                has_swarm,
                "an unfocused background strip must yield to the swarm:\n{text}"
            );
        }
    }
}

#[test]
fn focused_background_strip_shows_the_selected_task_output() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("hello")],
        messages_version: 1,
        bg_tasks: vec![bg_task("111111aaaa", "cargo test", BgStatus::Running)],
        bg_panel_focused: true,
        ..Default::default()
    };

    let terminal = draw(&state, 80, 24);
    let text = rows(&terminal).join("\n");
    assert!(
        text.contains("compiling"),
        "focused strip should tail the task output:\n{text}"
    );
    // stderr matters more than stdout here: a failing build says why on
    // stderr, and "see what my background tasks printed" was the whole point
    // of the panel. Filtering stderr out of the tail passed every other test
    // in the suite, so this is asserted explicitly at the buffer level.
    assert!(
        text.contains("warning"),
        "focused strip must show stderr lines, not just stdout:\n{text}"
    );
    assert!(
        text.contains("[stderr]"),
        "stderr lines must stay marked so the user can tell the streams \
         apart:\n{text}"
    );
    assert!(
        text.contains("select"),
        "focused strip should show its key hints:\n{text}"
    );
}

#[test]
fn background_page_replaces_the_transcript() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("TRANSCRIPT-MARKER")],
        messages_version: 1,
        bg_tasks: vec![bg_task("111111aaaa", "cargo test", BgStatus::Running)],
        bg_panel_focused: true,
        bg_panel_full_page: true,
        ..Default::default()
    };

    let terminal = draw(&state, 80, 24);
    let text = rows(&terminal).join("\n");
    assert!(
        text.contains("Background tasks"),
        "page header missing:\n{text}"
    );
    assert!(
        !text.contains("TRANSCRIPT-MARKER"),
        "the page should own the transcript viewport:\n{text}"
    );
    // The page is where a user goes to actually read output, so both streams
    // must be there. It previously asserted only the header and the takeover.
    assert!(
        text.contains("compiling"),
        "the page must show the selected task's stdout:\n{text}"
    );
    assert!(
        text.contains("warning") && text.contains("[stderr]"),
        "the page must show marked stderr, not just stdout:\n{text}"
    );
}

/// Geometry sweep: the strip must never panic or bleed outside the frame, at
/// any width or height, focused or not.
#[test]
fn background_strip_full_draw_survives_geometry_sweep() {
    let _lock = viewport_snapshot_test_lock();

    let tasks: Vec<BgTask> = (0..6)
        .map(|i| {
            bg_task(
                &format!("{:06}aaaa", i),
                "a long command line that needs truncating at narrow widths",
                if i % 2 == 0 {
                    BgStatus::Running
                } else {
                    BgStatus::Completed
                },
            )
        })
        .collect();

    for width in [12_u16, 20, 24, 40, 80, 120] {
        for height in [8_u16, 12, 24] {
            for focused in [false, true] {
                clear_flicker_frame_history_for_tests();
                crate::tui::info_widget::clear_widget_placements_for_tests();
                let state = TestState {
                    display_messages: vec![DisplayMessage::assistant("sweep")],
                    messages_version: 1,
                    bg_tasks: tasks.clone(),
                    bg_panel_selected: 3,
                    bg_panel_focused: focused,
                    ..Default::default()
                };
                let terminal = draw(&state, width, height);
                for row in rows(&terminal) {
                    assert!(
                        row.chars().count() <= width as usize,
                        "row wider than the frame at {width}x{height}: {row:?}"
                    );
                }
            }
        }
    }
}

/// An out-of-range selection (tasks finished and dropped out between frames)
/// must clamp, not panic.
#[test]
fn stale_selection_index_does_not_panic() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("hello")],
        messages_version: 1,
        bg_tasks: vec![bg_task("111111aaaa", "only task", BgStatus::Running)],
        bg_panel_selected: 99,
        bg_panel_focused: true,
        ..Default::default()
    };

    let terminal = draw(&state, 80, 24);
    assert!(rows(&terminal).join("\n").contains("111111aaaa"));
}

/// The dock stand-down is debounced: the strip keeps standing down for a
/// linger after the BackgroundTasks dock widget disengages.
///
/// Without the debounce, the dock's normal frame-to-frame placement churn
/// toggles the strip row on and off, which resizes the bottom chrome and makes
/// the whole transcript jump (the exact flicker the swarm strip was fixed for).
/// Previously only the boolean input to `strip_owner` was covered; this drives
/// the real clock.
#[test]
fn dock_stand_down_lingers_then_releases_the_strip() {
    let _lock = viewport_snapshot_test_lock();
    let linger = crate::tui::info_widget::bg_dock_stand_down_linger();

    let draw_with_dock_age = |age: Option<std::time::Duration>| -> String {
        clear_flicker_frame_history_for_tests();
        crate::tui::info_widget::clear_widget_placements_for_tests();
        crate::tui::info_widget::set_bg_dock_engaged_age_for_tests(age);
        let state = TestState {
            display_messages: vec![DisplayMessage::assistant("hello")],
            messages_version: 1,
            bg_tasks: vec![bg_task("111111aaaa", "cargo test", BgStatus::Running)],
            // Unfocused: a focused panel deliberately ignores the stand-down.
            bg_panel_focused: false,
            ..Default::default()
        };
        rows(&draw(&state, 80, 24)).join("\n")
    };

    // Dock disengaged just now: still inside the linger, strip stays hidden.
    let recent = draw_with_dock_age(Some(linger / 4));
    assert!(
        !recent.contains("111111aaaa"),
        "strip should stand down during the linger:\n{recent}"
    );

    // Linger elapsed: the strip comes back.
    let expired = draw_with_dock_age(Some(linger * 2));
    assert!(
        expired.contains("111111aaaa"),
        "strip should return once the linger expires:\n{expired}"
    );

    // Dock never engaged: no stand-down at all.
    let never = draw_with_dock_age(None);
    assert!(
        never.contains("111111aaaa"),
        "strip should show when the dock was never engaged:\n{never}"
    );
}

/// The unfocused strip must advertise the chord that actually opens it.
///
/// The strip renders "<chord> controls" as its call to action. That string is
/// built from `bg_panel_focus_key_label()`, which used to hardcode "alt+b", so
/// a user who rebound the panel chord was told to press a key that did
/// nothing. This asserts at the buffer level, which is the surface the user
/// really reads, rather than trusting the helper in isolation.
#[test]
fn the_unfocused_strip_advertises_the_configured_focus_chord() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("hello")],
        messages_version: 1,
        bg_tasks: vec![bg_task("111111aaaa", "cargo test --all", BgStatus::Running)],
        ..Default::default()
    };

    let terminal = draw(&state, 80, 24);
    let text = rows(&terminal).join("\n").to_lowercase();

    // Derive the expectation from the CONFIGURED binding, not from the same
    // helper that renders it. Comparing the frame against
    // `bg_panel_focus_key_label()` is a tautology: break the helper and both
    // sides move together, which is exactly what a mutation showed.
    let configured = crate::config::config()
        .keybindings
        .background_panel_focus
        .clone();
    let expected_letter = configured
        .rsplit('+')
        .next()
        .expect("a configured chord has a key")
        .to_lowercase();

    assert!(
        text.contains(&jcode_tui_core::keybind::alt_chord_lower(&expected_letter)),
        "the strip must advertise the configured chord {} \
         (config: {configured:?}):\n{text}",
        jcode_tui_core::keybind::alt_chord_lower(&expected_letter)
    );
    assert!(
        text.contains("controls"),
        "the strip must advertise that the chord opens controls:\n{text}"
    );
}

/// A stale selection must resolve to a real task's output, not just avoid a
/// panic.
///
/// `stale_selection_index_does_not_panic` only asserts the frame renders. The
/// selection is also used to decide WHICH task's output to load, one layer
/// earlier, so an unclamped index there silently shows the wrong task's output
/// (or none) while the rows still draw correctly.
#[test]
fn a_stale_selection_still_resolves_to_the_surviving_task_output() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("hello")],
        messages_version: 1,
        // Selection left over from a longer list; only one task survives.
        bg_tasks: vec![bg_task("111111aaaa", "only task", BgStatus::Running)],
        bg_panel_selected: 99,
        bg_panel_focused: true,
        ..Default::default()
    };

    let terminal = draw(&state, 80, 24);
    let text = rows(&terminal).join("\n");

    assert!(
        text.contains("111111aaaa"),
        "surviving task row missing:\n{text}"
    );
    assert!(
        text.contains("compiling"),
        "a stale index must fall back to the surviving task's output, not \
         silently show nothing:\n{text}"
    );
}
