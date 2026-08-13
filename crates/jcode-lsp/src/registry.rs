//! Process-global registry: config, PATH cache, and live clients keyed by
//! `(server_id, workspace_root)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use anyhow::{Result, anyhow};

use crate::catalog::{ServerSpec, resolve_catalog, spec_for_path, workspace_root};
use crate::client::LspClient;
use crate::config_compat::LspConfig;

/// Disable a server for the rest of the process after this many crashes.
const MAX_CRASHES: u32 = 3;

/// Cap on spawn + initialize handshake. A server that does not complete the
/// handshake in this window is killed and counted as a crash, so a hung
/// server can never stall write tools (spec: "never stall").
pub const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

static CONFIG: LazyLock<RwLock<LspConfig>> = LazyLock::new(|| RwLock::new(LspConfig::default()));

/// PATH lookup cache: binary name -> found.
static PATH_CACHE: LazyLock<RwLock<HashMap<String, bool>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

struct Registry {
    clients: tokio::sync::Mutex<HashMap<(String, PathBuf), Arc<LspClient>>>,
    crashes: std::sync::Mutex<HashMap<String, AtomicU32>>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| Registry {
    clients: tokio::sync::Mutex::new(HashMap::new()),
    crashes: std::sync::Mutex::new(HashMap::new()),
});

/// Store the process-global `[lsp]` config.
pub fn set_config(cfg: LspConfig) {
    if let Ok(mut guard) = CONFIG.write() {
        *guard = cfg;
    }
}

pub fn config() -> LspConfig {
    CONFIG.read().map(|g| g.clone()).unwrap_or_default()
}

/// Merged catalog for the current config.
pub fn catalog() -> Vec<ServerSpec> {
    resolve_catalog(&config())
}

/// PATH lookup with per-process caching (positive and negative).
pub fn binary_on_path(name: &str) -> bool {
    if let Ok(cache) = PATH_CACHE.read()
        && let Some(&found) = cache.get(name)
    {
        return found;
    }
    let found = which(name);
    if let Ok(mut cache) = PATH_CACHE.write() {
        cache.insert(name.to_string(), found);
    }
    found
}

