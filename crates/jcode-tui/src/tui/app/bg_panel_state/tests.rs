use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

/// The focused panel is an overlay, not a modal: plain typing must keep
/// reaching the chat input, so only Esc and Alt-chords may be claimed.
#[test]
fn only_esc_and_alt_chords_are_claimed() {
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Esc, KeyModifiers::NONE),
        Some(BgPanelAction::Exit)
    );
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Down, KeyModifiers::ALT),
        Some(BgPanelAction::SelectNext)
    );
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Up, KeyModifiers::ALT),
        Some(BgPanelAction::SelectPrev)
    );
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Char('j'), KeyModifiers::ALT),
        Some(BgPanelAction::SelectNext)
    );
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Char('k'), KeyModifiers::ALT),
        Some(BgPanelAction::SelectPrev)
    );
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Char('a'), KeyModifiers::ALT),
        Some(BgPanelAction::ToggleAllSessions)
    );

    // Plain keys, including the ones that are Alt-chords above, must fall
    // through to the input box.
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('x'),
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Enter,
    ] {
        assert_eq!(
            bg_panel_action_for_key(code, KeyModifiers::NONE),
            None,
            "{code:?} must not be claimed without Alt"
        );
    }
}

/// Esc with a modifier is not the panel's Esc: leave it to whoever wants it.
#[test]
fn modified_esc_is_not_claimed() {
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Esc, KeyModifiers::ALT),
        None
    );
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Esc, KeyModifiers::CONTROL),
        None
    );
}

/// The background panel must claim the same chords as the swarm panel so the
/// two feel identical, and must not claim the swarm-only ones.
#[test]
fn does_not_claim_swarm_only_chords() {
    // Alt+O pops out a swarm agent; the background panel has no such action
    // and must leave the chord alone.
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Char('o'), KeyModifiers::ALT),
        None
    );
}

/// Alt+B is shared with "move the running tool to the background". That action
/// is what creates background tasks, so the panel must not shadow it while a
/// tool is running, or the feature that fills the panel stops working.
///
/// This is a real regression that shipped briefly: the panel's key block sits
/// earlier in the dispatch chain than the background-tool handler, so without
/// the guard it swallowed the chord unconditionally.
#[test]
fn alt_b_yields_to_backgrounding_a_running_tool() {
    let mut app = crate::tui::app::tests::create_test_app();

    // Idle: the panel owns Alt+B.
    app.status = crate::tui::ProcessingStatus::Idle;
    assert!(!background_tool_action_owns_key(
        &app,
        KeyCode::Char('b'),
        KeyModifiers::ALT
    ));

    // Tool running: the background-tool action owns it.
    app.status = crate::tui::ProcessingStatus::RunningTool("bash".to_string());
    assert!(background_tool_action_owns_key(
        &app,
        KeyCode::Char('b'),
        KeyModifiers::ALT
    ));

    // Only the shared chord is guarded; a rebound panel key is unaffected.
    assert!(!background_tool_action_owns_key(
        &app,
        KeyCode::Char('w'),
        KeyModifiers::ALT
    ));
    // And plain 'b' (typing) is never the background-tool chord.
    assert!(!background_tool_action_owns_key(
        &app,
        KeyCode::Char('b'),
        KeyModifiers::NONE
    ));
}

/// Both dispatch chains must apply the running-tool guard. They are separate
/// hand-written chains that have drifted before, which is how this class of
/// bug appears. Driven rather than grepped: the local half goes through
/// `handle_key` below, this covers the remote half.
#[test]
fn both_dispatch_chains_guard_the_shared_chord() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    let mut app = crate::tui::app::tests::create_test_app();
    app.status = crate::tui::ProcessingStatus::RunningTool("bash".to_string());

    rt.block_on(app.handle_remote_key(KeyCode::Char('b'), KeyModifiers::ALT, &mut remote))
        .expect("remote alt+b");

    assert!(
        !crate::tui::TuiState::bg_panel_focused(&app),
        "the remote chain must let a running tool keep Alt+B"
    );
}

