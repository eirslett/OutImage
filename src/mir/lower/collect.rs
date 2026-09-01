//! Submodule of [`crate::mir::lower`].

use super::*;

/// Collect statement label names in the same order [`FunctionBuilder::lower_statement`]
/// / [`FunctionBuilder::lower_block`] will encounter them (for last-wins predeclare).
pub(in crate::mir::lower) fn collect_label_occurrence_names(
    statement: &Statement,
    out: &mut Vec<String>,
) {
    match &statement.kind {
        StatementKind::Labeled { label, statement } => {
            out.push(label.clone());
            collect_label_occurrence_names(statement, out);
        }
        StatementKind::Compound(block) => {
            collect_label_occurrence_names_in_block(block, out);
        }
        StatementKind::If(if_stmt) => {
            collect_label_occurrence_names(&if_stmt.then_branch, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_label_occurrence_names(else_branch, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_label_occurrence_names(&while_stmt.body, out);
        }
        StatementKind::For(for_stmt) => {
            collect_label_occurrence_names(&for_stmt.body, out);
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_label_occurrence_names(&when.body, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_label_occurrence_names(do_clause, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_label_occurrence_names(otherwise, out);
            }
        }
        _ => {}
    }
}

/// True when a designational expression might not transfer control (switch
/// subscript out of range is a no-op per Simula §4.5).
pub(in crate::mir::lower) fn designational_expr_may_fallthrough(expr: &DesignationalExpr) -> bool {
    match expr {
        DesignationalExpr::Label(_) => false,
        DesignationalExpr::SwitchDesignator { .. } => true,
        DesignationalExpr::Paren(inner) => designational_expr_may_fallthrough(inner),
        DesignationalExpr::If {
            then_expr,
            else_expr,
            ..
        } => {
            designational_expr_may_fallthrough(then_expr)
                || designational_expr_may_fallthrough(else_expr)
        }
    }
}

pub(in crate::mir::lower) fn collect_label_occurrence_names_in_block(
    block: &Block,
    out: &mut Vec<String>,
) {
    for statement in &block.statements {
        collect_label_occurrence_names(statement, out);
    }
    for inner in &block.body {
        collect_label_occurrence_names_in_block(inner, out);
    }
}

pub(in crate::mir::lower) fn register_switches_in_block(
    block: &Block,
    switches: &mut HashMap<String, Vec<crate::ast::DesignationalExpr>>,
) {
    for switch in &block.switches {
        switches.insert(switch.name.to_ascii_lowercase(), switch.elements.clone());
    }
    for inner in &block.body {
        register_switches_in_block(inner, switches);
    }
}

/// Names declared `external` that no source in this compilation defines, in
/// either the `external procedure p` head form or the `procedure p; external;`
/// body form. Multi-source units supply the real body, which is why this is a
/// whole-compilation question rather than a per-declaration one.
///
/// Foreign (`C` / `JS` / `Host`) stubs are bound by the backend and are not
/// unresolved Simula modules.
pub(in crate::mir::lower) fn collect_unresolved_externals(
    procedures: &[(&ProcedureDeclaration, HashSet<String>)],
    external_stubs: &[ExternalProcedureStub],
) -> Vec<crate::mir::UnresolvedExternal> {
    let defined: HashSet<String> = procedures
        .iter()
        .filter(|(procedure, _)| !procedure.is_external)
        .map(|(procedure, _)| procedure.name.to_ascii_lowercase())
        .collect();
    let declared = procedures
        .iter()
        .filter(|(procedure, _)| procedure.is_external)
        .map(|(procedure, _)| *procedure)
        .chain(external_stubs.iter().map(|stub| &stub.procedure));

    let mut seen = HashSet::new();
    let mut unresolved = Vec::new();
    for procedure in declared {
        let key = procedure.name.to_ascii_lowercase();
        if defined.contains(&key) || is_mir_known_external(&procedure.name) || !seen.insert(key) {
            continue;
        }
        if external_stubs.iter().any(|stub| {
            stub.procedure.name.eq_ignore_ascii_case(&procedure.name) && stub.foreign.is_some()
        }) {
            continue;
        }
        unresolved.push(crate::mir::UnresolvedExternal {
            name: procedure.name.clone(),
            providing_module: procedure.identification.clone(),
            span: procedure.span.clone(),
        });
    }
    unresolved
}

/// An `external procedure` lowered as a MIR stub, optionally with a foreign ABI.
#[derive(Debug, Clone)]
pub(in crate::mir::lower) struct ExternalProcedureStub {
    pub procedure: ProcedureDeclaration,
    pub foreign: Option<crate::mir::ForeignAbi>,
}

/// Synthesize stub [`ProcedureDeclaration`]s for `external procedure` items
/// so MIR check can resolve calls. Kind procedures carry a [`ForeignAbi`].
pub(in crate::mir::lower) fn collect_external_procedure_stubs(
    program: &Program,
) -> Vec<ExternalProcedureStub> {
    let mut stubs = Vec::new();
    let mut seen = HashSet::new();
    let mut visit = |externals: &[crate::ast::ExternalDeclaration]| {
        for external in externals {
            let ExternalDeclaration::Procedure(proc) = external else {
                continue;
            };
            if let Some(spec) = &proc.specification {
                if seen.insert(spec.name.to_ascii_lowercase()) {
                    let mut stub = spec.clone();
                    stub.is_external = true;
                    let identification = proc
                        .items
                        .first()
                        .and_then(|item| item.identification.as_deref());
                    stub.identification = identification.map(str::to_string);
                    let foreign =
                        foreign_abi_for(proc.kind.as_deref(), identification, spec, &proc.span);
                    stubs.push(ExternalProcedureStub {
                        procedure: stub,
                        foreign,
                    });
                }
                continue;
            }
            for item in &proc.items {
                if !seen.insert(item.name.to_ascii_lowercase()) {
                    continue;
                }
                stubs.push(ExternalProcedureStub {
                    procedure: ProcedureDeclaration {
                        result_type: proc.result_type.clone(),
                        name: item.name.clone(),
                        parameters: Vec::new(),
                        body: empty_procedure_body_block(),
                        is_external: true,
                        identification: item.identification.clone(),
                        span: proc.span.clone(),
                    },
                    foreign: None,
                });
            }
        }
    };
    visit(&program.external_head);
    for block in &program.blocks {
        visit_block_externals(block, &mut visit);
    }
    stubs
}

fn foreign_abi_for(
    kind: Option<&str>,
    identification: Option<&str>,
    spec: &ProcedureDeclaration,
    span: &Span,
) -> Option<crate::mir::ForeignAbi> {
    let kind = crate::mir::ForeignKind::parse(kind?)?;
    crate::mir::ForeignAbi::from_spec(kind, identification, spec, span.clone()).ok()
}

pub(in crate::mir::lower) fn visit_block_externals(
    block: &Block,
    visit: &mut impl FnMut(&[crate::ast::ExternalDeclaration]),
) {
    visit(&block.externals);
    for inner in &block.body {
        visit_block_externals(inner, visit);
    }
}

pub(in crate::mir::lower) fn empty_procedure_body_block() -> Block {
    Block {
        prefix: None,
        name: String::new(),
        directives: Vec::new(),
        externals: Vec::new(),
        declarations: Vec::new(),
        arrays: Vec::new(),
        switches: Vec::new(),
        procedures: Vec::new(),
        classes: Vec::new(),
        statements: Vec::new(),
        body: Vec::new(),
    }
}

/// Like [`collect_procedures`], but additionally records — for each
/// procedure — the set of simple variable/array/switch names declared in
/// its own block or any enclosing block (lower-cased). Used by
/// [`partition_procedures`] to detect local procedures that close over
/// enclosing locals (B8: nested procedure enclosing capture), which must be
/// inlined at each call site (like Jensen/`ref`-alias procedures) rather
/// than outlined into a standalone MIR `Function` with no access to the
/// caller's locals.
pub(in crate::mir::lower) fn collect_procedures_with_enclosing_names<'a>(
    block: &'a Block,
    outer_names: &HashSet<String>,
    out: &mut Vec<(&'a ProcedureDeclaration, HashSet<String>)>,
) {
    let mut names = outer_names.clone();
    add_block_own_data_names(block, &mut names);
    for procedure in &block.procedures {
        out.push((procedure, names.clone()));
        // Nested procedures (e.g. Bit0 inside Ipower) must be collected too —
        // otherwise call sites inside the parent see "unknown procedure".
        let mut nested_outer = names.clone();
        for param in &procedure.parameters {
            nested_outer.insert(param.name.to_ascii_lowercase());
        }
        collect_procedures_with_enclosing_names(&procedure.body, &nested_outer, out);
    }
    for statement in &block.statements {
        collect_procedures_from_statement(statement, &names, out);
    }
    for inner in &block.body {
        collect_procedures_with_enclosing_names(inner, &names, out);
    }
}

pub(in crate::mir::lower) fn collect_procedures_from_statement<'a>(
    statement: &'a Statement,
    enclosing_names: &HashSet<String>,
    out: &mut Vec<(&'a ProcedureDeclaration, HashSet<String>)>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => {
            collect_procedures_with_enclosing_names(block, enclosing_names, out);
        }
        StatementKind::If(if_stmt) => {
            collect_procedures_from_statement(&if_stmt.then_branch, enclosing_names, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_procedures_from_statement(else_branch, enclosing_names, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_procedures_from_statement(&while_stmt.body, enclosing_names, out);
        }
        StatementKind::For(for_stmt) => {
            collect_procedures_from_statement(&for_stmt.body, enclosing_names, out);
        }
        StatementKind::Labeled { statement, .. } => {
            collect_procedures_from_statement(statement, enclosing_names, out);
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_procedures_from_statement(&when.body, enclosing_names, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_procedures_from_statement(do_clause, enclosing_names, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_procedures_from_statement(otherwise, enclosing_names, out);
            }
        }
        _ => {}
    }
}

/// Adds the simple variable/array/switch names declared directly in `block`
/// (not procedures/classes: those are called by name, not captured as data,
/// so they never require closure inlining).
pub(in crate::mir::lower) fn add_block_own_data_names(block: &Block, names: &mut HashSet<String>) {
    for decl in &block.declarations {
        for item in &decl.items {
            names.insert(item.name.to_ascii_lowercase());
        }
    }
    for array in &block.arrays {
        for segment in &array.segments {
            for name in &segment.names {
                names.insert(name.to_ascii_lowercase());
            }
        }
    }
    for switch in &block.switches {
        names.insert(switch.name.to_ascii_lowercase());
    }
}

/// All names bound *within* `procedure` itself (parameters, its own result
/// name, and every variable/array/switch/for-control-variable/label declared
/// anywhere in its body, transitively). Used to tell apart genuine free
/// (enclosing-scope) references from the procedure's own locals.
pub(in crate::mir::lower) fn procedure_own_bound_names(
    procedure: &ProcedureDeclaration,
) -> HashSet<String> {
    let mut names = HashSet::new();
    names.insert(procedure.name.to_ascii_lowercase());
    for param in &procedure.parameters {
        names.insert(param.name.to_ascii_lowercase());
    }
    collect_block_bound_names(&procedure.body, &mut names);
    names
}

pub(in crate::mir::lower) fn collect_block_bound_names(block: &Block, names: &mut HashSet<String>) {
    add_block_own_data_names(block, names);
    for procedure in &block.procedures {
        names.insert(procedure.name.to_ascii_lowercase());
    }
    for class in &block.classes {
        names.insert(class.name.to_ascii_lowercase());
    }
    for statement in &block.statements {
        collect_statement_bound_names(statement, names);
    }
    for inner in &block.body {
        collect_block_bound_names(inner, names);
    }
}

pub(in crate::mir::lower) fn collect_statement_bound_names(
    statement: &Statement,
    names: &mut HashSet<String>,
) {
    match &statement.kind {
        StatementKind::For(for_stmt) => {
            // The for-control identifier is not a fresh binding in Simula
            // (§4.4) — it must already be declared in an enclosing block —
            // so it must *not* mask enclosing captures for `procedure_closes_over`.
            collect_statement_bound_names(&for_stmt.body, names);
        }
        StatementKind::While(while_stmt) => {
            collect_statement_bound_names(&while_stmt.body, names);
        }
        StatementKind::If(if_stmt) => {
            collect_statement_bound_names(&if_stmt.then_branch, names);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_statement_bound_names(else_branch, names);
            }
        }
        StatementKind::Compound(inner) => collect_block_bound_names(inner, names),
        StatementKind::Labeled { label, statement } => {
            names.insert(label.to_ascii_lowercase());
            collect_statement_bound_names(statement, names);
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_statement_bound_names(&when.body, names);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_statement_bound_names(do_clause, names);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_statement_bound_names(otherwise, names);
            }
        }
        _ => {}
    }
}

/// True when the procedure body `goto`s a label not defined inside the body
/// (§5.4.18 abandon-call). Such procedures must be call-site inlined so the
/// jump lands in the caller's CFG.
pub(in crate::mir::lower) fn procedure_has_outer_goto(procedure: &ProcedureDeclaration) -> bool {
    let mut names = Vec::new();
    collect_label_occurrence_names_in_block(&procedure.body, &mut names);
    let local_labels: HashSet<String> = names.into_iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut outer = false;
    collect_goto_label_names_in_block(&procedure.body, &mut |name| {
        if !local_labels.contains(&name.to_ascii_lowercase()) {
            outer = true;
        }
    });
    outer
}

pub(in crate::mir::lower) fn collect_goto_label_names_in_block(
    block: &Block,
    f: &mut dyn FnMut(&str),
) {
    for statement in &block.statements {
        collect_goto_label_names_in_statement(statement, f);
    }
    for inner in &block.body {
        collect_goto_label_names_in_block(inner, f);
    }
}

pub(in crate::mir::lower) fn collect_goto_label_names_in_statement(
    statement: &Statement,
    f: &mut dyn FnMut(&str),
) {
    match &statement.kind {
        StatementKind::Goto(goto) => collect_goto_label_names_in_designator(&goto.target, f),
        StatementKind::Labeled { statement, .. } => {
            collect_goto_label_names_in_statement(statement, f)
        }
        StatementKind::Compound(block) => collect_goto_label_names_in_block(block, f),
        StatementKind::If(if_stmt) => {
            collect_goto_label_names_in_statement(&if_stmt.then_branch, f);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_goto_label_names_in_statement(else_branch, f);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_goto_label_names_in_statement(&while_stmt.body, f)
        }
        StatementKind::For(for_stmt) => collect_goto_label_names_in_statement(&for_stmt.body, f),
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_goto_label_names_in_statement(&when.body, f);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_goto_label_names_in_statement(do_clause, f);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_goto_label_names_in_statement(otherwise, f);
            }
        }
        _ => {}
    }
}

