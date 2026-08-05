//! Auto-format-on-write layer for jcode.
//!
//! Public surface (see docs/superpowers/specs/2026-08-05-auto-format-design.md):
//! - [`configure`] / [`is_enabled`] — process-global config, like `jcode-lsp`.
//! - [`format_file`] — write path: format in place with every enabled
//!   formatter matching the file's extension, catalog order. Total: never
//!   panics, never errors out.

pub mod config_compat;

mod catalog;
mod evidence;
mod exec;
mod registry;

use std::path::Path;

pub use config_compat::{FormatterConfig, FormatterServerConfig};

/// Store the process-global `[formatter]` config. Call once at startup (and
/// again on config reload).
pub fn configure(cfg: FormatterConfig) {
    registry::set_config(cfg);
}

/// True when the config's master switch is enabled. Does not check for any
/// specific evidence/binary (that happens per-file in [`format_file`]),
/// mirroring `jcode-lsp::is_enabled`'s cheap short-circuit role at call
/// sites.
pub fn is_enabled() -> bool {
    registry::config().enabled
}

/// Format the file in place with every enabled formatter matching its
/// extension (catalog order). Returns a short notice like
/// "formatted with prettier" when at least one formatter ran cleanly,
/// `None` otherwise (disabled, no evidence, skip conditions, or every
/// matching formatter failed/timed out). Total: never panics, never errors
/// out.
pub async fn format_file(path: &Path) -> Option<String> {
    if !is_enabled() {
        return None;
    }
    if exec::should_skip(path).await {
        return None;
    }
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let formatters = registry::resolve_for_path(&abs_path);
    if formatters.is_empty() {
        return None;
    }
    // One shared deadline for the whole file: several formatters may match
    // (e.g. prettier + biome), and the write tools await this inline. The
    // per-formatter cap inside `run_one` still applies; this bounds the sum.
    let mut ran: Vec<&str> = Vec::new();
    let budget = tokio::time::Instant::now() + exec::EXEC_TIMEOUT;
    for formatter in &formatters {
        if tokio::time::Instant::now() >= budget {
            jcode_logging::debug(&format!(
                "fmt: file budget exhausted before `{}` on {}",
                formatter.id,
                abs_path.display()
            ));
            break;
        }
        match tokio::time::timeout_at(budget, exec::run_one(formatter, &abs_path)).await {
            Ok(true) => ran.push(&formatter.id),
            Ok(false) => {}
            Err(_) => break,
        }
    }
    if ran.is_empty() {
        None
    } else {
        Some(format!("formatted with {}", ran.join(", ")))
    }
}
