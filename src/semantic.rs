//! Semantic analysis for Simula programs.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::ast::{
    ArrayDeclaration, AssignOperator, Assignment, AssignmentRhs, BinaryOp, Block, ClassDeclaration,
    DesignationalExpr, Expr, ExprKind, ExternalDeclaration, ExternalProcedureDeclaration,
    FormalParameter, ParamMode, ProcedureDeclaration, Program, Specifier, Statement, StatementKind,
    UnaryOp, Variable,
};
use crate::concatenate::{
    AttributeMatch, concatenate_classes_with_externals, find_attribute_in_raw_class,
    find_innermost_attribute_match, find_innermost_procedure_match, find_remote_attribute_level,
    find_remote_attribute_match, find_remote_procedure_match, in_protection_hierarchy,
    is_fictitious_detach, is_hidden_from, prefix_chain, prefix_chain_ordered,
    ref_type_subordinates, substitute_remote_attribute, virtual_kind_matches_in_class,
};
use crate::diagnostics::ExpectRole;
use crate::environment::environment_constant_type;
use crate::environment::{builtin_result_type, is_environment_procedure};
use crate::error::{CompileError, CompileErrors};
use crate::mir::{ForeignAbi, ForeignKind};
use crate::runtime::fs::{filesystem_procedure_returns_value, is_filesystem_procedure};
use crate::text::{TextIntrinsic, is_text_frame_procedure};
use crate::types::{ArithmeticLiteralKind, Declaration, Type};

struct ModuleContext {
    seen_externals: HashSet<String>,
    non_simula_procedures: HashSet<String>,
    /// Formal parameter types keyed by procedure name (lowercase).
    procedure_formals: HashMap<String, Vec<Type>>,
    /// Indices of formal procedure parameters keyed by procedure name (lowercase).
    procedure_formal_proc_indices: HashMap<String, Vec<usize>>,
}

struct TypeContext<'a> {
    scope: &'a HashMap<String, Type>,
    current_class: Option<&'a str>,
    concatenated_classes: &'a HashMap<String, ClassDeclaration>,
    raw_classes: &'a HashMap<String, ClassDeclaration>,
    labels: &'a HashSet<String>,
    switches: &'a HashSet<String>,
}

pub fn analyze(program: &Program) -> Result<(), CompileError> {
    analyze_all(program).map_err(CompileErrors::into_first)
}

/// Collect `identifier = "module-id"` pairs from external declarations (§6.3.8 / §6.5).
///
/// Metadata only — reserved for a future linker; not required for interpretation.
pub fn external_identifications(program: &Program) -> HashMap<String, String> {
    let mut ids = HashMap::new();
    collect_external_identifications(&program.external_head, &mut ids);
    for block in &program.blocks {
        collect_block_external_identifications(block, &mut ids);
    }
    ids
}

fn collect_block_external_identifications(block: &Block, ids: &mut HashMap<String, String>) {
    collect_external_identifications(&block.externals, ids);
    for nested in &block.body {
        collect_block_external_identifications(nested, ids);
    }
    for class in &block.classes {
        collect_block_external_identifications(&class.body, ids);
    }
    for procedure in &block.procedures {
        collect_block_external_identifications(&procedure.body, ids);
    }
}

fn collect_external_identifications(
    externals: &[ExternalDeclaration],
    ids: &mut HashMap<String, String>,
) {
    for external in externals {
        match external {
            ExternalDeclaration::Class(class) => {
                for item in &class.items {
                    if let Some(id) = &item.identification {
                        ids.insert(item.name.clone(), id.clone());
                    }
                }
            }
            ExternalDeclaration::Procedure(procedure) => {
                for item in &procedure.items {
                    if let Some(id) = &item.identification {
                        ids.insert(item.name.clone(), id.clone());
                    }
                }
            }
        }
    }
}

/// Analyzes `program`, collecting independent semantic errors instead of
/// stopping at the first failure.
pub fn analyze_all(program: &Program) -> Result<(), CompileErrors> {
    let mut scope = HashMap::new();
    let mut module = ModuleContext {
        seen_externals: HashSet::new(),
        non_simula_procedures: HashSet::new(),
        procedure_formals: HashMap::new(),
        procedure_formal_proc_indices: HashMap::new(),
    };
    let mut outer_raw_classes = HashMap::new();
    apply_external_declarations(&program.external_head, &mut scope, &mut module)
        .map_err(CompileErrors::from)?;
    inject_external_classes(&program.external_head, &mut scope, &mut outer_raw_classes);
    let mut external_prefix_names = external_class_names(&program.external_head);
    let mut outer_classes: HashMap<String, ClassDeclaration> = HashMap::new();

    let mut errors = Vec::new();
    for block in &program.blocks {
        if let Err(structural) = analyze_block(
            block,
            &scope,
            &HashSet::new(),
            &HashSet::new(),
            None,
            &outer_classes,
            &outer_raw_classes,
            false,
            false,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &external_prefix_names,
            &mut module,
            &mut errors,
        ) {
            errors.push(structural);
        }

        // Multi-file / multi-block units: classes declared in earlier top-level
        // blocks (e.g. a package file) are available to later blocks that use
        // `external class` + prefixed blocks.
        if !block.classes.is_empty() {
            let mut stubs = outer_raw_classes.clone();
            if block_needs_simulation_system_classes(block) {
                crate::simulation::inject_system_classes(&mut stubs);
            }
            crate::basicio::inject_system_classes(&mut stubs);
            let prefix_name = block.prefix.as_ref().and_then(resolve_block_prefix_class);
            inject_prefix_nested_classes(prefix_name.as_deref(), &mut stubs);
            match concatenate_classes_with_externals(&block.classes, &stubs) {
                Ok(merged) => {
                    for (name, class) in merged {
                        external_prefix_names.insert(name.to_ascii_lowercase());
                        outer_classes.insert(name.clone(), class);
                    }
                }
                Err(error) => {
                    // `analyze_block` already concatenates this block; keep a
                    // unique report when that pass returned early for another reason.
                    let duplicate = errors.iter().any(|existing| {
                        existing.report_code() == error.report_code()
                            && existing.message == error.message
                    });
                    if !duplicate {
                        errors.push(error);
                    }
                }
            }
            for class in &block.classes {
                outer_raw_classes.insert(class.name.clone(), class.clone());
                scope.insert(class.name.clone(), Type::ObjectRef(class.name.clone()));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(CompileErrors::new(errors))
    }
}

fn apply_external_declarations(
    externals: &[ExternalDeclaration],
    scope: &mut HashMap<String, Type>,
    module: &mut ModuleContext,
) -> Result<(), CompileError> {
    for external in externals {
        let key = external_declaration_key(external);
        if !module.seen_externals.insert(key) {
            continue;
        }
        match external {
            ExternalDeclaration::Class(class) => {
                for item in &class.items {
                    scope.insert(item.name.clone(), Type::ObjectRef(item.name.clone()));
                }
            }
            ExternalDeclaration::Procedure(procedure) => {
                apply_external_procedure_declaration(procedure, scope, module)?;
            }
        }
    }
    Ok(())
}

fn external_procedure_spec_body_is_empty(block: &crate::ast::Block) -> bool {
    block.prefix.is_none()
        && block.externals.is_empty()
        && block.declarations.is_empty()
        && block.arrays.is_empty()
        && block.switches.is_empty()
        && block.procedures.is_empty()
        && block.classes.is_empty()
        && block.statements.is_empty()
        && block.body.is_empty()
}

fn block_needs_simulation_system_classes(block: &crate::ast::Block) -> bool {
    crate::simulation::block_needs_system_classes(block)
}

fn apply_external_procedure_declaration(
    external: &ExternalProcedureDeclaration,
    scope: &mut HashMap<String, Type>,
    module: &mut ModuleContext,
) -> Result<(), CompileError> {
    if let Some(kind_name) = &external.kind {
        let Some(kind) = ForeignKind::parse(kind_name) else {
            return Err(crate::diagnostics::unknown_external_kind(
                kind_name,
                external.span.clone(),
            ));
        };
        for item in &external.items {
            module.non_simula_procedures.insert(item.name.clone());
        }
        let Some(specification) = &external.specification else {
            let name = external
                .items
                .first()
                .map(|item| item.name.as_str())
                .unwrap_or("?");
            return Err(crate::diagnostics::missing_external_spec(
                kind.as_str(),
                name,
                external.span.clone(),
            ));
        };
        if !external_procedure_spec_body_is_empty(&specification.body) {
            return Err(crate::diagnostics::external_body_not_empty(
                external.span.clone(),
            ));
        }
        let identification = external
            .items
            .first()
            .and_then(|item| item.identification.as_deref());
        ForeignAbi::from_spec(kind, identification, specification, external.span.clone())?;
        register_external_spec(specification, scope, module);
        return Ok(());
    }

    if let Some(specification) = &external.specification {
        if !external_procedure_spec_body_is_empty(&specification.body) {
            return Err(crate::diagnostics::external_body_not_empty(
                external.span.clone(),
            ));
        }
        register_external_spec(specification, scope, module);
        return Ok(());
    }

    let default_type = external
        .result_type
        .clone()
        .unwrap_or(Type::Integer { short: false });
    for item in &external.items {
        scope.insert(item.name.clone(), default_type.clone());
    }
    Ok(())
}

fn register_external_spec(
    specification: &ProcedureDeclaration,
    scope: &mut HashMap<String, Type>,
    module: &mut ModuleContext,
) {
    let result_type = specification
        .result_type
        .clone()
        .unwrap_or(Type::Integer { short: false });
    scope.insert(specification.name.clone(), result_type);
    module.procedure_formals.insert(
        specification.name.to_ascii_lowercase(),
        specification
            .parameters
            .iter()
            .map(|param| param.ty.clone())
            .collect(),
    );
    module.procedure_formal_proc_indices.insert(
        specification.name.to_ascii_lowercase(),
        specification
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(index, param)| param.is_procedure.then_some(index))
            .collect(),
    );
}

fn external_declaration_key(external: &ExternalDeclaration) -> String {
    format!("{external:?}")
}

fn register_external_procedure_shorthand(
    procedure: &ProcedureDeclaration,
    scope: &mut HashMap<String, Type>,
) {
    let result_type = procedure
        .result_type
        .clone()
        .unwrap_or(Type::Integer { short: false });
    scope.insert(procedure.name.clone(), result_type);
}

fn stub_external_class(name: &str) -> ClassDeclaration {
    ClassDeclaration {
        prefix: None,
        name: name.to_string(),
        parameters: Vec::new(),
        specifications: Vec::new(),
        virtual_part: Vec::new(),
        protection_part: Vec::new(),
        protection_map: std::collections::BTreeMap::new(),
        body: crate::ast::Block {
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
        },
        has_inner: false,
        inner_label: None,
        tail_statements: Vec::new(),
        identifier_substitutions: std::collections::BTreeMap::new(),
        span: 0..0,
    }
}

fn inject_external_classes(
    externals: &[ExternalDeclaration],
    scope: &mut HashMap<String, Type>,
    raw_classes: &mut HashMap<String, ClassDeclaration>,
) {
    for external in externals {
        let ExternalDeclaration::Class(class) = external else {
            continue;
        };
        for item in &class.items {
            scope.insert(item.name.clone(), Type::ObjectRef(item.name.clone()));
            raw_classes
                .entry(item.name.clone())
                .or_insert_with(|| stub_external_class(&item.name));
        }
    }
}

fn external_class_names(externals: &[ExternalDeclaration]) -> HashSet<String> {
    let mut names = HashSet::new();
    for external in externals {
        let ExternalDeclaration::Class(class) = external else {
            continue;
        };
        for item in &class.items {
            names.insert(item.name.to_ascii_lowercase());
        }
    }
    names
}

fn analyze_block(
    block: &Block,
    outer: &HashMap<String, Type>,
    outer_constants: &HashSet<String>,
    outer_switches: &HashSet<String>,
    current_class: Option<&str>,
    outer_classes: &HashMap<String, ClassDeclaration>,
    outer_raw_classes: &HashMap<String, ClassDeclaration>,
    allow_attribute_shadowing: bool,
    allow_variable_shadowing: bool,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    visible_labels: &HashSet<String>,
    formal_procedure_params: &HashSet<String>,
    own_formal_params: &HashSet<String>,
    external_prefix_names: &HashSet<String>,
    module: &mut ModuleContext,
    errors: &mut Vec<CompileError>,
) -> Result<(), CompileError> {
    // §5.6.3 dynamic aspect: identifiers refer to the instantiated block/object at
    // runtime; static analysis uses the enclosing declaration structure below.
    let mut scope = outer.clone();
    let mut constants = outer_constants.clone();
    let mut switches = outer_switches.clone();
    let mut block_declared: HashMap<String, bool> = HashMap::new();
    let mut block_spans: HashMap<String, crate::error::Span> = HashMap::new();
    let block_head_names = block_head_names(block);

    let mut external_stubs = outer_raw_classes.clone();
    inject_external_classes(&block.externals, &mut scope, &mut external_stubs);
    let mut block_external_prefixes = external_prefix_names.clone();
    block_external_prefixes.extend(external_class_names(&block.externals));
    if block_needs_simulation_system_classes(block) {
        crate::simulation::inject_system_classes(&mut external_stubs);
        for (name, class) in &external_stubs {
            if crate::simulation::is_process_class(name)
                || crate::simulation::is_simulation_class(name)
                || name.eq_ignore_ascii_case("head")
                || name.eq_ignore_ascii_case("link")
                || name.eq_ignore_ascii_case("linkage")
                || name.eq_ignore_ascii_case("simset")
            {
                scope.insert(name.clone(), Type::ObjectRef(class.name.clone()));
            }
        }
        // Simulation sequencing procedures are in scope inside Simulation blocks.
        if crate::simulation::block_is_simulation_prefixed(&block.prefix) {
            scope.insert("hold".into(), Type::Integer { short: false });
            scope.insert("passivate".into(), Type::Integer { short: false });
            scope.insert("cancel".into(), Type::Integer { short: false });
            scope.insert("wait".into(), Type::Integer { short: false });
            scope.insert("time".into(), Type::Real { long: true });
            scope.insert("current".into(), Type::ObjectRef("Process".into()));
            scope.insert("main".into(), Type::ObjectRef("Process".into()));
            scope.insert("nextev".into(), Type::ObjectRef("Process".into()));
        }
    }
    crate::basicio::inject_system_classes(&mut external_stubs);
    for (name, class) in &external_stubs {
        if crate::basicio::is_basicio_class(name) {
            scope.insert(name.clone(), Type::ObjectRef(class.name.clone()));
        }
    }

    // Prefixed block (§4.10.1 / §5.5.4.9): treat as connection-block-like protection
    // context for the prefix class. Resolved early so classes nested in the
    // prefix class's own body (e.g. `Point` inside `Geometry`) are available
    // for concatenation of classes declared directly in this block (e.g.
    // `Point Class Color_Point` inside a `Geometry`-prefixed block).
    let prefix_class_owned: Option<String> =
        block.prefix.as_ref().and_then(resolve_block_prefix_class);
    inject_prefix_nested_classes(prefix_class_owned.as_deref(), &mut external_stubs);

    let mut concatenated = outer_classes.clone();
    if !block.classes.is_empty() {
        if let Err(error) = check_prefix_locality(
            block,
            &block_external_prefixes,
            &external_stubs,
            current_class,
        ) {
            errors.push(error);
            return Ok(());
        }
        // Class concatenation failure leaves the scope inconsistent — abort this block.
        // Merge (not replace) so outer/previously-declared classes (e.g. a
        // prefixed block's own prefix class) stay visible alongside classes
        // concatenated for this block.
        concatenated.extend(concatenate_classes_with_externals(
            &block.classes,
            &external_stubs,
        )?);
    }

    if let Some(prefix) = &block.prefix
        && let Err(error) = check_block_prefix(prefix)
    {
        errors.push(error);
    }
    if let Some(prefix_name) = &prefix_class_owned {
        insert_connection_attributes(&mut scope, prefix_name, &concatenated);
        insert_unmatched_virtual_attributes(&mut scope, prefix_name, &concatenated);
    }
    let current_class = prefix_class_owned.as_deref().or(current_class);

    let mut raw_classes = external_stubs;
    for class in &block.classes {
        raw_classes.insert(class.name.clone(), class.clone());
    }

    let mut block_formal_params = restricted_formal_params.clone();
    for procedure in &block.procedures {
        for param in &procedure.parameters {
            block_formal_params.insert(param.name.clone());
        }
    }
    for class in &block.classes {
        if let Some(merged) = concatenated.get(&class.name) {
            for param in &merged.parameters {
                block_formal_params.insert(param.name.clone());
            }
        }
    }

    let mut block_class_attributes = restricted_class_attributes.clone();
    for class in &block.classes {
        if let Some(merged) = concatenated.get(&class.name) {
            collect_class_attribute_names(merged, &mut block_class_attributes);
        }
    }

    let local_labels = collect_labels_from_statements(&block.statements);
    let mut labels = visible_labels.clone();
    labels.extend(local_labels);

    // External declaration failures affect the whole block scope — abort.
    apply_external_declarations(&block.externals, &mut scope, module)?;

    // Register switches before class bodies so constructors may reference them
    // (§5.6.13 enclosing-block visibility). Two-pass: register all switch
    // names first, since a switch's element list may designate another
    // switch declared later in the same block.
    for switch in &block.switches {
        if let Err(error) = register_switch_declaration(switch, &scope, &mut switches) {
            errors.push(error);
        }
    }

    // Class / procedure *names* must be visible to later head items (`ref(C)`,
    // typed calls) before their bodies are analyzed. Bodies themselves need the
    // full block-head scope so enclosing locals type-check (§5.6.13).
    for class in &block.classes {
        scope.insert(class.name.clone(), Type::ObjectRef(class.name.clone()));
    }
    for procedure in &block.procedures {
        if procedure.is_external {
            register_external_procedure_shorthand(procedure, &mut scope);
            continue;
        }
        scope.insert(
            procedure.name.clone(),
            procedure
                .result_type
                .clone()
                .unwrap_or(Type::Integer { short: false }),
        );
    }

    for declaration in &block.declarations {
        if let Err(error) = analyze_declaration(
            declaration,
            &mut scope,
            &mut constants,
            &mut block_declared,
            &mut block_spans,
            allow_attribute_shadowing,
            allow_variable_shadowing,
            &concatenated,
            &raw_classes,
        ) {
            errors.push(error);
        }
    }

    for array in &block.arrays {
        if let Err(error) = analyze_array_declaration(
            array,
            &mut scope,
            &mut block_declared,
            &mut block_spans,
            &block_head_names,
            allow_attribute_shadowing,
            allow_variable_shadowing,
        ) {
            errors.push(error);
        }
    }

    // Switch elements may reference locals declared later in the same block
    // head (`SWITCH S := IF b THEN …` with `BOOLEAN b;` below) — analyze them
    // only after declarations are in scope.
    for switch in &block.switches {
        if let Err(error) = analyze_switch_declaration_elements(switch, &scope, &switches, &labels)
        {
            errors.push(error);
        }
    }

    for class in &block.classes {
        if let Err(error) = analyze_class_declaration(
            class,
            &concatenated,
            &raw_classes,
            &scope,
            &switches,
            &labels,
            &block_external_prefixes,
            module,
            errors,
        ) {
            errors.push(error);
        }
    }

    for procedure in &block.procedures {
        if own_formal_params.contains(&procedure.name)
            && !formal_procedure_params.contains(&procedure.name)
        {
            errors.push(crate::diagnostics::formal_redeclared(
                &procedure.name,
                Some(procedure.span.clone()),
            ));
            continue;
        }
        if procedure.is_external {
            continue;
        }
        if let Err(error) = analyze_procedure_declaration(
            procedure,
            &scope,
            &switches,
            current_class,
            &concatenated,
            &raw_classes,
            &labels,
            &block_external_prefixes,
            module,
            errors,
        ) {
            errors.push(error);
        }
    }

    for statement in &block.statements {
        match &statement.kind {
            StatementKind::Compound(inner) => {
                if let Err(structural) = analyze_block(
                    inner,
                    &scope,
                    &constants,
                    &switches,
                    current_class,
                    &concatenated,
                    &raw_classes,
                    false,
                    // Nested compound blocks are their own scope — locals may
                    // shadow enclosing names (§4.1.3).
                    true,
                    &block_formal_params,
                    &block_class_attributes,
                    &labels,
                    &HashSet::new(),
                    &HashSet::new(),
                    &block_external_prefixes,
                    module,
                    errors,
                ) {
                    errors.push(structural);
                }
            }
            _ => {
                if let Err(error) = analyze_statement(
                    statement,
                    &scope,
                    &constants,
                    &switches,
                    &labels,
                    current_class,
                    &concatenated,
                    &raw_classes,
                    &block_formal_params,
                    &block_class_attributes,
                    &block_external_prefixes,
                    module,
                ) {
                    errors.push(error);
                }
            }
        }
    }

    for inner in &block.body {
        if let Err(structural) = analyze_block(
            inner,
            &scope,
            &constants,
            &switches,
            current_class,
            &concatenated,
            &raw_classes,
            false,
            true,
            &block_formal_params,
            &block_class_attributes,
            &labels,
            &HashSet::new(),
            &HashSet::new(),
            &block_external_prefixes,
            module,
            errors,
        ) {
            errors.push(structural);
        }
    }

    Ok(())
}

fn block_head_names(block: &Block) -> HashSet<String> {
    let mut names = HashSet::new();
    for declaration in &block.declarations {
        for item in &declaration.items {
            names.insert(item.name.clone());
        }
    }
    for array in &block.arrays {
        for segment in &array.segments {
            for name in &segment.names {
                names.insert(name.clone());
            }
        }
    }
    names
}

fn collect_class_attribute_names(class: &ClassDeclaration, names: &mut HashSet<String>) {
    for spec in &class.specifications {
        for name in &spec.names {
            names.insert(name.clone());
        }
    }
    for declaration in &class.body.declarations {
        for item in &declaration.items {
            names.insert(item.name.clone());
        }
    }
    for array in &class.body.arrays {
        for segment in &array.segments {
            for name in &segment.names {
                names.insert(name.clone());
            }
        }
    }
    for procedure in &class.body.procedures {
        names.insert(procedure.name.clone());
    }
    for switch in &class.body.switches {
        names.insert(switch.name.clone());
    }
    for spec in &class.virtual_part {
        for name in &spec.names {
            if !crate::concatenate::is_fictitious_detach(name) {
                names.insert(name.clone());
            }
        }
    }
}

fn collect_labels_from_statements(statements: &[Statement]) -> HashSet<String> {
    let mut labels = HashSet::new();
    for statement in statements {
        collect_labels_from_statement(statement, &mut labels);
    }
    labels
}

fn collect_labels_from_statement(statement: &Statement, labels: &mut HashSet<String>) {
    match &statement.kind {
        StatementKind::Labeled { label, statement } => {
            labels.insert(label.clone());
            collect_labels_from_statement(statement, labels);
        }
        StatementKind::Compound(block) => {
            labels.extend(collect_labels_from_block(block));
        }
        StatementKind::If(if_stmt) => {
            collect_labels_from_statement(&if_stmt.then_branch, labels);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_labels_from_statement(else_branch, labels);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_labels_from_statement(&while_stmt.body, labels);
        }
        StatementKind::For(for_stmt) => {
            collect_labels_from_statement(&for_stmt.body, labels);
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_labels_from_statement(&when.body, labels);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_labels_from_statement(do_clause, labels);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_labels_from_statement(otherwise, labels);
            }
        }
        _ => {}
    }
}

