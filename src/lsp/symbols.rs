//! Symbol index built from a parsed Simula program for LSP features.

use crate::ast::{
    Assignment, AssignmentRhs, Block, ClassDeclaration, DesignationalExpr, Expr, ExprKind,
    ExternalDeclaration, ForListElement, ProcedureCall, ProcedureDeclaration, Program, Statement,
    StatementKind, SwitchDeclaration, Variable,
};
use crate::environment::{
    builtin_result_type, environment_constants, environment_procedures, is_environment_constant,
    is_environment_procedure,
};
use crate::error::Span;
use crate::lex::{Keyword, Token, TokenKind, TokenStream};
use crate::types::{Declaration, Type};

/// Kind of a declared or builtin symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Variable,
    Constant,
    Array,
    Parameter,
    Procedure,
    Class,
    Switch,
    Label,
    Builtin,
}

impl SymbolKind {
    pub fn lsp_symbol_kind(self) -> tower_lsp_server::ls_types::SymbolKind {
        use tower_lsp_server::ls_types::SymbolKind as Sk;
        match self {
            Self::Variable | Self::Label | Self::Parameter => Sk::VARIABLE,
            Self::Constant => Sk::CONSTANT,
            Self::Array => Sk::ARRAY,
            Self::Procedure | Self::Builtin => Sk::FUNCTION,
            Self::Class => Sk::CLASS,
            Self::Switch => Sk::ENUM,
        }
    }

    pub fn lsp_completion_kind(self) -> tower_lsp_server::ls_types::CompletionItemKind {
        use tower_lsp_server::ls_types::CompletionItemKind as Ck;
        match self {
            Self::Variable | Self::Label | Self::Parameter => Ck::VARIABLE,
            Self::Constant => Ck::CONSTANT,
            Self::Array => Ck::VARIABLE,
            Self::Procedure | Self::Builtin => Ck::FUNCTION,
            Self::Class => Ck::CLASS,
            Self::Switch => Ck::ENUM,
        }
    }
}

/// A declared symbol with source locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub ty: Option<Type>,
    pub detail: String,
    pub name_span: Span,
    pub full_span: Span,
    pub scope: ScopeId,
    pub container: Option<SymbolId>,
    /// True for `procedure …; external` stubs.
    pub is_external: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub usize);

#[derive(Debug, Clone)]
struct Scope {
    parent: Option<ScopeId>,
    bindings: Vec<SymbolId>,
}

/// A use-site of a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameUse {
    pub name: String,
    pub span: Span,
    pub scope: ScopeId,
    pub definition: Option<SymbolId>,
    pub is_write: bool,
    /// When set, resolve `name` as an attribute of this simple receiver (`recv.attr`).
    pub remote_receiver: Option<String>,
}

/// Index of symbols, scopes, and name uses for one document.
#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    pub symbols: Vec<Symbol>,
    scopes: Vec<Scope>,
    pub uses: Vec<NameUse>,
    root: Option<ScopeId>,
    /// Class name (lowercase) → optional prefix class name for attribute lookup.
    class_prefixes: std::collections::HashMap<String, Option<String>>,
}

