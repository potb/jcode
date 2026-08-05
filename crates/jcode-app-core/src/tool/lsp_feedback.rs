//! Glue between the file tools (read/write/edit/multiedit/apply_patch) and
//! `jcode-lsp`/`jcode-fmt`. Every entry point here is total: formatter/LSP
//! failures, timeouts, or a disabled/missing server never affect the tool
//! call itself.
//!
//! See docs/superpowers/specs/2026-08-05-lsp-support-design.md
//! ("Diagnostics on read/write/edit") and
//! docs/superpowers/specs/2026-08-05-auto-format-design.md ("Auto-Format").

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

/// Blocking auto-format for a file that was just written. Runs BEFORE
/// diagnostics so diagnostics see the formatted content. Returns a short
/// notice like "formatted with prettier", or `None` when formatting is
/// disabled, no formatter has evidence for this file, or every matching
/// formatter failed/timed out.
pub(crate) async fn format_after_write(path: &Path) -> Option<String> {
    jcode_fmt::configure(crate::config::config().formatter.clone());
    if !jcode_fmt::is_enabled() {
        return None;
    }
    jcode_fmt::format_file(path).await
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

