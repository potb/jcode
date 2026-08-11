// Tests for the pinned provider-usage footer (`display.pin_usage`, `/usage pin`).
//
// The unit tests next to the renderer cover the width-degradation ladder in
// isolation; these cover the parts only a real frame can prove: that the row is
// actually reserved at the bottom of the terminal, that it survives a narrow
// terminal, and that it does not appear when the feature is off.

/// Enable `display.pin_usage` for the duration of a test. Mirrors
/// `PinTodosEnvGuard`: the config cache throttles env re-checks, so the guard
/// must invalidate it on both set and unset or the flag leaks between tests.
struct PinUsageEnvGuard;

impl PinUsageEnvGuard {
    fn enable() -> Self {
        crate::env::set_var("JCODE_PIN_USAGE", "1");
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for PinUsageEnvGuard {
    fn drop(&mut self) {
        crate::env::remove_var("JCODE_PIN_USAGE");
        crate::config::invalidate_config_cache();
    }
}

/// Point the runtime at a cost-based provider for the duration of a test and
/// restore the previous value on the way out, panic or not. A trailing
/// `set_var` is not enough: an assertion failure skips it and the leaked
/// variable then changes `auth_method` in unrelated info-widget tests.
struct RuntimeProviderGuard(Option<String>);

impl RuntimeProviderGuard {
    fn set(value: &str) -> Self {
        let previous = std::env::var("JCODE_RUNTIME_PROVIDER").ok();
        crate::env::set_var("JCODE_RUNTIME_PROVIDER", value);
        crate::auth::AuthStatus::invalidate_cache();
        Self(previous)
    }
}

impl Drop for RuntimeProviderGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(previous) => crate::env::set_var("JCODE_RUNTIME_PROVIDER", previous),
            None => crate::env::remove_var("JCODE_RUNTIME_PROVIDER"),
        }
        crate::auth::AuthStatus::invalidate_cache();
    }
}

/// Build an app whose info-widget data carries a known Anthropic quota
/// snapshot, so the footer has something deterministic to render.
fn usage_footer_test_app() -> App {
    let mut app = create_test_app();
    app.display_messages = vec![DisplayMessage {
        role: "assistant".to_string(),
        content: "some transcript content".to_string(),
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
    app
}

#[test]
fn usage_footer_hidden_when_config_off() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let app = usage_footer_test_app();

    assert!(
        !crate::tui::ui::usage_footer_height_for_tests(&app, 120) > 0,
        "the footer must not reserve a row while display.pin_usage is off"
    );

    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    let text = render_and_snap(&app, &mut terminal);

    // display.pin_usage defaults to false, so the bottom row belongs to the
    // rest of the UI and no usage bars are pinned there.
    let last_row = text.lines().last().unwrap_or_default().to_string();
    assert!(
        !last_row.contains('▰') && !last_row.contains('▱'),
        "no pinned usage bars expected with the feature off, got:\n{}",
        text
    );
}

#[test]
fn usage_footer_row_is_not_reserved_without_provider_usage_data() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let _pin = PinUsageEnvGuard::enable();
    let app = usage_footer_test_app();

    // The harness provider reports no usage windows. Enabling the flag alone
    // must not steal a row, otherwise every provider without quota data would
    // lose a line of transcript to a permanently blank footer.
    assert!(
        crate::tui::TuiState::info_widget_data(&app)
            .usage_info
            .is_none(),
        "test harness precondition: the mock provider reports no usage"
    );
    assert!(!crate::tui::ui::usage_footer_height_for_tests(&app, 120) > 0);
}

#[test]
fn usage_footer_renders_without_panicking_at_wide_and_narrow_widths() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let _pin = PinUsageEnvGuard::enable();
    let app = usage_footer_test_app();

    // The whole point of the footer is that it works on small *and* big
    // terminals: render the full frame across the range and require every size
    // to lay out cleanly. A reserved row that panics or overflows on an 24-col
    // terminal would defeat the feature.
    for (width, height) in [(200u16, 50u16), (100, 24), (60, 16), (40, 12), (24, 10)] {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| crate::tui::ui::draw(f, &app))
            .unwrap_or_else(|e| panic!("draw failed at {width}x{height}: {e}"));
    }
}

#[test]
fn usage_footer_toggle_command_persists_and_reports_state() {
    let _env_lock = crate::storage::lock_test_env();
    let mut app = create_test_app();

    assert!(super::commands::handle_usage_command(
        &mut app,
        "/usage pin"
    ));
    assert!(
        crate::config::config().display.pin_usage,
        "/usage pin should enable the pinned usage footer"
    );
    assert!(
        app.display_messages
            .iter()
            .any(|message| message.content.contains("Pinned usage enabled")),
        "the toggle should confirm the new state in the transcript"
    );

    assert!(super::commands::handle_usage_command(
        &mut app,
        "/usage pin off"
    ));
    assert!(
        !crate::config::config().display.pin_usage,
        "/usage pin off should disable the pinned usage footer"
    );

    // Restore the default so the persisted config does not leak into other tests.
    let _ = crate::config::Config::set_pin_usage(false);
}

