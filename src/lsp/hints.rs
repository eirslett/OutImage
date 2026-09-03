//! Inlay hints: parameter names at call sites, and types on class/procedure headings.

use tower_lsp_server::ls_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};

use crate::ast::{
    Block, ClassDeclaration, ExternalDeclaration, FormalParameter, ParamMode, ProcedureDeclaration,
    Program, Statement, StatementKind,
};
use crate::error::Span;
use crate::lex::{Keyword, Token, TokenKind};
use crate::types::Type;

use super::analysis::AnalysisSnapshot;
use super::position::{Encoding, byte_span_to_range};
use super::symbols::{SymbolIndex, SymbolKind};

/// Options for [`inlay_hints`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlayHintOptions {
    /// Ghost types after untyped formals in class/procedure headings.
    pub heading_parameter_types: bool,
}

impl Default for InlayHintOptions {
    fn default() -> Self {
        Self {
            heading_parameter_types: true,
        }
    }
}

/// Parameter-name hints at calls, plus optional heading type hints, in `range`
/// (or the whole document).
pub fn inlay_hints(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    range: Option<Range>,
    encoding: Encoding,
    options: InlayHintOptions,
) -> Vec<InlayHint> {
    let mut hints = call_parameter_name_hints(snap, index, range, encoding);
    if options.heading_parameter_types {
        hints.extend(heading_type_hints(snap, range, encoding));
    }
    hints
}

fn call_parameter_name_hints(
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

fn heading_type_hints(
    snap: &AnalysisSnapshot,
    range: Option<Range>,
    encoding: Encoding,
) -> Vec<InlayHint> {
    let Some(program) = snap.program.as_ref() else {
        return Vec::new();
    };
    let Some(tokens) = snap.tokens.as_ref() else {
        return Vec::new();
    };
    let mut sink = HeadingHintSink {
        text: &snap.text,
        tokens: &tokens.tokens,
        range,
        encoding,
        hints: Vec::new(),
    };
    walk_program(program, &mut |keyword, decl_span, parameters| {
        sink.collect(keyword, decl_span, parameters);
    });
    sink.hints
}

struct HeadingHintSink<'a> {
    text: &'a str,
    tokens: &'a [Token],
    range: Option<Range>,
    encoding: Encoding,
    hints: Vec<InlayHint>,
}

impl HeadingHintSink<'_> {
    fn collect(&mut self, keyword: Keyword, decl_span: &Span, parameters: &[FormalParameter]) {
        if parameters.is_empty() {
            return;
        }
        let slots = heading_param_slots(self.tokens, decl_span, keyword);
        for (formal, slot) in parameters.iter().zip(slots) {
            if slot.already_typed {
                continue;
            }
            let label = format_formal_inlay(formal);
            if label.is_empty() {
                continue;
            }
            if slot.name_span.end == 0 && slot.name_span.start == 0 {
                continue;
            }
            let pos = byte_span_to_range(
                self.text,
                slot.name_span.end..slot.name_span.end,
                self.encoding,
            )
            .start;
            if let Some(range) = self.range
                && !position_in_range(pos, range)
            {
                continue;
            }
            self.hints.push(InlayHint {
                position: pos,
                label: InlayHintLabel::String(format!(": {label}")),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: Some(false),
                padding_right: Some(false),
                data: None,
            });
        }
    }
}

fn format_formal_inlay(formal: &FormalParameter) -> String {
    let spec = if formal.is_label {
        "label".to_owned()
    } else if formal.is_switch {
        "switch".to_owned()
    } else if formal.is_procedure {
        match &formal.ty {
            Type::Integer { short: false } => "procedure".to_owned(),
            ty => format!("{} procedure", format_inlay_type(ty)),
        }
    } else {
        format_inlay_type(&formal.ty)
    };
    let mode = if formal.mode_explicit {
        match formal.mode {
            ParamMode::Value => Some("value"),
            ParamMode::Name => Some("name"),
            ParamMode::Reference => None,
        }
    } else {
        None
    };
    match mode {
        Some(mode) => format!("{mode} {spec}"),
        None => spec,
    }
}

fn format_inlay_type(ty: &Type) -> String {
    match ty {
        Type::Array { element, dims } if *dims == 0 => {
            format!("{} array", format_inlay_type(element))
        }
        Type::Array { element, dims } => {
            format!("{} array({dims})", format_inlay_type(element))
        }
        other => other.to_string(),
    }
}

