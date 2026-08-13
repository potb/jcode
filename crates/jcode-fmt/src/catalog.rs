//! Built-in formatter catalog and config merging.
//!
//! Mirrors `jcode-lsp`'s `catalog.rs` shape, adapted for formatters: instead
//! of a fixed command + root markers, each built-in has an [`EvidenceKind`]
//! that decides both *whether* it runs (evidence gating) and *how* its
//! command/workspace directory get resolved for a given file (see
//! `evidence.rs`).

use crate::config_compat::FormatterConfig;

/// Which evidence-resolution strategy a built-in formatter uses. See
/// `docs/superpowers/specs/2026-08-05-auto-format-design.md` catalog v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// Canonical toolchain formatter for Rust: binary on PATH; edition is
    /// read from a walked-up `Cargo.toml` (default 2024 when absent).
    Rustfmt,
    /// Canonical toolchain formatter for Go: binary on PATH only.
    Gofmt,
    /// `prettier` in a walked-up `package.json`'s deps; binary from the
    /// nearest `node_modules/.bin/prettier`.
    Prettier,
    /// `biome.json`/`biome.jsonc` found walking up; binary from the nearest
    /// `node_modules/.bin/biome`.
    Biome,
    /// `ruff` on PATH AND config evidence (pyproject `[tool.ruff]`,
    /// `ruff.toml`/`.ruff.toml`, or a `ruff` mention in
    /// `requirements.txt`/`Pipfile`).
    Ruff,
    /// Only used when `ruff` is not enabled for the same directory; `uv` on
    /// PATH.
    Uv,
    /// `.clang-format` found walking up AND binary on PATH.
    ClangFormat,
}

/// Where a resolved formatter's command comes from.
#[derive(Debug, Clone)]
pub enum Source {
    /// A catalog built-in, optionally with its default command overridden by
    /// config (`command` override). Evidence gating always follows `kind`;
    /// only the invoked command template changes.
    Builtin {
        kind: EvidenceKind,
        override_command: Option<Vec<String>>,
    },
    /// An unknown config id: a literal command, gated only by the first
    /// command element resolving on PATH or as an absolute/relative file
    /// path (no evidence walk-up).
    Custom { command: Vec<String> },
}

/// A fully-resolved formatter description: built-in entry merged with user
/// config, or a brand-new custom formatter defined entirely by config.
#[derive(Debug, Clone)]
pub struct FormatterSpec {
    pub id: String,
    pub extensions: Vec<String>,
    pub source: Source,
}

const PRETTIER_BIOME_EXTENSIONS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "html", "htm", "css", "scss", "sass",
    "less", "vue", "svelte", "json", "jsonc", "yaml", "yml", "toml", "xml", "md", "mdx", "graphql",
    "gql",
];

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Ensure a config-provided command contains a `$FILE` placeholder; append
/// the placeholder as the last argument when missing (per spec).
fn ensure_file_placeholder(mut command: Vec<String>) -> Vec<String> {
    if !command.iter().any(|arg| arg.contains("$FILE")) {
        command.push("$FILE".to_string());
    }
    command
}

fn builtin_catalog() -> Vec<FormatterSpec> {
    vec![
        FormatterSpec {
            id: "rustfmt".to_string(),
            extensions: strs(&["rs"]),
            source: Source::Builtin {
                kind: EvidenceKind::Rustfmt,
                override_command: None,
            },
        },
        FormatterSpec {
            id: "gofmt".to_string(),
            extensions: strs(&["go"]),
            source: Source::Builtin {
                kind: EvidenceKind::Gofmt,
                override_command: None,
            },
        },
        FormatterSpec {
            id: "prettier".to_string(),
            extensions: strs(PRETTIER_BIOME_EXTENSIONS),
            source: Source::Builtin {
                kind: EvidenceKind::Prettier,
                override_command: None,
            },
        },
        FormatterSpec {
            id: "biome".to_string(),
            extensions: strs(PRETTIER_BIOME_EXTENSIONS),
            source: Source::Builtin {
                kind: EvidenceKind::Biome,
                override_command: None,
            },
        },
        FormatterSpec {
            id: "ruff".to_string(),
            extensions: strs(&["py", "pyi"]),
            source: Source::Builtin {
                kind: EvidenceKind::Ruff,
                override_command: None,
            },
        },
        FormatterSpec {
            id: "uv".to_string(),
            extensions: strs(&["py", "pyi"]),
            source: Source::Builtin {
                kind: EvidenceKind::Uv,
                override_command: None,
            },
        },
        FormatterSpec {
            id: "clang-format".to_string(),
            extensions: strs(&["c", "h", "cpp", "hpp", "cc", "hh", "cxx", "hxx", "m", "mm"]),
            source: Source::Builtin {
                kind: EvidenceKind::ClangFormat,
                override_command: None,
            },
        },
    ]
}

