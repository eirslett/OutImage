//! Extra LSP lints layered on top of compiler diagnostics.

use crate::diagnostics::unused_binding;
use crate::error::{CompileError, Span};

use super::symbols::{SymbolId, SymbolIndex, SymbolKind};

/// Soft diagnostic produced by the language server (not the compiler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLint {
    pub span: Span,
    pub message: String,
    pub code: &'static str,
    pub unnecessary: bool,
    pub deprecated: bool,
}

/// Unused locals / parameters / labels (no references besides the declaration).
pub fn unused_symbol_lints(index: &SymbolIndex) -> Vec<LspLint> {
    unused_compile_errors(index)
        .into_iter()
        .map(|error| LspLint {
            span: error.span.clone().unwrap_or(0..0),
            message: error.message,
            code: "W0001",
            unnecessary: true,
            deprecated: false,
        })
        .collect()
}

/// Same unused set as [`unused_symbol_lints`], as catalogued [`CompileError`]s.
pub(crate) fn unused_compile_errors(index: &SymbolIndex) -> Vec<CompileError> {
    let mut out = Vec::new();
    for (i, symbol) in index.symbols.iter().enumerate() {
        if !matches!(
            symbol.kind,
            SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Label | SymbolKind::Constant
        ) {
            continue;
        }
        if symbol.is_external {
            continue;
        }
        let id = SymbolId(i);
        let refs = index.references_of(id, false);
        if refs.is_empty() {
            out.push(unused_binding(
                kind_word(symbol.kind),
                &symbol.name,
                symbol.name_span.clone(),
            ));
        }
    }
    out
}

fn kind_word(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Parameter => "parameter",
        SymbolKind::Label => "label",
        SymbolKind::Constant => "constant",
        _ => "variable",
    }
}

/// Suggest the correct Simula keyword when an identifier is a case-typo or near miss.
pub fn keyword_case_suggestion(identifier: &str) -> Option<&'static str> {
    super::symbols::all_keywords()
        .into_iter()
        .find(|&kw| kw.eq_ignore_ascii_case(identifier) && *kw != *identifier)
}

/// Edit-distance ≤ 1 keyword suggestion for unknown identifiers.
pub fn keyword_near_miss(identifier: &str) -> Option<&'static str> {
    let lower = identifier.to_ascii_lowercase();
    if keyword_case_suggestion(identifier).is_some() {
        return keyword_case_suggestion(identifier);
    }
    let mut best: Option<(&'static str, usize)> = None;
    for kw in super::symbols::all_keywords() {
        let dist = edit_distance(&lower, kw);
        if dist == 0 {
            continue;
        }
        if dist <= 2 && identifier.len() >= 4 {
            match best {
                Some((_, d)) if d <= dist => {}
                _ => best = Some((kw, dist)),
            }
        }
    }
    best.map(|(kw, _)| kw)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};

    #[test]
    fn unused_local_is_linted() {
        let snap = analyze_document(
            "begin integer x; integer y; y := 1; end",
            &AnalysisOptions::default(),
        );
        let index = snap.symbols.as_ref().unwrap();
        let lints = unused_symbol_lints(index);
        assert!(lints.iter().any(|l| l.message.contains("`x`")), "{lints:?}");
        assert!(lints.iter().any(|l| l.unnecessary));
    }

    fn assert_no_unused_label(source: &str, name: &str) {
        let snap = analyze_document(source, &AnalysisOptions::default());
        assert!(snap.ok(), "{source}\n{:?}", snap.diagnostics);
        assert!(
            !snap
                .lints
                .iter()
                .any(|l| { l.message.contains("unused label") && l.message.contains(name) }),
            "{source}\n{:?}",
            snap.lints
        );
    }

    /// simtst00 `PRINT`: label lives in `then begin … end`, `goto` is on `else`.
    #[test]
    fn goto_to_label_in_then_compound_is_not_unused() {
        assert_no_unused_label(
            "begin
               boolean ident;
               ident := true;
               if ident then goto PRINT;
               if ident then begin
                 PRINT: ident := false;
               end
               else goto PRINT;
             end",
            "PRINT",
        );
    }

    /// Nested `begin` with its own declarations is a block, but this compiler
    /// (and Simula goto) still treats the label as belonging to the enclosing
    /// procedure/program — the same false unused warning as simtst00.
    #[test]
    fn goto_to_label_in_then_block_with_locals_is_not_unused() {
        assert_no_unused_label(
            "begin
               integer x;
               x := 0;
               if x = 0 then begin
                 integer y;
                 y := 1;
                 L: x := 2;
               end
               else goto L;
             end",
            "L",
        );
    }

    #[test]
    fn goto_to_label_in_while_block_with_locals_is_not_unused() {
        assert_no_unused_label(
            "begin
               integer i;
               i := 0;
               goto L;
               while i < 1 do begin
                 integer dummy;
                 dummy := 0;
                 L: i := 1;
               end
             end",
            "L",
        );
    }

    #[test]
    fn goto_to_label_in_for_block_with_locals_is_not_unused() {
        assert_no_unused_label(
            "begin
               integer i;
               goto L;
               for i := 1 step 1 until 1 do begin
                 integer dummy;
                 dummy := 0;
                 L: dummy := 1;
               end
             end",
            "L",
        );
    }

    #[test]
    fn nested_procedure_goto_to_enclosing_block_label_is_not_unused() {
        assert_no_unused_label(
            "begin
               integer x;
               procedure p;
               begin
                 goto L;
               end;
               x := 0;
               begin
                 integer y;
                 y := 0;
                 L: x := 1;
               end
               p;
             end",
            "L",
        );
    }

    #[test]
    fn switch_element_to_label_in_nested_block_is_not_unused() {
        assert_no_unused_label(
            "begin
               integer x;
               switch s := L;
               x := 0;
               goto s(1);
               begin
                 integer y;
                 y := 0;
                 L: x := 1;
               end
             end",
            "L",
        );
    }

    #[test]
    fn unused_label_in_then_compound_is_linted() {
        let snap = analyze_document(
            "begin
               boolean ident;
               ident := true;
               if ident then begin
                 PRINT: ident := false;
               end
             end",
            &AnalysisOptions::default(),
        );
        assert!(snap.ok(), "{:?}", snap.diagnostics);
        assert!(
            snap.lints
                .iter()
                .any(|l| l.message.contains("unused label") && l.message.contains("PRINT")),
            "{:?}",
            snap.lints
        );
    }

    #[test]
    fn keyword_case_suggestion_works() {
        assert_eq!(keyword_case_suggestion("BEGIN"), Some("begin"));
        assert_eq!(keyword_case_suggestion("begin"), None);
    }

    #[test]
    fn keyword_near_miss_finds_typo() {
        assert_eq!(keyword_near_miss("begn"), Some("begin")); // missing 'i'
        assert_eq!(keyword_near_miss("whille"), Some("while"));
    }
}