pub(in crate::mir::lower) fn collect_goto_label_names_in_designator(
    target: &DesignationalExpr,
    f: &mut dyn FnMut(&str),
) {
    match target {
        DesignationalExpr::Label(name) => f(name),
        DesignationalExpr::Paren(inner) => collect_goto_label_names_in_designator(inner, f),
        DesignationalExpr::SwitchDesignator { .. } => {}
        DesignationalExpr::If {
            then_expr,
            else_expr,
            ..
        } => {
            collect_goto_label_names_in_designator(then_expr, f);
            collect_goto_label_names_in_designator(else_expr, f);
        }
    }
}

/// Whether `procedure`'s body reads/writes any of `enclosing_names` that
/// isn't one of the procedure's own bound names (B8: enclosing capture).
pub(in crate::mir::lower) fn procedure_closes_over_enclosing_locals(
    procedure: &ProcedureDeclaration,
    enclosing_names: &HashSet<String>,
) -> bool {
    if enclosing_names.is_empty() {
        return false;
    }
    let own = procedure_own_bound_names(procedure);
    let mut found = false;
    collect_variable_refs_in_block(&procedure.body, &mut |name| {
        let lower = name.to_ascii_lowercase();
        if !own.contains(&lower) && enclosing_names.contains(&lower) {
            found = true;
        }
    });
    found
}

