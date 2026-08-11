// Tests for the inline chat todo card (`/todos` command + todo-card hotkey).

#[test]
fn toggle_todo_card_pushes_then_dismisses_trailing_card() {
    let mut app = create_test_app();
    assert!(!app.display_messages.iter().any(|m| m.role == "todos"));

    app.toggle_todo_card();
    assert_eq!(
        app.display_messages
            .iter()
            .filter(|m| m.role == "todos")
            .count(),
        1
    );
    assert_eq!(
        app.display_messages.last().map(|m| m.role.as_str()),
        Some("todos")
    );

    // Toggling again while the card is the trailing message dismisses it.
    app.toggle_todo_card();
    assert!(!app.display_messages.iter().any(|m| m.role == "todos"));
}

#[test]
fn toggle_todo_card_moves_stale_card_to_bottom_instead_of_stacking() {
    let mut app = create_test_app();
    app.toggle_todo_card();
    app.push_display_message(DisplayMessage::system("later activity".to_string()));

    // Card exists but is no longer trailing: toggling re-shows at the bottom.
    app.toggle_todo_card();
    let card_count = app
        .display_messages
        .iter()
        .filter(|m| m.role == "todos")
        .count();
    assert_eq!(card_count, 1, "the transcript keeps at most one todo card");
    assert_eq!(
        app.display_messages.last().map(|m| m.role.as_str()),
        Some("todos")
    );
}

#[test]
fn todos_command_defaults_to_card_and_panel_subcommand_keeps_side_panel() {
    let mut app = create_test_app();

    assert!(super::commands::handle_session_command(&mut app, "/todos"));
    assert!(app.display_messages.iter().any(|m| m.role == "todos"));
    assert!(!app.todos_view_enabled());

    assert!(super::commands::handle_session_command(
        &mut app,
        "/todos panel"
    ));
    assert!(app.todos_view_enabled());

    assert!(super::commands::handle_session_command(
        &mut app,
        "/todos off"
    ));
    assert!(!app.todos_view_enabled());
}

#[test]
fn todo_alias_shows_card() {
    let mut app = create_test_app();
    assert!(super::commands::handle_session_command(&mut app, "/todo"));
    assert!(app.display_messages.iter().any(|m| m.role == "todos"));
}

#[test]
fn refresh_todo_card_updates_content_when_todos_change() {
    let _env_lock = crate::storage::lock_test_env();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();

    let todo = |content: &str, status: &str| crate::todo::TodoItem {
        id: "t1".to_string(),
        content: content.to_string(),
        status: status.to_string(),
        priority: "high".to_string(),
        group: None,
        confidence: Some(crate::todo::ConfidenceState::from_legacy_score(70)),
        completion_confidence: None,
        confidence_history: Vec::new(),
        blocked_by: Vec::new(),
        assigned_to: None,
    };

    crate::todo::save_todos(&session_id, &[todo("write the card", "pending")]).unwrap();
    app.toggle_todo_card();
    let card = app
        .display_messages
        .iter()
        .find(|m| m.role == "todos")
        .expect("todo card pushed");
    assert!(card.content.contains("write the card"));
    assert!(card.content.contains("\"goals\""));

    // Unchanged todos: refresh is a no-op.
    assert!(!app.refresh_todo_card_if_needed());

    crate::todo::save_todos(&session_id, &[todo("write the card", "completed")]).unwrap();
    assert!(app.refresh_todo_card_if_needed());
    let card = app
        .display_messages
        .iter()
        .find(|m| m.role == "todos")
        .expect("todo card still present");
    assert!(card.content.contains("completed"));

    // Cleanup the persisted todo file for this throwaway session.
    let _ = crate::todo::save_todos(&session_id, &[]);
}

