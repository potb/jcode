//! Lexical comment scanner behind the write/edit comment notice.
//!
//! Decisions and their evidence live in potb/jcode#49. The three that shape
//! this module: doc comments are never reported, the scan covers the whole
//! file rather than the diff, and the result is advisory text appended after
//! the write has already landed.
//!
//! The scanner is a hand-written lexer rather than a parser. It tracks string
//! and char literals only as far as needed to avoid reporting a `//` that
//! lives inside one, which is the only false positive that matters at this
//! resolution.

use std::path::Path;

/// Longest file the scanner will look at. Above this the notice is skipped;
/// the cost is a silent miss on generated files, which is the right trade.
const MAX_SCAN_BYTES: usize = 1024 * 1024;

/// Reported comments per file, matching `MAX_WARNINGS_PER_FILE` in
/// `jcode-lsp`'s diagnostics formatter.
const MAX_COMMENTS_PER_FILE: usize = 10;

/// A reportable comment: 1-based line number and the trimmed source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentSpan {
    pub line: usize,
    pub text: String,
    /// Matches upstream's memo patterns: a comment describing a change rather
    /// than the code. Near-certain verdict, so the report marks it.
    pub memo: bool,
}

/// Comment syntax family for a language id from `jcode_lsp::language_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    /// `//` and `/* */`, plus `///` and `//!` doc comments.
    CStyle,
    /// `#` only, plus `"""`/`'''` docstrings.
    Hash,
    /// No comment syntax worth scanning.
    None,
}

fn syntax_for(language_id: &str) -> Syntax {
    match language_id {
        "rust" | "typescript" | "typescriptreact" | "javascript" | "javascriptreact" | "go"
        | "c" | "cpp" | "objective-c" | "objective-cpp" | "jsonc" => Syntax::CStyle,
        "python" | "yaml" => Syntax::Hash,
        _ => Syntax::None,
    }
}

/// Comment syntax for a path the LSP catalog does not name.
///
/// `jcode_lsp::language_id` exists to pick a language server, so it only maps
/// extensions a server is configured for and calls everything else
/// `plaintext`. Comment syntax is a much weaker property than a language
/// server, so the files it drops (shell, Ruby, Java, TOML, ...) are scannable
/// even though nothing will ever run a server on them.
///
/// Only families whose line comments this lexer already handles correctly are
/// listed. CSS is deliberately absent: it has `/* */` but no `//`, so
/// [`scan_c_style`] would misread a protocol-relative URL as a comment.
fn syntax_for_extension(ext: &str) -> Syntax {
    match ext {
        "java" | "kt" | "kts" | "swift" | "scala" | "dart" | "zig" | "cs" | "php" | "proto"
        | "sol" | "hcl" | "tf" | "groovy" | "gradle" => Syntax::CStyle,
        "sh" | "bash" | "zsh" | "fish" | "rb" | "toml" | "nix" | "pl" | "pm" | "r" | "jl"
        | "ex" | "exs" | "tcl" | "awk" | "dockerfile" | "mk" | "makefile" | "cmake" | "ps1"
        | "gitignore" | "dockerignore" | "env" | "ini" | "cfg" | "conf" | "properties" => {
            Syntax::Hash
        }
        _ => Syntax::None,
    }
}

/// Comment syntax for a file, preferring the LSP catalog and falling back to
/// the extension (or the bare file name, for `Dockerfile`-style files).
fn syntax_for_path(path: &Path) -> Syntax {
    let syntax = syntax_for(jcode_lsp::language_id(path));
    if syntax != Syntax::None {
        return syntax;
    }
    let key = path
        .extension()
        .or_else(|| path.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.trim_start_matches('.').to_ascii_lowercase())
        .unwrap_or_default();
    syntax_for_extension(&key)
}

/// Lint directives and BDD step words, exempt regardless of language. A
/// directive is machine-readable configuration that happens to live in a
/// comment, and a `given`/`when`/`then` line is test structure.
const EXEMPT_PREFIXES: &[&str] = &[
    "noqa",
    "type:",
    "eslint",
    "prettier",
    "ts-",
    "clippy:",
    "allow(",
    "deny(",
    "expect(",
    "warn(",
    "rustfmt::",
    "safety:",
    "given",
    "when",
    "then",
    "@ts-",
    "biome-ignore",
    "codegen",
    "spell-checker",
    "cspell",
    "istanbul",
    "c8 ",
    "v8 ",
    "nolint",
    "go:",
    "!",
];

/// Upstream's memo patterns: first words that mark a comment as a note about
/// an edit rather than an explanation of the code.
const MEMO_PREFIXES: &[&str] = &[
    "added",
    "removed",
    "changed",
    "updated",
    "fixed",
    "renamed",
    "moved",
    "refactored",
    "new:",
    "was ",
    "now ",
    "previously",
    "note to self",
    "todo(agent)",
];

fn is_exempt(body: &str) -> bool {
    let lower = body.trim_start().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    EXEMPT_PREFIXES.iter().any(|p| lower.starts_with(p))
}

