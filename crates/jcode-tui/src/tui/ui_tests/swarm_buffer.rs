//! Buffer-level (terminal cell) verification for the inline swarm strip and
//! the notification line.
//!
//! Prior swarm-strip/notification tests only checked `Line` construction
//! (span widths). These tests close that gap: they render through ratatui's
//! `TestBackend` so actual cell writes are exercised, including the full
//! `ui::draw` layout path (ui.rs strip Paragraph at chunk 2, notification at
//! chunk 4) and direct widget draws into sub-areas, asserting no panics and
//! that nothing is written outside the target area even with wide glyphs.

use super::*;
use crate::protocol::SwarmMemberStatus;
use crate::tui::ui::clear_flicker_frame_history_for_tests;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn strip_member(id: &str, name: &str, status: &str) -> SwarmMemberStatus {
    SwarmMemberStatus {
        session_id: id.to_string(),
        friendly_name: Some(name.to_string()),
        status: status.to_string(),
        detail: Some("working on task".to_string()),
        task_label: None,
        role: None,
        is_headless: Some(true),
        live_attachments: None,
        status_age_secs: Some(5),
        output_tail: None,
        report_back_to_session_id: None,
        todo_progress: Some((2, 5)),
        todo_items: Vec::new(),
        runtime: crate::protocol::SwarmMemberRuntime::default(),
    }
}

/// Buffer contents as one string per row (not trimmed, full width).
fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
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

fn fact_test_state(input: String, scheduled: bool) -> TestState {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/test".to_string());
    let ambient_info = scheduled.then(|| info_widget::AmbientWidgetData {
        show_widget: false,
        status: crate::ambient::AmbientStatus::Idle,
        queue_count: 1,
        next_queue_preview: Some("check the build".to_string()),
        reminder_count: 1,
        next_reminder_preview: Some("check the build".to_string()),
        last_run_ago: None,
        last_summary: None,
        next_wake: None,
        next_reminder_wake: Some("in 4m".to_string()),
        budget_percent: None,
    });
    let info_widget_data = info_widget::InfoWidgetData {
        model: Some("gpt-5.6-sol".to_string()),
        reasoning_effort: Some("high".to_string()),
        context_limit: Some(256_000),
        provider_name: Some("openai".to_string()),
        auth_method: info_widget::AuthMethod::OpenAIOAuth,
        observed_context_tokens: Some(74_000),
        ambient_info,
        ..Default::default()
    };
    TestState {
        cursor_pos: input.len(),
        input,
        provider_name: Some("openai".to_string()),
        provider_model: Some("gpt-5.6-sol".to_string()),
        working_dir: Some(format!("{home}/jcode")),
        info_widget_data,
        suppress_info_widgets: true,
        display_messages: vec![DisplayMessage::assistant("last transcript line")],
        messages_version: 1,
        ..Default::default()
    }
}

fn row_containing(rows: &[String], needle: &str) -> usize {
    rows.iter()
        .rposition(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?} in frame:\n{}", rows.join("\n")))
}

fn fact_stack_rows(rows: &[String]) -> [usize; 4] {
    [
        row_containing(rows, "OpenAI · OAuth"),
        row_containing(rows, "GPT-5.6 Sol high"),
        row_containing(rows, "~/jcode"),
        row_containing(rows, "74k/256k"),
    ]
}

fn assert_fact_stack_is_contiguous(rows: &[String]) -> [usize; 4] {
    let positions = fact_stack_rows(rows);
    assert!(
        positions.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "facts must be one uninterrupted OAuth/model/directory/context block:\n{}",
        rows.join("\n")
    );
    positions
}

#[test]
fn right_fact_stack_uses_transcript_status_notification_and_input_rows_in_order() {
    // Pins `display.session_facts` rather than inheriting it. This test asserts
    // exact rows for the right-anchored stack, so it must not read whatever a
    // concurrently running sibling test (or the developer's config file) left in
    // the global config. `facts_test_locks` also serializes it against those
    // siblings in the one safe lock order.
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("right");
    clear_flicker_frame_history_for_tests();
    let state = fact_test_state(String::new(), true);
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("fact stack frame");

    let rows = buffer_rows(&terminal);
    let [oauth_y, model_y, dir_y, context_y] = assert_fact_stack_is_contiguous(&rows);
    assert!(oauth_y < model_y && model_y < dir_y && dir_y < context_y);
    assert!(rows[context_y].contains("▰▰▱▱▱▱ 29%"));
    assert!(rows[dir_y].contains("next scheduled task in 4m"));

    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
    let input = layout.input_area.expect("input area");
    let status = crate::tui::ui::last_status_area().expect("status area");
    assert_eq!(context_y as u16, input.bottom() - 1);
    assert_eq!(model_y as u16, status.y);
    assert_eq!(dir_y as u16, status.y + 1);
    assert_eq!(oauth_y as u16, layout.messages_area.bottom() - 1);
}

