//! Lexical comment scanner for C-family languages.

use crate::RawComment;

const MAX_TEXT_CHARS: usize = 200;

/// Extract every comment from C-family source (Rust, TypeScript, JavaScript, Go, C,
/// C++, Objective-C, JSONC).
///
/// The scanner is a single pass over `content` and skips string, char and raw string
/// literals so comment-like text inside them is not reported. Malformed input (an
/// unterminated string, block comment or raw string) is consumed to the end of input
/// instead of panicking or looping.
pub(crate) fn scan(content: &str) -> Vec<RawComment> {
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut comments = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;

    while i < len {
        let b = bytes[i];
        match b {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'/' => {
                let start = i;
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                comments.push(make_comment(&content[start..i], line));
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                let start = i;
                let start_line = line;
                i = skip_block_comment(bytes, i, &mut line);
                comments.push(make_comment(&content[start..i], start_line));
            }
            b'"' => {
                i = skip_quoted(bytes, i, b'"', &mut line);
            }
            b'`' => {
                i = skip_quoted_raw(bytes, i, b'`', &mut line);
            }
            b'\'' => {
                i = skip_single_quote(content, bytes, i, &mut line);
            }
            b'r' | b'b' if raw_string_start(bytes, i).is_some() => {
                i = skip_raw_string(bytes, i, &mut line);
            }
            _ => {
                i += 1;
            }
        }
    }

    comments
}

fn make_comment(raw: &str, line: usize) -> RawComment {
    let trimmed = raw.trim();
    let is_doc = is_doc_comment(trimmed);
    let collapsed = collapse(trimmed);
    let text = if collapsed.chars().count() > MAX_TEXT_CHARS {
        collapsed.chars().take(MAX_TEXT_CHARS).collect()
    } else {
        collapsed
    };
    RawComment { line, text, is_doc }
}

fn is_doc_comment(trimmed: &str) -> bool {
    if let Some(rest) = trimmed.strip_prefix("//") {
        if rest.starts_with('!') {
            return true;
        }
        return rest.starts_with('/') && !rest.starts_with("//");
    }
    if trimmed == "/**/" {
        return false;
    }
    trimmed.starts_with("/**") || trimmed.starts_with("/*!")
}

