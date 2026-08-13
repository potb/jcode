//! Lexical scanners for the `#` comment family (Python, YAML).

use crate::RawComment;

const MAX_TEXT: usize = 200;

/// Scan Python source for `#` line comments and triple-quoted strings.
///
/// Line comments are returned with `is_doc = false`; triple-quoted strings are returned with
/// `is_doc = true` and the line on which they start. A `#` inside any string literal is not a
/// comment. Unterminated literals consume the remainder of the input instead of panicking.
pub(crate) fn scan_python(content: &str) -> Vec<RawComment> {
    let mut out = Vec::new();
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut line = 1usize;

    while i < len {
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            i += 1;
        } else if b == b'#' {
            let mut j = i;
            while j < len && bytes[j] != b'\n' {
                j += 1;
            }
            out.push(RawComment {
                line,
                text: truncate_chars(content[i..j].trim(), MAX_TEXT),
                is_doc: false,
            });
            i = j;
        } else if b == b'\'' || b == b'"' {
            let (next_i, next_line) = scan_python_string(content, i, i, line, &mut out);
            i = next_i;
            line = next_line;
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            let mut j = i;
            while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j < len
                && (bytes[j] == b'\'' || bytes[j] == b'"')
                && is_string_prefix(&content[start..j])
            {
                let (next_i, next_line) = scan_python_string(content, start, j, line, &mut out);
                i = next_i;
                line = next_line;
            } else {
                i = j;
            }
        } else {
            i += char_len(content, i);
        }
    }

    out
}

/// Scan YAML for `#` comments, ignoring quoted scalars and block scalar bodies.
///
/// A `#` only opens a comment at the start of a line or after whitespace, so `http://x/#frag`
/// stays a plain scalar. All results have `is_doc = false`.
pub(crate) fn scan_yaml(content: &str) -> Vec<RawComment> {
    let mut out = Vec::new();
    let mut block_indent: Option<usize> = None;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let indent = raw_line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let blank = raw_line.trim().is_empty();

        // A block scalar body is literal text, so skip every line indented deeper than the
        // line that carried the `|` or `>` indicator (blank lines belong to the body too).
        if let Some(bi) = block_indent {
            if blank || indent > bi {
                continue;
            }
            block_indent = None;
        }

        let (comment_start, unclosed_quote) = yaml_comment_start(raw_line);
        if let Some(start) = comment_start {
            out.push(RawComment {
                line: line_no,
                text: truncate_chars(raw_line[start..].trim(), MAX_TEXT),
                is_doc: false,
            });
        }

        let code_end = comment_start.unwrap_or(raw_line.len());
        if !unclosed_quote && has_block_indicator(&raw_line[..code_end]) {
            block_indent = Some(indent);
        }
    }

    out
}

fn is_string_prefix(prefix: &str) -> bool {
    if prefix.len() > 2 {
        return false;
    }
    matches!(
        prefix.to_ascii_lowercase().as_str(),
        "r" | "b" | "f" | "u" | "rb" | "br" | "fr" | "rf"
    )
}

fn scan_python_string(
    content: &str,
    token_start: usize,
    quote_pos: usize,
    start_line: usize,
    out: &mut Vec<RawComment>,
) -> (usize, usize) {
    let bytes = content.as_bytes();
    let len = bytes.len();
    let quote = bytes[quote_pos];
    let triple =
        quote_pos + 2 < len && bytes[quote_pos + 1] == quote && bytes[quote_pos + 2] == quote;
    let mut line = start_line;
    let mut i = quote_pos + if triple { 3 } else { 1 };

    while i < len {
        let c = bytes[i];
        if c == b'\\' {
            if i + 1 < len {
                if bytes[i + 1] == b'\n' {
                    line += 1;
                }
                i += 1 + char_len(content, i + 1);
            } else {
                i += 1;
            }
            continue;
        }
        if c == b'\n' {
            if !triple {
                break;
            }
            line += 1;
            i += 1;
            continue;
        }
        if c == quote {
            if !triple {
                i += 1;
                break;
            }
            if i + 2 < len && bytes[i + 1] == quote && bytes[i + 2] == quote {
                i += 3;
                break;
            }
            i += 1;
            continue;
        }
        i += char_len(content, i);
    }

    if triple {
        out.push(RawComment {
            line: start_line,
            text: truncate_chars(&collapse_whitespace(&content[token_start..i]), MAX_TEXT),
            is_doc: true,
        });
    }

    (i, line)
}

