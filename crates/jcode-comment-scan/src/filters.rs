//! Classification of raw comments into exempt, agent-memo, and reportable.

const BDD_KEYWORDS: &[&str] = &[
    "given",
    "when",
    "then",
    "arrange",
    "act",
    "assert",
    "when & then",
    "when&then",
];

/// Directives matched as a plain case-insensitive prefix.
const DIRECTIVE_PREFIXES: &[&str] = &[
    "type:",
    "noqa",
    "pyright:",
    "ruff:",
    "mypy:",
    "pylint:",
    "flake8:",
    "pyre:",
    "pytype:",
    "eslint-disable",
    "eslint-ignore",
    "prettier-ignore",
    "ts-ignore",
    "ts-expect-error",
    "clippy:",
    "rustfmt:",
    "safety:",
    "cbindgen:",
    "codegen",
    "nolint",
    "go:",
    "#[",
    "biome-ignore",
];

/// Directives that are only directives when immediately followed by `(`, `:`,
/// or the end of the comment, so prose like "allows the caller" stays reported.
const DELIMITED_DIRECTIVES: &[&str] = &["allow", "deny", "warn", "forbid"];

const SEPARATOR_CHARS: &[char] = &['-', '=', '*', '_', '#', '/', '~'];

/// Verbs that make a leading "now we / now this / now it" read as a note about
/// a change rather than an explanation of the code.
const NOW_VERBS: &[&str] = &[
    "use", "uses", "used", "call", "calls", "return", "returns", "do", "does", "is", "are", "has",
    "have", "also", "only",
];

const FROM_TO: &[&str] = &["from", "to"];

/// True when this comment should be suppressed entirely (never reported).
pub(crate) fn is_exempt(text: &str) -> bool {
    let original = text.trim();
    if original.starts_with("#!") {
        return true;
    }

    let stripped = strip_delimiters(original);
    let stripped = stripped.strip_prefix('@').unwrap_or(stripped).trim();

    if stripped.is_empty() {
        return true;
    }
    if BDD_KEYWORDS
        .iter()
        .any(|keyword| stripped.eq_ignore_ascii_case(keyword))
    {
        return true;
    }
    if DIRECTIVE_PREFIXES
        .iter()
        .any(|directive| starts_with_ignore_ascii_case(stripped, directive))
    {
        return true;
    }
    if DELIMITED_DIRECTIVES
        .iter()
        .any(|directive| is_delimited_directive(stripped, directive))
    {
        return true;
    }
    if stripped.chars().all(|c| SEPARATOR_CHARS.contains(&c)) {
        return true;
    }
    is_url_only(stripped)
}

/// True when the comment reads like an agent's note about a change rather
/// than an explanation of the code.
pub(crate) fn is_agent_memo(text: &str) -> bool {
    let text = strip_delimiters(text.trim());
    is_english_agent_memo(text)
}

fn is_english_agent_memo(text: &str) -> bool {
    let starts_like_memo = match text.as_bytes().first().map(u8::to_ascii_lowercase) {
        Some(b'a') => starts_with_any(text, &["added", "after this"]),
        Some(b'b') => starts_with_ignore_ascii_case(text, "before this"),
        Some(b'c') => {
            starts_with_word_pair(text, "changed", FROM_TO)
                || starts_with_word_pair(text, "converted", FROM_TO)
        }
        Some(b'd') => starts_with_ignore_ascii_case(text, "deleted"),
        Some(b'h') => starts_with_ignore_ascii_case(text, "here we"),
        Some(b'i') => {
            starts_with_ignore_ascii_case(text, "implemented")
                || starts_with_word_pair(text, "implementation", &["of"])
        }
        Some(b'm') => {
            starts_with_word_pair(text, "modified", FROM_TO)
                || starts_with_word_pair(text, "moved", FROM_TO)
                || starts_with_word_pair(text, "migrated", FROM_TO)
        }
        Some(b'n') => {
            starts_with_now_change(text, "now we")
                || starts_with_now_change(text, "now this")
                || starts_with_now_change(text, "now it")
        }
        Some(b'p') => starts_with_ignore_ascii_case(text, "previously"),
        Some(b'r') => {
            starts_with_any(text, &["refactored", "replaced", "removed"])
                || starts_with_word_pair(text, "renamed", FROM_TO)
        }
        Some(b's') => starts_with_word_pair(text, "switched", FROM_TO),
        Some(b't') => starts_with_word_pair(
            text,
            "this",
            &["implements", "adds", "removes", "changes", "fixes"],
        ),
        Some(b'u') => starts_with_word_pair(text, "updated", FROM_TO),
        Some(b'w') => starts_with_word_pair(text, "was", &["changed"]),
        _ => false,
    };
    starts_like_memo
}