fn which(name: &str) -> bool {
    // Absolute/relative paths: check directly.
    if name.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(name).is_file();
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file() && is_executable(&candidate)
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Enabled AND at least one catalog server binary is on PATH.
pub fn enabled() -> bool {
    if !config().enabled {
        return false;
    }
    catalog()
        .iter()
        .any(|s| s.command.first().is_some_and(|bin| binary_on_path(bin)))
}

fn crash_count(server_id: &str) -> u32 {
    REGISTRY
        .crashes
        .lock()
        .ok()
        .and_then(|m| m.get(server_id).map(|c| c.load(Ordering::SeqCst)))
        .unwrap_or(0)
}

fn record_crash(server_id: &str) -> u32 {
    let Ok(mut map) = REGISTRY.crashes.lock() else {
        return MAX_CRASHES;
    };
    let counter = map.entry(server_id.to_string()).or_default();
    counter.fetch_add(1, Ordering::SeqCst) + 1
}

/// Whether a spawn was needed (cold) alongside the client.
pub struct ClientLease {
    pub client: Arc<LspClient>,
    pub cold: bool,
}

/// Resolve the spec + workspace root for a path, without spawning.
pub fn resolve(path: &Path) -> Option<(ServerSpec, PathBuf)> {
    let catalog = catalog();
    let spec = spec_for_path(&catalog, path)?.clone();
    let fallback = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = workspace_root(&spec, path, &fallback);
    Some((spec, root))
}

/// Get (or lazily spawn) the client for a file. Respawns once after a crash;
/// gives up for the process after [`MAX_CRASHES`] crashes of a server id.
pub async fn client_for(path: &Path) -> Result<ClientLease> {
    if !config().enabled {
        return Err(anyhow!("lsp is disabled"));
    }
    let (spec, root) = resolve(path)
        .ok_or_else(|| anyhow!("no language server configured for `{}`", path.display()))?;
    client_for_spec(&spec, &root).await
}

/// Get (or lazily spawn) the client for a `(spec, root)` pair. Used by both
/// the per-file path ([`client_for`]) and the workspace path
/// (`workspace_handle_for`). The clients map lock is NEVER held across the
/// spawn/handshake await: a hung server must not block other registry users.
pub async fn client_for_spec(spec: &ServerSpec, root: &Path) -> Result<ClientLease> {
    if !config().enabled {
        return Err(anyhow!("lsp is disabled"));
    }
    let bin = spec
        .command
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("server `{}` has an empty command", spec.id))?;
    if !binary_on_path(&bin) {
        return Err(anyhow!(
            "language server `{bin}` (for {}) not found on PATH",
            spec.id
        ));
    }
    if crash_count(&spec.id) >= MAX_CRASHES {
        return Err(anyhow!(
            "language server `{}` disabled after {MAX_CRASHES} crashes",
            spec.id
        ));
    }

    let key = (spec.id.clone(), root.to_path_buf());

    // Phase 1 (short critical section): reuse a live client or evict a dead
    // one. The lock is released before any spawn/handshake await.
    {
        let mut clients = REGISTRY.clients.lock().await;
        if let Some(existing) = clients.get(&key) {
            if existing.is_alive() {
                return Ok(ClientLease {
                    client: existing.clone(),
                    cold: false,
                });
            }
            // Crash detected: drop the dead client and maybe respawn below.
            clients.remove(&key);
            let crashes = record_crash(&spec.id);
            jcode_logging::warn(&format!(
                "lsp: server `{}` died (crash {crashes}/{MAX_CRASHES})",
                spec.id
            ));
            if crashes >= MAX_CRASHES {
                return Err(anyhow!(
                    "language server `{}` disabled after {MAX_CRASHES} crashes",
                    spec.id
                ));
            }
        }
    }

    // Phase 2 (no lock): spawn + initialize, bounded by INIT_TIMEOUT. Timeout
    // expiry drops the in-flight spawn future, which kills the child
    // (`kill_on_drop`), and counts toward the crash-disable ladder.
    let client = match tokio::time::timeout(INIT_TIMEOUT, LspClient::spawn(spec, root)).await {
        Ok(Ok(client)) => Arc::new(client),
        Ok(Err(err)) => return Err(err),
        Err(_) => {
            let crashes = record_crash(&spec.id);
            jcode_logging::warn(&format!(
                "lsp: server `{}` hung during initialize (crash {crashes}/{MAX_CRASHES})",
                spec.id
            ));
            return Err(anyhow!(
                "language server `{}` timed out during initialize",
                spec.id
            ));
        }
    };

    // Phase 3 (short critical section): insert with a double-check. Another
    // task may have spawned the same server while we were not holding the
    // lock; prefer theirs and shut ours down in the background.
    let mut clients = REGISTRY.clients.lock().await;
    if let Some(existing) = clients.get(&key)
        && existing.is_alive()
    {
        let existing = existing.clone();
        drop(clients);
        tokio::spawn(async move { client.shutdown().await });
        return Ok(ClientLease {
            client: existing,
            cold: false,
        });
    }
    clients.insert(key, client.clone());
    Ok(ClientLease { client, cold: true })
}

/// Best-effort shutdown of every live client.
pub async fn shutdown_all() {
    let clients: Vec<Arc<LspClient>> = {
        let mut guard = REGISTRY.clients.lock().await;
        guard.drain().map(|(_, c)| c).collect()
    };
    let mut tasks = Vec::new();
    for client in clients {
        tasks.push(tokio::spawn(async move { client.shutdown().await }));
    }
    for task in tasks {
        let _ = task.await;
    }
}
