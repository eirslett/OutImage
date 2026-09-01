//! Sync LSP feature handlers (hover, outline, navigation, complete, rename, tokens, signature help).

use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, DocumentHighlight, DocumentHighlightKind, DocumentSymbol,
    FoldingRange, FoldingRangeKind, Hover, HoverContents, InsertTextFormat, Location,
    MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position, Range,
    SelectionRange, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensDelta, SemanticTokensEdit, SemanticTokensLegend, SignatureHelp,
    SignatureInformation, SymbolInformation, TextEdit, Uri, WorkspaceEdit,
};

use crate::ast::{
    Assignment, AssignmentRhs, Block, ClassDeclaration, DesignationalExpr, Expr, ExprKind,
    ForListElement, Program, Specifier, Statement, StatementKind, Variable,
};
use crate::error::Span;
use crate::lex::TokenKind;
use crate::lex::highlight::{HighlightSpan, highlight_spans};
use crate::types::Type;

use super::analysis::AnalysisSnapshot;
use super::position::{Encoding, PositionIndex, byte_span_to_range, position_to_byte};
use super::symbols::{
    Symbol, SymbolId, SymbolIndex, SymbolKind as SimSymbolKind, all_keywords,
    builtin_completion_names, builtin_hover, hover_markdown, keyword_hover, token_at_offset,
};

pub use super::format::format_edits;

/// Hover at `position`, if anything is resolvable.
pub fn hover(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
) -> Option<Hover> {
    if let Some(hover) = diagnostic_hover(snap, position, encoding) {
        return Some(hover);
    }
    let offset = position_to_byte(&snap.text, position, encoding);
    if let Some(id) = index.resolve_at_offset(offset) {
        let symbol = index.symbol(id);
        let range = byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding);
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover_markdown(symbol),
            }),
            range: Some(range),
        });
    }
    if let Some(tokens) = &snap.tokens
        && let Some(token) = token_at_offset(&tokens.tokens, offset)
    {
        match &token.kind {
            TokenKind::Keyword(kw) => {
                let range = byte_span_to_range(&snap.text, token.span.clone(), encoding);
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: keyword_hover(*kw),
                    }),
                    range: Some(range),
                });
            }
            TokenKind::Identifier(name) => {
                if let Some(md) = builtin_hover(name) {
                    let range = byte_span_to_range(&snap.text, token.span.clone(), encoding);
                    return Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: md,
                        }),
                        range: Some(range),
                    });
                }
            }
            _ => {}
        }
    }
    None
}

fn diagnostic_hover(
    snap: &AnalysisSnapshot,
    position: Position,
    encoding: Encoding,
) -> Option<Hover> {
    let offset = position_to_byte(&snap.text, position, encoding);
    for error in &snap.diagnostics {
        let mut spans: Vec<&crate::error::Span> = error.span.iter().collect();
        for label in &error.labels {
            spans.push(&label.span);
        }
        let hit = spans.iter().any(|span| {
            let end = span.end.max(span.start.saturating_add(1));
            offset >= span.start && offset < end
        });
        if !hit {
            continue;
        }
        let code = error.report_code();
        let mut value = if let Some(help) = super::diagnostics::report_code_help(code) {
            format!(
                "**{} {}**\n\n{}\n\n{}",
                help.code, help.title, help.summary, help.detail
            )
        } else {
            let title = error.title.as_deref().unwrap_or("");
            format!("**{code} {title}**\n\n{}", error.message)
        };
        if !error.message.is_empty() && !value.contains(&error.message) {
            value.push_str("\n\n");
            value.push_str(&error.message);
        }
        for note in &error.notes {
            value.push_str("\n\n**Note:** ");
            value.push_str(note);
        }
        for help_line in &error.helps {
            value.push_str("\n\n**Help:** ");
            value.push_str(help_line);
        }
        let range = error
            .span
            .as_ref()
            .map(|span| byte_span_to_range(&snap.text, span.clone(), encoding));
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range,
        });
    }
    None
}

/// Hierarchical document symbols.
pub fn document_symbols(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    encoding: Encoding,
) -> Vec<DocumentSymbol> {
    index
        .outline_roots()
        .into_iter()
        .filter_map(|id| to_document_symbol(snap, index, id, encoding))
        .collect()
}

