//! Lexical comment scanner behind the write/edit comment directive.
//!
//! Decisions and their evidence live in potb/jcode#49. The three that shape
//! this module: doc comments are never reported, the scan covers the whole
//! file rather than the diff, and the result is appended after the write has
//! already landed. The text is a directive rather than advice: a comment in
//! code is a defect report about the code or about where its documentation
//! lives, and the agent is expected to resolve it in the same session.
//!
//! The scanner is a hand-written lexer rather than a parser. It tracks string
//! and char literals only as far as needed to avoid reporting a `//` that
//! lives inside one, which is the only false positive that matters at this
//! resolution.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Longest file the scanner will look at. Above this the notice is skipped;
/// the cost is a silent miss on generated files, which is the right trade.
const MAX_SCAN_BYTES: usize = 1024 * 1024;

/// Reported comments per file, matching `MAX_WARNINGS_PER_FILE` in
/// `jcode-lsp`'s diagnostics formatter.
const MAX_COMMENTS_PER_FILE: usize = 10;

/// Reported doc blocks per file. Its own budget, so a file full of doc blocks
/// cannot crowd the plain-comment list out of the report.
const MAX_DOCS_PER_FILE: usize = 10;

/// Lines a doc block may occupy before it counts as prose rather than a
/// summary of an interface. Three lines hold a summary; a page of design
/// rationale is a document that lost its way into a header, which is the case
/// the report exists to catch.
const MAX_DOC_LINES: usize = 3;

/// A reportable doc block: where it starts, its first line as a preview, how
/// many lines it spans, and whether the item under it is reachable from
/// outside the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocSpan {
    pub line: usize,
    pub text: String,
    pub lines: usize,
    /// Documents an item nothing outside the file can call, so the prose is
    /// about the implementation and has no reader who cannot read the code.
    pub private: bool,
}

/// A reportable comment: 1-based line number and the trimmed source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentSpan {
    pub line: usize,
    pub text: String,
    /// Matches upstream's memo patterns: a comment describing a change rather
    /// than the code. Near-certain verdict, so the report marks it.
    pub memo: bool,
    /// The comment without its delimiter, which is what the prefix lists
    /// assume. Kept so a merged run can be reclassified as one comment.
    body: String,
    /// Whether the comment is the whole line rather than a trailing note after
    /// code. Only whole-line comments join a run: two trailing comments on
    /// adjacent lines belong to their own statements.
    own_line: bool,
}

/// Comment syntax family for a language id from `jcode_lsp::language_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    /// `//` and `/* */`, plus `///` and `//!` doc comments.
    CStyle,
    /// `#` only, plus `"""`/`'''` docstrings.
    Hash,
    /// `#`, but only at a word start, so `$#` and `${#a[@]}` stay code.
    Shell,
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
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "pl" | "pm" => Syntax::Shell,
        "rb" | "toml" | "nix" | "r" | "jl" | "ex" | "exs" | "tcl" | "awk" | "dockerfile" | "mk"
        | "makefile" | "cmake" | "gitignore" | "dockerignore" | "env" | "ini" | "cfg" | "conf"
        | "properties" => Syntax::Hash,
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

/// Scan the doc blocks in a C-style file and keep the ones worth reporting.
///
/// A doc block earns a report when it runs past [`MAX_DOC_LINES`], or when the
/// item under it is private. Both are the same failure the plain-comment
/// report exists for, wearing a delimiter that used to make it invisible: a
/// private header explains an implementation to a reader who is already
/// reading it, and a long one is a design note that belongs in a markdown
/// document.
///
/// A one-line summary on an exported item is never reported. That is what a
/// doc comment is for, and reporting it would make the check unusable in any
/// codebase with a documentation standard.
fn scan_docs(content: &str, syntax: Syntax) -> Vec<DocSpan> {
    if syntax != Syntax::CStyle || content.len() > MAX_SCAN_BYTES {
        return Vec::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let Some(len) = doc_block_len(&lines, i) else {
            i += 1;
            continue;
        };
        let item = lines[i + len..].iter().find(|l| !l.trim().is_empty());
        let private = item.is_some_and(|l| !is_exported_item(l));
        // A one-line summary is a naming aid wherever it sits, so only a
        // private block that has grown past that is prose about an
        // implementation.
        if len > MAX_DOC_LINES || (private && len > 1) {
            out.push(DocSpan {
                line: i + 1,
                text: lines[i].trim().to_string(),
                lines: len,
                private,
            });
        }
        i += len;
    }

    out
}