#[test]
fn right_fact_stack_uses_neutral_gray_except_for_context_usage() {
    use unicode_width::UnicodeWidthStr;

    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    let state = fact_test_state(String::new(), true);
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("neutral fact colors frame");

    let rows = buffer_rows(&terminal);
    let buffer = terminal.backend().buffer();
    // Build the expected color through the same helper production uses, so the
    // assertion holds on 256-color terminals too. A hardcoded `Color::Rgb`
    // literal makes this test fail deterministically whenever the host does not
    // advertise truecolor (bare `TERM=dumb`, plain `xterm-256color`, CI), since
    // `rgb()` then quantizes every fact span to `Color::Indexed`.
    let neutral = jcode_tui_style::color::rgb(140, 140, 150);
    for needle in ["OpenAI · OAuth", "GPT-5.6 Sol high", "~/jcode"] {
        let y = row_containing(&rows, needle);
        let byte_x = rows[y].find(needle).expect("fact text start");
        let x = UnicodeWidthStr::width(&rows[y][..byte_x]) as u16;
        let width = UnicodeWidthStr::width(needle) as u16;
        assert!(
            (x..x + width)
                .filter(|&cell_x| buffer[(cell_x, y as u16)].symbol() != " ")
                .all(|cell_x| buffer[(cell_x, y as u16)].fg == neutral),
            "{needle:?} should use only neutral gray"
        );
    }

    let context_y = row_containing(&rows, "74k/256k");
    let filled_x = rows[context_y].find('▰').expect("filled context cell");
    let filled_x = UnicodeWidthStr::width(&rows[context_y][..filled_x]) as u16;
    assert_ne!(buffer[(filled_x, context_y as u16)].fg, neutral);
}

#[test]
fn right_fact_stack_shifts_up_when_scheduled_notification_row_is_absent() {
    // Pins `display.session_facts = "right"` instead of inheriting the global
    // config. These tests assert exact rows for the right-anchored stack, so a
    // sibling test that sets the edge (or a developer config that does) must not
    // change what they see. `facts_test_locks` serializes them in the one safe
    // lock order.
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("right");
    clear_flicker_frame_history_for_tests();
    let mut state = fact_test_state(String::new(), false);
    state.display_messages = vec![DisplayMessage::assistant("first line\nsecond line")];
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("fact stack frame without notification");

    let rows = buffer_rows(&terminal);
    let [oauth_y, model_y, dir_y, context_y] = assert_fact_stack_is_contiguous(&rows);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
    let input = layout.input_area.expect("input area");
    let status = crate::tui::ui::last_status_area().expect("status area");

    assert_eq!(context_y as u16, input.bottom() - 1);
    assert_eq!(dir_y as u16, status.y);
    assert_eq!(model_y as u16, layout.messages_area.bottom() - 1);
    assert!(oauth_y < model_y);
    assert!(!rows.iter().any(|row| row.contains("next scheduled task")));
}

#[test]
fn right_fact_stack_leaves_fully_used_input_rows_untouched_and_moves_up() {
    // Pins `display.session_facts = "right"` instead of inheriting the global
    // config. These tests assert exact rows for the right-anchored stack, so a
    // sibling test that sets the edge (or a developer config that does) must not
    // change what they see. `facts_test_locks` serializes them in the one safe
    // lock order.
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("right");
    clear_flicker_frame_history_for_tests();
    let input = ["x".repeat(115), "y".repeat(115), "z".repeat(115)].join("\n");
    let state = fact_test_state(input, true);
    let backend = TestBackend::new(120, 22);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("fact stack frame with full input");

    let rows = buffer_rows(&terminal);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
    let input_area = layout.input_area.expect("input area");
    let input_rows = &rows[input_area.y as usize..input_area.bottom() as usize];
    assert!(input_rows.iter().all(|row| !row.contains("74k/256k")));
    assert!(input_rows.iter().all(|row| !row.contains("~/jcode")));
    assert!(input_rows.iter().all(|row| !row.contains("OAuth")));
    assert!(
        input_rows
            .iter()
            .map(|row| row.matches('x').count())
            .sum::<usize>()
            >= 110
    );
    assert!(row_containing(&rows, "74k/256k") < input_area.y as usize);
    assert_fact_stack_is_contiguous(&rows);
}

#[test]
fn right_fact_stack_survives_narrow_widths_without_overwriting_content() {
    // Pins `display.session_facts = "right"` instead of inheriting the global
    // config. These tests assert exact rows for the right-anchored stack, so a
    // sibling test that sets the edge (or a developer config that does) must not
    // change what they see. `facts_test_locks` serializes them in the one safe
    // lock order.
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("right");
    for width in (18_u16..=60).chain([80, 120, 160]) {
        clear_flicker_frame_history_for_tests();
        let state = fact_test_state("typed text".to_string(), width % 2 == 0);
        let backend = TestBackend::new(width, 16);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .unwrap_or_else(|error| panic!("fact stack failed at width {width}: {error}"));
        let rows = buffer_rows(&terminal);
        assert!(rows.iter().any(|row| row.contains("typed text")));
    }
}