/// Free enclosing integer names referenced by an outlined call-by-name
/// procedure — passed as trailing [`MirType::RefI64`] cell addresses.
pub(in crate::mir::lower) fn free_enclosing_scalar_names(
    procedure: &ProcedureDeclaration,
    enclosing_names: &HashSet<String>,
) -> Vec<String> {
    if enclosing_names.is_empty() {
        return Vec::new();
    }
    let own = procedure_own_bound_names(procedure);
    let mut free: Vec<String> = Vec::new();
    collect_variable_refs_in_block(&procedure.body, &mut |name| {
        let lower = name.to_ascii_lowercase();
        if own.contains(&lower) || name.eq_ignore_ascii_case(&procedure.name) {
            return;
        }
        if enclosing_names.contains(&lower)
            && !free
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            free.push(name.to_string());
        }
    });
    free.sort_by_key(|n| n.to_ascii_lowercase());
    free
}

/// Infers whether a free enclosing cell is boolean or integer from how the
/// outlined procedure uses it (boolean conditions / `not` / boolean literals).
pub(in crate::mir::lower) fn infer_free_cell_value_ty(
    procedure: &ProcedureDeclaration,
    name: &str,
) -> MirType {
    if name_used_as_boolean(&procedure.body, name) {
        MirType::Bool
    } else {
        MirType::I64
    }
}

