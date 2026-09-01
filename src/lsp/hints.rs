//! Inlay hints (parameter names at call sites).

use tower_lsp_server::ls_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};

use super::analysis::AnalysisSnapshot;
use super::position::{Encoding, byte_span_to_range};
use super::symbols::{SymbolIndex, SymbolKind};

/// Parameter-name inlay hints for procedure calls in `range` (or whole doc).
pub fn inlay_hints(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    range: Option<Range>,
    encoding: Encoding,
) -> Vec<InlayHint> {
    let text = &snap.text;
    let mut hints = Vec::new();
    for u in &index.uses {
        let Some(def_id) = u.definition else {
            continue;
        };
        let callee = index.symbol(def_id);
        if callee.kind != SymbolKind::Procedure {
            continue;
        }
        let after = u.span.end;
        let Some(open) = find_open_paren(text, after) else {
            continue;
        };
        let hint_pos = byte_span_to_range(text, open..open, encoding).start;
        if let Some(range) = range
            && !position_in_range(hint_pos, range)
        {
            continue;
        }
        let params: Vec<_> = index
            .children_of(def_id)
            .into_iter()
            .filter(|&c| index.symbol(c).kind == SymbolKind::Parameter)
            .collect();
        if params.is_empty() {
            continue;
        }
        let args = argument_starts(text, open);
        for (i, &arg_start) in args.iter().enumerate() {
            let Some(&param_id) = params.get(i) else {
                break;
            };
            let param = index.symbol(param_id);
            let pos = byte_span_to_range(text, arg_start..arg_start, encoding).start;
            hints.push(InlayHint {
                position: pos,
                label: InlayHintLabel::String(format!("{}:", param.name)),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: Some(false),
                padding_right: Some(true),
                data: None,
            });
        }
    }
    hints
}

fn position_in_range(pos: Position, range: Range) -> bool {
    if pos.line < range.start.line || pos.line > range.end.line {
        return false;
    }
    if pos.line == range.start.line && pos.character < range.start.character {
        return false;
    }
    if pos.line == range.end.line && pos.character > range.end.character {
        return false;
    }
    true
}

fn find_open_paren(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'(' {
        Some(i)
    } else {
        None
    }
}

fn argument_starts(text: &str, open_paren: usize) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = open_paren + 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] == b')' {
        return out;
    }
    out.push(i);
    let mut depth = 0i32;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] != b')' {
                    out.push(i);
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Helper used by tests that don't have a range filter.
#[cfg(test)]
fn inlay_hints_full(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    encoding: Encoding,
) -> Vec<InlayHint> {
    inlay_hints(snap, index, None, encoding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};

    #[test]
    fn parameter_hints_at_call() {
        let src = r#"
begin
  procedure p(a, b); integer a, b; begin end;
  p(1, 2);
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let hints = inlay_hints_full(&snap, index, Encoding::Utf16);
        assert!(
            hints.iter().any(|h| match &h.label {
                InlayHintLabel::String(s) => s.starts_with("a:"),
                _ => false,
            }),
            "{hints:?}"
        );
        assert!(
            hints.iter().any(|h| match &h.label {
                InlayHintLabel::String(s) => s.starts_with("b:"),
                _ => false,
            }),
            "{hints:?}"
        );
    }
}