#[test]
fn right_fact_stack_hides_as_a_unit_when_streaming_chrome_cannot_fit_it() {
    // Pins `display.session_facts = "right"` instead of inheriting the global
    // config. These tests assert exact rows for the right-anchored stack, so a
    // sibling test that sets the edge (or a developer config that does) must not
    // change what they see. `facts_test_locks` serializes them in the one safe
    // lock order.
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("right");
    clear_flicker_frame_history_for_tests();
    let mut state = fact_test_state(String::new(), true);
    state.status = ProcessingStatus::Streaming;
    state.streaming_text = "live transcript tail".to_string();
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("streaming fact stack frame");

    let rows = buffer_rows(&terminal);
    assert!(
        rows.iter().all(|row| {
            !row.contains("OpenAI · OAuth")
                && !row.contains("GPT-5.6 Sol high")
                && !row.contains("74k/256k")
        }),
        "the stack must hide completely rather than render a partial block:\n{}",
        rows.join("\n")
    );
}

#[test]
fn swarm_strip_full_draw_writes_chips_row_above_status_line() {
    let _lock = viewport_snapshot_test_lock();
    clear_flicker_frame_history_for_tests();
    // Placement state is process-global; a dock placed by another test would
    // make the strip stand down. Clear it so this frame is self-contained.
    crate::tui::info_widget::clear_widget_placements_for_tests();
    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("hello from the coordinator")],
        messages_version: 1,
        swarm_members: vec![
            strip_member("s1", "researcher", "running"),
            strip_member("s2", "reviewer", "completed"),
        ],
        ..Default::default()
    };

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("full draw with inline swarm strip should not panic");

    let status_area = crate::tui::ui::last_status_area().expect("status area recorded");
    assert!(status_area.y > 0, "status line should not be the top row");
    let rows = buffer_rows(&terminal);
    // Vertical strip (default layout): one agent per row directly above the
    // status line, first row carrying the 🐝 marker.
    let strip_rows = rows[..status_area.y as usize]
        .iter()
        .rev()
        .take(2)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        strip_rows.contains("🐝"),
        "expected the swarm marker above the status line, got: {strip_rows:?}"
    );
    assert!(
        strip_rows.contains("researcher"),
        "expected member row in strip cells, got: {strip_rows:?}"
    );
    assert!(
        strip_rows.contains("2/5"),
        "expected todo progress counter in strip cells, got: {strip_rows:?}"
    );
}

#[test]
fn swarm_strip_full_draw_survives_narrow_width_sweep() {
    let _lock = viewport_snapshot_test_lock();
    crate::tui::info_widget::clear_widget_placements_for_tests();
    let state = TestState {
        display_messages: vec![DisplayMessage::assistant("narrow sweep")],
        messages_version: 1,
        swarm_members: vec![
            strip_member("s1", "alpha", "running"),
            strip_member("s2", "beta", "ready"),
            strip_member("s3", "gamma", "failed"),
        ],
        ..Default::default()
    };

    for width in 12_u16..=44 {
        for height in [8_u16, 12, 20] {
            clear_flicker_frame_history_for_tests();
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| crate::tui::ui::draw(frame, &state))
                .unwrap_or_else(|e| {
                    panic!("swarm strip draw failed at {width}x{height}: {e}");
                });
        }
    }
}

#[test]
fn swarm_strip_full_draw_handles_wide_glyph_member_names() {
    let _lock = viewport_snapshot_test_lock();
    crate::tui::info_widget::clear_widget_placements_for_tests();
    let mut coordinator = strip_member("s0", "調整役エージェント", "running");
    coordinator.role = Some("coordinator".to_string());
    let mut streaming = strip_member("s1", "深度搜索智能体", "running");
    streaming.output_tail = Some("正在分析：渲染管線的寬字元邊界 🐝🎨".to_string());
    let members = vec![
        coordinator,
        streaming,
        strip_member("s2", "🦊🦊🦊 fox-agent 🦊🦊🦊", "completed"),
    ];

    // Unfocused (1 line) and focused (chips + preview + hints) variants both
    // must survive cell-level rendering with wide glyphs at every width.
    for focused in [false, true] {
        let state = TestState {
            display_messages: vec![DisplayMessage::assistant("wide glyph check")],
            messages_version: 1,
            swarm_members: members.clone(),
            swarm_panel_focused: focused,
            swarm_panel_selected: 1,
            ..Default::default()
        };
        for width in [24_u16, 25, 30, 31, 44, 80] {
            clear_flicker_frame_history_for_tests();
            let backend = TestBackend::new(width, 16);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| crate::tui::ui::draw(frame, &state))
                .unwrap_or_else(|e| {
                    panic!("wide-glyph strip draw failed at width {width} focused={focused}: {e}");
                });
        }
    }
}