/// Drive Alt+B through the real local key-handling entry point.
///
/// Every runtime check of this feature ran against a headless tester, which
/// connects as a *remote* client and therefore only exercises
/// `remote/key_handling.rs`. The local chain in `input.rs` is a separate
/// hand-written dispatch list, and the two have drifted before (that is why
/// `commands_dispatch` exists). This drives `handle_key` so the local path is
/// covered by something other than reading it.
#[test]
fn local_key_path_cycles_the_panel_and_yields_to_a_running_tool() {
    let mut app = crate::tui::app::tests::create_test_app();

    // No tasks: the chord must not leave the panel focused, and must not panic.
    app.handle_key(KeyCode::Char('b'), KeyModifiers::ALT)
        .expect("alt+b with no tasks");
    assert!(
        !crate::tui::TuiState::bg_panel_focused(&app),
        "with no background tasks the panel must not open"
    );

    // With a tool running the chord belongs to the backgrounding action, so
    // the panel must stay closed regardless of task state.
    app.status = crate::tui::ProcessingStatus::RunningTool("bash".to_string());
    app.handle_key(KeyCode::Char('b'), KeyModifiers::ALT)
        .expect("alt+b while a tool runs");
    assert!(
        !crate::tui::TuiState::bg_panel_focused(&app),
        "alt+b must not open the panel while a tool is running"
    );

    // Typing must still reach the composer while the chord is in play: the
    // panel is an overlay, not a modal.
    app.status = crate::tui::ProcessingStatus::Idle;
    app.handle_key(KeyCode::Char('h'), KeyModifiers::NONE)
        .expect("plain typing");
    app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE)
        .expect("plain typing");
    assert_eq!(app.input, "hi", "plain keys must reach the chat input");
}

/// A declined Alt+B must never reach text input.
///
/// Regression: unbinding the readline word-back alias to free the chord for
/// the panel left no terminal handler for Alt+B in the local chain, so when
/// the panel declined (no tasks, or a tool running) the chord fell through and
/// typed a literal "b" into the composer. Both chains must consume it.
#[test]
fn a_declined_alt_b_never_types_a_literal_b() {
    // Behavioural check on the local chain, across both decline reasons.
    for status in [
        crate::tui::ProcessingStatus::Idle,
        crate::tui::ProcessingStatus::RunningTool("bash".to_string()),
    ] {
        let mut app = crate::tui::app::tests::create_test_app();
        app.status = status.clone();
        app.handle_key(KeyCode::Char('b'), KeyModifiers::ALT)
            .expect("alt+b");
        assert_eq!(
            app.input, "",
            "alt+b leaked into the composer with status {status:?}"
        );
    }
}

/// Alt+A is shared with "copy the chat viewport" (local chain, empty input).
///
/// The panel claims it only while focused, which is an explicit mode the user
/// entered, so the copy binding keeps working everywhere else. This mirrors how
/// the swarm panel scopes Alt+O. Asserted here because the precedence is a
/// function of handler ordering in a hand-written chain, which is exactly the
/// kind of thing that silently flips during a refactor.
#[test]
fn alt_a_only_belongs_to_the_panel_while_it_is_focused() {
    // Unfocused: the panel must not claim it, so the copy binding still runs.
    assert!(
        !bg_panel_action_for_key(KeyCode::Char('a'), KeyModifiers::NONE)
            .is_some_and(|action| action == BgPanelAction::ToggleAllSessions),
        "plain 'a' must never be a panel action"
    );

    let mut app = crate::tui::app::tests::create_test_app();
    // handle_bg_panel_key is gated on focus, so an unfocused app declines and
    // the chord falls through to the copy binding later in the chain.
    assert!(
        !app.handle_bg_panel_key(KeyCode::Char('a'), KeyModifiers::ALT),
        "an unfocused panel must not swallow alt+a"
    );

    // The action mapping itself is focus-independent; focus is enforced by the
    // handler, which is the contract the dispatch chain relies on.
    assert_eq!(
        bg_panel_action_for_key(KeyCode::Char('a'), KeyModifiers::ALT),
        Some(BgPanelAction::ToggleAllSessions)
    );
}

/// No Alt chord the panel binds may ever reach text input.
///
/// Two separate leaks of this kind have now been found: Alt+B (introduced by
/// unbinding word-back) and Alt+A with a non-empty composer (pre-existing, but
/// newly dangerous once the panel gave the chord a second meaning). An Alt
/// chord that types its own letter is always a bug, so this covers every chord
/// the panel claims rather than the two known cases.
#[test]
fn no_panel_alt_chord_leaks_into_the_composer() {
    for chord in ['a', 'b', 'j', 'k'] {
        for seed in ["", "xy"] {
            for status in [
                crate::tui::ProcessingStatus::Idle,
                crate::tui::ProcessingStatus::RunningTool("bash".to_string()),
            ] {
                let mut app = crate::tui::app::tests::create_test_app();
                app.set_input_for_test(seed);
                app.status = status.clone();
                app.handle_key(KeyCode::Char(chord), KeyModifiers::ALT)
                    .expect("alt chord");
                assert_eq!(
                    app.input, seed,
                    "alt+{chord} leaked into the composer (seed {seed:?}, status {status:?})"
                );
            }
        }
    }
}

