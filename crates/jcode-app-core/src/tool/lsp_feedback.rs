//! Glue between the file tools (read/write/edit/multiedit/apply_patch) and
//! `jcode-lsp`. Every entry point here is total: LSP failures, timeouts, or a
//! disabled/missing server never affect the tool call itself.
//!
//! See docs/superpowers/specs/2026-08-05-lsp-support-design.md
//! ("Diagnostics on read/write/edit").

use std::path::Path;

/// Ensure `jcode-lsp` has current config, then warm the server for `path` in
/// the background (fire-and-forget). Used by the read tool: never blocks,
/// never fails.
pub(crate) fn touch_background(path: &Path) {
    jcode_lsp::configure(crate::config::config().lsp.clone());
    let path = path.to_path_buf();
    tokio::spawn(async move {
        jcode_lsp::touch_background(&path).await;
    });
}

/// Blocking diagnostics fetch for a file that was just written. Returns the
/// formatted `<diagnostics>` block, or `None` when LSP is disabled, the file
/// is clean, or anything failed/timed out.
pub(crate) async fn diagnostics_after_write(path: &Path) -> Option<String> {
    jcode_lsp::configure(crate::config::config().lsp.clone());
    if !jcode_lsp::is_enabled() {
        return None;
    }
    jcode_lsp::diagnostics_block(path).await
}
