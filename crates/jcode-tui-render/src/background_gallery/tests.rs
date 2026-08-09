use super::*;

fn task(id: &str, label: &str, status: BgStatus) -> BgTask {
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
        output_tail: Vec::new(),
        session_id: "s1".to_string(),
        is_current_session: true,
    }
}

fn text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn hints() -> Vec<SwarmStripHint> {
    vec![SwarmStripHint {
        key: "alt+↑/↓".to_string(),
        label: "select".to_string(),
    }]
}

#[test]
fn empty_task_list_renders_nothing() {
    assert!(render_bg_strip(&[], 0, false, &[], None, 0, 80, 4, 6).is_empty());
}

#[test]
fn unfocused_strip_lists_tasks_with_ids_and_tally() {
    let tasks = vec![
        task("111111aaaa", "cargo test --all", BgStatus::Running),
        task("222222bbbb", "selfdev build", BgStatus::Completed),
    ];
    let lines = render_bg_strip(&tasks, 0, false, &[], Some("alt+b controls"), 0, 80, 4, 6);
    let out = text(&lines);
    assert!(out.contains("111111aaaa"), "{out}");
    assert!(out.contains("cargo test --all"), "{out}");
    assert!(out.contains("1/2 running"), "{out}");
    assert!(out.contains("alt+b controls"), "{out}");
}

/// Running tasks are what the user is waiting on, so they lead regardless of
/// where they landed in the adapter's start-time ordering.
#[test]
fn running_tasks_sort_before_finished_ones() {
    let tasks = vec![
        task("111111aaaa", "finished", BgStatus::Completed),
        task("222222bbbb", "still going", BgStatus::Running),
    ];
    let ordered = sort_tasks_for_display(&tasks);
    assert_eq!(ordered[0].id, "222222bbbb");
}

#[test]
fn focused_strip_expands_selected_output_under_its_row() {
    let mut tasks = vec![
        task("111111aaaa", "cargo test", BgStatus::Running),
        task("222222bbbb", "other", BgStatus::Running),
    ];
    tasks[0].output_tail = vec![
        "compiling foo".to_string(),
        "[stderr] warning: unused".to_string(),
    ];

    let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 10);
    let out = text(&lines);
    assert!(out.contains("compiling foo"), "{out}");
    assert!(out.contains("[stderr] warning: unused"), "{out}");
    assert!(out.contains("select"), "hint line missing: {out}");

    // Output must appear after the selected row and before the other task.
    let sel = out.find("111111aaaa").expect("selected row");
    let body = out.find("compiling foo").expect("output");
    let other = out.find("222222bbbb").expect("other row");
    assert!(sel < body && body < other, "wrong order: {out}");
}

#[test]
fn unfocused_strip_does_not_expand_output() {
    let mut tasks = vec![task("111111aaaa", "cargo test", BgStatus::Running)];
    tasks[0].output_tail = vec!["secret build line".to_string()];
    let lines = render_bg_strip(&tasks, 0, false, &hints(), None, 0, 80, 4, 8);
    assert!(!text(&lines).contains("secret build line"));
}

#[test]
fn selection_is_clamped_and_windowed_to_stay_visible() {
    let tasks: Vec<BgTask> = (0..20)
        .map(|i| {
            task(
                &format!("{:06}aaaa", i),
                &format!("task-{i}"),
                BgStatus::Running,
            )
        })
        .collect();

    // Out-of-range selection must not panic and must clamp to the last task.
    let lines = render_bg_strip(&tasks, 999, true, &hints(), None, 0, 80, 4, 10);
    let out = text(&lines);
    assert!(
        out.contains("task-19"),
        "last task should be visible: {out}"
    );
}

#[test]
fn overflow_marker_counts_hidden_tasks() {
    let tasks: Vec<BgTask> = (0..10)
        .map(|i| {
            task(
                &format!("{:06}aaaa", i),
                &format!("task-{i}"),
                BgStatus::Running,
            )
        })
        .collect();
    let lines = render_bg_strip(&tasks, 0, false, &[], None, 0, 80, 3, 4);
    let out = text(&lines);
    assert!(out.contains("more"), "expected overflow marker: {out}");
}