fn to_document_symbol(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    id: SymbolId,
    encoding: Encoding,
) -> Option<DocumentSymbol> {
    let symbol = index.symbol(id);
    if matches!(symbol.kind, SimSymbolKind::Parameter) {
        return None;
    }
    let range = byte_span_to_range(&snap.text, symbol.full_span.clone(), encoding);
    let selection_range = byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding);
    let children: Vec<DocumentSymbol> = index
        .children_of(id)
        .into_iter()
        .filter_map(|child| to_document_symbol(snap, index, child, encoding))
        .collect();
    #[allow(deprecated)]
    Some(DocumentSymbol {
        name: symbol.name.clone(),
        detail: Some(symbol.detail.clone()),
        kind: symbol.kind.lsp_symbol_kind(),
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

/// Selection ranges (expand selection) for each cursor position.
pub fn selection_ranges(
    snap: &AnalysisSnapshot,
    positions: &[Position],
    encoding: Encoding,
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|pos| selection_range_at(snap, *pos, encoding))
        .collect()
}

fn selection_range_at(
    snap: &AnalysisSnapshot,
    position: Position,
    encoding: Encoding,
) -> SelectionRange {
    let offset = position_to_byte(&snap.text, position, encoding);
    let mut spans: Vec<Span> = Vec::new();

    if let Some(tokens) = &snap.tokens
        && let Some(token) = token_at_offset(&tokens.tokens, offset)
    {
        push_unique_span(&mut spans, token.span.clone());
    }

    if let Some(program) = &snap.program {
        if let Some(expr_span) = smallest_expr_containing(program, offset) {
            push_unique_span(&mut spans, expr_span);
        }
        if let Some(stmt_span) = smallest_statement_containing(program, offset) {
            push_unique_span(&mut spans, stmt_span);
        }
    }

    if let Some(symbols) = &snap.symbols
        && let Some(sym_span) = smallest_procedure_or_class_span(symbols, offset)
    {
        push_unique_span(&mut spans, sym_span);
    }

    let doc_end = snap.text.len();
    push_unique_span(&mut spans, 0..doc_end);

    // If we somehow have nothing (empty doc), use a point at the cursor.
    if spans.is_empty() {
        spans.push(offset..offset);
    }

    build_selection_chain(&snap.text, &spans, encoding)
}

fn push_unique_span(spans: &mut Vec<Span>, span: Span) {
    if spans.last().is_some_and(|prev| *prev == span) {
        return;
    }
    spans.push(span);
}

fn build_selection_chain(
    text: &str,
    spans_inner_to_outer: &[Span],
    encoding: Encoding,
) -> SelectionRange {
    let mut parent: Option<Box<SelectionRange>> = None;
    for span in spans_inner_to_outer.iter().rev() {
        parent = Some(Box::new(SelectionRange {
            range: byte_span_to_range(text, span.clone(), encoding),
            parent,
        }));
    }
    *parent.expect("at least one selection span")
}

fn span_contains(span: &Span, offset: usize) -> bool {
    if span.start == span.end {
        return offset == span.start;
    }
    offset >= span.start && offset < span.end
}

fn span_len(span: &Span) -> usize {
    span.end.saturating_sub(span.start)
}

fn consider_span(best: &mut Option<Span>, candidate: &Span, offset: usize) {
    if candidate.start == candidate.end || !span_contains(candidate, offset) {
        return;
    }
    match best {
        None => *best = Some(candidate.clone()),
        Some(prev) => {
            let cand_len = span_len(candidate);
            let prev_len = span_len(prev);
            if cand_len < prev_len || (cand_len == prev_len && candidate.start > prev.start) {
                *best = Some(candidate.clone());
            }
        }
    }
}

fn smallest_expr_containing(program: &Program, offset: usize) -> Option<Span> {
    let mut best = None;
    for block in &program.blocks {
        walk_block_exprs(block, offset, &mut best);
    }
    best
}

fn walk_block_exprs(block: &Block, offset: usize, best: &mut Option<Span>) {
    if let Some(prefix) = &block.prefix {
        walk_expr(prefix, offset, best);
    }
    for array in &block.arrays {
        for seg in &array.segments {
            for bound in &seg.bounds {
                walk_expr(&bound.lower, offset, best);
                walk_expr(&bound.upper, offset, best);
            }
        }
    }
    for sw in &block.switches {
        for elem in &sw.elements {
            walk_designational(elem, offset, best);
        }
    }
    for proc in &block.procedures {
        walk_block_exprs(&proc.body, offset, best);
    }
    for class in &block.classes {
        walk_block_exprs(&class.body, offset, best);
        for stmt in &class.tail_statements {
            walk_statement_exprs(stmt, offset, best);
        }
    }
    for stmt in &block.statements {
        walk_statement_exprs(stmt, offset, best);
    }
    for nested in &block.body {
        walk_block_exprs(nested, offset, best);
    }
}

fn walk_statement_exprs(stmt: &Statement, offset: usize, best: &mut Option<Span>) {
    match &stmt.kind {
        StatementKind::ProcedureCall(call) => {
            for arg in &call.arguments {
                walk_expr(arg, offset, best);
            }
        }
        StatementKind::Assignment(assign) => walk_assignment(assign, offset, best),
        StatementKind::If(i) => {
            walk_expr(&i.condition, offset, best);
            walk_statement_exprs(&i.then_branch, offset, best);
            if let Some(else_branch) = &i.else_branch {
                walk_statement_exprs(else_branch, offset, best);
            }
        }
        StatementKind::While(w) => {
            walk_expr(&w.condition, offset, best);
            walk_statement_exprs(&w.body, offset, best);
        }
        StatementKind::For(f) => {
            for element in &f.elements {
                walk_for_element(element, offset, best);
            }
            walk_statement_exprs(&f.body, offset, best);
        }
        StatementKind::Goto(g) => walk_designational(&g.target, offset, best),
        StatementKind::Compound(block) => walk_block_exprs(block, offset, best),
        StatementKind::Labeled { statement, .. } => walk_statement_exprs(statement, offset, best),
        StatementKind::Expr(expr) => walk_expr(expr, offset, best),
        StatementKind::ObjectGenerator(obj) => {
            for arg in &obj.arguments {
                walk_expr(arg, offset, best);
            }
        }
        StatementKind::Inspect(inspect) => {
            walk_expr(&inspect.object, offset, best);
            for clause in &inspect.when_clauses {
                walk_statement_exprs(&clause.body, offset, best);
            }
            if let Some(do_clause) = &inspect.do_clause {
                walk_statement_exprs(do_clause, offset, best);
            }
            if let Some(otherwise) = &inspect.otherwise {
                walk_statement_exprs(otherwise, offset, best);
            }
        }
        StatementKind::Activate(a) => {
            walk_expr(&a.target, offset, best);
            if let Some(timing) = &a.timing {
                use crate::ast::SimulationTiming::*;
                let e = match timing {
                    Delay(e) | After(e) | At(e) | Before(e) => e,
                };
                walk_expr(e, offset, best);
            }
        }
        StatementKind::Reactivate(r) => {
            walk_expr(&r.target, offset, best);
            if let Some(timing) = &r.timing {
                use crate::ast::SimulationTiming::*;
                let e = match timing {
                    Delay(e) | After(e) | At(e) | Before(e) => e,
                };
                walk_expr(e, offset, best);
            }
        }
        StatementKind::Dummy | StatementKind::Inner { .. } => {}
    }
}

fn walk_assignment(assign: &Assignment, offset: usize, best: &mut Option<Span>) {
    walk_variable(&assign.lhs, offset, best);
    match &assign.rhs {
        AssignmentRhs::Expr(expr) => walk_expr(expr, offset, best),
        AssignmentRhs::Chain(inner) => walk_assignment(inner, offset, best),
    }
}

fn walk_for_element(element: &ForListElement, offset: usize, best: &mut Option<Span>) {
    match element {
        ForListElement::Value { expr, while_cond }
        | ForListElement::Reference { expr, while_cond } => {
            walk_expr(expr, offset, best);
            if let Some(cond) = while_cond {
                walk_expr(cond, offset, best);
            }
        }
        ForListElement::StepUntil { start, step, until } => {
            walk_expr(start, offset, best);
            walk_expr(step, offset, best);
            walk_expr(until, offset, best);
        }
    }
}

fn walk_designational(expr: &DesignationalExpr, offset: usize, best: &mut Option<Span>) {
    match expr {
        DesignationalExpr::Label(_) => {}
        DesignationalExpr::SwitchDesignator { subscript, .. } => {
            walk_expr(subscript, offset, best);
        }
        DesignationalExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, offset, best);
            walk_designational(then_expr, offset, best);
            walk_designational(else_expr, offset, best);
        }
        DesignationalExpr::Paren(inner) => walk_designational(inner, offset, best),
    }
}

fn walk_variable(var: &Variable, offset: usize, best: &mut Option<Span>) {
    match var {
        Variable::Simple(_) => {}
        Variable::Subscripted { subscripts, .. } => {
            for sub in subscripts {
                walk_expr(sub, offset, best);
            }
        }
        Variable::Qua { object, .. } => walk_variable(object, offset, best),
        Variable::Remote { object, .. } => walk_variable(object, offset, best),
        Variable::RemoteCall {
            object, arguments, ..
        } => {
            walk_variable(object, offset, best);
            for arg in arguments {
                walk_expr(arg, offset, best);
            }
        }
    }
}

fn walk_expr(expr: &Expr, offset: usize, best: &mut Option<Span>) {
    consider_span(best, &expr.span, offset);
    match &expr.kind {
        ExprKind::Variable(var) => walk_variable(var, offset, best),
        ExprKind::Unary { operand, .. } => walk_expr(operand, offset, best),
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            walk_expr(left, offset, best);
            walk_expr(right, offset, best);
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, offset, best);
            walk_expr(then_expr, offset, best);
            walk_expr(else_expr, offset, best);
        }
        ExprKind::Paren(inner) => walk_expr(inner, offset, best),
        ExprKind::FunctionCall { arguments, .. } => {
            for arg in arguments {
                walk_expr(arg, offset, best);
            }
        }
        ExprKind::RemoteAccess { object, .. } => walk_expr(object, offset, best),
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            walk_expr(object, offset, best);
            for arg in arguments {
                walk_expr(arg, offset, best);
            }
        }
        ExprKind::New { arguments, .. } => {
            if let Some(args) = arguments {
                for arg in args {
                    walk_expr(arg, offset, best);
                }
            }
        }
        ExprKind::Qua { object, .. } => walk_expr(object, offset, best),
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::This(_) => {}
    }
}

fn smallest_statement_containing(program: &Program, offset: usize) -> Option<Span> {
    let mut best = None;
    for block in &program.blocks {
        walk_block_statements(block, offset, &mut best);
    }
    best
}

fn walk_block_statements(block: &Block, offset: usize, best: &mut Option<Span>) {
    for proc in &block.procedures {
        walk_block_statements(&proc.body, offset, best);
    }
    for class in &block.classes {
        walk_block_statements(&class.body, offset, best);
        for stmt in &class.tail_statements {
            walk_statement(stmt, offset, best);
        }
    }
    for stmt in &block.statements {
        walk_statement(stmt, offset, best);
    }
    for nested in &block.body {
        walk_block_statements(nested, offset, best);
    }
}

fn walk_statement(stmt: &Statement, offset: usize, best: &mut Option<Span>) {
    consider_span(best, &stmt.span, offset);
    match &stmt.kind {
        StatementKind::If(i) => {
            walk_statement(&i.then_branch, offset, best);
            if let Some(else_branch) = &i.else_branch {
                walk_statement(else_branch, offset, best);
            }
        }
        StatementKind::While(w) => walk_statement(&w.body, offset, best),
        StatementKind::For(f) => walk_statement(&f.body, offset, best),
        StatementKind::Compound(block) => walk_block_statements(block, offset, best),
        StatementKind::Labeled { statement, .. } => walk_statement(statement, offset, best),
        StatementKind::Inspect(inspect) => {
            for clause in &inspect.when_clauses {
                walk_statement(&clause.body, offset, best);
            }
            if let Some(do_clause) = &inspect.do_clause {
                walk_statement(do_clause, offset, best);
            }
            if let Some(otherwise) = &inspect.otherwise {
                walk_statement(otherwise, offset, best);
            }
        }
        StatementKind::ProcedureCall(_)
        | StatementKind::Assignment(_)
        | StatementKind::Goto(_)
        | StatementKind::Expr(_)
        | StatementKind::ObjectGenerator(_)
        | StatementKind::Activate(_)
        | StatementKind::Reactivate(_)
        | StatementKind::Dummy
        | StatementKind::Inner { .. } => {}
    }
}