pub(in crate::mir::lower) fn name_used_as_boolean(block: &Block, name: &str) -> bool {
    block
        .statements
        .iter()
        .any(|statement| statement_uses_name_as_boolean(statement, name))
        || block
            .body
            .iter()
            .any(|inner| name_used_as_boolean(inner, name))
}

pub(in crate::mir::lower) fn statement_uses_name_as_boolean(
    statement: &Statement,
    name: &str,
) -> bool {
    match &statement.kind {
        StatementKind::If(if_stmt) => {
            expr_is_boolean_use_of_name(&if_stmt.condition, name)
                || statement_uses_name_as_boolean(&if_stmt.then_branch, name)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|s| statement_uses_name_as_boolean(s, name))
        }
        StatementKind::While(while_stmt) => {
            expr_is_boolean_use_of_name(&while_stmt.condition, name)
                || statement_uses_name_as_boolean(&while_stmt.body, name)
        }
        StatementKind::Assignment(assignment) => {
            let lhs_is_name = matches!(
                &assignment.lhs,
                Variable::Simple(lhs) if lhs.eq_ignore_ascii_case(name)
            );
            if lhs_is_name {
                match &assignment.rhs {
                    AssignmentRhs::Expr(expr) => {
                        matches!(expr.kind, ExprKind::BooleanLiteral(_))
                            || matches!(
                                &expr.kind,
                                ExprKind::Unary {
                                    op: UnaryOp::Not,
                                    ..
                                }
                            )
                    }
                    _ => false,
                }
            } else {
                false
            }
        }
        StatementKind::Compound(block) => name_used_as_boolean(block, name),
        StatementKind::Labeled { statement, .. } => statement_uses_name_as_boolean(statement, name),
        StatementKind::For(for_stmt) => statement_uses_name_as_boolean(&for_stmt.body, name),
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|when| statement_uses_name_as_boolean(&when.body, name))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|s| statement_uses_name_as_boolean(s, name))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|s| statement_uses_name_as_boolean(s, name))
        }
        _ => false,
    }
}

