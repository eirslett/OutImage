//! Submodule of [`crate::mir::lower`].

use super::*;

/// Gathers non-fictitious procedures declared inside class bodies (Phase 5
/// method MVP). Nested `begin ... end` heads inside a class are walked the
/// same way as [`collect_procedures`].
pub(in crate::mir::lower) fn collect_class_methods<'a>(
    block: &'a Block,
    out: &mut Vec<ClassMethod<'a>>,
) {
    for class in &block.classes {
        collect_methods_from_class_body(&class.name, &class.body, out);
    }
    for inner in &block.body {
        collect_class_methods(inner, out);
    }
    for statement in &block.statements {
        collect_class_methods_from_statement(statement, out);
    }
}

pub(in crate::mir::lower) fn collect_class_methods_from_statement<'a>(
    statement: &'a Statement,
    out: &mut Vec<ClassMethod<'a>>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => collect_class_methods(block, out),
        StatementKind::If(if_stmt) => {
            collect_class_methods_from_statement(&if_stmt.then_branch, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_class_methods_from_statement(else_branch, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_class_methods_from_statement(&while_stmt.body, out)
        }
        StatementKind::For(for_stmt) => collect_class_methods_from_statement(&for_stmt.body, out),
        StatementKind::Labeled { statement, .. } => {
            collect_class_methods_from_statement(statement, out)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_class_methods_from_statement(&when.body, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_class_methods_from_statement(do_clause, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_class_methods_from_statement(otherwise, out);
            }
        }
        _ => {}
    }
}

pub(in crate::mir::lower) fn collect_methods_from_class_body<'a>(
    class_name: &'a str,
    body: &'a Block,
    out: &mut Vec<ClassMethod<'a>>,
) {
    for procedure in &body.procedures {
        if is_fictitious_detach(&procedure.name) {
            continue;
        }
        out.push(ClassMethod {
            class_name,
            procedure,
        });
    }
    for class in &body.classes {
        collect_methods_from_class_body(&class.name, &class.body, out);
    }
    for inner in &body.body {
        collect_methods_from_class_body(class_name, inner, out);
    }
    for statement in &body.statements {
        match &statement.kind {
            StatementKind::Compound(block) => {
                collect_methods_from_class_body(class_name, block, out)
            }
            StatementKind::If(if_stmt) => {
                collect_methods_from_class_body_stmt(class_name, &if_stmt.then_branch, out);
                if let Some(else_branch) = &if_stmt.else_branch {
                    collect_methods_from_class_body_stmt(class_name, else_branch, out);
                }
            }
            StatementKind::While(while_stmt) => {
                collect_methods_from_class_body_stmt(class_name, &while_stmt.body, out)
            }
            StatementKind::Labeled { statement, .. } => {
                collect_methods_from_class_body_stmt(class_name, statement, out)
            }
            _ => {}
        }
    }
}