fn smallest_procedure_or_class_span(symbols: &SymbolIndex, offset: usize) -> Option<Span> {
    let mut best = None;
    for sym in &symbols.symbols {
        if matches!(sym.kind, SimSymbolKind::Procedure | SimSymbolKind::Class) {
            consider_span(&mut best, &sym.full_span, offset);
        }
    }
    best
}

/// Folding ranges for `begin`/`end` blocks and procedure/class bodies.
pub fn folding_ranges(snap: &AnalysisSnapshot, encoding: Encoding) -> Vec<FoldingRange> {
    let Some(program) = &snap.program else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    for block in &program.blocks {
        collect_folds(block, &snap.text, encoding, &mut ranges);
    }
    // Also fold from tokens: begin..end pairs via AST block spans from decls.
    for sym_span in procedure_class_spans(program) {
        push_fold(&snap.text, encoding, &sym_span, &mut ranges);
    }
    ranges
}

fn procedure_class_spans(program: &Program) -> Vec<Span> {
    let mut out = Vec::new();
    fn walk_block(block: &Block, out: &mut Vec<Span>) {
        for p in &block.procedures {
            out.push(p.span.clone());
            walk_block(&p.body, out);
        }
        for c in &block.classes {
            out.push(c.span.clone());
            walk_block(&c.body, out);
        }
        for nested in &block.body {
            walk_block(nested, out);
        }
    }
    for block in &program.blocks {
        walk_block(block, &mut out);
    }
    out
}

fn collect_folds(block: &Block, text: &str, encoding: Encoding, out: &mut Vec<FoldingRange>) {
    for nested in &block.body {
        // Nested blocks don't carry spans; fold via contained statements.
        if let (Some(first), Some(last)) = (nested.statements.first(), nested.statements.last()) {
            let span = first.span.start..last.span.end;
            push_fold(text, encoding, &span, out);
        }
        collect_folds(nested, text, encoding, out);
    }
    for p in &block.procedures {
        push_fold(text, encoding, &p.span, out);
        collect_folds(&p.body, text, encoding, out);
    }
    for c in &block.classes {
        push_fold(text, encoding, &c.span, out);
        collect_folds(&c.body, text, encoding, out);
    }
}

fn push_fold(text: &str, encoding: Encoding, span: &Span, out: &mut Vec<FoldingRange>) {
    let range = byte_span_to_range(text, span.clone(), encoding);
    if range.start.line >= range.end.line {
        return;
    }
    out.push(FoldingRange {
        start_line: range.start.line,
        start_character: Some(range.start.character),
        end_line: range.end.line,
        end_character: Some(range.end.character),
        kind: Some(FoldingRangeKind::Region),
        collapsed_text: None,
    });
}

#[cfg(test)]
pub fn goto_definition(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
) -> Option<Location> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let id = index.resolve_at_offset(offset)?;
    let symbol = index.symbol(id);
    Some(Location::new(
        uri.clone(),
        byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
    ))
}

/// Jump from a `ref(C)` variable (or similar) to the class declaration of `C`.
pub fn goto_type_definition(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
) -> Option<Location> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let id = index.resolve_at_offset(offset)?;
    let symbol = index.symbol(id);
    let class_name = match &symbol.ty {
        Some(Type::ObjectRef(class)) => class.as_str(),
        Some(Type::Array { element, .. }) => match element.as_ref() {
            Type::ObjectRef(class) => class.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    let class_id = index.find_class(class_name)?;
    let class = index.symbol(class_id);
    Some(Location::new(
        uri.clone(),
        byte_span_to_range(&snap.text, class.name_span.clone(), encoding),
    ))
}

#[cfg(test)]
pub fn find_references(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
    include_declaration: bool,
) -> Vec<Location> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let Some(id) = index.resolve_at_offset(offset) else {
        return Vec::new();
    };
    index
        .references_of(id, include_declaration)
        .into_iter()
        .map(|(span, _)| Location::new(uri.clone(), byte_span_to_range(&snap.text, span, encoding)))
        .collect()
}

pub fn document_highlight(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
) -> Vec<DocumentHighlight> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let Some(id) = index.resolve_at_offset(offset) else {
        return Vec::new();
    };
    index
        .references_of(id, true)
        .into_iter()
        .map(|(span, is_write)| DocumentHighlight {
            range: byte_span_to_range(&snap.text, span, encoding),
            kind: Some(if is_write {
                DocumentHighlightKind::WRITE
            } else {
                DocumentHighlightKind::READ
            }),
        })
        .collect()
}

/// Signature help for a procedure call at `position`, if an open call is found.
pub fn signature_help(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
) -> Option<SignatureHelp> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let (name, open_paren, active_parameter) = find_call_site(&snap.text, offset)?;
    let scope = index.scope_at_offset(open_paren);
    let id = index.lookup(scope, &name)?;
    let symbol = index.symbol(id);
    if !matches!(symbol.kind, SimSymbolKind::Procedure) {
        return None;
    }
    let parameters: Vec<ParameterInformation> = index
        .children_of(id)
        .into_iter()
        .filter(|&child| matches!(index.symbol(child).kind, SimSymbolKind::Parameter))
        .map(|child| {
            let param = index.symbol(child);
            ParameterInformation {
                label: ParameterLabel::Simple(param.detail.clone()),
                documentation: None,
            }
        })
        .collect();
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: symbol.detail.clone(),
            documentation: None,
            parameters: if parameters.is_empty() {
                None
            } else {
                Some(parameters)
            },
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

/// Scan backward from `offset` for an unmatched `(` preceded by an identifier.
/// Returns `(identifier, open_paren_byte_offset, active_parameter_index)`.
fn find_call_site(text: &str, offset: usize) -> Option<(String, usize, u32)> {
    let bytes = text.as_bytes();
    let mut i = offset.min(bytes.len());
    let mut depth: i32 = 0;
    let mut active_parameter: u32 = 0;
    let mut open_paren = None;

    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    open_paren = Some(i);
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                active_parameter = active_parameter.saturating_add(1);
            }
            _ => {}
        }
    }
    let open = open_paren?;

    let mut end = open;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == end {
        return None;
    }
    // Identifiers must start with a letter or underscore.
    let first = bytes[start];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    Some((text[start..end].to_owned(), open, active_parameter))
}

/// Options derived from negotiated client capabilities.
#[derive(Debug, Clone, Copy)]
pub struct CompletionOptions {
    pub include_snippets: bool,
    /// Soft cap; when exceeded, callers should mark the list incomplete.
    pub max_items: usize,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            include_snippets: true,
            max_items: 500,
        }
    }
}

/// Completions plus whether the list was truncated (`isIncomplete`).
pub fn completions_list(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
    options: &CompletionOptions,
) -> (Vec<CompletionItem>, bool) {
    let mut items = completions_with_options(snap, index, position, encoding, options);
    let incomplete = items.len() > options.max_items;
    if incomplete {
        items.truncate(options.max_items);
    }
    (items, incomplete)
}