/// The strip lives in the bottom chrome, so exceeding the caller's budget in
/// either dimension shoves the transcript around. Never do it.
#[test]
fn strip_never_exceeds_width_or_height_budget() {
    let mut tasks: Vec<BgTask> = (0..8)
        .map(|i| {
            task(
                &format!("{:06}aaaa", i),
                "a very long command line that will certainly need truncating somewhere",
                BgStatus::Running,
            )
        })
        .collect();
    tasks[0].output_tail = (0..40).map(|i| format!("output line {i}")).collect();
    tasks[0].progress = Some("[####------] 40% · Building".to_string());

    for width in [8usize, 12, 20, 40, 80, 200] {
        for height in [1usize, 2, 3, 5, 8, 14] {
            for focused in [false, true] {
                let lines = render_bg_strip(
                    &tasks,
                    3,
                    focused,
                    &hints(),
                    Some("alt+b"),
                    0,
                    width,
                    5,
                    height,
                );
                assert!(
                    lines.len() <= height,
                    "height {height} exceeded ({} lines) at width {width}",
                    lines.len()
                );
                for line in &lines {
                    let rendered: String =
                        line.spans.iter().map(|s| s.content.to_string()).collect();
                    assert!(
                        disp_w(&rendered) <= width,
                        "width {width} exceeded by {rendered:?}"
                    );
                }
            }
        }
    }
}

/// Multi-line commands (heredocs, `for` loops) are routine; a raw newline
/// would smear one logical row across several terminal rows.
#[test]
fn multiline_labels_and_output_stay_on_one_row_each() {
    let mut tasks = vec![task(
        "111111aaaa",
        "printf '%s\n' one\nsleep 1",
        BgStatus::Running,
    )];
    tasks[0].output_tail = vec!["line one\nline two".to_string()];
    let lines = render_bg_strip(&tasks, 0, true, &[], None, 0, 80, 4, 8);
    for line in &lines {
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!rendered.contains('\n'), "embedded newline in {rendered:?}");
    }
}

#[test]
fn orphaned_tasks_are_labelled_not_shown_as_running() {
    let tasks = vec![task("111111aaaa", "stale", BgStatus::Orphaned)];
    assert_eq!(BgStatus::Orphaned.label(), "orphaned");
    assert!(!BgStatus::Orphaned.is_active());
    let lines = render_bg_strip(&tasks, 0, false, &[], None, 0, 80, 4, 6);
    assert!(text(&lines).contains("0/1 running"));
}

#[test]
fn page_shows_scope_and_selected_output() {
    let mut tasks = vec![
        task("111111aaaa", "cargo test", BgStatus::Running),
        task("222222bbbb", "selfdev build", BgStatus::Completed),
    ];
    tasks[0].output_tail = vec!["page output line".to_string()];

    let lines = render_bg_page(&tasks, 0, false, 0, 80, 20);
    let out = text(&lines);
    assert!(out.contains("Background tasks"), "{out}");
    assert!(out.contains("this session"), "{out}");
    assert!(out.contains("page output line"), "{out}");

    let all = text(&render_bg_page(&tasks, 0, true, 0, 80, 20));
    assert!(all.contains("all sessions"), "{all}");
}

#[test]
fn page_handles_empty_and_tiny_geometry() {
    let out = text(&render_bg_page(&[], 0, false, 0, 80, 10));
    assert!(out.contains("No background tasks"), "{out}");

    let tasks: Vec<BgTask> = (0..200)
        .map(|i| {
            task(
                &format!("{:06}aaaa", i),
                &format!("task-{i}"),
                BgStatus::Running,
            )
        })
        .collect();
    for height in [0usize, 1, 2, 5, 40] {
        for width in [8usize, 30, 120] {
            let lines = render_bg_page(&tasks, 150, false, 0, width, height);
            assert!(lines.len() <= height, "height {height} exceeded");
            for line in &lines {
                let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
                assert!(
                    disp_w(&rendered) <= width,
                    "width {width} exceeded by {rendered:?}"
                );
            }
        }
    }
}

