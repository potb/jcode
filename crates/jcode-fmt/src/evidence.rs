//! Pure evidence-detection helpers: walking up from a file's directory to
//! find the config files / dependency declarations that gate each built-in
//! formatter. No PATH lookups or process execution here (see `registry.rs`
//! and `exec.rs`), so these are directly unit-testable against tempdir trees.

use std::path::{Path, PathBuf};

/// Walk up from `start` (inclusive) to the filesystem root, returning the
/// first directory for which `pred` returns true.
pub fn find_dir_upwards<F: Fn(&Path) -> bool>(start: &Path, pred: F) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if pred(d) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Directory containing a `package.json` (walking up from `start`) whose
/// `dependencies` or `devDependencies` object has a `dep_name` key.
pub fn package_json_dep_dir(start: &Path, dep_name: &str) -> Option<PathBuf> {
    find_dir_upwards(start, |d| package_json_has_dep(&d.join("package.json"), dep_name))
}

fn package_json_has_dep(package_json: &Path, dep_name: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(package_json) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    for key in ["dependencies", "devDependencies"] {
        if value
            .get(key)
            .and_then(|deps| deps.get(dep_name))
            .is_some()
        {
            return true;
        }
    }
    false
}

/// Directory containing any of `filenames` (walking up from `start`).
pub fn config_file_dir(start: &Path, filenames: &[&str]) -> Option<PathBuf> {
    find_dir_upwards(start, |d| filenames.iter().any(|f| d.join(f).is_file()))
}

