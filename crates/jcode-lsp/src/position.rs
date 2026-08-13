//! Line/column position conversion between 1-based user coordinates and
//! LSP `Position`s in UTF-16 code units (the LSP default, and the only
//! encoding we advertise in `positionEncodings`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf16,
}

/// Convert a 1-based (line, column) pair to an LSP position (0-based line,
/// 0-based character offset in the negotiated encoding), using `text` for
/// UTF-16 remapping. Columns beyond the end of line clamp to line end.
pub fn to_lsp_position(
    text: &str,
    line_1: u32,
    column_1: u32,
    encoding: PositionEncoding,
) -> lsp_types::Position {
    let PositionEncoding::Utf16 = encoding;
    let line0 = line_1.saturating_sub(1);
    let col0 = column_1.saturating_sub(1);
    let Some(line_text) = text.lines().nth(line0 as usize) else {
        return lsp_types::Position::new(line0, col0);
    };
    // Interpret the incoming 1-based column as a character (Unicode scalar)
    // index, then convert to UTF-16 code units.
    let mut units: u32 = 0;
    for (i, ch) in line_text.chars().enumerate() {
        if i as u32 >= col0 {
            break;
        }
        units += ch.len_utf16() as u32;
    }
    lsp_types::Position::new(line0, units)
}

/// Convert an LSP position back to 1-based (line, column) character
/// coordinates using `text` (line text lookup). Falls back to raw values +1
/// when the line is out of range.
pub fn from_lsp_position(
    text: &str,
    pos: lsp_types::Position,
    encoding: PositionEncoding,
) -> (u32, u32) {
    let PositionEncoding::Utf16 = encoding;
    let line_1 = pos.line + 1;
    let Some(line_text) = text.lines().nth(pos.line as usize) else {
        return (line_1, pos.character + 1);
    };
    let mut units: u32 = 0;
    let mut chars: u32 = 0;
    for ch in line_text.chars() {
        if units >= pos.character {
            break;
        }
        units += ch.len_utf16() as u32;
        chars += 1;
    }
    (line_1, chars + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip() {
        let text = "fn main() {}\nlet x = 1;\n";
        let p = to_lsp_position(text, 2, 5, PositionEncoding::Utf16);
        assert_eq!(p, lsp_types::Position::new(1, 4));
        assert_eq!(from_lsp_position(text, p, PositionEncoding::Utf16), (2, 5));
    }

    #[test]
    fn utf16_multibyte() {
        // "é" is 1 UTF-16 unit, "𐍈" (U+10348) is 2 UTF-16 units / 4 UTF-8 bytes.
        let text = "é𐍈x = 1;\n";
        // Column 3 (the 'x', third char).
        let p16 = to_lsp_position(text, 1, 3, PositionEncoding::Utf16);
        assert_eq!(p16.character, 3); // 1 (é) + 2 (𐍈)
        assert_eq!(
            from_lsp_position(text, p16, PositionEncoding::Utf16),
            (1, 3)
        );
    }

    #[test]
    fn clamps_out_of_range() {
        let text = "ab\n";
        // Column past end of line clamps to line end.
        let p = to_lsp_position(text, 1, 99, PositionEncoding::Utf16);
        assert_eq!(p.character, 2);
        // Line past end of file: raw passthrough, no panic.
        let p = to_lsp_position(text, 42, 7, PositionEncoding::Utf16);
        assert_eq!(p, lsp_types::Position::new(41, 6));
        let back = from_lsp_position(
            text,
            lsp_types::Position::new(41, 6),
            PositionEncoding::Utf16,
        );
        assert_eq!(back, (42, 7));
    }

    #[test]
    fn zero_inputs_do_not_underflow() {
        let p = to_lsp_position("a", 0, 0, PositionEncoding::Utf16);
        assert_eq!(p, lsp_types::Position::new(0, 0));
    }
}
