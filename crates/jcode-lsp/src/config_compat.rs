//! Re-exports of the `[lsp]` config types from `jcode-config-types`.
//!
//! Kept as a module so earlier in-flight consumers of
//! `jcode_lsp::config_compat::{LspConfig, LspServerConfig}` keep working.

pub use jcode_config_types::{LspConfig, LspServerConfig};
