//! Generic multi-server LSP client layer for jcode.
//!
//! Public surface (see docs/superpowers/specs/2026-08-05-lsp-support-design.md):
//! - [`configure`] / [`is_enabled`] — process-global config.
//! - [`touch_background`] — read path: warm up server, `didOpen`, no wait.
//! - [`diagnostics_block`] — write path: `didChange` + bounded wait, formatted.
//! - [`shutdown_all`] — best-effort shutdown on process exit.
//! - [`LspHandle`] / [`handle_for`] — request API for the agent-facing `lsp` tool.
//!
//! Every public function is total: LSP failures never propagate to tool calls.

pub mod config_compat;

mod catalog;
mod client;
mod format;
mod position;
mod registry;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use lsp_types::Url;

pub use client::language_id;
pub use config_compat::{LspConfig, LspServerConfig};
pub use format::{format_all_severities, format_file_diagnostics, format_write_feedback};

use client::{COLD_CAP, LspClient, WARM_CAP, file_uri, fingerprint, read_text_for_lsp};
use position::{from_lsp_position, to_lsp_position};

/// Store the process-global `[lsp]` config. Call once at startup (and again
/// on config reload).
pub fn configure(cfg: LspConfig) {
    registry::set_config(cfg);
}

/// True when the config enables LSP AND at least one catalog server binary is
/// on PATH. PATH lookups are cached per process.
pub fn is_enabled() -> bool {
    registry::enabled()
}

/// Read path: warm up the server for this file and send `didOpen`/`didChange`.
/// Returns immediately; all work happens in a spawned task.
pub async fn touch_background(path: &Path) {
    if !registry::config().enabled {
        return;
    }
    let path = path.to_path_buf();
    tokio::spawn(async move {
        if let Err(err) = touch_inner(&path).await {
            jcode_logging::debug(&format!("lsp touch {}: {err:#}", path.display()));
        }
    });
}

async fn touch_inner(path: &Path) -> Result<()> {
    let Some((spec, _)) = registry::resolve(path) else {
        return Ok(());
    };
    let Some(bin) = spec.command.first() else {
        return Ok(());
    };
    if !registry::binary_on_path(bin) {
        return Ok(());
    }
    let Some(text) = read_text_for_lsp(path).await else {
        return Ok(());
    };
    let lease = registry::client_for(path).await?;
    lease.client.open_or_update(path, &text).await?;
    Ok(())
}

/// Write path: sync the file to the server, wait for diagnostics per the spec
/// wait strategy, and format the result. Returns `None` when the file is
/// clean, LSP is unavailable, or anything fails or times out.
pub async fn diagnostics_block(path: &Path) -> Option<String> {
    if !registry::config().enabled {
        return None;
    }
    match diagnostics_block_inner(path).await {
        Ok(out) => out,
        Err(err) => {
            jcode_logging::debug(&format!("lsp diagnostics {}: {err:#}", path.display()));
            None
        }
    }
}