#[test]
fn durations_format_compactly() {
    assert_eq!(format_duration(0.4), "0s");
    assert_eq!(format_duration(12.0), "12s");
    assert_eq!(format_duration(62.0), "1m02s");
    assert_eq!(format_duration(3725.0), "1h02m");
}

#[test]
fn running_tasks_animate_and_terminal_states_do_not() {
    let a = status_glyph(BgStatus::Running, 0);
    let b = status_glyph(BgStatus::Running, 1);
    assert_ne!(a, b, "running glyph should animate");
    assert_eq!(status_glyph(BgStatus::Completed, 0), "✓");
    assert_eq!(status_glyph(BgStatus::Completed, 7), "✓");
}

/// Long output must show the NEWEST lines, not the oldest.
///
/// A build that has been running for minutes has thousands of lines; the ones
/// that matter are at the end. Replacing the tail slice with `start = 0` (show
/// the head) passed the entire suite, because every other fixture has fewer
/// output lines than the budget, so the slice never truncated and the
/// distinction was invisible.
#[test]
fn long_output_shows_the_newest_lines_not_the_oldest() {
    let mut tasks = vec![task("111111aaaa", "cargo test", BgStatus::Running)];
    tasks[0].output_tail = (1..=40).map(|i| format!("line-{i:02}")).collect();

    // Budget far smaller than the output, so the slice must actually choose.
    let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 8);
    let out = text(&lines);

    assert!(
        out.contains("line-40"),
        "the newest line must be visible; a build's progress is at the end:\n{out}"
    );
    assert!(
        !out.contains("line-01"),
        "the oldest line must have scrolled off, otherwise the panel shows the \
         head of the output forever and never current progress:\n{out}"
    );
}

/// Same rule on the full page, which is where long output is actually read.
#[test]
fn the_page_also_shows_the_newest_output_lines() {
    let mut tasks = vec![task("111111aaaa", "cargo test", BgStatus::Running)];
    tasks[0].output_tail = (1..=60).map(|i| format!("line-{i:02}")).collect();

    let lines = render_bg_page(&tasks, 0, false, 0, 80, 20);
    let out = text(&lines);

    assert!(
        out.contains("line-60"),
        "newest line missing from page:\n{out}"
    );
    assert!(
        !out.contains("line-01"),
        "page must show the tail, not the head:\n{out}"
    );
}

/// stderr lines must be visually distinct from stdout.
///
/// The renderer colours `[stderr]` lines so "a failing command is legible at a
/// glance", but every test compares plain text (the `text()` helper drops
/// styles), so making stderr identical to stdout passed the whole suite. This
/// asserts on the spans themselves.
#[test]
fn stderr_lines_are_coloured_differently_from_stdout() {
    let mut tasks = vec![task("111111aaaa", "cargo test", BgStatus::Running)];
    tasks[0].output_tail = vec![
        "plain stdout line".to_string(),
        "[stderr] the failure reason".to_string(),
    ];

    let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 10);

    let colour_of = |needle: &str| -> Option<Color> {
        lines.iter().find_map(|line| {
            line.spans
                .iter()
                .find(|s| s.content.contains(needle))
                .and_then(|s| s.style.fg)
        })
    };

    let stdout_fg = colour_of("plain stdout line").expect("stdout line rendered");
    let stderr_fg = colour_of("the failure reason").expect("stderr line rendered");

    assert_ne!(
        stdout_fg, stderr_fg,
        "stderr must not render identically to stdout, or a failing command is \
         indistinguishable from normal progress"
    );
}