fn is_memo(body: &str) -> bool {
    let lower = body.trim_start().to_ascii_lowercase();
    MEMO_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Scan `content` and return the non-doc, non-exempt comments in it.
///
/// Production callers reach the scanner through [`comment_notice`], which
/// resolves the syntax from the path so it can fall back past the LSP
/// catalog; this by-language-id entry point remains for the syntax tests.
#[cfg(test)]
fn scan(content: &str, language_id: &str) -> Vec<CommentSpan> {
    scan_with(content, syntax_for(language_id))
}

/// Scan `content` using an already-resolved comment syntax.
fn scan_with(content: &str, syntax: Syntax) -> Vec<CommentSpan> {
    if syntax == Syntax::None || content.len() > MAX_SCAN_BYTES {
        return Vec::new();
    }
    match syntax {
        Syntax::CStyle => scan_c_style(content),
        Syntax::Hash => scan_hash(content),
        Syntax::None => Vec::new(),
    }
}

/// Push a span when the comment body earns a report.
fn push_if_reportable(out: &mut Vec<CommentSpan>, line: usize, body: &str) {
    if is_exempt(body) {
        return;
    }
    out.push(CommentSpan {
        line,
        text: body.trim().to_string(),
        memo: is_memo(body),
    });
}

fn scan_c_style(content: &str) -> Vec<CommentSpan> {
    let mut out = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    let mut line = 1;
    // Cleared on every newline so an unterminated literal cannot swallow the
    // rest of the file, which is what a naive lexer does to broken source.
    let mut in_string: Option<u8> = None;
    let mut in_block = false;
    let mut block_is_doc = false;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            in_string = None;
            i += 1;
            continue;
        }
        if in_block {
            if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                in_block = false;
                i += 2;
                continue;
            }
            if !block_is_doc && !bytes[i].is_ascii_whitespace() {
                let end = line_end(bytes, i);
                let body = trim_block_marker(&content[i..end]);
                if !body.trim().is_empty() {
                    push_if_reportable(&mut out, line, body);
                }
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(quote) = in_string {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => {
                in_string = Some(b);
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = line_end(bytes, i);
                let raw = &content[i..end];
                if !is_doc_line(raw) {
                    push_if_reportable(&mut out, line, raw.trim_start_matches('/'));
                }
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                block_is_doc = matches!(bytes.get(i + 2), Some(b'*') | Some(b'!'));
                in_block = true;
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

/// `///`, `//!` in Rust and `///` in Go/TS are documentation, never reported.
fn is_doc_line(raw: &str) -> bool {
    raw.starts_with("///") || raw.starts_with("//!")
}

fn trim_block_marker(s: &str) -> &str {
    s.trim_start().trim_start_matches('*')
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn scan_hash(content: &str) -> Vec<CommentSpan> {
    let mut out = Vec::new();
    let mut in_docstring: Option<&str> = None;

    for (idx, raw_line) in content.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw_line.trim_start();

        if let Some(delim) = in_docstring {
            if trimmed.contains(delim) {
                in_docstring = None;
            }
            continue;
        }
        for delim in ["\"\"\"", "'''"] {
            if let Some(rest) = trimmed.strip_prefix(delim) {
                // A one-line docstring opens and closes on the same line.
                if !rest.contains(delim) {
                    in_docstring = Some(delim);
                }
                break;
            }
        }
        if in_docstring.is_some() || trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
            continue;
        }
        if line == 1 && trimmed.starts_with("#!") {
            continue;
        }
        if let Some(pos) = hash_comment_start(raw_line) {
            push_if_reportable(&mut out, line, &raw_line[pos + 1..]);
        }
    }
    out
}

/// Byte offset of a `#` that starts a comment, skipping ones inside quotes.
fn hash_comment_start(line: &str) -> Option<usize> {
    let mut in_string: Option<u8> = None;
    for (i, b) in line.bytes().enumerate() {
        match in_string {
            Some(q) => {
                if b == b'\\' {
                    continue;
                }
                if b == q {
                    in_string = None;
                }
            }
            None => match b {
                b'"' | b'\'' => in_string = Some(b),
                b'#' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

/// Advisory notice for a file that was just written, or `None` when the file
/// has nothing reportable. Mirrors `jcode-lsp`'s `<diagnostics>` block shape.
pub(crate) fn comment_notice(display_path: &str, content: &str, path: &Path) -> Option<String> {
    let spans = scan_with(content, syntax_for_path(path));
    if spans.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = spans
        .iter()
        .take(MAX_COMMENTS_PER_FILE)
        .map(|s| {
            let mark = if s.memo { " (memo)" } else { "" };
            format!("{} {}{mark}", s.line, s.text)
        })
        .collect();
    if spans.len() > MAX_COMMENTS_PER_FILE {
        lines.push(format!("... {} more", spans.len() - MAX_COMMENTS_PER_FILE));
    }
    Some(format!(
        "<comments file=\"{display_path}\">\n{}\n</comments>\nThis file has comments. Remove ones that restate the code. Keep them only where they explain why.",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(content: &str, lang: &str) -> Vec<String> {
        scan(content, lang).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn reports_ordinary_rust_comments() {
        let src = "// increment i\nlet i = i + 1;\n";
        assert_eq!(texts(src, "rust"), vec!["increment i"]);
    }

    #[test]
    fn doc_comments_are_never_reported() {
        let src = "/// Adds one.\n//! Module doc.\n/** Block doc */\npub fn f() {}\n";
        assert!(scan(src, "rust").is_empty());
    }

    #[test]
    fn slashes_inside_strings_are_not_comments() {
        let src = "let url = \"https://example.com\";\nlet c = '/';\n";
        assert!(scan(src, "rust").is_empty());
    }

    #[test]
    fn unterminated_string_does_not_swallow_the_file() {
        let src = "let s = \"oops;\n// real comment\n";
        assert_eq!(texts(src, "rust"), vec!["real comment"]);
    }

    #[test]
    fn block_comment_bodies_are_reported_per_line() {
        let src = "/* first\n   second */\n";
        assert_eq!(texts(src, "rust"), vec!["first", "second */"]);
    }

    #[test]
    fn lint_directives_are_exempt() {
        let src = "// clippy: allow\n// eslint-disable-next-line\n// noqa\n";
        assert!(scan(src, "rust").is_empty());
    }

    #[test]
    fn bdd_steps_are_exempt() {
        let src = "// given a user\n// when they log in\n// then it works\n";
        assert!(scan(src, "rust").is_empty());
    }

    #[test]
    fn memo_comments_are_flagged() {
        let spans = scan("// updated to use the new API\n", "rust");
        assert_eq!(spans.len(), 1);
        assert!(spans[0].memo);
    }

    #[test]
    fn python_hash_comments_are_reported_and_docstrings_are_not() {
        let src = "#!/usr/bin/env python\n\"\"\"Module doc.\"\"\"\n# set x\nx = 1  # inline\n";
        assert_eq!(texts(src, "python"), vec!["set x", "inline"]);
    }

    #[test]
    fn multiline_python_docstring_is_skipped() {
        let src = "\"\"\"\nDoc line that looks like # a comment\n\"\"\"\n# real\n";
        assert_eq!(texts(src, "python"), vec!["real"]);
    }

    #[test]
    fn hash_inside_a_python_string_is_not_a_comment() {
        let src = "color = \"#ff0000\"\n";
        assert!(scan(src, "python").is_empty());
    }

    #[test]
    fn unknown_languages_are_skipped() {
        assert!(scan("// comment\n", "plaintext").is_empty());
    }

    #[test]
    fn notice_caps_the_list_and_states_the_policy() {
        let src: String = (0..15).map(|i| format!("// note {i}\n")).collect();
        let notice = comment_notice("a.rs", &src, Path::new("a.rs")).unwrap();
        assert!(notice.contains("... 5 more"));
        assert_eq!(notice.matches("note ").count(), MAX_COMMENTS_PER_FILE);
        assert!(notice.contains("explain why"));
    }

    #[test]
    fn clean_file_yields_no_notice() {
        assert!(comment_notice("a.rs", "let x = 1;\n", Path::new("a.rs")).is_none());
    }

    #[test]
    fn shell_scripts_are_scanned_though_the_lsp_catalog_calls_them_plaintext() {
        assert_eq!(jcode_lsp::language_id(Path::new("a.sh")), "plaintext");
        let notice = comment_notice(
            "a.sh",
            "#!/bin/sh\n# set the flag\nx=1\n",
            Path::new("a.sh"),
        )
        .expect("shell comment reported");
        assert!(notice.contains("set the flag"));
    }

    #[test]
    fn extension_fallback_covers_both_syntax_families() {
        assert_eq!(syntax_for_path(Path::new("A.java")), Syntax::CStyle);
        assert_eq!(syntax_for_path(Path::new("a.rb")), Syntax::Hash);
        assert_eq!(syntax_for_path(Path::new("Cargo.toml")), Syntax::Hash);
        assert_eq!(syntax_for_path(Path::new("a.png")), Syntax::None);
    }

    #[test]
    fn extensionless_files_fall_back_to_the_file_name() {
        assert_eq!(syntax_for_path(Path::new("Dockerfile")), Syntax::Hash);
        assert_eq!(syntax_for_path(Path::new("dir/.gitignore")), Syntax::Hash);
    }

    #[test]
    fn the_lsp_catalog_still_wins_over_the_extension_table() {
        assert_eq!(syntax_for_path(Path::new("a.py")), Syntax::Hash);
        assert_eq!(syntax_for_path(Path::new("a.rs")), Syntax::CStyle);
    }

    #[test]
    fn css_stays_unscanned_so_protocol_relative_urls_are_not_comments() {
        assert_eq!(syntax_for_path(Path::new("a.css")), Syntax::None);
    }
}