fn collapse(trimmed: &str) -> String {
    if !trimmed.contains('\n') {
        return trimmed.to_string();
    }
    let mut out = String::with_capacity(trimmed.len());
    for part in trimmed
        .lines()
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

fn skip_block_comment(bytes: &[u8], start: usize, line: &mut usize) -> usize {
    let len = bytes.len();
    let mut i = start + 2;
    let mut depth = 1usize;
    while i < len {
        if bytes[i] == b'\n' {
            *line += 1;
            i += 1;
        } else if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    len
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8, line: &mut usize) -> usize {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        match bytes[i] {
            // A line continuation escapes the newline itself, so the escaped
            // byte still has to be counted or every later line is off by one.
            b'\\' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    *line += 1;
                }
                i += 2;
            }
            b'\n' => {
                *line += 1;
                i += 1;
            }
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    len
}

fn skip_quoted_raw(bytes: &[u8], start: usize, quote: u8, line: &mut usize) -> usize {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        if bytes[i] == b'\n' {
            *line += 1;
        } else if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    len
}

fn skip_single_quote(content: &str, bytes: &[u8], start: usize, line: &mut usize) -> usize {
    let len = bytes.len();
    if start + 1 >= len {
        return len;
    }
    // A `'` followed by an identifier character that is not itself closed by a `'` is a
    // Rust lifetime or loop label (`&'a str`, `'outer: loop`), not a char literal.
    let next = bytes[start + 1];
    if next != b'\\' && is_ident_byte(next) {
        let after = char_after(content, start + 1);
        if after != Some('\'') {
            return start + 1;
        }
    }
    skip_quoted(bytes, start, b'\'', line)
}

fn char_after(content: &str, index: usize) -> Option<char> {
    let mut chars = content[index..].chars();
    chars.next()?;
    chars.next()
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Returns `(hash_count, offset_of_opening_quote)` if a raw string literal starts at `i`.
fn raw_string_start(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if i > 0 && is_ident_byte(bytes[i - 1]) {
        return None;
    }
    let mut j = i;
    if bytes[j] == b'b' {
        j += 1;
        if j >= bytes.len() || bytes[j] != b'r' {
            return None;
        }
    }
    if bytes[j] != b'r' {
        return None;
    }
    j += 1;
    let hash_start = j;
    while j < bytes.len() && bytes[j] == b'#' {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'"' {
        Some((j - hash_start, j))
    } else {
        None
    }
}

fn skip_raw_string(bytes: &[u8], start: usize, line: &mut usize) -> usize {
    let len = bytes.len();
    let Some((hashes, quote_index)) = raw_string_start(bytes, start) else {
        return start + 1;
    };
    let mut i = quote_index + 1;
    while i < len {
        if bytes[i] == b'\n' {
            *line += 1;
            i += 1;
            continue;
        }
        // The terminator is a quote followed by exactly as many `#` as the opener had.
        if bytes[i] == b'"' {
            let mut seen = 0usize;
            while seen < hashes && i + 1 + seen < len && bytes[i + 1 + seen] == b'#' {
                seen += 1;
            }
            if seen == hashes {
                return i + 1 + hashes;
            }
        }
        i += 1;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(content: &str) -> Vec<String> {
        scan(content).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn line_comment_is_found() {
        let found = scan("let x = 1; // hello\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// hello");
        assert_eq!(found[0].line, 1);
        assert!(!found[0].is_doc);
    }

    #[test]
    fn block_comment_is_found() {
        let found = scan("a /* one */ b");
        assert_eq!(texts("a /* one */ b"), vec!["/* one */"]);
        assert_eq!(found[0].line, 1);
    }

    #[test]
    fn line_number_is_comment_start() {
        let found = scan("a\nb\n// third\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn double_quoted_string_hides_comment() {
        assert!(scan(r#"let s = "// not a comment";"#).is_empty());
        assert!(scan(r#"let s = "/* not a comment */";"#).is_empty());
    }

    #[test]
    fn escaped_quotes_in_string() {
        assert!(scan(r#"let s = "a\"// still string";"#).is_empty());
        let found = scan("let s = \"a\\\\\"; // yes\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// yes");
    }

    #[test]
    fn raw_string_with_hashes_hides_comment() {
        assert!(scan(r##"let r = r#"has "quotes" and // slashes"#;"##).is_empty());
        assert!(scan(r###"let r = r##"contains "# inside"##;"###).is_empty());
        assert!(scan(r##"let r = br#"bytes // here"#;"##).is_empty());
        assert!(scan(r#"let r = r"plain // raw";"#).is_empty());
    }

    #[test]
    fn raw_string_prefix_needs_word_boundary() {
        let found = scan("let bar = r\"x\"; // c\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// c");
    }

    #[test]
    fn nested_block_comment_is_one_comment() {
        let found = scan("/* outer /* inner */ still outer */");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "/* outer /* inner */ still outer */");
    }

    #[test]
    fn unterminated_block_comment_does_not_panic() {
        let found = scan("code(); /* dangling\nmore");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 1);
        assert_eq!(found[0].text, "/* dangling more");
    }

    #[test]
    fn unterminated_string_does_not_panic() {
        assert!(scan("let s = \"open // nope").is_empty());
        assert!(scan(r##"let r = r#"open // nope"##).is_empty());
    }

    #[test]
    fn char_literal_with_double_quote_does_not_desync() {
        let found = scan("let c = '\"'; // x\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// x");
    }

    #[test]
    fn escaped_char_literal_does_not_desync() {
        let found = scan("let c = '\\''; // x\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// x");
    }

    #[test]
    fn lifetime_is_not_a_char_literal() {
        let found = scan("fn f<'a>(x: &'a str) {} // trailing\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// trailing");
    }

    #[test]
    fn loop_label_is_not_a_char_literal() {
        let found = scan("'outer: loop { break 'outer; } // c\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// c");
    }

    #[test]
    fn doc_line_comments() {
        assert!(scan("/// doc\n")[0].is_doc);
        assert!(scan("//! inner doc\n")[0].is_doc);
        assert!(!scan("//// not doc\n")[0].is_doc);
        assert!(!scan("// plain\n")[0].is_doc);
    }

    #[test]
    fn doc_block_comments() {
        assert!(scan("/** doc */")[0].is_doc);
        assert!(scan("/*! inner doc */")[0].is_doc);
        assert!(!scan("/**/")[0].is_doc);
        assert!(!scan("/* plain */")[0].is_doc);
    }

    #[test]
    fn go_backtick_raw_string_hides_comment() {
        assert!(scan("s := `no // comment here`").is_empty());
        let found = scan("s := `raw` // real\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// real");
    }

    #[test]
    fn unicode_is_safe() {
        let found = scan("let s = \"日本語 🚀 // no\"; // 🎉 comment 漢字\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "// 🎉 comment 漢字");
        assert!(scan("let c = '🚀'; ").is_empty());
    }

    #[test]
    fn multiline_block_comment_is_collapsed() {
        let content = "x\n/* first\n   second\n   third */\n";
        let found = scan(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].text, "/* first second third */");
    }

    #[test]
    fn long_comment_is_truncated_at_char_boundary() {
        let content = format!("// {}\n", "🚀".repeat(300));
        let found = scan(&content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text.chars().count(), MAX_TEXT_CHARS);
        assert!(found[0].text.starts_with("// 🚀"));
    }

    #[test]
    fn multiple_comments_track_lines() {
        let found = scan("// a\nlet s = \"x\";\n/* b */\n// c\n");
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].line, 1);
        assert_eq!(found[1].line, 3);
        assert_eq!(found[2].line, 4);
    }

    #[test]
    fn string_line_continuation_keeps_later_lines_aligned() {
        let found = scan("let s = \"one \\\ntwo\";\n// after\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn escaped_backslash_before_newline_still_ends_the_string() {
        let found = scan("let s = \"ends \\\\\";\n// after\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(scan("").is_empty());
        assert!(scan("/").is_empty());
        assert!(scan("'").is_empty());
    }

    #[test]
    fn comment_text_is_trimmed() {
        let found = scan("   //   spaced   \n");
        assert_eq!(found[0].text, "//   spaced");
    }
}
