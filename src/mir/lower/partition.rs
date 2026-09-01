//! Submodule of [`crate::mir::lower`].

use super::*;

/// Result of [`partition_procedures`]: outlined value/array-ref procs vs
/// call-site-inlined name and text/`ref` alias procedures.
pub(in crate::mir::lower) struct PartitionedProcedures<'a> {
    pub(in crate::mir::lower) value_procedures: Vec<&'a ProcedureDeclaration>,
    pub(in crate::mir::lower) name_param_procs: HashMap<String, &'a ProcedureDeclaration>,
    /// Text / `ref(C)` call-by-reference: inlined by sharing the caller's
    /// [`LocalId`] (true aliasing for `:-`).
    pub(in crate::mir::lower) ref_alias_procs: HashMap<String, &'a ProcedureDeclaration>,
    /// Outlined call-by-name procedures → free enclosing integer cells to
    /// pass as trailing [`MirType::RefI64`] parameters.
    pub(in crate::mir::lower) name_outline_free_cells: HashMap<String, Vec<String>>,
}

/// Splits local procedures into outlined MIR [`Function`]s and call-site
/// inlined procedures (call-by-name, or text/`ref` call-by-reference).
///
/// Recursive call-by-name procedures whose name formals are all integers are
/// **outlined**, with each name formal expanded into a `(get: FuncRef,
/// set: FuncRef, env: RefI64)` thunk triple (simple-var actuals; read-only
/// formals may take expression actuals via a temp cell). Non-recursive
/// Jensen stays inlined.
///
/// Mutually recursive formal-procedure procedures (simtst34 `P` ↔ `P2`) are
/// outlined with a `(func: FuncRef, env: RefI64)` fat pointer per formal
/// procedure parameter; non-recursive formal-proc procedures stay inlined.
///
/// External procedures that alias MIR-known builtins (`OutText`, ENV, …)
/// are skipped: call sites already special-case those names (§6.3.7).
pub(in crate::mir::lower) fn partition_procedures<'a>(
    procedures: &[(&'a ProcedureDeclaration, HashSet<String>)],
) -> Result<PartitionedProcedures<'a>, CompileError> {
    let mut value_procedures = Vec::new();
    let mut name_param_procs = HashMap::new();
    let mut ref_alias_procs = HashMap::new();
    let mut name_outline_free_cells = HashMap::new();
    let mut formal_proc_candidates: Vec<&'a ProcedureDeclaration> = Vec::new();
    for (procedure, enclosing_names) in procedures {
        if procedure.is_external {
            if is_mir_known_external(&procedure.name) {
                continue;
            }
            // Unresolved Simula `external procedure` declarations become empty
            // stubs so `check` can proceed; foreign kind stubs are lowered with
            // a [`ForeignAbi`] and bound by the backend.
            value_procedures.push(*procedure);
            continue;
        }
        let has_name = procedure_has_name_params(procedure);
        let has_formal_proc = procedure_has_formal_proc_params(procedure);
        let has_label_or_switch = procedure_has_label_or_switch_params(procedure);
        if has_name || has_formal_proc || has_label_or_switch {
            validate_name_param_procedure(procedure)?;
            if has_formal_proc && !has_label_or_switch && formal_proc_outline_eligible(procedure) {
                formal_proc_candidates.push(*procedure);
                continue;
            }
            // Formal procedures / labels / switches force call-site inlining
            // (no FuncRef / designational ABI yet), except recursive formal-proc
            // candidates handled above after cycle detection.
            if has_name
                && !has_formal_proc
                && !has_label_or_switch
                && name_procedure_outline_eligible(procedure)
            {
                let free = free_enclosing_scalar_names(procedure, enclosing_names);
                if !free.is_empty() {
                    name_outline_free_cells.insert(procedure.name.clone(), free);
                }
                value_procedures.push(*procedure);
            } else {
                name_param_procs.insert(procedure.name.clone(), *procedure);
            }
        } else if procedure_closes_over_enclosing_locals(procedure, enclosing_names)
            || procedure_has_outer_goto(procedure)
            || procedure_needs_ref_alias_inline(procedure)
        {
            // B8: a local procedure reading/writing an enclosing block's
            // locals has no `Function`-level access to them once outlined,
            // so it must be inlined at each call site instead (sharing the
            // caller's `LocalId`s), just like text/`ref` call-by-reference.
            // Non-local goto (§5.4.18) likewise requires the caller's CFG.
            if procedure_calls_self(procedure)
                && !procedure_closes_over_enclosing_locals(procedure, enclosing_names)
                && !procedure_has_outer_goto(procedure)
                && object_ref_alias_outline_eligible(procedure)
            {
                value_procedures.push(*procedure);
            } else {
                validate_ref_alias_procedure(procedure)?;
                ref_alias_procs.insert(procedure.name.clone(), *procedure);
            }
        } else {
            value_procedures.push(*procedure);
        }
    }

    // Outline formal-proc procedures that participate in a call-graph cycle
    // (self- or mutual recursion). Acyclic ones stay call-site inlined.
    let cyclic_formal = formal_proc_procs_in_cycles(&formal_proc_candidates);
    for procedure in formal_proc_candidates {
        if cyclic_formal
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&procedure.name))
        {
            let enclosing = procedures
                .iter()
                .find(|(p, _)| p.name.eq_ignore_ascii_case(&procedure.name))
                .map(|(_, names)| names);
            if let Some(enclosing_names) = enclosing {
                let free = free_enclosing_scalar_names(procedure, enclosing_names);
                if !free.is_empty() {
                    name_outline_free_cells.insert(procedure.name.clone(), free);
                }
            }
            value_procedures.push(procedure);
        } else {
            name_param_procs.insert(procedure.name.clone(), procedure);
        }
    }

    // Force-outline parameterless procedures used as formal-proc actuals to
    // outlined formal-proc procedures (simtst34 `Q1`/`Q2`), with free cells for
    // enclosing integer/boolean locals — so call sites can take FuncAddr shims.
    let outlined_formal: HashSet<String> = value_procedures
        .iter()
        .filter(|p| procedure_has_formal_proc_params(p))
        .map(|p| p.name.to_ascii_lowercase())
        .collect();
    if !outlined_formal.is_empty() {
        let mut actuals: HashSet<String> = HashSet::new();
        for procedure in &value_procedures {
            if outlined_formal.contains(&procedure.name.to_ascii_lowercase()) {
                collect_formal_proc_actual_names_in_block(&procedure.body, &mut |name| {
                    actuals.insert(name.to_ascii_lowercase());
                });
            }
        }
        // Also scan sibling procedures that call outlined formal-proc procs
        // (main is lowered separately; its actuals are handled at call sites
        // via resolve_known_procedure + invoke shims).
        for (procedure, enclosing_names) in procedures {
            if outlined_formal.contains(&procedure.name.to_ascii_lowercase()) {
                continue;
            }
            if procedure_calls_any_names(procedure, &outlined_formal) {
                collect_formal_proc_actual_names_in_block(&procedure.body, &mut |name| {
                    actuals.insert(name.to_ascii_lowercase());
                });
            }
            let _ = enclosing_names;
        }
        let mut promote: Vec<&'a ProcedureDeclaration> = Vec::new();
        for name in &actuals {
            if let Some(procedure) = ref_alias_procs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, p)| *p)
            {
                if procedure.parameters.is_empty() {
                    promote.push(procedure);
                }
            } else if let Some(procedure) = name_param_procs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, p)| *p)
            {
                if procedure.parameters.is_empty() && !procedure_has_formal_proc_params(procedure) {
                    promote.push(procedure);
                }
            }
        }
        for procedure in promote {
            ref_alias_procs.remove(&procedure.name);
            // Case-insensitive remove from name_param_procs.
            let keys: Vec<String> = name_param_procs
                .keys()
                .filter(|k| k.eq_ignore_ascii_case(&procedure.name))
                .cloned()
                .collect();
            for key in keys {
                name_param_procs.remove(&key);
            }
            let enclosing = procedures
                .iter()
                .find(|(p, _)| p.name.eq_ignore_ascii_case(&procedure.name))
                .map(|(_, names)| names);
            if let Some(enclosing_names) = enclosing {
                let free = free_enclosing_scalar_names(procedure, enclosing_names);
                if !free.is_empty() {
                    name_outline_free_cells.insert(procedure.name.clone(), free);
                }
            }
            if !value_procedures
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&procedure.name))
            {
                value_procedures.push(procedure);
            }
        }

        // Outlined formal-proc procedures that pass Q1/Q2 must themselves
        // capture every free cell those actuals need (so `P2` can pack Q1's
        // env for `found_error` even though P2's body never mentions it).
        // Likewise, callers of such procedures (`P` → `P2`) need the same
        // cells so they can forward free-cell env arguments.
        let mut changed_free = true;
        while changed_free {
            changed_free = false;
            let snapshot: HashMap<String, Vec<String>> = name_outline_free_cells.clone();
            for procedure in &value_procedures {
                let enclosing = procedures
                    .iter()
                    .find(|(p, _)| p.name.eq_ignore_ascii_case(&procedure.name))
                    .map(|(_, names)| names);
                let Some(enclosing_names) = enclosing else {
                    continue;
                };
                let mut callee_names = HashSet::new();
                collect_called_procedure_names_in_block(&procedure.body, &mut |name| {
                    callee_names.insert(name.to_ascii_lowercase());
                });
                collect_formal_proc_actual_names_in_block(&procedure.body, &mut |name| {
                    callee_names.insert(name.to_ascii_lowercase());
                });
                let mut extra = Vec::new();
                for (callee, free) in &snapshot {
                    if !callee_names
                        .iter()
                        .any(|n| n.eq_ignore_ascii_case(&callee.to_ascii_lowercase()))
                    {
                        continue;
                    }
                    for cell in free {
                        if enclosing_names.contains(&cell.to_ascii_lowercase()) {
                            extra.push(cell.clone());
                        }
                    }
                }
                if extra.is_empty() {
                    continue;
                }
                let entry = name_outline_free_cells
                    .entry(procedure.name.clone())
                    .or_default();
                for cell in extra {
                    if !entry.iter().any(|e| e.eq_ignore_ascii_case(&cell)) {
                        entry.push(cell);
                        changed_free = true;
                    }
                }
                entry.sort_by_key(|n| n.to_ascii_lowercase());
            }
        }
    }

    // Transitively inline callers of ref-alias procedures (e.g. `P2` that only
    // calls `Sjekk`, which closes over outer `testnr`). Otherwise the callee
    // is inlined into an outlined caller that has no outer locals.
    let mut changed = true;
    while changed {
        changed = false;
        let mut still_value = Vec::new();
        for procedure in value_procedures.drain(..) {
            // Keep outlined formal-proc / promoted actuals even if they call
            // remaining ref-alias procs — those calls stay as Call + free cells.
            if procedure_has_formal_proc_params(procedure)
                || name_outline_free_cells.contains_key(&procedure.name)
            {
                still_value.push(procedure);
                continue;
            }
            if procedure_calls_any(procedure, &ref_alias_procs) {
                validate_ref_alias_procedure(procedure)?;
                ref_alias_procs.insert(procedure.name.clone(), procedure);
                name_outline_free_cells.remove(&procedure.name);
                changed = true;
            } else {
                still_value.push(procedure);
            }
        }
        value_procedures = still_value;
    }

    Ok(PartitionedProcedures {
        value_procedures,
        name_param_procs,
        ref_alias_procs,
        name_outline_free_cells,
    })
}