async fn diagnostics_block_inner(path: &Path) -> Result<Option<String>> {
    let Some((spec, _)) = registry::resolve(path) else {
        return Ok(None);
    };
    let Some(bin) = spec.command.first() else {
        return Ok(None);
    };
    if !registry::binary_on_path(bin) {
        return Ok(None);
    }
    let Some(text) = read_text_for_lsp(path).await else {
        return Ok(None);
    };
    let lease = registry::client_for(path).await?;
    let client = &lease.client;
    let uri = file_uri(path)?;

    // Snapshot other files' error fingerprints before the change.
    let before = client.error_snapshot();

    let version = client.open_or_update(path, &text).await?;
    let cap = if lease.cold { COLD_CAP } else { WARM_CAP };
    let diags = client.wait_for_diagnostics(&uri, version, cap).await;

    // Cross-file: open documents that gained new errors.
    let mut cross: Vec<(String, Vec<lsp_types::Diagnostic>)> = Vec::new();
    for other_uri in client.diagnosed_uris() {
        if other_uri == uri {
            continue;
        }
        let now = client.diagnostics_for(&other_uri);
        let prev: &HashSet<_> = match before.get(&other_uri) {
            Some(set) => set,
            None => &HashSet::new(),
        };
        let new_errors: Vec<lsp_types::Diagnostic> = now
            .into_iter()
            .filter(|d| {
                d.severity.unwrap_or(lsp_types::DiagnosticSeverity::ERROR)
                    == lsp_types::DiagnosticSeverity::ERROR
                    && !prev.contains(&fingerprint(d))
            })
            .collect();
        if new_errors.is_empty() {
            continue;
        }
        let display = other_uri
            .to_file_path()
            .map(|p| format::display_path(&p, &client.workspace_root))
            .unwrap_or_else(|_| other_uri.to_string());
        cross.push((display, new_errors));
    }
    cross.sort_by(|a, b| a.0.cmp(&b.0));

    let display = format::display_path(path, &client.workspace_root);
    Ok(format::format_write_feedback(&display, &diags, &cross))
}

/// Best-effort shutdown of all live servers (`shutdown` + `exit`, kill after
/// 500 ms). Call on process exit.
pub async fn shutdown_all() {
    registry::shutdown_all().await;
}

// ----- lsp tool API -----

/// A location result with 1-based line/column.
#[derive(Debug, Clone)]
pub struct LocationInfo {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub line_text: Option<String>,
}

/// A symbol result with a 1-based line.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub path: PathBuf,
    pub line: u32,
}

/// Result of a rename: files modified on disk.
#[derive(Debug, Clone)]
pub struct RenameOutcome {
    pub changed_files: Vec<PathBuf>,
}

/// A resolved (client, uri) pair for one file, used by the `lsp` tool.
pub struct LspHandle {
    client: Arc<LspClient>,
    uri: Url,
    path: PathBuf,
}

/// Resolve a handle for a file. Errors name the missing binary when the
/// server is not on PATH.
pub async fn handle_for(path: &Path) -> Result<LspHandle> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let text = read_text_for_lsp(&path).await.ok_or_else(|| {
        anyhow!(
            "cannot read `{}` (missing, binary, or >1MB)",
            path.display()
        )
    })?;
    let lease = registry::client_for(&path).await?;
    lease.client.open_or_update(&path, &text).await?;
    let uri = file_uri(&path)?;
    Ok(LspHandle {
        client: lease.client,
        uri,
        path,
    })
}

/// Resolve a handle for a DIRECTORY (workspace-level actions such as
/// `workspace/symbol`). Walks up from `dir` looking for any catalog server's
/// root markers and picks the first PATH-available server whose marker
/// matches; falls back to any PATH-available catalog server rooted at `dir`.
/// Gets or spawns the client WITHOUT sending `didOpen` (there is no file).
pub async fn workspace_handle_for(dir: &Path) -> Result<LspHandle> {
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(dir)
    };
    let catalog = registry::catalog();
    let available: Vec<_> = catalog
        .iter()
        .filter(|s| {
            s.command
                .first()
                .is_some_and(|bin| registry::binary_on_path(bin))
        })
        .collect();
    if available.is_empty() {
        return Err(anyhow!("no language server available on PATH"));
    }

    // Walk up from `dir`: first (directory, server) pair whose root marker
    // matches wins. Catalog order breaks ties within one directory.
    let mut chosen: Option<(catalog::ServerSpec, PathBuf)> = None;
    let mut cur = Some(dir.as_path());
    'walk: while let Some(d) = cur {
        for spec in &available {
            if spec.root_markers.iter().any(|m| d.join(m).exists()) {
                chosen = Some(((*spec).clone(), d.to_path_buf()));
                break 'walk;
            }
        }
        cur = d.parent();
    }
    let (spec, root) = chosen.unwrap_or_else(|| (available[0].clone(), dir.clone()));

    let lease = registry::client_for_spec(&spec, &root).await?;
    let uri = file_uri(&root)?;
    Ok(LspHandle {
        client: lease.client,
        uri,
        path: root,
    })
}