#[test]
fn swarm_strip_paragraph_never_writes_outside_target_area() {
    // Mirror the exact ui.rs render path (Paragraph::new(lines) into a chunk),
    // but deliberately render lines built for a wider area into a narrow Rect
    // to prove clipping happens at the cell level, including wide glyphs.
    let members = vec![
        strip_member("s1", "深度搜索エージェント", "running"),
        strip_member("s2", "reviewer-with-a-long-name", "completed"),
    ];
    let gallery_lines = crate::tui::info_widget::swarm_gallery::render_swarm_strip_lines(
        &members, 0, true, "ctrl+t", 3, 80, 16,
    );
    assert!(!gallery_lines.is_empty(), "expected focused strip lines");

    let backend = TestBackend::new(40, 6);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let area = Rect::new(2, 1, 20, gallery_lines.len() as u16);
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new(gallery_lines.clone()), area);
        })
        .expect("over-wide strip paragraph should clip, not panic");

    let rows = buffer_rows(&terminal);
    for (y, row) in rows.iter().enumerate() {
        let cells: Vec<char> = row.chars().collect();
        let inside_rows = (area.y as usize)..(area.y + area.height) as usize;
        if !inside_rows.contains(&y) {
            assert!(
                row.trim().is_empty(),
                "row {y} outside strip area should be untouched, got: {row:?}"
            );
            continue;
        }
        for (x, ch) in cells.iter().enumerate() {
            if x < area.x as usize || x >= (area.x + area.width) as usize {
                assert_eq!(
                    *ch, ' ',
                    "cell ({x},{y}) outside strip area must stay blank, got {ch:?} in row {row:?}"
                );
            }
        }
    }
    let first_row = &rows[area.y as usize];
    assert!(
        !first_row.trim().is_empty(),
        "strip content should be written inside the area"
    );
}

#[test]
fn notification_full_draw_survives_overwide_swarm_plan_notice() {
    let _lock = viewport_snapshot_test_lock();
    let notice = "Swarm plan v3 · 12/24 tasks · gate 'critique-swarm-ui' blocked · \
                  reassigning 深度搜索エージェント → sheep-1 · awaiting verify-buffer-draw \
                  · retry budget 2/5 · ⚠ worker fox timed out"
        .to_string();

    for (width, height) in [(30_u16, 12_u16), (44, 16), (80, 24)] {
        clear_flicker_frame_history_for_tests();
        let state = TestState {
            display_messages: vec![DisplayMessage::assistant("plan running")],
            messages_version: 1,
            status_notice: Some(notice.clone()),
            ..Default::default()
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .unwrap_or_else(|e| {
                panic!("over-wide notification draw failed at {width}x{height}: {e}");
            });

        let status_area = crate::tui::ui::last_status_area().expect("status area recorded");
        let rows = buffer_rows(&terminal);
        let notification_row = &rows[(status_area.y + 1) as usize];
        assert!(
            notification_row.contains("Swarm plan v3"),
            "expected notification cells below status line at {width}x{height}, got: {notification_row:?}"
        );
    }
}

/// Regression: the inline swarm strip must not oscillate against the dock.
///
/// The strip row grows the bottom chrome, so every appearance shoves the
/// transcript up one row. When the strip keyed off raw last-frame dock
/// visibility, the dock's natural placement churn (hidden-in-place blinks
/// while content scrolls under it) made the strip pop in and out every few
/// frames: visible up/down flicker. Now the stand-down is sticky: an anchored
/// (hidden-in-place) dock still counts as engaged, and disengagement is
/// debounced by a linger.
#[test]
fn swarm_strip_stands_down_through_dock_blinks() {
    let _lock = viewport_snapshot_test_lock();
    use crate::tui::info_widget::{
        WidgetKind, calculate_placements, swarm_strip_stands_down_for_dock,
    };
    crate::tui::info_widget::clear_widget_placements_for_tests();

    let mut coordinator = strip_member("s0", "researcher", "running");
    coordinator.role = Some("coordinator".to_string());
    let data = crate::tui::info_widget::InfoWidgetData {
        swarm_info: Some(crate::tui::info_widget::SwarmInfo {
            managed_members: vec![coordinator, strip_member("s1", "reviewer", "completed")],
            ..Default::default()
        }),
        ..Default::default()
    };
    let messages_area = Rect::new(0, 0, 120, 26);
    let wide_margins = crate::tui::info_widget::Margins {
        right_widths: vec![44; 26],
        ..Default::default()
    };
    // Zero free margin: the dock cannot render this frame (a wide line is
    // covering its slot), so it hides in place behind its anchor.
    let covered_margins = crate::tui::info_widget::Margins {
        right_widths: vec![0; 26],
        ..Default::default()
    };

    assert!(
        !swarm_strip_stands_down_for_dock(),
        "no dock engagement yet: strip should be free to show"
    );

    // Dock places: strip stands down.
    let placed = calculate_placements(messages_area, &wide_margins, &data);
    assert!(
        placed.iter().any(|p| p.kind == WidgetKind::SwarmStatus),
        "dock should place with a wide free margin"
    );
    assert!(
        swarm_strip_stands_down_for_dock(),
        "strip must stand down while the dock shows"
    );

    // Full-draw integration: with the dock engaged, ui::draw omits the strip
    // row above the status line (TestState's empty widget data means this
    // draw's own widget pass will clear the engagement afterwards, so this
    // must be checked before continuing the state-machine sequence).
    {
        let state = TestState {
            display_messages: vec![DisplayMessage::assistant("coordinating agents")],
            messages_version: 1,
            swarm_members: vec![
                strip_member("s0", "researcher", "running"),
                strip_member("s1", "reviewer", "completed"),
            ],
            ..Default::default()
        };
        clear_flicker_frame_history_for_tests();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("draw with engaged dock should not panic");
        let status_area = crate::tui::ui::last_status_area().expect("status area recorded");
        let rows = buffer_rows(&terminal);
        let above_status = rows[..status_area.y as usize]
            .iter()
            .rev()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !above_status.contains("🐝") && !above_status.contains("researcher"),
            "strip must not render while the dock stands it down, got: {above_status:?}"
        );
    }

    // Re-engage (the integration draw above cleared state via its own empty
    // widget pass), then blink the dock hidden-in-place: anchor retained,
    // nothing placed. The strip must NOT pop back for the blink.
    crate::tui::info_widget::clear_widget_placements_for_tests();
    calculate_placements(messages_area, &wide_margins, &data);
    let blink = calculate_placements(messages_area, &covered_margins, &data);
    assert!(
        blink.iter().all(|p| p.kind != WidgetKind::SwarmStatus),
        "covered margin must hide the dock this frame"
    );
    assert!(
        swarm_strip_stands_down_for_dock(),
        "strip must keep standing down through a hidden-in-place dock blink"
    );

    // Even after the anchor is abandoned (hidden too long), the linger keeps
    // the strip down so a re-homing dock does not race a strip pop-in.
    for _ in 0..32 {
        calculate_placements(messages_area, &covered_margins, &data);
    }
    assert!(
        swarm_strip_stands_down_for_dock(),
        "strip must keep standing down through the post-disengage linger"
    );

    // A real teardown (widget pass skipped entirely) releases the stand-down.
    crate::tui::info_widget::note_widget_pass_skipped();
    assert!(
        !swarm_strip_stands_down_for_dock(),
        "strip should be free to return once the dock is genuinely gone"
    );
}