pub(in crate::mir::lower) fn collect_methods_from_class_body_stmt<'a>(
    class_name: &'a str,
    statement: &'a Statement,
    out: &mut Vec<ClassMethod<'a>>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => collect_methods_from_class_body(class_name, block, out),
        StatementKind::If(if_stmt) => {
            collect_methods_from_class_body_stmt(class_name, &if_stmt.then_branch, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_methods_from_class_body_stmt(class_name, else_branch, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_methods_from_class_body_stmt(class_name, &while_stmt.body, out)
        }
        StatementKind::For(for_stmt) => {
            collect_methods_from_class_body_stmt(class_name, &for_stmt.body, out)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_methods_from_class_body_stmt(class_name, statement, out)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_methods_from_class_body_stmt(class_name, &when.body, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_methods_from_class_body_stmt(class_name, do_clause, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_methods_from_class_body_stmt(class_name, otherwise, out);
            }
        }
        _ => {}
    }
}

/// Collects procedures nested inside class methods (and nested further), so
/// call sites like `Bit0(exp)` inside `Ipower` resolve. Class attribute names
/// are part of the enclosing set so nested procedures that read `lnr` (etc.)
/// are forced to call-site inline with `__this` in scope.
pub(in crate::mir::lower) fn collect_nested_procedures_from_methods<'a>(
    methods: &[ClassMethod<'a>],
    layouts: &HashMap<String, ClassLayout>,
    out: &mut Vec<(&'a ProcedureDeclaration, HashSet<String>)>,
) {
    for method in methods {
        let mut enclosing = HashSet::new();
        for param in &method.procedure.parameters {
            enclosing.insert(param.name.to_ascii_lowercase());
        }
        add_block_own_data_names(&method.procedure.body, &mut enclosing);
        if let Some(layout) = layouts.get(method.class_name).or_else(|| {
            layouts
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(method.class_name))
                .map(|(_, layout)| layout)
        }) {
            for field in &layout.fields {
                if field.name.starts_with("__simrt_") {
                    continue;
                }
                enclosing.insert(field.name.to_ascii_lowercase());
            }
        }
        collect_procedures_with_enclosing_names(&method.procedure.body, &enclosing, out);
    }
}

/// Whether `class`'s own (pre-concatenation) body literally declares
/// `method` — i.e. whether [`collect_class_methods`] emits a
/// [`mangle_method_name`] symbol for `class` itself, as opposed to `class`
/// merely inheriting the method from a prefix ancestor.
pub(in crate::mir::lower) fn class_declares_method_directly(
    class: &ClassDeclaration,
    method: &str,
) -> bool {
    fn block_declares(block: &Block, method: &str) -> bool {
        block.procedures.iter().any(|procedure| {
            !is_fictitious_detach(&procedure.name) && procedure.name.eq_ignore_ascii_case(method)
        }) || block.body.iter().any(|inner| block_declares(inner, method))
    }
    block_declares(&class.body, method)
}

/// Walks `static_class`'s prefix chain (closest first) to find which raw
/// class literally declares `method` in its own body. A subclass's
/// [`ClassLayout`] (from `layout::layouts_from_classes`) reports every
/// inherited method as its own via concatenation, but MIR only synthesizes
/// a [`mangle_method_name`] function for the class that actually declares
/// the procedure (see [`collect_class_methods`]) — so a call through a
/// subclass-typed reference to a non-overridden method must still mangle
/// against the defining ancestor, not the static receiver class.
pub(in crate::mir::lower) fn defining_class_for_method<'a>(
    classes: &'a HashMap<String, ClassDeclaration>,
    static_class: &'a str,
    method: &str,
) -> &'a str {
    let mut current = static_class;
    loop {
        let Some(class) = classes.get(current).or_else(|| {
            classes
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(current))
                .map(|(_, class)| class)
        }) else {
            return static_class;
        };
        if class_declares_method_directly(class, method) {
            return class.name.as_str();
        }
        match class.prefix.as_deref() {
            Some(prefix) => current = prefix,
            None => return static_class,
        }
    }
}

/// Builds synthetic `ClassName$__init` bodies from concatenated class
/// declarations (so prefix initial statements run in order). Emitted when the
/// class has constructor parameters and/or non-dummy body statements.
pub(in crate::mir::lower) fn collect_class_inits(
    program: &Program,
    layouts: &HashMap<String, ClassLayout>,
) -> Result<Vec<ClassInit>, CompileError> {
    let mut raw = Vec::new();
    for block in &program.blocks {
        collect_raw_classes(block, &mut raw);
    }
    inject_basicio_classes_into_raw(&mut raw);
    if layout::program_needs_simulation_system_classes(program) {
        inject_system_classes_into_raw(&mut raw);
    }
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let enclosing_switches = collect_enclosing_switches_for_program(program);
    let concatenated = concatenate::concatenate_classes(&raw)?;
    let mut inits = Vec::new();
    for (name, class) in concatenated {
        let constructor_params = layouts
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(&name))
            .map(|(_, layout)| layout.constructor_params.clone())
            .unwrap_or_default();
        if class_body_has_init_statements(&class.body)
            || !constructor_params.is_empty()
            || !class.tail_statements.is_empty()
        {
            let switches = enclosing_switches
                .iter()
                .find(|(class_name, _)| class_name.eq_ignore_ascii_case(&name))
                .map(|(_, switches)| switches.clone())
                .unwrap_or_default();
            inits.push(ClassInit {
                class_name: name,
                body: class.body,
                tail_statements: class.tail_statements,
                constructor_params,
                enclosing_switches: switches,
            });
        }
    }
    Ok(inits)
}

/// Maps each raw class name to switch declarations in scope at its declaration
/// site (enclosing block switches, §5.6.13).
pub(in crate::mir::lower) fn collect_enclosing_switches_for_program(
    program: &Program,
) -> HashMap<String, HashMap<String, Vec<crate::ast::DesignationalExpr>>> {
    let mut out = HashMap::new();
    for block in &program.blocks {
        collect_enclosing_switches_in_block(block, &HashMap::new(), &mut out);
    }
    out
}