pub(in crate::mir::lower) fn procedure_calls_any(
    procedure: &ProcedureDeclaration,
    callees: &HashMap<String, &ProcedureDeclaration>,
) -> bool {
    if callees.is_empty() {
        return false;
    }
    let mut found = false;
    collect_called_procedure_names_in_block(&procedure.body, &mut |name| {
        if callees.keys().any(|key| key.eq_ignore_ascii_case(name)) {
            found = true;
        }
    });
    found
}

pub(in crate::mir::lower) fn procedure_calls_any_names(
    procedure: &ProcedureDeclaration,
    names: &HashSet<String>,
) -> bool {
    if names.is_empty() {
        return false;
    }
    let mut found = false;
    collect_called_procedure_names_in_block(&procedure.body, &mut |name| {
        if names.contains(&name.to_ascii_lowercase()) {
            found = true;
        }
    });
    found
}

/// Formal-procedure procedures eligible for outlining: formal proc formals plus
/// optional integer/boolean name or scalar value formals (simtst34).
pub(in crate::mir::lower) fn formal_proc_outline_eligible(
    procedure: &ProcedureDeclaration,
) -> bool {
    if procedure.is_external || procedure_has_label_or_switch_params(procedure) {
        return false;
    }
    if !procedure_has_formal_proc_params(procedure) {
        return false;
    }
    procedure.parameters.iter().all(|param| {
        if param.is_procedure {
            return true;
        }
        if param.is_label || param.is_switch {
            return false;
        }
        match param.mode {
            ParamMode::Name => matches!(param.ty, Type::Integer { .. } | Type::Boolean),
            ParamMode::Value => !matches!(
                param.ty,
                Type::Array { .. } | Type::Text | Type::ObjectRef(_)
            ),
            ParamMode::Reference => false,
        }
    })
}