impl SymbolIndex {
    pub fn build(program: &Program, tokens: Option<&TokenStream>) -> Self {
        let token_slice = tokens.map(|t| t.tokens.as_slice()).unwrap_or(&[]);
        let mut index = Self::default();
        let root = index.push_scope(None);
        index.root = Some(root);
        index.index_externals(&program.external_head, root, token_slice);
        for block in &program.blocks {
            index.index_block(block, root, None, token_slice, None);
        }
        index.resolve_uses();
        index
    }

    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0]
    }

    pub fn lookup(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            let scope = &self.scopes[scope_id.0];
            for &id in scope.bindings.iter().rev() {
                if self.symbols[id.0].name.eq_ignore_ascii_case(name) {
                    return Some(id);
                }
            }
            current = scope.parent;
        }
        None
    }

    /// Find a class symbol by name in the document.
    pub fn find_class(&self, class_name: &str) -> Option<SymbolId> {
        self.symbols
            .iter()
            .enumerate()
            .find(|(_, s)| s.kind == SymbolKind::Class && s.name.eq_ignore_ascii_case(class_name))
            .map(|(i, _)| SymbolId(i))
    }

    /// Resolve `attr` as a member of `class_name`, walking the prefix chain.
    pub fn lookup_class_attribute(&self, class_name: &str, attr: &str) -> Option<SymbolId> {
        let mut current = Some(class_name.to_owned());
        let mut seen = std::collections::HashSet::new();
        while let Some(name) = current {
            if !seen.insert(name.to_ascii_lowercase()) {
                break;
            }
            let Some(class_id) = self.find_class(&name) else {
                break;
            };
            for child in self.children_of(class_id) {
                if self.symbol(child).name.eq_ignore_ascii_case(attr) {
                    return Some(child);
                }
            }
            current = self
                .class_prefixes
                .get(&name.to_ascii_lowercase())
                .cloned()
                .flatten();
        }
        None
    }

    /// Immediate prefix class name for `class_name`, if any.
    pub fn class_prefix(&self, class_name: &str) -> Option<String> {
        self.class_prefixes
            .get(&class_name.to_ascii_lowercase())
            .cloned()
            .flatten()
    }

    /// Classes in this document that list `class_name` as their prefix.
    pub fn class_subtypes(&self, class_name: &str) -> Vec<SymbolId> {
        let key = class_name.to_ascii_lowercase();
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SymbolKind::Class)
            .filter(|(_, s)| {
                self.class_prefixes
                    .get(&s.name.to_ascii_lowercase())
                    .and_then(|p| p.as_ref())
                    .is_some_and(|p| p.eq_ignore_ascii_case(&key))
            })
            .map(|(i, _)| SymbolId(i))
            .collect()
    }

    /// Procedures named `proc_name` declared in `class_name` or related prefix /
    /// subclass classes in this document.
    pub fn related_class_procedures(&self, class_name: &str, proc_name: &str) -> Vec<SymbolId> {
        let mut classes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Walk prefixes upward.
        let mut current = Some(class_name.to_owned());
        while let Some(name) = current {
            let key = name.to_ascii_lowercase();
            if !seen.insert(key.clone()) {
                break;
            }
            if let Some(id) = self.find_class(&name) {
                classes.push(id);
            }
            current = self.class_prefixes.get(&key).cloned().flatten();
        }
        // Walk subtypes downward (BFS).
        let mut queue: Vec<String> = vec![class_name.to_owned()];
        while let Some(name) = queue.pop() {
            for sub in self.class_subtypes(&name) {
                let sub_name = self.symbol(sub).name.clone();
                let key = sub_name.to_ascii_lowercase();
                if seen.insert(key) {
                    classes.push(sub);
                    queue.push(sub_name);
                }
            }
        }
        let mut out = Vec::new();
        for class_id in classes {
            for child in self.children_of(class_id) {
                let sym = self.symbol(child);
                if sym.kind == SymbolKind::Procedure && sym.name.eq_ignore_ascii_case(proc_name) {
                    out.push(child);
                }
            }
        }
        out
    }

    pub fn symbol_at_offset(&self, offset: usize) -> Option<SymbolId> {
        self.symbols
            .iter()
            .enumerate()
            .rev()
            .find(|(_, sym)| span_contains(&sym.name_span, offset))
            .map(|(i, _)| SymbolId(i))
    }

    pub fn use_at_offset(&self, offset: usize) -> Option<&NameUse> {
        self.uses
            .iter()
            .rev()
            .find(|u| span_contains(&u.span, offset))
    }

    pub fn resolve_at_offset(&self, offset: usize) -> Option<SymbolId> {
        if let Some(id) = self.symbol_at_offset(offset) {
            return Some(id);
        }
        self.use_at_offset(offset).and_then(|u| u.definition)
    }

    pub fn references_of(&self, id: SymbolId, include_declaration: bool) -> Vec<(Span, bool)> {
        let mut out = Vec::new();
        if include_declaration {
            out.push((self.symbols[id.0].name_span.clone(), true));
        }
        for u in &self.uses {
            if u.definition == Some(id) {
                out.push((u.span.clone(), u.is_write));
            }
        }
        out
    }

    pub fn completions_in_scope(&self, scope: ScopeId) -> Vec<SymbolId> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            let scope = &self.scopes[scope_id.0];
            for &id in scope.bindings.iter().rev() {
                let key = self.symbols[id.0].name.to_ascii_lowercase();
                if seen.insert(key) {
                    out.push(id);
                }
            }
            current = scope.parent;
        }
        out
    }

    pub fn scope_at_offset(&self, offset: usize) -> ScopeId {
        if let Some(u) = self.use_at_offset(offset) {
            return u.scope;
        }
        if let Some(id) = self.symbol_at_offset(offset) {
            return self.symbols[id.0].scope;
        }
        // Prefer the deepest scope that declared something starting at or before
        // `offset` (so completions work in empty statement positions).
        let mut best = self.root.unwrap_or(ScopeId(0));
        let mut best_depth = 0usize;
        for sym in &self.symbols {
            if sym.name_span.start <= offset {
                let depth = self.scope_depth(sym.scope);
                if depth >= best_depth {
                    best_depth = depth;
                    best = sym.scope;
                }
            }
        }
        best
    }

    fn scope_depth(&self, mut scope: ScopeId) -> usize {
        let mut depth = 0;
        while let Some(parent) = self.scopes[scope.0].parent {
            depth += 1;
            scope = parent;
        }
        depth
    }

    /// Top-level outline symbols (classes, procedures, then others without a container).
    pub fn outline_roots(&self) -> Vec<SymbolId> {
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.container.is_none()
                    && matches!(
                        s.kind,
                        SymbolKind::Class
                            | SymbolKind::Procedure
                            | SymbolKind::Switch
                            | SymbolKind::Variable
                            | SymbolKind::Constant
                            | SymbolKind::Array
                            | SymbolKind::Label
                    )
            })
            .map(|(i, _)| SymbolId(i))
            .collect()
    }

    pub fn children_of(&self, parent: SymbolId) -> Vec<SymbolId> {
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.container == Some(parent))
            .map(|(i, _)| SymbolId(i))
            .collect()
    }

    fn push_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let id = ScopeId(self.scopes.len());
        self.scopes.push(Scope {
            parent,
            bindings: Vec::new(),
        });
        id
    }

    fn declare(
        &mut self,
        scope: ScopeId,
        container: Option<SymbolId>,
        name: String,
        kind: SymbolKind,
        ty: Option<Type>,
        detail: String,
        name_span: Span,
        full_span: Span,
    ) -> SymbolId {
        self.declare_ext(
            scope, container, name, kind, ty, detail, name_span, full_span, false,
        )
    }

    fn declare_ext(
        &mut self,
        scope: ScopeId,
        container: Option<SymbolId>,
        name: String,
        kind: SymbolKind,
        ty: Option<Type>,
        detail: String,
        name_span: Span,
        full_span: Span,
        is_external: bool,
    ) -> SymbolId {
        let id = SymbolId(self.symbols.len());
        self.symbols.push(Symbol {
            name,
            kind,
            ty,
            detail,
            name_span,
            full_span,
            scope,
            container,
            is_external,
        });
        self.scopes[scope.0].bindings.push(id);
        id
    }

    fn index_externals(
        &mut self,
        externals: &[ExternalDeclaration],
        scope: ScopeId,
        tokens: &[Token],
    ) {
        for external in externals {
            match external {
                ExternalDeclaration::Procedure(proc) => {
                    for item in &proc.items {
                        let name_span =
                            find_name_span(tokens, &item.name, &(0..usize::MAX)).unwrap_or(0..0);
                        let detail = match &proc.result_type {
                            Some(ty) => format!("{ty} procedure {}; external", item.name),
                            None => format!("procedure {}; external", item.name),
                        };
                        self.declare_ext(
                            scope,
                            None,
                            item.name.clone(),
                            SymbolKind::Procedure,
                            proc.result_type.clone(),
                            detail,
                            name_span.clone(),
                            name_span,
                            true,
                        );
                    }
                    if let Some(spec) = &proc.specification {
                        self.index_procedure(spec, scope, None, tokens);
                    }
                }
                ExternalDeclaration::Class(class) => {
                    for item in &class.items {
                        let name_span =
                            find_name_span(tokens, &item.name, &(0..usize::MAX)).unwrap_or(0..0);
                        self.declare_ext(
                            scope,
                            None,
                            item.name.clone(),
                            SymbolKind::Class,
                            Some(Type::ObjectRef(item.name.clone())),
                            format!("class {}; external", item.name),
                            name_span.clone(),
                            name_span,
                            true,
                        );
                    }
                }
            }
        }
    }

    fn index_block(
        &mut self,
        block: &Block,
        parent_scope: ScopeId,
        container: Option<SymbolId>,
        tokens: &[Token],
        inherited_labels: Option<ScopeId>,
    ) {
        // Locals of a nested `begin`…`end` with declarations get their own
        // scope. Labels do not: semantic analysis and `goto` treat them as
        // belonging to the enclosing procedure / class / program (simtst00
        // `PRINT`, and the same pattern with inner locals).
        let scope = if block_introduces_scope(block) {
            self.push_scope(Some(parent_scope))
        } else {
            parent_scope
        };
        let label_scope = inherited_labels.unwrap_or(scope);
        self.index_externals(&block.externals, scope, tokens);

        for decl in &block.declarations {
            self.index_declaration(decl, scope, container, tokens);
        }
        for array in &block.arrays {
            for segment in &array.segments {
                for name in &segment.names {
                    let name_span = find_name_span(tokens, name, &array.span)
                        .unwrap_or_else(|| array.span.clone());
                    self.declare(
                        scope,
                        container,
                        name.clone(),
                        SymbolKind::Array,
                        Some(Type::Array {
                            element: Box::new(array.element_type.clone()),
                            dims: segment.bounds.len(),
                        }),
                        format!("{} {}", array.element_type, name),
                        name_span,
                        array.span.clone(),
                    );
                }
            }
        }
        for switch in &block.switches {
            self.index_switch(switch, scope, container, tokens);
        }
        for procedure in &block.procedures {
            self.index_procedure(procedure, scope, container, tokens);
        }
        for class in &block.classes {
            self.index_class(class, scope, container, tokens);
        }
        for stmt in &block.statements {
            self.index_statement(stmt, scope, label_scope, tokens);
        }
        for nested in &block.body {
            self.index_block(nested, scope, container, tokens, Some(label_scope));
        }
    }

    fn index_declaration(
        &mut self,
        decl: &Declaration,
        scope: ScopeId,
        container: Option<SymbolId>,
        tokens: &[Token],
    ) {
        for item in &decl.items {
            let name_span =
                find_name_span(tokens, &item.name, &decl.span).unwrap_or_else(|| decl.span.clone());
            let kind = if item.is_constant {
                SymbolKind::Constant
            } else {
                SymbolKind::Variable
            };
            let detail = if item.is_constant {
                format!("constant {} {}", decl.ty, item.name)
            } else {
                format!("{} {}", decl.ty, item.name)
            };
            self.declare(
                scope,
                container,
                item.name.clone(),
                kind,
                Some(decl.ty.clone()),
                detail,
                name_span,
                decl.span.clone(),
            );
            if let Some(init) = &item.initializer {
                self.index_expr(init, scope, tokens);
            }
        }
    }

    fn index_switch(
        &mut self,
        switch: &SwitchDeclaration,
        scope: ScopeId,
        container: Option<SymbolId>,
        tokens: &[Token],
    ) {
        let name_span = find_name_span(tokens, &switch.name, &switch.span)
            .unwrap_or_else(|| switch.span.clone());
        self.declare(
            scope,
            container,
            switch.name.clone(),
            SymbolKind::Switch,
            None,
            format!("switch {}", switch.name),
            name_span,
            switch.span.clone(),
        );
        for element in &switch.elements {
            self.index_designational(element, scope, tokens, &switch.span);
        }
    }

    fn index_procedure(
        &mut self,
        procedure: &ProcedureDeclaration,
        parent_scope: ScopeId,
        container: Option<SymbolId>,
        tokens: &[Token],
    ) {
        let name_span = find_name_span(tokens, &procedure.name, &procedure.span)
            .unwrap_or_else(|| procedure.span.clone());
        let id = self.declare_ext(
            parent_scope,
            container,
            procedure.name.clone(),
            SymbolKind::Procedure,
            procedure.result_type.clone(),
            format_procedure_signature(procedure),
            name_span,
            procedure.span.clone(),
            procedure.is_external,
        );
        let body_scope = self.push_scope(Some(parent_scope));
        for formal in &procedure.parameters {
            let name_span = if formal.span != (0..0) {
                formal.span.clone()
            } else {
                find_name_span(tokens, &formal.name, &procedure.span)
                    .unwrap_or_else(|| procedure.span.clone())
            };
            self.declare(
                body_scope,
                Some(id),
                formal.name.clone(),
                SymbolKind::Parameter,
                Some(formal.ty.clone()),
                format!("{} {}", formal.ty, formal.name),
                name_span.clone(),
                name_span,
            );
        }
        self.index_block(
            &procedure.body,
            body_scope,
            Some(id),
            tokens,
            Some(body_scope),
        );
    }

    fn index_class(
        &mut self,
        class: &ClassDeclaration,
        parent_scope: ScopeId,
        container: Option<SymbolId>,
        tokens: &[Token],
    ) {
        let name_span =
            find_name_span(tokens, &class.name, &class.span).unwrap_or_else(|| class.span.clone());
        let id = self.declare(
            parent_scope,
            container,
            class.name.clone(),
            SymbolKind::Class,
            Some(Type::ObjectRef(class.name.clone())),
            format_class_signature(class),
            name_span,
            class.span.clone(),
        );
        self.class_prefixes
            .insert(class.name.to_ascii_lowercase(), class.prefix.clone());
        let body_scope = self.push_scope(Some(parent_scope));
        for formal in &class.parameters {
            let name_span = if formal.span != (0..0) {
                formal.span.clone()
            } else {
                find_name_span(tokens, &formal.name, &class.span)
                    .unwrap_or_else(|| class.span.clone())
            };
            self.declare(
                body_scope,
                Some(id),
                formal.name.clone(),
                SymbolKind::Parameter,
                Some(formal.ty.clone()),
                format!("{} {}", formal.ty, formal.name),
                name_span.clone(),
                name_span,
            );
        }
        // Index virtual procedure headings so hierarchy / implementation can resolve them.
        for virtual_spec in &class.virtual_part {
            for name in &virtual_spec.names {
                let name_span =
                    find_name_span(tokens, name, &class.span).unwrap_or_else(|| class.span.clone());
                let detail = format!("virtual procedure {name}");
                self.declare(
                    body_scope,
                    Some(id),
                    name.clone(),
                    SymbolKind::Procedure,
                    None,
                    detail,
                    name_span.clone(),
                    name_span,
                );
            }
            if let Some(heading) = &virtual_spec.procedure_heading {
                self.index_procedure(heading, body_scope, Some(id), tokens);
            }
        }
        self.index_block(&class.body, body_scope, Some(id), tokens, Some(body_scope));
        for stmt in &class.tail_statements {
            self.index_statement(stmt, body_scope, body_scope, tokens);
        }
    }

    fn index_statement(
        &mut self,
        stmt: &Statement,
        scope: ScopeId,
        label_scope: ScopeId,
        tokens: &[Token],
    ) {
        match &stmt.kind {
            StatementKind::Labeled { label, statement } => {
                let name_span =
                    find_name_span(tokens, label, &stmt.span).unwrap_or_else(|| stmt.span.clone());
                self.declare(
                    label_scope,
                    None,
                    label.clone(),
                    SymbolKind::Label,
                    None,
                    format!("label {label}"),
                    name_span,
                    stmt.span.clone(),
                );
                self.index_statement(statement, scope, label_scope, tokens);
            }
            StatementKind::Assignment(assign) => {
                self.index_assignment(assign, &stmt.span, scope, tokens);
            }
            StatementKind::ProcedureCall(call) => {
                self.index_procedure_call(call, &stmt.span, scope, tokens);
            }
            StatementKind::While(w) => {
                self.index_expr(&w.condition, scope, tokens);
                self.index_statement(&w.body, scope, label_scope, tokens);
            }
            StatementKind::If(i) => {
                self.index_expr(&i.condition, scope, tokens);
                self.index_statement(&i.then_branch, scope, label_scope, tokens);
                if let Some(else_branch) = &i.else_branch {
                    self.index_statement(else_branch, scope, label_scope, tokens);
                }
            }
            StatementKind::For(f) => {
                if let Some(span) = find_name_span(tokens, &f.variable, &stmt.span) {
                    self.record_use(f.variable.clone(), span, scope, true);
                }
                for element in &f.elements {
                    self.index_for_element(element, scope, tokens);
                }
                self.index_statement(&f.body, scope, label_scope, tokens);
            }
            StatementKind::Goto(g) => {
                self.index_designational(&g.target, scope, tokens, &stmt.span);
            }
            StatementKind::Compound(block) => {
                self.index_block(block, scope, None, tokens, Some(label_scope))
            }
            StatementKind::Expr(expr) => self.index_expr(expr, scope, tokens),
            StatementKind::ObjectGenerator(object_gen) => {
                if let Some(span) = find_name_span(tokens, &object_gen.class_name, &stmt.span) {
                    self.record_use(object_gen.class_name.clone(), span, scope, false);
                }
                for arg in &object_gen.arguments {
                    self.index_expr(arg, scope, tokens);
                }
            }
            StatementKind::Inspect(inspect) => {
                self.index_expr(&inspect.object, scope, tokens);
                for clause in &inspect.when_clauses {
                    if let Some(span) = find_name_span(tokens, &clause.class_name, &stmt.span) {
                        self.record_use(clause.class_name.clone(), span, scope, false);
                    }
                    self.index_statement(&clause.body, scope, label_scope, tokens);
                }
                if let Some(do_clause) = &inspect.do_clause {
                    self.index_statement(do_clause, scope, label_scope, tokens);
                }
                if let Some(otherwise) = &inspect.otherwise {
                    self.index_statement(otherwise, scope, label_scope, tokens);
                }
            }
            StatementKind::Activate(a) => {
                self.index_expr(&a.target, scope, tokens);
                if let Some(timing) = &a.timing {
                    self.index_timing(timing, scope, tokens);
                }
            }
            StatementKind::Reactivate(r) => {
                self.index_expr(&r.target, scope, tokens);
                if let Some(timing) = &r.timing {
                    self.index_timing(timing, scope, tokens);
                }
            }
            StatementKind::Inner { .. } | StatementKind::Dummy => {}
        }
    }

    fn index_timing(
        &mut self,
        timing: &crate::ast::SimulationTiming,
        scope: ScopeId,
        tokens: &[Token],
    ) {
        use crate::ast::SimulationTiming::*;
        let expr = match timing {
            Delay(e) | After(e) | At(e) | Before(e) => e,
        };
        self.index_expr(expr, scope, tokens);
    }

    fn index_assignment(
        &mut self,
        assign: &Assignment,
        stmt_span: &Span,
        scope: ScopeId,
        tokens: &[Token],
    ) {
        self.index_variable_in_span(&assign.lhs, stmt_span, scope, tokens, true);
        match &assign.rhs {
            AssignmentRhs::Expr(expr) => self.index_expr(expr, scope, tokens),
            AssignmentRhs::Chain(inner) => self.index_assignment(inner, stmt_span, scope, tokens),
        }
    }

    fn index_procedure_call(
        &mut self,
        call: &ProcedureCall,
        stmt_span: &Span,
        scope: ScopeId,
        tokens: &[Token],
    ) {
        if let Some(span) = find_name_span(tokens, &call.name, stmt_span) {
            self.record_use(call.name.clone(), span, scope, false);
        }
        for arg in &call.arguments {
            self.index_expr(arg, scope, tokens);
        }
    }

    fn index_for_element(&mut self, element: &ForListElement, scope: ScopeId, tokens: &[Token]) {
        match element {
            ForListElement::Value { expr, while_cond }
            | ForListElement::Reference { expr, while_cond } => {
                self.index_expr(expr, scope, tokens);
                if let Some(cond) = while_cond {
                    self.index_expr(cond, scope, tokens);
                }
            }
            ForListElement::StepUntil { start, step, until } => {
                self.index_expr(start, scope, tokens);
                self.index_expr(step, scope, tokens);
                self.index_expr(until, scope, tokens);
            }
        }
    }

    fn index_designational(
        &mut self,
        expr: &DesignationalExpr,
        scope: ScopeId,
        tokens: &[Token],
        within: &Span,
    ) {
        match expr {
            DesignationalExpr::Label(name) => {
                if let Some(span) = find_name_span(tokens, name, within) {
                    self.record_use(name.clone(), span, scope, false);
                }
            }
            DesignationalExpr::SwitchDesignator { name: _, subscript } => {
                self.index_expr(subscript, scope, tokens);
            }
            DesignationalExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.index_expr(condition, scope, tokens);
                self.index_designational(then_expr, scope, tokens, within);
                self.index_designational(else_expr, scope, tokens, within);
            }
            DesignationalExpr::Paren(inner) => {
                self.index_designational(inner, scope, tokens, within);
            }
        }
    }

    fn index_expr(&mut self, expr: &Expr, scope: ScopeId, tokens: &[Token]) {
        match &expr.kind {
            ExprKind::Variable(var) => {
                self.index_variable_in_span(var, &expr.span, scope, tokens, false);
            }
            ExprKind::FunctionCall { name, arguments } => {
                let name_span =
                    find_name_span(tokens, name, &expr.span).unwrap_or_else(|| expr.span.clone());
                self.record_use(name.clone(), name_span, scope, false);
                for arg in arguments {
                    self.index_expr(arg, scope, tokens);
                }
            }
            ExprKind::RemoteAccess { object, attribute } => {
                self.index_expr(object, scope, tokens);
                if let Some(span) = find_attr_span(tokens, attribute, &expr.span)
                    && let Some(receiver) = simple_expr_receiver_name(object)
                {
                    self.record_remote_use(attribute.clone(), span, scope, false, receiver);
                }
            }
            ExprKind::RemoteCall {
                object,
                attribute,
                arguments,
            } => {
                self.index_expr(object, scope, tokens);
                if let Some(span) = find_attr_span(tokens, attribute, &expr.span)
                    && let Some(receiver) = simple_expr_receiver_name(object)
                {
                    self.record_remote_use(attribute.clone(), span, scope, false, receiver);
                }
                for arg in arguments {
                    self.index_expr(arg, scope, tokens);
                }
            }
            ExprKind::Unary { operand, .. } => self.index_expr(operand, scope, tokens),
            ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
                self.index_expr(left, scope, tokens);
                self.index_expr(right, scope, tokens);
            }
            ExprKind::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.index_expr(condition, scope, tokens);
                self.index_expr(then_expr, scope, tokens);
                self.index_expr(else_expr, scope, tokens);
            }
            ExprKind::Paren(inner) => self.index_expr(inner, scope, tokens),
            ExprKind::New {
                class_name,
                arguments,
            } => {
                let name_span = find_name_span(tokens, class_name, &expr.span)
                    .unwrap_or_else(|| expr.span.clone());
                self.record_use(class_name.clone(), name_span, scope, false);
                if let Some(args) = arguments {
                    for arg in args {
                        self.index_expr(arg, scope, tokens);
                    }
                }
            }
            ExprKind::Qua { object, class_name } => {
                self.index_expr(object, scope, tokens);
                let name_span = find_name_span(tokens, class_name, &expr.span)
                    .unwrap_or_else(|| expr.span.clone());
                self.record_use(class_name.clone(), name_span, scope, false);
            }
            ExprKind::This(class_name) => {
                let name_span = find_name_span(tokens, class_name, &expr.span)
                    .unwrap_or_else(|| expr.span.clone());
                self.record_use(class_name.clone(), name_span, scope, false);
            }
            ExprKind::StringLiteral(_)
            | ExprKind::CharacterLiteral(_)
            | ExprKind::BooleanLiteral(_)
            | ExprKind::Notext
            | ExprKind::NumberLiteral { .. }
            | ExprKind::None => {}
        }
    }

    fn index_variable_in_span(
        &mut self,
        var: &Variable,
        within: &Span,
        scope: ScopeId,
        tokens: &[Token],
        is_write: bool,
    ) {
        match var {
            Variable::Simple(name) => {
                if let Some(span) = find_name_span(tokens, name, within) {
                    self.record_use(name.clone(), span, scope, is_write);
                }
            }
            Variable::Subscripted { name, subscripts } => {
                if let Some(span) = find_name_span(tokens, name, within) {
                    self.record_use(name.clone(), span, scope, is_write);
                }
                for sub in subscripts {
                    self.index_expr(sub, scope, tokens);
                }
            }
            Variable::Qua { object, .. } => {
                self.index_variable_in_span(object, within, scope, tokens, false);
            }
            Variable::Remote { object, attribute } => {
                self.index_variable_in_span(object, within, scope, tokens, false);
                if let Some(span) = find_attr_span(tokens, attribute, within)
                    && let Some(receiver) = simple_variable_receiver_name(object)
                {
                    self.record_remote_use(attribute.clone(), span, scope, is_write, receiver);
                }
            }
            Variable::RemoteCall {
                object,
                attribute,
                arguments,
            } => {
                self.index_variable_in_span(object, within, scope, tokens, false);
                if let Some(span) = find_attr_span(tokens, attribute, within)
                    && let Some(receiver) = simple_variable_receiver_name(object)
                {
                    self.record_remote_use(attribute.clone(), span, scope, is_write, receiver);
                }
                for arg in arguments {
                    self.index_expr(arg, scope, tokens);
                }
            }
        }
    }

    fn record_use(&mut self, name: String, span: Span, scope: ScopeId, is_write: bool) {
        self.uses.push(NameUse {
            name,
            span,
            scope,
            definition: None,
            is_write,
            remote_receiver: None,
        });
    }

    fn record_remote_use(
        &mut self,
        name: String,
        span: Span,
        scope: ScopeId,
        is_write: bool,
        receiver: String,
    ) {
        self.uses.push(NameUse {
            name,
            span,
            scope,
            definition: None,
            is_write,
            remote_receiver: Some(receiver),
        });
    }

    fn resolve_uses(&mut self) {
        for i in 0..self.uses.len() {
            let (name, scope, remote_receiver) = {
                let u = &self.uses[i];
                (u.name.clone(), u.scope, u.remote_receiver.clone())
            };
            self.uses[i].definition = if let Some(receiver) = remote_receiver {
                self.lookup_remote_attribute(scope, &receiver, &name)
            } else {
                self.lookup(scope, &name)
            };
        }
    }

    fn lookup_remote_attribute(
        &self,
        scope: ScopeId,
        receiver: &str,
        attr: &str,
    ) -> Option<SymbolId> {
        let recv_id = self.lookup(scope, receiver)?;
        let class_name = match &self.symbol(recv_id).ty {
            Some(Type::ObjectRef(class)) => class.clone(),
            _ => return None,
        };
        self.lookup_class_attribute(&class_name, attr)
    }
}