pub(in crate::mir::lower) fn collect_enclosing_switches_in_block(
    block: &Block,
    outer: &HashMap<String, Vec<crate::ast::DesignationalExpr>>,
    out: &mut HashMap<String, HashMap<String, Vec<crate::ast::DesignationalExpr>>>,
) {
    let mut switches = outer.clone();
    for switch in &block.switches {
        switches.insert(switch.name.to_ascii_lowercase(), switch.elements.clone());
    }
    for class in &block.classes {
        out.insert(class.name.clone(), switches.clone());
        collect_enclosing_switches_in_block(&class.body, &switches, out);
    }
    for procedure in &block.procedures {
        collect_enclosing_switches_in_block(&procedure.body, &switches, out);
    }
    for inner in &block.body {
        collect_enclosing_switches_in_block(inner, &switches, out);
    }
    for statement in &block.statements {
        collect_enclosing_switches_from_statement(statement, &switches, out);
    }
}

pub(in crate::mir::lower) fn collect_enclosing_switches_from_statement(
    statement: &Statement,
    switches: &HashMap<String, Vec<crate::ast::DesignationalExpr>>,
    out: &mut HashMap<String, HashMap<String, Vec<crate::ast::DesignationalExpr>>>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => collect_enclosing_switches_in_block(block, switches, out),
        StatementKind::If(if_stmt) => {
            collect_enclosing_switches_from_statement(&if_stmt.then_branch, switches, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_enclosing_switches_from_statement(else_branch, switches, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_enclosing_switches_from_statement(&while_stmt.body, switches, out);
        }
        StatementKind::For(for_stmt) => {
            collect_enclosing_switches_from_statement(&for_stmt.body, switches, out);
        }
        StatementKind::Labeled { statement, .. } => {
            collect_enclosing_switches_from_statement(statement, switches, out);
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_enclosing_switches_from_statement(&when.body, switches, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_enclosing_switches_from_statement(do_clause, switches, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_enclosing_switches_from_statement(otherwise, switches, out);
            }
        }
        _ => {}
    }
}

pub(in crate::mir::lower) fn inject_basicio_classes_into_raw(raw: &mut Vec<ClassDeclaration>) {
    let mut map: HashMap<String, ClassDeclaration> = HashMap::new();
    for class in raw.iter() {
        map.insert(class.name.clone(), class.clone());
    }
    // Stubs only — layout concatenates the prefix chain once.
    basicio::inject_system_class_stubs(&mut map);
    for class in map.into_values() {
        let exists = raw
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&class.name));
        if !exists {
            raw.push(class);
        }
    }
}

pub(in crate::mir::lower) fn inject_system_classes_into_raw(raw: &mut Vec<ClassDeclaration>) {
    let mut map: HashMap<String, ClassDeclaration> = HashMap::new();
    for class in raw.iter() {
        map.insert(class.name.clone(), class.clone());
    }
    crate::simulation::inject_system_classes(&mut map);
    for class in map.into_values() {
        let exists = raw
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&class.name));
        if !exists {
            raw.push(class);
        }
    }
}

pub(in crate::mir::lower) fn class_body_has_init_statements(body: &Block) -> bool {
    !body.arrays.is_empty()
        || body
            .statements
            .iter()
            .any(|statement| !matches!(statement.kind, StatementKind::Dummy))
        || body.body.iter().any(class_body_has_init_statements)
}

/// Raw class declarations from `program`, keyed by name (for `qua` subclass
/// checks via [`is_subclass_of`]).
pub(in crate::mir::lower) fn class_map_for_program(
    program: &Program,
) -> HashMap<String, ClassDeclaration> {
    let mut raw = Vec::new();
    for block in &program.blocks {
        collect_raw_classes(block, &mut raw);
    }
    inject_basicio_classes_into_raw(&mut raw);
    if layout::program_needs_simulation_system_classes(program) {
        inject_system_classes_into_raw(&mut raw);
    }
    // Keep raw (pre-concatenation) declarations here: method mangling via
    // [`defining_class_for_method`] must see which class *literally* declares a
    // procedure. Identifier substitutions for shadowed fields are resolved
    // through [`FunctionBuilder::remote_storage_name`] (concatenated view).
    raw.into_iter()
        .map(|class| (class.name.clone(), class))
        .collect()
}

pub(in crate::mir::lower) fn collect_raw_classes(block: &Block, out: &mut Vec<ClassDeclaration>) {
    for class in &block.classes {
        out.push(class.clone());
        // Nested classes declared in the class body (e.g. `Class C` inside
        // `Link Class A`) must be collected for `__init` emission — same as
        // [`layout::collect_classes_with_sibling_captures`].
        collect_raw_classes(&class.body, out);
    }
    for procedure in &block.procedures {
        collect_raw_classes(&procedure.body, out);
    }
    for inner in &block.body {
        collect_raw_classes(inner, out);
    }
    for statement in &block.statements {
        collect_raw_classes_from_statement(statement, out);
    }
}

