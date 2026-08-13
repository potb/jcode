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
    /// `#`, but only at line start or after whitespace, plus block scalars.
    Yaml,
    /// No comment syntax worth scanning.
    None,
}

fn syntax_for(language_id: &str) -> Syntax {
    match language_id {
        "rust" | "typescript" | "typescriptreact" | "javascript" | "javascriptreact" | "go"
        | "c" | "cpp" | "objective-c" | "objective-cpp" | "jsonc" => Syntax::CStyle,
        "python" => Syntax::Hash,
        "yaml" => Syntax::Yaml,
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
    "was changed",
    "previously",
    "note to self",
    "todo(agent)",
];

/// Subjects that make a leading `now` a note about an edit rather than prose.
const MEMO_NOW_SUBJECTS: &[&str] = &["we", "this", "it"];

/// Verbs that, after a [`MEMO_NOW_SUBJECTS`] word, describe a change.
const MEMO_NOW_VERBS: &[&str] = &[
    "use", "uses", "used", "call", "calls", "return", "returns", "do", "does", "is", "are", "has",
    "have", "also", "only",
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
    MEMO_PREFIXES.iter().any(|p| lower.starts_with(p)) || is_now_memo(&lower)
}

/// A bare `now` prefix matches ordinary prose ("Now that the config is
/// loaded, ..."), so it only counts with a subject and a change verb behind it.
fn is_now_memo(lower: &str) -> bool {
    let Some(rest) = lower.strip_prefix("now ") else {
        return false;
    };
    let mut words = rest.split_whitespace();
    let Some(subject) = words.next() else {
        return false;
    };
    if !MEMO_NOW_SUBJECTS.contains(&subject) {
        return false;
    }
    let Some(verb) = words.next() else {
        return false;
    };
    let verb = verb.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    MEMO_NOW_VERBS.contains(&verb)
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
        Syntax::Hash => scan_hash(content, false),
        Syntax::Yaml => scan_hash(content, true),
        Syntax::None => Vec::new(),
    }
}