impl LspHandle {
    fn text_document_position(
        &self,
        text: &str,
        line: u32,
        column: u32,
    ) -> lsp_types::TextDocumentPositionParams {
        lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: self.uri.clone(),
            },
            position: to_lsp_position(text, line, column, self.client.encoding()),
        }
    }

    async fn own_text(&self) -> String {
        read_text_for_lsp(&self.path).await.unwrap_or_default()
    }

    /// Go to definition. 1-based in and out.
    pub async fn definition(&self, line: u32, column: u32) -> Result<Vec<LocationInfo>> {
        let text = self.own_text().await;
        let resp = self
            .client
            .definition(lsp_types::GotoDefinitionParams {
                text_document_position_params: self.text_document_position(&text, line, column),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await?;
        let locations = match resp {
            None => Vec::new(),
            Some(lsp_types::GotoDefinitionResponse::Scalar(loc)) => vec![loc],
            Some(lsp_types::GotoDefinitionResponse::Array(locs)) => locs,
            Some(lsp_types::GotoDefinitionResponse::Link(links)) => links
                .into_iter()
                .map(|l| lsp_types::Location {
                    uri: l.target_uri,
                    range: l.target_selection_range,
                })
                .collect(),
        };
        Ok(self.location_infos(locations).await)
    }

    /// Find references. 1-based in and out.
    pub async fn references(&self, line: u32, column: u32) -> Result<Vec<LocationInfo>> {
        let text = self.own_text().await;
        let resp = self
            .client
            .references(lsp_types::ReferenceParams {
                text_document_position: self.text_document_position(&text, line, column),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: lsp_types::ReferenceContext {
                    include_declaration: true,
                },
            })
            .await?;
        Ok(self.location_infos(resp.unwrap_or_default()).await)
    }

    /// Hover contents as markdown/plaintext.
    pub async fn hover(&self, line: u32, column: u32) -> Result<Option<String>> {
        let text = self.own_text().await;
        let resp = self
            .client
            .hover(lsp_types::HoverParams {
                text_document_position_params: self.text_document_position(&text, line, column),
                work_done_progress_params: Default::default(),
            })
            .await?;
        Ok(resp.map(|h| hover_to_string(h.contents)))
    }

    /// Document symbols (flat list, nested symbols flattened).
    pub async fn document_symbols(&self) -> Result<Vec<SymbolInfo>> {
        let resp = self
            .client
            .document_symbol(lsp_types::DocumentSymbolParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: self.uri.clone(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await?;
        let mut out = Vec::new();
        match resp {
            None => {}
            Some(lsp_types::DocumentSymbolResponse::Flat(symbols)) => {
                for s in symbols {
                    out.push(SymbolInfo {
                        name: s.name,
                        kind: symbol_kind_name(s.kind).to_string(),
                        path: s
                            .location
                            .uri
                            .to_file_path()
                            .unwrap_or_else(|_| self.path.clone()),
                        line: s.location.range.start.line + 1,
                    });
                }
            }
            Some(lsp_types::DocumentSymbolResponse::Nested(symbols)) => {
                fn walk(
                    out: &mut Vec<SymbolInfo>,
                    path: &Path,
                    syms: Vec<lsp_types::DocumentSymbol>,
                ) {
                    for s in syms {
                        out.push(SymbolInfo {
                            name: s.name,
                            kind: symbol_kind_name(s.kind).to_string(),
                            path: path.to_path_buf(),
                            line: s.selection_range.start.line + 1,
                        });
                        if let Some(children) = s.children {
                            walk(out, path, children);
                        }
                    }
                }
                walk(&mut out, &self.path, symbols);
            }
        }
        Ok(out)
    }

    /// Workspace symbol search.
    pub async fn workspace_symbols(&self, query: &str) -> Result<Vec<SymbolInfo>> {
        let resp = self
            .client
            .workspace_symbol(lsp_types::WorkspaceSymbolParams {
                query: query.to_string(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await?;
        let mut out = Vec::new();
        match resp {
            None => {}
            Some(lsp_types::WorkspaceSymbolResponse::Flat(symbols)) => {
                for s in symbols {
                    out.push(SymbolInfo {
                        name: s.name,
                        kind: symbol_kind_name(s.kind).to_string(),
                        path: s
                            .location
                            .uri
                            .to_file_path()
                            .unwrap_or_else(|_| self.path.clone()),
                        line: s.location.range.start.line + 1,
                    });
                }
            }
            Some(lsp_types::WorkspaceSymbolResponse::Nested(symbols)) => {
                for s in symbols {
                    let (path, line) = match s.location {
                        lsp_types::OneOf::Left(loc) => (
                            loc.uri.to_file_path().unwrap_or_else(|_| self.path.clone()),
                            loc.range.start.line + 1,
                        ),
                        lsp_types::OneOf::Right(ws) => (
                            ws.uri.to_file_path().unwrap_or_else(|_| self.path.clone()),
                            1,
                        ),
                    };
                    out.push(SymbolInfo {
                        name: s.name,
                        kind: symbol_kind_name(s.kind).to_string(),
                        path,
                        line,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Current diagnostics for the file, all severities, formatted.
    pub async fn file_diagnostics(&self) -> Result<String> {
        let version = {
            // Re-sync so we report on the current content.
            let text = self.own_text().await;
            self.client.open_or_update(&self.path, &text).await?
        };
        let diags = self
            .client
            .wait_for_diagnostics(&self.uri, version, WARM_CAP)
            .await;
        let display = format::display_path(&self.path, &self.client.workspace_root);
        Ok(format::format_all_severities(&display, &diags))
    }

    /// Rename the symbol at (line, column). APPLIES the returned edits to disk
    /// (only files inside the workspace root) and lists the changed files.
    /// Files the server currently has open are re-synced via `didChange` so
    /// subsequent diagnostics reflect the on-disk content.
    pub async fn rename(&self, line: u32, column: u32, new_name: &str) -> Result<RenameOutcome> {
        let text = self.own_text().await;
        let edit = self
            .client
            .rename(lsp_types::RenameParams {
                text_document_position: self.text_document_position(&text, line, column),
                new_name: new_name.to_string(),
                work_done_progress_params: Default::default(),
            })
            .await?
            .ok_or_else(|| anyhow!("server returned no rename edit"))?;
        let changed = apply_workspace_edit(&edit, &self.client.workspace_root).await?;
        // Keep server buffers in sync with what we just wrote to disk.
        for path in &changed {
            if let Some(new_text) = read_text_for_lsp(path).await
                && let Err(err) = self.client.resync_if_open(path, &new_text).await
            {
                jcode_logging::debug(&format!(
                    "lsp rename: resync {} failed: {err:#}",
                    path.display()
                ));
            }
        }
        Ok(RenameOutcome {
            changed_files: changed,
        })
    }

    async fn location_infos(&self, locations: Vec<lsp_types::Location>) -> Vec<LocationInfo> {
        let mut out = Vec::new();
        let encoding = self.client.encoding();
        for loc in locations {
            let Ok(path) = loc.uri.to_file_path() else {
                continue;
            };
            let text = read_text_for_lsp(&path).await;
            let (line, column) = match &text {
                Some(t) => from_lsp_position(t, loc.range.start, encoding),
                None => (loc.range.start.line + 1, loc.range.start.character + 1),
            };
            let line_text = text.as_ref().and_then(|t| {
                t.lines()
                    .nth(loc.range.start.line as usize)
                    .map(|l| l.trim_end().to_string())
            });
            out.push(LocationInfo {
                path,
                line,
                column,
                line_text,
            });
        }
        out
    }
}

fn hover_to_string(contents: lsp_types::HoverContents) -> String {
    fn marked(ms: lsp_types::MarkedString) -> String {
        match ms {
            lsp_types::MarkedString::String(s) => s,
            lsp_types::MarkedString::LanguageString(ls) => {
                format!("```{}\n{}\n```", ls.language, ls.value)
            }
        }
    }
    match contents {
        lsp_types::HoverContents::Scalar(ms) => marked(ms),
        lsp_types::HoverContents::Array(items) => items
            .into_iter()
            .map(marked)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(m) => m.value,
    }
}

fn symbol_kind_name(kind: lsp_types::SymbolKind) -> &'static str {
    use lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "file",
        K::MODULE => "module",
        K::NAMESPACE => "namespace",
        K::PACKAGE => "package",
        K::CLASS => "class",
        K::METHOD => "method",
        K::PROPERTY => "property",
        K::FIELD => "field",
        K::CONSTRUCTOR => "constructor",
        K::ENUM => "enum",
        K::INTERFACE => "interface",
        K::FUNCTION => "function",
        K::VARIABLE => "variable",
        K::CONSTANT => "constant",
        K::STRING => "string",
        K::NUMBER => "number",
        K::BOOLEAN => "boolean",
        K::ARRAY => "array",
        K::OBJECT => "object",
        K::KEY => "key",
        K::NULL => "null",
        K::ENUM_MEMBER => "enum member",
        K::STRUCT => "struct",
        K::EVENT => "event",
        K::OPERATOR => "operator",
        K::TYPE_PARAMETER => "type parameter",
        _ => "symbol",
    }
}

/// Apply a `WorkspaceEdit`'s text edits to disk, confined to files inside
/// `workspace_root`. Returns the changed file paths.
///
/// Per the LSP spec, `documentChanges` is preferred over `changes` when both
/// are present (applying both would double-apply the same edits). Each file
/// is all-or-nothing: the full new content is built in memory first and
/// written once, so a bad edit range cannot leave a half-renamed file.
async fn apply_workspace_edit(
    edit: &lsp_types::WorkspaceEdit,
    workspace_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut per_file: Vec<(PathBuf, Vec<lsp_types::TextEdit>)> = Vec::new();
    if let Some(doc_changes) = &edit.document_changes {
        match doc_changes {
            lsp_types::DocumentChanges::Edits(edits) => {
                for e in edits {
                    if let Ok(path) = e.text_document.uri.to_file_path() {
                        let text_edits: Vec<lsp_types::TextEdit> = e
                            .edits
                            .iter()
                            .map(|oe| match oe {
                                lsp_types::OneOf::Left(te) => te.clone(),
                                lsp_types::OneOf::Right(annotated) => annotated.text_edit.clone(),
                            })
                            .collect();
                        per_file.push((path, text_edits));
                    }
                }
            }
            lsp_types::DocumentChanges::Operations(ops) => {
                for op in ops {
                    if let lsp_types::DocumentChangeOperation::Edit(e) = op
                        && let Ok(path) = e.text_document.uri.to_file_path()
                    {
                        let text_edits: Vec<lsp_types::TextEdit> = e
                            .edits
                            .iter()
                            .map(|oe| match oe {
                                lsp_types::OneOf::Left(te) => te.clone(),
                                lsp_types::OneOf::Right(annotated) => annotated.text_edit.clone(),
                            })
                            .collect();
                        per_file.push((path, text_edits));
                    }
                }
            }
        }
    } else if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            if let Ok(path) = uri.to_file_path() {
                per_file.push((path, edits.clone()));
            }
        }
    }

    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    // Phase 1: build every file's full new content in memory. Any failure
    // aborts BEFORE anything is written (all-or-nothing per rename).
    let mut pending: Vec<(PathBuf, String)> = Vec::new();
    for (path, mut edits) in per_file {
        if edits.is_empty() {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !canonical.starts_with(&root) {
            jcode_logging::warn(&format!(
                "lsp rename: skipping edit outside workspace root: {}",
                path.display()
            ));
            continue;
        }
        let text = tokio::fs::read_to_string(&canonical)
            .await
            .with_context(|| format!("read {}", canonical.display()))?;
        // Apply bottom-up so earlier offsets stay valid.
        edits.sort_by(|a, b| {
            (b.range.start.line, b.range.start.character)
                .cmp(&(a.range.start.line, a.range.start.character))
        });
        let mut new_text = text;
        for e in &edits {
            new_text = apply_text_edit(&new_text, e)
                .with_context(|| format!("apply rename edit in {}", canonical.display()))?;
        }
        pending.push((canonical, new_text));
    }
    // Phase 2: write each file once with its complete new content.
    let mut changed = Vec::new();
    for (canonical, new_text) in pending {
        tokio::fs::write(&canonical, new_text)
            .await
            .with_context(|| format!("write {}", canonical.display()))?;
        changed.push(canonical);
    }
    changed.sort();
    changed.dedup();
    Ok(changed)
}

/// Apply one LSP `TextEdit` to a document string. Offsets are interpreted as
/// UTF-16 code units per LSP default.
fn apply_text_edit(text: &str, edit: &lsp_types::TextEdit) -> Result<String> {
    let start = byte_offset(text, edit.range.start)
        .ok_or_else(|| anyhow!("rename edit start out of range"))?;
    let end =
        byte_offset(text, edit.range.end).ok_or_else(|| anyhow!("rename edit end out of range"))?;
    if start > end || end > text.len() {
        return Err(anyhow!("rename edit range invalid"));
    }
    let mut out = String::with_capacity(text.len() + edit.new_text.len());
    out.push_str(&text[..start]);
    out.push_str(&edit.new_text);
    out.push_str(&text[end..]);
    Ok(out)
}

/// Byte offset of an LSP position (UTF-16 character offsets) in `text`.
fn byte_offset(text: &str, pos: lsp_types::Position) -> Option<usize> {
    let mut line = 0u32;
    let mut iter = text.char_indices().peekable();
    // Advance to the start of the target line.
    while line < pos.line {
        let (_, ch) = iter.next()?;
        if ch == '\n' {
            line += 1;
        }
    }
    let line_start = iter.peek().map(|(i, _)| *i).unwrap_or(text.len());
    let mut offset = line_start;
    let mut units = 0u32;
    for (i, ch) in text[line_start..].char_indices() {
        if units >= pos.character || ch == '\n' {
            return Some(line_start + i);
        }
        units += ch.len_utf16() as u32;
        offset = line_start + i + ch.len_utf8();
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_edit_apply_ascii() {
        let text = "let old_name = 1;\nuse_it(old_name);\n";
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 4),
                lsp_types::Position::new(0, 12),
            ),
            new_text: "new_name".into(),
        };
        let out = apply_text_edit(text, &edit).unwrap();
        assert_eq!(out, "let new_name = 1;\nuse_it(old_name);\n");
    }

    #[test]
    fn text_edit_apply_multibyte_utf16() {
        // "𐍈" is 2 UTF-16 units, 4 UTF-8 bytes.
        let text = "𐍈abc\n";
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 2),
                lsp_types::Position::new(0, 3),
            ),
            new_text: "X".into(),
        };
        let out = apply_text_edit(text, &edit).unwrap();
        assert_eq!(out, "𐍈Xbc\n");
    }

    #[test]
    fn text_edit_second_line() {
        let text = "line one\nline two\n";
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(1, 5),
                lsp_types::Position::new(1, 8),
            ),
            new_text: "2".into(),
        };
        let out = apply_text_edit(text, &edit).unwrap();
        assert_eq!(out, "line one\nline 2\n");
    }

    #[test]
    fn byte_offset_out_of_range_line() {
        assert!(byte_offset("ab\n", lsp_types::Position::new(5, 0)).is_none());
    }
}