fn span_contains(span: &Span, offset: usize) -> bool {
    if span.start == span.end {
        return offset == span.start;
    }
    offset >= span.start && offset < span.end
}

fn find_name_span(tokens: &[Token], name: &str, within: &Span) -> Option<Span> {
    tokens.iter().find_map(|token| {
        if token.span.start < within.start || token.span.end > within.end {
            return None;
        }
        match &token.kind {
            TokenKind::Identifier(id) if id.eq_ignore_ascii_case(name) => Some(token.span.clone()),
            TokenKind::Keyword(kw) if kw.as_str().eq_ignore_ascii_case(name) => {
                Some(token.span.clone())
            }
            _ => None,
        }
    })
}

/// Prefer the rightmost name match — remote attributes sit after `.`.
fn find_attr_span(tokens: &[Token], name: &str, within: &Span) -> Option<Span> {
    tokens.iter().rev().find_map(|token| {
        if token.span.start < within.start || token.span.end > within.end {
            return None;
        }
        match &token.kind {
            TokenKind::Identifier(id) if id.eq_ignore_ascii_case(name) => Some(token.span.clone()),
            TokenKind::Keyword(kw) if kw.as_str().eq_ignore_ascii_case(name) => {
                Some(token.span.clone())
            }
            _ => None,
        }
    })
}