/// The swarm dock widget renders the compact summary at the cell level:
/// place it through the real `calculate_placements` + `render_all` path into
/// a TestBackend and assert the summary + progress bar landed inside the
/// placement rect.
#[test]
fn swarm_dock_widget_full_render_writes_agent_rows_in_margin() {
    let _lock = viewport_snapshot_test_lock();
    let mut coordinator = strip_member("s0", "researcher", "running");
    coordinator.role = Some("coordinator".to_string());
    coordinator.output_tail = Some("tracing the refresh path".to_string());
    let data = crate::tui::info_widget::InfoWidgetData {
        swarm_info: Some(crate::tui::info_widget::SwarmInfo {
            managed_members: vec![coordinator, strip_member("s1", "reviewer", "completed")],
            plan_progress: Some((3, 2, 7)),
            ..Default::default()
        }),
        ..Default::default()
    };

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let messages_area = Rect::new(0, 0, 120, 26);
    let margins = crate::tui::info_widget::Margins {
        right_widths: vec![44; 26],
        left_widths: Vec::new(),
        centered: false,
        ..Default::default()
    };
    let mut dock_rect: Option<Rect> = None;
    terminal
        .draw(|frame| {
            let placements =
                crate::tui::info_widget::calculate_placements(messages_area, &margins, &data);
            dock_rect = placements
                .iter()
                .find(|p| p.kind == crate::tui::info_widget::WidgetKind::SwarmStatus)
                .map(|p| p.rect);
            crate::tui::info_widget::render_all(frame, &placements, &data);
        })
        .expect("dock widget render should not panic");

    let rect = dock_rect.expect("SwarmStatus dock should be placed with a wide free margin");
    let rows = buffer_rows(&terminal);
    let dock_text: String = rows[rect.y as usize..(rect.y + rect.height) as usize]
        .iter()
        .map(|row| {
            row.chars()
                .skip(rect.x as usize)
                .take(rect.width as usize)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        dock_text.contains("1/2 agents"),
        "expected agents tally inside dock rect, got:\n{dock_text}"
    );
    assert!(
        dock_text.contains("nodes 3/7"),
        "expected node progress in dock header, got:\n{dock_text}"
    );
    assert!(
        dock_text.contains('▁'),
        "expected plan progress bar inside dock rect, got:\n{dock_text}"
    );
    // Nothing from the dock leaked left of its rect.
    for row in &rows[rect.y as usize..(rect.y + rect.height) as usize] {
        let left: String = row.chars().take(rect.x as usize).collect();
        assert!(
            left.trim().is_empty(),
            "dock must not write left of its rect, got: {left:?}"
        );
    }
}

#[test]
fn draw_notification_clips_overwide_notice_at_area_width() {
    let notice: String = "Swarm plan v3 · 12/24 tasks · gate blocked · ".repeat(8);
    let state = TestState {
        status_notice: Some(notice),
        ..Default::default()
    };

    let backend = TestBackend::new(60, 3);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let area = Rect::new(2, 1, 20, 1);
    terminal
        .draw(|frame| input_ui::draw_notification(frame, &state, area))
        .expect("over-wide notification should clip, not panic");

    let rows = buffer_rows(&terminal);
    assert!(rows[0].trim().is_empty(), "row above area must be blank");
    assert!(rows[2].trim().is_empty(), "row below area must be blank");
    let cells: Vec<char> = rows[1].chars().collect();
    for (x, ch) in cells.iter().enumerate() {
        if x < area.x as usize || x >= (area.x + area.width) as usize {
            assert_eq!(
                *ch, ' ',
                "cell ({x},1) outside notification area must stay blank, got {ch:?}"
            );
        }
    }
    let inside: String = cells[area.x as usize..(area.x + area.width) as usize]
        .iter()
        .collect();
    assert!(
        inside.starts_with("Swarm plan v3"),
        "expected clipped notice text inside area, got: {inside:?}"
    );
}

/// Both process-global locks these tests need, always acquired in the same
/// order: env first, then the viewport snapshot.
///
/// Order matters and is the whole point of this helper. The command tests take
/// the env lock and then touch config; the frame tests here take the viewport
/// lock. A guard that grabbed the env lock *after* the viewport lock created a
/// lock-order inversion that deadlocked the suite outright (tests sat at "has
/// been running for over 60 seconds" forever). Funnelling both through one
/// helper makes the order impossible to get wrong per-test.
struct FactsTestLocks {
    _env: crate::storage::TestEnvGuard,
    _viewport: crate::tui::ui::RenderStateTestGuard,
}

fn facts_test_locks() -> FactsTestLocks {
    let env = crate::storage::lock_test_env();
    let viewport = viewport_snapshot_test_lock();
    FactsTestLocks {
        _env: env,
        _viewport: viewport,
    }
}

/// Env guard for `display.session_facts` (JCODE_SESSION_FACTS), so a test can
/// pick an edge without touching the developer's real config. Locking is the
/// caller's job via [`facts_test_locks`].
struct SessionFactsEnvGuard;

impl SessionFactsEnvGuard {
    fn set(mode: &str) -> Self {
        crate::env::set_var("JCODE_SESSION_FACTS", mode);
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for SessionFactsEnvGuard {
    fn drop(&mut self) {
        crate::env::remove_var("JCODE_SESSION_FACTS");
        crate::config::invalidate_config_cache();
    }
}

/// Column where a fact row's text starts, measured in real buffer cells.
///
/// Deliberately not string-width math on a joined row: a wide glyph (the ⏰ on
/// the scheduled-task row) occupies two cells, and reconstructing columns from
/// the joined string drifts by one against the buffer. Scanning cells compares
/// like with like.
fn fact_cell_start(terminal: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
    let buf = terminal.backend().buffer();
    let width = buf.area.width;
    let height = buf.area.height;
    for y in (0..height).rev() {
        let cells: Vec<String> = (0..width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        for start in 0..width {
            let mut joined = String::new();
            let mut end = start;
            while end < width && joined.chars().count() < needle.chars().count() {
                joined.push_str(&cells[end as usize]);
                end += 1;
            }
            if joined == needle {
                return (start, end);
            }
        }
    }
    panic!("missing {needle:?} in frame");
}

/// `session_facts = "left"` moves the whole stack to the left edge of the
/// composer. The rows keep their order and stay contiguous; only the anchor
/// changes.
#[test]
fn session_facts_left_anchors_the_stack_to_the_left_edge() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("left");
    // A left gutter only exists in centered mode; see the fallback in
    // `draw_right_fact_stack` for what happens without one.
    let mut state = fact_test_state(String::new(), true);
    state.centered_mode = true;
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("left fact stack frame");

    let rows = buffer_rows(&terminal);
    // Same four rows, same order, still one uninterrupted block.
    let [oauth_y, model_y, dir_y, context_y] = assert_fact_stack_is_contiguous(&rows);
    assert!(oauth_y < model_y && model_y < dir_y && dir_y < context_y);

    // Every row starts hard against the left edge (one cell of padding) rather
    // than being right-aligned at the far side of a 120-column terminal.
    for needle in ["OpenAI · OAuth", "GPT-5.6 Sol high", "74k/256k"] {
        let (x, _) = fact_cell_start(&terminal, needle);
        assert_eq!(x, 1, "{needle:?} should hug the left edge, got column {x}");
    }
}

/// The default keeps the stack right-aligned, which is what every existing
/// layout expects. Paired with the left test so a regression that ignores the
/// setting entirely cannot pass both.
#[test]
fn session_facts_right_keeps_the_stack_on_the_right_edge() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("right");
    let state = fact_test_state(String::new(), true);
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("right fact stack frame");

    let rows = buffer_rows(&terminal);
    assert_fact_stack_is_contiguous(&rows);
    let terminal_width = terminal.backend().buffer().area.width;
    // Full row text, not a prefix: the assertion below is about where each row
    // *ends*, and the context row continues into its gauge.
    for needle in ["OpenAI · OAuth", "GPT-5.6 Sol high", "74k/256k ▰▰▱▱▱▱ 29%"] {
        let (x, end) = fact_cell_start(&terminal, needle);
        assert!(
            x > terminal_width / 2,
            "{needle:?} should sit on the right half, got column {x}"
        );
        // One cell of padding is kept against the edge, and the transcript
        // scrollbar may claim one more column on transcript rows.
        assert!(
            end >= terminal_width - 2 && end <= terminal_width - 1,
            "{needle:?} should end within a cell or two of the right edge, got {end} of {terminal_width}"
        );
    }
}

/// `session_facts = "off"` paints nothing at all. The facts remain available
/// through the info widgets, so this is a placement preference, not data loss.
#[test]
fn session_facts_off_paints_no_stack_at_all() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("off");
    let state = fact_test_state(String::new(), true);
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("suppressed fact stack frame");

    let rows = buffer_rows(&terminal);
    for needle in ["OpenAI · OAuth", "GPT-5.6 Sol high", "74k/256k"] {
        assert!(
            !rows.iter().any(|row| row.contains(needle)),
            "{needle:?} should not be painted when session_facts is off:\n{}",
            rows.join("\n")
        );
    }
}

/// The stack must never overwrite composer content on either edge. A left
/// anchor is the risky one: the prompt marker and the typed text both live at
/// the left of the input row.
#[test]
fn left_session_facts_never_overwrite_the_prompt_or_typed_text() {
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("left");
    for width in (40_u16..=60).chain([80, 120, 160]) {
        clear_flicker_frame_history_for_tests();
        let typed = "typed text that must survive";
        let state = fact_test_state(typed.to_string(), width % 2 == 0);
        let backend = TestBackend::new(width, 16);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("left fact stack frame");

        let rows = buffer_rows(&terminal);
        assert!(
            rows.iter().any(|row| row.contains(typed)),
            "typed text must survive at width {width}:\n{}",
            rows.join("\n")
        );
    }
}

/// Left-aligned mode has no left gutter to composite into, so the facts get
/// reserved rows below the composer instead. The setting must be honored there
/// exactly as in centered mode: same order, contiguous, hugging column 1. The
/// old behavior silently flipped to the right edge, which read as the setting
/// being ignored.
#[test]
fn left_session_facts_use_reserved_rows_without_a_left_gutter() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("left");
    let mut state = fact_test_state(String::new(), true);
    // Not centered: every row starts at column 0, so the left edge is busy.
    state.centered_mode = false;
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("left reserved-row frame");

    let rows = buffer_rows(&terminal);
    let [oauth_y, model_y, dir_y, context_y] = assert_fact_stack_is_contiguous(&rows);
    assert!(oauth_y < model_y && model_y < dir_y && dir_y < context_y);
    for needle in ["OpenAI · OAuth", "GPT-5.6 Sol high", "74k/256k"] {
        let (x, _) = fact_cell_start(&terminal, needle);
        assert_eq!(
            x, 1,
            "{needle:?} should hug the left edge in left-aligned mode, got column {x}"
        );
    }
}

