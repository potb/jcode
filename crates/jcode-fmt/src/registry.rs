//! Process-global registry: config, PATH cache, and the evidence cache keyed
//! by `(formatter_id, workspace_dir)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use crate::catalog::{EvidenceKind, FormatterSpec, Source, resolve_catalog, specs_for_extension};
use crate::config_compat::FormatterConfig;
use crate::evidence;

static CONFIG: LazyLock<RwLock<FormatterConfig>> =
    LazyLock::new(|| RwLock::new(FormatterConfig::default()));

/// PATH lookup cache: binary name -> found.
static PATH_CACHE: LazyLock<RwLock<HashMap<String, bool>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Evidence cache: `(formatter_id, workspace_dir)` -> resolved command, or
/// `None` when evidence was not found for that directory. Mid-session
/// installs need a config override or process restart (accepted, same as
/// LSP).
type EvidenceKey = (String, PathBuf);
static EVIDENCE_CACHE: LazyLock<RwLock<HashMap<EvidenceKey, Option<Vec<String>>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Store the process-global `[formatter]` config.
pub fn set_config(cfg: FormatterConfig) {
    if let Ok(mut guard) = CONFIG.write() {
        if *guard == cfg {
            return;
        }
        *guard = cfg;
    }
    // Config changed: resolved commands may embed old overrides. Drop the
    // evidence-derived caches so the next touch re-resolves under the new
    // config. PATH_CACHE stays (binary presence is config-independent).
    if let Ok(mut cache) = EVIDENCE_CACHE.write() {
        cache.clear();
    }
    if let Ok(mut cache) = WORKSPACE_DIR_CACHE.write() {
        cache.clear();
    }
}

pub fn config() -> FormatterConfig {
    CONFIG.read().map(|g| g.clone()).unwrap_or_default()
}

/// Merged catalog for the current config.
pub fn catalog() -> Vec<FormatterSpec> {
    resolve_catalog(&config())
}

/// PATH lookup with per-process caching (positive and negative). Copied from
/// `jcode-lsp`'s `registry::binary_on_path`.
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

/// A formatter fully resolved for a specific file: the command to run (with
/// `$FILE` still a placeholder) and the workspace directory to run it in.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedFormatter {
    pub id: String,
    pub command: Vec<String>,
    pub workspace_dir: PathBuf,
}

/// Resolve every enabled formatter that matches `path`'s extension, in
/// catalog order, evaluating evidence (with caching) for each. Returns the
/// empty vec when nothing matches or the master switch is off.
pub fn resolve_for_path(path: &Path) -> Vec<ResolvedFormatter> {
    if !config().enabled {
        return Vec::new();
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return Vec::new();
    };
    let ext = ext.to_ascii_lowercase();
    let dir = path.parent().unwrap_or(Path::new("."));
    let catalog = catalog();

    // Ruff/uv special case: uv only runs when ruff does NOT resolve for the
    // same directory. Compute ruff's resolution first so uv can consult it.
    let ruff_resolved = catalog
        .iter()
        .find(|s| s.id == "ruff")
        .and_then(|spec| resolve_one(spec, dir));

    let mut out = Vec::new();
    for spec in specs_for_extension(&catalog, &ext) {
        if spec.id == "uv" && ruff_resolved.is_some() {
            continue;
        }
        if let Some(resolved) = resolve_one(spec, dir) {
            out.push(resolved);
        }
    }
    out
}

fn cache_get(key: &EvidenceKey) -> Option<Option<Vec<String>>> {
    EVIDENCE_CACHE.read().ok()?.get(key).cloned()
}

fn cache_put(key: EvidenceKey, value: Option<Vec<String>>) {
    if let Ok(mut cache) = EVIDENCE_CACHE.write() {
        cache.insert(key, value);
    }
}