/// True when `name` itself is used as a boolean value (bare condition / `not`),
/// not merely mentioned inside a larger boolean expression like `i = 3`.
pub(in crate::mir::lower) fn expr_is_boolean_use_of_name(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(n)) => n.eq_ignore_ascii_case(name),
        ExprKind::Paren(inner) => expr_is_boolean_use_of_name(inner, name),
        ExprKind::Unary {
            op: UnaryOp::Not,
            operand,
        } => expr_is_boolean_use_of_name(operand, name),
        _ => false,
    }
}

pub(in crate::mir::lower) fn collect_variable_refs_in_block(
    block: &Block,
    visit: &mut impl FnMut(&str),
) {
    for statement in &block.statements {
        collect_variable_refs_in_statement(statement, visit);
    }
    for inner in &block.body {
        collect_variable_refs_in_block(inner, visit);
    }
}

pub(in crate::mir::lower) fn collect_variable_refs_in_statement(
    statement: &Statement,
    visit: &mut impl FnMut(&str),
) {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => {
            for arg in &call.arguments {
                collect_variable_refs_in_expr(arg, visit);
            }
        }
        StatementKind::Assignment(assignment) => {
            collect_variable_refs_in_assignment(assignment, visit)
        }
        StatementKind::If(if_stmt) => {
            collect_variable_refs_in_expr(&if_stmt.condition, visit);
            collect_variable_refs_in_statement(&if_stmt.then_branch, visit);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_variable_refs_in_statement(else_branch, visit);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_variable_refs_in_expr(&while_stmt.condition, visit);
            collect_variable_refs_in_statement(&while_stmt.body, visit);
        }
        StatementKind::For(for_stmt) => {
            // Control variable is an enclosing (or local) binding, not a fresh
            // declaration — count it as a use for enclosing-capture detection.
            visit(&for_stmt.variable);
            for element in &for_stmt.elements {
                match element {
                    ForListElement::Value { expr, while_cond }
                    | ForListElement::Reference { expr, while_cond } => {
                        collect_variable_refs_in_expr(expr, visit);
                        if let Some(cond) = while_cond {
                            collect_variable_refs_in_expr(cond, visit);
                        }
                    }
                    ForListElement::StepUntil { start, step, until } => {
                        collect_variable_refs_in_expr(start, visit);
                        collect_variable_refs_in_expr(step, visit);
                        collect_variable_refs_in_expr(until, visit);
                    }
                }
            }
            collect_variable_refs_in_statement(&for_stmt.body, visit);
        }
        StatementKind::Goto(_) => {}
        StatementKind::Compound(block) => collect_variable_refs_in_block(block, visit),
        StatementKind::Labeled { statement, .. } => {
            collect_variable_refs_in_statement(statement, visit);
        }
        StatementKind::Expr(expr) => collect_variable_refs_in_expr(expr, visit),
        StatementKind::Dummy => {}
        StatementKind::ObjectGenerator(generator) => {
            for arg in &generator.arguments {
                collect_variable_refs_in_expr(arg, visit);
            }
        }
        StatementKind::Inner { .. } => {}
        StatementKind::Inspect(inspect) => {
            collect_variable_refs_in_expr(&inspect.object, visit);
            for when in &inspect.when_clauses {
                collect_variable_refs_in_statement(&when.body, visit);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_variable_refs_in_statement(do_clause, visit);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_variable_refs_in_statement(otherwise, visit);
            }
        }
        StatementKind::Activate(activate) => {
            collect_variable_refs_in_expr(&activate.target, visit);
            if let Some(timing) = &activate.timing {
                collect_variable_refs_in_timing(timing, visit);
            }
        }
        StatementKind::Reactivate(reactivate) => {
            collect_variable_refs_in_expr(&reactivate.target, visit);
            if let Some(timing) = &reactivate.timing {
                collect_variable_refs_in_timing(timing, visit);
            }
        }
    }
}