fn collect_labels_from_block(block: &Block) -> HashSet<String> {
    let mut labels = collect_labels_from_statements(&block.statements);
    for inner in &block.body {
        labels.extend(collect_labels_from_block(inner));
    }
    labels
}

fn allows_shadowing(
    scope: &HashMap<String, Type>,
    block_declared: &HashMap<String, bool>,
    name: &str,
    allow_attribute_shadowing: bool,
    allow_variable_shadowing: bool,
) -> bool {
    if allow_attribute_shadowing || allow_variable_shadowing {
        return true;
    }
    !block_declared.contains_key(name) && !scope.contains_key(name)
}

fn analyze_array_declaration(
    array: &ArrayDeclaration,
    scope: &mut HashMap<String, Type>,
    block_declared: &mut HashMap<String, bool>,
    block_spans: &mut HashMap<String, crate::error::Span>,
    block_head_names: &HashSet<String>,
    allow_attribute_shadowing: bool,
    allow_variable_shadowing: bool,
) -> Result<(), CompileError> {
    for segment in &array.segments {
        if segment.bounds.is_empty() {
            return Err(crate::diagnostics::empty_array_bounds(array.span.clone()));
        }

        for bound in &segment.bounds {
            ensure_array_bound_expr(&bound.lower, scope, block_head_names)?;
            ensure_array_bound_expr(&bound.upper, scope, block_head_names)?;
            let bound_ctx = scope_type_context(scope);
            let lower_type = type_of_expr(&bound.lower, &bound_ctx)?;
            let upper_type = type_of_expr(&bound.upper, &bound_ctx)?;
            if !lower_type.is_arithmetic() || !upper_type.is_arithmetic() {
                return Err(crate::diagnostics::type_mismatch(
                    ExpectRole::ArrayBound,
                    if lower_type.is_arithmetic() {
                        &upper_type
                    } else {
                        &lower_type
                    },
                    &Type::Integer { short: false },
                    array.span.clone(),
                ));
            }
        }

        let array_type = Type::Array {
            element: Box::new(array.element_type.clone()),
            dims: segment.bounds.len(),
        };

        let mut seen_in_segment = HashSet::new();
        for name in &segment.names {
            if !seen_in_segment.insert(name.clone()) {
                return Err(crate::diagnostics::duplicate_declaration(
                    name,
                    array.span.clone(),
                    None,
                ));
            }
            if !allows_shadowing(
                scope,
                block_declared,
                name,
                allow_attribute_shadowing,
                allow_variable_shadowing,
            ) {
                return Err(crate::diagnostics::duplicate_declaration(
                    name,
                    array.span.clone(),
                    block_spans.get(name).cloned(),
                ));
            }
            block_declared.insert(name.clone(), false);
            block_spans.insert(name.clone(), array.span.clone());
            scope.insert(name.clone(), array_type.clone());
        }
    }

    Ok(())
}

fn ensure_array_bound_expr(
    expr: &Expr,
    scope: &HashMap<String, Type>,
    block_head_names: &HashSet<String>,
) -> Result<(), CompileError> {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => {
            if block_head_names.contains(name) {
                return Err(crate::diagnostics::array_bound_name(
                    format!(
                        "array bound may not reference `{name}` declared in the same block head"
                    ),
                    expr.span.clone(),
                ));
            }
            if !scope.contains_key(name) {
                let suggestion = crate::diagnostics::suggest_one(name, scope.keys());
                return Err(crate::diagnostics::unknown_name(
                    name,
                    expr.span.clone(),
                    suggestion.as_deref(),
                ));
            }
            Ok(())
        }
        ExprKind::Variable(
            Variable::Subscripted { .. }
            | Variable::Qua { .. }
            | Variable::Remote { .. }
            | Variable::RemoteCall { .. },
        ) => Err(crate::diagnostics::array_bound_name(
            "array bounds may only reference simple identifiers from outer scope",
            expr.span.clone(),
        )),
        ExprKind::Unary { operand, .. } => {
            ensure_array_bound_expr(operand, scope, block_head_names)
        }
        ExprKind::Binary { left, right, .. } => {
            ensure_array_bound_expr(left, scope, block_head_names)?;
            ensure_array_bound_expr(right, scope, block_head_names)
        }
        ExprKind::Relation { left, right, .. } => {
            ensure_array_bound_expr(left, scope, block_head_names)?;
            ensure_array_bound_expr(right, scope, block_head_names)
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            ensure_array_bound_expr(condition, scope, block_head_names)?;
            ensure_array_bound_expr(then_expr, scope, block_head_names)?;
            ensure_array_bound_expr(else_expr, scope, block_head_names)
        }
        ExprKind::Paren(inner) => ensure_array_bound_expr(inner, scope, block_head_names),
        ExprKind::FunctionCall { arguments, .. } => {
            for argument in arguments {
                ensure_array_bound_expr(argument, scope, block_head_names)?;
            }
            Ok(())
        }
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::New { .. }
        | ExprKind::This(_)
        | ExprKind::Qua { .. }
        | ExprKind::RemoteCall { .. }
        | ExprKind::RemoteAccess { .. } => Ok(()),
    }
}

/// §5.6.13: register a switch's name before analyzing any switch element
/// lists, since one switch's elements may designate another switch declared
/// later in the same block.
fn register_switch_declaration(
    switch: &crate::ast::SwitchDeclaration,
    scope: &HashMap<String, Type>,
    switches: &mut HashSet<String>,
) -> Result<(), CompileError> {
    if scope.contains_key(&switch.name) || switches.contains(&switch.name) {
        return Err(crate::diagnostics::duplicate_declaration(
            &switch.name,
            switch.span.clone(),
            None,
        ));
    }
    switches.insert(switch.name.clone());
    Ok(())
}

fn analyze_switch_declaration_elements(
    switch: &crate::ast::SwitchDeclaration,
    scope: &HashMap<String, Type>,
    switches: &HashSet<String>,
    visible_labels: &HashSet<String>,
) -> Result<(), CompileError> {
    if switch.elements.is_empty() {
        return Err(crate::diagnostics::empty_switch(
            &switch.name,
            switch.span.clone(),
        ));
    }

    for element in &switch.elements {
        analyze_designational_expr(
            element,
            scope,
            switches,
            visible_labels,
            Some(switch.span.clone()),
        )?;
    }

    Ok(())
}

fn analyze_procedure_declaration(
    procedure: &crate::ast::ProcedureDeclaration,
    outer: &HashMap<String, Type>,
    outer_switches: &HashSet<String>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    visible_labels: &HashSet<String>,
    external_prefix_names: &HashSet<String>,
    module: &mut ModuleContext,
    errors: &mut Vec<CompileError>,
) -> Result<(), CompileError> {
    let mut seen = HashSet::new();
    for param in &procedure.parameters {
        if !seen.insert(param.name.clone()) {
            return Err(crate::diagnostics::duplicate_formal(
                &param.name,
                procedure.span.clone(),
            ));
        }
        if param.name.eq_ignore_ascii_case(&procedure.name) {
            return Err(crate::diagnostics::procedure_name_as_formal(
                &procedure.name,
                procedure.span.clone(),
            ));
        }
    }

    // §4.6.6: infer formal array rank from body subscript uses so call sites
    // can reject mismatched actuals at analyze time.
    let formal_types = infer_formal_array_ranks(procedure)?;

    module
        .procedure_formals
        .insert(procedure.name.to_ascii_lowercase(), formal_types.clone());
    module.procedure_formal_proc_indices.insert(
        procedure.name.to_ascii_lowercase(),
        procedure
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(index, param)| param.is_procedure.then_some(index))
            .collect(),
    );

    let own_formal_params: HashSet<String> = procedure
        .parameters
        .iter()
        .map(|param| param.name.clone())
        .collect();
    let formal_procedure_params: HashSet<String> = procedure
        .parameters
        .iter()
        .filter(|param| param.is_procedure)
        .map(|param| param.name.clone())
        .collect();

    let mut labels = visible_labels.clone();
    let mut switches = outer_switches.clone();
    for param in &procedure.parameters {
        if param.is_label {
            labels.insert(param.name.clone());
        }
        if param.is_switch {
            switches.insert(param.name.clone());
        }
    }

    let mut scope = outer.clone();
    for (param, ty) in procedure.parameters.iter().zip(formal_types.iter()) {
        scope.insert(param.name.clone(), ty.clone());
    }
    if let Some(result_type) = &procedure.result_type {
        scope.insert(procedure.name.clone(), result_type.clone());
    }
    analyze_block(
        &procedure.body,
        &scope,
        &HashSet::new(),
        &switches,
        current_class,
        concatenated_classes,
        raw_classes,
        false,
        // Procedure body is a nested block — locals may shadow enclosing names (§4.1.3).
        true,
        &HashSet::new(),
        &HashSet::new(),
        // §5.6.13 / non-local goto: labels of the enclosing block are visible
        // inside a nested procedure body. LABEL formals are also designational
        // targets (§4.5 / §5.4.2).
        &labels,
        &formal_procedure_params,
        &own_formal_params,
        external_prefix_names,
        module,
        errors,
    )
}

/// §4.6.6: a formal array's dimension count is fixed by how it is subscripted
/// in the procedure body. Unused formals keep `dims: 0` (wildcard at call sites).
fn infer_formal_array_ranks(procedure: &ProcedureDeclaration) -> Result<Vec<Type>, CompileError> {
    let mut types: Vec<Type> = procedure.parameters.iter().map(|p| p.ty.clone()).collect();
    let mut name_to_index = HashMap::new();
    for (index, param) in procedure.parameters.iter().enumerate() {
        if matches!(param.ty, Type::Array { .. }) {
            name_to_index.insert(param.name.to_ascii_lowercase(), index);
        }
    }
    if name_to_index.is_empty() {
        return Ok(types);
    }

    let mut arities = HashMap::new();
    collect_formal_array_arities_block(&procedure.body, &name_to_index, &mut arities)?;
    for (index, arity) in arities {
        if let Type::Array { dims, .. } = &mut types[index] {
            *dims = arity;
        }
    }
    Ok(types)
}

fn note_formal_array_arity(
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
    name: &str,
    arity: usize,
) -> Result<(), CompileError> {
    let Some(&index) = name_to_index.get(&name.to_ascii_lowercase()) else {
        return Ok(());
    };
    match arities.insert(index, arity) {
        Some(previous) if previous != arity => Err(crate::diagnostics::formal_array_arity(
            name, previous, arity, None,
        )),
        _ => Ok(()),
    }
}

fn collect_formal_array_arities_block(
    block: &Block,
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
) -> Result<(), CompileError> {
    for procedure in &block.procedures {
        collect_formal_array_arities_block(&procedure.body, name_to_index, arities)?;
    }
    for class in &block.classes {
        collect_formal_array_arities_block(&class.body, name_to_index, arities)?;
    }
    for statement in &block.statements {
        collect_formal_array_arities_statement(statement, name_to_index, arities)?;
    }
    for inner in &block.body {
        collect_formal_array_arities_block(inner, name_to_index, arities)?;
    }
    Ok(())
}

fn collect_formal_array_arities_statement(
    statement: &Statement,
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
) -> Result<(), CompileError> {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => {
            collect_formal_array_arities_statement(statement, name_to_index, arities)
        }
        StatementKind::Compound(block) => {
            collect_formal_array_arities_block(block, name_to_index, arities)
        }
        StatementKind::If(if_stmt) => {
            collect_formal_array_arities_expr(&if_stmt.condition, name_to_index, arities)?;
            collect_formal_array_arities_statement(&if_stmt.then_branch, name_to_index, arities)?;
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_formal_array_arities_statement(else_branch, name_to_index, arities)?;
            }
            Ok(())
        }
        StatementKind::While(while_stmt) => {
            collect_formal_array_arities_expr(&while_stmt.condition, name_to_index, arities)?;
            collect_formal_array_arities_statement(&while_stmt.body, name_to_index, arities)
        }
        StatementKind::For(for_stmt) => {
            for element in &for_stmt.elements {
                match element {
                    crate::ast::ForListElement::Value { expr, while_cond }
                    | crate::ast::ForListElement::Reference { expr, while_cond } => {
                        collect_formal_array_arities_expr(expr, name_to_index, arities)?;
                        if let Some(cond) = while_cond {
                            collect_formal_array_arities_expr(cond, name_to_index, arities)?;
                        }
                    }
                    crate::ast::ForListElement::StepUntil { start, step, until } => {
                        collect_formal_array_arities_expr(start, name_to_index, arities)?;
                        collect_formal_array_arities_expr(step, name_to_index, arities)?;
                        collect_formal_array_arities_expr(until, name_to_index, arities)?;
                    }
                }
            }
            collect_formal_array_arities_statement(&for_stmt.body, name_to_index, arities)
        }
        StatementKind::Assignment(assignment) => {
            collect_formal_array_arities_variable(&assignment.lhs, name_to_index, arities)?;
            match &assignment.rhs {
                AssignmentRhs::Expr(expr) => {
                    collect_formal_array_arities_expr(expr, name_to_index, arities)
                }
                AssignmentRhs::Chain(inner) => {
                    collect_formal_array_arities_variable(&inner.lhs, name_to_index, arities)?;
                    match &inner.rhs {
                        AssignmentRhs::Expr(expr) => {
                            collect_formal_array_arities_expr(expr, name_to_index, arities)
                        }
                        AssignmentRhs::Chain(_) => {
                            // Deeper chains are rare; walk via a small loop.
                            let mut current = inner.as_ref();
                            loop {
                                collect_formal_array_arities_variable(
                                    &current.lhs,
                                    name_to_index,
                                    arities,
                                )?;
                                match &current.rhs {
                                    AssignmentRhs::Expr(expr) => {
                                        return collect_formal_array_arities_expr(
                                            expr,
                                            name_to_index,
                                            arities,
                                        );
                                    }
                                    AssignmentRhs::Chain(next) => current = next,
                                }
                            }
                        }
                    }
                }
            }
        }
        StatementKind::ProcedureCall(call) => {
            // `x(i)` as a statement is unusual; still record if `x` is a formal array.
            note_formal_array_arity(name_to_index, arities, &call.name, call.arguments.len())?;
            for argument in &call.arguments {
                collect_formal_array_arities_expr(argument, name_to_index, arities)?;
            }
            Ok(())
        }
        StatementKind::Expr(expr) => {
            collect_formal_array_arities_expr(expr, name_to_index, arities)
        }
        StatementKind::Goto(goto) => {
            collect_formal_array_arities_designational(&goto.target, name_to_index, arities)
        }
        StatementKind::ObjectGenerator(object_gen) => {
            for argument in &object_gen.arguments {
                collect_formal_array_arities_expr(argument, name_to_index, arities)?;
            }
            Ok(())
        }
        StatementKind::Activate(activate) => {
            collect_formal_array_arities_expr(&activate.target, name_to_index, arities)?;
            if let Some(timing) = &activate.timing {
                collect_formal_array_arities_timing(timing, name_to_index, arities)?;
            }
            Ok(())
        }
        StatementKind::Reactivate(reactivate) => {
            collect_formal_array_arities_expr(&reactivate.target, name_to_index, arities)?;
            if let Some(timing) = &reactivate.timing {
                collect_formal_array_arities_timing(timing, name_to_index, arities)?;
            }
            Ok(())
        }
        StatementKind::Inspect(inspect) => {
            collect_formal_array_arities_expr(&inspect.object, name_to_index, arities)?;
            for when in &inspect.when_clauses {
                collect_formal_array_arities_statement(&when.body, name_to_index, arities)?;
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_formal_array_arities_statement(do_clause, name_to_index, arities)?;
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_formal_array_arities_statement(otherwise, name_to_index, arities)?;
            }
            Ok(())
        }
        StatementKind::Dummy | StatementKind::Inner { .. } => Ok(()),
    }
}

