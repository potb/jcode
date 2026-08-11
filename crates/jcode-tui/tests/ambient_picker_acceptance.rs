//! Acceptance check for issue #26 against a REAL ambient home.
//!
//! Unit tests build `SessionInfo` values by hand, which cannot catch a mismatch
//! between what an ambient cycle actually writes to disk and what the picker
//! reads back. This test drives the real picker loader (`load_sessions`) over a
//! real `JCODE_HOME` produced by an actual ambient cycle.
//!
//! Ignored by default: it needs a home where ambient has run. Produce one with
//!
//! ```text
//! jcode serve --socket <home>/srv.sock   # with [ambient] enabled = true
//! ```
//!
//! then run
//!
//! ```text
//! JCODE_AMBIENT_ACCEPT_HOME=<home> cargo test -p jcode-tui --test ambient_picker_acceptance -- --ignored --nocapture
//! ```

#[test]
#[ignore = "requires a JCODE_HOME where a real ambient cycle has run"]
fn real_ambient_cycle_is_listed_as_ambient_by_the_picker() {
    let Ok(home) = std::env::var("JCODE_AMBIENT_ACCEPT_HOME") else {
        panic!("set JCODE_AMBIENT_ACCEPT_HOME to a home where ambient has run");
    };
    let home = std::path::PathBuf::from(home);
    assert!(
        home.join("sessions").is_dir(),
        "no sessions dir in {home:?}"
    );

    // Safety: single-threaded test process, set before any path is resolved.
    unsafe { std::env::set_var("JCODE_HOME", &home) };

    let transcripts = home.join("ambient").join("transcripts");
    let transcript_count = std::fs::read_dir(&transcripts)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert!(
        transcript_count > 0,
        "no ambient transcripts in {transcripts:?}; did a cycle actually run?"
    );

    jcode_tui::tui::session_picker::invalidate_session_list_cache();
    let sessions =
        jcode_tui::tui::session_picker::load_sessions().expect("picker must load sessions");
    assert!(!sessions.is_empty(), "picker loaded no sessions at all");

    let ambient: Vec<_> = sessions.iter().filter(|s| s.is_ambient).collect();
    println!(
        "picker loaded {} sessions, {} classified ambient",
        sessions.len(),
        ambient.len()
    );
    for session in &sessions {
        println!(
            "  {} is_ambient={} is_debug={} source={:?} badge={:?}",
            session.id,
            session.is_ambient,
            session.is_debug,
            session.source,
            session.source.badge()
        );
    }

    assert!(
        !ambient.is_empty(),
        "a real ambient cycle ran but the picker classified nothing as ambient"
    );
    for session in &ambient {
        assert_eq!(
            session.source,
            jcode_tui_session_picker::SessionSource::Ambient,
            "ambient session must carry the Ambient source so the row gets a badge"
        );
        assert_eq!(session.source.badge(), Some("🌙 ambient"));
    }

    // The acceptance surface is the rendered picker: ambient cycles are marked
    // `is_debug`, so before this change they were invisible unless the user
    // turned on the test-session toggle. Render the real widget with the toggle
    // in its default (off) state and read the screen.
    //
    // A real self-dev session is added alongside so "ambient is distinguishable
    // from self-dev/swarm debug noise" is checked against an actual debug row,
    // not merely by the absence of a flask in a home that has none.
    let ambient_ids: Vec<String> = ambient.iter().map(|s| s.id.clone()).collect();
    let mut selfdev = sessions
        .iter()
        .find(|s| s.is_ambient)
        .cloned()
        .expect("an ambient session to clone the shape from");
    selfdev.id = "session_selfdev_probe".to_string();
    selfdev.short_name = "selfdevprobe".to_string();
    selfdev.title = "Self-dev debug session".to_string();
    selfdev.is_ambient = false;
    selfdev.is_debug = true;
    selfdev.source = jcode_tui_session_picker::SessionSource::Jcode;
    selfdev.resume_target = jcode_tui_session_picker::ResumeTarget::JcodeSession {
        session_id: selfdev.id.clone(),
    };

    let mut all = sessions.clone();
    all.push(selfdev);

    let render = |picker: &mut jcode_tui::tui::session_picker::SessionPicker| -> String {
        let backend = ratatui::backend::TestBackend::new(160, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| picker.render(frame))
            .expect("render picker");
        let screen = terminal.backend().buffer().content();
        let mut out = String::new();
        for (idx, cell) in screen.iter().enumerate() {
            if idx % 160 == 0 {
                out.push('\n');
            }
            out.push_str(cell.symbol());
        }
        out
    };

    let mut picker = jcode_tui::tui::session_picker::SessionPicker::new(all);
    let rendered = render(&mut picker);
    println!("--- rendered picker (all) ---{rendered}\n--- end ---");

    assert!(
        rendered.contains("🌙"),
        "an ambient marker must be on screen with the test-session toggle off"
    );
    for id in &ambient_ids {
        let short = id.split('_').nth(1).unwrap_or(id);
        assert!(
            rendered.contains(short),
            "ambient session {id} (short name {short}) must be listed with the toggle off"
        );
    }
    // The self-dev row is the control. It must be hidden while the toggle is
    // off, and the header's hidden counter proves the picker counted exactly it
    // (ambient rows must not be counted as hidden test noise).
    assert!(
        !rendered.contains("selfdevprobe") && !rendered.contains("Self-dev debug session"),
        "a plain debug session must still be hidden while the toggle is off"
    );
    assert!(
        rendered.contains("(+1 hidden)"),
        "exactly the one self-dev session may be hidden; ambient rows must not be counted as test noise"
    );

    // Ambient must also have its own filter mode, reachable by the same `s` key
    // a user presses. Turn test sessions on first (`d`) so the self-dev control
    // row is genuinely eligible: otherwise the debug toggle alone hides it and
    // "the filter excluded it" would prove nothing.
    picker
        .handle_overlay_key(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        )
        .expect("toggle test sessions on");
    let with_debug = render(&mut picker);
    assert!(
        !with_debug.contains("hidden)"),
        "with test sessions shown, nothing may remain hidden: {}",
        with_debug.lines().next().unwrap_or_default()
    );
    assert_eq!(
        picker.visible_session_count(),
        sessions.len() + 1,
        "every session, ambient plus the self-dev control, must be visible once test sessions are shown"
    );

    let mut reached_ambient_filter = None;
    for _ in 0..12 {
        picker
            .handle_overlay_key(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::NONE,
            )
            .expect("cycle filter mode");
        let frame = render(&mut picker);
        if frame.contains("ambient (s/S filter)") {
            reached_ambient_filter = Some(frame);
            break;
        }
    }
    let ambient_filter_frame =
        reached_ambient_filter.expect("pressing `s` must reach the ambient filter mode");
    println!("--- rendered picker (ambient filter) ---{ambient_filter_frame}\n--- end ---");
    // Test sessions are still shown, so the self-dev row is eligible; the filter
    // is the only thing that can drop it.
    assert_eq!(
        picker.visible_session_count(),
        ambient_ids.len(),
        "the ambient filter must keep exactly the ambient sessions and drop the self-dev control"
    );
}
