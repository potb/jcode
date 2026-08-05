//! Diagnostics formatting per the design spec.
//!
//! ```text
//! <diagnostics file="src/foo.rs">
//! ERROR [12:5] cannot find value `x` in this scope
//! </diagnostics>
//! ```
//!
//! - Errors only, max 20 per file.
//! - File has zero errors -> warnings shown instead (`WARN [l:c] msg`), max 10.
//! - Cross-file: files that gained new errors among open documents, capped 5.

use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity};

pub const MAX_ERRORS_PER_FILE: usize = 20;
pub const MAX_WARNINGS_PER_FILE: usize = 10;
pub const MAX_CROSS_FILE_FILES: usize = 5;

fn severity(d: &Diagnostic) -> DiagnosticSeverity {
    d.severity.unwrap_or(DiagnosticSeverity::ERROR)
}

fn format_line(label: &str, d: &Diagnostic) -> String {
    // LSP ranges are 0-based; display 1-based.
    let line = d.range.start.line + 1;
    let col = d.range.start.character + 1;
    let msg = d.message.replace('\n', " ").trim().to_string();
    format!("{label} [{line}:{col}] {msg}")
}

/// Format one file's diagnostics per spec. Returns `None` when there is
/// nothing to show (no errors and no warnings).
pub fn format_file_diagnostics(display_path: &str, diags: &[Diagnostic]) -> Option<String> {
    let errors: Vec<&Diagnostic> = diags
        .iter()
        .filter(|d| severity(d) == DiagnosticSeverity::ERROR)
        .collect();
    let mut lines = Vec::new();
    if !errors.is_empty() {
        for d in errors.iter().take(MAX_ERRORS_PER_FILE) {
            lines.push(format_line("ERROR", d));
        }
        if errors.len() > MAX_ERRORS_PER_FILE {
            lines.push(format!(
                "... {} more errors",
                errors.len() - MAX_ERRORS_PER_FILE
            ));
        }
    } else {
        let warnings: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| severity(d) == DiagnosticSeverity::WARNING)
            .collect();
        if warnings.is_empty() {
            return None;
        }
        for d in warnings.iter().take(MAX_WARNINGS_PER_FILE) {
            lines.push(format_line("WARN", d));
        }
        if warnings.len() > MAX_WARNINGS_PER_FILE {
            lines.push(format!(
                "... {} more warnings",
                warnings.len() - MAX_WARNINGS_PER_FILE
            ));
        }
    }
    Some(format!(
        "<diagnostics file=\"{display_path}\">\n{}\n</diagnostics>",
        lines.join("\n")
    ))
}

/// Format all severities for the `lsp` tool's `diagnostics` action.
pub fn format_all_severities(display_path: &str, diags: &[Diagnostic]) -> String {
    if diags.is_empty() {
        return format!("No diagnostics for {display_path}");
    }
    let mut lines = Vec::new();
    for d in diags {
        let label = match severity(d) {
            DiagnosticSeverity::ERROR => "ERROR",
            DiagnosticSeverity::WARNING => "WARN",
            DiagnosticSeverity::INFORMATION => "INFO",
            DiagnosticSeverity::HINT => "HINT",
            _ => "DIAG",
        };
        lines.push(format_line(label, d));
    }
    format!(
        "<diagnostics file=\"{display_path}\">\n{}\n</diagnostics>",
        lines.join("\n")
    )
}

