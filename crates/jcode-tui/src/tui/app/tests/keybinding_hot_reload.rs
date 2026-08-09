// Editing `[keybindings]` in config.toml must take effect on the very next
// keystroke, without a restart and without waiting for an idle tick (which can
// be as slow as the 5s deep-idle cadence).
#[test]
fn keybinding_edit_applies_to_the_next_key_press() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::config::Config::invalidate_cache();

    let config_path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(config_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_path,
        "[keybindings]\nscroll_bookmark = \"ctrl+g\"\n",
    )
    .expect("write initial config");

    let mut app = create_test_app();
    assert!(
        app.scroll_keys
            .is_bookmark(KeyCode::Char('g'), KeyModifiers::CONTROL),
        "initial config should bind the bookmark key to Ctrl+G"
    );

    // Rebind on disk. Length differs as well as mtime so the config
    // fingerprint notices the edit on coarse-timestamp filesystems.
    std::fs::write(
        &config_path,
        "[keybindings]\nscroll_bookmark = \"ctrl+y\"\n# edited\n",
    )
    .expect("rewrite config");

    // The config cache re-stats the file on a 500ms throttle, so wait past it
    // to model a user who edits the file and then reaches for the keyboard.
    std::thread::sleep(std::time::Duration::from_millis(600));

    // The very next key press must already see the new binding.
    app.handle_key_press_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))
        .expect("handle key press");

    assert!(
        app.scroll_keys
            .is_bookmark(KeyCode::Char('y'), KeyModifiers::CONTROL),
        "edited config should rebind the bookmark key to Ctrl+Y without a restart"
    );
    assert!(
        !app.scroll_keys
            .is_bookmark(KeyCode::Char('g'), KeyModifiers::CONTROL),
        "the old Ctrl+G bookmark binding should no longer match"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::set_var(
            "JCODE_HOME",
            crate::tui::app::tests::shared_test_jcode_home(),
        );
    }
    crate::config::Config::invalidate_cache();
}

// A config reload that leaves `[keybindings]` untouched must not claim the
// config was reloaded.
//
// The reload generation is bumped by *any* config invalidation, so keying the
// notice on it alone announced "Config reloaded from disk" for edits to
// unrelated settings, and for invalidations the user never made at all. The
// notice is the user's only signal that a keybinding edit landed, so a false
// one directly undermines it.
#[test]
fn an_unrelated_config_reload_does_not_claim_a_keybinding_reload() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::config::Config::invalidate_cache();

    let config_path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(config_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_path,
        "[keybindings]\nscroll_bookmark = \"ctrl+g\"\n",
    )
    .expect("write initial config");

    let mut app = create_test_app();

    // Rewrite the file with the same bindings but a different unrelated
    // setting, then invalidate so the generation moves without the bindings
    // changing. This is what a concurrent config write looks like to the app.
    std::fs::write(
        &config_path,
        "[keybindings]\nscroll_bookmark = \"ctrl+g\"\n\n[display]\ncentered = true\n",
    )
    .expect("rewrite config");
    crate::config::Config::invalidate_cache();

    assert!(
        !app.refresh_keybindings_if_config_reloaded(),
        "unchanged bindings should report no reload"
    );
    assert!(
        app.status_notice().is_none(),
        "unchanged bindings must not raise the reload notice: {:?}",
        app.status_notice()
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::set_var(
            "JCODE_HOME",
            crate::tui::app::tests::shared_test_jcode_home(),
        );
    }
    crate::config::Config::invalidate_cache();
}