fn completions_with_options(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
    options: &CompletionOptions,
) -> Vec<CompletionItem> {
    let offset = position_to_byte(&snap.text, position, encoding);

    if let Some(receiver) = dot_receiver_name(&snap.text, offset) {
        if let Some(program) = &snap.program
            && let Some(class_name) = object_class_for_receiver(index, &receiver, offset)
        {
            return class_attribute_completions(program, &class_name);
        }
        return Vec::new();
    }

    let scope = index.scope_at_offset(offset);
    let mut items = Vec::new();

    for (sort_i, id) in index.completions_in_scope(scope).into_iter().enumerate() {
        let symbol = index.symbol(id);
        items.push(CompletionItem {
            label: symbol.name.clone(),
            kind: Some(symbol.kind.lsp_completion_kind()),
            detail: Some(symbol.detail.clone()),
            sort_text: Some(format!("0{sort_i:04}")),
            filter_text: Some(symbol.name.clone()),
            ..CompletionItem::default()
        });
    }

    for kw in all_keywords() {
        items.push(CompletionItem {
            label: kw.to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            sort_text: Some(format!("1{kw}")),
            filter_text: Some(kw.to_owned()),
            ..CompletionItem::default()
        });
    }

    for name in builtin_completion_names() {
        items.push(CompletionItem {
            label: name.to_owned(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("ENVIRONMENT".into()),
            sort_text: Some(format!("2{name}")),
            filter_text: Some(name.to_owned()),
            ..CompletionItem::default()
        });
    }

    if options.include_snippets {
        for (label, snippet, detail) in completion_snippets() {
            items.push(CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::SNIPPET),
                detail: Some(detail.to_string()),
                insert_text: Some(snippet.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                sort_text: Some(format!("3{label}")),
                filter_text: Some(label.to_string()),
                ..CompletionItem::default()
            });
        }
    }

    // Deduplicate by label (case-insensitive), prefer earlier (scope) entries.
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| {
        if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
            return seen.insert(format!("snippet:{}", item.label.to_ascii_lowercase()));
        }
        seen.insert(item.label.to_ascii_lowercase())
    });
    let _ = snap;
    items
}

fn dot_receiver_name(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = offset.min(bytes.len());
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    i -= 1;
    let end = i;
    while i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        i -= 1;
    }
    if i == end {
        return None;
    }
    let name = &text[i..end];
    let first = bytes[i];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    Some(name.to_owned())
}

fn object_class_for_receiver(index: &SymbolIndex, receiver: &str, offset: usize) -> Option<String> {
    let scope = index.scope_at_offset(offset);
    let id = index.lookup(scope, receiver)?;
    let sym = index.symbol(id);
    match &sym.ty {
        Some(Type::ObjectRef(class)) => Some(class.clone()),
        _ => None,
    }
}

fn class_attribute_completions(program: &Program, class_name: &str) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for_each_class(program, |class| {
        if !class.name.eq_ignore_ascii_case(class_name) {
            return;
        }
        for (name, detail) in attributes_of_class(class) {
            if seen.insert(name.to_ascii_lowercase()) {
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(detail),
                    ..CompletionItem::default()
                });
            }
        }
    });
    items
}

fn for_each_class(program: &Program, mut visit: impl FnMut(&ClassDeclaration)) {
    fn walk_block(block: &Block, visit: &mut dyn FnMut(&ClassDeclaration)) {
        for class in &block.classes {
            visit(class);
            walk_class_body(class, visit);
        }
        for procedure in &block.procedures {
            walk_block(&procedure.body, visit);
        }
        for nested in &block.body {
            walk_block(nested, visit);
        }
    }
    fn walk_class_body(class: &ClassDeclaration, visit: &mut dyn FnMut(&ClassDeclaration)) {
        walk_block(&class.body, visit);
    }
    for block in &program.blocks {
        walk_block(block, &mut visit);
    }
}

fn attributes_of_class(class: &ClassDeclaration) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for param in &class.parameters {
        out.push((param.name.clone(), param.ty.to_string()));
    }
    for spec in &class.specifications {
        let detail = specifier_detail(&spec.specifier);
        for name in &spec.names {
            out.push((name.clone(), detail.clone()));
        }
    }
    for decl in &class.body.declarations {
        for item in &decl.items {
            out.push((item.name.clone(), decl.ty.to_string()));
        }
    }
    for array in &class.body.arrays {
        for segment in &array.segments {
            for name in &segment.names {
                out.push((name.clone(), format!("{} {}", array.element_type, name)));
            }
        }
    }
    for procedure in &class.body.procedures {
        let detail = procedure
            .result_type
            .as_ref()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "procedure".into());
        out.push((procedure.name.clone(), detail));
    }
    for switch in &class.body.switches {
        out.push((switch.name.clone(), "switch".into()));
    }
    for spec in &class.virtual_part {
        let detail = specifier_detail(&spec.specifier);
        for name in &spec.names {
            out.push((name.clone(), detail.clone()));
        }
    }
    out
}

fn specifier_detail(specifier: &Specifier) -> String {
    match specifier {
        Specifier::Type(ty) => ty.to_string(),
        Specifier::TypeArray(ty) => format!("array of {ty}"),
        Specifier::Array => "array".into(),
        Specifier::Label | Specifier::Switch => "integer".into(),
        Specifier::Procedure => "procedure".into(),
        Specifier::TypeProcedure(ty) => ty.to_string(),
    }
}

fn completion_snippets() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        (
            "procedure",
            "procedure ${1:name}(${2:}); ${3:begin} end;",
            "procedure heading",
        ),
        (
            "class",
            "class ${1:Name}; begin end ${1:Name};",
            "class declaration",
        ),
        ("if", "if ${1:cond} then ${2:stmt}", "if statement"),
        ("while", "while ${1:cond} do ${2:stmt}", "while loop"),
        (
            "for",
            "for ${1:var} := ${2:start} step ${3:step} until ${4:limit} do ${5:stmt}",
            "for loop",
        ),
    ]
}

pub fn prepare_rename(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
) -> Option<Range> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let id = index.resolve_at_offset(offset)?;
    let symbol = index.symbol(id);
    if matches!(symbol.kind, SimSymbolKind::Builtin) {
        return None;
    }
    Some(byte_span_to_range(
        &snap.text,
        symbol.name_span.clone(),
        encoding,
    ))
}

pub fn rename(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    if new_name.is_empty()
        || new_name
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '_')
    {
        return None;
    }
    let offset = position_to_byte(&snap.text, position, encoding);
    let id = index.resolve_at_offset(offset)?;
    let symbol = index.symbol(id);
    if matches!(symbol.kind, SimSymbolKind::Builtin) {
        return None;
    }
    let edits: Vec<TextEdit> = index
        .references_of(id, true)
        .into_iter()
        .map(|(span, _)| TextEdit {
            range: byte_span_to_range(&snap.text, span, encoding),
            new_text: new_name.to_owned(),
        })
        .collect();
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// Semantic token legend advertised to clients.
///
/// Type names follow the TextMate scopes (`comment.block`, `storage.type`,
/// `entity.name.function`, …). Custom Simula types (`boolean`, `character`,
/// `parenthesis`, `semicolon`, …) describe the language directly; VS Code maps
/// them back to those scopes via `semanticTokenScopes` so themes still color them.
pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: LEGEND_TYPES.iter().cloned().collect(),
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::DEFAULT_LIBRARY,
        ],
    }
}

const LEGEND_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,      // 0  keyword.control.* / keyword.other.*
    SemanticTokenType::VARIABLE,     // 1  variable
    SemanticTokenType::FUNCTION,     // 2  entity.name.function
    SemanticTokenType::CLASS,        // 3  entity.name.class
    SemanticTokenType::NUMBER,       // 4  constant.numeric.*
    SemanticTokenType::STRING,       // 5  string / string.quoted.double
    SemanticTokenType::OPERATOR,     // 6  keyword.operator*
    SemanticTokenType::PARAMETER,    // 7  variable.parameter
    SemanticTokenType::TYPE,         // 8  storage.type
    SemanticTokenType::MODIFIER,     // 9  storage.modifier
    SemanticTokenType::COMMENT,      // 10 comment.block
    SemanticTokenType::NAMESPACE,    // 11 entity.name.other
    SemanticTokenType::new("label"), // 12 entity.name.label
    SemanticTokenType::new("boolean"), // 13 constant.language.bool
    SemanticTokenType::new("null"),  // 14 constant.language.null
    SemanticTokenType::new("character"), // 15 constant.character
    SemanticTokenType::new("commentDirective"), // 16 comment.directive
    SemanticTokenType::new("parenthesis"), // 17 punctuation.section.parens
    SemanticTokenType::new("bracket"), // 18 punctuation.section.brackets
    SemanticTokenType::new("punctuation"), // 19 comma / colon / dot / quotes
    SemanticTokenType::new("semicolon"), // 20 punctuation.terminator.statement
];