/// Prefer a workspace-relative display path, falling back to the full path.
pub fn display_path(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Format the primary file's diagnostics plus cross-file blocks for other
/// open files that gained new errors (capped at [`MAX_CROSS_FILE_FILES`]).
///
/// `cross_file` entries are `(display_path, new_error_diagnostics)`.
pub fn format_write_feedback(
    primary_display: &str,
    primary: &[Diagnostic],
    cross_file: &[(String, Vec<Diagnostic>)],
) -> Option<String> {
    let mut blocks = Vec::new();
    if let Some(b) = format_file_diagnostics(primary_display, primary) {
        blocks.push(b);
    }
    let mut shown = 0usize;
    for (path, diags) in cross_file {
        if shown >= MAX_CROSS_FILE_FILES {
            break;
        }
        let errors: Vec<Diagnostic> = diags
            .iter()
            .filter(|d| severity(d) == DiagnosticSeverity::ERROR)
            .cloned()
            .collect();
        if errors.is_empty() {
            continue;
        }
        if let Some(b) = format_file_diagnostics(path, &errors) {
            blocks.push(b);
            shown += 1;
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    fn diag(line: u32, col: u32, sev: DiagnosticSeverity, msg: &str) -> Diagnostic {
        Diagnostic {
            range: Range::new(Position::new(line, col), Position::new(line, col + 1)),
            severity: Some(sev),
            message: msg.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn errors_only_when_errors_present() {
        let diags = vec![
            diag(11, 4, DiagnosticSeverity::ERROR, "cannot find value `x` in this scope"),
            diag(2, 0, DiagnosticSeverity::WARNING, "unused variable"),
        ];
        let out = format_file_diagnostics("src/foo.rs", &diags).unwrap();
        assert_eq!(
            out,
            "<diagnostics file=\"src/foo.rs\">\nERROR [12:5] cannot find value `x` in this scope\n</diagnostics>"
        );
        assert!(!out.contains("WARN"));
    }

    #[test]
    fn warnings_fallback_when_zero_errors() {
        let diags = vec![
            diag(0, 0, DiagnosticSeverity::WARNING, "unused import"),
            diag(4, 2, DiagnosticSeverity::HINT, "consider this"),
        ];
        let out = format_file_diagnostics("a.rs", &diags).unwrap();
        assert!(out.contains("WARN [1:1] unused import"));
        assert!(!out.contains("HINT"));
    }

    #[test]
    fn none_when_clean() {
        assert!(format_file_diagnostics("a.rs", &[]).is_none());
        let only_hints = vec![diag(0, 0, DiagnosticSeverity::HINT, "meh")];
        assert!(format_file_diagnostics("a.rs", &only_hints).is_none());
    }

    #[test]
    fn error_cap_20() {
        let diags: Vec<_> = (0..25)
            .map(|i| diag(i, 0, DiagnosticSeverity::ERROR, "boom"))
            .collect();
        let out = format_file_diagnostics("a.rs", &diags).unwrap();
        assert_eq!(out.matches("ERROR [").count(), 20);
        assert!(out.contains("... 5 more errors"));
    }

    #[test]
    fn warning_cap_10() {
        let diags: Vec<_> = (0..15)
            .map(|i| diag(i, 0, DiagnosticSeverity::WARNING, "hm"))
            .collect();
        let out = format_file_diagnostics("a.rs", &diags).unwrap();
        assert_eq!(out.matches("WARN [").count(), 10);
        assert!(out.contains("... 5 more warnings"));
    }

    #[test]
    fn cross_file_cap_5_and_errors_only() {
        let primary = vec![diag(0, 0, DiagnosticSeverity::ERROR, "primary broken")];
        let cross: Vec<(String, Vec<Diagnostic>)> = (0..8)
            .map(|i| {
                (
                    format!("other{i}.rs"),
                    vec![diag(1, 1, DiagnosticSeverity::ERROR, "caller broken")],
                )
            })
            .collect();
        let out = format_write_feedback("main.rs", &primary, &cross).unwrap();
        assert_eq!(out.matches("<diagnostics").count(), 6); // primary + 5 cross
        assert!(out.contains("other4.rs"));
        assert!(!out.contains("other5.rs"));
    }

    #[test]
    fn cross_file_skips_warning_only_files() {
        let cross = vec![
            (
                "warnonly.rs".to_string(),
                vec![diag(0, 0, DiagnosticSeverity::WARNING, "meh")],
            ),
            (
                "err.rs".to_string(),
                vec![diag(0, 0, DiagnosticSeverity::ERROR, "bad")],
            ),
        ];
        let out = format_write_feedback("main.rs", &[], &cross).unwrap();
        assert!(!out.contains("warnonly.rs"));
        assert!(out.contains("err.rs"));
    }

    #[test]
    fn multiline_messages_flattened() {
        let diags = vec![diag(0, 0, DiagnosticSeverity::ERROR, "line one\nline two")];
        let out = format_file_diagnostics("a.rs", &diags).unwrap();
        assert!(out.contains("ERROR [1:1] line one line two"));
    }

    #[test]
    fn all_severities_format() {
        let diags = vec![
            diag(0, 0, DiagnosticSeverity::ERROR, "e"),
            diag(1, 0, DiagnosticSeverity::WARNING, "w"),
            diag(2, 0, DiagnosticSeverity::INFORMATION, "i"),
            diag(3, 0, DiagnosticSeverity::HINT, "h"),
        ];
        let out = format_all_severities("a.rs", &diags);
        for label in ["ERROR", "WARN", "INFO", "HINT"] {
            assert!(out.contains(label), "missing {label}");
        }
        assert_eq!(format_all_severities("a.rs", &[]), "No diagnostics for a.rs");
    }
}