/// Resolve one spec against a given file's directory. Uses the evidence
/// cache keyed by `(id, workspace_dir)`, where `workspace_dir` is the
/// directory containing the evidence file (or `dir` itself for PATH-only
/// formatters).
fn resolve_one(spec: &FormatterSpec, dir: &Path) -> Option<ResolvedFormatter> {
    match &spec.source {
        Source::Custom { command } => {
            let bin = command.first()?;
            if !binary_on_path(bin) {
                return None;
            }
            Some(ResolvedFormatter {
                id: spec.id.clone(),
                command: command.clone(),
                workspace_dir: dir.to_path_buf(),
            })
        }
        Source::Builtin {
            kind,
            override_command,
        } => resolve_builtin(spec, *kind, override_command.as_deref(), dir),
    }
}

fn resolve_builtin(
    spec: &FormatterSpec,
    kind: EvidenceKind,
    override_command: Option<&[String]>,
    dir: &Path,
) -> Option<ResolvedFormatter> {
    // PATH gate on the EFFECTIVE binary: a config `command` override (e.g.
    // an absolute path to a binary outside the daemon's PATH) replaces the
    // built-in default, so the gate must check the override's first element,
    // not the hardcoded default name.
    let effective_binary_available = |default_bin: &str| -> bool {
        match override_command.and_then(|c| c.first()) {
            Some(first) => binary_on_path(first),
            None => binary_on_path(default_bin),
        }
    };
    match kind {
        EvidenceKind::Rustfmt => {
            if !effective_binary_available("rustfmt") {
                return None;
            }
            let key = (spec.id.clone(), dir.to_path_buf());
            if let Some(cached) = cache_get(&key) {
                return cached.map(|command| ResolvedFormatter {
                    id: spec.id.clone(),
                    command,
                    workspace_dir: dir.to_path_buf(),
                });
            }
            let edition = evidence::cargo_edition(dir);
            let command = override_command.map(|c| c.to_vec()).unwrap_or_else(|| {
                vec![
                    "rustfmt".to_string(),
                    "--edition".to_string(),
                    edition,
                    "$FILE".to_string(),
                ]
            });
            cache_put(key, Some(command.clone()));
            Some(ResolvedFormatter {
                id: spec.id.clone(),
                command,
                workspace_dir: dir.to_path_buf(),
            })
        }
        EvidenceKind::Gofmt => {
            if !effective_binary_available("gofmt") {
                return None;
            }
            let command = override_command.map(|c| c.to_vec()).unwrap_or_else(|| {
                vec!["gofmt".to_string(), "-w".to_string(), "$FILE".to_string()]
            });
            Some(ResolvedFormatter {
                id: spec.id.clone(),
                command,
                workspace_dir: dir.to_path_buf(),
            })
        }
        EvidenceKind::Prettier => {
            resolve_node_tool(spec, "prettier", override_command, dir, |bin| {
                vec![
                    bin.to_string_lossy().into_owned(),
                    "--write".to_string(),
                    "$FILE".to_string(),
                ]
            })
        }
        EvidenceKind::Biome => resolve_node_tool(spec, "biome", override_command, dir, |bin| {
            vec![
                bin.to_string_lossy().into_owned(),
                "format".to_string(),
                "--write".to_string(),
                "$FILE".to_string(),
            ]
        }),
        EvidenceKind::Ruff => {
            if !effective_binary_available("ruff") {
                return None;
            }
            let key = (spec.id.clone(), dir.to_path_buf());
            if let Some(cached) = cache_get(&key) {
                return cached.map(|command| ResolvedFormatter {
                    id: spec.id.clone(),
                    command,
                    workspace_dir: cached_ws(&key),
                });
            }
            let Some(evidence_dir) = evidence::ruff_config_evidence_dir(dir) else {
                cache_put(key, None);
                return None;
            };
            let command = override_command.map(|c| c.to_vec()).unwrap_or_else(|| {
                vec![
                    "ruff".to_string(),
                    "format".to_string(),
                    "$FILE".to_string(),
                ]
            });
            cache_put(key.clone(), Some(command.clone()));
            record_ws(&key, &evidence_dir);
            Some(ResolvedFormatter {
                id: spec.id.clone(),
                command,
                workspace_dir: evidence_dir,
            })
        }
        EvidenceKind::Uv => {
            if !effective_binary_available("uv") {
                return None;
            }
            let command = override_command.map(|c| c.to_vec()).unwrap_or_else(|| {
                vec![
                    "uv".to_string(),
                    "format".to_string(),
                    "--".to_string(),
                    "$FILE".to_string(),
                ]
            });
            Some(ResolvedFormatter {
                id: spec.id.clone(),
                command,
                workspace_dir: dir.to_path_buf(),
            })
        }
        EvidenceKind::ClangFormat => {
            if !effective_binary_available("clang-format") {
                return None;
            }
            let key = (spec.id.clone(), dir.to_path_buf());
            if let Some(cached) = cache_get(&key) {
                return cached.map(|command| ResolvedFormatter {
                    id: spec.id.clone(),
                    command,
                    workspace_dir: cached_ws(&key),
                });
            }
            let Some(evidence_dir) = evidence::clang_format_dir(dir) else {
                cache_put(key, None);
                return None;
            };
            let command = override_command.map(|c| c.to_vec()).unwrap_or_else(|| {
                vec![
                    "clang-format".to_string(),
                    "-i".to_string(),
                    "$FILE".to_string(),
                ]
            });
            cache_put(key.clone(), Some(command.clone()));
            record_ws(&key, &evidence_dir);
            Some(ResolvedFormatter {
                id: spec.id.clone(),
                command,
                workspace_dir: evidence_dir,
            })
        }
    }
}

