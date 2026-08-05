//! Built-in server catalog and config merging.

use std::path::{Path, PathBuf};

use crate::config_compat::LspConfig;

/// A fully-resolved server description: built-in entry merged with user config.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSpec {
    pub id: String,
    /// Binary + args. First element is looked up on PATH.
    pub command: Vec<String>,
    /// File extensions (without dot) this server handles.
    pub extensions: Vec<String>,
    /// Files/dirs that mark a workspace root, in priority order.
    pub root_markers: Vec<String>,
    /// `initializationOptions` for the initialize request.
    pub initialization_options: Option<serde_json::Value>,
}

fn builtin_catalog() -> Vec<ServerSpec> {
    fn spec(
        id: &str,
        command: &[&str],
        extensions: &[&str],
        root_markers: &[&str],
    ) -> ServerSpec {
        ServerSpec {
            id: id.to_string(),
            command: command.iter().map(|s| s.to_string()).collect(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            root_markers: root_markers.iter().map(|s| s.to_string()).collect(),
            initialization_options: None,
        }
    }
    vec![
        spec(
            "rust-analyzer",
            &["rust-analyzer"],
            &["rs"],
            &["Cargo.toml"],
        ),
        spec(
            "typescript-language-server",
            &["typescript-language-server", "--stdio"],
            &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"],
            &["tsconfig.json", "jsconfig.json", "package.json"],
        ),
        spec(
            "pyright",
            &["pyright-langserver", "--stdio"],
            &["py", "pyi"],
            &[
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
                "Pipfile",
                "pyrightconfig.json",
            ],
        ),
        spec("gopls", &["gopls"], &["go"], &["go.mod", "go.work"]),
        spec(
            "clangd",
            &["clangd"],
            &["c", "h", "cpp", "hpp", "cc", "hh", "cxx", "hxx", "m", "mm"],
            &["compile_commands.json", ".clangd", "compile_flags.txt"],
        ),
        spec(
            "vscode-json-language-server",
            &["vscode-json-language-server", "--stdio"],
            &["json", "jsonc"],
            &[],
        ),
        spec(
            "yaml-language-server",
            &["yaml-language-server", "--stdio"],
            &["yaml", "yml"],
            &[],
        ),
    ]
}

/// Merge built-in catalog with user config.
///
/// - `disabled = true` removes a built-in.
/// - Fields set in config override the built-in's fields.
/// - Unknown ids with a `command` define new custom servers.
pub fn resolve_catalog(cfg: &LspConfig) -> Vec<ServerSpec> {
    let mut out = Vec::new();
    let builtins = builtin_catalog();
    let builtin_ids: std::collections::HashSet<String> =
        builtins.iter().map(|s| s.id.clone()).collect();
    for builtin in builtins {
        match cfg.servers.get(&builtin.id) {
            Some(over) if over.disabled => continue,
            Some(over) => {
                let mut merged = builtin;
                if let Some(cmd) = &over.command
                    && !cmd.is_empty()
                {
                    merged.command = cmd.clone();
                }
                if let Some(exts) = &over.extensions {
                    merged.extensions = exts.clone();
                }
                if let Some(markers) = &over.root_markers {
                    merged.root_markers = markers.clone();
                }
                if over.initialization_options.is_some() {
                    merged.initialization_options = over.initialization_options.clone();
                }
                out.push(merged);
            }
            None => out.push(builtin),
        }
    }
    // Custom servers: unknown ids with a command.
    let mut customs: Vec<_> = cfg
        .servers
        .iter()
        .filter(|(id, sc)| !builtin_ids.contains(id.as_str()) && !sc.disabled)
        .collect();
    customs.sort_by(|a, b| a.0.cmp(b.0));
    for (id, sc) in customs {
        let Some(command) = sc.command.clone().filter(|c| !c.is_empty()) else {
            continue;
        };
        out.push(ServerSpec {
            id: id.clone(),
            command,
            extensions: sc.extensions.clone().unwrap_or_default(),
            root_markers: sc.root_markers.clone().unwrap_or_default(),
            initialization_options: sc.initialization_options.clone(),
        });
    }
    out
}

/// Pick the first catalog server matching the file extension.
pub fn spec_for_path<'a>(catalog: &'a [ServerSpec], path: &Path) -> Option<&'a ServerSpec> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    catalog
        .iter()
        .find(|s| s.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)))
}