fn simple_variable_receiver_name(var: &Variable) -> Option<String> {
    match var {
        Variable::Simple(name) | Variable::Subscripted { name, .. } => Some(name.clone()),
        Variable::Qua { .. } | Variable::Remote { .. } | Variable::RemoteCall { .. } => None,
    }
}

fn simple_expr_receiver_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Variable(var) => simple_variable_receiver_name(var),
        ExprKind::Paren(inner) => simple_expr_receiver_name(inner),
        _ => None,
    }
}

fn block_introduces_scope(block: &Block) -> bool {
    block.prefix.is_some()
        || !block.externals.is_empty()
        || !block.declarations.is_empty()
        || !block.arrays.is_empty()
        || !block.switches.is_empty()
        || !block.procedures.is_empty()
        || !block.classes.is_empty()
}

fn format_procedure_signature(procedure: &ProcedureDeclaration) -> String {
    let mut out = String::new();
    if let Some(ty) = &procedure.result_type {
        out.push_str(&ty.to_string());
        out.push(' ');
    }
    out.push_str("procedure ");
    out.push_str(&procedure.name);
    if !procedure.parameters.is_empty() {
        out.push('(');
        for (i, formal) in procedure.parameters.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            out.push_str(&format!("{} {}", formal.ty, formal.name));
        }
        out.push(')');
    }
    if procedure.is_external {
        out.push_str("; external");
    }
    out
}