const TOKEN_KEYWORD: u32 = 0;
const TOKEN_VARIABLE: u32 = 1;
const TOKEN_FUNCTION: u32 = 2;
const TOKEN_CLASS: u32 = 3;
const TOKEN_NUMBER: u32 = 4;
const TOKEN_STRING: u32 = 5;
const TOKEN_OPERATOR: u32 = 6;
const TOKEN_PARAMETER: u32 = 7;
const TOKEN_TYPE: u32 = 8;
const TOKEN_MODIFIER: u32 = 9;
const TOKEN_COMMENT: u32 = 10;
const TOKEN_NAMESPACE: u32 = 11;
const TOKEN_LABEL: u32 = 12;
const TOKEN_BOOLEAN: u32 = 13;
const TOKEN_NULL: u32 = 14;
const TOKEN_CHARACTER: u32 = 15;
const TOKEN_COMMENT_DIRECTIVE: u32 = 16;
const TOKEN_PARENTHESIS: u32 = 17;
const TOKEN_BRACKET: u32 = 18;
const TOKEN_PUNCTUATION: u32 = 19;
const TOKEN_SEMICOLON: u32 = 20;

const MOD_DECLARATION: u32 = 1;
const MOD_READONLY: u32 = 2;
const MOD_DEFAULT_LIBRARY: u32 = 4;

pub fn semantic_tokens_full(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    encoding: Encoding,
) -> SemanticTokens {
    encode_semantic_tokens(snap, index, encoding, None)
}

/// Semantic tokens overlapping `range` (inclusive of any token that touches it).
pub fn semantic_tokens_range(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    encoding: Encoding,
    range: Range,
) -> SemanticTokens {
    encode_semantic_tokens(snap, index, encoding, Some(range))
}

/// Build a delta from a previously returned token stream to `next`.
///
/// Uses a common-prefix / common-suffix edit. When `previous` is [`None`], returns
/// a full `SemanticTokens` payload (client should treat it as a reset).
pub fn semantic_tokens_delta(
    previous: Option<&[SemanticToken]>,
    next: SemanticTokens,
) -> Result<SemanticTokensDelta, SemanticTokens> {
    let Some(previous) = previous else {
        return Err(next);
    };
    let new_id = next.result_id.clone();
    let next_data = next.data;
    if previous == next_data.as_slice() {
        return Ok(SemanticTokensDelta {
            result_id: new_id,
            edits: Vec::new(),
        });
    }

    let mut prefix = 0usize;
    let max_prefix = previous.len().min(next_data.len());
    while prefix < max_prefix && previous[prefix] == next_data[prefix] {
        prefix += 1;
    }

    let mut suffix = 0usize;
    let max_suffix = previous.len().min(next_data.len()) - prefix;
    while suffix < max_suffix
        && previous[previous.len() - 1 - suffix] == next_data[next_data.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let delete_count = (previous.len() - prefix - suffix) as u32;
    let insert = next_data[prefix..next_data.len() - suffix].to_vec();
    Ok(SemanticTokensDelta {
        result_id: new_id,
        edits: vec![SemanticTokensEdit {
            start: prefix as u32,
            delete_count,
            data: if insert.is_empty() {
                None
            } else {
                Some(insert)
            },
        }],
    })
}

fn encode_semantic_tokens(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    encoding: Encoding,
    range_filter: Option<Range>,
) -> SemanticTokens {
    let Some(tokens) = &snap.tokens else {
        return SemanticTokens {
            result_id: None,
            data: Vec::new(),
        };
    };
    let pos_index = PositionIndex::new(&snap.text);
    let mut encoded = Vec::new();
    let mut last_line = 0u32;
    let mut last_start = 0u32;

    for span in highlight_spans(&snap.text, &tokens.tokens, &snap.trivia) {
        let Some((ty, modifiers)) = classify_highlight_span(&span, index, &snap.text) else {
            continue;
        };
        for (seg_start, seg_end) in line_segments(&snap.text, span.span.start, span.span.end) {
            let start = pos_index.offset_to_position(&snap.text, seg_start, encoding);
            let end = pos_index.offset_to_position(&snap.text, seg_end, encoding);
            if let Some(range) = &range_filter
                && !token_overlaps_range(start, end, range)
            {
                continue;
            }
            let length = encode_length(&snap.text[seg_start..seg_end], encoding);
            if length == 0 {
                continue;
            }
            let delta_line = start.line.saturating_sub(last_line);
            let delta_start = if delta_line == 0 {
                start.character.saturating_sub(last_start)
            } else {
                start.character
            };
            encoded.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type: ty,
                token_modifiers_bitset: modifiers,
            });
            last_line = start.line;
            last_start = start.character;
        }
    }

    SemanticTokens {
        result_id: None,
        data: encoded,
    }
}

/// Split a byte span into per-line segments (LSP tokens cannot cross `\n`).
fn line_segments(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let start = start.min(text.len());
    let end = end.min(text.len());
    if start >= end {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let rest = &text[cursor..end];
        match rest.find('\n') {
            Some(rel) => {
                let line_end = cursor + rel;
                if line_end > cursor {
                    let trimmed = if text.as_bytes()[line_end - 1] == b'\r' {
                        line_end - 1
                    } else {
                        line_end
                    };
                    if trimmed > cursor {
                        out.push((cursor, trimmed));
                    }
                }
                cursor = line_end + 1;
            }
            None => {
                out.push((cursor, end));
                break;
            }
        }
    }
    out
}

fn token_overlaps_range(start: Position, end: Position, range: &Range) -> bool {
    // Token ends at or before range start → no overlap.
    if end.line < range.start.line
        || (end.line == range.start.line && end.character <= range.start.character)
    {
        return false;
    }
    // Token starts at or after range end → no overlap.
    if start.line > range.end.line
        || (start.line == range.end.line && start.character >= range.end.character)
    {
        return false;
    }
    true
}

/// Fuzzy-ish workspace symbol search over open-document indexes.
pub fn workspace_symbols(
    docs: &[(Uri, &AnalysisSnapshot, &SymbolIndex)],
    query: &str,
    encoding: Encoding,
) -> Vec<SymbolInformation> {
    let query = query.trim();
    let query_lower = query.to_ascii_lowercase();
    let mut out = Vec::new();
    for (uri, snap, index) in docs {
        for symbol in &index.symbols {
            if matches!(symbol.kind, SimSymbolKind::Builtin) {
                continue;
            }
            if !query_lower.is_empty() && !symbol.name.to_ascii_lowercase().contains(&query_lower) {
                continue;
            }
            let container_name = symbol.container.map(|id| index.symbol(id).name.clone());
            #[allow(deprecated)]
            out.push(SymbolInformation {
                name: symbol.name.clone(),
                kind: symbol.kind.lsp_symbol_kind(),
                tags: None,
                deprecated: None,
                location: Location::new(
                    uri.clone(),
                    byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
                ),
                container_name,
            });
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.location.uri.as_str().cmp(b.location.uri.as_str()))
    });
    out
}

fn encode_length(slice: &str, encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Utf8 => slice.len() as u32,
        Encoding::Utf16 => slice.chars().map(char::len_utf16).sum::<usize>() as u32,
        Encoding::Utf32 => slice.chars().count() as u32,
    }
}

fn classify_highlight_span(
    span: &HighlightSpan,
    index: &SymbolIndex,
    text: &str,
) -> Option<(u32, u32)> {
    let mut ty = type_for_tm_scope(span.scope)?;
    let mut modifiers = 0u32;
    if is_identifier_scope(span.scope) {
        if let Some(id) = index.symbol_at_offset(span.span.start) {
            let (sym_ty, extra) = token_type_for_symbol(index.symbol(id), true);
            ty = sym_ty;
            modifiers |= extra;
        } else if let Some(u) = index.use_at_offset(span.span.start)
            && let Some(id) = u.definition
        {
            let (sym_ty, extra) = token_type_for_symbol(index.symbol(id), false);
            ty = sym_ty;
            modifiers |= extra;
        } else if let Some(name) = text.get(span.span.clone()) {
            if crate::environment::is_environment_procedure(name)
                || crate::environment::is_environment_constant(name)
            {
                ty = TOKEN_FUNCTION;
                modifiers |= MOD_DEFAULT_LIBRARY;
            }
        }
    }
    Some((ty, modifiers))
}

fn is_identifier_scope(scope: &str) -> bool {
    matches!(
        scope,
        "variable"
            | "variable.parameter"
            | "entity.name.class"
            | "entity.name.function"
            | "entity.name.label"
            | "entity.name.other"
    )
}