#[test]
fn refresh_todo_card_updates_content_when_goal_scores_change() {
    let _env_lock = crate::storage::lock_test_env();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();
    let todos = [crate::todo::TodoItem {
        id: "t1".to_string(),
        content: "render scores".to_string(),
        status: "in_progress".to_string(),
        priority: "high".to_string(),
        group: None,
        confidence: Some(crate::todo::ConfidenceState::from_legacy_score(80)),
        completion_confidence: None,
        confidence_history: Vec::new(),
        blocked_by: Vec::new(),
        assigned_to: None,
    }];
    let goal = |score| crate::todo::TodoGoal {
        group: None,
        closed_feedback_loop: Some(crate::todo::FeedbackLoopState::from_legacy_score(score)),
        feedback_loop: Some("inspect the frame".to_string()),
        delivery_state: Some(crate::todo::DeliveryState::from_legacy_score(90)),
        ..Default::default()
    };

    let plan = crate::todo::TodoPlan {
        user_intention: Some("keep the plan state visible".to_string()),
        understands_user_intent: Some(crate::todo::IntentUnderstanding::from_legacy_score(95)),
        ..Default::default()
    };

    crate::todo::save_todos(&session_id, &todos).unwrap();
    crate::todo::save_goals(&session_id, &[goal(70)]).unwrap();
    crate::todo::save_plan(&session_id, &plan).unwrap();
    app.toggle_todo_card();
    let card = app
        .display_messages
        .iter()
        .find(|message| message.role == "todos")
        .expect("todo card pushed");
    assert!(card.content.contains("\"closed_feedback_loop\":\"usable\""));
    assert!(
        card.content
            .contains("\"understands_user_intent\":\"partial\"")
    );

    crate::todo::save_goals(&session_id, &[goal(95)]).unwrap();
    assert!(app.refresh_todo_card_if_needed());
    let card = app
        .display_messages
        .iter()
        .find(|message| message.role == "todos")
        .expect("todo card still present");
    assert!(card.content.contains("\"closed_feedback_loop\":\"strong\""));

    let _ = crate::todo::save_todos(&session_id, &[]);
    let _ = crate::todo::save_goals(&session_id, &[]);
    let _ = crate::todo::save_plan(&session_id, &crate::todo::TodoPlan::default());
}

/// Simple todo used by the pinned-band tests.
fn pinned_band_todo(id: &str, content: &str, status: &str) -> crate::todo::TodoItem {
    crate::todo::TodoItem {
        id: id.to_string(),
        content: content.to_string(),
        status: status.to_string(),
        priority: "high".to_string(),
        group: None,
        confidence: Some(crate::todo::ConfidenceState::from_legacy_score(80)),
        completion_confidence: None,
        confidence_history: Vec::new(),
        blocked_by: Vec::new(),
        assigned_to: None,
    }
}

/// RAII guard for the JCODE_PIN_TODOS env override used by the band tests.
struct PinTodosEnvGuard;

impl PinTodosEnvGuard {
    fn enable() -> Self {
        Self::set("1")
    }

    /// The band is on by default, so a test that wants it off must say so.
    /// Relying on the default silently coupled these tests to it, and they
    /// all broke when upstream flipped `display.pin_todos` to true.
    fn disable() -> Self {
        Self::set("0")
    }