/// Names of formal-proc procedures that lie on a self- or mutual-recursion cycle.
pub(in crate::mir::lower) fn formal_proc_procs_in_cycles(
    candidates: &[&ProcedureDeclaration],
) -> HashSet<String> {
    if candidates.is_empty() {
        return HashSet::new();
    }
    let names: HashSet<String> = candidates
        .iter()
        .map(|p| p.name.to_ascii_lowercase())
        .collect();
    // Adjacency among candidates (direct calls to other candidates).
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
    for procedure in candidates {
        let from = procedure.name.to_ascii_lowercase();
        let mut tos = HashSet::new();
        collect_called_procedure_names_in_block(&procedure.body, &mut |name| {
            let lower = name.to_ascii_lowercase();
            if names.contains(&lower) {
                tos.insert(lower);
            }
        });
        adj.insert(from, tos);
    }
    // Reachability closure: keep nodes that can reach themselves.
    let mut cyclic = HashSet::new();
    for start in &names {
        let mut stack = vec![start.clone()];
        let mut seen = HashSet::new();
        while let Some(node) = stack.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            if let Some(nexts) = adj.get(&node) {
                for next in nexts {
                    if next == start {
                        cyclic.insert(start.clone());
                    }
                    stack.push(next.clone());
                }
            }
        }
    }
    cyclic
}