pub(in crate::mir::lower) fn collect_variable_refs_in_timing(
    timing: &SimulationTiming,
    visit: &mut impl FnMut(&str),
) {
    match timing {
        SimulationTiming::Delay(expr)
        | SimulationTiming::After(expr)
        | SimulationTiming::At(expr)
        | SimulationTiming::Before(expr) => collect_variable_refs_in_expr(expr, visit),
    }
}

pub(in crate::mir::lower) fn collect_variable_refs_in_assignment(
    assignment: &Assignment,
    visit: &mut impl FnMut(&str),
) {
    collect_variable_refs_in_variable(&assignment.lhs, visit);
    match &assignment.rhs {
        AssignmentRhs::Expr(expr) => collect_variable_refs_in_expr(expr, visit),
        AssignmentRhs::Chain(chain) => collect_variable_refs_in_assignment(chain, visit),
    }
}

pub(in crate::mir::lower) fn collect_variable_refs_in_variable(
    variable: &Variable,
    visit: &mut impl FnMut(&str),
) {
    match variable {
        Variable::Simple(name) => visit(name),
        Variable::Subscripted { name, subscripts } => {
            visit(name);
            for sub in subscripts {
                collect_variable_refs_in_expr(sub, visit);
            }
        }
        Variable::Remote { object, .. } => collect_variable_refs_in_variable(object, visit),
        Variable::Qua { object, .. } => collect_variable_refs_in_variable(object, visit),
        Variable::RemoteCall {
            object, arguments, ..
        } => {
            collect_variable_refs_in_variable(object, visit);
            for arg in arguments {
                collect_variable_refs_in_expr(arg, visit);
            }
        }
    }
}

pub(in crate::mir::lower) fn collect_variable_refs_in_expr(
    expr: &Expr,
    visit: &mut impl FnMut(&str),
) {
    match &expr.kind {
        ExprKind::Variable(variable) => collect_variable_refs_in_variable(variable, visit),
        ExprKind::Unary { operand, .. } => collect_variable_refs_in_expr(operand, visit),
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            collect_variable_refs_in_expr(left, visit);
            collect_variable_refs_in_expr(right, visit);
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_variable_refs_in_expr(condition, visit);
            collect_variable_refs_in_expr(then_expr, visit);
            collect_variable_refs_in_expr(else_expr, visit);
        }
        ExprKind::Paren(inner) => collect_variable_refs_in_expr(inner, visit),
        // `a(i)` parses as FunctionCall; treat the callee name as a variable
        // reference so enclosing array captures force call-site inlining.
        ExprKind::FunctionCall { name, arguments } => {
            visit(name);
            for arg in arguments {
                collect_variable_refs_in_expr(arg, visit);
            }
        }
        ExprKind::RemoteAccess { object, .. } => collect_variable_refs_in_expr(object, visit),
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            collect_variable_refs_in_expr(object, visit);
            for arg in arguments {
                collect_variable_refs_in_expr(arg, visit);
            }
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for arg in arguments {
                    collect_variable_refs_in_expr(arg, visit);
                }
            }
        }
        ExprKind::Qua { object, .. } => collect_variable_refs_in_expr(object, visit),
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::This(_) => {}
    }
}
