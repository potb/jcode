//! Cost probe for issue #150 direction (3): see `docs/issue-150-direction-3-cost-probe.md`.

#![cfg(feature = "renderer")]

use jcode_tui_mermaid::{RenderResult, clear_cache, debug_stats, render_mermaid_untracked};

const PANE_CELLS: u16 = 80;

const ZOOM_START: u32 = 100;
const ZOOM_END: u32 = 250;
const ZOOM_STEP: u32 = 10;

const TALLER_THAN_4_3_FLOWCHART: &str = "flowchart TD\n    A[Ingest] --> B[Validate]\n    B --> C[Normalize]\n    C --> D[Enrich]\n    D --> E[Store]\n    E --> F[Index]\n    F --> G[Serve]\n    G --> H[Archive]";

fn backend() -> &'static str {
    if cfg!(mmdr_size_api_available) {
        "mmdr-size-api"
    } else {
        "svg-retarget-fallback"
    }
}

fn rasterization_count() -> u64 {
    debug_stats().cache_misses
}

fn render_png_dimensions(content: &str, width_cells: u16) -> (u32, u32) {
    match render_mermaid_untracked(content, Some(width_cells)) {
        RenderResult::Image { width, height, .. } => (width, height),
        RenderResult::Error(err) => panic!("render failed at {width_cells} cells: {err}"),
    }
}

fn zoom_levels() -> impl Iterator<Item = u32> {
    (ZOOM_START..=ZOOM_END).step_by(ZOOM_STEP as usize)
}

fn zoom_step_count() -> u64 {
    zoom_levels().count() as u64
}

fn width_ignoring_zoom(_zoom_percent: u32) -> u16 {
    PANE_CELLS
}

fn width_scaled_by_zoom(zoom_percent: u32) -> u16 {
    let scaled = (PANE_CELLS as u32).saturating_mul(zoom_percent) / 100;
    scaled.min(u16::MAX as u32) as u16
}

fn rasterizations_across_zoom_sweep(
    label: &str,
    content: &str,
    width_for_zoom: impl Fn(u32) -> u16,
) -> u64 {
    clear_cache().expect("clear_cache");

    let before = rasterization_count();
    let mut per_step = Vec::new();
    for zoom_percent in zoom_levels() {
        let width_cells = width_for_zoom(zoom_percent);
        let (png_w, png_h) = render_png_dimensions(content, width_cells);
        per_step.push((zoom_percent, width_cells, png_w, png_h));
    }
    let rasterizations = rasterization_count() - before;

    println!(
        "\n[{}] backend={} steps={}",
        label,
        backend(),
        per_step.len()
    );
    for (zoom_percent, width_cells, png_w, png_h) in &per_step {
        println!(
            "    zoom {zoom_percent:>3}% -> request {width_cells:>4} cells -> png {png_w}x{png_h}"
        );
    }
    println!("    rasterizations across sweep = {rasterizations}");
    rasterizations
}

#[test]
#[ignore = "explicit measurement probe for issue #150 direction (3); rasterizes real SVGs"]
fn zoom_sweep_rasterization_cost() {
    println!(
        "\n=== issue #150 direction (3) cost probe | backend={} | pane={} cells | zoom {}..={} step {} ===",
        backend(),
        PANE_CELLS,
        ZOOM_START,
        ZOOM_END,
        ZOOM_STEP
    );

    let today = rasterizations_across_zoom_sweep(
        "CLAIM A: today (width ignores zoom)",
        TALLER_THAN_4_3_FLOWCHART,
        width_ignoring_zoom,
    );

    let with_direction_3 = rasterizations_across_zoom_sweep(
        "CLAIM B: direction (3) (width = pane * zoom)",
        TALLER_THAN_4_3_FLOWCHART,
        width_scaled_by_zoom,
    );

    let naive_worst_case = zoom_step_count();
    println!(
        "\n=== RESULT: today={} rasterization(s), direction(3)={} across {} zoom steps ===",
        today, with_direction_3, naive_worst_case
    );
    println!(
        "    per-keystroke rasterization would be {naive_worst_case}; direction (3) costs \
         {with_direction_3}, i.e. {:.1}x today's cost and {:.0}% of the naive worst case.",
        with_direction_3 as f64 / today.max(1) as f64,
        100.0 * with_direction_3 as f64 / naive_worst_case as f64
    );

    assert_eq!(
        today, 1,
        "zoom is not an input to the render width today, so a whole sweep should rasterize once"
    );

    assert!(
        with_direction_3 > today,
        "direction (3) must really re-rasterize rather than reuse one PNG via the 85% width \
         tolerance (got {with_direction_3}, today {today})"
    );

    assert!(
        with_direction_3 < naive_worst_case,
        "direction (3) must not rasterize once per zoom keystroke \
         (got {with_direction_3} of {naive_worst_case})"
    );
}
