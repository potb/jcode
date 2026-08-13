# Comment check implementation plan (issue #49)

State file for the in-tree comment checker. Decisions are recorded on
<https://github.com/potb/jcode/issues/49#issuecomment-5285465744>; this file is the
implementation plan derived from them.

## Decisions (fixed, do not relitigate)

1. **Engine**: in-tree lexical scanner. No tree-sitter, no new third-party dependency.
2. **Mode**: advisory. Appends to tool output, never blocks a call.
3. **Docstrings**: always exempt (`///`, `//!`, `/** */`, Python `"""`). Lexed for
   correctness, never reported.
4. **Scope**: whole-file scan, not diff-based. Pre-existing comments are re-reported.
5. **Config**: single global `[comment_check] enabled`, default true. No per-project layer.
6. **Message**: our own, mirroring `crates/jcode-lsp/src/format.rs`. Capped at 10 entries.
7. **Policy**: no non-doc comments by default. `AGENTS.md` gains the written rule.

## Target shape

New crate `crates/jcode-comment-scan`, modelled on `crates/jcode-fmt`:

```
crates/jcode-comment-scan/
  Cargo.toml          # publish = false, no third-party deps beyond serde if needed
  src/lib.rs          # configure / is_enabled / scan entry point
  src/scan.rs         # the lexers
  src/format.rs       # <comments> block rendering
  src/memo.rs         # agent-memo pattern classification
```

### Public API

```rust
pub fn configure(config: CommentCheckConfig);
pub fn is_enabled() -> bool;
pub fn scan(content: &str, language_id: &str) -> Vec<CommentSpan>;
pub fn comments_block(display_path: &str, spans: &[CommentSpan]) -> Option<String>;

pub struct CommentSpan {
    pub line: usize,      // 1-based
    pub text: String,     // trimmed comment text, delimiters included
    pub is_memo: bool,    // matches an agent-memo pattern
}
```

`scan` returns only reportable comments: doc comments, lint directives, shebangs,
and BDD markers are filtered out inside the scanner.

## Language handling

Key off `jcode_lsp::language_id(path)`, which already maps every supported
extension. Three families:

| family | language_id values | delimiters |
| --- | --- | --- |
| C-family | rust, typescript, typescriptreact, javascript, javascriptreact, go, c, cpp, objective-c, objective-cpp, jsonc | `//`, `/* */` |
| hash-family | python, yaml | `#`, plus Python `"""` docstrings |
| none | json, plaintext | no scan |

### Correctness requirements for the lexer

- Track string literals so `let s = "// x";` and `#` inside a quoted YAML scalar
  do not register as comments.
- Rust raw strings: `r"..."`, `r#"..."#`, `r##"..."##` (arbitrary hash count).
- Rust nested block comments: `/* /* */ */` requires depth counting.
- Escape sequences inside ordinary strings (`"\\"` must not swallow the closing quote).
- Python: single and triple quoted strings, both `'` and `"` flavours, raw and
  f-string prefixes. A docstring is a triple-quoted string in statement position;
  treat every triple-quoted string as a docstring for exemption purposes.
- Go and C have no raw-string nesting concerns beyond backticks in Go.
- Never panic on malformed input. Unterminated string or block comment means the
  scan stops cleanly at end of input.

### Exemptions (not reported)

- Doc comments: `///`, `//!`, `/** ... */`, `#!` shebang, Python `"""`.
- Lint directives: a comment whose first token matches `noqa`, `type:`, `pyright:`,
  `ruff:`, `mypy:`, `pylint:`, `flake8:`, `eslint-disable`, `eslint-ignore`,
  `prettier-ignore`, `ts-ignore`, `ts-expect-error`, `clippy:`, `allow`, `deny`,
  `warn`, `forbid`, `rustfmt:`, `SAFETY:`.
- BDD markers: comment text equal to `given`, `when`, `then`, `arrange`, `act`,
  `assert`, `when & then`, case-insensitive.

### Memo classification

Port the intent of upstream `filters.rs::is_english_agent_memo`, corrected for the
false positives measured in this tree. Match only when the comment reads as a note
about a change rather than a sentence that happens to start with the same word:

