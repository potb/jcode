//! End-to-end tests for `jcode-comment-scan`'s public API: `scan`,
//! `comments_block`, `supports_language`, and the process-global config.

use jcode_comment_scan::{
    CommentCheckConfig, CommentSpan, MAX_COMMENTS_PER_FILE, comments_block, configure, is_enabled,
    scan, supports_language,
};

/// The config is process-global, so serialize the tests that touch it against
/// each other (cargo runs test fns in parallel by default).
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

const SUPPORTED_LANGUAGES: &[&str] = &[
    "rust",
    "typescript",
    "typescriptreact",
    "javascript",
    "javascriptreact",
    "go",
    "c",
    "cpp",
    "objective-c",
    "objective-cpp",
    "jsonc",
    "python",
    "yaml",
];

fn one_comment_source(language_id: &str) -> &'static str {
    match language_id {
        "python" | "yaml" => "# why this exists\n",
        _ => "// why this exists\n",
    }
}

fn summarize(spans: &[CommentSpan]) -> Vec<(usize, &str, bool)> {
    spans
        .iter()
        .map(|s| (s.line, s.text.as_str(), s.is_memo))
        .collect()
}

const RUST_FILE: &str = r#"//! Module docs.

use std::fmt::Write;

/// Documented helper.
pub fn render(input: &str) -> String {
    // allow(dead_code)
    let marker = "// not a comment";
    // wrapped explanation of why the buffer is
    // preallocated before the loop
    let mut out = String::new();
    out.reserve(marker.len()); // trailing explanation
    let _ = write!(out, "{}", marker.len() + input.len());
    /* block comment explaining a subtle invariant */
    out
}
"#;

#[test]
fn realistic_rust_file_yields_expected_spans() {
    let spans = scan(RUST_FILE, "rust");
    assert_eq!(
        summarize(&spans),
        vec![
            (
                9,
                "// wrapped explanation of why the buffer is preallocated before the loop",
                false
            ),
            (12, "// trailing explanation", false),
            (
                14,
                "/* block comment explaining a subtle invariant */",
                false
            ),
        ]
    );
}

const PYTHON_FILE: &str = r##""""Module docstring."""

SEPARATOR = "#"


def render(items):
    """Return a joined string."""
    joined = SEPARATOR.join(items)  # trailing note

    # explains why the result is stripped
    return joined.strip()
"##;

#[test]
fn realistic_python_file_yields_expected_spans() {
    let spans = scan(PYTHON_FILE, "python");
    assert_eq!(
        summarize(&spans),
        vec![
            (8, "# trailing note", false),
            (10, "# explains why the result is stripped", false),
        ]
    );
}

const YAML_FILE: &str = r#"# top-level explanation of this config
name: build
url: https://example.com/docs#anchor
message: "a # inside quotes"
runs: 3 # trailing explanation
"#;

#[test]
fn realistic_yaml_file_yields_expected_spans() {
    let spans = scan(YAML_FILE, "yaml");
    assert_eq!(
        summarize(&spans),
        vec![
            (1, "# top-level explanation of this config", false),
            (5, "# trailing explanation", false),
        ]
    );
}

#[test]
fn supported_languages_all_scan_a_comment() {
    for language_id in SUPPORTED_LANGUAGES {
        assert!(supports_language(language_id), "{language_id}");
        let spans = scan(one_comment_source(language_id), language_id);
        assert!(!spans.is_empty(), "{language_id}");
    }
}

#[test]
fn unsupported_languages_are_rejected_and_scan_empty() {
    for language_id in ["plaintext", "json"] {
        assert!(!supports_language(language_id), "{language_id}");
        assert!(scan("// why this exists\n", language_id).is_empty());
        assert!(scan("# why this exists\n", language_id).is_empty());
    }
}

#[test]
fn disabling_config_toggles_is_enabled_and_restores() {
    let _guard = lock();
    configure(CommentCheckConfig { enabled: false });
    assert!(!is_enabled());
    configure(CommentCheckConfig::default());
    assert!(is_enabled());
}

/// 13 reportable comments, one every other line so runs of consecutive line
/// comments are not merged into a single span.
fn thirteen_comment_source() -> String {
    (1..=13)
        .map(|i| format!("let x{i} = {i}; // restates line {i}\n\n"))
        .collect()
}

#[test]
fn comments_block_caps_entries_and_summarises_the_tail() {
    let spans = scan(&thirteen_comment_source(), "rust");
    assert_eq!(spans.len(), 13);

    let block = comments_block("src/generated.rs", &spans).expect("block");
    assert!(
        block.starts_with("<comments file=\"src/generated.rs\">\n1 // restates line 1\n"),
        "{block}"
    );
    assert!(block.contains("... 3 more comments"), "{block}");
    assert!(
        block.ends_with("Remove comments that restate the code. Keep only ones that explain why."),
        "{block}"
    );
    assert_eq!(
        block.matches("// restates line").count(),
        MAX_COMMENTS_PER_FILE
    );
}

#[test]
fn comments_block_is_none_when_nothing_is_reportable() {
    let source = "//! Module docs.\n\n/// Documented.\npub fn f() {}\n\n// allow(dead_code)\n";
    let spans = scan(source, "rust");
    assert!(spans.is_empty());
    assert!(comments_block("src/clean.rs", &spans).is_none());
}

#[test]
fn file_of_only_doc_comments_yields_no_spans() {
    let source = "//! Inner doc.\n\n/// Outer doc.\n///\n/// More docs.\npub struct S;\n\n/** Block doc. */\n";
    assert!(scan(source, "rust").is_empty());
}

#[test]
fn unterminated_block_comment_does_not_panic() {
    let spans = scan("fn f() {}\n/* dangling explanation\nstill going\n", "rust");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].line, 2);
}

#[test]
fn unterminated_string_does_not_panic() {
    assert!(scan("let s = \"open // nope\n", "rust").is_empty());
    assert!(scan("x = \"open # nope\n", "python").is_empty());
    assert!(scan("key: \"open # nope\n", "yaml").is_empty());
}

#[test]
fn emoji_only_file_does_not_panic() {
    for language_id in SUPPORTED_LANGUAGES {
        assert!(
            scan("🚀🎉🚀\n🎉🚀🎉\n", language_id).is_empty(),
            "{language_id}"
        );
    }
}

#[test]
fn five_thousand_line_file_scans_completely() {
    let source: String = (1..=2500)
        .map(|i| format!("// explanation number {i}\n\n"))
        .collect();
    assert_eq!(source.lines().count(), 5000);
    assert_eq!(scan(&source, "rust").len(), 2500);
}

#[test]
fn trailing_comment_after_code_is_its_own_span() {
    let source = "// standalone explanation\nlet x = 1; // trailing explanation\n";
    assert_eq!(
        summarize(&scan(source, "rust")),
        vec![
            (1, "// standalone explanation", false),
            (2, "// trailing explanation", false),
        ]
    );
}