fn format_class_signature(class: &ClassDeclaration) -> String {
    let mut out = String::new();
    if let Some(prefix) = &class.prefix {
        out.push_str(prefix);
        out.push(' ');
    }
    out.push_str("class ");
    out.push_str(&class.name);
    if !class.parameters.is_empty() {
        out.push('(');
        for (i, formal) in class.parameters.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            out.push_str(&format!("{} {}", formal.ty, formal.name));
        }
        out.push(')');
    }
    out
}

pub fn hover_markdown(symbol: &Symbol) -> String {
    let mut md = format!("```simula\n{}\n```\n", symbol.detail);
    let kind = match symbol.kind {
        SymbolKind::Builtin => "ENVIRONMENT / builtin",
        SymbolKind::Class => "class",
        SymbolKind::Procedure => "procedure",
        SymbolKind::Parameter => "parameter",
        SymbolKind::Label => "label",
        SymbolKind::Switch => "switch",
        SymbolKind::Array => "array",
        SymbolKind::Constant => "constant",
        SymbolKind::Variable => "variable",
    };
    md.push_str(&format!("\n_{kind}_\n"));
    md
}

pub fn keyword_hover(keyword: Keyword) -> String {
    format!("**`{}`** — Simula reserved word\n", keyword.as_str())
}

