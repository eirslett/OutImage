//! Byte offset ↔ LSP [`Position`] conversion (UTF-8 / UTF-16 / UTF-32).

use tower_lsp_server::ls_types::{Position, Range};

use crate::error::Span;

/// Negotiated (or default) character encoding for LSP positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// Character offsets are UTF-16 code units (LSP default).
    #[default]
    Utf16,
    /// Character offsets are UTF-8 code units (bytes).
    Utf8,
    /// Character offsets are Unicode scalar values (UTF-32 / code points).
    Utf32,
}

/// Precomputed line-start byte offsets for fast position mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionIndex {
    /// Byte offset of the first character of each line (line 0 at offset 0).
    line_starts: Vec<usize>,
    /// Total source length in bytes.
    len: usize,
}

impl PositionIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                line_starts.push(i + ch.len_utf8());
            }
        }
        Self {
            line_starts,
            len: text.len(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Converts a byte offset to an LSP position under `encoding`.
    pub fn offset_to_position(&self, text: &str, offset: usize, encoding: Encoding) -> Position {
        let offset = offset.min(self.len);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(insert) => insert.saturating_sub(1),
        };
        let line_start = self.line_starts[line];
        let character = encode_column(&text[line_start..offset], encoding);
        Position::new(line as u32, character)
    }

    /// Converts an LSP position to a byte offset under `encoding`.
    ///
    /// Out-of-range lines clamp to EOF; out-of-range columns clamp to the end
    /// of the target line (LSP semantics).
    pub fn position_to_offset(&self, text: &str, position: Position, encoding: Encoding) -> usize {
        if self.line_starts.is_empty() {
            return 0;
        }
        let line = position.line as usize;
        if line >= self.line_starts.len() {
            return self.len;
        }
        let line_start = self.line_starts[line];
        let line_end = self.line_starts.get(line + 1).copied().unwrap_or(self.len);
        // Exclude trailing `\n` from the clamp target so a column past EOL
        // lands on the newline boundary (exclusive end of line content).
        let content_end = if line_end > line_start && text.as_bytes()[line_end - 1] == b'\n' {
            line_end - 1
        } else {
            line_end
        };
        let column_bytes =
            decode_column(&text[line_start..content_end], position.character, encoding);
        (line_start + column_bytes).min(content_end)
    }
}

/// Maps a byte [`Span`] to an LSP [`Range`].
pub fn byte_span_to_range(text: &str, span: Span, encoding: Encoding) -> Range {
    let index = PositionIndex::new(text);
    let start = index.offset_to_position(text, span.start, encoding);
    let end = index.offset_to_position(text, span.end, encoding);
    Range::new(start, end)
}

/// Maps an LSP [`Position`] to a byte offset.
pub fn position_to_byte(text: &str, position: Position, encoding: Encoding) -> usize {
    PositionIndex::new(text).position_to_offset(text, position, encoding)
}

fn encode_column(prefix: &str, encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Utf8 => prefix.len() as u32,
        Encoding::Utf16 => prefix.chars().map(char::len_utf16).sum::<usize>() as u32,
        Encoding::Utf32 => prefix.chars().count() as u32,
    }
}

fn decode_column(line: &str, column: u32, encoding: Encoding) -> usize {
    match encoding {
        Encoding::Utf8 => {
            let col = column as usize;
            if col >= line.len() {
                line.len()
            } else {
                // Snap to a char boundary if the client sent a mid-code-unit offset.
                let mut end = col.min(line.len());
                while end > 0 && !line.is_char_boundary(end) {
                    end -= 1;
                }
                end
            }
        }
        Encoding::Utf16 => {
            let mut units = 0u32;
            for (byte_idx, ch) in line.char_indices() {
                let width = ch.len_utf16() as u32;
                if units + width > column {
                    return byte_idx;
                }
                units += width;
                if units == column {
                    return byte_idx + ch.len_utf8();
                }
            }
            line.len()
        }
        Encoding::Utf32 => {
            for (count, (byte_idx, _)) in line.char_indices().enumerate() {
                if count as u32 == column {
                    return byte_idx;
                }
            }
            line.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trip() {
        let text = "begin\n  x := 1;\nend";
        let index = PositionIndex::new(text);
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for offset in [0, 5, 6, 10, text.len()] {
                let pos = index.offset_to_position(text, offset, encoding);
                let back = index.position_to_offset(text, pos, encoding);
                assert_eq!(back, offset, "encoding={encoding:?} offset={offset}");
            }
        }
    }

    #[test]
    fn utf8_multibyte_column_differs_by_encoding() {
        // "æ" is U+00E6 — 2 UTF-8 bytes, 1 UTF-16 unit, 1 UTF-32 unit.
        let text = "æx";
        let index = PositionIndex::new(text);
        let after_ae = "æ".len();
        assert_eq!(
            index.offset_to_position(text, after_ae, Encoding::Utf8),
            Position::new(0, 2)
        );
        assert_eq!(
            index.offset_to_position(text, after_ae, Encoding::Utf16),
            Position::new(0, 1)
        );
        assert_eq!(
            index.offset_to_position(text, after_ae, Encoding::Utf32),
            Position::new(0, 1)
        );
    }

    #[test]
    fn surrogate_pair_uses_two_utf16_units() {
        // U+1F600 😀 — 4 UTF-8 bytes, 2 UTF-16 units, 1 UTF-32 unit.
        let text = "😀!";
        let index = PositionIndex::new(text);
        let after_emoji = "😀".len();
        assert_eq!(
            index.offset_to_position(text, after_emoji, Encoding::Utf16),
            Position::new(0, 2)
        );
        assert_eq!(
            index.offset_to_position(text, after_emoji, Encoding::Utf8),
            Position::new(0, 4)
        );
        assert_eq!(
            index.offset_to_position(text, after_emoji, Encoding::Utf32),
            Position::new(0, 1)
        );
        let pos = Position::new(0, 2);
        assert_eq!(
            index.position_to_offset(text, pos, Encoding::Utf16),
            after_emoji
        );
    }

    #[test]
    fn empty_document() {
        let text = "";
        let index = PositionIndex::new(text);
        assert_eq!(
            index.offset_to_position(text, 0, Encoding::Utf16),
            Position::new(0, 0)
        );
        assert_eq!(
            index.position_to_offset(text, Position::new(5, 5), Encoding::Utf16),
            0
        );
    }

    #[test]
    fn crlf_line_breaks() {
        let text = "a\r\nb";
        let index = PositionIndex::new(text);
        let b_offset = text.find('b').unwrap();
        assert_eq!(
            index.offset_to_position(text, b_offset, Encoding::Utf16),
            Position::new(1, 0)
        );
    }

    #[test]
    fn clamp_past_eol_and_eof() {
        let text = "hi\n";
        let index = PositionIndex::new(text);
        assert_eq!(
            index.position_to_offset(text, Position::new(0, 99), Encoding::Utf16),
            2
        );
        assert_eq!(
            index.position_to_offset(text, Position::new(9, 0), Encoding::Utf16),
            text.len()
        );
    }

    #[test]
    fn span_to_range() {
        let text = "begin @@@";
        let range = byte_span_to_range(text, 6..9, Encoding::Utf16);
        assert_eq!(range.start, Position::new(0, 6));
        assert_eq!(range.end, Position::new(0, 9));
    }
}