pub(in crate::mir::lower) fn collect_formal_proc_actual_names_in_block(
    block: &Block,
    visit: &mut impl FnMut(&str),
) {
    for statement in &block.statements {
        collect_formal_proc_actual_names_in_statement(statement, visit);
    }
    for procedure in &block.procedures {
        collect_formal_proc_actual_names_in_block(&procedure.body, visit);
    }
    for inner in &block.body {
        collect_formal_proc_actual_names_in_block(inner, visit);
    }
}

pub(in crate::mir::lower) fn collect_formal_proc_actual_names_in_statement(
    statement: &Statement,
    visit: &mut impl FnMut(&str),
) {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => {
            for argument in &call.arguments {
                if let Ok(name) = procedure_identifier_actual(argument) {
                    visit(&name);
                }
                collect_formal_proc_actual_names_in_expr(argument, visit);
            }
        }
        StatementKind::Compound(block) => collect_formal_proc_actual_names_in_block(block, visit),
        StatementKind::If(if_stmt) => {
            collect_formal_proc_actual_names_in_statement(&if_stmt.then_branch, visit);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_formal_proc_actual_names_in_statement(else_branch, visit);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_formal_proc_actual_names_in_statement(&while_stmt.body, visit)
        }
        StatementKind::For(for_stmt) => {
            collect_formal_proc_actual_names_in_statement(&for_stmt.body, visit)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_formal_proc_actual_names_in_statement(statement, visit)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_formal_proc_actual_names_in_statement(&when.body, visit);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_formal_proc_actual_names_in_statement(do_clause, visit);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_formal_proc_actual_names_in_statement(otherwise, visit);
            }
        }
        StatementKind::Assignment(assignment) => {
            if let AssignmentRhs::Expr(expr) = &assignment.rhs {
                collect_formal_proc_actual_names_in_expr(expr, visit);
            }
        }
        StatementKind::Expr(expr) => collect_formal_proc_actual_names_in_expr(expr, visit),
        _ => {}
    }
}