pub fn builtin_hover(name: &str) -> Option<String> {
    if is_environment_procedure(name) {
        let ty = builtin_result_type(name)
            .map(|t| format!(" → `{t}`"))
            .unwrap_or_default();
        return Some(format!(
            "```simula\n{name}{ty}\n```\n\n_ENVIRONMENT procedure_\n"
        ));
    }
    if is_environment_constant(name) {
        return Some(format!(
            "```simula\n{name}\n```\n\n_ENVIRONMENT constant_\n"
        ));
    }
    None
}

pub fn all_keywords() -> Vec<&'static str> {
    vec![
        "activate",
        "after",
        "and",
        "array",
        "at",
        "before",
        "begin",
        "boolean",
        "character",
        "class",
        "delay",
        "do",
        "else",
        "end",
        "eq",
        "eqv",
        "external",
        "false",
        "for",
        "ge",
        "go",
        "goto",
        "gt",
        "hidden",
        "if",
        "imp",
        "in",
        "inner",
        "inspect",
        "integer",
        "is",
        "label",
        "le",
        "long",
        "lt",
        "name",
        "ne",
        "new",
        "none",
        "not",
        "notext",
        "or",
        "otherwise",
        "prior",
        "procedure",
        "protected",
        "qua",
        "reactivate",
        "real",
        "ref",
        "short",
        "step",
        "switch",
        "text",
        "then",
        "this",
        "to",
        "true",
        "until",
        "value",
        "virtual",
        "when",
        "while",
    ]
}