fn type_for_tm_scope(scope: &str) -> Option<u32> {
    Some(match scope {
        "comment.directive" => TOKEN_COMMENT_DIRECTIVE,
        scope if scope.starts_with("comment") => TOKEN_COMMENT,
        "storage.type" => TOKEN_TYPE,
        "storage.modifier" => TOKEN_MODIFIER,
        "keyword.operator"
        | "keyword.operator.assignment"
        | "keyword.operator.arithmetic"
        | "keyword.operator.comparison" => TOKEN_OPERATOR,
        "constant.language.bool" | "constant.language.boolean" => TOKEN_BOOLEAN,
        "constant.language.null" => TOKEN_NULL,
        "constant.numeric.radix" | "constant.numeric.decimal" => TOKEN_NUMBER,
        "constant.character" => TOKEN_CHARACTER,
        "string" | "string.quoted.double" | "string.quoted.single" => TOKEN_STRING,
        "entity.name.class" => TOKEN_CLASS,
        "entity.name.function" => TOKEN_FUNCTION,
        "entity.name.label" => TOKEN_LABEL,
        "entity.name.other" => TOKEN_NAMESPACE,
        "variable" => TOKEN_VARIABLE,
        "variable.parameter" => TOKEN_PARAMETER,
        "punctuation.section.parens" => TOKEN_PARENTHESIS,
        "punctuation.section.brackets" => TOKEN_BRACKET,
        "punctuation.accessor" | "punctuation.separator" | "punctuation.definition.character" => {
            TOKEN_PUNCTUATION
        }
        "punctuation.terminator.statement" => TOKEN_SEMICOLON,
        scope if scope.starts_with("keyword.") => TOKEN_KEYWORD,
        other => {
            debug_assert!(false, "unmapped highlight scope: {other}");
            TOKEN_KEYWORD
        }
    })
}

fn token_type_for_symbol(sym: &Symbol, is_declaration: bool) -> (u32, u32) {
    let mut mods = 0;
    if is_declaration {
        mods |= MOD_DECLARATION;
    }
    if matches!(sym.kind, SimSymbolKind::Constant) {
        mods |= MOD_READONLY;
    }
    if matches!(sym.kind, SimSymbolKind::Builtin) {
        mods |= MOD_DEFAULT_LIBRARY;
    }
    let ty = match sym.kind {
        SimSymbolKind::Procedure | SimSymbolKind::Builtin => TOKEN_FUNCTION,
        SimSymbolKind::Class => TOKEN_CLASS,
        SimSymbolKind::Parameter => TOKEN_PARAMETER,
        SimSymbolKind::Label => TOKEN_LABEL,
        SimSymbolKind::Variable
        | SimSymbolKind::Constant
        | SimSymbolKind::Array
        | SimSymbolKind::Switch => TOKEN_VARIABLE,
    };
    (ty, mods)
}

/// Flat `SymbolInformation` list for clients that lack hierarchical document symbols.
pub fn document_symbols_flat(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    encoding: Encoding,
) -> Vec<SymbolInformation> {
    let mut out = Vec::new();
    for symbol in &index.symbols {
        if matches!(
            symbol.kind,
            SimSymbolKind::Builtin | SimSymbolKind::Parameter
        ) {
            continue;
        }
        let container_name = symbol.container.map(|id| index.symbol(id).name.clone());
        #[allow(deprecated)]
        out.push(SymbolInformation {
            name: symbol.name.clone(),
            kind: symbol.kind.lsp_symbol_kind(),
            tags: None,
            deprecated: None,
            location: Location::new(
                uri.clone(),
                byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
            ),
            container_name,
        });
    }
    out
}

/// Reference-count code lenses above procedures and classes.
pub fn code_lenses(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    _uri: &Uri,
    encoding: Encoding,
) -> Vec<tower_lsp_server::ls_types::CodeLens> {
    use tower_lsp_server::ls_types::{CodeLens, Command};
    let mut out = Vec::new();
    for (i, symbol) in index.symbols.iter().enumerate() {
        if !matches!(symbol.kind, SimSymbolKind::Procedure | SimSymbolKind::Class) {
            continue;
        }
        let id = SymbolId(i);
        let refs = index.references_of(id, false);
        let count = refs.len();
        let title = if count == 1 {
            "1 reference".to_owned()
        } else {
            format!("{count} references")
        };
        out.push(CodeLens {
            range: byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
            command: Some(Command {
                title,
                // Display-only — no shell / editor command execution from the server.
                command: String::new(),
                arguments: None,
            }),
            data: None,
        });
    }
    out
}

/// Linked editing ranges for named `begin`/`end` identifiers and matching
/// procedure / class names that appear as named ends.
pub fn linked_editing_ranges(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
) -> Option<tower_lsp_server::ls_types::LinkedEditingRanges> {
    use tower_lsp_server::ls_types::LinkedEditingRanges;
    let offset = position_to_byte(&snap.text, position, encoding);
    // Prefer symbol under cursor: rename-style linked ranges for all refs.
    if let Some(id) = index.resolve_at_offset(offset) {
        let ranges: Vec<Range> = index
            .references_of(id, true)
            .into_iter()
            .map(|(span, _)| byte_span_to_range(&snap.text, span, encoding))
            .collect();
        if ranges.len() >= 2 {
            return Some(LinkedEditingRanges {
                ranges,
                word_pattern: None,
            });
        }
    }
    // Named block: find matching occurrences of the block name near the cursor.
    let Some(program) = &snap.program else {
        return None;
    };
    let mut ranges = Vec::new();
    for block in &program.blocks {
        collect_named_block_ranges(block, &snap.text, offset, encoding, &mut ranges);
    }
    if ranges.len() >= 2 {
        Some(LinkedEditingRanges {
            ranges,
            word_pattern: None,
        })
    } else {
        None
    }
}

fn collect_named_block_ranges(
    block: &Block,
    text: &str,
    offset: usize,
    encoding: Encoding,
    out: &mut Vec<Range>,
) {
    if !block.name.is_empty() {
        // Collect identifier occurrences of the block name within the block's
        // approximate statement span and keep them when the cursor is inside.
        let name = block.name.as_str();
        let mut spans = Vec::new();
        find_identifier_spans(text, name, &mut spans);
        let cursor_hit = spans.iter().any(|s| offset >= s.start && offset <= s.end);
        if cursor_hit {
            for span in spans {
                out.push(byte_span_to_range(text, span, encoding));
            }
        }
    }
    for nested in &block.body {
        collect_named_block_ranges(nested, text, offset, encoding, out);
    }
    for p in &block.procedures {
        collect_named_block_ranges(&p.body, text, offset, encoding, out);
    }
    for c in &block.classes {
        collect_named_block_ranges(&c.body, text, offset, encoding, out);
    }
}

