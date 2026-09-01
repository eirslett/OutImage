//! Conservative Simula document formatter (indent + trailing whitespace).

use tower_lsp_server::ls_types::{Position, Range, TextEdit};

use super::position::Encoding;

/// Formats `text` with the given indentation settings.
///
/// Always returns the formatted document (including when unchanged).
#[cfg_attr(not(test), allow(dead_code))]
pub fn format_document(text: &str, tab_size: u32, insert_spaces: bool) -> Option<String> {
    Some(format_document_inner(text, tab_size, insert_spaces))
}

/// Full-document format edits, or `None` when the buffer is already formatted.
pub fn format_edits(
    text: &str,
    tab_size: u32,
    insert_spaces: bool,
    encoding: Encoding,
) -> Option<Vec<TextEdit>> {
    let formatted = format_document_inner(text, tab_size, insert_spaces);
    if formatted == text {
        return None;
    }
    let end =
        super::position::PositionIndex::new(text).offset_to_position(text, text.len(), encoding);
    Some(vec![TextEdit {
        range: Range::new(Position::new(0, 0), end),
        new_text: formatted,
    }])
}

/// Range formatting: formats the whole document and returns an edit covering
/// only the requested range (conservative — may adjust indentation context).
pub fn format_range_edits(
    text: &str,
    range: Range,
    tab_size: u32,
    insert_spaces: bool,
    encoding: Encoding,
) -> Option<Vec<TextEdit>> {
    let index = super::position::PositionIndex::new(text);
    let start = index.position_to_offset(text, range.start, encoding);
    let end = index.position_to_offset(text, range.end, encoding);
    if start > end || end > text.len() {
        return None;
    }
    let formatted = format_document_inner(text, tab_size, insert_spaces);
    if formatted == text {
        return None;
    }
    // Map the original byte range onto the formatted document by line numbers.
    // When line counts diverge, fall back to a full-document replace.
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let orig_lines: Vec<&str> = normalized.split('\n').collect();
    let fmt_lines: Vec<&str> = formatted.split('\n').collect();
    if orig_lines.len() != fmt_lines.len() {
        return format_edits(text, tab_size, insert_spaces, encoding);
    }
    let start_line = range.start.line as usize;
    let end_line = range.end.line as usize;
    if start_line >= orig_lines.len() || end_line >= orig_lines.len() {
        return format_edits(text, tab_size, insert_spaces, encoding);
    }
    let mut new_text = String::new();
    for (i, line) in fmt_lines
        .iter()
        .enumerate()
        .take(end_line + 1)
        .skip(start_line)
    {
        if i > start_line {
            new_text.push('\n');
        }
        new_text.push_str(line);
    }
    // Preserve whether the original range included a trailing newline on the last line.
    let slice = &text[start..end];
    if slice.ends_with('\n') && !new_text.ends_with('\n') {
        new_text.push('\n');
    }
    if new_text == slice {
        return None;
    }
    Some(vec![TextEdit { range, new_text }])
}

fn format_document_inner(text: &str, tab_size: u32, insert_spaces: bool) -> String {
    if text.is_empty() {
        return String::new();
    }

    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let tab_size = tab_size.max(1);
    let indent_unit = if insert_spaces {
        " ".repeat(tab_size as usize)
    } else {
        "\t".to_owned()
    };

    let mut out = String::with_capacity(normalized.len());
    let mut depth: usize = 0;
    let lines: Vec<&str> = normalized.split('\n').collect();
    // `split` keeps a trailing empty piece when the file ends with `\n`.
    let last_idx = lines.len().saturating_sub(1);

    for (i, line) in lines.iter().enumerate() {
        // Skip the artificial empty segment after a final newline; we re-add one.
        if i == last_idx && line.is_empty() && normalized.ends_with('\n') {
            break;
        }

        let trimmed_end = line.trim_end();
        let content = trimmed_end.trim_start();

        if content.is_empty() {
            // Preserve blank lines (no trailing spaces).
            if i < last_idx || !normalized.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }

        let (opens, closes) = count_block_keywords(content);
        let leading_end = first_word_is(content, "end");
        if leading_end {
            depth = depth.saturating_sub(1);
        }

        for _ in 0..depth {
            out.push_str(&indent_unit);
        }
        out.push_str(content);
        out.push('\n');

        let closes_after = if leading_end {
            closes.saturating_sub(1)
        } else {
            closes
        };
        depth = depth.saturating_add(opens).saturating_sub(closes_after);
    }

    // Ensure a single trailing newline for non-empty output.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Counts indent openers (`begin`) and closers (`end`) as whole words,
/// case-insensitively, ignoring string/character literals.
///
/// `class` / `procedure` headings are recognized when scanning but do not
/// change depth on their own; bodies open with a following `begin`.
fn count_block_keywords(line: &str) -> (usize, usize) {
    let mut opens = 0usize;
    let mut closes = 0usize;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &line[start..i];
                if eq_ignore_ascii(word, "begin") {
                    opens += 1;
                } else if eq_ignore_ascii(word, "end") {
                    closes += 1;
                }
                // `class` / `procedure` intentionally ignored for depth.
            }
            _ => i += 1,
        }
    }
    (opens, closes)
}

fn first_word_is(line: &str, word: &str) -> bool {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut end = 0usize;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    eq_ignore_ascii(&trimmed[..end], word)
}

fn eq_ignore_ascii(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_spaces() {
        let input = "begin   \n  integer x;  \nend  \n";
        let out = format_document(input, 2, true).unwrap();
        assert!(!out.lines().any(|l| l.ends_with(' ')));
        assert!(out.ends_with('\n'));
        assert_eq!(out.matches('\n').count(), out.lines().count());
    }

    #[test]
    fn indents_nested_begin_end() {
        let input = "begin\nbegin\ninteger x;\nend;\nend\n";
        let out = format_document(input, 2, true).unwrap();
        assert_eq!(out, "begin\n  begin\n    integer x;\n  end;\nend\n");
    }

    #[test]
    fn respects_tab_size() {
        let input = "begin\ninteger x;\nend\n";
        let out2 = format_document(input, 2, true).unwrap();
        let out4 = format_document(input, 4, true).unwrap();
        assert!(out2.contains("\n  integer"));
        assert!(out4.contains("\n    integer"));
        assert!(!out4.contains("\n  integer x"));
    }

    #[test]
    fn uses_tabs_when_requested() {
        let input = "begin\ninteger x;\nend\n";
        let out = format_document(input, 4, false).unwrap();
        assert!(out.contains("\n\tinteger x;\n"));
    }

    #[test]
    fn format_edits_none_when_unchanged() {
        let text = "begin\n  integer x;\nend\n";
        assert!(format_edits(text, 2, true, Encoding::Utf16).is_none());
    }

    #[test]
    fn format_edits_replaces_whole_document() {
        let text = "begin  \ninteger x;\nend";
        let edits = format_edits(text, 2, true, Encoding::Utf16).expect("edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start, Position::new(0, 0));
        assert_eq!(edits[0].new_text, "begin\n  integer x;\nend\n");
    }

    #[test]
    fn normalizes_crlf() {
        let out = format_document("begin\r\nend\r\n", 2, true).unwrap();
        assert!(!out.contains('\r'));
        assert_eq!(out, "begin\nend\n");
    }

    #[test]
    fn empty_input() {
        assert_eq!(format_document("", 2, true).unwrap(), "");
        assert!(format_edits("", 2, true, Encoding::Utf16).is_none());
    }
}