    fn set(value: &str) -> Self {
        crate::env::set_var("JCODE_PIN_TODOS", value);
        // jcode-base's config cache throttles env re-checks (the zero
        // interval under cfg!(test) applies only when jcode-base itself is
        // the crate under test), so force a reload or a sibling test's
        // JCODE_PIN_TODOS state leaks into this one for up to 500ms.
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for PinTodosEnvGuard {
    fn drop(&mut self) {
        crate::env::remove_var("JCODE_PIN_TODOS");
        // See enable(): flush the removal too, so later tests that expect
        // pin_todos off do not observe this test's stale cached config.
        crate::config::invalidate_config_cache();
    }
}

#[test]
fn pinned_todos_payload_stays_empty_when_config_off() {
    let _env_lock = crate::storage::lock_test_env();
    let _pin = PinTodosEnvGuard::disable();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();
    crate::todo::save_todos(&session_id, &[pinned_band_todo("t1", "pin me", "pending")]).unwrap();

    // Pin explicitly off: no payload, no redraw churn.
    assert!(!app.refresh_pinned_todos_if_needed());
    assert!(app.pinned_todos_payload_ref().is_none());

    let _ = crate::todo::save_todos(&session_id, &[]);
}

#[test]
fn pinned_todos_payload_refreshes_and_clears_with_config_and_todos() {
    let _env_lock = crate::storage::lock_test_env();
    let _pin = PinTodosEnvGuard::enable();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();

    // No todos yet: enabled but nothing to pin.
    app.refresh_pinned_todos_now();
    assert!(app.pinned_todos_payload_ref().is_none());

    crate::todo::save_todos(&session_id, &[pinned_band_todo("t1", "pin me", "pending")]).unwrap();
    app.refresh_pinned_todos_now();
    let payload = app
        .pinned_todos_payload_ref()
        .expect("payload populated when enabled with todos");
    assert!(payload.contains("pin me"));

    // Unchanged todos within the throttle window: no redraw.
    assert!(!app.refresh_pinned_todos_if_needed());

    // Todos cleared: payload clears too.
    crate::todo::save_todos(&session_id, &[]).unwrap();
    app.refresh_pinned_todos_now();
    assert!(app.pinned_todos_payload_ref().is_none());
}

#[test]
fn pinned_todos_are_omitted_from_info_widgets() {
    let _env_lock = crate::storage::lock_test_env();
    let _pin = PinTodosEnvGuard::enable();
    let app = create_test_app();
    let session_id = app.session.id.clone();
    crate::todo::save_todos(
        &session_id,
        &[pinned_band_todo("t1", "only in pinned band", "pending")],
    )
    .unwrap();

    let info = app.info_widget_data();
    assert!(info.todos.is_empty());
    assert!(info.todo_goals.is_empty());
    assert!(!info.has_data_for(crate::tui::info_widget::WidgetKind::Todos));

    crate::todo::save_todos(&session_id, &[]).unwrap();
}

#[test]
fn pinned_todos_hide_todo_tool_messages_from_the_transcript() {
    let _env_lock = crate::storage::lock_test_env();
    let _pin = PinTodosEnvGuard::enable();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();
    crate::todo::save_todos(
        &session_id,
        &[pinned_band_todo("pinned", "PINNED_ONLY", "in_progress")],
    )
    .unwrap();
    app.refresh_pinned_todos_now();
    app.display_messages = vec![
        DisplayMessage::tool(
            "duplicate todo transcript card",
            crate::message::ToolCall {
                id: "todo-tool".to_string(),
                name: "todo".to_string(),
                input: serde_json::json!({"todos": []}),
                intent: None,
                thought_signature: None,
            },
        ),
        DisplayMessage::tool(
            "ordinary tool remains visible",
            crate::message::ToolCall {
                id: "read-tool".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "README.md"}),
                intent: None,
                thought_signature: None,
            },
        ),
    ];
    app.bump_display_messages_version();
    app.session.short_name = Some("test".to_string());
    let backend = ratatui::backend::TestBackend::new(80, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    let transcript = render_and_snap(&app, &mut terminal);
    assert!(!transcript.contains("duplicate todo transcript card"));
    assert!(transcript.contains("PINNED_ONLY"), "{transcript}");
    let _ = crate::todo::save_todos(&session_id, &[]);
}

#[test]
fn pinned_todo_band_renders_below_sticky_prompt_without_separator() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let _pin = PinTodosEnvGuard::enable();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();
    crate::todo::save_todos(
        &session_id,
        &[pinned_band_todo("t1", "pinned band item", "in_progress")],
    )
    .unwrap();
    app.refresh_pinned_todos_now();
    assert!(app.pinned_todos_payload_ref().is_some());

    app.display_messages = vec![
        DisplayMessage {
            role: "user".to_string(),
            content: "kick off the work".to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        },
        DisplayMessage {
            role: "assistant".to_string(),
            content: App::build_scroll_test_content(0, 40, None),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        },
    ];
    app.bump_display_messages_version();
    app.scroll_offset = 0;
    app.auto_scroll_paused = false;
    app.is_processing = false;
    app.streaming.streaming_text.clear();
    app.status = ProcessingStatus::Idle;
    app.session.short_name = Some("test".to_string());

    let backend = ratatui::backend::TestBackend::new(60, 16);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");