pub(in crate::mir::lower) fn collect_formal_proc_actual_names_in_expr(
    expr: &Expr,
    visit: &mut impl FnMut(&str),
) {
    match &expr.kind {
        ExprKind::FunctionCall { arguments, .. } | ExprKind::RemoteCall { arguments, .. } => {
            for argument in arguments {
                if let Ok(name) = procedure_identifier_actual(argument) {
                    visit(&name);
                }
                collect_formal_proc_actual_names_in_expr(argument, visit);
            }
        }
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            collect_formal_proc_actual_names_in_expr(inner, visit)
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            collect_formal_proc_actual_names_in_expr(left, visit);
            collect_formal_proc_actual_names_in_expr(right, visit);
        }
        ExprKind::Unary { operand, .. } => collect_formal_proc_actual_names_in_expr(operand, visit),
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_formal_proc_actual_names_in_expr(condition, visit);
            collect_formal_proc_actual_names_in_expr(then_expr, visit);
            collect_formal_proc_actual_names_in_expr(else_expr, visit);
        }
        ExprKind::RemoteAccess { object, .. } => {
            collect_formal_proc_actual_names_in_expr(object, visit)
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    collect_formal_proc_actual_names_in_expr(argument, visit);
                }
            }
        }
        _ => {}
    }
}

pub(in crate::mir::lower) fn collect_called_procedure_names_in_block(
    block: &Block,
    visit: &mut impl FnMut(&str),
) {
    for statement in &block.statements {
        collect_called_procedure_names_in_statement(statement, visit);
    }
    for procedure in &block.procedures {
        collect_called_procedure_names_in_block(&procedure.body, visit);
    }
    for inner in &block.body {
        collect_called_procedure_names_in_block(inner, visit);
    }
}

pub(in crate::mir::lower) fn collect_called_procedure_names_in_statement(
    statement: &Statement,
    visit: &mut impl FnMut(&str),
) {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => {
            visit(&call.name);
            for argument in &call.arguments {
                collect_called_procedure_names_in_expr(argument, visit);
            }
        }
        StatementKind::Compound(block) => collect_called_procedure_names_in_block(block, visit),
        StatementKind::If(if_stmt) => {
            collect_called_procedure_names_in_statement(&if_stmt.then_branch, visit);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_called_procedure_names_in_statement(else_branch, visit);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_called_procedure_names_in_statement(&while_stmt.body, visit)
        }
        StatementKind::For(for_stmt) => {
            collect_called_procedure_names_in_statement(&for_stmt.body, visit)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_called_procedure_names_in_statement(statement, visit)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_called_procedure_names_in_statement(&when.body, visit);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_called_procedure_names_in_statement(do_clause, visit);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_called_procedure_names_in_statement(otherwise, visit);
            }
        }
        StatementKind::Assignment(assignment) => {
            if let AssignmentRhs::Expr(expr) = &assignment.rhs {
                collect_called_procedure_names_in_expr(expr, visit);
            }
        }
        StatementKind::Expr(expr) => collect_called_procedure_names_in_expr(expr, visit),
        _ => {}
    }
}

pub(in crate::mir::lower) fn collect_called_procedure_names_in_expr(
    expr: &Expr,
    visit: &mut impl FnMut(&str),
) {
    match &expr.kind {
        ExprKind::FunctionCall { name, arguments } => {
            visit(name);
            for argument in arguments {
                collect_called_procedure_names_in_expr(argument, visit);
            }
        }
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            collect_called_procedure_names_in_expr(inner, visit)
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            collect_called_procedure_names_in_expr(left, visit);
            collect_called_procedure_names_in_expr(right, visit);
        }
        ExprKind::Unary { operand, .. } => collect_called_procedure_names_in_expr(operand, visit),
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_called_procedure_names_in_expr(condition, visit);
            collect_called_procedure_names_in_expr(then_expr, visit);
            collect_called_procedure_names_in_expr(else_expr, visit);
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            collect_called_procedure_names_in_expr(object, visit);
            for argument in arguments {
                collect_called_procedure_names_in_expr(argument, visit);
            }
        }
        ExprKind::RemoteAccess { object, .. } => {
            collect_called_procedure_names_in_expr(object, visit)
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    collect_called_procedure_names_in_expr(argument, visit);
                }
            }
        }
        _ => {}
    }
}