/// The remote key chain must consume the panel's Alt chords too.
///
/// The sweep above drives `handle_key`, which is the LOCAL chain only. Remote
/// clients (including self-dev sessions) take a separate path, and it was the
/// asymmetry between the two that hid the Alt+A leak in the first place. An
/// earlier version of this test grepped both files for `KeyCode::Char('a')`,
/// which proved only that some source text existed; `handle_remote_key` is
/// directly callable, so assert on behavior instead.
#[test]
fn remote_chain_consumes_panel_alt_chords_without_typing_them() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    for chord in ['a', 'b', 'j', 'k'] {
        for seed in ["", "xy"] {
            let mut app = crate::tui::app::tests::create_test_app();
            app.set_input_for_test(seed);
            rt.block_on(app.handle_remote_key(
                KeyCode::Char(chord),
                KeyModifiers::ALT,
                &mut remote,
            ))
            .expect("remote alt chord");
            assert_eq!(
                app.input, seed,
                "remote alt+{chord} leaked into the composer (seed {seed:?})"
            );
        }
    }
}

/// Alt+B must not move the cursor: the readline word-back alias is unbound.
///
/// Freeing Alt+B for this panel meant removing its word-back alias, but a
/// later fix for "a declined Alt+B types a literal b" restored word-back as
/// the fallback arm. That silently reinstated the binding the user had asked
/// to remove, and it was invisible because every test only asserted on
/// `input`, which word-back does not change. Assert on the cursor too.
#[test]
fn alt_b_never_moves_the_cursor_by_word() {
    for status in [
        crate::tui::ProcessingStatus::Idle,
        crate::tui::ProcessingStatus::RunningTool("bash".to_string()),
    ] {
        let mut app = crate::tui::app::tests::create_test_app();
        app.set_input_for_test("hello world");
        app.cursor_pos = "hello world".len();
        app.status = status.clone();

        app.handle_key(KeyCode::Char('b'), KeyModifiers::ALT)
            .expect("alt+b");

        assert_eq!(
            app.cursor_pos,
            "hello world".len(),
            "alt+b moved the cursor by word (status {status:?}): the readline \
             alias is supposed to be unbound, Alt+Left still does this"
        );
        assert_eq!(app.input, "hello world", "alt+b must not edit the input");
    }

    // The word-back arm itself is untouched, so the capability is moved rather
    // than deleted. Asserted through handle_alt_key: Alt+Left does not reach
    // it via handle_key in this harness (confirmed on master, where Alt+Left
    // also leaves the cursor at 11), so driving handle_key here would assert a
    // property that never held rather than one this change affects.
    let mut app = crate::tui::app::tests::create_test_app();
    app.set_input_for_test("hello world");
    app.cursor_pos = "hello world".len();
    assert!(
        crate::tui::app::input::handle_alt_key(&mut app, KeyCode::Left),
        "alt+left must still be handled"
    );
    assert_eq!(
        app.cursor_pos, 6,
        "the word-back arm must survive: Alt+B lost the alias, Alt+Left keeps it"
    );
}

/// With no tasks, Alt+B must not claim it closed something that never opened.
#[test]
fn alt_b_with_no_tasks_says_there_are_none() {
    let mut app = crate::tui::app::tests::create_test_app();
    assert_eq!(
        app.cycle_bg_panel_view(),
        BgPanelCycle::NothingToShow,
        "with no tasks the cycle must report nothing to show, not a close"
    );

    app.handle_key(KeyCode::Char('b'), KeyModifiers::ALT)
        .expect("alt+b");
    let notice = crate::tui::TuiState::status_notice(&app);
    assert_eq!(
        notice.as_deref(),
        Some("No background tasks"),
        "pressing alt+b with no tasks must not report closing a view that was \
         never open"
    );
}