pub fn builtin_completion_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    names.extend_from_slice(environment_procedures());
    names.extend_from_slice(environment_constants());
    names
}

pub fn token_at_offset(tokens: &[Token], offset: usize) -> Option<&Token> {
    tokens.iter().find(|t| span_contains(&t.span, offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};

    fn index(source: &str) -> (String, SymbolIndex) {
        let snap = analyze_document(source, &AnalysisOptions::default());
        assert!(
            snap.program.is_some(),
            "parse failed: {:?}",
            snap.diagnostics
        );
        let idx = SymbolIndex::build(snap.program.as_ref().unwrap(), snap.tokens.as_ref());
        (snap.text, idx)
    }

    #[test]
    fn indexes_variables_and_uses() {
        let (text, idx) = index("begin integer x; x := 1; end");
        let x_decl = idx
            .symbols
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case("x") && s.kind == SymbolKind::Variable)
            .expect("x decl");
        assert!(text[x_decl.name_span.clone()].eq_ignore_ascii_case("x"));
        assert!(
            idx.uses
                .iter()
                .any(|u| u.name.eq_ignore_ascii_case("x") && u.definition.is_some())
        );
    }

    #[test]
    fn indexes_procedures_and_classes() {
        let (_, idx) = index(
            "begin
               procedure p(n); integer n; begin end;
               class c; begin end;
             end",
        );
        assert!(idx.symbols.iter().any(|s| s.kind == SymbolKind::Procedure));
        assert!(idx.symbols.iter().any(|s| s.kind == SymbolKind::Class));
        assert!(idx.symbols.iter().any(|s| s.kind == SymbolKind::Parameter));
    }

    #[test]
    fn resolve_at_use_finds_definition() {
        let (text, idx) = index("begin integer count; count := 2; end");
        let use_offset = text.rfind("count").unwrap();
        let id = idx.resolve_at_offset(use_offset).expect("resolve");
        assert_eq!(idx.symbol(id).kind, SymbolKind::Variable);
    }

    #[test]
    fn goto_resolves_label_inside_then_compound() {
        let (text, idx) = index(
            "begin
               boolean ident;
               ident := true;
               if ident then begin
                 L: ident := false;
               end
               else goto L;
             end",
        );
        let use_offset = text.rfind("goto L").unwrap() + "goto ".len();
        let id = idx
            .resolve_at_offset(use_offset)
            .expect("resolve goto target");
        assert_eq!(idx.symbol(id).kind, SymbolKind::Label);
        assert!(text[idx.symbol(id).name_span.clone()].eq_ignore_ascii_case("L"));
    }

    #[test]
    fn goto_resolves_label_inside_then_block_with_locals() {
        let (text, idx) = index(
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
        );
        let use_offset = text.rfind("goto L").unwrap() + "goto ".len();
        let id = idx
            .resolve_at_offset(use_offset)
            .expect("resolve goto target");
        assert_eq!(idx.symbol(id).kind, SymbolKind::Label);
        assert!(text[idx.symbol(id).name_span.clone()].eq_ignore_ascii_case("L"));
    }
}