/// External procedure names that MIR already lowers as builtins / ENV / file
/// ops by identifier (§6.3.7). Separate-compilation FFI remains out of scope.
pub(in crate::mir::lower) fn is_mir_known_external(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "outtext"
            | "outimage"
            | "outint"
            | "outchar"
            | "breakoutimage"
            | "inimage"
            | "inchar"
            | "endfile"
            | "sysin"
            | "sysout"
            | "inline"
            | "upcase"
            | "lowcase"
            | "fileexists"
            | "fileread"
            | "filewrite"
            | "terminate_program"
    ) || crate::environment::is_environment_procedure(name)
        || crate::runtime::fs::is_filesystem_procedure(name)
}

pub(in crate::mir::lower) fn unknown_external_procedure_error(
    procedure: &ProcedureDeclaration,
) -> CompileError {
    spanned_error(
        format!(
            "MIR lowering: external procedure '{}' has no native implementation \
             (only ENVIRONMENT / BASICIO builtins such as OutText, OutImage, OutInt, \
             and fileExists are linked by name; separate-compilation FFI is not supported)",
            procedure.name
        ),
        procedure.span.clone(),
    )
}

/// Recursive integer/boolean name-param procs can be outlined with pointer formals.
pub(in crate::mir::lower) fn name_procedure_outline_eligible(
    procedure: &ProcedureDeclaration,
) -> bool {
    if !procedure_calls_self(procedure) {
        return false;
    }
    procedure.parameters.iter().all(|param| match param.mode {
        ParamMode::Name => matches!(param.ty, Type::Integer { .. } | Type::Boolean),
        ParamMode::Value => !matches!(
            param.ty,
            Type::Array { .. } | Type::Text | Type::ObjectRef(_)
        ),
        ParamMode::Reference => false,
    })
}

/// The single statement of a procedure body that declares nothing and contains
/// exactly one statement (unwrapping a redundant `begin … end` nesting).
pub(in crate::mir::lower) fn sole_procedure_body_statement(
    procedure: &ProcedureDeclaration,
) -> Option<&Statement> {
    fn sole_block_statement(block: &Block) -> Option<&Statement> {
        if !block.declarations.is_empty()
            || !block.arrays.is_empty()
            || !block.switches.is_empty()
            || !block.procedures.is_empty()
            || !block.classes.is_empty()
            || !block.externals.is_empty()
            || block.prefix.is_some()
        {
            return None;
        }
        match (block.statements.len(), block.body.len()) {
            (1, 0) => match &block.statements[0].kind {
                StatementKind::Compound(inner) => sole_block_statement(inner),
                _ => Some(&block.statements[0]),
            },
            (0, 1) => sole_block_statement(&block.body[0]),
            _ => None,
        }
    }
    sole_block_statement(&procedure.body)
}

/// For a parameterless type procedure whose whole body is `procname := expr`,
/// returns `expr`. Such a procedure is a pure abbreviation for its result
/// expression, so a call-by-name actual naming it can be lowered as a re-eval
/// thunk over `expr` instead of a snapshot of one call (simtst35 `P(sqri)`
/// with `integer procedure sqri; sqri := i * i`).
pub(in crate::mir::lower) fn type_procedure_simple_result_expr(
    procedure: &ProcedureDeclaration,
) -> Option<&Expr> {
    if procedure.result_type.is_none() || !procedure.parameters.is_empty() {
        return None;
    }
    let statement = sole_procedure_body_statement(procedure)?;
    let StatementKind::Assignment(assignment) = &statement.kind else {
        return None;
    };
    if assignment.operator != AssignOperator::Assign {
        return None;
    }
    let Variable::Simple(target) = &assignment.lhs else {
        return None;
    };
    if !target.eq_ignore_ascii_case(&procedure.name) {
        return None;
    }
    assignment.rhs.as_expr()
}

pub(in crate::mir::lower) fn procedure_calls_self(procedure: &ProcedureDeclaration) -> bool {
    block_calls_name(&procedure.body, &procedure.name)
}

pub(in crate::mir::lower) fn block_calls_name(block: &Block, name: &str) -> bool {
    statements_call_name(&block.statements, name)
        || block.body.iter().any(|inner| block_calls_name(inner, name))
}

pub(in crate::mir::lower) fn statements_call_name(statements: &[Statement], name: &str) -> bool {
    statements
        .iter()
        .any(|statement| statement_calls_name(statement, name))
}

