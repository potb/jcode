// The mermaid PNG cache is keyed by diagram content and shared on disk across
// runs and worktrees, so a fixed diagram can be answered by a stale entry
// rasterized at an unrelated width. A unique label per run forces a real
// rasterization.
fn unique_flowchart_page() -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "```mermaid\nflowchart TD\n    A[Ingest {nonce}] --> B[Validate]\n    B --> C[Normalize]\n    C --> D[Enrich]\n    D --> E[Store]\n    E --> F[Index]\n    F --> G[Serve]\n    G --> H[Archive]\n```"
    )
}

fn side_panel_app_with_diagram() -> App {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: String::new(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: unique_flowchart_page(),
            updated_at_ms: 1,
        }],
    };
    app
}

// The side panel renders mermaid through the deferred worker, so the first
// draw only reserves a pending placeholder. Redraw until the background
// rasterization lands, exactly as the real UI does on the next frame.
fn draw_until_diagram_rasterized(
    app: &App,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    label: &str,
) -> u64 {
    for _ in 0..200 {
        let width = crate::tui::markdown::with_mermaid_rendering_override(Some(true), || {
            terminal
                .draw(|f| crate::tui::ui::draw(f, app))
                .expect("draw failed");
            widest_drawn_png_width()
        });
        if let Some(width) = width {
            return width;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{label}: the side panel never drew a rasterized diagram")
}

// Widest PNG behind a diagram the side panel actually drew, read from the
// live debug snapshot the draw path writes, i.e. the pixels the terminal
// really received.
fn widest_drawn_png_width() -> Option<u64> {
    let debug = crate::tui::side_panel_debug_json()?;
    debug["live"]["visible_mermaids"]
        .as_array()?
        .iter()
        .filter_map(|m| m["rendered_png_width_px"].as_u64())
        .max()
}

#[test]
fn pressing_plus_on_the_side_panel_rasterizes_a_sharper_diagram() {
    let _lock = scroll_render_test_lock();
    struct ResetVideoExportMode;
    impl Drop for ResetVideoExportMode {
        fn drop(&mut self) {
            crate::tui::mermaid::set_video_export_mode(false);
        }
    }
    // Without this the diagram degrades to a text placeholder on a headless
    // runner and the test would pass while asserting nothing.
    crate::tui::mermaid::set_video_export_mode(true);
    let _reset = ResetVideoExportMode;

    let mut app = side_panel_app_with_diagram();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    assert!(app.handle_diagram_ctrl_key(KeyCode::Char('l'), false));
    assert!(app.diff_pane_focus, "side panel must be focused to zoom");

    crate::tui::clear_side_panel_render_caches();
    let unzoomed_png_width = draw_until_diagram_rasterized(&app, &mut terminal, "zoom 100%");

    // Real user input: ten `+` presses, 10% each, 100% -> 200%.
    for _ in 0..10 {
        app.handle_key(KeyCode::Char('+'), KeyModifiers::empty())
            .unwrap();
    }
    assert_eq!(
        app.side_panel_image_zoom_percent, 200,
        "ten `+` presses must reach 200% zoom"
    );

    let zoomed_png_width = draw_until_diagram_rasterized(&app, &mut terminal, "zoom 200%");

    println!(
        "keystroke-driven: zoom 100% -> widest png {unzoomed_png_width}px, \
         after ten `+` presses (200%) -> {zoomed_png_width}px"
    );

    assert!(
        zoomed_png_width > unzoomed_png_width,
        "zooming in with `+` must rasterize more pixels for the terminal \
         (100% gave {unzoomed_png_width}px, 200% gave {zoomed_png_width}px)"
    );
}

// Negative control for the >150% gate, at the same end-user boundary: a
// modest zoom must NOT change what the terminal receives, so ordinary
// zooming keeps today's cost and cache behaviour exactly.
#[test]
fn pressing_plus_below_the_gate_does_not_change_the_rasterized_diagram() {
    let _lock = scroll_render_test_lock();
    struct ResetVideoExportMode;
    impl Drop for ResetVideoExportMode {
        fn drop(&mut self) {
            crate::tui::mermaid::set_video_export_mode(false);
        }
    }
    crate::tui::mermaid::set_video_export_mode(true);
    let _reset = ResetVideoExportMode;

    let mut app = side_panel_app_with_diagram();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    assert!(app.handle_diagram_ctrl_key(KeyCode::Char('l'), false));

    crate::tui::clear_side_panel_render_caches();
    let unzoomed_png_width = draw_until_diagram_rasterized(&app, &mut terminal, "zoom 100%");

    // Five `+` presses: 100% -> 150%, the boundary itself, still not above it.
    for _ in 0..5 {
        app.handle_key(KeyCode::Char('+'), KeyModifiers::empty())
            .unwrap();
    }
    assert_eq!(
        app.side_panel_image_zoom_percent, 150,
        "five `+` presses must land exactly on the gate boundary"
    );

    let gated_png_width = draw_until_diagram_rasterized(&app, &mut terminal, "zoom 150%");

    println!(
        "keystroke-driven negative control: zoom 100% -> widest png \
         {unzoomed_png_width}px, at the 150% boundary -> {gated_png_width}px"
    );

    assert_eq!(
        gated_png_width, unzoomed_png_width,
        "zoom at or below the gate must not re-rasterize (100% gave \
         {unzoomed_png_width}px, 150% gave {gated_png_width}px)"
    );

    // Equality alone would also hold if the gate were ignored and BOTH draws
    // were scaled up, so pin the unzoomed draw to the width the pane implies.
    // 47 content cells at an 8px font is 376px, which the renderer clamps to
    // its 400px floor; anything wider means a scale leaked into the 100% path.
    assert!(
        unzoomed_png_width <= 512,
        "the unzoomed draw must not be scaled at all, got {unzoomed_png_width}px"
    );
}
