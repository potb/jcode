# Issue #150 direction (3): rasterization cost probe

Companion document for
`crates/jcode-tui-mermaid/tests/zoom_sweep_rasterization_cost.rs`.

## The question

Direction (3) of [issue #150](https://github.com/potb/jcode/issues/150)
proposes re-rendering a mermaid diagram at `natural * zoom` when the planned
zoom is high, instead of upscaling an under-resolved PNG and getting
interpolated mush.

The open question was never whether that fixes the blur. It is what it costs.
The side panel clears both of its caches on *every* zoom keypress
(`adjust_side_panel_image_zoom` -> `clear_side_panel_render_caches`), so once
the width handed to `render_mermaid_sized` depends on the zoom level, every
keystroke can reach a fresh SVG rasterization. That would be a worse regression
than the blur it fixes.

## What the probe measures

Both claims run through the crate's real public API — `render_mermaid_untracked`
plus `debug_stats`, the same entry point the TUI's `render_mermaid_sized`
wraps — and count `cache_misses` deltas as the rasterization signal. Calling
the render path once per zoom step is exactly what the side panel does after
wiping its caches.

**Claim A, today's behaviour.** Zoom is not an input to the render width, so
`calculate_render_size(nodes, edges, terminal_width)` returns the same
`target_width` at every zoom level and the width-keyed PNG lookup always hits.
A whole 100% -> 250% sweep should therefore rasterize exactly **once**.

This is the claim worth pinning down, because it corrects an earlier reading of
mine: what keeps zooming cheap today is this PNG width key, *not* the side
panel's markdown cache. That cache is wiped on every keypress regardless, so it
offers no protection to lose.

**Claim B, direction (3)'s cost.** If the width becomes `pane * zoom`, the same
sweep must rasterize more than once — otherwise it is not actually delivering
more pixels — but far less than once per step, because
`CACHE_WIDTH_MATCH_PERCENT = 85` lets a single PNG serve nearby zoom levels.

## Interpreting the result

The measured number decides a design question that was previously guessed:

- **Well below one per step.** The 85% width tolerance already collapses
  neighbouring zoom levels onto one PNG, so explicit zoom bucketing is
  redundant machinery and should not be added.
- **At or near one per step.** Direction (3) needs explicit quantization
  before it can ship, or it is not worth shipping at all.

## Failure modes each assertion catches

- `today == 1` failing means zooming already costs rasterizations, so the
  premise above is wrong and the correction needs revisiting.
- `with_direction_3 > today` failing means the fix silently reused one PNG via
  the width tolerance and changed nothing. This is the false "no change" trap
  recorded three times on issue #150; it is why the probe asserts a re-render
  happened rather than trusting that a bigger width was requested.
- `with_direction_3 < naive_worst_case` failing means it degenerated into one
  rasterization per keystroke.

## Running it

`#[ignore]`-d because it rasterizes real SVGs.

```
cargo test -p jcode-tui-mermaid --test zoom_sweep_rasterization_cost -- --ignored --nocapture
JCODE_MMDR_SIZE_API_DISABLE=1 cargo test -p jcode-tui-mermaid --test zoom_sweep_rasterization_cost -- --ignored --nocapture
```

The report prints `backend=` so size-API and legacy runs are distinguishable.
`JCODE_MMDR_SIZE_API_DISABLE` is a compile-time toggle consumed by `build.rs`,
so the second command triggers a rebuild.