/// The reserved rows sit at the very bottom of the chat column, below the
/// input. Anchoring anywhere else would leave the facts floating mid-screen
/// whenever the transcript is shorter than the terminal.
#[test]
fn left_reserved_facts_sit_below_the_input() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("left");
    let typed = "typed text that must survive";
    let mut state = fact_test_state(typed.to_string(), true);
    state.centered_mode = false;
    let backend = TestBackend::new(120, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &state))
        .expect("left reserved-row frame");

    let rows = buffer_rows(&terminal);
    let input_y = rows
        .iter()
        .position(|row| row.contains(typed))
        .expect("typed input row");
    let first_fact_y = rows
        .iter()
        .position(|row| row.contains("OpenAI · OAuth"))
        .expect("fact row");
    assert!(
        first_fact_y > input_y,
        "facts should be reserved below the input (input {input_y}, facts {first_fact_y}):\n{}",
        rows.join("\n")
    );
}

/// Env guard for `display.context_widget` (JCODE_CONTEXT_WIDGET).
struct ContextWidgetEnvGuard;

impl ContextWidgetEnvGuard {
    fn set(mode: &str) -> Self {
        crate::env::set_var("JCODE_CONTEXT_WIDGET", mode);
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for ContextWidgetEnvGuard {
    fn drop(&mut self) {
        crate::env::remove_var("JCODE_CONTEXT_WIDGET");
        crate::config::invalidate_config_cache();
    }
}

/// The user's actual request: the context reading moves to the input, it does
/// not appear in both places. Once the fact stack has drawn its context row,
/// the side card stands down under `auto`.
#[test]
fn context_card_stands_down_once_the_fact_stack_draws_context() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("left");
    let _ctx = ContextWidgetEnvGuard::set("auto");
    let _drew = crate::tui::info_widget::SessionFactsContextDrawnGuard::set(false);

    let mut state = fact_test_state(String::new(), true);
    state.centered_mode = true;
    // Let the info widgets run: this test is about them yielding.
    state.suppress_info_widgets = false;
    state.info_widget_data.context_info = Some(crate::prompt::ContextInfo {
        total_chars: 400_000,
        ..Default::default()
    });

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    // First frame primes the flag, second observes the card standing down.
    for _ in 0..2 {
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("context dedupe frame");
    }

    // Guard against a vacuous pass: the card standing down only means
    // anything if the stack really painted the context row this frame.
    let rows = buffer_rows(&terminal);
    assert!(
        rows.iter().any(|row| row.contains("74k/256k")),
        "the fact stack must actually show context for this test to mean anything:\n{}",
        rows.join("\n")
    );
    assert!(
        !crate::tui::info_widget::context_widget_visible(),
        "once the stack drew context, the side card should stand down"
    );
    let data = &state.info_widget_data;
    assert!(
        !data.show_context(),
        "no context render or height path should run while the stack owns it"
    );
}

/// The dedupe keys off what the stack actually drew, not what is configured.
/// With the stack suppressed, the card must keep showing context: hiding it
/// would take the reading off screen entirely.
#[test]
fn context_card_returns_when_the_fact_stack_draws_nothing() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("off");
    let _ctx = ContextWidgetEnvGuard::set("auto");
    let _drew = crate::tui::info_widget::SessionFactsContextDrawnGuard::set(true);

