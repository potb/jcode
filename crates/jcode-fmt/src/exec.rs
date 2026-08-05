//! Formatter execution: spawn, 5s timeout, exit-code interpretation. Total —
//! never panics, every error path returns `Ok(false)` (or the whole
//! `format_file` skips silently via `jcode_logging::debug`).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::registry::ResolvedFormatter;

/// Files bigger than this are never formatted (matches `jcode-lsp`'s
/// `MAX_FILE_SIZE`).
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// Per-formatter execution timeout.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(5);

/// Skip formatting: file missing, not a regular file, oversized, or looks
/// binary (null byte in the first 8KB). Mirrors `jcode-lsp`'s
/// `read_text_for_lsp` size/binary gate.
pub async fn should_skip(path: &Path) -> bool {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return true;
    };
    if !meta.is_file() || meta.len() > MAX_FILE_SIZE {
        return true;
    }
    let Ok(bytes) = tokio::fs::read(path).await else {
        return true;
    };
    bytes.iter().take(8192).any(|&b| b == 0)
}

/// Substitute the `$FILE` token in a command template with the absolute
/// path. Boundary-aware: `$FILE` is replaced anywhere in an arg (so
/// `--write=$FILE` works) unless followed by an identifier character, so
/// literals like `$FILENAME` pass through untouched. If no token is
/// present (should not happen post config-merge, but be defensive), append
/// the path as the last argument.
fn substitute_file(command: &[String], abs_path: &Path) -> Vec<String> {
    let file_str = abs_path.to_string_lossy().into_owned();
    let mut had_token = false;
    let mut out: Vec<String> = command
        .iter()
        .map(|arg| {
            let mut result = String::with_capacity(arg.len());
            let mut rest = arg.as_str();
            while let Some(pos) = rest.find("$FILE") {
                let after = &rest[pos + "$FILE".len()..];
                let boundary = !after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                result.push_str(&rest[..pos]);
                if boundary {
                    result.push_str(&file_str);
                    had_token = true;
                } else {
                    result.push_str("$FILE");
                }
                rest = after;
            }
            result.push_str(rest);
            result
        })
        .collect();
    if !had_token {
        out.push(file_str);
    }
    out
}

/// Run one resolved formatter against `path`. `true` on a clean (exit code
/// 0) run; `false` on any failure (non-zero exit, spawn error, timeout).
/// Never panics, never propagates an error to the caller.
pub async fn run_one(formatter: &ResolvedFormatter, abs_path: &Path) -> bool {
    let argv = substitute_file(&formatter.command, abs_path);
    let Some((bin, args)) = argv.split_first() else {
        return false;
    };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .current_dir(&formatter.workspace_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let spawn_result = cmd.spawn();
    let mut child = match spawn_result {
        Ok(child) => child,
        Err(err) => {
            jcode_logging::debug(&format!(
                "fmt: failed to spawn `{}` for {}: {err:#}",
                formatter.id,
                abs_path.display()
            ));
            return false;
        }
    };

    match tokio::time::timeout(EXEC_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                true
            } else {
                jcode_logging::debug(&format!(
                    "fmt: `{}` exited with {status} for {}",
                    formatter.id,
                    abs_path.display()
                ));
                false
            }
        }
        Ok(Err(err)) => {
            jcode_logging::debug(&format!(
                "fmt: `{}` wait failed for {}: {err:#}",
                formatter.id,
                abs_path.display()
            ));
            false
        }
        Err(_) => {
            jcode_logging::debug(&format!(
                "fmt: `{}` timed out for {}",
                formatter.id,
                abs_path.display()
            ));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_file_replaces_placeholder() {
        let command = vec![
            "prettier".to_string(),
            "--write".to_string(),
            "$FILE".to_string(),
        ];
        let out = substitute_file(&command, Path::new("/tmp/foo.ts"));
        assert_eq!(out, vec!["prettier", "--write", "/tmp/foo.ts"]);
    }

    #[test]
    fn substitute_file_appends_when_missing() {
        let command = vec!["prettier".to_string(), "--write".to_string()];
        let out = substitute_file(&command, Path::new("/tmp/foo.ts"));
        assert_eq!(out, vec!["prettier", "--write", "/tmp/foo.ts"]);
    }

    #[tokio::test]
    async fn should_skip_missing_file() {
        assert!(should_skip(Path::new("/nonexistent/path/xyz")).await);
    }

    #[tokio::test]
    async fn should_skip_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.txt");
        let big = vec![b'a'; (MAX_FILE_SIZE + 1) as usize];
        tokio::fs::write(&file, &big).await.unwrap();
        assert!(should_skip(&file).await);
    }

    #[tokio::test]
    async fn should_skip_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bin.dat");
        tokio::fs::write(&file, [0u8, 1, 2, 3]).await.unwrap();
        assert!(should_skip(&file).await);
    }

    #[tokio::test]
    async fn should_not_skip_normal_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ok.txt");
        tokio::fs::write(&file, "hello world\n").await.unwrap();
        assert!(!should_skip(&file).await);
    }
}