- `added`, `removed`, `deleted`, `refactored`, `replaced`, `implemented` followed by
  end of comment or an object phrase.
- `changed`/`modified`/`moved`/`migrated`/`renamed`/`converted` followed by `from` or `to`.
- `now we`, `now this`, `here we`, `previously`, `note:`, `implementation of`.

Explicitly do **not** match a bare sentence-initial `Now ...`, which produced most of
the 143 matches measured across 24,281 comments and was almost entirely false positives.

## Output format

Mirrors `crates/jcode-lsp/src/format.rs`:

```
<comments file="crates/jcode-app-core/src/tool/webfetch.rs">
270 // HTML comments frequently contain build metadata, conditional markup, and
318 // strip before extracting text
MEMO 402 // changed from regex to parser
</comments>
This file has comments. Remove ones that restate the code. Keep them only where they explain why.
```

- Cap at 10 entries, then `... N more comments`, matching `MAX_WARNINGS_PER_FILE`.
- Memo-classified entries are prefixed `MEMO` so the near-certain cases stand out.
- Returns `None` when there is nothing to report.
- No emoji, no capitalised exhortations.

## Integration

`crates/jcode-app-core/src/tool/lsp_feedback.rs` gains a sibling of
`diagnostics_after_write`:

```rust
pub(crate) async fn comment_notice_after_write(path: &Path) -> Option<String>;
```

It configures the crate from `crate::config::config().comment_check`, short-circuits
on `!is_enabled()`, reads the file with the same guards `read_text_for_lsp` uses
(skip missing, non-UTF8, oversized, binary), resolves the language via
`jcode_lsp::language_id`, scans, and renders.

Call sites, appended after the existing diagnostics call so ordering is
format, diagnostics, comments:

- `crates/jcode-app-core/src/tool/write.rs` around line 139
- `crates/jcode-app-core/src/tool/edit.rs` around line 160
- `crates/jcode-app-core/src/tool/multiedit.rs` around line 195
- `crates/jcode-app-core/src/tool/apply_patch.rs` inside the existing
  `DIAGNOSTICS_MAX_FILES` / `DIAGNOSTICS_TOTAL_BUDGET` loop. The scan is pure CPU
  with no timeout risk, so it goes inside the loop but does not need its own
  `tokio::time::timeout` wrapper.

## Config

`crates/jcode-config-types/src/lib.rs`, next to `FormatterConfig`:

```rust
/// Comment check integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CommentCheckConfig {
    /// Master switch. Default true.
    #[serde(default = "default_true")]
    pub enabled: bool,
}
```

Wired into `Config` in `crates/jcode-base/src/config.rs` beside `pub formatter`,
with `#[serde(default)]`.

## Work order

1. Create the crate skeleton, add to workspace members in the root `Cargo.toml`
   and as a dependency of `jcode-app-core`.
2. Implement the C-family lexer with its unit tests.
3. Implement the hash-family lexer with its unit tests.
4. Implement exemptions and memo classification with unit tests.
5. Implement `comments_block` rendering with unit tests.
6. Add `CommentCheckConfig` and wire it into `Config`.
7. Add `comment_notice_after_write` and the four call sites.
8. Add the `AGENTS.md` policy line.
9. Run the full test suite plus the repo guardrails, then commit.

## Verification

- Unit tests must cover, at minimum: comment-like text inside an ordinary string,
  inside a Rust raw string with hashes, nested block comments, an unterminated
  block comment, a Python triple-quoted docstring, `#` inside a YAML quoted scalar,
  every exemption category, each memo pattern, and the cap plus `... N more`.
- Behavioural check on real files, since counts are already known:
  `crates/jcode-app-core/src/tool/write.rs` must report 12,
  `crates/jcode-app-core/src/tool/webfetch.rs` 14,
  `crates/jcode-lsp/src/format.rs` 1,
  `crates/jcode-app-core/src/tool/lsp_feedback.rs` 0.
  Any deviation means the lexer or the exemption set is wrong.
- Whole-tree sanity: scanning every file under `crates/` must report close to
  24,281 comments in total and must never panic.
- `cargo fmt`, `cargo clippy`, and the ratchet gates must stay green.
