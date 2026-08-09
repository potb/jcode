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
    fn single_line_collapses_newlines_and_runs() {
        assert_eq!(single_line("a\nb"), "a b");
        assert_eq!(single_line("  a   b  \n c "), "a b c");
        assert_eq!(single_line("   "), "");
    }
}