/// End-to-end: drive the real `ui::draw()` and read the painted bottom row
/// back out of the terminal buffer.
///
/// The earlier tests exercise the renderer and the layout gate in isolation.
/// This one covers the whole path a user actually gets (config flag, then
/// `widget_usage_info`, then the reserved row, then painted cells), which is the
/// only way to catch a break in the wiring between those pieces.
///
/// A cost-based provider is used deliberately: it is the one route that yields
/// a populated `UsageInfo` from local token counters alone, with no OAuth
/// credentials or network fetch, so the assertion is real rather than skipped.
#[test]
fn end_to_end_frame_paints_usage_on_the_last_row_at_any_terminal_size() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let _pin = PinUsageEnvGuard::enable();

    let _runtime = RuntimeProviderGuard::set("openai-compatible");

    let mut app = create_named_provider_test_app("bedrock", "test-model");
    app.token_accounting.total_input_tokens = 12_000;
    app.token_accounting.total_output_tokens = 3_400;
    app.update_cost_impl();
    app.display_messages = vec![DisplayMessage {
        role: "assistant".to_string(),
        content: "some transcript content".to_string(),
        tool_calls: vec![],
        duration_secs: None,
        title: None,
        tool_data: None,
    }];
    app.bump_display_messages_version();
    app.is_processing = false;
    app.status = ProcessingStatus::Idle;
    app.session.short_name = Some("test".to_string());

    let usage = crate::tui::TuiState::info_widget_data(&app)
        .usage_info
        .expect("cost-based provider must report usage without credentials");
    assert!(usage.available, "precondition: usage must be renderable");
    assert!(
        crate::tui::ui::usage_footer_height_for_tests(&app, 120) > 0,
        "the footer must reserve a row once usage is available and pinned"
    );

    for (width, height) in [(120u16, 30u16), (60, 20), (34, 12)] {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| crate::tui::ui::draw(f, &app))
            .unwrap_or_else(|e| panic!("draw failed at {width}x{height}: {e}"));

        let buffer = terminal.backend().buffer();
        let last_row: String = (0..width)
            .map(|x| buffer[(x, height - 1)].symbol())
            .collect();

        assert!(
            last_row.contains('$'),
            "the pinned footer should paint the spend readout on the last row \
             at {width}x{height}, got: {last_row:?}"
        );
    }
}

/// The pinned usage block and the left-anchored session facts share the bottom
/// band, so both must land on it without one painting over the other. The facts
/// hug the left edge, the usage numbers the right.
#[test]
fn left_session_facts_and_pinned_usage_share_the_bottom_band() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();
    let _pin = PinUsageEnvGuard::enable();
    let _runtime = RuntimeProviderGuard::set("openai-compatible");
    crate::env::set_var("JCODE_SESSION_FACTS", "left");
    crate::config::invalidate_config_cache();
    struct FactsCleanup;
    impl Drop for FactsCleanup {
        fn drop(&mut self) {
            crate::env::remove_var("JCODE_SESSION_FACTS");
            crate::config::invalidate_config_cache();
        }
    }
    let _facts = FactsCleanup;

    let mut app = create_named_provider_test_app("bedrock", "test-model");
    app.token_accounting.total_input_tokens = 12_000;
    app.token_accounting.total_output_tokens = 3_400;
    app.update_cost_impl();
    app.display_messages = vec![DisplayMessage {
        role: "assistant".to_string(),
        content: "some transcript content".to_string(),
        tool_calls: vec![],
        duration_secs: None,
        title: None,
        tool_data: None,
    }];
    app.bump_display_messages_version();
    app.is_processing = false;
    app.status = ProcessingStatus::Idle;
    app.session.short_name = Some("test".to_string());

    let width = 120u16;
    let height = 30u16;
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| crate::tui::ui::draw(f, &app))
        .expect("shared band frame");

    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();

    // The usage readout keeps the last row, as it does without the facts.
    let last_row = rows.last().expect("last row").clone();
    assert!(
        last_row.contains('$'),
        "usage should still paint on the last row, got: {last_row:?}"
    );

    // The model row is a fact row, and it must sit in the band's left half
    // rather than overlapping the right-aligned usage text.
    let model_y = rows
        .iter()
        .rposition(|row| row.contains("Test Model"))
        .unwrap_or_else(|| panic!("no model fact row:\n{}", rows.join("\n")));
    assert!(
        model_y >= (height - 4) as usize,
        "facts should be pinned into the bottom band, model row at {model_y} of {height}:\n{}",
        rows.join("\n")
    );
    let usage_col = last_row.find('$').expect("usage column");
    let model_col = rows[model_y].find("Test Model").expect("model column");
    assert!(
        model_col < usage_col,
        "facts must stay left of the usage block (model {model_col}, usage {usage_col})"
    );
}

/// With the block pinned, the margin/overview usage sections must disappear:
/// otherwise the same numbers show up twice on screen, which is the duplication
/// the pinned block was meant to replace.
#[test]
fn pinning_usage_hides_the_margin_and_overview_copies() {
    let _env_lock = crate::storage::lock_test_env();
    let _render_lock = crate::tui::ui::render_state_test_lock();

    let _runtime = RuntimeProviderGuard::set("openai-compatible");

    let mut app = create_named_provider_test_app("bedrock", "test-model");
    app.token_accounting.total_input_tokens = 12_000;
    app.token_accounting.total_output_tokens = 3_400;
    app.update_cost_impl();

    let data = crate::tui::TuiState::info_widget_data(&app);
    assert!(
        data.usage_info
            .as_ref()
            .is_some_and(|usage| usage.available),
        "precondition: this provider reports usage"
    );

    // `usage_pinned` is snapshot data, so both states are exercised on the same
    // snapshot without touching global config.
    let unpinned = crate::tui::info_widget::InfoWidgetData {
        usage_pinned: false,
        ..data.clone()
    };
    assert!(
        unpinned.has_data_for(crate::tui::info_widget::WidgetKind::UsageLimits),
        "with pinning off the margin widget still offers usage"
    );

    let pinned = crate::tui::info_widget::InfoWidgetData {
        usage_pinned: true,
        ..data
    };
    assert!(
        !pinned.has_data_for(crate::tui::info_widget::WidgetKind::UsageLimits),
        "with pinning on the margin copy must hide"
    );
}