    app.auto_scroll_paused = true;
    let top_text = render_and_snap(&app, &mut terminal);
    assert!(
        top_text
            .lines()
            .take(6)
            .any(|row| row.contains("pinned band item")),
        "pinned todo should remain visible at the top of scrollback, got:\n{}",
        top_text
    );

    app.auto_scroll_paused = false;
    let text = render_and_snap(&app, &mut terminal);

    let first_rows = text.lines().take(6).collect::<Vec<_>>();
    let prompt_row = first_rows
        .iter()
        .position(|row| row.contains("kick off the work"))
        .expect("sticky prompt should be visible");
    let todo_row = first_rows
        .iter()
        .position(|row| row.contains("pinned band item"))
        .expect("pinned todo should be visible");
    assert!(
        prompt_row < todo_row,
        "pinned todo band should render below the sticky prompt, got:\n{}",
        text
    );
    assert!(
        !first_rows.iter().any(|row| row.contains("────")),
        "pinned todo band should not render a horizontal separator, got:\n{}",
        text
    );

    let _ = crate::todo::save_todos(&session_id, &[]);
}

/// Env guard for `display.todo_widget` (JCODE_TODO_WIDGET).
struct TodoWidgetEnvGuard;

impl TodoWidgetEnvGuard {
    fn set(mode: &str) -> Self {
        crate::env::set_var("JCODE_TODO_WIDGET", mode);
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for TodoWidgetEnvGuard {
    fn drop(&mut self) {
        crate::env::remove_var("JCODE_TODO_WIDGET");
        crate::config::invalidate_config_cache();
    }
}

/// `display.todo_widget = auto` (the default) must not repeat the todo list on
/// the side while `display.pin_todos` keeps it pinned above the transcript.
#[test]
fn side_todo_widget_hides_under_auto_when_pinned_band_is_on() {
    use crate::tui::TuiState;

    let _env_lock = crate::storage::lock_test_env();
    let app = create_test_app();
    let session_id = app.session.id.clone();
    crate::todo::save_todos(&session_id, &[pinned_band_todo("t1", "pin me", "pending")]).unwrap();

    // `gather_todos_and_goals_for_session` warms its cache on a background
    // thread, so the first read is always empty regardless of config. Assert
    // on `show_todos()`, which is what every render and height path consults;
    // the raw `todos` vector stays populated by design.
    fn wait_for_widget_todos(app: &App) -> crate::tui::info_widget::InfoWidgetData {
        for _ in 0..200 {
            let data = app.info_widget_data();
            if !data.todos.is_empty() {
                return data;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.info_widget_data()
    }

    // Pin off: the side widget is the only place the list shows, so keep it.
    {
        let _pin_off = PinTodosEnvGuard::disable();
        let _widget = TodoWidgetEnvGuard::set("auto");
        assert!(crate::tui::info_widget::todo_widget_visible());
        assert!(
            wait_for_widget_todos(&app).show_todos(),
            "side widget should carry the todo list when nothing is pinned"
        );
    }

    // Pin on: `auto` yields to the pinned band.
    {
        let _pin = PinTodosEnvGuard::enable();
        let _widget = TodoWidgetEnvGuard::set("auto");
        assert!(!crate::tui::info_widget::todo_widget_visible());
        let data = wait_for_widget_todos(&app);
        assert!(!data.show_todos());
        assert!(!data.has_data_for(crate::tui::info_widget::WidgetKind::Todos));

        // Explicit `on` keeps both, for people who want the duplicate.
        let _widget_on = TodoWidgetEnvGuard::set("on");
        assert!(crate::tui::info_widget::todo_widget_visible());
        assert!(
            wait_for_widget_todos(&app).show_todos(),
            "`on` should keep the side widget even with the pinned band"
        );
    }

    // Explicit `off` hides the side widget even with no pinned band.
    {
        let _pin_off = PinTodosEnvGuard::disable();
        let _widget = TodoWidgetEnvGuard::set("off");
        assert!(!crate::tui::info_widget::todo_widget_visible());
        assert!(
            !app.info_widget_data()
                .has_data_for(crate::tui::info_widget::WidgetKind::Todos)
        );
    }

    let _ = crate::todo::save_todos(&session_id, &[]);
}

/// `/todos status` reports the side-widget mode and its effective visibility.
#[test]
fn todos_status_reports_side_widget_mode() {
    let _env_lock = crate::storage::lock_test_env();
    let mut app = create_test_app();

    {
        let _widget = TodoWidgetEnvGuard::set("off");
        let status = super::todos_view::todos_view_status_message(&app);
        assert!(
            status.contains("Side todo widget (display.todo_widget): off (currently hidden)"),
            "status should report the widget mode, got:\n{}",
            status
        );
    }

    {
        let _pin_off = PinTodosEnvGuard::disable();
        let _widget = TodoWidgetEnvGuard::set("auto");
        let status = super::todos_view::todos_view_status_message(&app);
        assert!(
            status.contains("Side todo widget (display.todo_widget): auto (currently shown)"),
            "auto with pin off should report shown, got:\n{}",
            status
        );
    }

    {
        let _pin_on = PinTodosEnvGuard::enable();
        let _widget = TodoWidgetEnvGuard::set("auto");
        let status = super::todos_view::todos_view_status_message(&app);
        assert!(
            status.contains("Side todo widget (display.todo_widget): auto (currently hidden)"),
            "auto must report hidden while the pinned band shows the list, got:\n{}",
            status
        );
    }

    // Unknown subcommands still surface usage, now mentioning `widget`.
    assert!(super::todos_view::handle_todos_view_command(
        &mut app,
        "/todos bogus"
    ));
    let usage = app
        .display_messages
        .iter()
        .rev()
        .find(|m| m.role == "error")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    assert!(
        usage.contains("widget"),
        "usage should list widget: {}",
        usage
    );
}

/// End-to-end frame check: with the pinned band on and `todo_widget = auto`,
/// a todo's text must appear exactly once in the rendered frame (the band),
/// and twice when the widget is forced `on`.
#[test]
fn pinned_band_and_side_widget_do_not_duplicate_todo_text_under_auto() {
    use crate::tui::TuiState;

    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let _pin = PinTodosEnvGuard::enable();
    let mut app = create_test_app();
    let session_id = app.session.id.clone();
    crate::todo::save_todos(
        &session_id,
        &[pinned_band_todo("t1", "zqx duplicate probe", "in_progress")],
    )
    .unwrap();
    app.refresh_pinned_todos_now();
    assert!(app.pinned_todos_payload_ref().is_some());

    app.display_messages = vec![DisplayMessage {
        role: "assistant".to_string(),
        content: App::build_scroll_test_content(0, 40, None),
        tool_calls: vec![],
        duration_secs: None,
        title: None,
        tool_data: None,
    }];
    app.bump_display_messages_version();
    app.scroll_offset = 0;
    app.auto_scroll_paused = false;
    app.is_processing = false;
    app.streaming.streaming_text.clear();
    app.status = ProcessingStatus::Idle;
    app.session.short_name = Some("test".to_string());

    fn probe_count(text: &str) -> usize {
        text.lines()
            .filter(|line| line.contains("zqx duplicate probe"))
            .count()
    }

    // Warm the async todo cache the info widget reads from, so a miss can't
    // masquerade as the feature working.
    for _ in 0..200 {
        if !app.info_widget_data().todos.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let backend = ratatui::backend::TestBackend::new(160, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");

    {
        let _widget_on = TodoWidgetEnvGuard::set("on");
        let text = render_and_snap(&app, &mut terminal);
        assert_eq!(
            probe_count(&text),
            2,
            "`on` should render the todo in both the band and the side widget, got:\n{}",
            text
        );
    }

    {
        let _widget_auto = TodoWidgetEnvGuard::set("auto");
        let text = render_and_snap(&app, &mut terminal);
        assert_eq!(
            probe_count(&text),
            1,
            "`auto` with a pinned band should render the todo exactly once, got:\n{}",
            text
        );
    }

    let _ = crate::todo::save_todos(&session_id, &[]);
}

/// The swarm-plan projection is not the list the pinned band shows, so `auto`
/// must leave it alone; only an explicit `off` hides it. Regression guard: an
/// earlier version cleared the plan in one gate while the overview heights
/// used a different one, which reserves rows for a section that never draws.
#[test]
fn swarm_plan_projection_survives_auto_but_not_off() {
    use crate::tui::info_widget::{InfoWidgetData, WidgetKind};

    let _env_lock = crate::storage::lock_test_env();

    let plan_data = |plan: bool| InfoWidgetData {
        todos: vec![pinned_band_todo("t1", "plan node", "in_progress")],
        todos_are_swarm_plan: plan,
        ..Default::default()
    };

    // `auto` + pinned band: the band already shows the session list, so the
    // side copy yields. The swarm plan is a different list and must survive.
    {
        let yielding = |plan: bool| crate::tui::info_widget::InfoWidgetData {
            todo_widget_yields_to_band: true,
            ..plan_data(plan)
        };

        let plan = yielding(true);
        assert!(plan.show_todos(), "auto must not hide the swarm plan");
        assert!(plan.has_data_for(WidgetKind::Todos));

        let session = yielding(false);
        assert!(
            !session.show_todos(),
            "auto + pinned band must hide this session's todo list"
        );
        assert!(!session.has_data_for(WidgetKind::Todos));
    }

    // `off` suppresses the side list outright, plan projection included.
    {
        let off = |plan: bool| crate::tui::info_widget::InfoWidgetData {
            todo_widget_mode_off: true,
            ..plan_data(plan)
        };
        let plan = off(true);
        assert!(!plan.show_todos(), "off must hide the plan too");
        assert!(!plan.has_data_for(WidgetKind::Todos));
    }
}

/// Layout-level guard: hiding the side todos must remove the placement and its
/// height, not just blank the text. A widget that is still placed but renders
/// nothing leaves a hole in the margin.
#[test]
fn hidden_todo_widget_is_neither_placed_nor_allocated_height() {
    use crate::tui::info_widget::{
        InfoWidgetData, WidgetKind, calculate_placements, calculate_widget_height,
    };
    use ratatui::prelude::Rect;

    let _env_lock = crate::storage::lock_test_env();

    let data = InfoWidgetData {
        todos: vec![pinned_band_todo("t1", "layout probe", "in_progress")],
        ..Default::default()
    };
    let area = Rect::new(0, 0, 160, 40);
    let margins = crate::tui::info_widget::Margins {
        right_widths: vec![40; 40],
        left_widths: vec![0; 40],
        centered: false,
        ..Default::default()
    };

    // Shown: the widget takes a placement and a non-zero height.
    {
        assert!(calculate_widget_height(WidgetKind::Todos, &data, 40, 20) > 0);
        assert!(
            calculate_placements(area, &margins, &data)
                .iter()
                .any(|p| p.kind == WidgetKind::Todos),
            "the todos widget should be placed when it is shown"
        );
    }

    // Hidden: no placement, no reserved rows, no rendered lines.
    {
        let data = InfoWidgetData {
            todo_widget_mode_off: true,
            ..data.clone()
        };
        assert_eq!(calculate_widget_height(WidgetKind::Todos, &data, 40, 20), 0);
        assert!(
            !calculate_placements(area, &margins, &data)
                .iter()
                .any(|p| p.kind == WidgetKind::Todos),
            "a hidden todos widget must not be placed"
        );
        assert!(
            !data.available_widgets().contains(&WidgetKind::Todos),
            "a hidden todos widget must leave the available set, so an anchored \
             resident from a previous frame is dropped instead of held in place"
        );
    }
}

/// The remote-disconnected key path routes a hand-listed set of local slash
/// commands. It used to enumerate spellings, which silently dropped
/// `/todos pin` and would have dropped `/todos widget` too. Assert the whole
/// `/todos` family reaches the local handler.
#[test]
fn every_todos_subcommand_is_handled_locally() {
    let _env_lock = crate::storage::lock_test_env();

    // `/todos pin` and `/todos widget` persist to config.toml. Point JCODE_HOME
    // at a temp dir so the run cannot rewrite the developer's real settings.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => crate::env::set_var("JCODE_HOME", home),
                None => crate::env::remove_var("JCODE_HOME"),
            }
            crate::config::invalidate_config_cache();
        }
    }

    let temp = tempfile::TempDir::new().expect("temp home");
    let _home = HomeGuard(std::env::var_os("JCODE_HOME"));
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    for command in [
        "/todo",
        "/todos",
        "/todos card",
        "/todos panel",
        "/todos on",
        "/todos off",
        "/todos pin",
        "/todos pin on",
        "/todos pin off",
        "/todos widget",
        "/todos widget auto",
        "/todos widget on",
        "/todos widget off",
        "/todos status",
    ] {
        let mut app = create_test_app();
        assert!(
            super::commands::handle_session_command(&mut app, command),
            "`{}` must be handled locally, not sent to the model",
            command
        );
        assert!(
            !app.display_messages
                .iter()
                .any(|m| m.role == "error" && m.content.contains("Usage: /todos")),
            "`{}` should not fall through to the usage error",
            command
        );
    }
}

/// `/facts` is a display preference, so every spelling must be handled locally
/// and must persist the setting. A command that parses but never reaches the
/// dispatch table would look like a no-op to the user.
#[test]
fn every_facts_spelling_is_handled_locally_and_persists() {
    use jcode_config_types::SessionFactsMode;

    let _env_lock = crate::storage::lock_test_env();

    // `/facts` writes config.toml. Point JCODE_HOME at a temp dir so the run
    // cannot rewrite the developer's real settings.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => crate::env::set_var("JCODE_HOME", home),
                None => crate::env::remove_var("JCODE_HOME"),
            }
            crate::config::invalidate_config_cache();
        }
    }