fn collect_formal_array_arities_timing(
    timing: &crate::ast::SimulationTiming,
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
) -> Result<(), CompileError> {
    match timing {
        crate::ast::SimulationTiming::Delay(expr)
        | crate::ast::SimulationTiming::At(expr)
        | crate::ast::SimulationTiming::Before(expr)
        | crate::ast::SimulationTiming::After(expr) => {
            collect_formal_array_arities_expr(expr, name_to_index, arities)
        }
    }
}

fn collect_formal_array_arities_expr(
    expr: &Expr,
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
) -> Result<(), CompileError> {
    match &expr.kind {
        ExprKind::Variable(variable) => {
            collect_formal_array_arities_variable(variable, name_to_index, arities)
        }
        ExprKind::Unary { operand, .. } | ExprKind::Paren(operand) => {
            collect_formal_array_arities_expr(operand, name_to_index, arities)
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            collect_formal_array_arities_expr(left, name_to_index, arities)?;
            collect_formal_array_arities_expr(right, name_to_index, arities)
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_formal_array_arities_expr(condition, name_to_index, arities)?;
            collect_formal_array_arities_expr(then_expr, name_to_index, arities)?;
            collect_formal_array_arities_expr(else_expr, name_to_index, arities)
        }
        ExprKind::FunctionCall { name, arguments } => {
            note_formal_array_arity(name_to_index, arities, name, arguments.len())?;
            for argument in arguments {
                collect_formal_array_arities_expr(argument, name_to_index, arities)?;
            }
            Ok(())
        }
        ExprKind::RemoteAccess { object, .. } | ExprKind::Qua { object, .. } => {
            collect_formal_array_arities_expr(object, name_to_index, arities)
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            collect_formal_array_arities_expr(object, name_to_index, arities)?;
            for argument in arguments {
                collect_formal_array_arities_expr(argument, name_to_index, arities)?;
            }
            Ok(())
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    collect_formal_array_arities_expr(argument, name_to_index, arities)?;
                }
            }
            Ok(())
        }
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::This(_) => Ok(()),
    }
}

fn collect_formal_array_arities_designational(
    expr: &crate::ast::DesignationalExpr,
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
) -> Result<(), CompileError> {
    match expr {
        crate::ast::DesignationalExpr::Label(_) => Ok(()),
        crate::ast::DesignationalExpr::SwitchDesignator { subscript, .. } => {
            collect_formal_array_arities_expr(subscript, name_to_index, arities)
        }
        crate::ast::DesignationalExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_formal_array_arities_expr(condition, name_to_index, arities)?;
            collect_formal_array_arities_designational(then_expr, name_to_index, arities)?;
            collect_formal_array_arities_designational(else_expr, name_to_index, arities)
        }
        crate::ast::DesignationalExpr::Paren(inner) => {
            collect_formal_array_arities_designational(inner, name_to_index, arities)
        }
    }
}

fn collect_formal_array_arities_variable(
    variable: &Variable,
    name_to_index: &HashMap<String, usize>,
    arities: &mut HashMap<usize, usize>,
) -> Result<(), CompileError> {
    match variable {
        Variable::Simple(_) => Ok(()),
        Variable::Subscripted { name, subscripts } => {
            note_formal_array_arity(name_to_index, arities, name, subscripts.len())?;
            for subscript in subscripts {
                collect_formal_array_arities_expr(subscript, name_to_index, arities)?;
            }
            Ok(())
        }
        Variable::Qua { object, .. } => {
            collect_formal_array_arities_variable(object, name_to_index, arities)
        }
        Variable::Remote { object, .. } => {
            collect_formal_array_arities_variable(object, name_to_index, arities)
        }
        Variable::RemoteCall {
            object, arguments, ..
        } => {
            collect_formal_array_arities_variable(object, name_to_index, arities)?;
            for argument in arguments {
                collect_formal_array_arities_expr(argument, name_to_index, arities)?;
            }
            Ok(())
        }
    }
}

/// Validate class formal parameter transmission modes (Standard §5.5.5, fig. 5.4).
fn validate_class_parameter_modes(class: &ClassDeclaration) -> Result<(), CompileError> {
    for param in &class.parameters {
        if let Some(message) = illegal_class_param_mode(param) {
            return Err(crate::diagnostics::illegal_param_mode(
                &class.name,
                &param.name,
                message,
                class.span.clone(),
            ));
        }
    }
    Ok(())
}

fn illegal_class_param_mode(param: &FormalParameter) -> Option<&'static str> {
    match (&param.ty, param.mode) {
        (ty, ParamMode::Name) => {
            let _ = ty;
            Some("call-by-name is not permitted for class parameters")
        }
        (ty, ParamMode::Reference) if ty.is_value_type() => {
            Some("value-type parameters may not use call-by-reference transmission")
        }
        (Type::ObjectRef(_), ParamMode::Value) => {
            Some("object reference parameters may not use call-by-value transmission")
        }
        (Type::Array { element, .. }, ParamMode::Value) if element.is_reference_type() => {
            Some("reference-type array parameters may not use call-by-value transmission")
        }
        _ => None,
    }
}

fn analyze_class_declaration(
    class: &ClassDeclaration,
    concatenated: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    outer: &HashMap<String, Type>,
    outer_switches: &HashSet<String>,
    visible_labels: &HashSet<String>,
    external_prefix_names: &HashSet<String>,
    module: &mut ModuleContext,
    errors: &mut Vec<CompileError>,
) -> Result<(), CompileError> {
    let merged = concatenated
        .get(&class.name)
        .cloned()
        .unwrap_or_else(|| class.clone());

    let mut scope = outer.clone();
    for param in &merged.parameters {
        scope.insert(param.name.clone(), param.ty.clone());
    }
    insert_connection_attributes(&mut scope, &class.name, concatenated);
    insert_unmatched_virtuals_into_scope(&mut scope, &merged);

    let mut labels = visible_labels.clone();
    let mut switches = outer_switches.clone();
    insert_unmatched_virtual_labels_and_switches(&merged, &mut labels, &mut switches);

    validate_class_parameter_modes(&merged)?;

    analyze_block(
        &merged.body,
        &scope,
        &HashSet::new(),
        &switches,
        Some(&class.name),
        concatenated,
        raw_classes,
        true,
        false,
        &HashSet::new(),
        &HashSet::new(),
        &labels,
        &HashSet::new(),
        &HashSet::new(),
        external_prefix_names,
        module,
        errors,
    )?;

    for spec in &merged.virtual_part {
        for name in &spec.names {
            if let Some(matched) = find_innermost_attribute_match(&merged, name)
                && !virtual_kind_matches_in_class(&spec.specifier, &matched, concatenated)
            {
                return Err(crate::diagnostics::virtual_mismatch(
                    "quantity",
                    name,
                    &class.name,
                    Some(class.span.clone()),
                ));
            }
            if let Some(expected_heading) = &spec.procedure_heading {
                let Some(matched_proc) = find_innermost_procedure_match(&merged, name) else {
                    continue;
                };
                if !procedure_headings_match(expected_heading, &matched_proc) {
                    return Err(crate::diagnostics::virtual_mismatch(
                        "procedure",
                        name,
                        &class.name,
                        Some(class.span.clone()),
                    ));
                }
            }
        }
    }

    Ok(())
}

/// §5.5.1.5: a class may be used as prefix only where it is available —
/// local to the declaring block, declared `external` at this level, or a
/// system class. Module-level prefixes must appear in the external head (§6.1.5).
///
/// `known_classes` additionally allows any class already known from an outer
/// scope (outer raw classes / external stubs), found case-insensitively, as
/// well as classes nested in the block's own prefix class body (e.g. `Point`
/// declared inside `Geometry`, used as a prefix within a `Geometry`-prefixed
/// block).
fn check_prefix_locality(
    block: &Block,
    external_prefix_names: &HashSet<String>,
    known_classes: &HashMap<String, ClassDeclaration>,
    current_class: Option<&str>,
) -> Result<(), CompileError> {
    let local_names: HashSet<String> = block
        .classes
        .iter()
        .map(|class| class.name.to_ascii_lowercase())
        .collect();

    let block_prefix_class = block
        .prefix
        .as_ref()
        .and_then(resolve_block_prefix_class)
        .and_then(|name| {
            known_classes
                .get(&name)
                .or_else(|| find_known_class_ignore_case(known_classes, &name))
        });

    for class in &block.classes {
        let Some(prefix) = &class.prefix else {
            continue;
        };
        if crate::simulation::is_simset_family_class(prefix)
            || crate::basicio::is_basicio_class(prefix)
        {
            continue;
        }
        let prefix_key = prefix.to_ascii_lowercase();
        if local_names.contains(&prefix_key) || external_prefix_names.contains(&prefix_key) {
            continue;
        }
        // The enclosing class is in `known_classes` while analyzing its body, but
        // §5.5.1.5 still forbids `Outer class Bad` nested inside `Outer`.
        if current_class.is_some_and(|cc| cc.eq_ignore_ascii_case(prefix)) {
            return Err(crate::diagnostics::prefix_not_local(
                prefix,
                &class.name,
                class.span.clone(),
            ));
        }
        if find_known_class_ignore_case(known_classes, prefix).is_some() {
            continue;
        }
        if let Some(prefix_class) = block_prefix_class
            && prefix_class
                .body
                .classes
                .iter()
                .any(|nested| nested.name.eq_ignore_ascii_case(prefix))
        {
            continue;
        }
        return Err(crate::diagnostics::prefix_not_local(
            prefix,
            &class.name,
            class.span.clone(),
        ));
    }
    Ok(())
}

fn find_known_class_ignore_case<'a>(
    known_classes: &'a HashMap<String, ClassDeclaration>,
    name: &str,
) -> Option<&'a ClassDeclaration> {
    known_classes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, class)| class)
}

/// Resolve the class identifier of a block prefix (`C` or `C(args)`).
/// Flattens the nested class declarations of `prefix_name` (if known) into
/// `stubs` so that classes local to a prefix class's own body (e.g. `Point`
/// nested inside `Geometry`) are resolvable as prefixes for classes declared
/// directly in a block prefixed by that class (e.g. `Point Class Color_Point`
/// inside a `Geometry`-prefixed block).
fn inject_prefix_nested_classes(
    prefix_name: Option<&str>,
    stubs: &mut HashMap<String, ClassDeclaration>,
) {
    let Some(prefix_name) = prefix_name else {
        return;
    };
    let Some(prefix_class) = stubs
        .get(prefix_name)
        .or_else(|| find_known_class_ignore_case(stubs, prefix_name))
        .cloned()
    else {
        return;
    };
    for nested in &prefix_class.body.classes {
        stubs
            .entry(nested.name.clone())
            .or_insert_with(|| nested.clone());
    }
}

fn resolve_block_prefix_class(prefix: &Expr) -> Option<String> {
    match &prefix.kind {
        ExprKind::Variable(Variable::Simple(name)) => Some(name.clone()),
        ExprKind::FunctionCall { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// §5.5.1.6: `this class-identifier` is illegal as a block prefix.
fn check_block_prefix(prefix: &Expr) -> Result<(), CompileError> {
    fn contains_this(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::This(_) => true,
            ExprKind::Paren(inner) => contains_this(inner),
            ExprKind::Qua { object, .. } => contains_this(object),
            ExprKind::RemoteAccess { object, .. } => contains_this(object),
            ExprKind::FunctionCall { arguments, .. } => arguments.iter().any(contains_this),
            _ => false,
        }
    }
    if contains_this(prefix) {
        return Err(crate::diagnostics::illegal_this(prefix.span.clone()));
    }
    Ok(())
}

/// §5.5.3.6: matching procedure must have the same heading as the virtual `is` specification.
fn procedure_headings_match(
    expected: &ProcedureDeclaration,
    actual: &ProcedureDeclaration,
) -> bool {
    let result_ok = match (&expected.result_type, &actual.result_type) {
        (None, None) => true,
        (Some(a), Some(b)) => a.is_compatible_with(b),
        _ => false,
    };
    if !result_ok {
        return false;
    }
    if expected.parameters.len() != actual.parameters.len() {
        return false;
    }
    expected
        .parameters
        .iter()
        .zip(actual.parameters.iter())
        .all(|(expected_param, actual_param)| {
            expected_param.name.eq_ignore_ascii_case(&actual_param.name)
                && expected_param.ty.is_compatible_with(&actual_param.ty)
                && expected_param.mode == actual_param.mode
                && expected_param.is_procedure == actual_param.is_procedure
        })
}

fn analyze_declaration(
    declaration: &Declaration,
    scope: &mut HashMap<String, Type>,
    constants: &mut HashSet<String>,
    block_declared: &mut HashMap<String, bool>,
    block_spans: &mut HashMap<String, crate::error::Span>,
    allow_attribute_shadowing: bool,
    allow_variable_shadowing: bool,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Result<(), CompileError> {
    let mut seen_in_declaration = HashSet::new();
    for item in &declaration.items {
        if !seen_in_declaration.insert(item.name.clone()) {
            return Err(crate::diagnostics::duplicate_declaration(
                &item.name,
                declaration.span.clone(),
                None,
            ));
        }
        if !allows_shadowing(
            scope,
            block_declared,
            &item.name,
            allow_attribute_shadowing,
            allow_variable_shadowing,
        ) {
            return Err(crate::diagnostics::duplicate_declaration(
                &item.name,
                declaration.span.clone(),
                block_spans.get(&item.name).cloned(),
            ));
        }

        if item.is_constant {
            let Some(initializer) = &item.initializer else {
                return Err(crate::diagnostics::constant_initializer(
                    format!(
                        "constant `{}` requires an initializer expression",
                        item.name
                    ),
                    Some(declaration.span.clone()),
                ));
            };
            ensure_constant_initializer(initializer, scope, block_declared)?;
            let init_ctx = scope_type_context(scope);
            let init_type = type_of_expr_expecting(initializer, &init_ctx, Some(&declaration.ty))?;
            if !types_assignment_compatible(
                &declaration.ty,
                &init_type,
                concatenated_classes,
                raw_classes,
            ) {
                return Err(crate::diagnostics::type_mismatch_assign(
                    AssignOperator::Assign,
                    &init_type,
                    &declaration.ty,
                    declaration.span.clone(),
                    declaration.span.clone(),
                ));
            }
            constants.insert(item.name.clone());
        } else if let Some(initializer) = &item.initializer {
            let init_ctx = scope_type_context(scope);
            let init_type = type_of_expr_expecting(initializer, &init_ctx, Some(&declaration.ty))?;
            if !types_assignment_compatible(
                &declaration.ty,
                &init_type,
                concatenated_classes,
                raw_classes,
            ) {
                return Err(crate::diagnostics::type_mismatch_assign(
                    AssignOperator::Assign,
                    &init_type,
                    &declaration.ty,
                    initializer.span.clone(),
                    declaration.span.clone(),
                ));
            }
        }

        block_declared.insert(item.name.clone(), item.is_constant);
        block_spans.insert(item.name.clone(), declaration.span.clone());
        scope.insert(item.name.clone(), declaration.ty.clone());
    }

    Ok(())
}

fn ensure_constant_initializer(
    expr: &Expr,
    scope: &HashMap<String, Type>,
    block_declared: &HashMap<String, bool>,
) -> Result<(), CompileError> {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => {
            if let Some(&is_constant) = block_declared.get(name) {
                if !is_constant {
                    return Err(crate::diagnostics::constant_initializer(
                        format!(
                            "constant initializer may not reference variable `{name}` from the same block head"
                        ),
                        Some(expr.span.clone()),
                    ));
                }
            } else if !scope.contains_key(name) {
                let suggestion = crate::diagnostics::suggest_one(name, scope.keys());
                return Err(crate::diagnostics::unknown_name(
                    name,
                    expr.span.clone(),
                    suggestion.as_deref(),
                ));
            }
            Ok(())
        }
        ExprKind::Variable(
            Variable::Subscripted { .. }
            | Variable::Qua { .. }
            | Variable::Remote { .. }
            | Variable::RemoteCall { .. },
        ) => Err(crate::diagnostics::constant_initializer(
            "constant initializer may only reference simple identifiers",
            Some(expr.span.clone()),
        )),
        ExprKind::Unary { operand, .. } => {
            ensure_constant_initializer(operand, scope, block_declared)
        }
        ExprKind::Binary { left, right, .. } => {
            ensure_constant_initializer(left, scope, block_declared)?;
            ensure_constant_initializer(right, scope, block_declared)
        }
        ExprKind::Relation { left, right, .. } => {
            ensure_constant_initializer(left, scope, block_declared)?;
            ensure_constant_initializer(right, scope, block_declared)
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            ensure_constant_initializer(condition, scope, block_declared)?;
            ensure_constant_initializer(then_expr, scope, block_declared)?;
            ensure_constant_initializer(else_expr, scope, block_declared)
        }
        ExprKind::Paren(inner) => ensure_constant_initializer(inner, scope, block_declared),
        ExprKind::FunctionCall { arguments, .. } => {
            for argument in arguments {
                ensure_constant_initializer(argument, scope, block_declared)?;
            }
            Ok(())
        }
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::New { .. }
        | ExprKind::This(_)
        | ExprKind::Qua { .. }
        | ExprKind::RemoteCall { .. }
        | ExprKind::RemoteAccess { .. } => Ok(()),
    }
}

fn analyze_statement(
    statement: &Statement,
    scope: &HashMap<String, Type>,
    constants: &HashSet<String>,
    switches: &HashSet<String>,
    visible_labels: &HashSet<String>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    external_prefix_names: &HashSet<String>,
    module: &mut ModuleContext,
) -> Result<(), CompileError> {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => analyze_procedure_call(
            call,
            statement.span.clone(),
            scope,
            current_class,
            concatenated_classes,
            raw_classes,
            visible_labels,
            switches,
            module,
        ),
        StatementKind::Expr(expr) => analyze_expr(
            expr,
            scope,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            visible_labels,
            switches,
        ),
        StatementKind::Assignment(assignment) => analyze_assignment(
            assignment,
            statement.span.clone(),
            scope,
            constants,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        StatementKind::Labeled { statement, .. } => analyze_statement(
            statement,
            scope,
            constants,
            switches,
            visible_labels,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            external_prefix_names,
            module,
        ),
        StatementKind::If(if_stmt) => {
            let ctx = type_context(scope, current_class, concatenated_classes, raw_classes);
            ensure_boolean_role(&if_stmt.condition, &ctx, ExpectRole::IfCondition)?;
            analyze_statement(
                &if_stmt.then_branch,
                scope,
                constants,
                switches,
                visible_labels,
                current_class,
                concatenated_classes,
                raw_classes,
                restricted_formal_params,
                restricted_class_attributes,
                external_prefix_names,
                module,
            )?;
            if let Some(else_branch) = &if_stmt.else_branch {
                analyze_statement(
                    else_branch,
                    scope,
                    constants,
                    switches,
                    visible_labels,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    external_prefix_names,
                    module,
                )?;
            }
            Ok(())
        }
        StatementKind::While(while_stmt) => {
            let ctx = type_context(scope, current_class, concatenated_classes, raw_classes);
            ensure_boolean_role(&while_stmt.condition, &ctx, ExpectRole::WhileCondition)?;
            analyze_statement(
                &while_stmt.body,
                scope,
                constants,
                switches,
                visible_labels,
                current_class,
                concatenated_classes,
                raw_classes,
                restricted_formal_params,
                restricted_class_attributes,
                external_prefix_names,
                module,
            )
        }
        StatementKind::For(for_stmt) => analyze_for_statement(
            for_stmt,
            scope,
            constants,
            switches,
            visible_labels,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            external_prefix_names,
            module,
        ),
        StatementKind::Goto(goto_stmt) => analyze_designational_expr(
            &goto_stmt.target,
            scope,
            switches,
            visible_labels,
            Some(statement.span.clone()),
        ),
        StatementKind::Compound(block) => {
            let mut nested_errors = Vec::new();
            if let Err(structural) = analyze_block(
                block,
                scope,
                constants,
                switches,
                current_class,
                concatenated_classes,
                raw_classes,
                false,
                // Nested compound blocks are their own scope — locals may
                // shadow enclosing names (§4.1.3), including a `for`/`while`/
                // `if` statement body reached via this generic dispatch.
                true,
                restricted_formal_params,
                restricted_class_attributes,
                visible_labels,
                &HashSet::new(),
                &HashSet::new(),
                external_prefix_names,
                module,
                &mut nested_errors,
            ) {
                nested_errors.push(structural);
            }
            match nested_errors.len() {
                0 => Ok(()),
                1 => Err(nested_errors.remove(0)),
                _ => Err(CompileErrors::new(nested_errors).into_bundled()),
            }
        }
        StatementKind::Dummy => Ok(()),
        StatementKind::ObjectGenerator(generator) => {
            for arg in &generator.arguments {
                analyze_expr(
                    arg,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
            }
            Ok(())
        }
        StatementKind::Inspect(inspect) => analyze_inspect_statement(
            inspect,
            scope,
            constants,
            switches,
            visible_labels,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            external_prefix_names,
            module,
        ),
        StatementKind::Activate(activate) => {
            analyze_expr(
                &activate.target,
                scope,
                current_class,
                concatenated_classes,
                raw_classes,
                restricted_formal_params,
                restricted_class_attributes,
                visible_labels,
                switches,
            )?;
            if let Some(timing) = &activate.timing {
                let expr = match timing {
                    crate::ast::SimulationTiming::Delay(expr)
                    | crate::ast::SimulationTiming::After(expr)
                    | crate::ast::SimulationTiming::At(expr)
                    | crate::ast::SimulationTiming::Before(expr) => expr,
                };
                analyze_expr(
                    expr,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
            }
            Ok(())
        }
        StatementKind::Reactivate(reactivate) => {
            analyze_expr(
                &reactivate.target,
                scope,
                current_class,
                concatenated_classes,
                raw_classes,
                restricted_formal_params,
                restricted_class_attributes,
                visible_labels,
                switches,
            )?;
            if let Some(timing) = &reactivate.timing {
                let expr = match timing {
                    crate::ast::SimulationTiming::Delay(expr)
                    | crate::ast::SimulationTiming::After(expr)
                    | crate::ast::SimulationTiming::At(expr)
                    | crate::ast::SimulationTiming::Before(expr) => expr,
                };
                analyze_expr(
                    expr,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
            }
            Ok(())
        }
        StatementKind::Inner { .. } => Ok(()),
    }
}

fn type_context<'a>(
    scope: &'a HashMap<String, Type>,
    current_class: Option<&'a str>,
    concatenated_classes: &'a HashMap<String, ClassDeclaration>,
    raw_classes: &'a HashMap<String, ClassDeclaration>,
) -> TypeContext<'a> {
    TypeContext {
        scope,
        current_class,
        concatenated_classes,
        raw_classes,
        labels: empty_names(),
        switches: empty_names(),
    }
}

fn type_context_full<'a>(
    scope: &'a HashMap<String, Type>,
    current_class: Option<&'a str>,
    concatenated_classes: &'a HashMap<String, ClassDeclaration>,
    raw_classes: &'a HashMap<String, ClassDeclaration>,
    labels: &'a HashSet<String>,
    switches: &'a HashSet<String>,
) -> TypeContext<'a> {
    TypeContext {
        scope,
        current_class,
        concatenated_classes,
        raw_classes,
        labels,
        switches,
    }
}

fn empty_names() -> &'static HashSet<String> {
    static EMPTY: OnceLock<HashSet<String>> = OnceLock::new();
    EMPTY.get_or_init(HashSet::new)
}