    let mut state = fact_test_state(String::new(), true);
    state.suppress_info_widgets = false;
    state.info_widget_data.context_info = Some(crate::prompt::ContextInfo {
        total_chars: 400_000,
        ..Default::default()
    });

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    for _ in 0..2 {
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("context restore frame");
    }

    assert!(
        crate::tui::info_widget::context_widget_visible(),
        "with the stack off, the side card must keep reporting context"
    );
    assert!(state.info_widget_data.show_context());
}

/// `context_widget = "on"` keeps both, for anyone who wants the card back
/// alongside the stack.
#[test]
fn context_card_can_be_forced_on_alongside_the_stack() {
    let _locks = facts_test_locks();
    let _ctx = ContextWidgetEnvGuard::set("on");
    let _drew = crate::tui::info_widget::SessionFactsContextDrawnGuard::set(true);
    assert!(crate::tui::info_widget::context_widget_visible());
}

/// Env guard for `display.model_widget` (JCODE_MODEL_WIDGET).
struct ModelWidgetEnvGuard;

impl ModelWidgetEnvGuard {
    fn set(mode: &str) -> Self {
        crate::env::set_var("JCODE_MODEL_WIDGET", mode);
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for ModelWidgetEnvGuard {
    fn drop(&mut self) {
        crate::env::remove_var("JCODE_MODEL_WIDGET");
        crate::config::invalidate_config_cache();
    }
}

/// The user's request: with the model, effort, and provider now pinned bottom
/// left, the info widget must stop repeating them. What stays is the session
/// line, which the stack never reports.
#[test]
fn model_card_drops_its_identity_rows_once_the_fact_stack_draws_them() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("left");
    let _model = ModelWidgetEnvGuard::set("auto");

    let mut state = fact_test_state(String::new(), false);
    state.centered_mode = true;
    state.suppress_info_widgets = false;
    state.info_widget_data.session_count = Some(1);
    state.info_widget_data.session_name = Some("citadel snail".to_string());

    let backend = TestBackend::new(110, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    // First frame primes the drawn flag, second observes the card yielding.
    for _ in 0..2 {
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("model dedupe frame");
    }

    let rows = buffer_rows(&terminal);
    // Guard against a vacuous pass: the card yielding only means something if
    // the stack really painted those rows.
    let stack_row = rows
        .iter()
        .rposition(|row| row.contains("GPT-5.6 Sol high"))
        .expect("the fact stack must show the model for this test to mean anything");
    assert!(
        rows.iter().any(|row| row.contains("1 session")),
        "the session line has no equivalent in the stack and must survive:\n{}",
        rows.join("\n")
    );
    // Only the card is in scope: the onboarding banner at the top of an empty
    // transcript also names the model, and it is not a duplicate of the stack.
    // The card is identified by its border, which is the one region this gate
    // controls.
    let card_rows: Vec<&String> = rows.iter().filter(|row| row.contains('\u{2502}')).collect();
    assert!(
        !card_rows.is_empty(),
        "precondition: the model card should still be placed:\n{}",
        rows.join("\n")
    );
    for row in &card_rows {
        assert!(
            !row.contains("GPT-5.6 Sol") && !row.contains("OAuth") && !row.contains("openai"),
            "the card must not repeat what the stack reports, got: {row:?}"
        );
    }
    // And the stack row itself is untouched by the gate.
    assert!(rows[stack_row].contains("GPT-5.6 Sol high"));
    assert!(!state.info_widget_data.show_model_identity());
}

/// The gate keys off what the stack actually drew. With the stack off, the card
/// keeps stating the model: hiding it would take the identity off screen.
#[test]
fn model_card_keeps_its_identity_when_the_fact_stack_is_off() {
    let _locks = facts_test_locks();
    clear_flicker_frame_history_for_tests();
    let _facts = SessionFactsEnvGuard::set("off");
    let _model = ModelWidgetEnvGuard::set("auto");

    let mut state = fact_test_state(String::new(), false);
    state.suppress_info_widgets = false;

    let backend = TestBackend::new(110, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    for _ in 0..2 {
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("model restore frame");
    }

    let rows = buffer_rows(&terminal);
    assert!(
        rows.iter().any(|row| row.contains("GPT-5.6 Sol")),
        "with the stack off the card must keep stating the model:\n{}",
        rows.join("\n")
    );
    assert!(state.info_widget_data.show_model_identity());
}

/// `model_widget = "on"` keeps both, for anyone who wants the card unchanged.
#[test]
fn model_card_can_be_forced_on_alongside_the_stack() {
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("left");
    let _model = ModelWidgetEnvGuard::set("on");
    assert!(crate::tui::info_widget::model_widget_identity_visible());
}

/// `model_widget = "off"` drops the identity rows even without the stack, which
/// is the escape hatch for anyone who never wants them in the widget.
#[test]
fn model_card_identity_can_be_forced_off_without_the_stack() {
    let _locks = facts_test_locks();
    let _facts = SessionFactsEnvGuard::set("off");
    let _model = ModelWidgetEnvGuard::set("off");
    assert!(!crate::tui::info_widget::model_widget_identity_visible());
}