/// Directory containing the nearest `node_modules/.bin/<bin_name>` (walking
/// up from `start`).
pub fn nearest_node_modules_bin(start: &Path, bin_name: &str) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join("node_modules").join(".bin").join(bin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Ruff config evidence: directory containing a `pyproject.toml` with a
/// `[tool.ruff]` table, a `ruff.toml`/`.ruff.toml`, or a `requirements.txt`/
/// `Pipfile` that mentions `ruff` (case-insensitive substring), whichever is
/// found first walking up from `start`.
pub fn ruff_config_evidence_dir(start: &Path) -> Option<PathBuf> {
    find_dir_upwards(start, |d| {
        pyproject_has_ruff_table(&d.join("pyproject.toml"))
            || d.join("ruff.toml").is_file()
            || d.join(".ruff.toml").is_file()
            || file_mentions(&d.join("requirements.txt"), "ruff")
            || file_mentions(&d.join("Pipfile"), "ruff")
    })
}

fn pyproject_has_ruff_table(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content
        .lines()
        .any(|line| line.trim() == "[tool.ruff]" || line.trim_start().starts_with("[tool.ruff."))
}

fn file_mentions(path: &Path, needle: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

/// Directory containing a `.clang-format` file (walking up from `start`).
pub fn clang_format_dir(start: &Path) -> Option<PathBuf> {
    config_file_dir(start, &[".clang-format"])
}

/// `biome.json`/`biome.jsonc` evidence directory (walking up from `start`).
pub fn biome_config_dir(start: &Path) -> Option<PathBuf> {
    config_file_dir(start, &["biome.json", "biome.jsonc"])
}

/// Parse the Rust edition from the nearest walked-up `Cargo.toml`'s
/// `[package]` table. Returns `"2024"` when no `Cargo.toml` is found or it
/// has no `edition` key.
pub fn cargo_edition(start: &Path) -> String {
    let Some(dir) = find_dir_upwards(start, |d| d.join("Cargo.toml").is_file()) else {
        return "2024".to_string();
    };
    let Ok(content) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return "2024".to_string();
    };
    parse_cargo_edition(&content).unwrap_or_else(|| "2024".to_string())
}

/// Extract `edition = "..."` from Cargo.toml source. Reads the
/// `[package]` table first; falls back to `[workspace.package]` (covering
/// `edition.workspace = true` crates and virtual workspace roots).
/// Deliberately simple line-based parsing rather than a full TOML parse
/// (this crate has no `toml` dependency).
fn parse_cargo_edition(content: &str) -> Option<String> {
    parse_edition_in_section(content, "[package]")
        .or_else(|| parse_edition_in_section(content, "[workspace.package]"))
}

fn parse_edition_in_section(content: &str, section: &str) -> Option<String> {
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == section;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("edition") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                if let Some(value) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
                    return Some(value.to_string());
                }
                if let Some(value) = rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn package_json_dep_detection_walks_up() {
        let dir = tmp();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(
            root.join("a/package.json"),
            r#"{"devDependencies": {"prettier": "^3.0.0"}}"#,
        )
        .unwrap();
        let found = package_json_dep_dir(&root.join("a/b/c"), "prettier");
        assert_eq!(found, Some(root.join("a")));
    }

    #[test]
    fn package_json_without_dep_not_found() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(root.join("package.json"), r#"{"dependencies": {}}"#).unwrap();
        assert!(package_json_dep_dir(root, "prettier").is_none());
    }

    #[test]
    fn package_json_dependencies_key_also_matches() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"dependencies": {"prettier": "3.0.0"}}"#,
        )
        .unwrap();
        assert_eq!(package_json_dep_dir(root, "prettier"), Some(root.to_path_buf()));
    }

    #[test]
    fn biome_config_walkup() {
        let dir = tmp();
        let root = dir.path();
        std::fs::create_dir_all(root.join("x/y")).unwrap();
        std::fs::write(root.join("x/biome.jsonc"), "{}").unwrap();
        assert_eq!(biome_config_dir(&root.join("x/y")), Some(root.join("x")));
        assert!(biome_config_dir(root).is_none());
    }

    #[test]
    fn nearest_node_modules_bin_walkup() {
        let dir = tmp();
        let root = dir.path();
        std::fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        std::fs::write(root.join("node_modules/.bin/prettier"), "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(root.join("nested/deep")).unwrap();
        let found = nearest_node_modules_bin(&root.join("nested/deep"), "prettier");
        assert_eq!(found, Some(root.join("node_modules/.bin/prettier")));
    }

    #[test]
    fn ruff_pyproject_tool_ruff_table() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.poetry]\nname = \"x\"\n\n[tool.ruff]\nline-length = 100\n",
        )
        .unwrap();
        assert_eq!(ruff_config_evidence_dir(root), Some(root.to_path_buf()));
    }

    #[test]
    fn ruff_pyproject_without_tool_ruff_not_evidence() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(root.join("pyproject.toml"), "[tool.poetry]\nname = \"x\"\n").unwrap();
        assert!(ruff_config_evidence_dir(root).is_none());
    }

    #[test]
    fn ruff_toml_is_evidence() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(root.join("ruff.toml"), "line-length = 88\n").unwrap();
        assert_eq!(ruff_config_evidence_dir(root), Some(root.to_path_buf()));
    }

    #[test]
    fn dot_ruff_toml_is_evidence() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(root.join(".ruff.toml"), "line-length = 88\n").unwrap();
        assert_eq!(ruff_config_evidence_dir(root), Some(root.to_path_buf()));
    }

    #[test]
    fn requirements_txt_mentioning_ruff_is_evidence() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(root.join("requirements.txt"), "black\nRuff==0.5.0\n").unwrap();
        assert_eq!(ruff_config_evidence_dir(root), Some(root.to_path_buf()));
    }

    #[test]
    fn requirements_txt_without_ruff_not_evidence() {
        let dir = tmp();
        let root = dir.path();
        std::fs::write(root.join("requirements.txt"), "black\nflake8\n").unwrap();
        assert!(ruff_config_evidence_dir(root).is_none());
    }

    #[test]
    fn clang_format_requires_dotfile() {
        let dir = tmp();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        assert!(clang_format_dir(&root.join("src")).is_none());
        std::fs::write(root.join(".clang-format"), "BasedOnStyle: LLVM\n").unwrap();
        assert_eq!(clang_format_dir(&root.join("src")), Some(root.to_path_buf()));
    }

    #[test]
    fn cargo_edition_parses_package_section() {
        let dir = tmp();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"foo\"\nedition = \"2021\"\n\n[dependencies]\nedition-lookalike = \"1\"\n",
        )
        .unwrap();
        assert_eq!(cargo_edition(&root.join("src")), "2021");
    }

    #[test]
    fn cargo_edition_defaults_when_missing() {
        let dir = tmp();
        let root = dir.path();
        assert_eq!(cargo_edition(root), "2024");
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"foo\"\n").unwrap();
        assert_eq!(cargo_edition(root), "2024");
    }
}