fn empty_class_map() -> &'static HashMap<String, ClassDeclaration> {
    static EMPTY: OnceLock<HashMap<String, ClassDeclaration>> = OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

fn scope_type_context<'a>(scope: &'a HashMap<String, Type>) -> TypeContext<'a> {
    type_context(scope, None, empty_class_map(), empty_class_map())
}

fn analyze_inspect_statement(
    inspect: &crate::ast::InspectStatement,
    scope: &HashMap<String, Type>,
    constants: &HashSet<String>,
    switches: &HashSet<String>,
    visible_labels: &HashSet<String>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    external_prefix_names: &HashSet<String>,
    module: &mut ModuleContext,
) -> Result<(), CompileError> {
    analyze_expr(
        &inspect.object,
        scope,
        current_class,
        concatenated_classes,
        raw_classes,
        restricted_formal_params,
        restricted_class_attributes,
        visible_labels,
        switches,
    )?;
    for when in &inspect.when_clauses {
        // Connection-block-1: block qualification is the when-class (§4.8).
        let connection_scope =
            connection_block_scope(scope, &when.class_name, concatenated_classes);
        analyze_statement(
            &when.body,
            &connection_scope,
            constants,
            switches,
            visible_labels,
            Some(&when.class_name),
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            external_prefix_names,
            module,
        )?;
    }
    if let Some(do_clause) = &inspect.do_clause {
        // Connection-block-2: block qualification is the object expression's
        // qualification (§4.8).
        let (connection_scope, connection_class) =
            match expr_object_class_name(&inspect.object, scope)? {
                Some(class_name) if !class_name.eq_ignore_ascii_case("none") => {
                    let scoped = connection_block_scope(scope, &class_name, concatenated_classes);
                    (scoped, Some(class_name))
                }
                _ => (scope.clone(), None),
            };
        analyze_statement(
            do_clause,
            &connection_scope,
            constants,
            switches,
            visible_labels,
            connection_class.as_deref().or(current_class),
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            external_prefix_names,
            module,
        )?;
    }
    if let Some(otherwise) = &inspect.otherwise {
        // `otherwise` is not a connection block — no attribute injection.
        analyze_statement(
            otherwise,
            scope,
            constants,
            switches,
            visible_labels,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            external_prefix_names,
            module,
        )?;
    }
    Ok(())
}

/// Scope for a connection block: outer names plus attributes of the block
/// qualification class (excluding labels/switches per §4.8).
fn connection_block_scope(
    outer: &HashMap<String, Type>,
    class_name: &str,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
) -> HashMap<String, Type> {
    let mut scope = outer.clone();
    insert_connection_attributes(&mut scope, class_name, concatenated_classes);
    scope
}

fn lookup_concatenated_class<'a>(
    class_name: &str,
    concatenated_classes: &'a HashMap<String, ClassDeclaration>,
) -> Option<&'a ClassDeclaration> {
    concatenated_classes.get(class_name).or_else(|| {
        concatenated_classes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(class_name))
            .map(|(_, class)| class)
    })
}

/// Whether `class_name` (or a prefix along its chain) provides SIMSET Link/Head methods.
fn class_provides_simset_methods(
    class_name: &str,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    let mut current = Some(class_name.to_string());
    let mut guard = 0;
    while let Some(name) = current {
        if guard > 64 {
            break;
        }
        guard += 1;
        if crate::simulation::is_simset_family_class(&name) {
            return true;
        }
        current = lookup_concatenated_class(&name, concatenated_classes)
            .and_then(|class| class.prefix.clone());
    }
    false
}

/// Inject accessible attributes of `class_name` into `scope` for connection-block
/// analysis (§4.8 / §5.5.6.9). Label and switch attributes are excluded.
fn insert_connection_attributes(
    scope: &mut HashMap<String, Type>,
    class_name: &str,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
) {
    let Some(class) = lookup_concatenated_class(class_name, concatenated_classes) else {
        return;
    };
    for param in &class.parameters {
        scope.insert(param.name.clone(), param.ty.clone());
    }
    for spec in &class.specifications {
        match &spec.specifier {
            Specifier::Type(ty) | Specifier::TypeProcedure(ty) => {
                for name in &spec.names {
                    scope.insert(name.clone(), ty.clone());
                }
            }
            Specifier::TypeArray(ty) => {
                for name in &spec.names {
                    scope.insert(
                        name.clone(),
                        Type::Array {
                            element: Box::new(ty.clone()),
                            dims: 0,
                        },
                    );
                }
            }
            Specifier::Array => {
                for name in &spec.names {
                    scope.insert(
                        name.clone(),
                        Type::Array {
                            element: Box::new(Type::Integer { short: false }),
                            dims: 0,
                        },
                    );
                }
            }
            Specifier::Procedure => {
                for name in &spec.names {
                    scope.insert(name.clone(), Type::Integer { short: false });
                }
            }
            Specifier::Label | Specifier::Switch => {}
        }
    }
    for declaration in &class.body.declarations {
        for item in &declaration.items {
            scope.insert(item.name.clone(), declaration.ty.clone());
        }
    }
    for array in &class.body.arrays {
        for segment in &array.segments {
            let array_type = Type::Array {
                element: Box::new(array.element_type.clone()),
                dims: segment.bounds.len(),
            };
            for name in &segment.names {
                scope.insert(name.clone(), array_type.clone());
            }
        }
    }
    for procedure in &class.body.procedures {
        let result_type = procedure
            .result_type
            .clone()
            .unwrap_or(Type::Integer { short: false });
        scope.insert(procedure.name.clone(), result_type);
    }
    insert_unmatched_virtuals_into_scope(scope, class);
}

/// Inject unmatched virtual quantities into a scope (§5.6.7).
fn insert_unmatched_virtuals_into_scope(
    scope: &mut HashMap<String, Type>,
    class: &ClassDeclaration,
) {
    for spec in &class.virtual_part {
        for name in &spec.names {
            if crate::concatenate::is_fictitious_detach(name) {
                continue;
            }
            if scope.contains_key(name) {
                continue;
            }
            if find_innermost_attribute_match(class, name).is_some() {
                continue;
            }
            if let Some(ty) = virtual_specifier_type(&spec.specifier) {
                scope.insert(name.clone(), ty);
            }
        }
    }
}

/// Unmatched virtual labels/switches are valid `goto` / switch designator targets
/// inside the class body (§5.6.7); matching labels appear in subclasses or
/// prefixed-block additional main parts.
fn insert_unmatched_virtual_labels_and_switches(
    class: &ClassDeclaration,
    labels: &mut HashSet<String>,
    switches: &mut HashSet<String>,
) {
    for spec in &class.virtual_part {
        for name in &spec.names {
            if find_innermost_attribute_match(class, name).is_some() {
                continue;
            }
            match &spec.specifier {
                Specifier::Label => {
                    labels.insert(name.clone());
                }
                Specifier::Switch => {
                    switches.insert(name.clone());
                }
                _ => {}
            }
        }
    }
}

fn insert_unmatched_virtual_attributes(
    scope: &mut HashMap<String, Type>,
    class_name: &str,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
) {
    let Some(class) = lookup_concatenated_class(class_name, concatenated_classes) else {
        return;
    };
    insert_unmatched_virtuals_into_scope(scope, class);
}

fn virtual_specifier_type(specifier: &Specifier) -> Option<Type> {
    match specifier {
        Specifier::Type(ty) | Specifier::TypeProcedure(ty) => Some(ty.clone()),
        Specifier::TypeArray(ty) => Some(Type::Array {
            element: Box::new(ty.clone()),
            dims: 0,
        }),
        Specifier::Array => Some(Type::Array {
            element: Box::new(Type::Integer { short: false }),
            dims: 0,
        }),
        Specifier::Procedure | Specifier::Label | Specifier::Switch => {
            Some(Type::Integer { short: false })
        }
    }
}