/// Push a span when the comment body earns a report.
///
/// `text` is what the report shows (delimiter included); `body` is the same
/// comment with its delimiter stripped, which is what the prefix lists assume.
fn push_if_reportable(out: &mut Vec<CommentSpan>, line: usize, text: &str, body: &str) {
    if is_exempt(body) {
        return;
    }
    out.push(CommentSpan {
        line,
        text: text.trim().to_string(),
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
                    push_if_reportable(&mut out, line, body, body);
                }
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(quote) = in_string {
            if b == b'\\' {
                // An escaped newline is a line continuation: the literal stays
                // open across it, but the line still has to be counted.
                if bytes.get(i + 1) == Some(&b'\n') {
                    line += 1;
                }
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
                    push_if_reportable(&mut out, line, raw, raw.trim_start_matches('/'));
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

/// Scan `#`-comment syntax. `yaml` selects the YAML dialect: a `#` only opens
/// a comment at line start or after whitespace, block scalars are literal
/// text, and there are no docstrings.
fn scan_hash(content: &str, yaml: bool) -> Vec<CommentSpan> {
    let mut out = Vec::new();
    let mut in_docstring: Option<&str> = None;
    let mut block_indent: Option<usize> = None;

    for (idx, raw_line) in content.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw_line.trim_start();

        // A block scalar body is literal text, so it ends only on a line that
        // dedents back to the indicator's own indentation.
        if let Some(indent) = block_indent {
            if trimmed.is_empty() || indent_of(raw_line) > indent {
                continue;
            }
            block_indent = None;
        }
        if !yaml {
            if let Some(delim) = in_docstring {
                if trimmed.contains(delim) {
                    in_docstring = None;
                }
                continue;
            }
            if let Some(delim) = triple_quote_on_line(raw_line) {
                // An odd count leaves the string open past the end of the line.
                if raw_line.matches(delim).count() % 2 == 1 {
                    in_docstring = Some(delim);
                }
                continue;
            }
        }
        if line == 1 && trimmed.starts_with("#!") {
            continue;
        }
        let start = hash_comment_start(raw_line, yaml);
        if yaml {
            let code = &raw_line[..start.unwrap_or(raw_line.len())];
            if ends_with_block_indicator(code) {
                block_indent = Some(indent_of(raw_line));
            }
        }
        if let Some(pos) = start {
            let body = &raw_line[pos + 1..];
            push_if_reportable(&mut out, line, body, body);
        }
    }
    out
}

/// The triple-quote delimiter a Python line opens or closes, if any. Unlike a
/// docstring proper this also catches `x = """...`, whose body is still a
/// string.
fn triple_quote_on_line(line: &str) -> Option<&'static str> {
    ["\"\"\"", "'''"]
        .into_iter()
        .filter(|d| line.contains(d))
        .min_by_key(|d| line.find(d).unwrap_or(usize::MAX))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Whether a YAML line ends in a `|`/`>` block scalar indicator, including the
/// chomping and explicit-indentation forms (`|-`, `>+`, `|2`).
fn ends_with_block_indicator(code: &str) -> bool {
    let head = code
        .trim_end()
        .trim_end_matches(|c: char| c.is_ascii_digit() || c == '+' || c == '-');
    head.ends_with('|') || head.ends_with('>')
}

/// Byte offset of a `#` that starts a comment, skipping ones inside quotes.
///
/// `require_space` applies YAML's rule that a `#` only opens a comment at the
/// start of a line or after whitespace, so `.../#frag` in a URL is not one.
/// Python has no such rule: `x = 1#c` is a comment.
fn hash_comment_start(line: &str, require_space: bool) -> Option<usize> {
    let mut in_string: Option<u8> = None;
    let mut escaped = false;
    let mut prev: Option<u8> = None;
    for (i, b) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
            prev = Some(b);
            continue;
        }
        match in_string {
            Some(q) => {
                if b == b'\\' {
                    escaped = true;
                    prev = Some(b);
                    continue;
                }
                if b == q {
                    in_string = None;
                }
            }
            None => match b {
                b'"' | b'\'' => in_string = Some(b),
                b'#' => {
                    if !require_space || prev.is_none_or(|p| p.is_ascii_whitespace()) {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        prev = Some(b);
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
        assert_eq!(texts(src, "rust"), vec!["// increment i"]);
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
        assert_eq!(texts(src, "rust"), vec!["// real comment"]);
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

    #[test]
    fn line_comment_text_keeps_its_delimiter() {
        let src = "let x = 1; // trailing\n";
        assert_eq!(texts(src, "rust"), vec!["// trailing"]);
    }

    #[test]
    fn escaped_backslash_still_closes_the_string_on_its_line() {
        let src = "let s = \"ends \\\\\";\n// after\n";
        assert_eq!(
            scan(src, "rust"),
            vec![CommentSpan {
                line: 2,
                text: "// after".to_string(),
                memo: false,
            }]
        );
    }

    #[test]
    fn yaml_hash_after_whitespace_is_still_a_comment() {
        let src = "key: value # note\n# standalone\n";
        assert_eq!(texts(src, "yaml"), vec!["note", "standalone"]);
    }

    #[test]
    fn python_hash_without_leading_space_is_a_comment() {
        assert_eq!(texts("x = 1#c\n", "python"), vec!["c"]);
    }

    #[test]
    fn yaml_comment_after_a_block_scalar_dedents_is_found() {
        let src = "script: |-\n  echo hi # inner\n\n  more\nnext: 1 # real\n";
        assert_eq!(texts(src, "yaml"), vec!["real"]);
    }

    #[test]
    fn now_with_a_change_verb_is_still_a_memo() {
        let spans = scan("// now we use the pooled client\n", "rust");
        assert!(spans[0].memo);
    }
}

#[cfg(test)]
mod differential_tests {
    use super::*;

    fn spans(content: &str, lang: &str) -> Vec<(usize, String)> {
        scan(content, lang)
            .into_iter()
            .map(|s| (s.line, s.text))
            .collect()
    }

    #[test]
    fn diff_raw_string_with_hashes_is_not_a_comment() {
        let src = "let r = r#\"has \"quotes\" and // slashes\"#;\n";
        assert_eq!(spans(src, "rust"), Vec::<(usize, String)>::new());
    }

    #[test]
    fn diff_lifetime_does_not_open_a_char_literal() {
        let src = "fn f<'a>(x: &'a str) {} // trailing\n";
        assert_eq!(spans(src, "rust").len(), 1);
    }

    #[test]
    fn diff_nested_block_comment_terminates() {
        let src = "/* outer /* inner */ still outer */\nlet x = 1;\n// after\n";
        let got = spans(src, "rust");
        assert_eq!(got.last().map(|s| s.0), Some(3));
    }

    #[test]
    fn diff_string_line_continuation_keeps_lines_aligned() {
        let src = "let s = \"one \\\ntwo\";\n// after\n";
        assert_eq!(spans(src, "rust"), vec![(3, "// after".to_string())]);
    }

    #[test]
    fn diff_yaml_url_fragment_is_not_a_comment() {
        let src = "url: http://example.com/#frag\n";
        assert_eq!(spans(src, "yaml"), Vec::<(usize, String)>::new());
    }

    #[test]
    fn diff_yaml_block_scalar_body_is_not_a_comment() {
        let src = "script: |\n  echo hi # not a comment\nnext: 1\n";
        assert_eq!(spans(src, "yaml"), Vec::<(usize, String)>::new());
    }

    #[test]
    fn diff_python_raw_string_then_real_comment() {
        let src = "x = r\"\\\"\"\n# real\n";
        assert_eq!(spans(src, "python").len(), 1);
    }

    #[test]
    fn diff_note_that_is_not_a_memo() {
        let flagged = scan("// Note that the range is inclusive.\n", "rust")
            .into_iter()
            .any(|s| s.memo);
        assert!(!flagged, "explanatory 'Note that' should not be a memo");
    }

    #[test]
    fn diff_now_that_is_not_a_memo() {
        let flagged = scan(
            "// Now that the config is loaded, the server starts.\n",
            "rust",
        )
        .into_iter()
        .any(|s| s.memo);
        assert!(!flagged, "explanatory 'Now that' should not be a memo");
    }

    #[test]
    fn diff_was_the_word_is_not_a_memo() {
        let flagged = scan("// was the previous owner of the lock\n", "rust")
            .into_iter()
            .any(|s| s.memo);
        assert!(!flagged);
    }
}
#[cfg(test)]
mod real_file_line_accuracy {
    use super::*;

    fn check(path: &str, lang: &str) -> Vec<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root");
        let content = std::fs::read_to_string(root.join(path)).expect("read");
        let lines: Vec<&str> = content.lines().collect();
        let mut bad = Vec::new();
        for s in scan(&content, lang) {
            let actual = lines.get(s.line - 1).map(|l| l.trim()).unwrap_or("<oob>");
            let head: String = s.text.trim().chars().take(20).collect();
            if !actual.contains(head.trim()) {
                bad.push(format!(
                    "line {} span={:?} actual={:?}",
                    s.line, head, actual
                ));
            }
        }
        bad
    }

    #[test]
    fn line_numbers_match_real_repository_files() {
        for path in [
            "crates/jcode-app-core/src/ambient/prompt.rs",
            "crates/jcode-tui/src/tui/app/tests/swarm_plan_graph_inline.rs",
            "crates/jcode-app-core/src/tool/comment_check.rs",
            "crates/jcode-app-core/src/tool/write.rs",
            "crates/jcode-lsp/src/lib.rs",
        ] {
            let bad = check(path, "rust");
            assert!(bad.is_empty(), "{path}: {bad:#?}");
        }
    }
}
