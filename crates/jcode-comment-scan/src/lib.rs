//! Advisory comment scanner for jcode.
//!
//! Public surface:
//! - [`configure`] / [`is_enabled`] — process-global config, like `jcode-fmt`.
//! - [`scan`] — extract reportable comments from source text.
//! - [`comments_block`] — render a `<comments>` block for a tool result.
//!
//! Doc comments (`///`, `//!`, `/** */`, Python docstrings) are always exempt:
//! the policy targets comments that restate code, not API documentation.
//!
//! Every public function is total: a malformed or unsupported file yields an
//! empty result rather than an error.

pub mod config_compat;

mod c_family;
mod filters;
mod format;
mod hash_family;

use std::sync::{LazyLock, RwLock};

pub use config_compat::CommentCheckConfig;
pub use format::{MAX_COMMENTS_PER_FILE, comments_block};

/// Upper bound on the reported text of one comment, in characters.
const MAX_TEXT_CHARS: usize = 200;

/// A comment as produced by a language lexer, before filtering.
pub(crate) struct RawComment {
    /// 1-based line where the comment starts.
    pub line: usize,
    /// Comment source, delimiters included, collapsed to one line.
    pub text: String,
    /// Doc comment (`///`, `//!`, `/** */`, Python docstring).
    pub is_doc: bool,
}

/// A comment worth reporting to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentSpan {
    /// 1-based line where the comment starts.
    pub line: usize,
    /// Comment source, delimiters included, collapsed to one line.
    pub text: String,
    /// Reads like a note about a change rather than an explanation.
    pub is_memo: bool,
}

static CONFIG: LazyLock<RwLock<CommentCheckConfig>> =
    LazyLock::new(|| RwLock::new(CommentCheckConfig::default()));

/// Store the process-global `[comment_check]` config. Call once at startup
/// (and again on config reload).
pub fn configure(cfg: CommentCheckConfig) {
    if let Ok(mut guard) = CONFIG.write() {
        *guard = cfg;
    }
}

/// True when the config's master switch is enabled.
pub fn is_enabled() -> bool {
    CONFIG.read().map(|g| g.enabled).unwrap_or(true)
}

/// True when [`scan`] understands this `jcode_lsp::language_id`. Lets callers
/// skip reading the file at all for unsupported languages.
pub fn supports_language(language_id: &str) -> bool {
    matches!(
        language_id,
        "rust"
            | "typescript"
            | "typescriptreact"
            | "javascript"
            | "javascriptreact"
            | "go"
            | "c"
            | "cpp"
            | "objective-c"
            | "objective-cpp"
            | "jsonc"
            | "python"
            | "yaml"
    )
}

/// Extract reportable comments from `content`, interpreted as `language_id`
/// (the ids produced by `jcode_lsp::language_id`). Doc comments, lint
/// directives, BDD markers, shebangs, and separator rules are filtered out.
/// Unsupported languages yield an empty vector.
pub fn scan(content: &str, language_id: &str) -> Vec<CommentSpan> {
    let raw = match language_id {
        "rust" | "typescript" | "typescriptreact" | "javascript" | "javascriptreact" | "go"
        | "c" | "cpp" | "objective-c" | "objective-cpp" | "jsonc" => c_family::scan(content),
        "python" => hash_family::scan_python(content),
        "yaml" => hash_family::scan_yaml(content),
        _ => return Vec::new(),
    };
    merge_line_runs(content, raw)
        .into_iter()
        .filter(|c| !c.is_doc && !filters::is_exempt(&c.text))
        .map(|c| CommentSpan {
            line: c.line,
            is_memo: filters::is_agent_memo(&c.text),
            text: c.text,
        })
        .collect()
}

