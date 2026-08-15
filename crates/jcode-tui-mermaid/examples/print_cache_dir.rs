//! Prints the PNG cache directory this process resolved to.
//!
//! Run as a normal (non-test) binary, it verifies the inverse of the
//! test-harness guard in `mermaid_cache_render.rs`: a shipped binary must
//! resolve the REAL user cache, not a sandbox. A unit test can only check
//! path strings, so this is the check that a misfiring guard would fail.
//!
//! ```text
//! cargo run -p jcode-tui-mermaid --example print_cache_dir
//! cache_dir="/home/<user>/.cache/jcode/mermaid"
//! ```
fn main() {
    let stats = jcode_tui_mermaid::debug_stats_json().expect("mermaid debug stats");
    println!("cache_dir={}", stats["cache_dir"]);
}
