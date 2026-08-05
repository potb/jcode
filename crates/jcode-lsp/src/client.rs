//! `LspClient`: wraps one spawned language server process and its async-lsp
//! client main loop.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::{LanguageServer, ServerSocket};
use lsp_types::notification::PublishDiagnostics;
use lsp_types::{
    ClientCapabilities, Diagnostic, DiagnosticClientCapabilities, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, DocumentDiagnosticParams, DocumentDiagnosticReport,
    DocumentDiagnosticReportResult, GeneralClientCapabilities, InitializeParams,
    InitializedParams, PositionEncodingKind, PublishDiagnosticsClientCapabilities,
    TextDocumentClientCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, Url, VersionedTextDocumentIdentifier, WorkspaceFolder,
};
use tokio::sync::{Mutex, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

use crate::catalog::ServerSpec;
use crate::position::PositionEncoding;

/// Skip files larger than this (bytes).
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// Debounce window after a version-matched diagnostics push.
pub const DEBOUNCE: Duration = Duration::from_millis(150);
/// Wait cap when the server was already running.
pub const WARM_CAP: Duration = Duration::from_millis(1500);
/// Wait cap when this touch spawned the server.
pub const COLD_CAP: Duration = Duration::from_secs(5);
/// Per-request cap on pull diagnostics.
pub const PULL_CAP: Duration = Duration::from_secs(3);

/// A diagnostic identity for "gained new errors" comparisons.
pub type Fingerprint = (u32, u32, String);

#[derive(Debug, Clone, Default)]
struct StoredDiags {
    /// Version reported by the server in `publishDiagnostics`, when present.
    version: Option<i32>,
    diags: Vec<Diagnostic>,
}

struct DiagStore {
    map: std::sync::Mutex<HashMap<Url, StoredDiags>>,
    tx: watch::Sender<u64>,
}

impl DiagStore {
    fn new() -> Self {
        let (tx, _) = watch::channel(0u64);
        Self {
            map: std::sync::Mutex::new(HashMap::new()),
            tx,
        }
    }

    fn publish(&self, uri: Url, version: Option<i32>, diags: Vec<Diagnostic>) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(uri, StoredDiags { version, diags });
        }
        self.tx.send_modify(|g| *g = g.wrapping_add(1));
    }

    fn get(&self, uri: &Url) -> StoredDiags {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.get(uri).cloned())
            .unwrap_or_default()
    }
}

struct ClientState {
    store: Arc<DiagStore>,
}

struct Stop;

/// One language server process shared by every session touching the same
/// `(server_id, workspace_root)`.
pub struct LspClient {
    pub spec: ServerSpec,
    pub workspace_root: PathBuf,
    socket: ServerSocket,
    store: Arc<DiagStore>,
    /// uri -> current document version we sent.
    docs: Mutex<HashMap<Url, i32>>,
    child: Mutex<Option<tokio::process::Child>>,
    alive: Arc<AtomicBool>,
    supports_pull: bool,
}