/// Length in lines of the doc block starting at `start`, or `None` when no
/// block starts there. Consecutive `///` and `//!` lines form one block, and a
/// `/** */` block runs to its terminator.
fn doc_block_len(lines: &[&str], start: usize) -> Option<usize> {
    let first = lines[start].trim_start();

    if first.starts_with("///") || first.starts_with("//!") {
        let len = lines[start..]
            .iter()
            .take_while(|l| {
                let t = l.trim_start();
                t.starts_with("///") || t.starts_with("//!")
            })
            .count();
        return Some(len);
    }

    if first.starts_with("/**") || first.starts_with("/*!") {
        // A `/**/` empty block is not a doc block, and a single-line
        // `/** x */` closes on its own line.
        if first.len() > 3 && first[2..].contains("*/") {
            return Some(1);
        }
        let len = lines[start..]
            .iter()
            .position(|l| l.contains("*/"))
            .map(|end| end + 1)
            .unwrap_or(lines.len() - start);
        return Some(len);
    }

    None
}

/// Whether the item a doc block sits on is reachable from outside its file.
///
/// Lexical and deliberately permissive: an item this misses is one report the
/// agent does not get, while a false positive would nag about a genuine public
/// interface. Rust `pub`, and the `export`/`declare` keywords that make a
/// TypeScript or JavaScript binding importable, all count as exported. Every
/// attribute, decorator, and modifier line is skipped first.
fn is_exported_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with('#') || t.starts_with('@') {
        return true;
    }
    t.starts_with("pub ")
        || t.starts_with("pub(")
        || t.starts_with("export ")
        || t.starts_with("export{")
        || t.starts_with("export default")
        || t.starts_with("declare ")
        || t.starts_with("module.exports")
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
    let spans = match syntax {
        Syntax::CStyle => scan_c_style(content),
        Syntax::Hash => scan_hash(content, Syntax::Hash),
        Syntax::Shell => scan_hash(content, Syntax::Shell),
        Syntax::Yaml => scan_hash(content, Syntax::Yaml),
        Syntax::None => Vec::new(),
    };
    merge_runs(spans)
}

/// Join runs of whole-line comments on consecutive lines into one span.
///
/// A wrapped paragraph is one comment that happens to be written across
/// several lines. Classifying each line on its own reads continuations as
/// independent comments, so a paragraph whose second line starts "removed
/// when the next prompt begins" is flagged as a memo on that line alone, and
/// a long paragraph can spend the whole per-file budget by itself.
///
/// The run is reported at its first line and keeps that line's text as the
/// preview; only the classification sees the joined body.
fn merge_runs(spans: Vec<CommentSpan>) -> Vec<CommentSpan> {
    let mut out: Vec<CommentSpan> = Vec::with_capacity(spans.len());
    // Last line absorbed into the open run, which is not the run's reported
    // line once it spans more than one.
    let mut run_end = 0usize;
    for span in spans {
        let joins = out
            .last()
            .is_some_and(|prev| prev.own_line && span.own_line && run_end + 1 == span.line);
        if joins {
            let prev = out.last_mut().expect("joins implies a previous span");
            prev.body.push(' ');
            prev.body.push_str(span.body.trim());
        } else {
            out.push(span.clone());
        }
        run_end = span.line;
    }
    for span in &mut out {
        span.memo = is_memo(&span.body);
    }
    out
}