/// The focus chord is configurable, and rebinding it frees Alt+B.
///
/// "it's configurable, so unbind wordback and b to bg" was the premise for
/// taking Alt+B in the first place, but nothing verified the configurable
/// half. A binding that silently ignored the config would make Alt+B a
/// hard-coded seizure of a chord the user was told they could move.
///
/// `load_toggle_keys` reads the process-global config, so this drives the
/// parameterized layer underneath it, which is where the config string is
/// actually interpreted.
#[test]
fn the_focus_chord_can_be_rebound_and_alt_b_then_does_nothing() {
    use crate::tui::keybind::{ToggleBinding, background_panel_focus_default};

    let rebound = ToggleBinding::load_with_default("alt+g", background_panel_focus_default());
    assert!(
        rebound.matches(KeyCode::Char('g'), KeyModifiers::ALT),
        "a rebound chord must be honored"
    );
    assert!(
        !rebound.matches(KeyCode::Char('b'), KeyModifiers::ALT),
        "rebinding must release alt+b rather than binding both"
    );

    // The default supplied by the config layer is alt+b.
    let defaulted = ToggleBinding::load_with_default("alt+b", background_panel_focus_default());
    assert!(
        defaulted.matches(KeyCode::Char('b'), KeyModifiers::ALT),
        "the default string must resolve to alt+b"
    );

    // An empty string disables the chord entirely rather than falling back to
    // the default. That is the documented "set to empty to disable" behavior,
    // and it is the escape hatch for anyone who wants Alt+B left alone.
    let disabled = ToggleBinding::load_with_default("", background_panel_focus_default());
    assert!(
        !disabled.matches(KeyCode::Char('b'), KeyModifiers::ALT),
        "an empty binding must disable the chord, not silently re-take alt+b"
    );
}

/// Selection clamping: never past the last task, never below zero.
///
/// An off-by-one in the upper clamp (`count` instead of `count - 1`) escaped
/// the entire test suite, which is how this gap was found. Selecting past the
/// end renders a blank output pane and indexes a task that is not there.
#[test]
fn selection_moves_are_clamped_to_the_task_list() {
    // Typical navigation.
    assert_eq!(moved_selection(0, 4, 1), 1, "down moves one");
    assert_eq!(moved_selection(2, 4, -1), 1, "up moves one");

    // The bug the mutation exposed: never past the last index.
    assert_eq!(moved_selection(3, 4, 1), 3, "down at the end must stay put");
    assert_eq!(
        moved_selection(0, 4, 99),
        3,
        "a big jump clamps to the last"
    );
    assert_eq!(moved_selection(0, 1, 1), 0, "single task cannot move");

    // Never below zero (isize arithmetic makes this an underflow risk).
    assert_eq!(moved_selection(0, 4, -1), 0, "up at the top must stay put");
    assert_eq!(
        moved_selection(2, 4, -99),
        0,
        "a big jump back clamps to zero"
    );

    // Empty list: no tasks, no movement, no panic on count - 1.
    assert_eq!(moved_selection(0, 0, 1), 0, "empty list must not move");
    assert_eq!(moved_selection(5, 0, -1), 5, "empty list must not index");

    // A stale index (list shrank under us) is pulled back into range.
    assert_eq!(
        moved_selection(9, 3, 0),
        2,
        "a selection left over from a longer list must clamp into range"
    );
}

/// The status hints must name the chord the user actually configured.
///
/// `bg_view_hint`/`bg_page_hint` hardcoded "alt+b", so rebinding the panel
/// chord left every hint and the tips line telling the user to press a key
/// that no longer did anything. Nothing caught it: blanking the whole hint
/// function failed no test either, which is how the gap surfaced.
#[test]
fn the_status_hints_name_the_configured_chord() {
    let view = crate::tui::keybind::bg_view_hint("full page");
    let page = crate::tui::keybind::bg_page_hint();

    // Derive the expectation from the CONFIGURED chord, not from
    // `bg_panel_focus_key_label()`. Comparing the hints against the same
    // helper that builds them is a tautology: a mutation that made the helper
    // return the wrong chord moved both sides together and passed.
    let configured = crate::config::config()
        .keybindings
        .background_panel_focus
        .clone();
    // The hints render the Alt modifier with the platform keycap (`⌥` on
    // macOS), while the config stores it spelled out, so compare against the
    // configured chord translated into the platform label.
    let expected = configured
        .rsplit_once('+')
        .map(|(_, key)| jcode_tui_core::keybind::alt_chord_lower(key))
        .unwrap_or_else(|| configured.to_lowercase());

    for (name, hint) in [("strip", &view), ("page", &page)] {
        assert!(
            hint.to_lowercase().contains(&expected),
            "the {name} hint {hint:?} must name the configured chord {configured:?}"
        );
    }

    // Both hints must also advertise the keys that always work, since alt+j/k
    // are shadowed by workspace navigation.
    for hint in [&view, &page] {
        assert!(
            hint.contains("↑/↓"),
            "{hint:?} must advertise the arrow selection keys"
        );
        assert!(hint.contains("esc"), "{hint:?} must advertise esc to exit");
        assert!(
            !hint.is_empty(),
            "an empty hint tells the user nothing about the panel they just opened"
        );
    }
}