    let temp = tempfile::TempDir::new().expect("temp home");
    let _home = HomeGuard(std::env::var_os("JCODE_HOME"));
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::config::invalidate_config_cache();

    for (command, expected) in [
        ("/facts left", SessionFactsMode::Left),
        ("/facts right", SessionFactsMode::Right),
        ("/facts off", SessionFactsMode::Off),
        // Bare `/facts` toggles sides, and from `off` it brings the stack back
        // rather than flipping invisibly.
        ("/facts", SessionFactsMode::Right),
        ("/facts", SessionFactsMode::Left),
    ] {
        let mut app = create_test_app();
        assert!(
            super::commands::handle_facts_command(&mut app, command),
            "`{}` must be handled locally, not sent to the model",
            command
        );
        assert!(
            !app.display_messages.iter().any(|m| m.role == "error"),
            "`{}` should not produce an error message",
            command
        );
        crate::config::invalidate_config_cache();
        assert_eq!(
            crate::config::config().display.session_facts,
            expected,
            "`{}` should persist {:?}",
            command,
            expected
        );
    }

    // The context-card subcommand persists too, and toggles from bare form.
    for (command, expected) in [
        (
            "/facts context off",
            jcode_config_types::ContextWidgetMode::Off,
        ),
        (
            "/facts context on",
            jcode_config_types::ContextWidgetMode::On,
        ),
        (
            "/facts context auto",
            jcode_config_types::ContextWidgetMode::Auto,
        ),
    ] {
        let mut app = create_test_app();
        assert!(
            super::commands::handle_facts_command(&mut app, command),
            "`{}` must be handled locally",
            command
        );
        crate::config::invalidate_config_cache();
        assert_eq!(
            crate::config::config().display.context_widget,
            expected,
            "`{}` should persist {:?}",
            command,
            expected
        );
    }

    let mut app = create_test_app();
    assert!(super::commands::handle_facts_command(
        &mut app,
        "/facts context sideways"
    ));
    assert!(
        app.display_messages
            .iter()
            .any(|m| m.role == "error" && m.content.contains("Usage: /facts context")),
        "an unknown context argument should explain the valid ones"
    );

    // A bad argument reports usage instead of silently picking a side.
    let mut app = create_test_app();
    assert!(super::commands::handle_facts_command(
        &mut app,
        "/facts sideways"
    ));
    assert!(
        app.display_messages
            .iter()
            .any(|m| m.role == "error" && m.content.contains("Usage: /facts")),
        "an unknown argument should explain the valid ones"
    );

    // A command that merely starts with the same letters is not ours.
    let mut app = create_test_app();
    assert!(!super::commands::handle_facts_command(
        &mut app,
        "/factsheet please"
    ));
}