/// Push a span when the comment body earns a report.
///
/// `text` is what the report shows (delimiter included); `body` is the same
/// comment with its delimiter stripped, which is what the prefix lists assume.
/// `own_line` says the comment occupies the whole line, which is what makes it
/// eligible to join a wrapped run.
fn push_if_reportable(
    out: &mut Vec<CommentSpan>,
    line: usize,
    text: &str,
    body: &str,
    own_line: bool,
) {
    if is_exempt(body) {
        return;
    }
    out.push(CommentSpan {
        line,
        text: text.trim().to_string(),
        memo: is_memo(body),
        body: body.trim().to_string(),
        own_line,
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
                    // Block bodies stay one report per line: a `/* */` block is
                    // already delimited, so its lines need no run detection.
                    push_if_reportable(&mut out, line, body, body, false);
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
                    let own_line = only_blanks_before(bytes, i);
                    push_if_reportable(&mut out, line, raw, raw.trim_start_matches('/'), own_line);
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

/// Whether only whitespace precedes `from` on its line, meaning the comment
/// starting there owns the line rather than trailing a statement.
fn only_blanks_before(bytes: &[u8], from: usize) -> bool {
    bytes[..from]
        .iter()
        .rev()
        .take_while(|&&b| b != b'\n')
        .all(|b| b.is_ascii_whitespace())
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Scan `#`-comment syntax in one of three dialects.
///
/// [`Syntax::Hash`] is Python's rule, where `x = 1#c` is a comment.
/// [`Syntax::Shell`] requires the `#` to start a word, so the parameter
/// expansions `$#` and `${#a[@]}` stay code. [`Syntax::Yaml`] adds that rule
/// plus literal block scalars, and has no docstrings.
fn scan_hash(content: &str, dialect: Syntax) -> Vec<CommentSpan> {
    let yaml = dialect == Syntax::Yaml;
    let require_word_start = dialect != Syntax::Hash;
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
        if dialect == Syntax::Hash {
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
        let start = hash_comment_start(raw_line, require_word_start);
        if yaml {
            let code = &raw_line[..start.unwrap_or(raw_line.len())];
            if ends_with_block_indicator(code) {
                block_indent = Some(indent_of(raw_line));
            }
        }
        if let Some(pos) = start {
            let body = &raw_line[pos + 1..];
            push_if_reportable(
                &mut out,
                line,
                body,
                body,
                raw_line[..pos].trim().is_empty(),
            );
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
/// `require_word_start` applies the shell and YAML rule that a `#` only opens
/// a comment at the start of a line or after whitespace, so `$#`, `${#a[@]}`
/// and a `.../#frag` URL are code. Python has no such rule: `x = 1#c` is a
/// comment.
fn hash_comment_start(line: &str, require_word_start: bool) -> Option<usize> {
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
                    if !require_word_start || prev.is_none_or(|p| p.is_ascii_whitespace()) {
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

/// How many times a directive has already been issued for a given file in this
/// process. A second report on the same file means the first one was read and
/// not acted on, which is the case worth escalating.
fn repeat_counts() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn bump_repeat(path: &Path) -> usize {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut counts = repeat_counts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let seen = counts.entry(key).or_insert(0);
    *seen += 1;
    *seen
}

/// Directive for a file that was just written. Mirrors `jcode-lsp`'s
/// `<diagnostics>` block shape, with one section per kind of finding.
///
/// `repeat` is 1 on the first report for a file and grows on every later one,
/// which is what selects the escalated wording.
fn directive_text(
    display_path: &str,
    spans: &[CommentSpan],
    docs: &[DocSpan],
    repeat: usize,
) -> String {
    let mut out = String::new();

    if !spans.is_empty() {
        let lines = capped(
            spans.iter().map(|s| {
                let mark = if s.memo { " (memo)" } else { "" };
                format!("{} {}{mark}", s.line, s.text)
            }),
            spans.len(),
            MAX_COMMENTS_PER_FILE,
        );
        out.push_str(&format!(
            "<comments file=\"{display_path}\">\n{lines}\n</comments>\n{DIRECTIVE}"
        ));
    }

    if !docs.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let lines = capped(
            docs.iter().map(|d| {
                let why = if d.private {
                    "private item"
                } else {
                    "over 3 lines"
                };
                format!("{} {} ({} lines, {why})", d.line, d.text, d.lines)
            }),
            docs.len(),
            MAX_DOCS_PER_FILE,
        );
        out.push_str(&format!(
            "<docs file=\"{display_path}\">\n{lines}\n</docs>\n{DOC_DIRECTIVE}"
        ));
    }

    if repeat > 1 {
        out.push_str(&format!(
            "\nThis is report {repeat} for this file: the earlier one was not acted on. Resolve these now, or state in your reply what you kept and the reason it cannot be code or a document."
        ));
    }

    out
}

/// Render up to `max` entries, with a trailing count when `total` exceeds it.
fn capped(entries: impl Iterator<Item = String>, total: usize, max: usize) -> String {
    let mut lines: Vec<String> = entries.take(max).collect();
    if total > max {
        lines.push(format!("... {} more", total - max));
    }
    lines.join("\n")
}

/// The standing rule the report carries. A comment in code is a defect signal,
/// so each one gets resolved rather than weighed: the code becomes clear
/// enough not to need it, the explanation moves to documentation, or the
/// comment goes away because it restated the code.
const DIRECTIVE: &str = "This file has comments. A comment in code is a signal that something is wrong: either the code is unclear, or documentation is in the wrong place. Resolve every comment listed above, in this order:\n\
1. Fix the code so the comment is unnecessary (name, split, or restructure until it reads as the comment did).\n\
2. Move the explanation where documentation belongs: a doc comment on the public item, or a markdown document such as a README, an architecture note, or an ADR. Doc comments are never reported here.\n\
3. Delete it when it restates the code, records what you changed, or is otherwise dead weight.\n\
Do this in the same session, and clean up pre-existing comments in the file, not only ones you added. Keep a comment only when none of the three applies, and say which one and why.";

/// The rule for doc blocks. A doc comment is the right home for an interface
/// summary and the wrong home for everything else, so the ones reported here
/// are the two shapes that are not a summary: prose long enough to be a
/// document, and documentation of an item no caller outside the file can see.
const DOC_DIRECTIVE: &str = "These doc comments are not interface summaries. A doc comment holds a short summary of something a caller cannot see; anything longer is a document, and one on a private item explains an implementation to a reader who is already reading it. Resolve each one:\n\
1. Move the prose to a markdown document (a README, an architecture note, or an ADR) and leave a one-line summary with a pointer to it.\n\
2. Fix the code so the explanation is unnecessary, especially on private items where the name and the shape can carry it.\n\
3. Delete it when it restates the signature.\n\
A short summary on an exported item is correct and is never reported.";

pub(crate) fn comment_notice(display_path: &str, content: &str, path: &Path) -> Option<String> {
    let syntax = syntax_for_path(path);
    let spans = scan_with(content, syntax);
    let docs = scan_docs(content, syntax);
    if spans.is_empty() && docs.is_empty() {
        return None;
    }
    Some(directive_text(
        display_path,
        &spans,
        &docs,
        bump_repeat(path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_for(content: &str, path: &Path) -> Vec<CommentSpan> {
        scan_with(content, syntax_for_path(path))
    }

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
        // Blank-separated so each is its own comment: adjacent lines would be
        // one wrapped paragraph and could never reach the cap.
        let src: String = (0..15).map(|i| format!("// note {i}\n\n")).collect();
        let notice = comment_notice("a.rs", &src, Path::new("a.rs")).unwrap();
        assert!(notice.contains("... 5 more"));
        assert_eq!(notice.matches("note ").count(), MAX_COMMENTS_PER_FILE);
        assert!(notice.contains("Resolve every comment listed above"));
    }

    #[test]
    fn the_directive_names_all_three_resolutions() {
        let spans = scan("// set the flag\n", "rust");
        let text = directive_text("a.rs", &spans, &[], 1);
        assert!(text.contains("Fix the code"));
        assert!(text.contains("markdown document"));
        assert!(text.contains("Delete it"));
        assert!(
            text.contains("pre-existing comments"),
            "the directive covers cleanup of the whole file: {text}"
        );
    }

    #[test]
    fn a_first_report_carries_no_escalation() {
        let spans = scan("// set the flag\n", "rust");
        assert!(!directive_text("a.rs", &spans, &[], 1).contains("not acted on"));
    }

    #[test]
    fn a_repeat_report_escalates() {
        let spans = scan("// set the flag\n", "rust");
        let text = directive_text("a.rs", &spans, &[], 2);
        assert!(text.contains("report 2 for this file"));
        assert!(text.contains("not acted on"));
    }

    fn docs(src: &str) -> Vec<DocSpan> {
        scan_docs(src, Syntax::CStyle)
    }

    #[test]
    fn a_short_doc_on_an_exported_item_is_not_reported() {
        assert!(docs("/// Adds one.\npub fn f() {}\n").is_empty());
        assert!(docs("/** Adds one. */\nexport function f() {}\n").is_empty());
        assert!(docs("/// Adds one.\n/// Twice, even.\npub fn f() {}\n").is_empty());
    }

    #[test]
    fn a_one_line_doc_on_a_private_item_is_not_reported() {
        assert!(docs("/// Adds one.\nfn f() {}\n").is_empty());
    }

    #[test]
    fn a_multi_line_doc_on_a_private_item_is_reported() {
        let found = docs("/// Adds one.\n/// Callers rely on the wrap.\nfn f() {}\n");
        assert_eq!(found.len(), 1);
        assert!(found[0].private);
        assert_eq!(found[0].line, 1);
    }

    #[test]
    fn a_long_doc_is_reported_even_on_an_exported_item() {
        let src = "/// One.\n/// Two.\n/// Three.\n/// Four.\npub fn f() {}\n";
        let found = docs(src);
        assert_eq!(found.len(), 1);
        assert!(!found[0].private);
        assert_eq!(found[0].lines, 4);
    }

    #[test]
    fn a_long_jsdoc_block_is_reported() {
        let src = "/**\n * Rekicks the dispatcher.\n * Returns how many triggers were issued.\n * One bad batch cannot abort the sweep.\n */\nexport function rekick() {}\n";
        let found = docs(src);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lines, 5);
        assert!(!found[0].private);
    }

    #[test]
    fn an_attribute_or_decorator_line_does_not_make_an_item_private() {
        let src = "/// One.\n/// Two.\n#[derive(Debug)]\npub struct S;\n";
        assert!(docs(src).is_empty());
    }

    #[test]
    fn module_docs_at_the_top_of_a_file_are_reported_when_long() {
        let src = "//! One.\n//! Two.\n//! Three.\n//! Four.\n\nuse std::fmt;\n";
        let found = docs(src);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lines, 4);
    }

    #[test]
    fn the_docs_section_names_its_reason_and_its_rule() {
        let src = "/// One.\n/// Two.\nfn f() {}\n";
        let text = directive_text("a.rs", &[], &docs(src), 1);
        assert!(text.contains("<docs file=\"a.rs\">"));
        assert!(text.contains("private item"));
        assert!(text.contains("markdown document"));
        assert!(!text.contains("<comments"));
    }

    #[test]
    fn a_long_doc_reports_its_length_as_the_reason() {
        let src = "/// One.\n/// Two.\n/// Three.\n/// Four.\npub fn f() {}\n";
        let text = directive_text("a.rs", &[], &docs(src), 1);
        assert!(text.contains("(4 lines, over 3 lines)"), "{text}");
    }

    #[test]
    fn both_sections_appear_together() {
        let src = "// set the flag\n/// One.\n/// Two.\nfn f() {}\n";
        let text = directive_text("a.rs", &scan(src, "rust"), &docs(src), 1);
        assert!(text.contains("<comments"));
        assert!(text.contains("<docs"));
    }

    #[test]
    fn a_file_with_only_long_docs_still_produces_a_report() {
        let src = "/// One.\n/// Two.\nfn f() {}\n";
        let dir = std::env::temp_dir().join(format!("jcode-cc-docs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("docs_only.rs");
        std::fs::write(&path, src).expect("write");

        let report = comment_notice("docs_only.rs", src, &path).expect("a report");
        assert!(report.contains("<docs"));
        assert!(!report.contains("<comments"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_docs_list_is_capped() {
        let src: String = (0..15)
            .map(|i| format!("/// One {i}.\n/// Two.\nfn f{i}() {{}}\n\n"))
            .collect();
        let text = directive_text("a.rs", &[], &docs(&src), 1);
        assert!(text.contains("... 5 more"), "{text}");
    }

    #[test]
    fn docs_are_only_scanned_in_c_style_files() {
        assert!(scan_docs("### One.\n### Two.\ndef f():\n    pass\n", Syntax::Hash).is_empty());
    }

    #[test]
    fn an_unterminated_jsdoc_block_does_not_hang_or_overrun() {
        let found = docs("/**\n * One.\n * Two.\n * Three.\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lines, 4, "the block runs to the end of the file");
        assert!(!found[0].private, "no item follows an unterminated block");
    }

    #[test]
    fn writing_the_same_file_again_escalates() {
        let dir = std::env::temp_dir().join(format!("jcode-cc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("repeat.rs");
        std::fs::write(&path, "// set the flag\n").expect("write");

        let first = comment_notice("repeat.rs", "// set the flag\n", &path).expect("a directive");
        let second = comment_notice("repeat.rs", "// set the flag\n", &path).expect("a directive");

        assert!(!first.contains("not acted on"));
        assert!(second.contains("report 2 for this file"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn clean_file_yields_no_notice() {
        assert!(comment_notice("a.rs", "let x = 1;\n", Path::new("a.rs")).is_none());
    }

    #[test]
    fn shell_parameter_expansions_are_not_comments() {
        let src = "if [ $# -lt 2 ]; then\n  echo \"n=${#args[@]}\"\nfi\n";
        assert!(spans_for(src, Path::new("deploy.sh")).is_empty());
    }

    #[test]
    fn shell_comments_are_still_found() {
        let src = "cp a b # keep the original\n# why this runs twice\nrun\n";
        assert_eq!(
            spans_for(src, Path::new("deploy.sh"))
                .into_iter()
                .map(|s| s.line)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn perl_and_powershell_use_the_shell_rule() {
        assert_eq!(syntax_for_path(Path::new("a.pl")), Syntax::Shell);
        assert_eq!(syntax_for_path(Path::new("a.ps1")), Syntax::Shell);
        assert!(spans_for("my $n = $#list;\n", Path::new("a.pl")).is_empty());
    }

    #[test]
    fn python_keeps_its_no_space_rule() {
        assert_eq!(syntax_for_path(Path::new("a.py")), Syntax::Hash);
        assert_eq!(texts("x = 1#c\n", "python"), vec!["c"]);
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
        let spans = scan(src, "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].line, 2);
        assert_eq!(spans[0].text, "// after");
        assert!(!spans[0].memo);
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

    #[test]
    fn wrapped_paragraph_is_one_comment_reported_at_its_first_line() {
        let src = "// The cache is keyed by tenant so a lookup\n// removed when the next prompt begins cannot\n// leak across restaurants.\nlet x = 1;\n";
        let spans = scan(src, "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].line, 1);
        assert_eq!(spans[0].text, "// The cache is keyed by tenant so a lookup");
    }

    #[test]
    fn a_continuation_line_no_longer_reads_as_a_memo() {
        let src = "// The cache is keyed by tenant so a lookup\n// removed when the next prompt begins cannot\n// leak across restaurants.\n";
        assert!(!scan(src, "rust")[0].memo);
    }

    #[test]
    fn a_run_that_really_is_a_memo_is_still_flagged() {
        let src = "// removed the legacy path because nothing\n// called it any more.\n";
        assert!(scan(src, "rust")[0].memo);
    }

    #[test]
    fn a_blank_line_separates_two_comments() {
        let src = "// first paragraph\n\n// second paragraph\n";
        let spans = scan(src, "rust");
        assert_eq!(spans.iter().map(|s| s.line).collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn trailing_comments_on_adjacent_lines_stay_separate() {
        let src = "let a = 1; // first\nlet b = 2; // second\n";
        assert_eq!(scan(src, "rust").len(), 2);
    }

    #[test]
    fn a_wrapped_run_costs_one_entry_of_the_per_file_budget() {
        let src = (1..=30)
            .map(|i| format!("// wrapped line {i}\n"))
            .collect::<String>();
        let notice = comment_notice("a.rs", &src, Path::new("a.rs")).expect("a notice");
        assert!(!notice.contains("more"), "{notice}");
    }

    #[test]
    fn python_comment_runs_merge_too() {
        let src = "# the loop keeps the last seen row\n# removed rows are skipped\n";
        let spans = scan(src, "python");
        assert_eq!(spans.len(), 1);
        assert!(!spans[0].memo);
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