fn strip_delimiters(text: &str) -> &str {
    let mut text = text.trim();
    if let Some(rest) = text.strip_suffix("*/") {
        text = rest.trim_end();
    }
    loop {
        let trimmed = match text {
            _ if text.starts_with("#[") => break,
            _ if text.starts_with("//") => &text[2..],
            _ if text.starts_with("/*") => &text[2..],
            _ if text.starts_with("--") => &text[2..],
            _ if text.starts_with('#') => &text[1..],
            _ if text.starts_with('*') => &text[1..],
            _ => break,
        };
        text = trimmed.trim();
    }
    text
}

fn is_delimited_directive(text: &str, directive: &str) -> bool {
    if !starts_with_ignore_ascii_case(text, directive) {
        return false;
    }
    match text.as_bytes().get(directive.len()) {
        None => true,
        Some(b'(' | b':') => true,
        Some(_) => false,
    }
}

fn is_url_only(text: &str) -> bool {
    !text.contains(char::is_whitespace)
        && (text.starts_with("http://") || text.starts_with("https://"))
}

fn starts_with_now_change(text: &str, phrase: &str) -> bool {
    let Some(rest) = strip_prefix_ignore_ascii_case(text, phrase) else {
        return false;
    };
    let rest = rest.trim_start();
    let word = rest
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("");
    NOW_VERBS.iter().any(|verb| word.eq_ignore_ascii_case(verb))
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes
        .iter()
        .any(|prefix| starts_with_ignore_ascii_case(text, prefix))
}

fn starts_with_word_pair(text: &str, first: &str, seconds: &[&str]) -> bool {
    let Some(rest) = strip_prefix_ignore_ascii_case(text, first) else {
        return false;
    };
    let rest = rest.trim_start();
    seconds
        .iter()
        .any(|second| starts_with_ignore_ascii_case(rest, second))
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    starts_with_ignore_ascii_case(text, prefix).then(|| &text[prefix.len()..])
}

fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    let text = text.as_bytes();
    let prefix = prefix.as_bytes();
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::{is_agent_memo, is_exempt};

    #[test]
    fn exempts_empty_comments() {
        assert!(is_exempt("//"));
        assert!(is_exempt("#"));
        assert!(is_exempt("/* */"));
        assert!(is_exempt("--"));
    }

    #[test]
    fn exempts_shebang() {
        assert!(is_exempt("#!/usr/bin/env python3"));
        assert!(!is_exempt("# ! not a shebang"));
    }

    #[test]
    fn exempts_bdd_markers() {
        for marker in [
            "// given",
            "// When",
            "# THEN",
            "// arrange",
            "// act",
            "// assert",
            "// when & then",
            "// when&then",
        ] {
            assert!(is_exempt(marker), "{marker}");
        }
        assert!(!is_exempt("// given the parser is ready"));
    }

    #[test]
    fn exempts_tool_directives() {
        for directive in [
            "# type: ignore",
            "# noqa: E501",
            "# pyright: ignore",
            "# ruff: noqa",
            "# mypy: ignore-errors",
            "# pylint: disable=all",
            "# flake8: noqa",
            "# pyre: ignore",
            "# pytype: skip-file",
            "// eslint-disable-next-line",
            "// eslint-ignore",
            "// prettier-ignore",
            "// @ts-ignore",
            "// @ts-expect-error",
            "// clippy: pedantic",
            "// rustfmt::skip",
            "// SAFETY: pointer is valid",
            "/* cbindgen:ignore */",
            "// codegen generated",
            "// nolint",
            "// go:generate stringer",
            "// #[allow(dead_code)]",
            "// biome-ignore lint: reason",
        ] {
            assert!(is_exempt(directive), "{directive}");
        }
    }

    #[test]
    fn exempts_delimited_lint_directives_only() {
        assert!(is_exempt("// allow(dead_code)"));
        assert!(is_exempt("// deny(warnings)"));
        assert!(is_exempt("// warn:"));
        assert!(is_exempt("// forbid"));
        assert!(!is_exempt("// warning: this is subtle"));
        assert!(!is_exempt("// allows the caller to opt out"));
        assert!(!is_exempt("// denies access when the token expired"));
    }

    #[test]
    fn exempts_separator_lines() {
        assert!(is_exempt("// ----------------"));
        assert!(is_exempt("// ================"));
        assert!(is_exempt("// ~~~~"));
        assert!(!is_exempt("// --- keep this ---"));
    }

    #[test]
    fn exempts_url_only_comments() {
        assert!(is_exempt("// https://example.com/spec"));
        assert!(is_exempt("# http://example.com"));
        assert!(!is_exempt("// see https://example.com/spec"));
    }

    #[test]
    fn detects_change_verbs_with_from_or_to() {
        assert!(is_agent_memo("// Changed from regex to a parser."));
        assert!(is_agent_memo("// modified to use the cache"));
        assert!(is_agent_memo("// moved from utils to core"));
        assert!(is_agent_memo("// migrated to the new API"));
        assert!(is_agent_memo("// renamed from foo to bar"));
        assert!(is_agent_memo("// converted to a builder"));
        assert!(is_agent_memo("// switched to tokio"));
        assert!(is_agent_memo("// updated from v1 to v2"));
        assert!(!is_agent_memo("// changed behaviour is documented below"));
    }

    #[test]
    fn detects_past_tense_change_notes() {
        assert!(is_agent_memo("// added a retry loop"));
        assert!(is_agent_memo("// deleted the legacy path"));
        assert!(is_agent_memo("// removed the old shim"));
        assert!(is_agent_memo("// replaced the map with a vec"));
        assert!(is_agent_memo("// implemented the fallback"));
        assert!(is_agent_memo("// refactored to share the buffer"));
    }

    #[test]
    fn ignores_present_tense_imperatives() {
        assert!(!is_agent_memo("// add the header before sending"));
        assert!(!is_agent_memo("// delete the file when done"));
        assert!(!is_agent_memo("// Remove the temp dir on drop."));
        assert!(!is_agent_memo("// replace placeholders in the template"));
        assert!(!is_agent_memo("// implement Display for the error type"));
        assert!(!is_agent_memo("// refactor candidates live here"));
    }

    #[test]
    fn detects_narrative_markers() {
        assert!(is_agent_memo("// here we build the index"));
        assert!(is_agent_memo("// Previously this returned an Option."));
        assert!(!is_agent_memo("// Note: the caller must hold the lock."));
        assert!(is_agent_memo("// implementation of the retry policy"));
        assert!(is_agent_memo("// after this the buffer is empty"));
        assert!(is_agent_memo("// before this we held the lock"));
        assert!(is_agent_memo("// was changed to avoid a deadlock"));
    }

    #[test]
    fn detects_this_plus_third_person_verb() {
        assert!(is_agent_memo("// This implements the retry logic."));
        assert!(is_agent_memo("// this adds a header"));
        assert!(is_agent_memo("// this removes the guard"));
        assert!(is_agent_memo("// this changes the ordering"));
        assert!(is_agent_memo("// this fixes the race"));
        assert!(!is_agent_memo("// this buffer is reused"));
    }

    #[test]
    fn detects_now_plus_change_verb() {
        assert!(is_agent_memo("// Now we use a parser instead."));
        assert!(is_agent_memo("// now this returns an error"));
        assert!(is_agent_memo("// now it is lazily initialized"));
        assert!(!is_agent_memo(
            "// Now that the config is loaded, the server can start."
        ));
        assert!(!is_agent_memo("// now we need a lock around the map"));
    }

    #[test]
    fn ascii_arrows_are_not_memos() {
        assert!(!is_agent_memo("// regex -> parser"));
        assert!(!is_agent_memo("// running -> thinking: animation noise"));
        assert!(!is_agent_memo("// Slug -> Title Case fallback."));
    }

    #[test]
    fn strips_block_and_hash_delimiters() {
        assert!(is_agent_memo("/* previously this was recursive */"));
        assert!(is_agent_memo("* removed the old shim"));
        assert!(is_agent_memo("# added a retry loop"));
        assert!(is_agent_memo("-- renamed from foo to bar"));
    }

    #[test]
    fn regressions_from_this_repo_are_not_memos() {
        for comment in [
            "// Now that the config is loaded, the server can start.",
            "// Note that the range is inclusive.",
            "// NOTE: the prewarm is intentionally NOT triggered here.",
            "// Unassigned -> allow, regardless of progress freshness.",
            "// Adds a trailing newline when missing.",
            "// Remove the temp dir on drop.",
            "// warning: this is subtle",
            "// allows the caller to opt out",
        ] {
            assert!(!is_agent_memo(comment), "{comment}");
        }
    }
}