/// Join runs of line comments on consecutive lines into a single span, so a
/// wrapped paragraph is classified and reported as the one comment it is.
///
/// Only comments that own their whole line merge. A comment trailing code is
/// a remark about that line, so joining it to its neighbour would invent a
/// sentence neither of them says.
fn merge_line_runs(content: &str, raw: Vec<RawComment>) -> Vec<RawComment> {
    let line_starts_with_comment: Vec<bool> = content
        .lines()
        .map(|line| is_line_comment(line.trim_start()))
        .collect();
    let owns_line = |line: usize| {
        line.checked_sub(1)
            .and_then(|index| line_starts_with_comment.get(index))
            .copied()
            .unwrap_or(false)
    };

    let mut merged: Vec<RawComment> = Vec::with_capacity(raw.len());
    let mut run_end_line = 0usize;
    for comment in raw {
        let previous = merged.last_mut();
        let continues = previous.as_ref().is_some_and(|prev| {
            is_line_comment(&prev.text)
                && is_line_comment(&comment.text)
                && prev.is_doc == comment.is_doc
                && comment.line == run_end_line + 1
                && owns_line(run_end_line)
                && owns_line(comment.line)
        });
        run_end_line = comment.line;
        match previous {
            Some(prev) if continues => {
                prev.text.push(' ');
                prev.text
                    .push_str(strip_line_delimiter(comment.text.trim()));
                prev.text = truncate_chars(&prev.text, MAX_TEXT_CHARS);
            }
            _ => merged.push(comment),
        }
    }
    merged
}

fn is_line_comment(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("//") || text.starts_with('#')
}

fn strip_line_delimiter(text: &str) -> &str {
    let rest = text
        .strip_prefix("//")
        .or_else(|| text.strip_prefix('#'))
        .unwrap_or(text);
    rest.strip_prefix(' ').unwrap_or(rest)
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_language_yields_nothing() {
        assert!(scan("// hi", "plaintext").is_empty());
        assert!(scan("// hi", "json").is_empty());
    }

    #[test]
    fn doc_comments_are_never_reported() {
        let spans = scan("/// documented\nfn f() {}\n", "rust");
        assert!(spans.is_empty());
    }

    #[test]
    fn plain_comment_is_reported_with_line() {
        let spans = scan("fn f() {}\n// restates the code\n", "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].line, 2);
        assert_eq!(spans[0].text, "// restates the code");
        assert!(!spans[0].is_memo);
    }

    #[test]
    fn memo_comment_is_flagged() {
        let spans = scan("// Changed from regex to a parser.\n", "rust");
        assert_eq!(spans.len(), 1);
        assert!(spans[0].is_memo);
    }

    #[test]
    fn directives_and_bdd_markers_are_exempt() {
        let spans = scan("// clippy: allow\n// given\n// noqa\n", "rust");
        assert!(spans.is_empty());
    }

    #[test]
    fn python_docstring_is_exempt_but_hash_comment_is_not() {
        let spans = scan("\"\"\"module doc\"\"\"\nx = 1  # restates\n", "python");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].line, 2);
    }

    #[test]
    fn yaml_comment_is_reported() {
        let spans = scan("key: value # why\n", "yaml");
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn merges_consecutive_line_comments() {
        let spans = scan("// first line\n// second line\n", "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].line, 1);
        assert_eq!(spans[0].text, "// first line second line");
    }

    #[test]
    fn merges_three_consecutive_line_comments() {
        let spans = scan("// one\n// two\n// three\n", "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "// one two three");
    }

    #[test]
    fn blank_line_prevents_merging() {
        let spans = scan("// first line\n\n// second line\n", "rust");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].line, 1);
        assert_eq!(spans[1].line, 3);
    }

    #[test]
    fn block_comment_never_merges() {
        assert_eq!(scan("/* first */\n// second\n", "rust").len(), 2);
        assert_eq!(scan("// first\n/* second */\n", "rust").len(), 2);
    }

    #[test]
    fn doc_comment_never_merges_with_non_doc() {
        let spans = scan("/// documented\n// plain\n", "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "// plain");
        assert_eq!(spans[0].line, 2);
    }

    #[test]
    fn merged_text_is_truncated_at_200_chars() {
        let long = format!("// {}\n// {}\n", "a".repeat(150), "b".repeat(150));
        let spans = scan(&long, "rust");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text.chars().count(), 200);
    }

    #[test]
    fn merges_consecutive_hash_comments() {
        let spans = scan("# first line\n# second line\n", "python");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "# first line second line");
    }

    #[test]
    fn continuation_line_is_not_classified_alone() {
        let spans = scan(
            "// the region is\n// removed when the prompt begins.\n",
            "rust",
        );
        assert_eq!(spans.len(), 1);
        assert!(!spans[0].is_memo);
    }

    #[test]
    fn config_toggle_round_trips() {
        configure(CommentCheckConfig { enabled: false });
        assert!(!is_enabled());
        configure(CommentCheckConfig::default());
        assert!(is_enabled());
    }
}
