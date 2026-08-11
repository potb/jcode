//! Width-aware text helpers shared by the inline gallery renderers.
//!
//! These started as private helpers inside [`crate::swarm_gallery`]. The
//! background-task gallery needs exactly the same primitives (terminal display
//! width, ellipsis truncation, hard clamping of a styled line, digit counting
//! for "+N" markers), and two copies would drift: the swarm strip's careful
//! handling of wide glyphs straddling the right edge is the kind of detail a
//! second implementation gets subtly wrong.

use ratatui::prelude::*;

/// Terminal display width of a string (wide glyphs like 🐝 count as 2).
pub fn disp_w(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// Truncate `s` to at most `max` display columns (wide glyphs count as 2),
/// appending an ellipsis when truncated.
pub fn truncate_label(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if disp_w(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let target = max - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > target {
            break;
        }
        used += cw;
        out.push(ch);
    }
    out.push('…');
    out
}

/// Number of decimal digits in `n` (for budgeting "+N" markers).
pub fn count_digits(n: usize) -> usize {
    let mut n = n.max(1);
    let mut d = 0;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

/// Truncate a styled line so its display width never exceeds `max_width`.
/// Splits mid-span if needed, dropping a trailing wide glyph that would
/// straddle the boundary.
pub fn clamp_line_to_width(line: &mut Line<'static>, max_width: usize) {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0usize;
    let mut clamped: Vec<Span<'static>> = Vec::new();
    for span in line.spans.drain(..) {
        let w = disp_w(&span.content);
        if used + w <= max_width {
            used += w;
            clamped.push(span);
            continue;
        }
        // Partial span: take chars while they fit.
        let mut taken = String::new();
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            if used + cw > max_width {
                break;
            }
            used += cw;
            taken.push(ch);
        }
        if !taken.is_empty() {
            clamped.push(Span::styled(taken, span.style));
        }
        break;
    }
    line.spans = clamped;
}

/// Wrap `value` to `width` display columns, returning one string per row.
///
/// Embedded newlines start a new row: a multi-line shell command is readable
/// precisely because of its line structure. Within a line, breaks prefer
/// whitespace, and a word longer than the whole width is hard-split rather
/// than allowed to overflow. An empty input yields a single empty row so a
/// blank output line still occupies its row.
pub fn wrap_text(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw in value.split('\n') {
        let logical = raw.trim_end_matches('\r').replace('\t', "    ");
        out.extend(wrap_one_line(&logical, width));
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Wrap a single newline-free line. Kept separate so [`wrap_text`] stays a
/// readable statement about how newlines are treated.
///
/// Whitespace between words is a break opportunity and is dropped when a break
/// happens there, so continuation rows never start with stray spaces. Leading
/// indentation of the source line is content, not a separator: scripts read by
/// their indentation.
fn wrap_one_line(line: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    let mut pending_space: Option<String> = None;

    for token in tokenize(line) {
        if token.chars().all(char::is_whitespace) {
            if current_w == 0 && rows.is_empty() {
                // Source indentation: keep it, clamped to the row.
                for chunk in hard_split(&token, width) {
                    if current_w > 0 {
                        rows.push(std::mem::take(&mut current));
                    }
                    current_w = disp_w(&chunk);
                    current = chunk;
                }
            } else if current_w > 0 {
                pending_space = Some(token);
            }
            continue;
        }

        let word_w = disp_w(&token);
        let sep_w = pending_space.as_deref().map(disp_w).unwrap_or(0);
        if current_w > 0 && current_w + sep_w + word_w > width {
            rows.push(std::mem::take(&mut current));
            current_w = 0;
            pending_space = None;
        }
        if let Some(space) = pending_space.take() {
            current.push_str(&space);
            current_w += disp_w(&space);
        }
        if current_w + word_w <= width {
            current.push_str(&token);
            current_w += word_w;
            continue;
        }
        // Word longer than a whole row: hard-split it across rows.
        for chunk in hard_split(&token, width.saturating_sub(current_w).max(1)) {
            if current_w + disp_w(&chunk) > width {
                rows.push(std::mem::take(&mut current));
                current_w = 0;
            }
            current.push_str(&chunk);
            current_w += disp_w(&chunk);
        }
    }

    rows.push(current);
    rows
}

/// Split a line into alternating whitespace and non-whitespace runs.
fn tokenize(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ch in line.chars() {
        let is_ws = ch.is_whitespace();
        match out.last_mut() {
            Some(last) if last.chars().next().is_some_and(char::is_whitespace) == is_ws => {
                last.push(ch)
            }
            _ => out.push(ch.to_string()),
        }
    }
    out
}

/// Cut `value` into chunks of at most `width` display columns, never splitting
/// a character. A single character wider than `width` gets a row of its own and
/// is kept rather than dropped: at that point the viewport is narrower than one
/// glyph, and the caller's final `clamp_line_to_width` is what enforces the
/// hard bound.
fn hard_split(value: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut chunk = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > width && !chunk.is_empty() {
            out.push(std::mem::take(&mut chunk));
            used = 0;
        }
        chunk.push(ch);
        used += cw;
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

/// Collapse any run of whitespace (including newlines) into single spaces.
///
/// Background-task display names come from real shell commands, so heredocs
/// and multi-line `for` loops are routine. An embedded newline would smear one
/// logical row across several terminal rows and wreck a fixed-width layout.
pub fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_label_respects_wide_glyphs() {
        assert_eq!(truncate_label("abc", 10), "abc");
        assert_eq!(truncate_label("abcdef", 4), "abc…");
        assert_eq!(truncate_label("abcdef", 1), "…");
        // 🐝 is two columns wide, so only one fits in three columns with the
        // ellipsis taking one.
        assert_eq!(disp_w(&truncate_label("🐝🐝🐝", 5)), 5);
    }

    #[test]
    fn clamp_line_never_exceeds_width() {
        for width in 0..12 {
            let mut line = Line::from(vec![Span::raw("🐝🐝"), Span::raw("abc"), Span::raw("🐝")]);
            clamp_line_to_width(&mut line, width);
            let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(
                disp_w(&rendered) <= width,
                "width {width} exceeded by {rendered:?}"
            );
        }
    }

    #[test]
    fn count_digits_counts_decimal_places() {
        assert_eq!(count_digits(0), 1);
        assert_eq!(count_digits(9), 1);
        assert_eq!(count_digits(10), 2);
        assert_eq!(count_digits(1234), 4);
    }

    #[test]
    fn wrap_text_breaks_on_whitespace_and_keeps_every_character() {
        let rows = wrap_text("the quick brown fox jumps", 10);
        assert!(rows.iter().all(|row| disp_w(row) <= 10), "{rows:?}");
        assert_eq!(rows.join(" "), "the quick brown fox jumps");
    }

    #[test]
    fn wrap_text_hard_splits_a_word_longer_than_the_width() {
        let rows = wrap_text(&"x".repeat(25), 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.concat(), "x".repeat(25));
        assert!(rows.iter().all(|row| disp_w(row) <= 10));
    }

    #[test]
    fn wrap_text_keeps_newlines_as_row_boundaries() {
        assert_eq!(wrap_text("a\nb", 10), vec!["a", "b"]);
        assert_eq!(wrap_text("", 10), vec![String::new()]);
        assert_eq!(wrap_text("a\r\nb", 10), vec!["a", "b"]);
    }

    /// Wide glyphs must not straddle a row edge. Width 1 cannot hold a
    /// two-column glyph at all, so that degenerate case is excluded: see
    /// [`hard_split`].
    #[test]
    fn wrap_text_never_exceeds_width_for_wide_glyphs() {
        for width in 2..12 {
            let rows = wrap_text("🐝🐝 abc 🐝🐝🐝", width);
            for row in &rows {
                assert!(disp_w(row) <= width, "width {width} exceeded by {row:?}");
            }
        }
    }

    #[test]
    fn single_line_collapses_newlines_and_runs() {
        assert_eq!(single_line("a\nb"), "a b");
        assert_eq!(single_line("  a   b  \n c "), "a b c");
        assert_eq!(single_line("   "), "");
    }
}