/// Walk up from `path` looking for the spec's root markers.
/// Falls back to `fallback` (session cwd) when no marker is found.
pub fn workspace_root(spec: &ServerSpec, path: &Path, fallback: &Path) -> PathBuf {
    let start = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    let mut dir = Some(start);
    while let Some(d) = dir {
        for marker in &spec.root_markers {
            if d.join(marker).exists() {
                return d.to_path_buf();
            }
        }
        dir = d.parent();
    }
    fallback.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_compat::LspServerConfig;

    fn cfg_with(servers: Vec<(&str, LspServerConfig)>) -> LspConfig {
        LspConfig {
            enabled: true,
            servers: servers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn resolves_by_extension() {
        let catalog = resolve_catalog(&LspConfig::default());
        let rs = spec_for_path(&catalog, Path::new("/tmp/foo.rs")).unwrap();
        assert_eq!(rs.id, "rust-analyzer");
        let ts = spec_for_path(&catalog, Path::new("/tmp/a/b.tsx")).unwrap();
        assert_eq!(ts.id, "typescript-language-server");
        let py = spec_for_path(&catalog, Path::new("x.PY")).unwrap();
        assert_eq!(py.id, "pyright");
        assert_eq!(py.command, vec!["pyright-langserver", "--stdio"]);
        assert!(spec_for_path(&catalog, Path::new("noext")).is_none());
        assert!(spec_for_path(&catalog, Path::new("a.zig")).is_none());
    }

    #[test]
    fn root_marker_walkup_and_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("proj/src/deep")).unwrap();
        std::fs::write(root.join("proj/Cargo.toml"), "").unwrap();
        let catalog = resolve_catalog(&LspConfig::default());
        let rs = spec_for_path(&catalog, Path::new("f.rs")).unwrap();
        let found = workspace_root(rs, &root.join("proj/src/deep/main.rs"), Path::new("/fb"));
        assert_eq!(found, root.join("proj"));
        // No marker anywhere -> fallback.
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp2.path().join("x")).unwrap();
        let found = workspace_root(rs, &tmp2.path().join("x/main.rs"), Path::new("/fb"));
        assert_eq!(found, PathBuf::from("/fb"));
    }

    #[test]
    fn config_merge_override_command() {
        let cfg = cfg_with(vec![(
            "rust-analyzer",
            LspServerConfig {
                command: Some(vec!["ra-custom".into(), "--flag".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        let rs = catalog.iter().find(|s| s.id == "rust-analyzer").unwrap();
        assert_eq!(rs.command, vec!["ra-custom", "--flag"]);
        // Untouched fields keep builtin values.
        assert_eq!(rs.extensions, vec!["rs"]);
        assert_eq!(rs.root_markers, vec!["Cargo.toml"]);
    }

    #[test]
    fn config_merge_disable_builtin() {
        let cfg = cfg_with(vec![(
            "gopls",
            LspServerConfig {
                disabled: true,
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        assert!(!catalog.iter().any(|s| s.id == "gopls"));
        assert!(catalog.iter().any(|s| s.id == "rust-analyzer"));
    }

    #[test]
    fn config_merge_custom_server() {
        let cfg = cfg_with(vec![(
            "zls",
            LspServerConfig {
                command: Some(vec!["zls".into()]),
                extensions: Some(vec!["zig".into()]),
                root_markers: Some(vec!["build.zig".into()]),
                initialization_options: Some(serde_json::json!({"a": 1})),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        let zls = catalog.iter().find(|s| s.id == "zls").unwrap();
        assert_eq!(zls.command, vec!["zls"]);
        assert_eq!(zls.extensions, vec!["zig"]);
        assert_eq!(zls.root_markers, vec!["build.zig"]);
        assert!(zls.initialization_options.is_some());
        let z = spec_for_path(&catalog, Path::new("m.zig")).unwrap();
        assert_eq!(z.id, "zls");
    }

    #[test]
    fn custom_server_without_command_ignored() {
        let cfg = cfg_with(vec![(
            "mystery",
            LspServerConfig {
                extensions: Some(vec!["myst".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        assert!(!catalog.iter().any(|s| s.id == "mystery"));
    }
}
