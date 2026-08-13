//! Rendering of the `<comments>` block, mirroring `jcode-lsp`'s
//! `<diagnostics>` format.
//!
//! ```text
//! <comments file="src/foo.rs">
//! 12 // restates the code
//! MEMO 40 // changed from regex to a parser
//! </comments>
//! ```

use crate::CommentSpan;

/// Entries shown before the tail is summarised, matching
/// `jcode_lsp::format::MAX_WARNINGS_PER_FILE`.
pub const MAX_COMMENTS_PER_FILE: usize = 10;

/// Render one file's comments. Returns `None` when there is nothing to show.
pub fn comments_block(display_path: &str, spans: &[CommentSpan]) -> Option<String> {
    if spans.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(MAX_COMMENTS_PER_FILE + 1);
    for span in spans.iter().take(MAX_COMMENTS_PER_FILE) {
        if span.is_memo {
            lines.push(format!("MEMO {} {}", span.line, span.text));
        } else {
            lines.push(format!("{} {}", span.line, span.text));
        }
    }
    if spans.len() > MAX_COMMENTS_PER_FILE {
        lines.push(format!(
            "... {} more comments",
            spans.len() - MAX_COMMENTS_PER_FILE
        ));
    }
    Some(format!(
        "<comments file=\"{display_path}\">\n{}\n</comments>\n{}",
        lines.join("\n"),
        advice(spans)
    ))
}

fn advice(spans: &[CommentSpan]) -> &'static str {
    if spans.iter().any(|s| s.is_memo) {
        "MEMO comments describe the change, not the code, and go stale immediately. \
         Remove them. Remove the others too unless they explain why the code is \
         the way it is."
    } else {
        "Remove comments that restate the code. Keep only ones that explain why."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line: usize, text: &str, is_memo: bool) -> CommentSpan {
        CommentSpan {
            line,
            text: text.to_string(),
            is_memo,
        }
    }

    #[test]
    fn empty_yields_none() {
        assert!(comments_block("a.rs", &[]).is_none());
    }

    #[test]
    fn single_comment_renders_line_and_text() {
        let block = comments_block("src/a.rs", &[span(12, "// restates", false)]).unwrap();
        assert!(block.starts_with("<comments file=\"src/a.rs\">\n12 // restates\n</comments>\n"));
        assert!(block.contains("Remove comments that restate the code."));
    }

    #[test]
    fn memo_entries_are_prefixed_and_change_the_advice() {
        let block = comments_block("a.rs", &[span(3, "// added retry", true)]).unwrap();
        assert!(block.contains("MEMO 3 // added retry"));
        assert!(block.contains("MEMO comments describe the change"));
    }

    #[test]
    fn tail_is_summarised_past_the_cap() {
        let spans: Vec<CommentSpan> = (1..=13).map(|i| span(i, "// x", false)).collect();
        let block = comments_block("a.rs", &spans).unwrap();
        assert!(block.contains("... 3 more comments"));
        assert_eq!(block.matches("// x").count(), MAX_COMMENTS_PER_FILE);
    }

    #[test]
    fn exactly_the_cap_has_no_tail() {
        let spans: Vec<CommentSpan> = (1..=MAX_COMMENTS_PER_FILE)
            .map(|i| span(i, "// x", false))
            .collect();
        let block = comments_block("a.rs", &spans).unwrap();
        assert!(!block.contains("more comments"));
    }
}