struct ParamSlot {
    name_span: Span,
    already_typed: bool,
}

/// Formals in the heading list after `class` / `procedure` `Name ( … )`.
fn heading_param_slots(tokens: &[Token], decl_span: &Span, keyword: Keyword) -> Vec<ParamSlot> {
    let Some(kw_idx) = tokens.iter().position(|token| {
        token_in_span(token, decl_span)
            && matches!(&token.kind, TokenKind::Keyword(k) if *k == keyword)
    }) else {
        return Vec::new();
    };
    let Some(name_idx) =
        ((kw_idx + 1)..tokens.len()).find(|&i| token_in_span(&tokens[i], decl_span))
    else {
        return Vec::new();
    };
    let mut i = name_idx + 1;
    if i < tokens.len() && tokens[i].kind == TokenKind::Eq {
        i += 1;
        if i < tokens.len() && matches!(tokens[i].kind, TokenKind::StringLiteral(_)) {
            i += 1;
        }
    }
    if i >= tokens.len() || tokens[i].kind != TokenKind::LeftParen {
        return Vec::new();
    }
    i += 1;
    let mut slots = Vec::new();
    let mut slot_tokens: Vec<&Token> = Vec::new();
    let mut depth = 1i32;
    while i < tokens.len() && depth > 0 {
        match tokens[i].kind {
            TokenKind::LeftParen => {
                depth += 1;
                slot_tokens.push(&tokens[i]);
            }
            TokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    if let Some(slot) = finish_slot(&slot_tokens) {
                        slots.push(slot);
                    }
                    break;
                }
                slot_tokens.push(&tokens[i]);
            }
            TokenKind::Comma if depth == 1 => {
                if let Some(slot) = finish_slot(&slot_tokens) {
                    slots.push(slot);
                }
                slot_tokens.clear();
            }
            _ => slot_tokens.push(&tokens[i]),
        }
        i += 1;
    }
    slots
}

fn finish_slot(tokens: &[&Token]) -> Option<ParamSlot> {
    if tokens.is_empty() {
        return None;
    }
    let name = tokens
        .iter()
        .rev()
        .find(|token| matches!(token.kind, TokenKind::Identifier(_) | TokenKind::Keyword(_)))?;
    Some(ParamSlot {
        name_span: name.span.clone(),
        already_typed: tokens.len() != 1,
    })
}

fn token_in_span(token: &Token, decl_span: &Span) -> bool {
    token.span.start >= decl_span.start && token.span.end <= decl_span.end
}

fn walk_program(program: &Program, visit: &mut impl FnMut(Keyword, &Span, &[FormalParameter])) {
    for external in &program.external_head {
        walk_external(external, visit);
    }
    for block in &program.blocks {
        walk_block(block, visit);
    }
}

fn walk_block(block: &Block, visit: &mut impl FnMut(Keyword, &Span, &[FormalParameter])) {
    for external in &block.externals {
        walk_external(external, visit);
    }
    for procedure in &block.procedures {
        walk_procedure(procedure, visit);
    }
    for class in &block.classes {
        walk_class(class, visit);
    }
    for stmt in &block.statements {
        walk_statement(stmt, visit);
    }
    for nested in &block.body {
        walk_block(nested, visit);
    }
}

fn walk_external(
    external: &ExternalDeclaration,
    visit: &mut impl FnMut(Keyword, &Span, &[FormalParameter]),
) {
    if let ExternalDeclaration::Procedure(proc) = external
        && let Some(spec) = &proc.specification
    {
        walk_procedure(spec, visit);
    }
}

fn walk_procedure(
    procedure: &ProcedureDeclaration,
    visit: &mut impl FnMut(Keyword, &Span, &[FormalParameter]),
) {
    visit(Keyword::Procedure, &procedure.span, &procedure.parameters);
    walk_block(&procedure.body, visit);
}

fn walk_class(
    class: &ClassDeclaration,
    visit: &mut impl FnMut(Keyword, &Span, &[FormalParameter]),
) {
    visit(Keyword::Class, &class.span, &class.parameters);
    for virtual_spec in &class.virtual_part {
        if let Some(heading) = &virtual_spec.procedure_heading {
            walk_procedure(heading, visit);
        }
    }
    walk_block(&class.body, visit);
    for stmt in &class.tail_statements {
        walk_statement(stmt, visit);
    }
}