pub(in crate::mir::lower) fn collect_raw_classes_from_statement(
    statement: &Statement,
    out: &mut Vec<ClassDeclaration>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => collect_raw_classes(block, out),
        StatementKind::If(if_stmt) => {
            collect_raw_classes_from_statement(&if_stmt.then_branch, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_raw_classes_from_statement(else_branch, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_raw_classes_from_statement(&while_stmt.body, out);
        }
        StatementKind::For(for_stmt) => {
            collect_raw_classes_from_statement(&for_stmt.body, out);
        }
        StatementKind::Labeled { statement, .. } => {
            collect_raw_classes_from_statement(statement, out);
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_raw_classes_from_statement(&when.body, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_raw_classes_from_statement(do_clause, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_raw_classes_from_statement(otherwise, out);
            }
        }
        _ => {}
    }
}

/// Builds the `name -> signature` table used to type-check and lower call
/// sites, up front so forward references and (mutual) recursion resolve
/// without a second pass. Class methods are keyed by
/// [`mangle_method_name`] and include a leading [`MirType::ObjectRef`]
/// receiver parameter.
pub(in crate::mir::lower) fn build_signatures(
    procedures: &[&ProcedureDeclaration],
    methods: &[ClassMethod<'_>],
    inits: &[ClassInit],
    name_outline_free_cells: &HashMap<String, Vec<String>>,
) -> Result<HashMap<String, ProcSignature>, CompileError> {
    let mut signatures = HashMap::new();
    for procedure in procedures {
        if procedure.name == "main" {
            return Err(spanned_error(
                "MIR lowering: a local procedure cannot be named 'main' (reserved for the program entry point)",
                0..0,
            ));
        }
        if signatures.contains_key(&procedure.name) {
            return Err(spanned_error(
                format!(
                    "MIR lowering: duplicate local procedure '{}'",
                    procedure.name
                ),
                0..0,
            ));
        }
        let free = name_outline_free_cells
            .get(&procedure.name)
            .cloned()
            .unwrap_or_default();
        signatures.insert(procedure.name.clone(), build_signature(procedure, free)?);
    }
    for method in methods {
        let mangled = mangle_method_name(method.class_name, &method.procedure.name);
        if signatures.contains_key(&mangled) {
            return Err(spanned_error(
                format!("MIR lowering: duplicate procedure/method name '{mangled}'"),
                0..0,
            ));
        }
        let mut signature = build_signature(method.procedure, Vec::new())?;
        for start in &mut signature.name_thunk_starts {
            *start += 1;
        }
        for index in &mut signature.formal_proc_param_indices {
            *index += 1;
        }
        for index in &mut signature.value_array_params {
            *index += 1;
        }
        for index in &mut signature.value_text_params {
            *index += 1;
        }
        signature.params.insert(0, MirType::ObjectRef);
        signatures.insert(mangled, signature);
    }
    for init in inits {
        let mangled = mangle_init_name(&init.class_name);
        if signatures.contains_key(&mangled) {
            return Err(spanned_error(
                format!("MIR lowering: duplicate procedure/method name '{mangled}'"),
                0..0,
            ));
        }
        let mut params = vec![MirType::ObjectRef];
        for (_, field_ty) in &init.constructor_params {
            params.push(mir_type_for_field(*field_ty));
        }
        signatures.insert(
            mangled,
            ProcSignature {
                params,
                result: None,
                result_object_qual: None,
                name_thunk_starts: Vec::new(),
                name_thunk_assigned: Vec::new(),
                value_array_params: Vec::new(),
                value_text_params: Vec::new(),
                free_cell_params: Vec::new(),
                formal_proc_param_indices: Vec::new(),
                external_stub: false,
            },
        );
    }
    Ok(signatures)
}

/// Validates and converts one procedure heading into a [`ProcSignature`].
/// Hard errors: `external` procedures (no body to lower) and unsupported
/// transmission modes. Call-by-name and formal-procedure procedures are
/// partitioned out before this runs and inlined at call sites. Call-by-reference
/// is allowed for integer/text array formals (descriptor pointer aliasing).
pub(in crate::mir::lower) fn build_signature(
    procedure: &ProcedureDeclaration,
    free_cell_params: Vec<String>,
) -> Result<ProcSignature, CompileError> {
    if procedure.is_external {
        // Corpus `external procedure` declarations without a linked body get
        // empty stub signatures so check/MIR can proceed (separate-compilation
        // FFI remains unsupported for real linking).
        let mut params = Vec::new();
        for param in &procedure.parameters {
            if param.is_procedure {
                continue;
            }
            params.push(outlined_param_mir_type(param).unwrap_or(MirType::I64));
        }
        let result = procedure
            .result_type
            .as_ref()
            .map(mir_type_for)
            .transpose()?;
        let external_stub = params.is_empty();
        return Ok(ProcSignature {
            params,
            result,
            result_object_qual: result_object_qual_of(&procedure.result_type),
            name_thunk_starts: Vec::new(),
            name_thunk_assigned: Vec::new(),
            value_array_params: Vec::new(),
            value_text_params: Vec::new(),
            free_cell_params: Vec::new(),
            formal_proc_param_indices: Vec::new(),
            // Unknown formals in `external procedure pa, pb` lists.
            external_stub,
        });
    }

    let mut params = Vec::new();
    let mut name_thunk_starts = Vec::new();
    let mut name_thunk_assigned = Vec::new();
    let mut value_array_params = Vec::new();
    let mut value_text_params = Vec::new();
    let mut formal_proc_param_indices = Vec::new();
    for param in &procedure.parameters {
        if param.is_procedure {
            // Outlined recursive formal-proc procedures (simtst34): fat
            // pointer `(func: FuncRef, env: RefI64)`. Non-recursive formal-proc
            // procedures stay call-site inlined and never reach here.
            formal_proc_param_indices.push(params.len());
            params.push(MirType::FuncRef);
            params.push(MirType::RefI64);
            continue;
        }
        if is_name_thunk_formal(param)? {
            name_thunk_starts.push(params.len());
            name_thunk_assigned.push(name_formal_is_assigned(procedure, &param.name));
            params.push(MirType::FuncRef);
            params.push(MirType::FuncRef);
            params.push(MirType::ObjectRef);
            continue;
        }
        let ty = outlined_param_mir_type(param)?;
        if let Err(reason) = outlined_param_allowed(param.mode, ty) {
            return Err(spanned_error(
                format!(
                    "MIR lowering: procedure '{}' parameter '{}': {reason}",
                    procedure.name, param.name
                ),
                param.span.clone(),
            ));
        }
        if param.mode == ParamMode::Value
            && matches!(
                ty,
                MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
            )
        {
            value_array_params.push(params.len());
        }
        if param.mode == ParamMode::Value && ty == MirType::Text {
            value_text_params.push(params.len());
        }
        params.push(ty);
    }

    for _ in &free_cell_params {
        params.push(MirType::RefI64);
    }

    let result = match &procedure.result_type {
        Some(ty) => Some(mir_type_for(ty)?),
        None => None,
    };

    Ok(ProcSignature {
        params,
        result,
        result_object_qual: result_object_qual_of(&procedure.result_type),
        name_thunk_starts,
        name_thunk_assigned,
        value_array_params,
        value_text_params,
        free_cell_params,
        formal_proc_param_indices,
        external_stub: false,
    })
}

/// Whether `formal` appears as the LHS of an assignment somewhere in `procedure`.
pub(in crate::mir::lower) fn name_formal_is_assigned(
    procedure: &ProcedureDeclaration,
    formal: &str,
) -> bool {
    block_assigns_name(&procedure.body, formal)
}

pub(in crate::mir::lower) fn block_assigns_name(block: &Block, name: &str) -> bool {
    statements_assign_name(&block.statements, name)
        || block
            .body
            .iter()
            .any(|inner| block_assigns_name(inner, name))
}

pub(in crate::mir::lower) fn statements_assign_name(statements: &[Statement], name: &str) -> bool {
    statements
        .iter()
        .any(|statement| statement_assigns_name(statement, name))
}

pub(in crate::mir::lower) fn statement_assigns_name(statement: &Statement, name: &str) -> bool {
    match &statement.kind {
        StatementKind::Assignment(assignment) => match &assignment.lhs {
            Variable::Simple(lhs) => lhs.eq_ignore_ascii_case(name),
            _ => false,
        },
        StatementKind::If(if_stmt) => {
            statement_assigns_name(&if_stmt.then_branch, name)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|s| statement_assigns_name(s, name))
        }
        StatementKind::While(while_stmt) => statement_assigns_name(&while_stmt.body, name),
        StatementKind::For(for_stmt) => statement_assigns_name(&for_stmt.body, name),
        StatementKind::Compound(block) => block_assigns_name(block, name),
        StatementKind::Labeled { statement, .. } => statement_assigns_name(statement, name),
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|when| statement_assigns_name(&when.body, name))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|s| statement_assigns_name(s, name))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|s| statement_assigns_name(s, name))
        }
        _ => false,
    }
}