pub(in crate::mir::lower) fn statement_calls_name(statement: &Statement, name: &str) -> bool {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => call.name.eq_ignore_ascii_case(name),
        StatementKind::Expr(expr) => expr_calls_name(expr, name),
        StatementKind::Assignment(assignment) => assignment_calls_name(assignment, name),
        StatementKind::If(if_stmt) => {
            expr_calls_name(&if_stmt.condition, name)
                || statement_calls_name(&if_stmt.then_branch, name)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|branch| statement_calls_name(branch, name))
        }
        StatementKind::While(while_stmt) => {
            expr_calls_name(&while_stmt.condition, name)
                || statement_calls_name(&while_stmt.body, name)
        }
        StatementKind::For(for_stmt) => statement_calls_name(&for_stmt.body, name),
        StatementKind::Labeled { statement, .. } => statement_calls_name(statement, name),
        StatementKind::Compound(block) => block_calls_name(block, name),
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|when| statement_calls_name(&when.body, name))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|body| statement_calls_name(body, name))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|body| statement_calls_name(body, name))
        }
        _ => false,
    }
}

pub(in crate::mir::lower) fn expr_calls_name(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::FunctionCall {
            name: callee,
            arguments,
        } => {
            callee.eq_ignore_ascii_case(name)
                || arguments.iter().any(|arg| expr_calls_name(arg, name))
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            expr_calls_name(left, name) || expr_calls_name(right, name)
        }
        ExprKind::Unary { operand, .. } => expr_calls_name(operand, name),
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            expr_calls_name(inner, name)
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_calls_name(condition, name)
                || expr_calls_name(then_expr, name)
                || expr_calls_name(else_expr, name)
        }
        ExprKind::RemoteAccess { object, .. } => expr_calls_name(object, name),
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            expr_calls_name(object, name) || arguments.iter().any(|arg| expr_calls_name(arg, name))
        }
        ExprKind::New {
            arguments: Some(arguments),
            ..
        } => arguments.iter().any(|arg| expr_calls_name(arg, name)),
        _ => false,
    }
}

pub(in crate::mir::lower) fn assignment_calls_name(assignment: &Assignment, name: &str) -> bool {
    match &assignment.rhs {
        AssignmentRhs::Expr(expr) => expr_calls_name(expr, name),
        AssignmentRhs::Chain(inner) => assignment_calls_name(inner, name),
    }
}

pub(in crate::mir::lower) fn procedure_has_name_params(procedure: &ProcedureDeclaration) -> bool {
    procedure
        .parameters
        .iter()
        .any(|param| param.mode == ParamMode::Name)
}

pub(in crate::mir::lower) fn procedure_has_formal_proc_params(
    procedure: &ProcedureDeclaration,
) -> bool {
    procedure.parameters.iter().any(|param| param.is_procedure)
}

/// Splits class methods into outlined MIR functions vs call-site-inlined
/// methods (formal procedure / label / switch parameters).
pub(in crate::mir::lower) fn partition_class_methods<'a>(
    methods: &[ClassMethod<'a>],
) -> (
    Vec<ClassMethod<'a>>,
    HashMap<String, &'a ProcedureDeclaration>,
) {
    let mut outline = Vec::new();
    let mut inline_map = HashMap::new();
    for method in methods {
        if procedure_has_formal_proc_params(method.procedure)
            || procedure_has_label_or_switch_params(method.procedure)
        {
            let mangled = mangle_method_name(method.class_name, &method.procedure.name);
            inline_map.insert(mangled, method.procedure);
        } else {
            outline.push(*method);
        }
    }
    (outline, inline_map)
}

pub(in crate::mir::lower) fn procedure_has_label_or_switch_params(
    procedure: &ProcedureDeclaration,
) -> bool {
    procedure
        .parameters
        .iter()
        .any(|param| param.is_label || param.is_switch)
}

pub(in crate::mir::lower) fn procedure_needs_ref_alias_inline(
    procedure: &ProcedureDeclaration,
) -> bool {
    procedure.parameters.iter().any(|param| {
        param.mode == ParamMode::Reference && matches!(param.ty, Type::Text | Type::ObjectRef(_))
    })
}