/// Every status must be visually distinguishable: distinct glyph and colour.
///
/// The glyph and accent are how a user tells "still running" from "failed" at
/// a glance. Collapsing either to a single value for all statuses passed the
/// whole suite: the existing tests assert individual glyphs but never that the
/// set is distinct, and nothing asserted the accent colours at all.
#[test]
fn each_status_has_a_distinct_glyph_and_accent() {
    use std::collections::HashSet;

    let all = [
        BgStatus::Running,
        BgStatus::Completed,
        BgStatus::Failed,
        BgStatus::Superseded,
        BgStatus::Orphaned,
    ];

    let glyphs: Vec<&str> = all.iter().map(|s| status_glyph(*s, 0)).collect();
    let unique: HashSet<&&str> = glyphs.iter().collect();
    assert_eq!(
        unique.len(),
        glyphs.len(),
        "statuses must not share a glyph, or running and failed look alike: {glyphs:?}"
    );

    let accents: Vec<Color> = all.iter().map(|s| status_accent(*s)).collect();
    let unique_accents: HashSet<String> = accents.iter().map(|c| format!("{c:?}")).collect();
    assert_eq!(
        unique_accents.len(),
        accents.len(),
        "statuses must not share an accent colour: {accents:?}"
    );

    // The two that matter most must be unmistakable.
    assert_ne!(
        status_accent(BgStatus::Failed),
        status_accent(BgStatus::Completed),
        "a failed task must not be coloured like a successful one"
    );
}

/// The strip must refuse to draw into geometry it cannot fit.
///
/// `empty_task_list_renders_nothing` covers the empty list, but nothing
/// covered a real task list against a degenerate viewport: removing the whole
/// `width < 8 || max_height == 0` guard passed the entire renderer suite. A
/// zero-height budget means the strip has no rows to use, and a very narrow
/// terminal leaves no room for the id plus a label, so drawing anyway either
/// overflows the row budget or emits unreadable fragments.
#[test]
fn the_strip_draws_nothing_into_degenerate_geometry() {
    let tasks = vec![
        task("111111aaaa", "cargo test --all", BgStatus::Running),
        task("222222bbbb", "selfdev build", BgStatus::Completed),
    ];

    assert!(
        render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 0).is_empty(),
        "a zero-height budget must produce no rows"
    );
    for width in [0usize, 1, 4, 7] {
        assert!(
            render_bg_strip(&tasks, 0, true, &hints(), None, 0, width, 4, 6).is_empty(),
            "width {width} is too narrow to render a readable row"
        );
    }

    // Just past the threshold it must draw again, so the guard is a floor and
    // not a silent disable.
    assert!(
        !render_bg_strip(&tasks, 0, false, &hints(), None, 0, 8, 4, 6).is_empty(),
        "width 8 is the documented minimum and must still render"
    );

    // And whatever it draws must respect the row budget at every size.
    for height in [1usize, 2, 3, 8] {
        for width in [8usize, 20, 120] {
            let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, width, 4, height);
            assert!(
                lines.len() <= height,
                "strip exceeded its height budget at {width}x{height}"
            );
        }
    }
}

#[test]
fn focused_detail_shows_the_command_behind_a_collapsed_label() {
    // The task row shows only "build", which does not say what is running.
    let mut tasks = vec![task("111111aaaa", "build", BgStatus::Running)];
    tasks[0].command = Some("cargo build --release -p jcode-tui".to_string());

    let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 10);
    let out = text(&lines);
    assert!(
        out.contains("cargo build --release -p jcode-tui"),
        "command not surfaced: {out}"
    );
    // It belongs under the row it describes, not above it.
    let row = out.find("111111aaaa").expect("task row");
    let cmd = out.find("cargo build").expect("command line");
    assert!(row < cmd, "command must render under its row: {out}");
}

#[test]
fn command_line_is_omitted_when_it_only_repeats_the_label() {
    let mut tasks = vec![task("111111aaaa", "cargo test", BgStatus::Running)];
    tasks[0].command = Some("cargo test".to_string());
    let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 10);
    let out = text(&lines);
    assert_eq!(
        out.matches("cargo test").count(),
        1,
        "duplicate command line wastes a row: {out}"
    );
}

#[test]
fn multiline_command_is_collapsed_to_one_row() {
    let mut tasks = vec![task("111111aaaa", "build", BgStatus::Running)];
    tasks[0].command = Some("set -e\nmake all".to_string());
    let lines = render_bg_strip(&tasks, 0, true, &hints(), None, 0, 80, 4, 10);
    for line in &lines {
        let content: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!content.contains('\n'), "embedded newline breaks layout");
    }
    assert!(text(&lines).contains("set -e"), "{}", text(&lines));
}