/// Merge built-in catalog with user config.
///
/// - `disabled = true` removes a built-in.
/// - `command` set on a built-in overrides its invoked command (evidence
///   gating still follows the built-in's [`EvidenceKind`]).
/// - `extensions` set on a built-in overrides its extension list.
/// - Unknown ids with a `command` AND `extensions` define new custom
///   formatters (no evidence gate beyond the command's first element
///   resolving on PATH or as a file path).
pub fn resolve_catalog(cfg: &FormatterConfig) -> Vec<FormatterSpec> {
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
                    let cmd = ensure_file_placeholder(cmd.clone());
                    if let Source::Builtin {
                        override_command, ..
                    } = &mut merged.source
                    {
                        *override_command = Some(cmd);
                    }
                }
                if let Some(exts) = &over.extensions {
                    merged.extensions = exts.clone();
                }
                out.push(merged);
            }
            None => out.push(builtin),
        }
    }
    // Custom formatters: unknown ids with a command AND extensions.
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
        let Some(extensions) = sc.extensions.clone().filter(|e| !e.is_empty()) else {
            continue;
        };
        out.push(FormatterSpec {
            id: id.clone(),
            extensions,
            source: Source::Custom {
                command: ensure_file_placeholder(command),
            },
        });
    }
    out
}

/// Every catalog entry whose extension list contains `ext` (case-insensitive),
/// in catalog order.
pub fn specs_for_extension<'a>(catalog: &'a [FormatterSpec], ext: &str) -> Vec<&'a FormatterSpec> {
    catalog
        .iter()
        .filter(|s| s.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_compat::FormatterServerConfig;

    fn cfg_with(servers: Vec<(&str, FormatterServerConfig)>) -> FormatterConfig {
        FormatterConfig {
            enabled: true,
            servers: servers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn builtin_catalog_has_all_seven() {
        let catalog = resolve_catalog(&FormatterConfig::default());
        let ids: Vec<&str> = catalog.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "rustfmt",
                "gofmt",
                "prettier",
                "biome",
                "ruff",
                "uv",
                "clang-format"
            ]
        );
    }

    #[test]
    fn resolves_by_extension() {
        let catalog = resolve_catalog(&FormatterConfig::default());
        let rs = specs_for_extension(&catalog, "rs");
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].id, "rustfmt");
        let py = specs_for_extension(&catalog, "PY");
        let py_ids: Vec<&str> = py.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(py_ids, vec!["ruff", "uv"]);
        assert!(specs_for_extension(&catalog, "zig").is_empty());
    }

    #[test]
    fn config_merge_disable_builtin() {
        let cfg = cfg_with(vec![(
            "gofmt",
            FormatterServerConfig {
                disabled: true,
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        assert!(!catalog.iter().any(|s| s.id == "gofmt"));
        assert!(catalog.iter().any(|s| s.id == "rustfmt"));
    }

    #[test]
    fn config_merge_override_command_appends_file_placeholder() {
        let cfg = cfg_with(vec![(
            "prettier",
            FormatterServerConfig {
                command: Some(vec!["prettier-custom".into(), "--write".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        let p = catalog.iter().find(|s| s.id == "prettier").unwrap();
        match &p.source {
            Source::Builtin {
                kind,
                override_command,
            } => {
                assert_eq!(*kind, EvidenceKind::Prettier);
                assert_eq!(
                    override_command.as_ref().unwrap(),
                    &vec![
                        "prettier-custom".to_string(),
                        "--write".to_string(),
                        "$FILE".to_string()
                    ]
                );
            }
            _ => panic!("expected builtin source"),
        }
        // Untouched fields keep builtin values.
        assert!(p.extensions.contains(&"ts".to_string()));
    }

    #[test]
    fn config_merge_override_command_keeps_existing_file_placeholder() {
        let cfg = cfg_with(vec![(
            "rustfmt",
            FormatterServerConfig {
                command: Some(vec!["rustfmt".into(), "$FILE".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        let rs = catalog.iter().find(|s| s.id == "rustfmt").unwrap();
        match &rs.source {
            Source::Builtin {
                override_command, ..
            } => {
                assert_eq!(
                    override_command.as_ref().unwrap(),
                    &vec!["rustfmt".to_string(), "$FILE".to_string()]
                );
            }
            _ => panic!("expected builtin source"),
        }
    }

    #[test]
    fn config_merge_custom_formatter() {
        let cfg = cfg_with(vec![(
            "shfmt",
            FormatterServerConfig {
                command: Some(vec!["shfmt".into(), "-w".into()]),
                extensions: Some(vec!["sh".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        let custom = catalog.iter().find(|s| s.id == "shfmt").unwrap();
        match &custom.source {
            Source::Custom { command } => {
                assert_eq!(
                    command,
                    &vec!["shfmt".to_string(), "-w".to_string(), "$FILE".to_string()]
                );
            }
            _ => panic!("expected custom source"),
        }
        assert_eq!(custom.extensions, vec!["sh".to_string()]);
    }

    #[test]
    fn custom_formatter_without_command_ignored() {
        let cfg = cfg_with(vec![(
            "mystery",
            FormatterServerConfig {
                extensions: Some(vec!["myst".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        assert!(!catalog.iter().any(|s| s.id == "mystery"));
    }

    #[test]
    fn custom_formatter_without_extensions_ignored() {
        let cfg = cfg_with(vec![(
            "mystery",
            FormatterServerConfig {
                command: Some(vec!["mystery-fmt".into()]),
                ..Default::default()
            },
        )]);
        let catalog = resolve_catalog(&cfg);
        assert!(!catalog.iter().any(|s| s.id == "mystery"));
    }
}