/// Recursive call-by-reference procedures whose formals are only `ref(C)`
/// (no text aliases) can be outlined: the object pointer is passed by value
/// and attribute updates still mutate the shared instance.
pub(in crate::mir::lower) fn object_ref_alias_outline_eligible(
    procedure: &ProcedureDeclaration,
) -> bool {
    procedure.parameters.iter().all(|param| {
        if param.is_procedure || param.mode == ParamMode::Name {
            return false;
        }
        match (&param.mode, &param.ty) {
            (ParamMode::Reference, Type::ObjectRef(_)) => true,
            (ParamMode::Reference, Type::Text) => false,
            (ParamMode::Reference, Type::Array { .. }) => true,
            (ParamMode::Value, Type::Text | Type::Array { .. }) => false,
            (ParamMode::Value, _) => true,
            _ => false,
        }
    })
}

/// Name-param / formal-procedure procedures are inlined; reject unsupported
/// heading features. Recursion is checked at each call site.
pub(in crate::mir::lower) fn validate_name_param_procedure(
    procedure: &ProcedureDeclaration,
) -> Result<(), CompileError> {
    if procedure.is_external {
        if is_mir_known_external(&procedure.name) {
            return Ok(());
        }
        return Err(unknown_external_procedure_error(procedure));
    }
    if procedure_has_formal_proc_params(procedure) && procedure_needs_ref_alias_inline(procedure) {
        return Err(spanned_error(
            format!(
                "MIR lowering: procedure '{}' mixes formal procedure parameters with text/ref call-by-reference, which is not supported yet",
                procedure.name
            ),
            procedure.span.clone(),
        ));
    }
    for param in &procedure.parameters {
        if param.is_procedure || param.is_label || param.is_switch {
            // Formal procedure / label / switch actuals are bound by rewriting;
            // no MIR type is materialised for the formal itself.
            continue;
        }
        if param.mode == ParamMode::Reference {
            let ty = mir_type_for(&param.ty)?;
            if !matches!(
                ty,
                MirType::Text
                    | MirType::ObjectRef
                    | MirType::ArrayI64
                    | MirType::ArrayF64
                    | MirType::ArrayText
            ) {
                return Err(spanned_error(
                    format!(
                        "MIR lowering: procedure '{}' parameter '{}': call-by-reference is only supported for text, object-reference, and array parameters",
                        procedure.name, param.name
                    ),
                    param.span.clone(),
                ));
            }
            continue;
        }
        let _ = mir_type_for(&param.ty)?;
    }
    if let Some(result_ty) = &procedure.result_type {
        let _ = mir_type_for(result_ty)?;
    }
    Ok(())
}

pub(in crate::mir::lower) fn validate_ref_alias_procedure(
    procedure: &ProcedureDeclaration,
) -> Result<(), CompileError> {
    if procedure.is_external {
        if is_mir_known_external(&procedure.name) {
            return Ok(());
        }
        return Err(unknown_external_procedure_error(procedure));
    }
    for param in &procedure.parameters {
        if param.is_procedure {
            return Err(spanned_error(
                format!(
                    "MIR lowering: procedure '{}' parameter '{}' is a formal procedure parameter, which is not supported in the scalar subset yet",
                    procedure.name, param.name
                ),
                param.span.clone(),
            ));
        }
        let ty = mir_type_for(&param.ty)?;
        match param.mode {
            ParamMode::Value => {
                // Scalars / text / arrays: arrays deep-copied at the call site.
            }
            ParamMode::Reference => {
                if !matches!(
                    ty,
                    MirType::Text
                        | MirType::ObjectRef
                        | MirType::ArrayI64
                        | MirType::ArrayF64
                        | MirType::ArrayText
                ) {
                    return Err(spanned_error(
                        format!(
                            "MIR lowering: procedure '{}' parameter '{}': call-by-reference is only supported for text, object-reference, and array parameters",
                            procedure.name, param.name
                        ),
                        param.span.clone(),
                    ));
                }
            }
            ParamMode::Name => {
                return Err(spanned_error(
                    format!(
                        "MIR lowering: internal error: name parameter '{}' should have been partitioned earlier",
                        param.name
                    ),
                    param.span.clone(),
                ));
            }
        }
    }
    if let Some(result_ty) = &procedure.result_type {
        let _ = mir_type_for(result_ty)?;
    }
    Ok(())
}