fn analyze_for_statement(
    for_stmt: &crate::ast::ForStatement,
    scope: &HashMap<String, Type>,
    constants: &HashSet<String>,
    switches: &HashSet<String>,
    visible_labels: &HashSet<String>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    external_prefix_names: &HashSet<String>,
    module: &mut ModuleContext,
) -> Result<(), CompileError> {
    let ctx = type_context(scope, current_class, concatenated_classes, raw_classes);
    for element in &for_stmt.elements {
        match element {
            crate::ast::ForListElement::Value { expr, while_cond } => {
                analyze_expr(
                    expr,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
                if let Some(cond) = while_cond {
                    ensure_boolean_role(cond, &ctx, ExpectRole::WhileCondition)?;
                }
            }
            crate::ast::ForListElement::Reference { expr, while_cond } => {
                analyze_expr(
                    expr,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
                if let Some(cond) = while_cond {
                    ensure_boolean_role(cond, &ctx, ExpectRole::WhileCondition)?;
                }
            }
            crate::ast::ForListElement::StepUntil { start, step, until } => {
                analyze_expr(
                    start,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
                analyze_expr(
                    step,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
                analyze_expr(
                    until,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
            }
        }
    }

    analyze_statement(
        &for_stmt.body,
        scope,
        constants,
        switches,
        visible_labels,
        current_class,
        concatenated_classes,
        raw_classes,
        restricted_formal_params,
        restricted_class_attributes,
        external_prefix_names,
        module,
    )
}

fn analyze_designational_expr(
    expr: &DesignationalExpr,
    scope: &HashMap<String, Type>,
    switches: &HashSet<String>,
    visible_labels: &HashSet<String>,
    span: Option<crate::error::Span>,
) -> Result<(), CompileError> {
    match expr {
        DesignationalExpr::Label(label) => {
            if !visible_labels
                .iter()
                .any(|visible| visible.eq_ignore_ascii_case(label))
            {
                return Err(crate::diagnostics::undefined_label(label, span));
            }
            Ok(())
        }
        DesignationalExpr::SwitchDesignator { name, subscript } => {
            if !switches
                .iter()
                .any(|visible| visible.eq_ignore_ascii_case(name))
            {
                return Err(crate::diagnostics::undefined_switch(name, span));
            }
            let subscript_ctx = scope_type_context(scope);
            let subscript_type = type_of_expr(subscript, &subscript_ctx)?;
            if !matches!(subscript_type, Type::Integer { .. } | Type::Real { .. }) {
                return Err(crate::diagnostics::type_mismatch(
                    ExpectRole::SwitchSubscript,
                    &subscript_type,
                    &Type::Integer { short: false },
                    subscript.span.clone(),
                ));
            }
            analyze_expr(
                subscript,
                scope,
                None,
                empty_class_map(),
                empty_class_map(),
                &HashSet::new(),
                &HashSet::new(),
                visible_labels,
                switches,
            )
        }
        DesignationalExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            let ctx = scope_type_context(scope);
            ensure_boolean_role(condition, &ctx, ExpectRole::IfCondition)?;
            analyze_designational_expr(then_expr, scope, switches, visible_labels, span.clone())?;
            analyze_designational_expr(else_expr, scope, switches, visible_labels, span)
        }
        DesignationalExpr::Paren(inner) => {
            analyze_designational_expr(inner, scope, switches, visible_labels, span)
        }
    }
}

fn assignment_chain(assignment: &Assignment) -> Vec<&Assignment> {
    let mut chain = vec![assignment];
    let mut current = assignment;
    while let AssignmentRhs::Chain(inner) = &current.rhs {
        chain.push(inner.as_ref());
        current = inner.as_ref();
    }
    chain
}

fn analyze_assignment(
    assignment: &Assignment,
    statement_span: crate::error::Span,
    scope: &HashMap<String, Type>,
    constants: &HashSet<String>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
) -> Result<(), CompileError> {
    for link in assignment_chain(assignment) {
        ensure_not_constant_assignment(&link.lhs, constants, statement_span.clone())?;
        check_simple_variable_visibility(
            &link.lhs,
            scope,
            restricted_formal_params,
            restricted_class_attributes,
            statement_span.clone(),
        )?;
        check_variable_protection(
            &link.lhs,
            scope,
            current_class,
            concatenated_classes,
            Some(statement_span.clone()),
        )?;
        match &link.rhs {
            AssignmentRhs::Expr(expr) => analyze_assignment_to_expr(
                link,
                expr,
                statement_span.clone(),
                scope,
                current_class,
                concatenated_classes,
                raw_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?,
            AssignmentRhs::Chain(inner) => {
                ensure_not_constant_assignment(&inner.lhs, constants, statement_span.clone())?;
                check_simple_variable_visibility(
                    &inner.lhs,
                    scope,
                    restricted_formal_params,
                    restricted_class_attributes,
                    statement_span.clone(),
                )?;
                check_variable_protection(
                    &inner.lhs,
                    scope,
                    current_class,
                    concatenated_classes,
                    Some(statement_span.clone()),
                )?;
                analyze_assignment_to_destination(
                    link,
                    &inner.lhs,
                    statement_span.clone(),
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                )?
            }
        }
    }
    Ok(())
}

fn ensure_not_constant_assignment(
    variable: &Variable,
    constants: &HashSet<String>,
    span: crate::error::Span,
) -> Result<(), CompileError> {
    if let Variable::Simple(name) = variable
        && constants.contains(name)
    {
        return Err(crate::diagnostics::assign_to_constant(name, span));
    }
    Ok(())
}

fn analyze_assignment_to_expr(
    assignment: &Assignment,
    expr: &Expr,
    statement_span: crate::error::Span,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
) -> Result<(), CompileError> {
    let ctx = type_context(scope, current_class, concatenated_classes, raw_classes);
    let Some(lhs_type) = type_of_variable(&assignment.lhs, &ctx) else {
        analyze_expr(
            expr,
            scope,
            current_class,
            concatenated_classes,
            raw_classes,
            restricted_formal_params,
            restricted_class_attributes,
            empty_names(),
            empty_names(),
        )?;
        return ensure_declared_variable(&assignment.lhs, &ctx, statement_span);
    };
    analyze_expr_expecting(
        expr,
        scope,
        current_class,
        concatenated_classes,
        raw_classes,
        restricted_formal_params,
        restricted_class_attributes,
        empty_names(),
        empty_names(),
        Some(&lhs_type),
    )?;
    let rhs_type = type_of_expr_expecting(expr, &ctx, Some(&lhs_type))?;
    check_assignment_compatibility(
        assignment.operator,
        &lhs_type,
        &rhs_type,
        concatenated_classes,
        raw_classes,
        expr.span.clone(),
        statement_span,
    )
}

fn analyze_assignment_to_destination(
    assignment: &Assignment,
    destination: &Variable,
    statement_span: crate::error::Span,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
) -> Result<(), CompileError> {
    check_simple_variable_visibility(
        destination,
        scope,
        restricted_formal_params,
        restricted_class_attributes,
        statement_span.clone(),
    )?;
    let ctx = type_context(scope, current_class, concatenated_classes, raw_classes);
    let Some(lhs_type) = type_of_variable(&assignment.lhs, &ctx) else {
        return ensure_declared_variable(&assignment.lhs, &ctx, statement_span.clone());
    };
    let Some(rhs_type) = type_of_variable(destination, &ctx) else {
        return ensure_declared_variable(destination, &ctx, statement_span);
    };
    check_assignment_compatibility(
        assignment.operator,
        &lhs_type,
        &rhs_type,
        concatenated_classes,
        raw_classes,
        statement_span.clone(),
        statement_span,
    )
}

fn types_assignment_compatible(
    target: &Type,
    source: &Type,
    classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    match (target, source) {
        (Type::ObjectRef(_), Type::ObjectRef(_)) => {
            // Simula §3.6 reference assignment: legal in either "widening"
            // direction (assigning a more-specific ref to a more-general
            // variable) or "narrowing" (CIM-style downcast, checked at
            // runtime) as long as the two declared classes share a prefix
            // chain. Falls back to `raw_classes` (pre-concatenation stubs,
            // e.g. injected BASICIO/Simulation system classes) when a block
            // has no user class declarations of its own to concatenate.
            ref_type_subordinates(source, target, classes)
                || ref_type_subordinates(target, source, classes)
                || ref_type_subordinates(source, target, raw_classes)
                || ref_type_subordinates(target, source, raw_classes)
        }
        _ => target.accepts_assignment_from(source),
    }
}

fn closest_common_ref_prefix(
    found: &str,
    expected: &str,
    classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Option<String> {
    let found_chain = {
        let mut chain = prefix_chain_ordered(found, classes);
        if chain.len() <= 1 {
            chain = prefix_chain_ordered(found, raw_classes);
        }
        chain
    };
    let expected_set = {
        let mut set = prefix_chain(expected, classes);
        if set.len() <= 1 {
            set = prefix_chain(expected, raw_classes);
        }
        set
    };
    found_chain.into_iter().rev().find(|name| {
        expected_set
            .iter()
            .any(|other| other.eq_ignore_ascii_case(name))
    })
}

fn ref_assignment_note(
    found: &Type,
    expected: &Type,
    classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Option<String> {
    let (Type::ObjectRef(found_q), Type::ObjectRef(expected_q)) = (found, expected) else {
        return None;
    };
    let common = closest_common_ref_prefix(found_q, expected_q, classes, raw_classes);
    crate::diagnostics::ref_prefix_note(found, expected, common.as_deref())
}

fn check_assignment_compatibility(
    operator: AssignOperator,
    lhs_type: &Type,
    rhs_type: &Type,
    classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    primary_span: crate::error::Span,
    related_span: crate::error::Span,
) -> Result<(), CompileError> {
    match operator {
        AssignOperator::Assign => {
            if matches!(lhs_type, Type::ObjectRef(_)) {
                return Err(crate::diagnostics::value_assign_to_ref(primary_span));
            }
            if !types_assignment_compatible(lhs_type, rhs_type, classes, raw_classes) {
                let mut error = crate::diagnostics::type_mismatch_assign(
                    AssignOperator::Assign,
                    rhs_type,
                    lhs_type,
                    primary_span,
                    related_span,
                );
                if let Some(note) = ref_assignment_note(rhs_type, lhs_type, classes, raw_classes) {
                    error = error.with_note(note);
                }
                return Err(error);
            }
        }
        AssignOperator::AssignAlt => {
            if !lhs_type.is_reference() || !rhs_type.is_reference() {
                return Err(crate::diagnostics::ref_assign_to_value(primary_span));
            }
            if !types_assignment_compatible(lhs_type, rhs_type, classes, raw_classes) {
                let mut error = crate::diagnostics::type_mismatch_assign(
                    AssignOperator::AssignAlt,
                    rhs_type,
                    lhs_type,
                    primary_span,
                    related_span,
                );
                if let Some(note) = ref_assignment_note(rhs_type, lhs_type, classes, raw_classes) {
                    error = error.with_note(note);
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

fn analyze_procedure_call(
    call: &crate::ast::ProcedureCall,
    call_span: crate::error::Span,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    visible_labels: &HashSet<String>,
    switches: &HashSet<String>,
    module: &ModuleContext,
) -> Result<(), CompileError> {
    for argument in &call.arguments {
        analyze_expr(
            argument,
            scope,
            current_class,
            concatenated_classes,
            raw_classes,
            &HashSet::new(),
            &HashSet::new(),
            visible_labels,
            switches,
        )?;
    }

    if is_fictitious_detach(&call.name) {
        if current_class.is_some() {
            return Ok(());
        }
        return Err(crate::diagnostics::detach_needs_object(call_span));
    }

    match call.name.to_ascii_lowercase().as_str() {
        "outtext" => {
            if call.arguments.len() != 1 {
                return Err(crate::diagnostics::arity_mismatch(
                    "OutText",
                    1,
                    call.arguments.len(),
                    call_span,
                ));
            }

            ensure_text_role(
                &call.arguments[0],
                &type_context(scope, current_class, concatenated_classes, raw_classes),
                ExpectRole::CallArgument {
                    callee: "OutText".into(),
                    index: 0,
                    formal: Some("t".into()),
                },
            )?;
        }
        "outint" => {
            if call.arguments.len() != 2 {
                return Err(crate::diagnostics::arity_mismatch(
                    "OutInt",
                    2,
                    call.arguments.len(),
                    call_span,
                ));
            }
            let ctx = &type_context(scope, current_class, concatenated_classes, raw_classes);
            ensure_arithmetic_role(
                &call.arguments[0],
                ctx,
                ExpectRole::CallArgument {
                    callee: "OutInt".into(),
                    index: 0,
                    formal: Some("i".into()),
                },
            )?;
            ensure_integer_role(
                &call.arguments[1],
                ctx,
                ExpectRole::CallArgument {
                    callee: "OutInt".into(),
                    index: 1,
                    formal: Some("w".into()),
                },
            )?;
        }
        "outimage" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "OutImage",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "outchar" => {
            if call.arguments.len() != 1 {
                return Err(crate::diagnostics::arity_mismatch(
                    "OutChar",
                    1,
                    call.arguments.len(),
                    call_span,
                ));
            }
            ensure_character(
                &call.arguments[0],
                &type_context(scope, current_class, concatenated_classes, raw_classes),
            )?;
        }
        "breakoutimage" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "BreakOutImage",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "inimage" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "InImage",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "inchar" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "InChar",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "endfile" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "Endfile",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "terminate_program" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "terminate_program",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "sysin" | "sysout" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    &call.name,
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        "inline" => {
            if !call.arguments.is_empty() {
                return Err(crate::diagnostics::arity_mismatch(
                    "InLine",
                    0,
                    call.arguments.len(),
                    call_span,
                ));
            }
        }
        name if is_text_frame_procedure(name) => {
            analyze_text_frame_procedure_call(
                name,
                &call.arguments,
                &type_context(scope, current_class, concatenated_classes, raw_classes),
            )?;
        }
        name if is_environment_procedure(name) => {}
        name if is_filesystem_procedure(name) => {
            analyze_filesystem_procedure_call(
                name,
                &call.arguments,
                &type_context(scope, current_class, concatenated_classes, raw_classes),
            )?;
        }
        name if crate::basicio::free_basicio_target(name).is_some()
            && crate::basicio::is_basicio_method(name) =>
        {
            // §10 embedding: free SYSIN/SYSOUT attributes (eject, line, …).
        }
        name if crate::basicio::is_basicio_method(name)
            && current_class.is_some_and(crate::basicio::is_basicio_class) =>
        {
            // Connection / class body: bare BASICIO methods on `this` (e.g.
            // `inspect new DirectFile(...) do locate(1);`).
        }
        name if crate::simulation::is_simset_method(name)
            && current_class.is_some_and(|class| {
                class_provides_simset_methods(class, concatenated_classes)
            }) =>
        {
            // Link/Head/Process (and subclasses): bare `out`, `into`, …
        }
        name if crate::simulation::is_simulation_builtin(name) => {
            // Argument counts are checked lightly; presence in a Simulation
            // block is enforced at runtime.
            match name {
                "hold" | "cancel" | "wait" => {
                    if call.arguments.len() != 1 {
                        return Err(crate::diagnostics::arity_mismatch(
                            name,
                            1,
                            call.arguments.len(),
                            call_span,
                        ));
                    }
                }
                "passivate" | "time" | "current" if !call.arguments.is_empty() => {
                    return Err(crate::diagnostics::arity_mismatch(
                        name,
                        0,
                        call.arguments.len(),
                        call_span,
                    ));
                }
                _ => {}
            }
        }
        name => {
            if let Some(formals) = module.procedure_formals.get(name) {
                validate_actual_parameters(
                    name,
                    formals,
                    &call.arguments,
                    call_span.clone(),
                    &type_context_full(
                        scope,
                        current_class,
                        concatenated_classes,
                        raw_classes,
                        visible_labels,
                        switches,
                    ),
                )?;
                reject_non_simula_formal_actuals(name, module, &call.arguments)?;
                return Ok(());
            }
            if scope.keys().any(|key| key.eq_ignore_ascii_case(name)) {
                return Ok(());
            }
            let suggestion =
                crate::diagnostics::suggest_one(name, procedure_name_candidates(module, scope));
            return Err(crate::diagnostics::unknown_procedure(
                name,
                call_span,
                suggestion.as_deref(),
            ));
        }
    }

    Ok(())
}

fn procedure_name_candidates(module: &ModuleContext, scope: &HashMap<String, Type>) -> Vec<String> {
    const WELL_KNOWN: &[&str] = &[
        "OutText",
        "OutInt",
        "OutImage",
        "OutChar",
        "InImage",
        "InChar",
        "InLine",
        "BreakOutImage",
        "hold",
        "passivate",
        "activate",
        "cancel",
        "wait",
    ];
    let mut names: Vec<String> = WELL_KNOWN.iter().map(|name| (*name).to_string()).collect();
    names.extend(
        crate::environment::environment_procedures()
            .iter()
            .map(|name| (*name).to_string()),
    );
    names.extend(module.procedure_formals.keys().cloned());
    names.extend(scope.keys().cloned());
    names
}

fn validate_actual_parameters(
    name: &str,
    formals: &[Type],
    arguments: &[Expr],
    call_span: crate::error::Span,
    ctx: &TypeContext<'_>,
) -> Result<(), CompileError> {
    if arguments.len() != formals.len() {
        return Err(crate::diagnostics::arity_mismatch(
            name,
            formals.len(),
            arguments.len(),
            call_span,
        ));
    }

    for (index, (formal, argument)) in formals.iter().zip(arguments).enumerate() {
        let actual = type_of_expr_expecting(argument, ctx, Some(formal))?;
        if !formal_accepts_actual(formal, &actual, ctx.concatenated_classes, ctx.raw_classes) {
            return Err(crate::diagnostics::type_mismatch(
                ExpectRole::CallArgument {
                    callee: name.to_string(),
                    index,
                    formal: None,
                },
                &actual,
                formal,
                argument.span.clone(),
            ));
        }
    }

    Ok(())
}

/// §6.3.5: a non-Simula (`kind`) procedure may not be an actual for a formal procedure.
fn reject_non_simula_formal_actuals(
    callee: &str,
    module: &ModuleContext,
    arguments: &[Expr],
) -> Result<(), CompileError> {
    let Some(indices) = module
        .procedure_formal_proc_indices
        .get(&callee.to_ascii_lowercase())
    else {
        return Ok(());
    };
    for &index in indices {
        let Some(argument) = arguments.get(index) else {
            continue;
        };
        let Some(actual_name) = procedure_actual_name(argument) else {
            continue;
        };
        if module
            .non_simula_procedures
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&actual_name))
        {
            return Err(crate::diagnostics::non_simula_formal_proc(
                &actual_name,
                argument.span.clone(),
            ));
        }
    }
    Ok(())
}

fn procedure_actual_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => Some(name.clone()),
        ExprKind::Paren(inner) => procedure_actual_name(inner),
        _ => None,
    }
}

/// Whether an actual argument type satisfies a formal parameter type (§4.6.1).
fn formal_accepts_actual(
    formal: &Type,
    actual: &Type,
    classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    match (formal, actual) {
        (
            Type::Array {
                element: formal_element,
                dims: formal_dims,
            },
            Type::Array {
                element: actual_element,
                dims: actual_dims,
            },
        ) => {
            // `dims == 0` means the formal was never subscripted in the body
            // (§4.6.6 restriction applies when uses fix the rank).
            (*formal_dims == 0 || *formal_dims == *actual_dims)
                && types_assignment_compatible(formal_element, actual_element, classes, raw_classes)
        }
        _ => types_assignment_compatible(formal, actual, classes, raw_classes),
    }
}

fn check_simple_variable_visibility(
    variable: &Variable,
    scope: &HashMap<String, Type>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    span: crate::error::Span,
) -> Result<(), CompileError> {
    if let Variable::Simple(name) = variable {
        if scope.contains_key(name) {
            return Ok(());
        }
        if restricted_formal_params.contains(name) {
            return Err(crate::diagnostics::formal_not_visible(name, span));
        }
        if restricted_class_attributes.contains(name) {
            return Err(crate::diagnostics::attribute_not_visible(name, span));
        }
    }
    Ok(())
}

fn is_label_or_switch(name: &str, ctx: &TypeContext<'_>) -> bool {
    ctx.labels
        .iter()
        .any(|visible| visible.eq_ignore_ascii_case(name))
        || ctx
            .switches
            .iter()
            .any(|visible| visible.eq_ignore_ascii_case(name))
}

fn is_implicit_simple_name(name: &str, ctx: &TypeContext<'_>) -> bool {
    if name.eq_ignore_ascii_case("InLine")
        || name.eq_ignore_ascii_case("InChar")
        || name.eq_ignore_ascii_case("Endfile")
        || name.eq_ignore_ascii_case("sysin")
        || name.eq_ignore_ascii_case("sysout")
        || name.eq_ignore_ascii_case("CURRENTLOWTEN")
        || name.eq_ignore_ascii_case("CURRENTDECIMALMARK")
    {
        return true;
    }
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "datetime" | "cputime" | "clocktime" | "sourceline"
    ) && crate::environment::builtin_result_type(name).is_some()
    {
        return true;
    }
    if crate::basicio::basicio_free_result_type(name).is_some() {
        return true;
    }
    if crate::basicio::is_basicio_method(name)
        && ctx
            .current_class
            .is_some_and(crate::basicio::is_basicio_class)
    {
        return true;
    }
    crate::simulation::is_simset_method(name)
        && ctx
            .current_class
            .is_some_and(|class| class_provides_simset_methods(class, ctx.concatenated_classes))
}

fn unknown_simple(
    name: &str,
    span: crate::error::Span,
    scope: &HashMap<String, Type>,
) -> CompileError {
    let suggestion = crate::diagnostics::suggest_one(name, scope.keys());
    crate::diagnostics::unknown_name(name, span, suggestion.as_deref())
}

fn ensure_declared_variable(
    variable: &Variable,
    ctx: &TypeContext<'_>,
    span: crate::error::Span,
) -> Result<(), CompileError> {
    match variable {
        Variable::Simple(name) => {
            if type_of_variable(variable, ctx).is_some()
                || is_implicit_simple_name(name, ctx)
                || is_label_or_switch(name, ctx)
            {
                Ok(())
            } else {
                Err(unknown_simple(name, span, ctx.scope))
            }
        }
        Variable::Subscripted { name, .. } => {
            if scope_get_ignore_case(ctx.scope, name).is_some() {
                Ok(())
            } else {
                Err(unknown_simple(name, span, ctx.scope))
            }
        }
        _ => Ok(()),
    }
}

fn analyze_expr(
    expr: &Expr,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    visible_labels: &HashSet<String>,
    switches: &HashSet<String>,
) -> Result<(), CompileError> {
    analyze_expr_expecting(
        expr,
        scope,
        current_class,
        concatenated_classes,
        raw_classes,
        restricted_formal_params,
        restricted_class_attributes,
        visible_labels,
        switches,
        None,
    )
}

fn analyze_expr_expecting(
    expr: &Expr,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    raw_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
    visible_labels: &HashSet<String>,
    switches: &HashSet<String>,
    expected: Option<&Type>,
) -> Result<(), CompileError> {
    let ctx = type_context_full(
        scope,
        current_class,
        concatenated_classes,
        raw_classes,
        visible_labels,
        switches,
    );
    match &expr.kind {
        ExprKind::Variable(variable) => {
            check_simple_variable_visibility(
                variable,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
                expr.span.clone(),
            )?;
            check_variable_protection(
                variable,
                scope,
                current_class,
                concatenated_classes,
                Some(expr.span.clone()),
            )?;
            validate_remote_access(variable, &ctx)?;
            ensure_declared_variable(variable, &ctx, expr.span.clone())?;
            Ok(())
        }
        ExprKind::This(class_name) => {
            if let Some(current) = current_class {
                let chain = prefix_chain(current, concatenated_classes);
                if !chain.contains(class_name) {
                    return Err(crate::diagnostics::not_prefix_class(
                        class_name,
                        current,
                        expr.span.clone(),
                    ));
                }
            }
            Ok(())
        }
        ExprKind::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            validate_remote_procedure(object, attribute, &ctx)?;
            for argument in arguments {
                analyze_expr(
                    argument,
                    scope,
                    current_class,
                    concatenated_classes,
                    raw_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                    visible_labels,
                    switches,
                )?;
            }
            let _ = type_of_expr(expr, &ctx)?;
            walk_expr_for_protection(
                expr,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                expr,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::RemoteAccess { object, attribute } => {
            validate_remote_access_expr(object, attribute, &ctx)?;
            let _ = type_of_expr(expr, &ctx)?;
            walk_expr_for_protection(
                expr,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                expr,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        _ => {
            let _ = type_of_expr_expecting(expr, &ctx, expected)?;
            walk_expr_for_protection(
                expr,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                expr,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            Ok(())
        }
    }
}

fn validate_remote_access(variable: &Variable, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    let Variable::Remote { object, attribute } = variable else {
        return Ok(());
    };
    if is_fictitious_detach(attribute) {
        return Ok(());
    }
    if is_text_expression(object, ctx.scope) {
        if TextIntrinsic::parse(attribute).is_some() {
            return Ok(());
        }
        return Err(crate::diagnostics::unknown_attribute(
            "text",
            attribute,
            "attribute",
            None,
            None,
        ));
    }
    let Some(object_class) = object_class_name(object, ctx.scope)? else {
        return Ok(());
    };
    if crate::simulation::is_simset_method(attribute) {
        return Ok(());
    }
    if crate::basicio::is_basicio_method(attribute) {
        return Ok(());
    }
    if find_remote_attribute_match(&object_class, attribute, ctx.current_class, ctx.raw_classes)
        .is_none()
    {
        return Err(crate::diagnostics::unknown_attribute(
            &object_class,
            attribute,
            "attribute",
            None,
            None,
        ));
    }
    Ok(())
}

fn validate_remote_procedure(
    object: &Expr,
    attribute: &str,
    ctx: &TypeContext<'_>,
) -> Result<(), CompileError> {
    if is_fictitious_detach(attribute) {
        return Ok(());
    }
    if crate::simulation::is_simset_method(attribute) {
        return Ok(());
    }
    if crate::basicio::is_basicio_method(attribute) {
        return Ok(());
    }
    if attribute.eq_ignore_ascii_case("idle") || attribute.eq_ignore_ascii_case("terminated") {
        return Ok(());
    }
    if attribute.eq_ignore_ascii_case("evtime") {
        return Ok(());
    }
    if is_text_expression_expr(object, ctx.scope) {
        if TextIntrinsic::parse(attribute).is_some() {
            return Ok(());
        }
        return Err(crate::diagnostics::unknown_attribute(
            "text",
            attribute,
            "procedure",
            None,
            None,
        ));
    }
    let Some(object_class) = expr_object_class_name(object, ctx.scope)? else {
        return Ok(());
    };
    // `obj.arr(i)` is array indexing, not a procedure call.
    if let Some(AttributeMatch::Variable(Type::Array { .. })) =
        find_remote_attribute_match(&object_class, attribute, ctx.current_class, ctx.raw_classes)
    {
        return Ok(());
    }
    if find_remote_procedure_match(&object_class, attribute, ctx.current_class, ctx.raw_classes)
        .is_some()
    {
        return Ok(());
    }
    // Unmatched virtual procedures remain callable through the prefix class
    // qualification (§5.6.7 / §5.5.3); matching bodies live in subclasses.
    if matches!(
        find_remote_attribute_match(&object_class, attribute, ctx.current_class, ctx.raw_classes,),
        Some(AttributeMatch::Procedure)
    ) || is_virtual_procedure_quantity(&object_class, attribute, ctx.raw_classes)
    {
        return Ok(());
    }
    Err(crate::diagnostics::unknown_attribute(
        &object_class,
        attribute,
        "procedure",
        None,
        None,
    ))
}

/// Whether `attribute` is a virtual procedure quantity on `object_class` (or a prefix).
fn is_virtual_procedure_quantity(
    object_class: &str,
    attribute: &str,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    let Some((_, matched)) =
        find_remote_attribute_level(object_class, attribute, None, raw_classes)
    else {
        return false;
    };
    match matched {
        AttributeMatch::Procedure => true,
        AttributeMatch::Variable(_) => {
            // Typed procedure virtuals (`text procedure Q`) surface as Variable.
            let chain = prefix_chain_ordered(object_class, raw_classes);
            for class_name in chain {
                let Some(class) = raw_classes.get(&class_name).or_else(|| {
                    raw_classes
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&class_name))
                        .map(|(_, c)| c)
                }) else {
                    continue;
                };
                for spec in &class.virtual_part {
                    if !spec
                        .names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(attribute))
                    {
                        continue;
                    }
                    if matches!(
                        spec.specifier,
                        Specifier::Procedure | Specifier::TypeProcedure(_)
                    ) {
                        return true;
                    }
                }
            }
            false
        }
    }
}

fn validate_remote_access_expr(
    object: &Expr,
    attribute: &str,
    ctx: &TypeContext<'_>,
) -> Result<(), CompileError> {
    if is_fictitious_detach(attribute) {
        return Ok(());
    }
    if is_text_expression_expr(object, ctx.scope) {
        if TextIntrinsic::parse(attribute).is_some() {
            return Ok(());
        }
        return Err(crate::diagnostics::unknown_attribute(
            "text",
            attribute,
            "attribute",
            None,
            None,
        ));
    }
    let Some(object_class) = expr_object_class_name(object, ctx.scope)? else {
        return Ok(());
    };
    if crate::simulation::is_simset_method(attribute) {
        return Ok(());
    }
    if crate::basicio::is_basicio_method(attribute) {
        return Ok(());
    }
    if attribute.eq_ignore_ascii_case("idle")
        || attribute.eq_ignore_ascii_case("terminated")
        || attribute.eq_ignore_ascii_case("evtime")
    {
        return Ok(());
    }
    if find_remote_attribute_match(&object_class, attribute, ctx.current_class, ctx.raw_classes)
        .is_none()
    {
        return Err(crate::diagnostics::unknown_attribute(
            &object_class,
            attribute,
            "attribute",
            None,
            None,
        ));
    }
    Ok(())
}

fn walk_expr_for_visibility(
    expr: &Expr,
    scope: &HashMap<String, Type>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
) -> Result<(), CompileError> {
    match &expr.kind {
        ExprKind::Variable(variable) => check_simple_variable_visibility(
            variable,
            scope,
            restricted_formal_params,
            restricted_class_attributes,
            expr.span.clone(),
        ),
        ExprKind::Unary { operand, .. } => walk_expr_for_visibility(
            operand,
            scope,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::Binary { left, right, .. } => {
            walk_expr_for_visibility(
                left,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                right,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::Relation { left, right, .. } => {
            walk_expr_for_visibility(
                left,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                right,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr_for_visibility(
                condition,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                then_expr,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_visibility(
                else_expr,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::Paren(inner) => walk_expr_for_visibility(
            inner,
            scope,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::FunctionCall { arguments, .. } => {
            for argument in arguments {
                walk_expr_for_visibility(
                    argument,
                    scope,
                    restricted_formal_params,
                    restricted_class_attributes,
                )?;
            }
            Ok(())
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            walk_expr_for_visibility(
                object,
                scope,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            for argument in arguments {
                walk_expr_for_visibility(
                    argument,
                    scope,
                    restricted_formal_params,
                    restricted_class_attributes,
                )?;
            }
            Ok(())
        }
        ExprKind::RemoteAccess { object, .. } => walk_expr_for_visibility(
            object,
            scope,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    walk_expr_for_visibility(
                        argument,
                        scope,
                        restricted_formal_params,
                        restricted_class_attributes,
                    )?;
                }
            }
            Ok(())
        }
        ExprKind::Qua { object, .. } => walk_expr_for_visibility(
            object,
            scope,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::This(_) => Ok(()),
    }
}

#[allow(clippy::only_used_in_recursion)] // visibility checked via walk_expr_for_visibility
fn walk_expr_for_protection(
    expr: &Expr,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    restricted_formal_params: &HashSet<String>,
    restricted_class_attributes: &HashSet<String>,
) -> Result<(), CompileError> {
    match &expr.kind {
        ExprKind::Variable(variable) => check_variable_protection(
            variable,
            scope,
            current_class,
            concatenated_classes,
            Some(expr.span.clone()),
        ),
        ExprKind::Unary { operand, .. } => walk_expr_for_protection(
            operand,
            scope,
            current_class,
            concatenated_classes,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::Binary { left, right, .. } => {
            walk_expr_for_protection(
                left,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_protection(
                right,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::Relation { left, right, .. } => {
            walk_expr_for_protection(
                left,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_protection(
                right,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr_for_protection(
                condition,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_protection(
                then_expr,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            walk_expr_for_protection(
                else_expr,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )
        }
        ExprKind::Paren(inner) => walk_expr_for_protection(
            inner,
            scope,
            current_class,
            concatenated_classes,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::FunctionCall { arguments, .. } => {
            for argument in arguments {
                walk_expr_for_protection(
                    argument,
                    scope,
                    current_class,
                    concatenated_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                )?;
            }
            Ok(())
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            walk_expr_for_protection(
                object,
                scope,
                current_class,
                concatenated_classes,
                restricted_formal_params,
                restricted_class_attributes,
            )?;
            for argument in arguments {
                walk_expr_for_protection(
                    argument,
                    scope,
                    current_class,
                    concatenated_classes,
                    restricted_formal_params,
                    restricted_class_attributes,
                )?;
            }
            Ok(())
        }
        ExprKind::RemoteAccess { object, .. } => walk_expr_for_protection(
            object,
            scope,
            current_class,
            concatenated_classes,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    walk_expr_for_protection(
                        argument,
                        scope,
                        current_class,
                        concatenated_classes,
                        restricted_formal_params,
                        restricted_class_attributes,
                    )?;
                }
            }
            Ok(())
        }
        ExprKind::Qua { object, .. } => walk_expr_for_protection(
            object,
            scope,
            current_class,
            concatenated_classes,
            restricted_formal_params,
            restricted_class_attributes,
        ),
        ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None
        | ExprKind::This(_) => Ok(()),
    }
}

/// §5.5.6.5 access-level resolution: if the innermost matching attribute at
/// `class_name` is protected and inaccessible from `current_class`, continue
/// searching outward along the prefix chain for a same-named attribute that
/// *is* accessible (e.g. an unprotected declaration in an ancestor class that
/// a subclass separately re-declared and protected under its own name).
///
/// The common case (no protection recorded for `attribute` at `class_name`)
/// is a single map lookup, matching the previous behavior's cost; the
/// prefix-chain walk (and per-level attribute-existence scan) only runs when
/// an actual protected-and-blocked attribute is found, since that's the only
/// situation where the fallback search is needed.
fn check_remote_attribute_protection(
    class_name: &str,
    attribute: &str,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    access_span: Option<crate::error::Span>,
) -> Result<(), CompileError> {
    let Some(merged) = concatenated_classes
        .get(class_name)
        .or_else(|| find_known_class_ignore_case(concatenated_classes, class_name))
    else {
        return Ok(());
    };
    let storage_name = substitute_remote_attribute(class_name, attribute, concatenated_classes);
    let Some(protection) = merged
        .protection_map
        .get(attribute)
        .or_else(|| merged.protection_map.get(&storage_name))
    else {
        return Ok(());
    };

    if protection.protected
        && !in_protection_hierarchy(current_class, protection, concatenated_classes)
    {
        let chain = prefix_chain_ordered(class_name, concatenated_classes);
        for level in chain.iter().rev().skip(1) {
            let Some(level_class) = concatenated_classes
                .get(level)
                .or_else(|| find_known_class_ignore_case(concatenated_classes, level))
            else {
                continue;
            };
            let level_storage = substitute_remote_attribute(level, attribute, concatenated_classes);
            match level_class
                .protection_map
                .get(attribute)
                .or_else(|| level_class.protection_map.get(&level_storage))
            {
                Some(level_protection) => {
                    if level_protection.protected
                        && !in_protection_hierarchy(
                            current_class,
                            level_protection,
                            concatenated_classes,
                        )
                    {
                        continue;
                    }
                    if let Some(access_class) = current_class
                        && is_hidden_from(access_class, level_protection, concatenated_classes)
                    {
                        continue;
                    }
                    return Ok(());
                }
                None => {
                    // No protection recorded at this level: accessible if the
                    // attribute is actually declared here (directly, not
                    // merely inherited under a different storage name).
                    if find_attribute_in_raw_class(level_class, &level_storage).is_some()
                        || find_attribute_in_raw_class(level_class, attribute).is_some()
                    {
                        return Ok(());
                    }
                }
            }
        }
        return Err(crate::diagnostics::protected_attribute(
            attribute,
            access_span,
            protection.protected_span.clone(),
        ));
    }

    if let Some(access_class) = current_class
        && is_hidden_from(access_class, protection, concatenated_classes)
    {
        return Err(crate::diagnostics::hidden_attribute(
            attribute,
            access_class,
            access_span,
            protection.hidden_span.clone(),
        ));
    }
    Ok(())
}

fn check_variable_protection(
    variable: &Variable,
    scope: &HashMap<String, Type>,
    current_class: Option<&str>,
    concatenated_classes: &HashMap<String, ClassDeclaration>,
    access_span: Option<crate::error::Span>,
) -> Result<(), CompileError> {
    match variable {
        // Bare names: a hidden/inaccessible class attribute is simply not a
        // candidate for lookup (§5.5.4 / §5.5.6.5), so an enclosing or global
        // binding of the same name can be used. Remote `obj.attr` still uses
        // [`check_remote_attribute_protection`].
        Variable::Simple(_) | Variable::Subscripted { .. } => Ok(()),
        Variable::Qua { object, .. } => check_variable_protection(
            object,
            scope,
            current_class,
            concatenated_classes,
            access_span,
        ),
        Variable::Remote { object, attribute } => {
            let object_class = object_class_name(object, scope)?;
            if let Some(class_name) = object_class {
                check_remote_attribute_protection(
                    &class_name,
                    attribute,
                    current_class,
                    concatenated_classes,
                    access_span.clone(),
                )?;
            }
            check_variable_protection(
                object,
                scope,
                current_class,
                concatenated_classes,
                access_span,
            )
        }
        Variable::RemoteCall { object, .. } => check_variable_protection(
            object,
            scope,
            current_class,
            concatenated_classes,
            access_span,
        ),
    }
}

fn object_class_name(
    expr: &Variable,
    scope: &HashMap<String, Type>,
) -> Result<Option<String>, CompileError> {
    match expr {
        Variable::Simple(name) => Ok(scope_get_ignore_case(scope, name).and_then(|ty| match ty {
            Type::ObjectRef(class_name) => Some(class_name.clone()),
            _ => None,
        })),
        Variable::Qua { class_name, .. } => Ok(Some(class_name.clone())),
        Variable::Remote { object, .. } => object_class_name(object, scope),
        Variable::RemoteCall { object, .. } => object_class_name(object, scope),
        Variable::Subscripted { name, .. } => {
            Ok(scope_get_ignore_case(scope, name).and_then(|ty| match ty {
                Type::Array { element, .. } => match element.as_ref() {
                    Type::ObjectRef(class_name) => Some(class_name.clone()),
                    _ => None,
                },
                Type::ObjectRef(class_name) => Some(class_name.clone()),
                _ => None,
            }))
        }
    }
}

/// Class of a remote receiver after evaluating array/procedure attributes
/// (`wr.ra2(0,0)` → `A`, not `W`).
fn typed_object_class_name(expr: &Variable, ctx: &TypeContext<'_>) -> Option<String> {
    match type_of_variable(expr, ctx) {
        Some(Type::ObjectRef(class_name)) => Some(class_name),
        _ => object_class_name(expr, ctx.scope).ok().flatten(),
    }
}

fn expr_object_class_name(
    expr: &Expr,
    scope: &HashMap<String, Type>,
) -> Result<Option<String>, CompileError> {
    match &expr.kind {
        ExprKind::Variable(variable) => object_class_name(variable, scope),
        ExprKind::This(class_name) => Ok(Some(class_name.clone())),
        ExprKind::Qua { class_name, .. } => Ok(Some(class_name.clone())),
        ExprKind::New { class_name, .. } => Ok(Some(class_name.clone())),
        ExprKind::RemoteAccess { object, .. } => expr_object_class_name(object, scope),
        // Array indexing often parses as `FunctionCall` (`ra(0)`); treat like
        // `Variable::Subscripted` so `ra(0).t` resolves to the element class.
        ExprKind::FunctionCall { name, arguments } => Ok(scope_get_ignore_case(scope, name)
            .and_then(|ty| match ty {
                Type::Array { element, dims } if *dims == 0 || *dims == arguments.len() => {
                    match element.as_ref() {
                        Type::ObjectRef(class_name) => Some(class_name.clone()),
                        _ => None,
                    }
                }
                Type::ObjectRef(class_name) if arguments.is_empty() => Some(class_name.clone()),
                _ => None,
            })),
        ExprKind::None => Ok(Some("none".into())),
        _ => Ok(None),
    }
}

fn typed_expr_object_class_name(
    expr: &Expr,
    ctx: &TypeContext<'_>,
) -> Result<Option<String>, CompileError> {
    match &expr.kind {
        ExprKind::Variable(variable) => Ok(typed_object_class_name(variable, ctx)),
        _ => match type_of_expr(expr, ctx)? {
            Type::ObjectRef(class_name) => Ok(Some(class_name)),
            _ => expr_object_class_name(expr, ctx.scope),
        },
    }
}

fn type_of_expr(expr: &Expr, ctx: &TypeContext<'_>) -> Result<Type, CompileError> {
    type_of_expr_expecting(expr, ctx, None)
}

fn type_of_expr_expecting(
    expr: &Expr,
    ctx: &TypeContext<'_>,
    expected: Option<&Type>,
) -> Result<Type, CompileError> {
    match &expr.kind {
        ExprKind::StringLiteral(_) | ExprKind::Notext => Ok(Type::Text),
        ExprKind::CharacterLiteral(_) => Ok(Type::Character),
        ExprKind::BooleanLiteral(_) => Ok(Type::Boolean),
        ExprKind::NumberLiteral { kind, .. } => Ok(match kind {
            ArithmeticLiteralKind::Integer => Type::integer_literal(),
            ArithmeticLiteralKind::Real => Type::real_literal(false),
            ArithmeticLiteralKind::LongReal => Type::real_literal(true),
        }),
        ExprKind::Variable(variable) => {
            // Parameterless `InLine` (no `()`) parses as a simple variable.
            if matches!(variable, Variable::Simple(name) if name.eq_ignore_ascii_case("InLine")) {
                return Ok(Type::Text);
            }
            if matches!(variable, Variable::Simple(name) if name.eq_ignore_ascii_case("InChar")) {
                return Ok(Type::Character);
            }
            if matches!(variable, Variable::Simple(name) if name.eq_ignore_ascii_case("Endfile")) {
                return Ok(Type::Boolean);
            }
            if matches!(variable, Variable::Simple(name) if name.eq_ignore_ascii_case("sysin")) {
                return Ok(Type::ObjectRef("InFile".into()));
            }
            if matches!(variable, Variable::Simple(name) if name.eq_ignore_ascii_case("sysout")) {
                return Ok(Type::ObjectRef("PrintFile".into()));
            }
            if matches!(
                variable,
                Variable::Simple(name)
                    if name.eq_ignore_ascii_case("CURRENTLOWTEN")
                        || name.eq_ignore_ascii_case("CURRENTDECIMALMARK")
            ) {
                return Ok(Type::Character);
            }
            if let Variable::Simple(name) = variable
                && let Some(ty) = crate::environment::builtin_result_type(name)
                && matches!(
                    name.to_ascii_lowercase().as_str(),
                    "datetime" | "cputime" | "clocktime" | "sourceline"
                )
            {
                return Ok(ty);
            }
            if let Some(ty) = type_of_variable(variable, ctx) {
                return Ok(ty);
            }
            if let Variable::Simple(name) = variable {
                if crate::simulation::is_simset_method(name)
                    && ctx.current_class.is_some_and(|class| {
                        class_provides_simset_methods(class, ctx.concatenated_classes)
                    })
                {
                    return Ok(crate::simulation::simset_method_result_type(name));
                }
                if crate::basicio::is_basicio_method(name)
                    && ctx
                        .current_class
                        .is_some_and(crate::basicio::is_basicio_class)
                {
                    return Ok(crate::basicio::basicio_method_result_type(name));
                }
                if let Some(ty) = crate::basicio::basicio_free_result_type(name) {
                    return Ok(ty);
                }
                if is_label_or_switch(name, ctx) {
                    return Ok(Type::Integer { short: false });
                }
                return Err(unknown_simple(name, expr.span.clone(), ctx.scope));
            }
            if let Variable::Subscripted { name, .. } = variable
                && scope_get_ignore_case(ctx.scope, name).is_none()
            {
                return Err(unknown_simple(name, expr.span.clone(), ctx.scope));
            }
            Ok(Type::Integer { short: false })
        }
        ExprKind::Unary { op, operand } => match op {
            UnaryOp::Not => {
                ensure_boolean_role(operand, ctx, ExpectRole::NotOperand)?;
                Ok(Type::Boolean)
            }
            UnaryOp::Plus | UnaryOp::Minus => type_of_expr(operand, ctx),
        },
        ExprKind::Binary { op, left, right } => type_of_binary(*op, left, right, ctx),
        ExprKind::Relation { left, .. } => {
            let _ = type_of_expr(left, ctx)?;
            Ok(Type::Boolean)
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            ensure_boolean_role(condition, ctx, ExpectRole::IfCondition)?;
            let then_type = type_of_expr_expecting(then_expr, ctx, expected)?;
            let else_type = type_of_expr_expecting(else_expr, ctx, expected)?;
            conditional_result_type(&then_type, &else_type, then_expr, else_expr, expected, ctx)
        }
        ExprKind::Paren(inner) => type_of_expr_expecting(inner, ctx, expected),
        ExprKind::FunctionCall { name, arguments } => {
            for argument in arguments {
                let _ = type_of_expr(argument, ctx)?;
            }
            if name.eq_ignore_ascii_case("inline") {
                return Ok(Type::Text);
            }
            if let Some(ty) = crate::basicio::basicio_free_result_type(name) {
                return Ok(ty);
            }
            if crate::basicio::free_basicio_target(name).is_some()
                && crate::basicio::is_basicio_method(name)
            {
                return Err(crate::diagnostics::statement_as_expression(
                    name,
                    Some(expr.span.clone()),
                ));
            }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "inimage"
                    | "outchar"
                    | "breakoutimage"
                    | "outtext"
                    | "outimage"
                    | "outint"
                    | "terminate_program"
            ) {
                return Err(crate::diagnostics::statement_as_expression(
                    name,
                    Some(expr.span.clone()),
                ));
            }
            if is_text_frame_procedure(name) {
                return Ok(Type::Text);
            }
            if let Some(ty) = builtin_result_type(name) {
                return Ok(ty);
            }
            if is_environment_procedure(name) {
                return Ok(Type::Integer { short: false });
            }
            if is_filesystem_procedure(name) {
                if !filesystem_procedure_returns_value(name) {
                    return Err(crate::diagnostics::statement_as_expression(
                        name,
                        Some(expr.span.clone()),
                    ));
                }
                analyze_filesystem_procedure_call(name, arguments, ctx)?;
                return Ok(match name.to_ascii_lowercase().as_str() {
                    "fileexists" => Type::Boolean,
                    "fileread" => Type::Text,
                    _ => Type::Integer { short: false },
                });
            }
            if let Some(Type::Array { element, dims }) = scope_get_ignore_case(ctx.scope, name)
                && (*dims == 0 || *dims == arguments.len())
            {
                return Ok((**element).clone());
            }
            if let Some(ty) = scope_get_ignore_case(ctx.scope, name) {
                return Ok(ty.clone());
            }
            Ok(Type::Integer { short: false })
        }
        ExprKind::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            for argument in arguments {
                let _ = type_of_expr(argument, ctx)?;
            }
            if is_text_expression_expr(object, ctx.scope) {
                return Ok(text_intrinsic_type(attribute).unwrap_or(Type::Integer { short: false }));
            }
            if crate::simulation::is_simset_method(attribute) {
                return Ok(crate::simulation::simset_method_result_type(attribute));
            }
            if crate::basicio::is_basicio_method(attribute) {
                return Ok(crate::basicio::basicio_method_result_type(attribute));
            }
            let Some(object_class) = expr_object_class_name(object, ctx.scope)? else {
                return Ok(Type::Integer { short: false });
            };
            if let Some(AttributeMatch::Variable(Type::Array { element, dims })) =
                find_remote_attribute_match(
                    &object_class,
                    attribute,
                    ctx.current_class,
                    ctx.raw_classes,
                )
            {
                if dims == 0 || dims == arguments.len() {
                    return Ok((*element).clone());
                }
            }
            if attribute.eq_ignore_ascii_case("idle")
                || attribute.eq_ignore_ascii_case("terminated")
            {
                return Ok(Type::Boolean);
            }
            if attribute.eq_ignore_ascii_case("evtime") {
                return Ok(Type::Real { long: true });
            }
            let Some(procedure) = find_remote_procedure_match(
                &object_class,
                attribute,
                ctx.current_class,
                ctx.raw_classes,
            ) else {
                return Ok(Type::Integer { short: false });
            };
            Ok(procedure
                .result_type
                .clone()
                .unwrap_or(Type::Integer { short: false }))
        }
        ExprKind::RemoteAccess { object, attribute } => {
            if is_text_expression_expr(object, ctx.scope) {
                return Ok(text_intrinsic_type(attribute).unwrap_or(Type::Integer { short: false }));
            }
            if crate::simulation::is_simset_method(attribute) {
                return Ok(crate::simulation::simset_method_result_type(attribute));
            }
            if crate::basicio::is_basicio_method(attribute) {
                return Ok(crate::basicio::basicio_method_result_type(attribute));
            }
            if attribute.eq_ignore_ascii_case("idle")
                || attribute.eq_ignore_ascii_case("terminated")
            {
                return Ok(Type::Boolean);
            }
            if attribute.eq_ignore_ascii_case("evtime") {
                return Ok(Type::Real { long: true });
            }
            let Some(object_class) = typed_expr_object_class_name(object, ctx)? else {
                return Ok(Type::Integer { short: false });
            };
            let Some(matched) = find_remote_attribute_match(
                &object_class,
                attribute,
                ctx.current_class,
                ctx.raw_classes,
            ) else {
                return Ok(Type::Integer { short: false });
            };
            match matched {
                AttributeMatch::Variable(ty) => Ok(ty),
                AttributeMatch::Procedure => Ok(Type::Integer { short: false }),
            }
        }
        ExprKind::None => Ok(Type::ObjectRef("none".into())),
        ExprKind::New { class_name, .. } => Ok(Type::ObjectRef(class_name.clone())),
        ExprKind::This(class_name) => Ok(Type::ObjectRef(class_name.clone())),
        ExprKind::Qua { object, class_name } => {
            let _ = type_of_expr(object, ctx)?;
            Ok(Type::ObjectRef(class_name.clone()))
        }
    }
}

fn type_of_binary(
    op: BinaryOp,
    left: &Expr,
    right: &Expr,
    ctx: &TypeContext<'_>,
) -> Result<Type, CompileError> {
    match op {
        BinaryOp::TextConcat => {
            ensure_text_role(left, ctx, ExpectRole::TextConcat)?;
            ensure_text_role(right, ctx, ExpectRole::TextConcat)?;
            Ok(Type::Text)
        }
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Imp
        | BinaryOp::Eqv
        | BinaryOp::AndThen
        | BinaryOp::OrElse => {
            let op = binary_op_lexeme(op);
            ensure_boolean_role(left, ctx, ExpectRole::BooleanOp { op })?;
            ensure_boolean_role(right, ctx, ExpectRole::BooleanOp { op })?;
            Ok(Type::Boolean)
        }
        BinaryOp::IntDiv => {
            ensure_integer_role(left, ctx, ExpectRole::ArithmeticOp { op: "//" })?;
            ensure_integer_role(right, ctx, ExpectRole::ArithmeticOp { op: "//" })?;
            Ok(Type::Integer { short: false })
        }
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow => {
            let left_type = type_of_expr(left, ctx)?;
            let right_type = type_of_expr(right, ctx)?;
            if op == BinaryOp::Add && left_type == Type::Text && right_type == Type::Text {
                return Err(crate::diagnostics::plus_on_text(
                    &text_concat_example(left, right),
                    left.span.start..right.span.end,
                    plus_operator_span(left, right),
                ));
            }
            arithmetic_result_type(
                &left_type,
                &right_type,
                left.span.clone(),
                binary_op_lexeme(op),
            )
        }
    }
}

fn binary_op_lexeme(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::IntDiv => "//",
        BinaryOp::Pow => "**",
        BinaryOp::TextConcat => "&",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
        BinaryOp::Imp => "imp",
        BinaryOp::Eqv => "eqv",
        BinaryOp::AndThen => "and then",
        BinaryOp::OrElse => "or else",
    }
}

fn plus_operator_span(left: &Expr, right: &Expr) -> Option<crate::error::Span> {
    let span = left.span.end..right.span.start;
    (span.start < span.end).then_some(span)
}

fn text_concat_example(left: &Expr, right: &Expr) -> String {
    match (text_concat_operand(left), text_concat_operand(right)) {
        (Some(left), Some(right)) => format!("{left} & {right}"),
        _ => "\"hello\" & \"world\"".into(),
    }
}

fn text_concat_operand(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::StringLiteral(value) => Some(format!("\"{}\"", value.replace('"', "\"\""))),
        ExprKind::Notext => Some("notext".into()),
        ExprKind::Variable(Variable::Simple(name)) => Some(name.clone()),
        ExprKind::Paren(inner) => text_concat_operand(inner),
        _ => None,
    }
}

fn arithmetic_result_type(
    left: &Type,
    right: &Type,
    span: crate::error::Span,
    op: &'static str,
) -> Result<Type, CompileError> {
    if !left.is_arithmetic() {
        return Err(crate::diagnostics::type_mismatch(
            ExpectRole::ArithmeticOp { op },
            left,
            &Type::Integer { short: false },
            span,
        ));
    }
    if !right.is_arithmetic() {
        return Err(crate::diagnostics::type_mismatch(
            ExpectRole::ArithmeticOp { op },
            right,
            &Type::Integer { short: false },
            span,
        ));
    }

    Ok(promote_arithmetic(left, right))
}

fn promote_arithmetic(left: &Type, right: &Type) -> Type {
    use Type::{Integer, Real};

    match (left, right) {
        (Real { long: true }, _) | (_, Real { long: true }) => Type::Real { long: true },
        (Real { .. }, _) | (_, Real { .. }) => Type::Real { long: false },
        (Integer { .. }, Integer { .. }) => Type::Integer { short: false },
        _ => Type::Integer { short: false },
    }
}

fn conditional_result_type(
    then_type: &Type,
    else_type: &Type,
    then_expr: &Expr,
    else_expr: &Expr,
    expected: Option<&Type>,
    ctx: &TypeContext<'_>,
) -> Result<Type, CompileError> {
    if let Some(unified) = unify_conditional_branches(then_type, else_type, ctx.raw_classes) {
        return Ok(unified);
    }

    if let Some(expected) = expected {
        if !types_assignment_compatible(
            expected,
            then_type,
            ctx.concatenated_classes,
            ctx.raw_classes,
        ) {
            return Err(crate::diagnostics::if_branch_should_be(
                crate::diagnostics::IfBranch::Then,
                then_type,
                expected,
                then_expr.span.clone(),
            ));
        }
        if !types_assignment_compatible(
            expected,
            else_type,
            ctx.concatenated_classes,
            ctx.raw_classes,
        ) {
            return Err(crate::diagnostics::if_branch_should_be(
                crate::diagnostics::IfBranch::Else,
                else_type,
                expected,
                else_expr.span.clone(),
            ));
        }
        return Ok(expected.clone());
    }

    Err(crate::diagnostics::incompatible_branches(
        then_type,
        else_type,
        then_expr.span.clone(),
    ))
}

fn unify_conditional_branches(
    then_type: &Type,
    else_type: &Type,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Option<Type> {
    if then_type.is_arithmetic() && else_type.is_arithmetic() {
        return Some(promote_arithmetic(then_type, else_type));
    }

    if then_type == else_type {
        return Some(then_type.clone());
    }

    if then_type.accepts_assignment_from(else_type) {
        return Some(then_type.clone());
    }

    if else_type.accepts_assignment_from(then_type) {
        return Some(else_type.clone());
    }

    // Reference subordination in either direction: branches yielding refs to
    // related classes (e.g. `ref(station)` and `ref(Link)` in a common
    // hierarchy) share a compatible ancestor type, per Simula §2.4.2.
    if let (Type::ObjectRef(_), Type::ObjectRef(_)) = (then_type, else_type) {
        if crate::concatenate::ref_type_subordinates(else_type, then_type, raw_classes) {
            return Some(then_type.clone());
        }
        if crate::concatenate::ref_type_subordinates(then_type, else_type, raw_classes) {
            return Some(else_type.clone());
        }
    }

    None
}

fn scope_get_ignore_case<'a>(scope: &'a HashMap<String, Type>, name: &str) -> Option<&'a Type> {
    scope.get(name).or_else(|| {
        scope
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, ty)| ty)
    })
}

fn type_of_variable(variable: &Variable, ctx: &TypeContext<'_>) -> Option<Type> {
    match variable {
        Variable::Simple(name) => {
            if name.eq_ignore_ascii_case("CURRENTLOWTEN")
                || name.eq_ignore_ascii_case("CURRENTDECIMALMARK")
            {
                return Some(Type::Character);
            }
            scope_get_ignore_case(ctx.scope, name)
                .cloned()
                .or_else(|| environment_constant_type(name))
                .or_else(|| {
                    name.strip_prefix("__simrt_encl_")
                        .and_then(|base| scope_get_ignore_case(ctx.scope, base).cloned())
                })
        }
        Variable::Subscripted { name, subscripts } => {
            let ty = scope_get_ignore_case(ctx.scope, name)?;
            match ty {
                Type::Array { element, dims } if *dims == 0 || *dims == subscripts.len() => {
                    Some((**element).clone())
                }
                _ => None,
            }
        }
        Variable::Qua { class_name, .. } => Some(Type::ObjectRef(class_name.clone())),
        Variable::Remote { object, attribute } => {
            if is_text_expression(object, ctx.scope) {
                return text_intrinsic_type(attribute);
            }
            if crate::simulation::is_simset_method(attribute) {
                return Some(crate::simulation::simset_method_result_type(attribute));
            }
            if crate::basicio::is_basicio_method(attribute) {
                return Some(crate::basicio::basicio_method_result_type(attribute));
            }
            if attribute.eq_ignore_ascii_case("idle")
                || attribute.eq_ignore_ascii_case("terminated")
            {
                return Some(Type::Boolean);
            }
            if attribute.eq_ignore_ascii_case("evtime") {
                return Some(Type::Real { long: true });
            }
            let object_class = typed_object_class_name(object, ctx)?;
            let matched = find_remote_attribute_match(
                &object_class,
                attribute,
                ctx.current_class,
                ctx.raw_classes,
            )?;
            match matched {
                AttributeMatch::Variable(ty) => Some(ty),
                AttributeMatch::Procedure => None,
            }
        }
        Variable::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            if is_text_expression(object, ctx.scope)
                && TextIntrinsic::parse(attribute) == Some(TextIntrinsic::Sub)
                && arguments.len() == 2
            {
                return Some(Type::Text);
            }
            if crate::simulation::is_simset_method(attribute) {
                return Some(crate::simulation::simset_method_result_type(attribute));
            }
            if crate::basicio::is_basicio_method(attribute) {
                return Some(crate::basicio::basicio_method_result_type(attribute));
            }
            let object_class = object_class_name(object, ctx.scope).ok()??;
            if let Some(AttributeMatch::Variable(Type::Array { element, dims })) =
                find_remote_attribute_match(
                    &object_class,
                    attribute,
                    ctx.current_class,
                    ctx.raw_classes,
                )
            {
                if dims == 0 || dims == arguments.len() {
                    return Some((*element).clone());
                }
            }
            if attribute.eq_ignore_ascii_case("idle")
                || attribute.eq_ignore_ascii_case("terminated")
            {
                return Some(Type::Boolean);
            }
            if attribute.eq_ignore_ascii_case("evtime") {
                return Some(Type::Real { long: true });
            }
            let _ = arguments;
            None
        }
    }
}

#[allow(dead_code)]
fn ensure_boolean(expr: &Expr, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    ensure_boolean_role(expr, ctx, ExpectRole::Generic { wanted: "boolean" })
}

fn ensure_boolean_role(
    expr: &Expr,
    ctx: &TypeContext<'_>,
    role: ExpectRole,
) -> Result<(), CompileError> {
    let ty = type_of_expr_expecting(expr, ctx, Some(&Type::Boolean))?;
    if ty != Type::Boolean {
        return Err(crate::diagnostics::type_mismatch(
            role,
            &ty,
            &Type::Boolean,
            expr.span.clone(),
        ));
    }
    Ok(())
}

fn ensure_integer(expr: &Expr, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    ensure_integer_role(expr, ctx, ExpectRole::Generic { wanted: "integer" })
}

fn ensure_integer_role(
    expr: &Expr,
    ctx: &TypeContext<'_>,
    role: ExpectRole,
) -> Result<(), CompileError> {
    let ty = type_of_expr_expecting(expr, ctx, Some(&Type::Integer { short: false }))?;
    if !matches!(ty, Type::Integer { .. }) {
        return Err(crate::diagnostics::type_mismatch(
            role,
            &ty,
            &Type::Integer { short: false },
            expr.span.clone(),
        ));
    }
    Ok(())
}

#[allow(dead_code)]
fn ensure_arithmetic(expr: &Expr, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    ensure_arithmetic_role(
        expr,
        ctx,
        ExpectRole::Generic {
            wanted: "an arithmetic value",
        },
    )
}

fn ensure_arithmetic_role(
    expr: &Expr,
    ctx: &TypeContext<'_>,
    role: ExpectRole,
) -> Result<(), CompileError> {
    let ty = type_of_expr(expr, ctx)?;
    if !ty.is_arithmetic() {
        return Err(crate::diagnostics::type_mismatch(
            role,
            &ty,
            &Type::Integer { short: false },
            expr.span.clone(),
        ));
    }
    Ok(())
}

fn ensure_character(expr: &Expr, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    let ty = type_of_expr_expecting(expr, ctx, Some(&Type::Character))?;
    if !matches!(ty, Type::Character | Type::Integer { .. }) {
        return Err(crate::diagnostics::type_mismatch(
            ExpectRole::Generic {
                wanted: "character",
            },
            &ty,
            &Type::Character,
            expr.span.clone(),
        ));
    }
    Ok(())
}

fn ensure_text(expr: &Expr, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    ensure_text_role(expr, ctx, ExpectRole::Generic { wanted: "text" })
}

fn ensure_text_role(
    expr: &Expr,
    ctx: &TypeContext<'_>,
    role: ExpectRole,
) -> Result<(), CompileError> {
    let ty = type_of_expr_expecting(expr, ctx, Some(&Type::Text))?;
    if ty != Type::Text {
        return Err(crate::diagnostics::type_mismatch(
            role,
            &ty,
            &Type::Text,
            expr.span.clone(),
        ));
    }
    Ok(())
}

fn is_text_expression(object: &Variable, scope: &HashMap<String, Type>) -> bool {
    match object {
        Variable::Simple(name) => {
            matches!(scope_get_ignore_case(scope, name), Some(Type::Text))
        }
        Variable::Remote { object, attribute } => {
            // `sysin.image` / file.image is a text quantity (BASICIO).
            if crate::basicio::is_basicio_method(attribute)
                && crate::basicio::basicio_method_result_type(attribute) == Type::Text
                && is_basicio_receiver_variable(object, scope)
            {
                return true;
            }
            is_text_expression(object, scope)
        }
        Variable::Qua { .. } => false,
        Variable::RemoteCall {
            object, attribute, ..
        } => {
            if text_intrinsic_type(attribute) == Some(Type::Text)
                && is_text_expression(object, scope)
            {
                return true;
            }
            is_text_expression(object, scope)
        }
        Variable::Subscripted { .. } => false,
    }
}

fn is_basicio_receiver_variable(object: &Variable, scope: &HashMap<String, Type>) -> bool {
    match object {
        Variable::Simple(name) => {
            name.eq_ignore_ascii_case("sysin")
                || name.eq_ignore_ascii_case("sysout")
                || matches!(
                    scope_get_ignore_case(scope, name),
                    Some(Type::ObjectRef(class)) if crate::basicio::is_basicio_class(class)
                )
        }
        Variable::Qua { object, .. }
        | Variable::Remote { object, .. }
        | Variable::RemoteCall { object, .. } => is_basicio_receiver_variable(object, scope),
        Variable::Subscripted { .. } => false,
    }
}

fn is_text_expression_expr(expr: &Expr, scope: &HashMap<String, Type>) -> bool {
    match &expr.kind {
        ExprKind::Variable(variable) => is_text_expression(variable, scope),
        ExprKind::StringLiteral(_) | ExprKind::Notext => true,
        ExprKind::Paren(inner) => is_text_expression_expr(inner, scope),
        ExprKind::RemoteAccess { object, attribute }
        | ExprKind::RemoteCall {
            object, attribute, ..
        } => {
            if text_intrinsic_type(attribute) == Some(Type::Text)
                && is_text_expression_expr(object, scope)
            {
                return true;
            }
            // `sysout.image` / file.image yields text for further intrinsics
            // (`.strip`, `.main`, …).
            crate::basicio::is_basicio_method(attribute)
                && crate::basicio::basicio_method_result_type(attribute) == Type::Text
                && is_basicio_receiver_expr(object, scope)
        }
        ExprKind::FunctionCall { name, .. }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "blanks" | "copy" | "fileread" | "inline"
            ) =>
        {
            true
        }
        ExprKind::If {
            then_expr,
            else_expr,
            ..
        } => is_text_expression_expr(then_expr, scope) && is_text_expression_expr(else_expr, scope),
        ExprKind::Binary {
            op: BinaryOp::TextConcat,
            ..
        } => true,
        _ => false,
    }
}

fn is_basicio_receiver_expr(expr: &Expr, scope: &HashMap<String, Type>) -> bool {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => {
            name.eq_ignore_ascii_case("sysin")
                || name.eq_ignore_ascii_case("sysout")
                || matches!(
                    scope_get_ignore_case(scope, name),
                    Some(Type::ObjectRef(qual)) if crate::basicio::is_basicio_class(qual)
                )
        }
        ExprKind::Paren(inner) => is_basicio_receiver_expr(inner, scope),
        _ => false,
    }
}

fn text_intrinsic_type(attribute: &str) -> Option<Type> {
    TextIntrinsic::parse(attribute).and_then(|intrinsic| intrinsic.result_type())
}

fn analyze_text_frame_procedure_call(
    name: &str,
    arguments: &[Expr],
    ctx: &TypeContext<'_>,
) -> Result<(), CompileError> {
    match name.to_ascii_lowercase().as_str() {
        "blanks" => {
            if arguments.len() != 1 {
                return Err(crate::diagnostics::arity_mismatch(
                    "blanks",
                    1,
                    arguments.len(),
                    arguments.first().map(|a| a.span.clone()).unwrap_or(0..0),
                ));
            }
            ensure_integer(&arguments[0], ctx)?;
        }
        "copy" => {
            if arguments.len() != 1 {
                return Err(crate::diagnostics::arity_mismatch(
                    "copy",
                    1,
                    arguments.len(),
                    arguments.first().map(|a| a.span.clone()).unwrap_or(0..0),
                ));
            }
            ensure_text(&arguments[0], ctx)?;
        }
        _ => {}
    }
    Ok(())
}

fn analyze_filesystem_procedure_call(
    name: &str,
    arguments: &[Expr],
    ctx: &TypeContext<'_>,
) -> Result<(), CompileError> {
    match name.to_ascii_lowercase().as_str() {
        "fileexists" | "fileread" => {
            if arguments.len() != 1 {
                return Err(crate::diagnostics::arity_mismatch(
                    name,
                    1,
                    arguments.len(),
                    arguments.first().map(|a| a.span.clone()).unwrap_or(0..0),
                ));
            }
            ensure_text(&arguments[0], ctx)?;
        }
        "filewrite" => {
            if arguments.len() != 2 {
                return Err(crate::diagnostics::arity_mismatch(
                    "fileWrite",
                    2,
                    arguments.len(),
                    arguments.first().map(|a| a.span.clone()).unwrap_or(0..0),
                ));
            }
            ensure_text(&arguments[0], ctx)?;
            ensure_text(&arguments[1], ctx)?;
        }
        _ => {}
    }
    Ok(())
}

#[allow(dead_code)]
fn ensure_text_expr(expr: &Expr, ctx: &TypeContext<'_>) -> Result<(), CompileError> {
    ensure_text(expr, ctx)
}

trait TypeExt {
    fn is_arithmetic(&self) -> bool;
    fn is_reference(&self) -> bool;
}

impl TypeExt for Type {
    fn is_arithmetic(&self) -> bool {
        matches!(self, Type::Integer { .. } | Type::Real { .. })
    }

    fn is_reference(&self) -> bool {
        matches!(self, Type::ObjectRef(_) | Type::Text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lex::tokenize, parse::parse, source::SourceFile};

    fn analyze_source(source: &str) -> Result<(), CompileError> {
        let file = SourceFile::anonymous(source);
        let tokens = tokenize(&file)?;
        let program = parse(&tokens)?;
        analyze(&program)
    }

    #[test]
    fn accepts_label_and_switch_formals() {
        analyze_source(
            r#"begin
               procedure P(L); label L; begin goto L end;
               procedure R(S); switch S; begin goto S(1) end;
               switch S1 := L1;
               P(L1); R(S1);
               L1: ;
             end;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_unmatched_virtual_label_goto() {
        analyze_source(
            r#"begin
               class C;
               virtual: label EOP;
               begin goto EOP end;
               C begin EOP: end;
             end;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_basicio_methods_in_inspect_new_directfile() {
        analyze_source(
            r#"begin
               inspect new DirectFile("f") do begin
                  locate(1); outint(1, 2); outimage;
               end;
             end;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_directfile_location_in_inspect() {
        analyze_source(
            r#"begin
               inspect new DirectFile("f") do begin
                  integer n; n := location;
               end;
             end;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_simulation_main_and_nextev() {
        analyze_source(
            r#"begin
               Simulation begin
                  Process class P; begin
                     if nextev == none then activate main;
                  end;
                  activate new P;
               end;
             end;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_simset_out_in_process_body() {
        analyze_source(
            r#"begin
               Simulation begin
                  Process class P; begin out end;
                  activate new P;
               end;
             end;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_outtext_and_outimage() {
        analyze_source(r#"begin OutText("hello"); OutImage; end;"#).unwrap();
    }

    #[test]
    fn accepts_sysin_image_sub_reference_assignment() {
        analyze_source(r#"begin sysin.image:-sysin.image.sub(1,5); end;"#).unwrap();
    }

    #[test]
    fn accepts_two_simset_blocks_with_same_class_name() {
        analyze_source(
            r#"BEGIN
SIMSET Begin
  Link Class A; Begin End;
End;
SIMSET Begin
  Link Class A; Begin End;
End;
END;"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_arithmetic_expressions() {
        analyze_source("begin integer x; x := 1 + 2 * 3; end;").unwrap();
        analyze_source("begin real r; r := 1.0 / 2.0; end;").unwrap();
        analyze_source("begin integer x; x := 7 // 3; end;").unwrap();
    }

    #[test]
    fn accepts_boolean_expressions() {
        analyze_source("begin boolean a, b, c; b := not a and b or c; end;").unwrap();
        analyze_source("begin boolean a, b, c; b := a and then b or else c; end;").unwrap();
    }

    #[test]
    fn accepts_relational_expressions() {
        analyze_source("begin boolean b; integer a; b := a + 1 < 2; end;").unwrap();
    }

    #[test]
    fn accepts_text_concatenation() {
        analyze_source(r#"begin text t; t := "a" & "b"; end;"#).unwrap();
    }

    #[test]
    fn accepts_conditional_expression() {
        analyze_source("begin integer x; x := if true then 1 else 2; end;").unwrap();
    }

    #[test]
    fn accepts_conditional_expression_with_related_ref_branches() {
        // Branches yielding refs to a common ancestor/descendant pair (e.g.
        // `ref(station)` vs `ref(Link)` in a SIMSET-style hierarchy) should
        // resolve to the ancestor type rather than being rejected outright.
        analyze_source(
            "begin class Link; begin end;
             Link class Station; begin end;
             ref(Link) g; ref(Station) s; boolean flag;
             g :- if flag then g else s; end;",
        )
        .unwrap();
    }

    #[test]
    fn rejects_integer_division_with_real() {
        let error = analyze_source("begin integer x; x := 1.0 // 2; end;").unwrap_err();
        assert!(
            error.to_string().contains("integer") || error.to_string().contains("`//`"),
            "{}",
            error
        );
    }

    #[test]
    fn rejects_outtext_with_wrong_argument_count() {
        let error = analyze_source(r#"begin OutText; end;"#).unwrap_err();
        assert!(
            error.to_string().contains("OutText") && error.to_string().contains("argument"),
            "{}",
            error
        );
    }

    #[test]
    fn accepts_enclosing_integer_and_text_in_class_body() {
        // Block-head declarations must be visible while analyzing class bodies
        // (§5.6.13); otherwise free `text` names default to integer and OutText fails.
        analyze_source(
            r#"begin
                integer n;
                text t;
                class Worker;
                begin
                    OutInt(n, 0);
                    OutText(t);
                end;
            end;"#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_array_formal_rank_mismatch_at_call() {
        let error = analyze_source(
            r#"begin
                integer array a(1:2,1:2);
                procedure take(x); integer array x;
                begin integer v; v := x(1); end;
                take(a);
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().to_ascii_lowercase().contains("array"),
            "{error}"
        );
    }

    #[test]
    fn rejects_variable_assignments_without_declarations() {
        assert!(analyze_source("begin A := B; end;").is_err());
        assert!(analyze_source("begin A := B.C; end;").is_err());
        assert!(analyze_source("begin A(1) := 1; end;").is_err());
    }

    #[test]
    fn accepts_valid_typed_declarations() {
        analyze_source("begin integer i; end;").unwrap();
        analyze_source("begin short integer si := 1; end;").unwrap();
        analyze_source("begin long real lr := 1.0; end;").unwrap();
        analyze_source("begin boolean b := false; end;").unwrap();
        analyze_source("begin character c := 'X'; end;").unwrap();
        analyze_source(r#"begin text t := "hi"; end;"#).unwrap();
        analyze_source("begin text t := notext; end;").unwrap();
        analyze_source("begin ref(File) f; end;").unwrap();
    }

    #[test]
    fn rejects_mismatched_initializer_type() {
        let error = analyze_source("begin integer i := true; end;").unwrap_err();
        assert!(
            error.to_string().contains("boolean") || error.to_string().contains("TYPE MISMATCH"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accepts_short_integer_assigned_to_integer() {
        analyze_source("begin integer i; short integer si; i := si; end;").unwrap();
    }

    #[test]
    fn accepts_long_real_assigned_to_real() {
        analyze_source("begin real r; long real lr; r := lr; end;").unwrap();
    }

    #[test]
    fn rejects_boolean_assigned_to_integer() {
        let error = analyze_source("begin integer i; boolean b; i := b; end;").unwrap_err();
        assert!(
            error.to_string().contains("assignment") && error.to_string().contains("boolean"),
            "{}",
            error
        );
    }

    #[test]
    fn reports_multiple_independent_statement_type_errors() {
        let source = "begin integer i; boolean b; i := b; b := 1; end;";
        let file = SourceFile::anonymous(source);
        let tokens = tokenize(&file).unwrap();
        let program = parse(&tokens).unwrap();
        let errors = analyze_all(&program).expect_err("expected multiple type errors");
        assert!(
            errors.len() >= 2,
            "expected at least two errors, got {}: {errors}",
            errors.len()
        );
        let rendered = errors.render_all(&file);
        assert!(
            rendered.contains("assignment needs"),
            "rendered: {rendered}"
        );
        let messages: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("boolean")),
            "messages: {messages:?}"
        );
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.contains("assignment needs"))
                .count(),
            2,
            "expected two assignment errors, messages: {messages:?}"
        );

        let bundled = analyze_all(&program).unwrap_err().into_bundled();
        assert_eq!(bundled.related.len(), 1);
        let bundled_render = bundled.render(&file);
        assert!(
            bundled_render.matches("assignment needs").count() >= 2,
            "bundled render: {bundled_render}"
        );
    }

    #[test]
    fn rejects_duplicate_declaration() {
        let error = analyze_source("begin integer i, i; end;").unwrap_err();
        assert!(
            error.to_string().contains("already declared")
                || error.to_string().contains("duplicate"),
            "{}",
            error
        );
    }

    #[test]
    fn accepts_chained_value_assignment() {
        analyze_source("begin integer a, b, c; a := b := c := 1; end;").unwrap();
        analyze_source("begin real x; integer i; real y; x := i := y := 3.14; end;").unwrap();
    }

    #[test]
    fn accepts_reference_assignment() {
        analyze_source("begin ref(Node) r, p; r :- p; end;").unwrap();
        analyze_source("begin ref(Node) a, b; a :- b :- none; end;").unwrap();
    }

    #[test]
    fn accepts_subordinate_ref_assignment() {
        analyze_source(
            "begin class Point(x); integer x; begin end;
             Point class Polar(r); real r; begin end;
             ref(Point) p; ref(Polar) q; p :- q; end;",
        )
        .unwrap();
    }

    #[test]
    fn rejects_value_assignment_to_reference_with_colon_equals() {
        let error = analyze_source("begin ref(Node) r; r := none; end;").unwrap_err();
        assert!(error.to_string().contains("value assignment"));
    }

    #[test]
    fn rejects_reference_assignment_with_wrong_operator() {
        let error = analyze_source("begin integer i; i :- 1; end;").unwrap_err();
        assert!(error.to_string().contains("reference assignment"));
    }

    #[test]
    fn accepts_while_and_if_statements() {
        analyze_source("begin integer i; while i < 1 do i := i + 1; end;").unwrap();
        analyze_source("begin integer n; if true then n := 1 else n := 0; end;").unwrap();
    }

    #[test]
    fn accepts_for_statement() {
        analyze_source("begin integer i; for i := 1 step 1 until 10 do i := i; end;").unwrap();
        analyze_source("begin integer i; for i := 1, 2 while true do i := i; end;").unwrap();
    }

    #[test]
    fn accepts_array_declarations() {
        analyze_source("begin integer array a(1:10); end;").unwrap();
        analyze_source("begin array a(0:5); end;").unwrap();
        analyze_source("begin integer array a, b(1:10); end;").unwrap();
        analyze_source("begin integer array m(1:10, 2:20); end;").unwrap();
    }

    #[test]
    fn rejects_array_bound_referencing_same_block_head() {
        let error = analyze_source("begin integer n; integer array a(1:n); end;").unwrap_err();
        assert!(error.to_string().contains("same block head"));
    }

    #[test]
    fn rejects_class_attribute_reference_outside_class_body() {
        let error =
            analyze_source("begin class Point; begin integer x; end; integer y; y := x; end;")
                .unwrap_err();
        assert!(
            error.to_string().contains("not visible"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accepts_connection_block_attribute_reference() {
        analyze_source(
            "begin class Node; integer x; begin x := 1; end; ref(Node) n; n :- new Node; inspect n do x := 2; end;",
        )
        .unwrap();
        analyze_source(
            "begin class Node; integer x; begin x := 1; end; ref(Node) n; n :- new Node; inspect n when Node do x := 2; end;",
        )
        .unwrap();
    }

    #[test]
    fn bare_hidden_without_protection_is_rejected() {
        // §5.5.4: only a protected attribute may be specified hidden.
        let error =
            analyze_source("begin class C; hidden x; begin integer x; end; end;").unwrap_err();
        assert!(
            error.to_string().contains("protected"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn hidden_attribute_allows_enclosing_bare_name() {
        // simtst98: `b` hides `a'i`; inside `c` bare `i` is the global.
        analyze_source(
            r#"begin
                integer i;
                class a;
                protected i;
                begin integer i; end;
                a class b;
                hidden i;
                begin end;
                b class c;
                begin integer x; x := i; end;
            end;"#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_remote_access_to_hidden_attribute() {
        let error = analyze_source(
            r#"begin
                class a;
                protected i;
                begin integer i; end;
                a class b;
                hidden i;
                begin end;
                ref(b) r;
                r :- new b;
                OutInt(r.i, 2);
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().to_ascii_lowercase().contains("hidden")
                || error.to_string().to_ascii_lowercase().contains("protected"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn outint_accepts_mixed_arithmetic_if() {
        // simtst64: Outint((if b then iv else rv), w)
        analyze_source(
            r#"begin
                boolean b;
                integer iv;
                real rv;
                OutInt((if b then iv else rv), 4);
            end;"#,
        )
        .unwrap();
    }

    #[test]
    fn remote_array_element_attribute_types_as_text() {
        // simtst65: Outtext(wr.ra2(0, 0).t)
        analyze_source(
            r#"begin
                class A; begin text t; end;
                class W(ra2); ref(A) array ra2; begin end;
                ref(W) wr;
                ref(A) array a(0:0);
                wr :- new W(a);
                OutText(wr.ra2(0, 0).t);
            end;"#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_virtual_procedure_heading_mismatch() {
        let error = analyze_source(
            r#"begin class C;
            virtual: procedure hash is integer procedure hash;
            begin
                integer procedure hash(s); text s;
                begin hash := 100; end;
            end;
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("heading"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accepts_virtual_procedure_heading_match() {
        analyze_source(
            r#"begin class C;
            virtual: procedure hash is integer procedure hash(s); text s;;
            begin
                integer procedure hash(s); text s;
                begin hash := 100; end;
            end;
            end;"#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_prefix_not_local_to_declaring_block() {
        let error = analyze_source(
            r#"begin
            class Outer; begin
                class Nested; begin end;
                Outer class Bad; begin end;
            end;
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("not local"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_this_in_block_prefix() {
        let error = analyze_source(
            r#"begin
            this Point begin end;
            class Point; begin end;
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("this"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_unknown_external_kind() {
        let error = analyze_source(
            r#"begin
            external Fortran procedure sin;
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("unknown external kind"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_non_simula_procedure_as_formal_actual() {
        let error = analyze_source(
            r#"begin
            external C procedure sin = "sin"
               is real procedure sin(x); real x;
            procedure use(f); procedure f;
            begin end;
            use(sin);
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("non-Simula"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accepts_constant_arithmetic_conversion() {
        analyze_source("begin integer n = 3.14; real r = n; end;").unwrap();
    }
}