fn yaml_comment_start(line: &str) -> (Option<usize>, bool) {
    let bytes = line.as_bytes();
    let len = line.len();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_ws = true;

    while i < len {
        let c = bytes[i];
        if in_single {
            if c == b'\'' {
                if i + 1 < len && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += char_len(line, i);
            continue;
        }
        if in_double {
            if c == b'\\' {
                i += if i + 1 < len {
                    1 + char_len(line, i + 1)
                } else {
                    1
                };
                continue;
            }
            if c == b'"' {
                in_double = false;
            }
            i += char_len(line, i);
            continue;
        }
        // Outside quotes a `#` only opens a comment at line start or after whitespace.
        if c == b'#' && prev_ws {
            return (Some(i), false);
        }
        if c == b'\'' {
            in_single = true;
        } else if c == b'"' {
            in_double = true;
        }
        prev_ws = c == b' ' || c == b'\t';
        i += char_len(line, i);
    }

    (None, in_single || in_double)
}

fn has_block_indicator(code: &str) -> bool {
    let trimmed = code.trim_end();
    let token = match trimmed.rsplit([' ', '\t']).next() {
        Some(token) => token,
        None => return false,
    };
    let mut chars = token.chars();
    match chars.next() {
        Some('|') | Some('>') => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_digit() || c == '+' || c == '-')
}

fn char_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(1, |c| c.len_utf8())
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (count, c) in s.chars().enumerate() {
        if count >= max {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comments(found: &[RawComment]) -> Vec<&RawComment> {
        found.iter().filter(|c| !c.is_doc).collect()
    }

    fn docs(found: &[RawComment]) -> Vec<&RawComment> {
        found.iter().filter(|c| c.is_doc).collect()
    }

    #[test]
    fn python_hash_inside_double_quoted_string_is_not_comment() {
        let found = scan_python("x = \"# not a comment\"\n");
        assert!(comments(&found).is_empty());
    }

    #[test]
    fn python_hash_inside_single_quoted_string_is_not_comment() {
        let found = scan_python("x = '# nope'\n");
        assert!(comments(&found).is_empty());
    }

    #[test]
    fn python_triple_quoted_hash_is_doc_only() {
        let found = scan_python("x = \"\"\"# nope\"\"\"\n");
        assert!(comments(&found).is_empty());
        assert_eq!(docs(&found).len(), 1);
        assert!(docs(&found)[0].text.contains("# nope"));
    }

    #[test]
    fn python_raw_string_escaped_quote_then_real_comment() {
        let found = scan_python("x = r\"\\\"\" ; # real\n");
        let comments = comments(&found);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "# real");
        assert_eq!(comments[0].line, 1);
    }

    #[test]
    fn python_module_docstring_line_one() {
        let found = scan_python("\"\"\"Module docs.\"\"\"\nx = 1\n");
        let docs = docs(&found);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].line, 1);
    }

    #[test]
    fn python_function_docstring_start_line() {
        let src = "def f():\n    \"\"\"Doc\n    spanning lines.\"\"\"\n    return 1\n";
        let found = scan_python(src);
        let docs = docs(&found);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].line, 2);
        assert_eq!(docs[0].text, "\"\"\"Doc spanning lines.\"\"\"");
    }

    #[test]
    fn python_shebang_emitted_as_comment() {
        let found = scan_python("#!/usr/bin/env python\nx = 1\n");
        let comments = comments(&found);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, 1);
        assert!(!comments[0].is_doc);
    }

    #[test]
    fn python_fstring_hash_not_comment() {
        let found = scan_python("y = f\"{x}#y\"\n");
        assert!(comments(&found).is_empty());
    }

    #[test]
    fn python_unterminated_triple_quote_no_panic() {
        let found = scan_python("x = \"\"\"unterminated\nstill going\n");
        assert_eq!(docs(&found).len(), 1);
        assert!(comments(&found).is_empty());
    }

    #[test]
    fn python_emoji_string_and_comment_no_panic() {
        let found = scan_python("x = \"🎉 # nope\"  # real 🎉\n");
        let comments = comments(&found);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "# real 🎉");
    }

    #[test]
    fn python_line_numbers_after_multiline_string() {
        let found = scan_python("x = '''a\nb'''\n# tail\n");
        let comments = comments(&found);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, 3);
    }

    #[test]
    fn yaml_trailing_comment() {
        let found = scan_yaml("key: value # comment\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "# comment");
        assert_eq!(found[0].line, 1);
        assert!(!found[0].is_doc);
    }

    #[test]
    fn yaml_url_fragment_not_comment() {
        let found = scan_yaml("url: http://x/#frag\n");
        assert!(found.is_empty());
    }

    #[test]
    fn yaml_single_quoted_hash_not_comment() {
        let found = scan_yaml("key: 'has # inside'\n");
        assert!(found.is_empty());
    }

    #[test]
    fn yaml_double_quoted_hash_not_comment() {
        let found = scan_yaml("key: \"has # inside\"\n");
        assert!(found.is_empty());
    }

    #[test]
    fn yaml_literal_block_scalar_body_not_comment() {
        let found = scan_yaml("script: |\n  echo hi\n  # not a comment\n");
        assert!(found.is_empty());
    }

    #[test]
    fn yaml_folded_block_scalar_body_not_comment() {
        let found = scan_yaml("text: >-\n  wrapped\n  # not a comment\n");
        assert!(found.is_empty());
    }

    #[test]
    fn yaml_full_line_comment_line_number() {
        let found = scan_yaml("key: value\n# full line comment\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].text, "# full line comment");
    }

    #[test]
    fn yaml_comment_after_block_scalar_dedent() {
        let found = scan_yaml("script: |\n  echo hi\n  # inside\nnext: 1 # real\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 4);
        assert_eq!(found[0].text, "# real");
    }

    #[test]
    fn yaml_emoji_no_panic() {
        let found = scan_yaml("key: \"🎉 # inside\" # real 🎉\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "# real 🎉");
    }
}