/// Separate cache for the workspace directory associated with an evidence
/// key, since `EVIDENCE_CACHE` only stores the command. Small and process
/// global like the rest of the registry.
static WORKSPACE_DIR_CACHE: LazyLock<RwLock<HashMap<EvidenceKey, PathBuf>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn record_ws(key: &EvidenceKey, dir: &Path) {
    if let Ok(mut cache) = WORKSPACE_DIR_CACHE.write() {
        cache.insert(key.clone(), dir.to_path_buf());
    }
}

fn cached_ws(key: &EvidenceKey) -> PathBuf {
    WORKSPACE_DIR_CACHE
        .read()
        .ok()
        .and_then(|c| c.get(key).cloned())
        .unwrap_or_else(|| key.1.clone())
}

/// Shared resolution for prettier/biome: package.json dep evidence (prettier)
/// or config-file evidence (biome), plus nearest `node_modules/.bin/<name>`.
fn resolve_node_tool(
    spec: &FormatterSpec,
    bin_name: &str,
    override_command: Option<&[String]>,
    dir: &Path,
    build_command: impl Fn(&Path) -> Vec<String>,
) -> Option<ResolvedFormatter> {
    let key = (spec.id.clone(), dir.to_path_buf());
    if let Some(cached) = cache_get(&key) {
        return cached.map(|command| ResolvedFormatter {
            id: spec.id.clone(),
            command,
            workspace_dir: cached_ws(&key),
        });
    }
    let evidence_dir = if bin_name == "prettier" {
        evidence::package_json_dep_dir(dir, "prettier")
    } else {
        evidence::biome_config_dir(dir)
    };
    let Some(evidence_dir) = evidence_dir else {
        cache_put(key, None);
        return None;
    };
    // With a config `command` override, the project-local node_modules/.bin
    // binary is not needed (the override names its own binary); evidence
    // gating above still applies per spec.
    let command = match override_command {
        Some(c) => {
            let first = c.first()?;
            if !binary_on_path(first) {
                cache_put(key, None);
                return None;
            }
            c.to_vec()
        }
        None => {
            let Some(bin_path) = evidence::nearest_node_modules_bin(dir, bin_name) else {
                cache_put(key, None);
                return None;
            };
            build_command(&bin_path)
        }
    };
    cache_put(key.clone(), Some(command.clone()));
    record_ws(&key, &evidence_dir);
    Some(ResolvedFormatter {
        id: spec.id.clone(),
        command,
        workspace_dir: evidence_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_compat::FormatterServerConfig;

    /// The registry's config/caches are process-global; serialize tests in
    /// this module so `set_config` calls from concurrent test threads don't
    /// race each other (cargo runs `#[test]` fns in parallel by default).
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_config(FormatterConfig::default());
        if let Ok(mut c) = EVIDENCE_CACHE.write() {
            c.clear();
        }
        if let Ok(mut c) = WORKSPACE_DIR_CACHE.write() {
            c.clear();
        }
        guard
    }

    #[test]
    fn custom_formatter_resolves_with_path_binary() {
        let _guard = reset();
        let mut servers = HashMap::new();
        servers.insert(
            "true-formatter".to_string(),
            FormatterServerConfig {
                command: Some(vec!["true".to_string()]),
                extensions: Some(vec!["truefmt".to_string()]),
                ..Default::default()
            },
        );
        set_config(FormatterConfig {
            enabled: true,
            servers,
        });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.truefmt");
        std::fs::write(&file, "hi").unwrap();
        let resolved = resolve_for_path(&file);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "true-formatter");
    }

    /// A built-in whose default binary is NOT on PATH must still resolve
    /// when config overrides its command with an available binary
    /// (absolute path or PATH name). Regression: the PATH gate used to
    /// check the hardcoded default name even with an override present.
    #[test]
    fn builtin_override_gates_on_effective_binary() {
        let _guard = reset();
        // `rustfmt` may or may not be on PATH in CI; the override points at
        // /usr/bin/true which always exists on unix, so resolution must
        // succeed regardless.
        let mut servers = HashMap::new();
        servers.insert(
            "rustfmt".to_string(),
            FormatterServerConfig {
                command: Some(vec!["/usr/bin/true".to_string(), "$FILE".to_string()]),
                ..Default::default()
            },
        );
        set_config(FormatterConfig {
            enabled: true,
            servers,
        });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let resolved = resolve_for_path(&file);
        assert_eq!(
            resolved.len(),
            1,
            "override with absolute binary must resolve"
        );
        assert_eq!(resolved[0].command[0], "/usr/bin/true");

        // And an override naming a nonexistent binary must NOT resolve.
        let mut servers = HashMap::new();
        servers.insert(
            "rustfmt".to_string(),
            FormatterServerConfig {
                command: Some(vec![
                    "/nonexistent/definitely-not-a-binary".to_string(),
                    "$FILE".to_string(),
                ]),
                ..Default::default()
            },
        );
        set_config(FormatterConfig {
            enabled: true,
            servers,
        });
        let resolved = resolve_for_path(&file);
        assert!(resolved.is_empty(), "missing override binary must gate out");
    }

    #[test]
    fn disabled_master_switch_returns_nothing() {
        let _guard = reset();
        set_config(FormatterConfig {
            enabled: false,
            servers: HashMap::new(),
        });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.rs");
        assert!(resolve_for_path(&file).is_empty());
    }

    #[test]
    fn uv_suppressed_when_ruff_has_evidence() {
        let _guard = reset();
        // Both ruff and uv only resolve if their binaries are on PATH, which
        // is environment-dependent; this test only exercises the config
        // merge/gating logic by checking that when ruff evidence exists,
        // resolve_for_path never includes "uv" alongside "ruff" (regardless
        // of whether either binary is actually installed here).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ruff.toml"), "line-length = 88\n").unwrap();
        let file = dir.path().join("x.py");
        std::fs::write(&file, "x=1\n").unwrap();
        let resolved = resolve_for_path(&file);
        let ids: Vec<&str> = resolved.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !(ids.contains(&"ruff") && ids.contains(&"uv")),
            "got: {ids:?}"
        );
    }
}