fn walk_statement(stmt: &Statement, visit: &mut impl FnMut(Keyword, &Span, &[FormalParameter])) {
    match &stmt.kind {
        StatementKind::Labeled { statement, .. } => walk_statement(statement, visit),
        StatementKind::While(w) => walk_statement(&w.body, visit),
        StatementKind::If(i) => {
            walk_statement(&i.then_branch, visit);
            if let Some(else_branch) = &i.else_branch {
                walk_statement(else_branch, visit);
            }
        }
        StatementKind::For(f) => walk_statement(&f.body, visit),
        StatementKind::Compound(block) => walk_block(block, visit),
        StatementKind::Inspect(inspect) => {
            for clause in &inspect.when_clauses {
                walk_statement(&clause.body, visit);
            }
            if let Some(do_clause) = &inspect.do_clause {
                walk_statement(do_clause, visit);
            }
            if let Some(otherwise) = &inspect.otherwise {
                walk_statement(otherwise, visit);
            }
        }
        _ => {}
    }
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
    inlay_hints(snap, index, None, encoding, InlayHintOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};

    fn type_labels(hints: &[InlayHint]) -> Vec<String> {
        hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::TYPE))
            .filter_map(|h| match &h.label {
                InlayHintLabel::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

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

    #[test]
    fn heading_types_on_class_formals() {
        let src = r#"
begin
  class Person (yob, yod, children, sex, father, mp, es, yo, pname);
    value pname;
    text pname;
    integer yob, yod, children;
    boolean sex;
    ref (Person) mp, es, yo, father;
  begin
  end;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let hints = inlay_hints_full(&snap, index, Encoding::Utf16);
        let labels = type_labels(&hints);
        assert!(labels.iter().any(|s| s == ": integer"), "{labels:?}");
        assert!(labels.iter().any(|s| s == ": boolean"), "{labels:?}");
        assert!(labels.iter().any(|s| s == ": ref(Person)"), "{labels:?}");
        assert!(labels.iter().any(|s| s == ": value text"), "{labels:?}");
        assert_eq!(labels.iter().filter(|s| *s == ": integer").count(), 3);
        assert_eq!(labels.iter().filter(|s| *s == ": ref(Person)").count(), 4);
        // Specification-part names must not get a second inlay.
        let spec_yob = src.find("integer yob").expect("spec");
        let spec_pos = byte_span_to_range(src, spec_yob..spec_yob, Encoding::Utf16).start;
        assert!(
            !hints.iter().any(|h| {
                h.kind == Some(InlayHintKind::TYPE) && h.position.line == spec_pos.line
            }),
            "type inlay leaked onto the specification line: {hints:?}"
        );
    }

    #[test]
    fn heading_types_on_procedure_formals() {
        let src = r#"
begin
  procedure p(a, b, t);
    name a;
    integer a, b;
    text t;
  begin
  end;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let hints = inlay_hints_full(&snap, index, Encoding::Utf16);
        let labels = type_labels(&hints);
        assert!(labels.iter().any(|s| s == ": name integer"), "{labels:?}");
        assert!(labels.iter().any(|s| s == ": integer"), "{labels:?}");
        assert!(labels.iter().any(|s| s == ": text"), "{labels:?}");
    }

    #[test]
    fn heading_types_include_arrays_and_procedures() {
        let src = r#"
begin
  procedure p(x, f);
    integer array x;
    procedure f;
  begin
  end;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let hints = inlay_hints_full(&snap, index, Encoding::Utf16);
        let labels = type_labels(&hints);
        assert!(labels.iter().any(|s| s == ": integer array"), "{labels:?}");
        assert!(labels.iter().any(|s| s == ": procedure"), "{labels:?}");
    }

    #[test]
    fn heading_types_skipped_when_already_written() {
        let src = r#"
begin
  procedure p(integer a, boolean b); begin end;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let hints = inlay_hints_full(&snap, index, Encoding::Utf16);
        assert!(
            type_labels(&hints).is_empty(),
            "typed heading should not get inlays: {:?}",
            type_labels(&hints)
        );
    }

    #[test]
    fn heading_types_can_be_disabled() {
        let src = r#"
begin
  procedure p(a); integer a; begin end;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let hints = inlay_hints(
            &snap,
            index,
            None,
            Encoding::Utf16,
            InlayHintOptions {
                heading_parameter_types: false,
            },
        );
        assert!(type_labels(&hints).is_empty(), "{hints:?}");
    }
}