fn find_identifier_spans(text: &str, name: &str, out: &mut Vec<Span>) {
    let lower = text.as_bytes();
    let needle = name.as_bytes();
    if needle.is_empty() {
        return;
    }
    let mut i = 0;
    while i + needle.len() <= lower.len() {
        let slice = &lower[i..i + needle.len()];
        let equal = slice.eq_ignore_ascii_case(needle);
        let boundary_before = i == 0 || !is_ident_byte(lower[i - 1]);
        let boundary_after =
            i + needle.len() == lower.len() || !is_ident_byte(lower[i + needle.len()]);
        if equal && boundary_before && boundary_after {
            out.push(i..i + needle.len());
            i += needle.len();
        } else {
            i += 1;
        }
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Hover that respects client markdown preference.
pub fn hover_with_markup(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    position: Position,
    encoding: Encoding,
    markdown: bool,
) -> Option<Hover> {
    let mut hover = hover(snap, index, position, encoding)?;
    if !markdown && let HoverContents::Markup(ref mut content) = hover.contents {
        content.kind = MarkupKind::PlainText;
        // Strip light markdown fences for plaintext clients.
        content.value = content
            .value
            .replace("```simula\n", "")
            .replace("```\n", "")
            .replace('`', "")
            .replace("**", "")
            .replace('_', "");
    }
    Some(hover)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};
    use std::str::FromStr;
    use tower_lsp_server::ls_types::CodeActionOrCommand;

    fn ready(source: &str) -> (AnalysisSnapshot, SymbolIndex) {
        let snap = analyze_document(source, &AnalysisOptions::default());
        assert!(snap.program.is_some(), "{:?}", snap.diagnostics);
        let idx = SymbolIndex::build(snap.program.as_ref().unwrap(), snap.tokens.as_ref());
        (snap, idx)
    }

    #[test]
    fn hover_on_variable() {
        let (snap, idx) = ready("begin integer x; x := 1; end");
        let pos = Position::new(0, snap.text.find('x').unwrap() as u32);
        let h = hover(&snap, &idx, pos, Encoding::Utf16).expect("hover");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markup");
        };
        assert!(md.value.contains("integer"));
    }

    #[test]
    fn hover_on_type_mismatch_shows_explain_essay() {
        let snap = analyze_document(
            "begin integer x; x := true; end",
            &AnalysisOptions::default(),
        );
        let idx = SymbolIndex::build(snap.program.as_ref().unwrap(), snap.tokens.as_ref());
        let pos = Position::new(0, snap.text.find("true").unwrap() as u32);
        let h = hover(&snap, &idx, pos, Encoding::Utf16).expect("hover");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markup");
        };
        assert!(
            md.value.contains("E0201") && md.value.contains("TYPE MISMATCH"),
            "{}",
            md.value
        );
    }

    #[test]
    fn document_symbols_list_proc() {
        let (snap, idx) = ready("begin procedure p; begin end; end");
        let symbols = document_symbols(&snap, &idx, Encoding::Utf16);
        assert!(symbols.iter().any(|s| s.name.eq_ignore_ascii_case("p")));
    }

    #[test]
    fn goto_definition_from_use() {
        let (snap, idx) = ready("begin integer count; count := 1; end");
        let uri = Uri::from_str("file:///t.sim").unwrap();
        let use_pos = Position::new(0, snap.text.rfind("count").unwrap() as u32);
        let loc = goto_definition(&snap, &idx, &uri, use_pos, Encoding::Utf16).expect("def");
        assert_eq!(
            loc.range.start.character,
            snap.text.find("count").unwrap() as u32
        );
    }

    #[test]
    fn remote_attribute_goto_and_hover() {
        let source =
            "begin class Point; begin integer x; end; ref(Point) p; p :- new Point; p.x := 1; end";
        let (snap, idx) = ready(source);
        let uri = Uri::from_str("file:///t.sim").unwrap();
        let attr_pos = Position::new(0, snap.text.rfind(".x").unwrap() as u32 + 1);
        let loc = goto_definition(&snap, &idx, &uri, attr_pos, Encoding::Utf16)
            .expect("remote attr definition");
        assert_eq!(
            loc.range.start.character,
            snap.text.find("integer x").unwrap() as u32 + "integer ".len() as u32
        );
        let h = hover(&snap, &idx, attr_pos, Encoding::Utf16).expect("hover");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markup");
        };
        assert!(md.value.contains("integer"), "{}", md.value);
        let refs = find_references(&snap, &idx, &uri, attr_pos, Encoding::Utf16, true);
        assert!(refs.len() >= 2, "expected decl + use, got {}", refs.len());
    }

    #[test]
    fn goto_type_definition_for_object_ref() {
        let source = "begin class Point; begin end; ref(Point) p; end";
        let (snap, idx) = ready(source);
        let uri = Uri::from_str("file:///t.sim").unwrap();
        let pos = Position::new(0, snap.text.rfind('p').unwrap() as u32);
        let loc = goto_type_definition(&snap, &idx, &uri, pos, Encoding::Utf16).expect("type def");
        assert_eq!(
            loc.range.start.character,
            snap.text.find("Point").unwrap() as u32
        );
    }

    #[test]
    fn references_include_decl_and_use() {
        let (snap, idx) = ready("begin integer x; x := 1; end");
        let uri = Uri::from_str("file:///t.sim").unwrap();
        let pos = Position::new(0, snap.text.rfind('x').unwrap() as u32);
        let refs = find_references(&snap, &idx, &uri, pos, Encoding::Utf16, true);
        assert!(refs.len() >= 2);
    }

    #[test]
    fn completions_include_locals_and_keywords() {
        let (snap, idx) = ready("begin integer foo;  end");
        let pos = Position::new(0, snap.text.find(';').unwrap() as u32 + 1);
        let items = completions_list(
            &snap,
            &idx,
            pos,
            Encoding::Utf16,
            &CompletionOptions::default(),
        )
        .0;
        assert!(items.iter().any(|i| i.label.eq_ignore_ascii_case("foo")));
        assert!(items.iter().any(|i| i.label == "begin"));
        assert!(items.iter().any(|i| i.label == "sqrt"));
    }

    #[test]
    fn rename_rewrites_uses() {
        let (snap, idx) = ready("begin integer x; x := 1; end");
        let uri = Uri::from_str("file:///t.sim").unwrap();
        let pos = Position::new(0, snap.text.find('x').unwrap() as u32);
        let edit = rename(&snap, &idx, &uri, pos, Encoding::Utf16, "y").expect("rename");
        let changes = edit.changes.unwrap();
        assert!(changes[&uri].len() >= 2);
    }

    #[test]
    fn semantic_tokens_nonempty() {
        let (snap, idx) = ready("begin integer x; end");
        let tokens = semantic_tokens_full(&snap, &idx, Encoding::Utf16);
        assert!(!tokens.data.is_empty());
    }

    fn decoded_types(source: &str) -> Vec<(String, String, u32)> {
        let (snap, idx) = ready(source);
        let tokens = semantic_tokens_full(&snap, &idx, Encoding::Utf16);
        let legend: Vec<String> = semantic_tokens_legend()
            .token_types
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        let mut line = 0u32;
        let mut character = 0u32;
        let lines: Vec<&str> = source.lines().collect();
        let mut out = Vec::new();
        for token in &tokens.data {
            if token.delta_line > 0 {
                line += token.delta_line;
                character = token.delta_start;
            } else {
                character += token.delta_start;
            }
            let text = lines
                .get(line as usize)
                .and_then(|l| {
                    let start = character as usize;
                    let end = start + token.length as usize;
                    l.get(start..end)
                })
                .unwrap_or("")
                .to_string();
            let ty = legend
                .get(token.token_type as usize)
                .cloned()
                .unwrap_or_else(|| "?".into());
            out.push((text, ty, token.token_modifiers_bitset));
        }
        out
    }

    #[test]
    fn semantic_tokens_cover_comments_and_keywords() {
        let classified = decoded_types("%opt\nbegin integer x; ! hi;\n-- line\nend;");
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text.starts_with("%opt") && ty == "commentDirective"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == "begin" && ty == "keyword"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == "integer" && ty == "type"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text.contains("hi") && ty == "comment"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text.contains("line") && ty == "comment"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == ";" && ty == "semicolon"),
            "{classified:?}"
        );
    }

    #[test]
    fn semantic_tokens_end_comment_is_not_a_variable() {
        let classified = decoded_types("begin end trace;");
        assert!(
            !classified
                .iter()
                .any(|(text, ty, _)| text == "trace" && ty == "variable"),
            "{classified:?}"
        );
    }

    #[test]
    fn semantic_tokens_declaration_modifier_only_on_definition() {
        let classified = decoded_types("begin integer x; x := 1; end");
        let xs: Vec<_> = classified
            .iter()
            .filter(|(text, ty, _)| text == "x" && ty == "variable")
            .collect();
        assert_eq!(xs.len(), 2, "{classified:?}");
        assert_eq!(xs[0].2 & 1, 1, "definition should be declaration");
        assert_eq!(xs[1].2 & 1, 0, "use should not be declaration");
    }

    #[test]
    fn semantic_tokens_bool_char_and_class() {
        let classified = decoded_types(
            "begin class Point; begin end; ref(Point) p; p := none; b := true; c := 'A'; end",
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text.eq_ignore_ascii_case("Point") && ty == "class"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == "ref" && ty == "modifier"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == "none" && ty == "null"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == "true" && ty == "boolean"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text.contains('A') && ty == "character"),
            "{classified:?}"
        );
        assert!(
            classified
                .iter()
                .any(|(text, ty, _)| text == "(" && ty == "parenthesis"),
            "{classified:?}"
        );
    }

    #[test]
    fn semantic_tokens_range_is_subset() {
        let (snap, idx) = ready("begin integer x; real y; end");
        let full = semantic_tokens_full(&snap, &idx, Encoding::Utf16);
        let range = Range::new(Position::new(0, 0), Position::new(0, 10));
        let ranged = semantic_tokens_range(&snap, &idx, Encoding::Utf16, range);
        assert!(!ranged.data.is_empty());
        assert!(ranged.data.len() <= full.data.len());
    }

    #[test]
    fn semantic_tokens_delta_common_prefix() {
        let previous = vec![
            SemanticToken {
                delta_line: 0,
                delta_start: 0,
                length: 5,
                token_type: 0,
                token_modifiers_bitset: 0,
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 6,
                length: 3,
                token_type: 1,
                token_modifiers_bitset: 0,
            },
        ];
        let mut next = previous.clone();
        next.push(SemanticToken {
            delta_line: 0,
            delta_start: 4,
            length: 1,
            token_type: 2,
            token_modifiers_bitset: 0,
        });
        let delta = semantic_tokens_delta(
            Some(&previous),
            SemanticTokens {
                result_id: Some("2".into()),
                data: next.clone(),
            },
        )
        .expect("delta");
        assert_eq!(delta.result_id.as_deref(), Some("2"));
        assert_eq!(delta.edits.len(), 1);
        assert_eq!(delta.edits[0].start, 2);
        assert_eq!(delta.edits[0].delete_count, 0);
        assert_eq!(delta.edits[0].data.as_ref().map(|d| d.len()), Some(1));

        let unchanged = semantic_tokens_delta(
            Some(&next),
            SemanticTokens {
                result_id: Some("3".into()),
                data: next.clone(),
            },
        )
        .expect("noop delta");
        assert!(unchanged.edits.is_empty());
    }

    #[test]
    fn workspace_symbols_match_query() {
        let (snap, idx) = ready("begin class Point; begin end; procedure draw; begin end; end");
        let uri = Uri::from_str("file:///t.sim").unwrap();
        let docs = [(uri, &snap, &idx)];
        let hits = workspace_symbols(&docs, "poi", Encoding::Utf16);
        assert!(
            hits.iter().any(|s| s.name.eq_ignore_ascii_case("Point")),
            "{hits:?}"
        );
        let draws = workspace_symbols(&docs, "draw", Encoding::Utf16);
        assert!(draws.iter().any(|s| s.name.eq_ignore_ascii_case("draw")));
    }

    #[test]
    fn folding_for_procedure() {
        let (snap, _) = ready(
            "begin
               procedure p;
               begin
                 integer x;
               end;
             end",
        );
        let folds = folding_ranges(&snap, Encoding::Utf16);
        assert!(!folds.is_empty(), "expected folding ranges");
    }

    #[test]
    fn signature_help_for_procedure_call() {
        let (snap, idx) = ready("begin procedure p(a, b); integer a, b; begin end; p(1, ); end");
        let call = snap.text.find("p(1,").expect("call site");
        let after_paren = Position::new(0, (call + 2) as u32);
        let help = signature_help(&snap, &idx, after_paren, Encoding::Utf16).expect("sig help");
        assert_eq!(help.signatures.len(), 1);
        assert!(
            help.signatures[0]
                .label
                .to_ascii_lowercase()
                .contains("procedure p"),
            "label={}",
            help.signatures[0].label
        );
        let params = help.signatures[0].parameters.as_ref().expect("params");
        assert_eq!(params.len(), 2);
        assert_eq!(help.active_parameter, Some(0));

        let after_comma = Position::new(0, (call + 4) as u32);
        let help2 = signature_help(&snap, &idx, after_comma, Encoding::Utf16).expect("sig help");
        assert_eq!(help2.active_parameter, Some(1));
    }

    #[test]
    fn selection_ranges_expand_from_token_to_document() {
        let (snap, _) = ready("begin integer x; x := 1 + 2; end");
        let pos = Position::new(0, snap.text.find('2').unwrap() as u32);
        let ranges = selection_ranges(&snap, &[pos], Encoding::Utf16);
        assert_eq!(ranges.len(), 1);
        let mut cur = &ranges[0];
        let mut chain = vec![cur.range];
        while let Some(parent) = &cur.parent {
            chain.push(parent.range);
            cur = parent;
        }
        // Innermost should cover the `2` token.
        assert_eq!(
            chain[0].start.character,
            snap.text.find('2').unwrap() as u32
        );
        assert_eq!(chain[0].end.character, chain[0].start.character + 1);
        // Chain must grow (or stay equal) toward the outer document.
        for window in chain.windows(2) {
            let inner = &window[0];
            let outer = &window[1];
            assert!(
                outer.start.character <= inner.start.character
                    && outer.end.character >= inner.end.character,
                "inner={inner:?} outer={outer:?}"
            );
        }
        // Outermost is the whole document.
        let last = chain.last().unwrap();
        assert_eq!(last.start, Position::new(0, 0));
        assert_eq!(last.end.character, snap.text.len() as u32);
        // Should include an expression larger than the token and a statement.
        assert!(
            chain.len() >= 3,
            "expected token/expr/stmt/doc chain, got {}",
            chain.len()
        );
    }

    #[test]
    fn selection_ranges_include_procedure_span() {
        let (snap, _) = ready(
            "begin
               procedure p;
               begin
                 integer x;
                 x := 1;
               end;
             end",
        );
        let x_assign = snap.text.find("x := 1").expect("assign");
        // Position on the `1`.
        let one = snap.text[x_assign..].find('1').unwrap() + x_assign;
        let pos = Position::new(
            snap.text[..one].matches('\n').count() as u32,
            (one - snap.text[..one].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
        );
        let ranges = selection_ranges(&snap, &[pos], Encoding::Utf16);
        let mut cur = &ranges[0];
        let mut found_multi_line = false;
        loop {
            if cur.range.start.line < cur.range.end.line {
                found_multi_line = true;
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => break,
            }
        }
        assert!(
            found_multi_line,
            "expected procedure/document parent ranges"
        );
    }

    #[test]
    fn dot_completions_offer_class_attributes() {
        let text = "begin class Point; integer x, y; begin end Point; ref(Point) p; p.x := 1; end";
        let (snap, idx) = ready(text);
        let dot = text.find("p.x").unwrap() + 2;
        let pos = Position::new(0, dot as u32);
        let items = completions_list(
            &snap,
            &idx,
            pos,
            Encoding::Utf16,
            &CompletionOptions::default(),
        )
        .0;
        assert!(items.iter().any(|i| i.label == "x"));
        assert!(items.iter().any(|i| i.label == "y"));
        assert!(!items.iter().any(|i| i.label == "begin"));
    }

    #[test]
    fn goto_definition_on_label_use() {
        let text = "begin procedure p; begin integer x; goto done; x := 99; done: x := 1; end; end";
        let (snap, idx) = ready(text);
        let uri = Uri::from_str("file:///label.sim").unwrap();
        let use_col = text.find("goto done").unwrap() + "goto ".len();
        let pos = Position::new(0, use_col as u32);
        let loc = goto_definition(&snap, &idx, &uri, pos, Encoding::Utf16).expect("label def");
        assert_eq!(
            loc.range.start.character,
            text.find("done:").unwrap() as u32
        );
    }

    #[test]
    fn code_actions_explain_semantic_errors() {
        let (snap, _) = ready("begin integer x; x := true; end");
        let uri = Uri::from_str("file:///err.sim").unwrap();
        let err_col = snap.text.find("true").unwrap();
        let range = Range::new(
            Position::new(0, err_col as u32),
            Position::new(0, (err_col + 4) as u32),
        );
        let actions = crate::lsp::actions::code_actions(
            &snap,
            snap.symbols.as_ref(),
            &uri,
            range,
            Encoding::Utf16,
            &[],
        );
        assert!(
            actions.iter().any(|a| match a {
                CodeActionOrCommand::CodeAction(action) => {
                    action.title.contains("E0201")
                        || action.title.contains("TYPE MISMATCH")
                        || action.title.contains("E-semantic")
                }
                CodeActionOrCommand::Command(_) => false,
            }),
            "actions={actions:?}"
        );
    }

    #[test]
    fn completions_include_snippets() {
        let (snap, idx) = ready("begin end");
        let pos = Position::new(0, snap.text.find("end").unwrap() as u32);
        let items = completions_list(
            &snap,
            &idx,
            pos,
            Encoding::Utf16,
            &CompletionOptions::default(),
        )
        .0;
        assert!(items.iter().any(|i| {
            i.label == "procedure" && i.insert_text_format == Some(InsertTextFormat::SNIPPET)
        }));
    }
}