impl LspClient {
    /// Spawn the server process, run the initialize handshake, and start the
    /// client main loop task.
    pub async fn spawn(spec: &ServerSpec, workspace_root: &Path) -> Result<Self> {
        let (bin, args) = spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("server `{}` has an empty command", spec.id))?;
        let mut child = tokio::process::Command::new(bin)
            .args(args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn language server `{bin}`"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout pipe for `{bin}`"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin pipe for `{bin}`"))?;

        let store = Arc::new(DiagStore::new());
        let router_store = store.clone();
        let (mainloop, mut socket) = async_lsp::MainLoop::new_client(move |_server| {
            let mut router = Router::new(ClientState {
                store: router_store,
            });
            router
                .notification::<PublishDiagnostics>(|this, params| {
                    this.store
                        .publish(params.uri, params.version, params.diagnostics);
                    ControlFlow::Continue(())
                })
                .request::<lsp_types::request::WorkspaceConfiguration, _>(|_, params| {
                    let n = params.items.len();
                    async move { Ok(vec![serde_json::Value::Null; n]) }
                })
                .request::<lsp_types::request::RegisterCapability, _>(|_, _| async move { Ok(()) })
                .request::<lsp_types::request::UnregisterCapability, _>(
                    |_, _| async move { Ok(()) },
                )
                .request::<lsp_types::request::WorkDoneProgressCreate, _>(
                    |_, _| async move { Ok(()) },
                )
                .unhandled_notification(|_, _| ControlFlow::Continue(()))
                .event(|_, _: Stop| ControlFlow::Break(Ok(())));
            ServiceBuilder::new()
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(router)
        });

        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();
        let server_id = spec.id.clone();
        tokio::spawn(async move {
            let result = mainloop
                .run_buffered(stdout.compat(), stdin.compat_write())
                .await;
            alive_task.store(false, Ordering::SeqCst);
            if let Err(err) = result {
                jcode_logging::debug(&format!("lsp: {server_id} main loop ended: {err}"));
            }
        });

        let root_uri = Url::from_file_path(workspace_root)
            .map_err(|_| anyhow!("workspace root is not an absolute path"))?;
        let init = socket
            .initialize(InitializeParams {
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri.clone(),
                    name: workspace_root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "root".to_string()),
                }]),
                #[allow(deprecated)]
                root_uri: Some(root_uri),
                initialization_options: spec.initialization_options.clone(),
                capabilities: ClientCapabilities {
                    general: Some(GeneralClientCapabilities {
                        // UTF-16 only (the LSP default). We apply WorkspaceEdit
                        // ranges as UTF-16 byte offsets, so never offer UTF-8:
                        // a server picking it would corrupt edits on non-ASCII
                        // lines.
                        position_encodings: Some(vec![PositionEncodingKind::UTF16]),
                        ..Default::default()
                    }),
                    text_document: Some(TextDocumentClientCapabilities {
                        publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                            version_support: Some(true),
                            ..Default::default()
                        }),
                        diagnostic: Some(DiagnosticClientCapabilities::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .with_context(|| format!("initialize failed for `{}`", spec.id))?;
        socket.initialized(InitializedParams {})?;

        let supports_pull = init.capabilities.diagnostic_provider.is_some();

        Ok(Self {
            spec: spec.clone(),
            workspace_root: workspace_root.to_path_buf(),
            socket,
            store,
            docs: Mutex::new(HashMap::new()),
            child: Mutex::new(Some(child)),
            alive,
            supports_pull,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Position encoding in effect. Always UTF-16: we only ever advertise
    /// UTF-16 in `positionEncodings` (see the initialize capabilities).
    pub fn encoding(&self) -> PositionEncoding {
        PositionEncoding::Utf16
    }

    fn sock(&self) -> ServerSocket {
        self.socket.clone()
    }

    /// Send `didOpen` (first touch) or a full-sync `didChange`. Returns the
    /// document version now in flight.
    pub async fn open_or_update(&self, path: &Path, content: &str) -> Result<i32> {
        let uri = file_uri(path)?;
        let mut docs = self.docs.lock().await;
        let mut sock = self.sock();
        match docs.get(&uri).copied() {
            None => {
                sock.did_open(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri.clone(),
                        language_id: language_id(path).to_string(),
                        version: 1,
                        text: content.to_string(),
                    },
                })?;
                docs.insert(uri, 1);
                Ok(1)
            }
            Some(v) => {
                let next = v.wrapping_add(1);
                sock.did_change(DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: uri.clone(),
                        version: next,
                    },
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: content.to_string(),
                    }],
                })?;
                docs.insert(uri, next);
                Ok(next)
            }
        }
    }

    /// Current push-store diagnostics for a file.
    pub fn diagnostics_for(&self, uri: &Url) -> Vec<Diagnostic> {
        self.store.get(uri).diags
    }

    /// If `path` is currently tracked open, resend its content via a full-sync
    /// `didChange` (version bump). Used after a rename writes files to disk so
    /// server buffers do not go stale. Returns whether the file was open.
    pub async fn resync_if_open(&self, path: &Path, content: &str) -> Result<bool> {
        let uri = file_uri(path)?;
        let open = { self.docs.lock().await.contains_key(&uri) };
        if !open {
            return Ok(false);
        }
        self.open_or_update(path, content).await?;
        Ok(true)
    }

    /// Snapshot error fingerprints for all files that currently have stored
    /// diagnostics (used to detect files gaining new errors after a write).
    pub fn error_snapshot(&self) -> HashMap<Url, HashSet<Fingerprint>> {
        let Ok(map) = self.store.map.lock() else {
            return HashMap::new();
        };
        map.iter()
            .map(|(uri, stored)| {
                let fps = stored
                    .diags
                    .iter()
                    .filter(|d| {
                        d.severity.unwrap_or(lsp_types::DiagnosticSeverity::ERROR)
                            == lsp_types::DiagnosticSeverity::ERROR
                    })
                    .map(fingerprint)
                    .collect();
                (uri.clone(), fps)
            })
            .collect()
    }

    /// All uris with stored diagnostics.
    pub fn diagnosed_uris(&self) -> Vec<Url> {
        self.store
            .map
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Wait for diagnostics per the spec: version-matched push + 150 ms
    /// debounce, capped at `cap`; a parallel pull request (when advertised)
    /// unblocks on first NON-EMPTY result (an empty pull response proves
    /// nothing and must not cut the push wait short). Returns the merged
    /// diagnostics for `uri`.
    pub async fn wait_for_diagnostics(
        &self,
        uri: &Url,
        version: i32,
        cap: Duration,
    ) -> Vec<Diagnostic> {
        let push = self.wait_for_push(uri, version, cap);
        let mut pulled: Option<Vec<Diagnostic>> = None;
        if self.supports_pull {
            tokio::pin!(push);
            let pull = self.pull_diagnostics(uri);
            tokio::pin!(pull);
            let mut pull_pending = true;
            loop {
                tokio::select! {
                    _ = &mut push => break,
                    res = &mut pull, if pull_pending => {
                        pull_pending = false;
                        match res {
                            // Non-empty pull result: real signal, unblock.
                            Ok(items) if !items.is_empty() => {
                                pulled = Some(items);
                                break;
                            }
                            // Empty or failed pull: keep waiting on push.
                            _ => {}
                        }
                    }
                }
            }
        } else {
            push.await;
        }
        let mut merged = self.diagnostics_for(uri);
        if let Some(items) = pulled {
            let seen: HashSet<Fingerprint> = merged.iter().map(fingerprint).collect();
            for d in items {
                if !seen.contains(&fingerprint(&d)) {
                    merged.push(d);
                }
            }
        }
        merged
    }

    async fn wait_for_push(&self, uri: &Url, version: i32, cap: Duration) {
        let mut rx = self.store.tx.subscribe();
        let deadline = tokio::time::Instant::now() + cap;
        let matched = |stored: &StoredDiags| match stored.version {
            Some(v) => v >= version,
            None => false,
        };
        // A version-less push is treated as matching the current version once
        // it arrives after our change (detected via a generation bump); this
        // includes CLEAN (empty) publishes, which must also end the wait.
        loop {
            let stored = self.store.get(uri);
            if matched(&stored) {
                break;
            }
            let gen_before = *rx.borrow();
            tokio::select! {
                res = rx.changed() => {
                    if res.is_err() {
                        return;
                    }
                    let stored = self.store.get(uri);
                    if matched(&stored) {
                        break;
                    }
                    // Version-less publish after the change: treat as current.
                    if stored.version.is_none() && *rx.borrow() != gen_before {
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => return,
            }
        }
        // Debounce: wait for 150 ms of quiet (bounded by the deadline).
        loop {
            tokio::select! {
                res = rx.changed() => {
                    if res.is_err() {
                        return;
                    }
                }
                _ = tokio::time::sleep(DEBOUNCE) => return,
                _ = tokio::time::sleep_until(deadline) => return,
            }
        }
    }

    /// One pull-diagnostics request, capped at [`PULL_CAP`].
    pub async fn pull_diagnostics(&self, uri: &Url) -> Result<Vec<Diagnostic>> {
        let mut sock = self.sock();
        let fut = sock.document_diagnostic(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        let result = tokio::time::timeout(PULL_CAP, fut)
            .await
            .map_err(|_| anyhow!("pull diagnostics timed out"))??;
        match result {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
                Ok(full.full_document_diagnostic_report.items)
            }
            _ => Ok(Vec::new()),
        }
    }

    // ----- request wrappers used by the lsp tool -----

    pub async fn definition(
        &self,
        params: lsp_types::GotoDefinitionParams,
    ) -> Result<Option<lsp_types::GotoDefinitionResponse>> {
        Ok(self.sock().definition(params).await?)
    }

    pub async fn references(
        &self,
        params: lsp_types::ReferenceParams,
    ) -> Result<Option<Vec<lsp_types::Location>>> {
        Ok(self.sock().references(params).await?)
    }

    pub async fn hover(&self, params: lsp_types::HoverParams) -> Result<Option<lsp_types::Hover>> {
        Ok(self.sock().hover(params).await?)
    }

    pub async fn document_symbol(
        &self,
        params: lsp_types::DocumentSymbolParams,
    ) -> Result<Option<lsp_types::DocumentSymbolResponse>> {
        Ok(self.sock().document_symbol(params).await?)
    }

    pub async fn workspace_symbol(
        &self,
        params: lsp_types::WorkspaceSymbolParams,
    ) -> Result<Option<lsp_types::WorkspaceSymbolResponse>> {
        Ok(self.sock().symbol(params).await?)
    }

    pub async fn rename(
        &self,
        params: lsp_types::RenameParams,
    ) -> Result<Option<lsp_types::WorkspaceEdit>> {
        Ok(self.sock().rename(params).await?)
    }

    /// Best-effort shutdown: `shutdown` + `exit`, kill after 500 ms.
    pub async fn shutdown(&self) {
        jcode_logging::debug(&format!("lsp: shutting down `{}`", self.spec.id));
        let mut sock = self.sock();
        let _ = tokio::time::timeout(Duration::from_millis(500), sock.shutdown(())).await;
        let _ = sock.exit(());
        let mut child = self.child.lock().await;
        if let Some(mut c) = child.take() {
            tokio::select! {
                _ = c.wait() => {}
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    let _ = c.kill().await;
                }
            }
        }
        let _ = sock.emit(Stop);
    }
}

pub fn fingerprint(d: &Diagnostic) -> Fingerprint {
    (d.range.start.line, d.range.start.character, d.message.clone())
}

pub fn file_uri(path: &Path) -> Result<Url> {
    Url::from_file_path(path).map_err(|_| anyhow!("not an absolute file path: {}", path.display()))
}

/// LSP `languageId` from the file extension.
pub fn language_id(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("py" | "pyi") => "python",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx") => "cpp",
        Some("m") => "objective-c",
        Some("mm") => "objective-cpp",
        Some("json") => "json",
        Some("jsonc") => "jsonc",
        Some("yaml" | "yml") => "yaml",
        _ => "plaintext",
    }
}

/// Read a file for LSP consumption. Returns `None` for missing, non-UTF8,
/// or oversized (> [`MAX_FILE_SIZE`]) files.
pub async fn read_text_for_lsp(path: &Path) -> Option<String> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_SIZE {
        return None;
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}
