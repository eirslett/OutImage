//! Class concatenation (Simula Standard ?5.5.1??5.5.2).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ast::{
    AttributeProtection, Block, ClassDeclaration, Expr, ExprKind, ProcedureDeclaration,
    ProtectionSpec, Specifier, Statement, StatementKind, Variable, VirtualSpec,
};
use crate::error::CompileError;
use crate::types::Type;

/// Name of the fictitious outermost-prefix detach attribute (?5.5.7).
pub const FICTITIOUS_DETACH_NAME: &str = "detach";

/// Whether `name` is the fictitious outermost-prefix detach procedure.
pub fn is_fictitious_detach(name: &str) -> bool {
    name.eq_ignore_ascii_case(FICTITIOUS_DETACH_NAME)
}

/// Resolve and concatenate all class declarations in a module.
pub fn concatenate_classes(
    classes: &[ClassDeclaration],
) -> Result<HashMap<String, ClassDeclaration>, CompileError> {
    concatenate_classes_with_externals(classes, &HashMap::new())
}

pub fn concatenate_classes_with_externals(
    classes: &[ClassDeclaration],
    externals: &HashMap<String, ClassDeclaration>,
) -> Result<HashMap<String, ClassDeclaration>, CompileError> {
    let mut raw: HashMap<String, ClassDeclaration> = externals.clone();
    let external_keys: HashSet<String> = externals
        .keys()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    for class in classes {
        if let Some(existing_name) = raw
            .keys()
            .find(|name| name.eq_ignore_ascii_case(&class.name))
            .cloned()
        {
            // A real local declaration satisfies `external class` from another
            // compilation unit / earlier block — replace the stub instead of
            // reporting a duplicate.
            if external_keys.contains(&existing_name.to_ascii_lowercase()) {
                raw.remove(&existing_name);
                raw.insert(class.name.clone(), class.clone());
                continue;
            }
            // Same simple name in disjoint scopes (e.g. two `SIMSET` blocks each
            // declaring `Link Class A`) is legal Simula. MIR flattens the program
            // into one class map — keep both by span-qualifying the name.
            let mut renamed = class.clone();
            renamed.name = format!("{}@{}", class.name, class.span.start);
            raw.insert(renamed.name.clone(), renamed);
            continue;
        }
        raw.insert(class.name.clone(), class.clone());
    }

    let mut concatenated: HashMap<String, ClassDeclaration> = HashMap::new();
    for class in classes {
        let qualified = format!("{}@{}", class.name, class.span.start);
        let key = if raw.contains_key(&qualified) {
            qualified
        } else {
            raw.keys()
                .find(|name| name.eq_ignore_ascii_case(&class.name))
                .cloned()
                .unwrap_or_else(|| class.name.clone())
        };
        let merged = concatenate_one(&key, &raw, &concatenated, &mut Vec::new())?;
        concatenated.insert(merged.name.clone(), merged);
    }

    Ok(concatenated)
}

fn concatenate_one(
    name: &str,
    raw: &HashMap<String, ClassDeclaration>,
    done: &HashMap<String, ClassDeclaration>,
    stack: &mut Vec<String>,
) -> Result<ClassDeclaration, CompileError> {
    if let Some(existing) = done
        .get(name)
        .or_else(|| find_class_ignore_case(done, name))
    {
        return Ok(existing.clone());
    }

    if stack
        .iter()
        .any(|entry| entry == name || entry.eq_ignore_ascii_case(name))
    {
        return Err(crate::diagnostics::prefix_cycle(
            name,
            raw.get(name)
                .or_else(|| find_class_ignore_case(raw, name))
                .map(|class| class.span.clone()),
        ));
    }

    let Some(class) = raw.get(name).or_else(|| find_class_ignore_case(raw, name)) else {
        return Err(crate::diagnostics::undefined_class(name, None));
    };

    stack.push(name.to_string());

    let mut merged = if let Some(prefix_name) = &class.prefix {
        let prefix = concatenate_one(prefix_name, raw, done, stack)?;
        merge_classes(&prefix, class)?
    } else {
        class.clone()
    };

    merged.prefix = class.prefix.clone();
    merged.name = class.name.clone();
    if merged.protection_map.is_empty() {
        merged.protection_map = build_protection_map(&merged)?;
    }
    if class.prefix.is_none() {
        inject_fictitious_detach_stub(&mut merged)?;
    }

    stack.pop();
    Ok(merged)
}

fn find_class_ignore_case<'a>(
    classes: &'a HashMap<String, ClassDeclaration>,
    name: &str,
) -> Option<&'a ClassDeclaration> {
    classes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, class)| class)
}

/// Inject the fictitious outermost-prefix `procedure detach` stub (?5.5.7).
fn inject_fictitious_detach_stub(class: &mut ClassDeclaration) -> Result<(), CompileError> {
    if !class
        .virtual_part
        .iter()
        .any(|spec| spec.names.iter().any(|name| is_fictitious_detach(name)))
    {
        class.virtual_part.insert(
            0,
            VirtualSpec {
                specifier: Specifier::Procedure,
                names: vec![FICTITIOUS_DETACH_NAME.to_string()],
                procedure_heading: None,
            },
        );
    }

    if find_innermost_procedure_match(class, FICTITIOUS_DETACH_NAME).is_none() {
        class.body.procedures.insert(
            0,
            ProcedureDeclaration {
                result_type: None,
                name: FICTITIOUS_DETACH_NAME.to_string(),
                parameters: Vec::new(),
                body: empty_block(),
                is_external: false,
                identification: None,
                span: 0..0,
            },
        );
    }

    check_virtual_uniqueness(&class.virtual_part)
}

fn empty_block() -> Block {
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

fn merge_classes(
    prefix: &ClassDeclaration,
    main: &ClassDeclaration,
) -> Result<ClassDeclaration, CompileError> {
    // §5.5.2: apply prefix substitutions, then rename main-part conflicts.
    // Protection specs keep source identifiers (`hidden i`) so hide/protect can
    // resolve to the *currently visible* attribute — not a stale conflict rename
    // from an outer prefix (`i`→`i$d` would make `hidden i` in a further subclass
    // re-hide `i$d` instead of the revealed outer `i`; simtst98 `e`/`f`).
    let mut main = main.clone();
    apply_identifier_substitutions_except_protection(&mut main, &prefix.identifier_substitutions);
    let conflict_renames = conflict_renames_for_main(prefix, &main);
    apply_identifier_substitutions(&mut main, &conflict_renames);

    let mut identifier_substitutions = prefix.identifier_substitutions.clone();
    for (from, to) in &conflict_renames {
        identifier_substitutions.insert(from.clone(), to.clone());
    }

    let mut parameters = prefix.parameters.clone();
    parameters.extend(main.parameters.clone());

    let mut specifications = prefix.specifications.clone();
    specifications.extend(main.specifications.clone());

    // A subclass redeclaring a virtual quantity of the same name replaces the
    // prefix's declaration rather than duplicating it (§5.5.3: a virtual
    // quantity of a prefix class may be further specified in a subclass).
    let main_virtual_names: HashSet<String> = main
        .virtual_part
        .iter()
        .flat_map(|spec| spec.names.iter().map(|name| name.to_ascii_lowercase()))
        .collect();
    let mut virtual_part: Vec<VirtualSpec> = prefix
        .virtual_part
        .iter()
        .filter_map(|spec| {
            let names: Vec<String> = spec
                .names
                .iter()
                .filter(|name| !main_virtual_names.contains(&name.to_ascii_lowercase()))
                .cloned()
                .collect();
            if names.is_empty() {
                None
            } else {
                Some(VirtualSpec {
                    names,
                    ..spec.clone()
                })
            }
        })
        .collect();
    virtual_part.extend(main.virtual_part.clone());
    check_virtual_uniqueness(&virtual_part)?;

    let mut protection_part = prefix.protection_part.clone();
    protection_part.extend(main.protection_part.clone());

    let mut protection_map = prefix.protection_map.clone();
    for spec in &main.protection_part {
        apply_protection_spec(&mut protection_map, spec, &main.name)?;
    }

    // §5.5.3: a hidden attribute is invisible in *subclasses* of the hider; the
    // identifier then means whatever it would if that attribute definition were
    // absent (next outer same-named attribute, else the enclosing block). Apply
    // that only to this main part — the hider's own text still sees the attribute.
    let hide_fallthrough =
        inherited_hide_fallthrough_substitutions(&main.name, &protection_map, prefix);
    // Only rewrite the subclass *text* (body / tails). Do not rename protection
    // map keys or parameters — those still name the hidden attribute slots.
    if !hide_fallthrough.is_empty() {
        rewrite_block(&mut main.body, &hide_fallthrough, &HashSet::new());
        for statement in &mut main.tail_statements {
            rewrite_statement(statement, &hide_fallthrough, &HashSet::new());
        }
        for (from, to) in &hide_fallthrough {
            identifier_substitutions.insert(from.clone(), to.clone());
        }
    }

    let main_overrides = main_virtual_overrides(prefix, &main);
    let body = merge_class_bodies(prefix, &main, &main_overrides)?;

    Ok(ClassDeclaration {
        prefix: main.prefix.clone(),
        name: main.name.clone(),
        parameters,
        specifications,
        virtual_part,
        protection_part,
        protection_map,
        body,
        has_inner: main.has_inner || prefix.has_inner,
        inner_label: main.inner_label.clone().or(prefix.inner_label.clone()),
        tail_statements: merge_tail_statements(prefix, &main),
        identifier_substitutions,

        // Preserve the subclass declaration span so `new A` among disjoint
        // same-named classes (e.g. two `SIMSET` blocks) resolves via
        // `find_layout_at` (simtst76).
        span: main.span.clone(),
    })
}

/// Substitutions for attributes hidden by a prefix of `main_name` (§5.5.3).
///
/// Maps the source identifier onto the next outer same-named attribute storage,
/// or onto `__simrt_encl_*` when nothing remains (enclosing block binding).
/// Virtual/procedure names are skipped: hiding those only disables further
/// matching (§5.5.3 note), it does not rewrite calls to an enclosing binding.
///
/// Multiple stacked hides (`e` hides `i$d`, `f` hides `i`) are peeled in one
/// walk so a subclass of `f` resolves to the enclosing capture, not back to `i`.
fn inherited_hide_fallthrough_substitutions(
    main_name: &str,
    protection_map: &BTreeMap<String, AttributeProtection>,
    prefix: &ClassDeclaration,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut bases: HashSet<String> = HashSet::new();
    for key in protection_map.keys() {
        bases.insert(protection_base_name(key).to_string());
    }
    for base in bases {
        if is_virtual_quantity(prefix, &base) || prefix_has_procedure(prefix, &base) {
            continue;
        }
        let initial = prefix
            .identifier_substitutions
            .iter()
            .find(|(from, _)| from.eq_ignore_ascii_case(&base))
            .map(|(_, to)| to.clone())
            .unwrap_or_else(|| base.clone());
        let mut storage = initial.clone();
        for _ in 0..64 {
            let Some(protection) = protection_map.get(&storage).or_else(|| {
                protection_map
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(&storage))
                    .map(|(_, p)| p)
            }) else {
                break;
            };
            if !protection.hidden || protection.defining_class.eq_ignore_ascii_case(main_name) {
                break;
            }
            if is_virtual_quantity(prefix, &storage)
                || prefix_has_procedure(prefix, &storage)
                || is_virtual_quantity(prefix, protection_base_name(&storage))
                || prefix_has_procedure(prefix, protection_base_name(&storage))
            {
                break;
            }
            let next = hide_fallthrough_storage(&storage);
            if next.eq_ignore_ascii_case(&storage) {
                break;
            }
            storage = next;
        }
        if storage != initial {
            out.insert(base.clone(), storage.clone());
            if !initial.eq_ignore_ascii_case(&base) {
                out.insert(initial, storage);
            }
        }
    }
    out
}

/// Storage name that replaces a hidden attribute for subclass text.
///
/// - Hiding a redeclared attribute (`i$d`, `k$b`) restores the base identifier
///   (outer attribute or same-named enclosing capture).
/// - Hiding an attribute whose storage *is* the base name (`i`) leaves no
///   attribute binding, so the enclosing capture is `__simrt_encl_i`.
fn hide_fallthrough_storage(hidden_name: &str) -> String {
    let base = protection_base_name(hidden_name);
    if !hidden_name.eq_ignore_ascii_case(base) {
        return base.to_string();
    }
    format!("__simrt_encl_{}", base.to_ascii_lowercase())
}

fn prefix_has_procedure(class: &ClassDeclaration, name: &str) -> bool {
    class
        .body
        .procedures
        .iter()
        .any(|procedure| procedure.name.eq_ignore_ascii_case(name))
        || class.virtual_part.iter().any(|spec| {
            spec.names
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case(name))
        })
}

/// Apply `from → to` renames to uncommitted identifier occurrences in `class`
/// (§5.5.2.6–2.7). Remote attribute identifiers are not rewritten.
fn apply_identifier_substitutions(
    class: &mut ClassDeclaration,
    substitutions: &BTreeMap<String, String>,
) {
    apply_identifier_substitutions_with(class, substitutions, true);
}

fn apply_identifier_substitutions_except_protection(
    class: &mut ClassDeclaration,
    substitutions: &BTreeMap<String, String>,
) {
    apply_identifier_substitutions_with(class, substitutions, false);
}

fn apply_identifier_substitutions_with(
    class: &mut ClassDeclaration,
    substitutions: &BTreeMap<String, String>,
    rewrite_protection: bool,
) {
    if substitutions.is_empty() {
        return;
    }
    for param in &mut class.parameters {
        rename_string(&mut param.name, substitutions);
    }
    for spec in &mut class.specifications {
        for name in &mut spec.names {
            rename_string(name, substitutions);
        }
    }
    for spec in &mut class.virtual_part {
        for name in &mut spec.names {
            rename_string(name, substitutions);
        }
        if let Some(heading) = &mut spec.procedure_heading {
            rename_string(&mut heading.name, substitutions);
            rewrite_procedure(heading, substitutions, &HashSet::new());
        }
    }
    if rewrite_protection {
        for spec in &mut class.protection_part {
            for name in &mut spec.names {
                rename_string(name, substitutions);
            }
        }
        let old_map = std::mem::take(&mut class.protection_map);
        let mut map = BTreeMap::new();
        for (key, value) in old_map {
            let new_key = substitutions
                .iter()
                .find(|(from, _)| from.eq_ignore_ascii_case(&key))
                .map(|(_, to)| to.clone())
                .unwrap_or(key);
            map.insert(new_key, value);
        }
        class.protection_map = map;
    }

    rewrite_block(&mut class.body, substitutions, &HashSet::new());
    for statement in &mut class.tail_statements {
        rewrite_statement(statement, substitutions, &HashSet::new());
    }
}

fn rename_string(name: &mut String, substitutions: &BTreeMap<String, String>) {
    if let Some(replacement) = substitutions
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(name))
        .map(|(_, to)| to.clone())
    {
        *name = replacement;
    }
}

/// Main-part attribute names that collide with uncommitted prefix occurrences,
/// excluding virtual quantities defined in the prefix (§5.5.2.7).
fn conflict_renames_for_main(
    prefix: &ClassDeclaration,
    main: &ClassDeclaration,
) -> BTreeMap<String, String> {
    let prefix_uncommitted = collect_uncommitted_identifiers(prefix);
    let prefix_virtuals: HashSet<String> = prefix
        .virtual_part
        .iter()
        .flat_map(|spec| spec.names.iter().cloned())
        .collect();
    let main_attrs = collect_defined_attribute_names(main);

    let mut renames = BTreeMap::new();
    for name in &main_attrs {
        if prefix_virtuals
            .iter()
            .any(|virtual_name| virtual_name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        if prefix_uncommitted
            .iter()
            .any(|occurrence| occurrence.eq_ignore_ascii_case(name))
        {
            let renamed = format!("{name}${}", main.name);
            renames.insert(name.clone(), renamed);
        }
    }
    renames
}

fn collect_defined_attribute_names(class: &ClassDeclaration) -> HashSet<String> {
    let mut names = HashSet::new();
    for param in &class.parameters {
        names.insert(param.name.clone());
    }
    for spec in &class.specifications {
        names.extend(spec.names.iter().cloned());
    }
    for declaration in &class.body.declarations {
        for item in &declaration.items {
            names.insert(item.name.clone());
        }
    }
    for array in &class.body.arrays {
        for segment in &array.segments {
            names.extend(segment.names.iter().cloned());
        }
    }
    for procedure in &class.body.procedures {
        names.insert(procedure.name.clone());
    }
    for switch in &class.body.switches {
        names.insert(switch.name.clone());
    }
    for spec in &class.virtual_part {
        names.extend(spec.names.iter().cloned());
    }
    names
}

fn collect_uncommitted_identifiers(class: &ClassDeclaration) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_uncommitted_in_block(&class.body, &HashSet::new(), &mut names);
    for statement in &class.tail_statements {
        collect_uncommitted_in_statement(statement, &HashSet::new(), &mut names);
    }
    // Heading attribute names count as occurrences in the prefix class.
    names.extend(collect_defined_attribute_names(class));
    names
}

fn collect_uncommitted_in_block(
    block: &Block,
    outer_locals: &HashSet<String>,
    names: &mut HashSet<String>,
) {
    let mut locals = outer_locals.clone();
    for declaration in &block.declarations {
        for item in &declaration.items {
            locals.insert(item.name.clone());
        }
    }
    for array in &block.arrays {
        for segment in &array.segments {
            for name in &segment.names {
                locals.insert(name.clone());
            }
        }
    }
    for procedure in &block.procedures {
        locals.insert(procedure.name.clone());
        collect_uncommitted_in_procedure(procedure, &locals, names);
    }
    for switch in &block.switches {
        locals.insert(switch.name.clone());
    }
    for statement in &block.statements {
        collect_uncommitted_in_statement(statement, &locals, names);
    }
    for inner in &block.body {
        collect_uncommitted_in_block(inner, &locals, names);
    }
}

fn collect_uncommitted_in_procedure(
    procedure: &ProcedureDeclaration,
    outer_locals: &HashSet<String>,
    names: &mut HashSet<String>,
) {
    let mut locals = outer_locals.clone();
    for param in &procedure.parameters {
        locals.insert(param.name.clone());
    }
    collect_uncommitted_in_block(&procedure.body, &locals, names);
}

fn collect_uncommitted_in_statement(
    statement: &Statement,
    locals: &HashSet<String>,
    names: &mut HashSet<String>,
) {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => {
            collect_uncommitted_in_statement(statement, locals, names);
        }
        StatementKind::Compound(block) => collect_uncommitted_in_block(block, locals, names),
        StatementKind::Assignment(assignment) => {
            collect_uncommitted_in_variable(&assignment.lhs, locals, names);
            match &assignment.rhs {
                crate::ast::AssignmentRhs::Expr(expr) => {
                    collect_uncommitted_in_expr(expr, locals, names);
                }
                crate::ast::AssignmentRhs::Chain(inner) => {
                    collect_uncommitted_in_variable(&inner.lhs, locals, names);
                    match &inner.rhs {
                        crate::ast::AssignmentRhs::Expr(expr) => {
                            collect_uncommitted_in_expr(expr, locals, names)
                        }
                        crate::ast::AssignmentRhs::Chain(_) => {}
                    }
                }
            }
        }
        StatementKind::ProcedureCall(call) => {
            if !locals
                .iter()
                .any(|local| local.eq_ignore_ascii_case(&call.name))
            {
                names.insert(call.name.clone());
            }
            for argument in &call.arguments {
                collect_uncommitted_in_expr(argument, locals, names);
            }
        }
        StatementKind::Expr(expr) => collect_uncommitted_in_expr(expr, locals, names),
        StatementKind::If(if_stmt) => {
            collect_uncommitted_in_expr(&if_stmt.condition, locals, names);
            collect_uncommitted_in_statement(&if_stmt.then_branch, locals, names);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_uncommitted_in_statement(else_branch, locals, names);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_uncommitted_in_expr(&while_stmt.condition, locals, names);
            collect_uncommitted_in_statement(&while_stmt.body, locals, names);
        }
        StatementKind::For(for_stmt) => {
            if !locals
                .iter()
                .any(|local| local.eq_ignore_ascii_case(&for_stmt.variable))
            {
                names.insert(for_stmt.variable.clone());
            }
            for element in &for_stmt.elements {
                collect_uncommitted_in_for_element(element, locals, names);
            }
            collect_uncommitted_in_statement(&for_stmt.body, locals, names);
        }
        StatementKind::Goto(goto_stmt) => {
            collect_uncommitted_in_designational(&goto_stmt.target, locals, names);
        }
        StatementKind::Inspect(inspect) => {
            collect_uncommitted_in_expr(&inspect.object, locals, names);
            for when in &inspect.when_clauses {
                collect_uncommitted_in_statement(&when.body, locals, names);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_uncommitted_in_statement(do_clause, locals, names);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_uncommitted_in_statement(otherwise, locals, names);
            }
        }
        StatementKind::Activate(activate) => {
            collect_uncommitted_in_expr(&activate.target, locals, names);
        }
        StatementKind::Reactivate(reactivate) => {
            collect_uncommitted_in_expr(&reactivate.target, locals, names);
        }
        StatementKind::ObjectGenerator(generator) => {
            for argument in &generator.arguments {
                collect_uncommitted_in_expr(argument, locals, names);
            }
        }
        StatementKind::Dummy | StatementKind::Inner { .. } => {}
    }
}

fn collect_uncommitted_in_for_element(
    element: &crate::ast::ForListElement,
    locals: &HashSet<String>,
    names: &mut HashSet<String>,
) {
    match element {
        crate::ast::ForListElement::Value { expr, while_cond }
        | crate::ast::ForListElement::Reference { expr, while_cond } => {
            collect_uncommitted_in_expr(expr, locals, names);
            if let Some(cond) = while_cond {
                collect_uncommitted_in_expr(cond, locals, names);
            }
        }
        crate::ast::ForListElement::StepUntil { start, step, until } => {
            collect_uncommitted_in_expr(start, locals, names);
            collect_uncommitted_in_expr(step, locals, names);
            collect_uncommitted_in_expr(until, locals, names);
        }
    }
}

fn collect_uncommitted_in_variable(
    variable: &Variable,
    locals: &HashSet<String>,
    names: &mut HashSet<String>,
) {
    match variable {
        Variable::Simple(name) => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                names.insert(name.clone());
            }
        }
        Variable::Subscripted { name, subscripts } => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                names.insert(name.clone());
            }
            for subscript in subscripts {
                collect_uncommitted_in_expr(subscript, locals, names);
            }
        }
        Variable::Qua { object, .. }
        | Variable::Remote { object, .. }
        | Variable::RemoteCall { object, .. } => {
            // Attribute identifier of a remote id is committed (§5.5.2).
            collect_uncommitted_in_variable(object, locals, names);
            if let Variable::RemoteCall { arguments, .. } = variable {
                for argument in arguments {
                    collect_uncommitted_in_expr(argument, locals, names);
                }
            }
        }
    }
}

fn collect_uncommitted_in_expr(expr: &Expr, locals: &HashSet<String>, names: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Variable(variable) => collect_uncommitted_in_variable(variable, locals, names),
        ExprKind::Unary { operand, .. } => collect_uncommitted_in_expr(operand, locals, names),
        ExprKind::Binary { left, right, .. } => {
            collect_uncommitted_in_expr(left, locals, names);
            collect_uncommitted_in_expr(right, locals, names);
        }
        ExprKind::Relation { left, right, .. } => {
            collect_uncommitted_in_expr(left, locals, names);
            collect_uncommitted_in_expr(right, locals, names);
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_uncommitted_in_expr(condition, locals, names);
            collect_uncommitted_in_expr(then_expr, locals, names);
            collect_uncommitted_in_expr(else_expr, locals, names);
        }
        ExprKind::Paren(inner) => collect_uncommitted_in_expr(inner, locals, names),
        ExprKind::FunctionCall { name, arguments } => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                names.insert(name.clone());
            }
            for argument in arguments {
                collect_uncommitted_in_expr(argument, locals, names);
            }
        }
        ExprKind::RemoteAccess { object, .. } => {
            collect_uncommitted_in_expr(object, locals, names);
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            collect_uncommitted_in_expr(object, locals, names);
            for argument in arguments {
                collect_uncommitted_in_expr(argument, locals, names);
            }
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    collect_uncommitted_in_expr(argument, locals, names);
                }
            }
        }
        ExprKind::Qua { object, .. } => collect_uncommitted_in_expr(object, locals, names),
        ExprKind::This(_)
        | ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None => {}
    }
}

fn collect_uncommitted_in_designational(
    expr: &crate::ast::DesignationalExpr,
    locals: &HashSet<String>,
    names: &mut HashSet<String>,
) {
    match expr {
        crate::ast::DesignationalExpr::Label(label) => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(label)) {
                names.insert(label.clone());
            }
        }
        crate::ast::DesignationalExpr::SwitchDesignator { name, subscript } => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                names.insert(name.clone());
            }
            collect_uncommitted_in_expr(subscript, locals, names);
        }
        crate::ast::DesignationalExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_uncommitted_in_expr(condition, locals, names);
            collect_uncommitted_in_designational(then_expr, locals, names);
            collect_uncommitted_in_designational(else_expr, locals, names);
        }
        crate::ast::DesignationalExpr::Paren(inner) => {
            collect_uncommitted_in_designational(inner, locals, names);
        }
    }
}

fn rewrite_block(
    block: &mut Block,
    substitutions: &BTreeMap<String, String>,
    outer_locals: &HashSet<String>,
) {
    let mut locals = outer_locals.clone();
    for declaration in &mut block.declarations {
        for item in &mut declaration.items {
            if !outer_locals
                .iter()
                .any(|local| local.eq_ignore_ascii_case(&item.name))
            {
                rename_string(&mut item.name, substitutions);
            }
            locals.insert(item.name.clone());
            if let Some(initializer) = &mut item.initializer {
                rewrite_expr(initializer, substitutions, &locals);
            }
        }
    }
    for array in &mut block.arrays {
        for segment in &mut array.segments {
            for name in &mut segment.names {
                if !outer_locals
                    .iter()
                    .any(|local| local.eq_ignore_ascii_case(name))
                {
                    rename_string(name, substitutions);
                }
                locals.insert(name.clone());
            }
            for bound in &mut segment.bounds {
                rewrite_expr(&mut bound.lower, substitutions, &locals);
                rewrite_expr(&mut bound.upper, substitutions, &locals);
            }
        }
    }
    for procedure in &mut block.procedures {
        if !outer_locals
            .iter()
            .any(|local| local.eq_ignore_ascii_case(&procedure.name))
        {
            rename_string(&mut procedure.name, substitutions);
        }
        locals.insert(procedure.name.clone());
        rewrite_procedure(procedure, substitutions, &locals);
    }
    for switch in &mut block.switches {
        if !outer_locals
            .iter()
            .any(|local| local.eq_ignore_ascii_case(&switch.name))
        {
            rename_string(&mut switch.name, substitutions);
        }
        locals.insert(switch.name.clone());
        for element in &mut switch.elements {
            rewrite_designational(element, substitutions, &locals);
        }
    }
    for statement in &mut block.statements {
        rewrite_statement(statement, substitutions, &locals);
    }
    for inner in &mut block.body {
        rewrite_block(inner, substitutions, &locals);
    }
}

fn rewrite_procedure(
    procedure: &mut ProcedureDeclaration,
    substitutions: &BTreeMap<String, String>,
    outer_locals: &HashSet<String>,
) {
    let mut locals = outer_locals.clone();
    for param in &mut procedure.parameters {
        locals.insert(param.name.clone());
    }
    rewrite_block(&mut procedure.body, substitutions, &locals);
}

fn rewrite_statement(
    statement: &mut Statement,
    substitutions: &BTreeMap<String, String>,
    locals: &HashSet<String>,
) {
    match &mut statement.kind {
        StatementKind::Labeled { statement, .. } => {
            rewrite_statement(statement, substitutions, locals);
        }
        StatementKind::Compound(block) => rewrite_block(block, substitutions, locals),
        StatementKind::Assignment(assignment) => {
            rewrite_variable(&mut assignment.lhs, substitutions, locals);
            match &mut assignment.rhs {
                crate::ast::AssignmentRhs::Expr(expr) => {
                    rewrite_expr(expr, substitutions, locals);
                }
                crate::ast::AssignmentRhs::Chain(inner) => {
                    rewrite_variable(&mut inner.lhs, substitutions, locals);
                    match &mut inner.rhs {
                        crate::ast::AssignmentRhs::Expr(expr) => {
                            rewrite_expr(expr, substitutions, locals)
                        }
                        crate::ast::AssignmentRhs::Chain(_) => {}
                    }
                }
            }
        }
        StatementKind::ProcedureCall(call) => {
            if !locals
                .iter()
                .any(|local| local.eq_ignore_ascii_case(&call.name))
            {
                rename_string(&mut call.name, substitutions);
            }
            for argument in &mut call.arguments {
                rewrite_expr(argument, substitutions, locals);
            }
        }
        StatementKind::Expr(expr) => rewrite_expr(expr, substitutions, locals),
        StatementKind::If(if_stmt) => {
            rewrite_expr(&mut if_stmt.condition, substitutions, locals);
            rewrite_statement(&mut if_stmt.then_branch, substitutions, locals);
            if let Some(else_branch) = &mut if_stmt.else_branch {
                rewrite_statement(else_branch, substitutions, locals);
            }
        }
        StatementKind::While(while_stmt) => {
            rewrite_expr(&mut while_stmt.condition, substitutions, locals);
            rewrite_statement(&mut while_stmt.body, substitutions, locals);
        }
        StatementKind::For(for_stmt) => {
            if !locals
                .iter()
                .any(|local| local.eq_ignore_ascii_case(&for_stmt.variable))
            {
                rename_string(&mut for_stmt.variable, substitutions);
            }
            for element in &mut for_stmt.elements {
                rewrite_for_element(element, substitutions, locals);
            }
            rewrite_statement(&mut for_stmt.body, substitutions, locals);
        }
        StatementKind::Goto(goto_stmt) => {
            rewrite_designational(&mut goto_stmt.target, substitutions, locals);
        }
        StatementKind::Inspect(inspect) => {
            rewrite_expr(&mut inspect.object, substitutions, locals);
            for when in &mut inspect.when_clauses {
                rewrite_statement(&mut when.body, substitutions, locals);
            }
            if let Some(do_clause) = &mut inspect.do_clause {
                rewrite_statement(do_clause, substitutions, locals);
            }
            if let Some(otherwise) = &mut inspect.otherwise {
                rewrite_statement(otherwise, substitutions, locals);
            }
        }
        StatementKind::Activate(activate) => {
            rewrite_expr(&mut activate.target, substitutions, locals);
        }
        StatementKind::Reactivate(reactivate) => {
            rewrite_expr(&mut reactivate.target, substitutions, locals);
        }
        StatementKind::ObjectGenerator(generator) => {
            for argument in &mut generator.arguments {
                rewrite_expr(argument, substitutions, locals);
            }
        }
        StatementKind::Dummy | StatementKind::Inner { .. } => {}
    }
}

fn rewrite_for_element(
    element: &mut crate::ast::ForListElement,
    substitutions: &BTreeMap<String, String>,
    locals: &HashSet<String>,
) {
    match element {
        crate::ast::ForListElement::Value { expr, while_cond }
        | crate::ast::ForListElement::Reference { expr, while_cond } => {
            rewrite_expr(expr, substitutions, locals);
            if let Some(cond) = while_cond {
                rewrite_expr(cond, substitutions, locals);
            }
        }
        crate::ast::ForListElement::StepUntil { start, step, until } => {
            rewrite_expr(start, substitutions, locals);
            rewrite_expr(step, substitutions, locals);
            rewrite_expr(until, substitutions, locals);
        }
    }
}

fn rewrite_variable(
    variable: &mut Variable,
    substitutions: &BTreeMap<String, String>,
    locals: &HashSet<String>,
) {
    match variable {
        Variable::Simple(name) => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                rename_string(name, substitutions);
            }
        }
        Variable::Subscripted { name, subscripts } => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                rename_string(name, substitutions);
            }
            for subscript in subscripts {
                rewrite_expr(subscript, substitutions, locals);
            }
        }
        Variable::Qua { object, .. } => {
            rewrite_variable(object, substitutions, locals);
        }
        Variable::Remote { object, .. } => {
            rewrite_variable(object, substitutions, locals);
        }
        Variable::RemoteCall {
            object, arguments, ..
        } => {
            rewrite_variable(object, substitutions, locals);
            for argument in arguments {
                rewrite_expr(argument, substitutions, locals);
            }
        }
    }
}

fn rewrite_expr(
    expr: &mut Expr,
    substitutions: &BTreeMap<String, String>,
    locals: &HashSet<String>,
) {
    match &mut expr.kind {
        ExprKind::Variable(variable) => rewrite_variable(variable, substitutions, locals),
        ExprKind::Unary { operand, .. } => rewrite_expr(operand, substitutions, locals),
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            rewrite_expr(left, substitutions, locals);
            rewrite_expr(right, substitutions, locals);
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_expr(condition, substitutions, locals);
            rewrite_expr(then_expr, substitutions, locals);
            rewrite_expr(else_expr, substitutions, locals);
        }
        ExprKind::Paren(inner) => rewrite_expr(inner, substitutions, locals),
        ExprKind::FunctionCall { name, arguments } => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                rename_string(name, substitutions);
            }
            for argument in arguments {
                rewrite_expr(argument, substitutions, locals);
            }
        }
        ExprKind::RemoteAccess { object, .. } => {
            rewrite_expr(object, substitutions, locals);
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            rewrite_expr(object, substitutions, locals);
            for argument in arguments {
                rewrite_expr(argument, substitutions, locals);
            }
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    rewrite_expr(argument, substitutions, locals);
                }
            }
        }
        ExprKind::Qua { object, .. } => rewrite_expr(object, substitutions, locals),
        ExprKind::This(_)
        | ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::Notext
        | ExprKind::NumberLiteral { .. }
        | ExprKind::None => {}
    }
}

fn rewrite_designational(
    expr: &mut crate::ast::DesignationalExpr,
    substitutions: &BTreeMap<String, String>,
    locals: &HashSet<String>,
) {
    match expr {
        crate::ast::DesignationalExpr::Label(label) => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(label)) {
                rename_string(label, substitutions);
            }
        }
        crate::ast::DesignationalExpr::SwitchDesignator { name, subscript } => {
            if !locals.iter().any(|local| local.eq_ignore_ascii_case(name)) {
                rename_string(name, substitutions);
            }
            rewrite_expr(subscript, substitutions, locals);
        }
        crate::ast::DesignationalExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_expr(condition, substitutions, locals);
            rewrite_designational(then_expr, substitutions, locals);
            rewrite_designational(else_expr, substitutions, locals);
        }
        crate::ast::DesignationalExpr::Paren(inner) => {
            rewrite_designational(inner, substitutions, locals);
        }
    }
}

/// Resolve a remote attribute name through the access-level class's
/// identifier substitutions (§5.5.6.6).
///
/// Follows the substitution chain to a fixed point so three-level shadowing
/// (`A.i` / `B.i`→`i$B` / `C.i`→`i$B$C`) resolves `C.i` to the C-level field.
pub fn substitute_remote_attribute(
    access_class: &str,
    attribute: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> String {
    let Some(class) = classes.get(access_class).or_else(|| {
        classes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(access_class))
            .map(|(_, class)| class)
    }) else {
        return attribute.to_string();
    };
    let mut name = attribute.to_string();
    for _ in 0..64 {
        let Some(next) = class
            .identifier_substitutions
            .iter()
            .find(|(from, _)| from.eq_ignore_ascii_case(&name))
            .map(|(_, to)| to.clone())
        else {
            break;
        };
        if next.eq_ignore_ascii_case(&name) {
            break;
        }
        name = next;
    }
    name
}

/// Storage name for remote access that skips protected/hidden attributes the
/// caller cannot see, falling through the prefix chain (§5.5.4 / §5.5.6.5).
///
/// Example (simtst60): `ref(B) xb; xb.i` when `B` redeclares protected `i`
/// resolves to prefix `A`'s `i`, not `i$B`.
pub fn accessible_remote_storage_name(
    object_class: &str,
    attribute: &str,
    access_class: Option<&str>,
    classes: &HashMap<String, ClassDeclaration>,
) -> String {
    let storage = substitute_remote_attribute(object_class, attribute, classes);
    let Some(merged) = classes.get(object_class).or_else(|| {
        classes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(object_class))
            .map(|(_, class)| class)
    }) else {
        return storage;
    };
    let Some(protection) = merged
        .protection_map
        .get(attribute)
        .or_else(|| merged.protection_map.get(&storage))
    else {
        return storage;
    };

    let blocked = (protection.protected
        && !in_protection_hierarchy(access_class, protection, classes))
        || access_class.is_some_and(|access| is_hidden_from(access, protection, classes));
    if !blocked {
        return storage;
    }

    let chain = prefix_chain_ordered(object_class, classes);
    for level in chain.iter().rev().skip(1) {
        let Some(level_class) = classes.get(level).or_else(|| {
            classes
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(level))
                .map(|(_, class)| class)
        }) else {
            continue;
        };
        let level_storage = substitute_remote_attribute(level, attribute, classes);
        match level_class
            .protection_map
            .get(attribute)
            .or_else(|| level_class.protection_map.get(&level_storage))
        {
            Some(level_protection) => {
                if level_protection.protected
                    && !in_protection_hierarchy(access_class, level_protection, classes)
                {
                    continue;
                }
                if access_class
                    .is_some_and(|access| is_hidden_from(access, level_protection, classes))
                {
                    continue;
                }
                return level_storage;
            }
            None => {
                if find_attribute_in_raw_class(level_class, &level_storage).is_some()
                    || find_attribute_in_raw_class(level_class, attribute).is_some()
                {
                    return level_storage;
                }
            }
        }
    }
    storage
}

/// Like [`accessible_remote_storage_name`], but returns `None` when every prefix
/// level is blocked for the caller (so bare-name lookup can fall through to an
/// enclosing binding instead of forcing the protected storage).
pub fn try_accessible_remote_storage_name(
    object_class: &str,
    attribute: &str,
    access_class: Option<&str>,
    classes: &HashMap<String, ClassDeclaration>,
) -> Option<String> {
    let storage = substitute_remote_attribute(object_class, attribute, classes);
    let Some(merged) = classes.get(object_class).or_else(|| {
        classes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(object_class))
            .map(|(_, class)| class)
    }) else {
        return Some(storage);
    };
    let Some(protection) = merged
        .protection_map
        .get(attribute)
        .or_else(|| merged.protection_map.get(&storage))
    else {
        return Some(storage);
    };

    let blocked = (protection.protected
        && !in_protection_hierarchy(access_class, protection, classes))
        || access_class.is_some_and(|access| is_hidden_from(access, protection, classes));
    if !blocked {
        return Some(storage);
    }

    let chain = prefix_chain_ordered(object_class, classes);
    for level in chain.iter().rev().skip(1) {
        let Some(level_class) = classes.get(level).or_else(|| {
            classes
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(level))
                .map(|(_, class)| class)
        }) else {
            continue;
        };
        let level_storage = substitute_remote_attribute(level, attribute, classes);
        match level_class
            .protection_map
            .get(attribute)
            .or_else(|| level_class.protection_map.get(&level_storage))
        {
            Some(level_protection) => {
                if level_protection.protected
                    && !in_protection_hierarchy(access_class, level_protection, classes)
                {
                    continue;
                }
                if access_class
                    .is_some_and(|access| is_hidden_from(access, level_protection, classes))
                {
                    continue;
                }
                return Some(level_storage);
            }
            None => {
                if find_attribute_in_raw_class(level_class, &level_storage).is_some()
                    || find_attribute_in_raw_class(level_class, attribute).is_some()
                {
                    return Some(level_storage);
                }
            }
        }
    }
    None
}

/// Program text position an attribute is accessed from (§5.5.5–§5.5.6).
///
/// Protection and hiding are judged from the *text* being compiled, never from
/// the object an `inspect` happens to be connected to (simtst98: `inspect xa do
/// i` sees the global `i` because the connection block's text is outside `a`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AccessLevel<'a> {
    /// Innermost class whose body/procedure text is being compiled; `None`
    /// outside every class.
    pub class: Option<&'a str>,
    /// The text is a block prefixed by [`Self::class`], i.e. a fictitious inner
    /// prefix level — so that class's own `hidden` specifications apply (§4.10.1).
    pub prefixed_block: bool,
}

impl<'a> AccessLevel<'a> {
    pub fn outside() -> Self {
        Self {
            class: None,
            prefixed_block: false,
        }
    }

    pub fn class_text(class: &'a str) -> Self {
        Self {
            class: Some(class),
            prefixed_block: false,
        }
    }
}

/// What an attribute identifier denotes at the prefix level that declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeKind {
    /// Declared in a `virtual` part: calls dispatch on the runtime class.
    Virtual,
    Procedure,
    Variable,
}

/// The prefix level an attribute identifier binds to, and what it denotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeBinding {
    pub level: String,
    pub kind: AttributeKind,
}

/// Prefix chain of `class_name` (outermost first), matched case-insensitively.
fn raw_prefix_chain(class_name: &str, classes: &HashMap<String, ClassDeclaration>) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = Some(class_name.to_string());
    while let Some(name) = current {
        if chain
            .iter()
            .any(|seen: &String| seen.eq_ignore_ascii_case(&name))
        {
            break;
        }
        let Some(class) = find_class_ignore_case_or_exact(classes, &name) else {
            chain.push(name);
            break;
        };
        chain.push(class.name.clone());
        current = class.prefix.clone();
    }
    chain.reverse();
    chain
}

fn find_class_ignore_case_or_exact<'a>(
    classes: &'a HashMap<String, ClassDeclaration>,
    name: &str,
) -> Option<&'a ClassDeclaration> {
    classes
        .get(name)
        .or_else(|| find_class_ignore_case(classes, name))
}

fn names_contain(names: &[String], name: &str) -> bool {
    names.iter().any(|entry| entry.eq_ignore_ascii_case(name))
}

/// What prefix level `class` declares for `name`, if anything. A virtual
/// specification wins over a body procedure of the same name: that procedure is
/// the virtual quantity's *match*, not a separate attribute (§5.5.3).
fn declared_attribute_kind(class: &ClassDeclaration, name: &str) -> Option<AttributeKind> {
    if class
        .virtual_part
        .iter()
        .any(|spec| names_contain(&spec.names, name))
    {
        return Some(AttributeKind::Virtual);
    }
    if class
        .body
        .procedures
        .iter()
        .any(|procedure| procedure.name.eq_ignore_ascii_case(name))
    {
        return Some(AttributeKind::Procedure);
    }
    let is_variable = class
        .parameters
        .iter()
        .any(|param| param.name.eq_ignore_ascii_case(name))
        || class
            .specifications
            .iter()
            .any(|spec| names_contain(&spec.names, name))
        || class
            .body
            .declarations
            .iter()
            .flat_map(|declaration| declaration.items.iter())
            .any(|item| item.name.eq_ignore_ascii_case(name))
        || class
            .body
            .arrays
            .iter()
            .flat_map(|array| array.segments.iter())
            .any(|segment| names_contain(&segment.names, name))
        || class
            .body
            .switches
            .iter()
            .any(|switch| switch.name.eq_ignore_ascii_case(name));
    is_variable.then_some(AttributeKind::Variable)
}

fn hidden_count_at(class: &ClassDeclaration, name: &str) -> usize {
    class
        .protection_part
        .iter()
        .filter(|spec| spec.hidden && names_contain(&spec.names, name))
        .count()
}

fn protected_at(class: &ClassDeclaration, name: &str) -> bool {
    class
        .protection_part
        .iter()
        .any(|spec| spec.protected && names_contain(&spec.names, name))
}

/// Whether the accessing text is the class body of `level` or of one of its
/// subclasses — the only texts a `protected` attribute is visible in (§5.5.4).
fn access_at_or_inside(
    access: AccessLevel<'_>,
    level: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    access.class.is_some_and(|class| {
        class.eq_ignore_ascii_case(level) || is_subclass_of(class, level, classes)
    })
}

/// Whether a `hidden` specification at `level` applies to the accessing text.
/// The hider's own class body still sees the attribute it hid; its subclasses,
/// prefixed blocks and connection blocks do not (§5.5.3).
fn hides_apply_at(access: AccessLevel<'_>, level: &str) -> bool {
    match access.class {
        Some(class) if class.eq_ignore_ascii_case(level) => access.prefixed_block,
        _ => true,
    }
}

/// Stack of same-named attribute bindings along `walk` (outermost first),
/// after applying `hidden` peels and marking protection-blocked entries.
fn binding_stack(
    walk: &[String],
    name: &str,
    access: AccessLevel<'_>,
    classes: &HashMap<String, ClassDeclaration>,
) -> Vec<(AttributeBinding, bool)> {
    let mut stack: Vec<(AttributeBinding, bool)> = Vec::new();
    for level in walk {
        let Some(class) = find_class_ignore_case_or_exact(classes, level) else {
            continue;
        };
        let declared = declared_attribute_kind(class, name);
        if let Some(kind) = declared {
            // A declaration at a level where a virtual quantity of that name is
            // visible is that quantity's match, not a new binding (§5.5.3).
            let matches_visible_virtual = kind != AttributeKind::Virtual
                && stack
                    .last()
                    .is_some_and(|(binding, _)| binding.kind == AttributeKind::Virtual);
            if !matches_visible_virtual {
                let blocked =
                    protected_at(class, name) && !access_at_or_inside(access, level, classes);
                stack.push((
                    AttributeBinding {
                        level: class.name.clone(),
                        kind,
                    },
                    blocked,
                ));
            }
        } else if protected_at(class, name)
            && !access_at_or_inside(access, level, classes)
            && let Some(top) = stack.last_mut()
        {
            // §5.5.4: an inherited attribute may be protected at an inner level.
            top.1 = true;
        }
        if hides_apply_at(access, level) {
            for _ in 0..hidden_count_at(class, name) {
                stack.pop();
            }
        }
    }
    stack
}

/// Prefix level `name` binds to when accessed from `access` on an object
/// qualified by `qual`, or `None` when no attribute of that name is visible
/// there (the identifier then means whatever it does in the enclosing block).
pub fn visible_attribute_binding(
    access: AccessLevel<'_>,
    qual: &str,
    name: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> Option<AttributeBinding> {
    let qual_chain = raw_prefix_chain(qual, classes);
    // Visibility follows the accessing text's prefix level when that text
    // belongs to the same prefix family; otherwise only the qualification's
    // own levels are in scope.
    let same_family = access.class.is_some_and(|class| {
        qual_chain
            .iter()
            .any(|level| level.eq_ignore_ascii_case(class))
            || raw_prefix_chain(class, classes)
                .iter()
                .any(|level| level.eq_ignore_ascii_case(qual))
    });
    let walk = match access.class {
        Some(class) if same_family => raw_prefix_chain(class, classes),
        _ => qual_chain.clone(),
    };
    let stack = binding_stack(&walk, name, access, classes);
    stack
        .into_iter()
        .rev()
        .find(|(binding, blocked)| {
            !blocked
                && qual_chain
                    .iter()
                    .any(|level| level.eq_ignore_ascii_case(&binding.level))
        })
        .map(|(binding, _)| binding)
}

/// Whether any prefix level of `qual` declares an attribute called `name`.
/// Distinguishes "not an attribute at all" (enclosing-block capture, compiler
/// slot) from "an attribute that is invisible here".
pub fn attribute_declared_in_prefix_chain(
    qual: &str,
    name: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    raw_prefix_chain(qual, classes).iter().any(|level| {
        find_class_ignore_case_or_exact(classes, level)
            .and_then(|class| declared_attribute_kind(class, name))
            .is_some()
    })
}

/// Prefix level whose declaration of `name` matches the virtual quantity
/// declared at `virtual_level`, for an object of class `runtime_class`.
///
/// §5.5.3: a `hidden` virtual quantity admits no further matching in subclasses
/// of the hider, and a subclass redeclaring `virtual: … name` starts a new
/// quantity — so the innermost level whose own text still sees *this* quantity
/// wins (simtst98: `a`'s virtual keeps `b`'s match for `c`…`z`).
pub fn virtual_match_level(
    runtime_class: &str,
    virtual_level: &str,
    name: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> Option<String> {
    let chain = raw_prefix_chain(runtime_class, classes);
    let start = chain
        .iter()
        .position(|level| level.eq_ignore_ascii_case(virtual_level))?;
    for level in chain[start..].iter().rev() {
        let Some(class) = find_class_ignore_case_or_exact(classes, level) else {
            continue;
        };
        if !class
            .body
            .procedures
            .iter()
            .any(|procedure| procedure.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let own_view =
            visible_attribute_binding(AccessLevel::class_text(level), level, name, classes);
        match own_view {
            Some(binding)
                if binding.kind == AttributeKind::Virtual
                    && binding.level.eq_ignore_ascii_case(virtual_level) =>
            {
                return Some(class.name.clone());
            }
            _ => {}
        }
    }
    None
}

/// Declared spelling of the procedure attribute `name` at prefix level `level`.
pub fn declared_procedure_name(
    level: &str,
    name: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> Option<String> {
    find_class_ignore_case_or_exact(classes, level)?
        .body
        .procedures
        .iter()
        .rev()
        .find(|procedure| procedure.name.eq_ignore_ascii_case(name))
        .map(|procedure| procedure.name.clone())
}

/// Names of virtual quantities that the main part redefines with a matching attribute.
fn main_virtual_overrides(prefix: &ClassDeclaration, main: &ClassDeclaration) -> HashSet<String> {
    let mut overrides = HashSet::new();
    for spec in &prefix.virtual_part {
        for name in &spec.names {
            if find_attribute_in_block(&main.body, name).is_some() {
                overrides.insert(name.clone());
            }
        }
    }
    for spec in &main.virtual_part {
        for name in &spec.names {
            if find_attribute_in_block(&main.body, name).is_some() {
                overrides.insert(name.clone());
            }
        }
    }
    overrides
}

fn find_attribute_in_block(block: &Block, name: &str) -> Option<()> {
    find_innermost_in_block(block, name).map(|_| ())
}

fn merge_class_bodies(
    prefix: &ClassDeclaration,
    main: &ClassDeclaration,
    main_overrides: &HashSet<String>,
) -> Result<Block, CompileError> {
    let (prefix_initial, _prefix_final) = split_body_statements(prefix);
    let (main_initial, _main_final) = split_body_statements(main);

    let mut body = main.body.clone();

    let mut prefix_decls =
        filter_overridden_declarations(&prefix.body.declarations, main_overrides);
    prefix_decls.extend(main.body.declarations.clone());
    body.declarations = prefix_decls;

    let mut prefix_arrays = filter_overridden_arrays(&prefix.body.arrays, main_overrides);
    prefix_arrays.extend(main.body.arrays.clone());
    body.arrays = prefix_arrays;

    let mut prefix_procedures =
        filter_overridden_procedures(&prefix.body.procedures, main_overrides);
    prefix_procedures.extend(main.body.procedures.clone());
    body.procedures = prefix_procedures;

    let mut switch_overrides = main_overrides.clone();
    for switch in &main.body.switches {
        switch_overrides.insert(switch.name.clone());
    }
    let mut prefix_switches = filter_overridden_switches(&prefix.body.switches, &switch_overrides);
    prefix_switches.extend(main.body.switches.clone());
    body.switches = prefix_switches;

    body.classes.extend(prefix.body.classes.clone());

    let mut initial = prefix_initial;
    initial.extend(main_initial);
    body.statements = initial;

    Ok(body)
}

/// Split a class body into initial and final statement sections around `inner`.
/// Non-split bodies (?5.5.2.9): all statements are initial, final is empty.
fn split_body_statements(class: &ClassDeclaration) -> (Vec<Statement>, Vec<Statement>) {
    if class.has_inner {
        (class.body.statements.clone(), class.tail_statements.clone())
    } else {
        (class.body.statements.clone(), Vec::new())
    }
}

/// Prefixed block as an additional main part of `prefix` (§4.10.1).
///
/// Returns `(initial, final)` statement lists: prefix initials, then the
/// block's statements (at `inner` when the prefix is split), then prefix
/// finals. Callers lower both lists in one CFG so virtual labels in the
/// block can match `goto` from the prefix body.
pub fn prefixed_block_statements(
    prefix: &ClassDeclaration,
    block: &Block,
) -> (Vec<Statement>, Vec<Statement>) {
    let (mut initial, finals) = split_body_statements(prefix);
    initial.extend(block.statements.iter().cloned());
    (initial, finals)
}

fn merge_tail_statements(prefix: &ClassDeclaration, main: &ClassDeclaration) -> Vec<Statement> {
    let (_, prefix_final) = split_body_statements(prefix);
    let (_, main_final) = split_body_statements(main);
    let mut tail = main_final;
    tail.extend(prefix_final);
    tail
}

fn filter_overridden_declarations(
    declarations: &[crate::types::Declaration],
    overrides: &HashSet<String>,
) -> Vec<crate::types::Declaration> {
    declarations
        .iter()
        .filter_map(|decl| {
            let items: Vec<_> = decl
                .items
                .iter()
                .filter(|item| !overrides.contains(&item.name))
                .cloned()
                .collect();
            if items.is_empty() {
                None
            } else {
                Some(crate::types::Declaration {
                    ty: decl.ty.clone(),
                    items,

                    span: 0..0,
                })
            }
        })
        .collect()
}

fn filter_overridden_arrays(
    arrays: &[crate::ast::ArrayDeclaration],
    overrides: &HashSet<String>,
) -> Vec<crate::ast::ArrayDeclaration> {
    arrays
        .iter()
        .filter_map(|array| {
            let segments: Vec<_> = array
                .segments
                .iter()
                .filter_map(|segment| {
                    let names: Vec<_> = segment
                        .names
                        .iter()
                        .filter(|name| !overrides.contains(*name))
                        .cloned()
                        .collect();
                    if names.is_empty() {
                        None
                    } else {
                        Some(crate::ast::ArraySegment {
                            names,
                            bounds: segment.bounds.clone(),
                        })
                    }
                })
                .collect();
            if segments.is_empty() {
                None
            } else {
                Some(crate::ast::ArrayDeclaration {
                    element_type: array.element_type.clone(),
                    segments,

                    span: 0..0,
                })
            }
        })
        .collect()
}

fn filter_overridden_procedures(
    procedures: &[ProcedureDeclaration],
    overrides: &HashSet<String>,
) -> Vec<ProcedureDeclaration> {
    procedures
        .iter()
        .filter(|procedure| !overrides.contains(&procedure.name))
        .cloned()
        .collect()
}

fn filter_overridden_switches(
    switches: &[crate::ast::SwitchDeclaration],
    overrides: &HashSet<String>,
) -> Vec<crate::ast::SwitchDeclaration> {
    switches
        .iter()
        .filter(|switch| !overrides.contains(&switch.name))
        .cloned()
        .collect()
}

/// Storage names gain a `$Subclass` suffix when a subclass redeclares an
/// attribute, so protection lookups compare the declared identifier.
fn protection_base_name(name: &str) -> &str {
    name.split('$').next().unwrap_or(name)
}

fn is_already_protected(already_protected: &HashSet<String>, name: &str) -> bool {
    let base = protection_base_name(name);
    already_protected
        .iter()
        .any(|known| protection_base_name(known).eq_ignore_ascii_case(base))
}

fn apply_protection_spec(
    map: &mut BTreeMap<String, AttributeProtection>,
    spec: &ProtectionSpec,
    defining_class: &str,
) -> Result<(), CompileError> {
    let mut already_protected: HashSet<String> = map
        .iter()
        .filter(|(_, protection)| protection.protected)
        .map(|(name, _)| name.clone())
        .collect();

    for name in &spec.names {
        // §5.5.4: "Only a protected attribute may be specified hidden. However,
        // this specification may occur at a prefix level inner to the protected
        // specification." `map` already carries the prefix chain's protections,
        // so an inner `hidden` is legal exactly when the name is protected here
        // or at some outer level.
        if spec.hidden && !spec.protected && !is_already_protected(&already_protected, name) {
            return Err(crate::diagnostics::hidden_requires_protected(
                name,
                defining_class,
                spec.span.clone(),
            ));
        }

        // `hidden i` / `protected i` names the identifier, which binds to the
        // innermost still-visible protected attribute of that base name (after
        // earlier hides have peeled away inner redeclarations).
        let target_key = if spec.hidden && !spec.protected {
            resolve_visible_protection_key(map, name).unwrap_or_else(|| name.clone())
        } else if map.keys().any(|key| key.eq_ignore_ascii_case(name))
            || map
                .keys()
                .any(|key| protection_base_name(key).eq_ignore_ascii_case(name))
        {
            resolve_visible_protection_key(map, name).unwrap_or_else(|| name.clone())
        } else {
            name.clone()
        };

        let entry = map
            .entry(target_key.clone())
            .or_insert_with(|| AttributeProtection {
                protected: false,
                hidden: false,
                defining_class: defining_class.to_string(),
                protected_span: None,
                hidden_span: None,
            });

        if spec.protected {
            entry.protected = true;
            entry.defining_class = defining_class.to_string();
            entry.protected_span = spec.span.clone();
            already_protected.insert(target_key.clone());
        }
        if spec.hidden {
            entry.hidden = true;
            entry.defining_class = defining_class.to_string();
            entry.hidden_span = spec.span.clone();
        }
    }

    Ok(())
}

/// Storage key of the innermost protected attribute named `name` that is not
/// already hidden (so a further `hidden i` can peel the next outer binding).
fn resolve_visible_protection_key(
    map: &BTreeMap<String, AttributeProtection>,
    name: &str,
) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for (key, protection) in map {
        if !protection.protected {
            continue;
        }
        if !(key.eq_ignore_ascii_case(name) || protection_base_name(key).eq_ignore_ascii_case(name))
        {
            continue;
        }
        if protection.hidden {
            continue;
        }
        // Prefer the most specific mangled storage (`i$d` over `i`).
        let specificity = key.bytes().filter(|&b| b == b'$').count();
        if best.as_ref().is_none_or(|(best_spec, best_key)| {
            specificity > *best_spec || (specificity == *best_spec && key > best_key)
        }) {
            best = Some((specificity, key.clone()));
        }
    }
    best.map(|(_, key)| key)
}

fn build_protection_map(
    class: &ClassDeclaration,
) -> Result<BTreeMap<String, AttributeProtection>, CompileError> {
    let mut map = BTreeMap::new();
    for spec in &class.protection_part {
        apply_protection_spec(&mut map, spec, &class.name)?;
    }
    Ok(map)
}

/// All classes in the prefix chain of `class_name`, including `class_name` itself.
pub fn prefix_chain(
    class_name: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> HashSet<String> {
    let mut chain = HashSet::new();
    let mut current = Some(class_name.to_string());
    while let Some(name) = current {
        chain.insert(name.clone());
        current = classes
            .get(&name)
            .or_else(|| find_class_ignore_case(classes, &name))
            .and_then(|class| class.prefix.clone());
    }
    chain
}

/// Whether `class_name` is a subclass of `ancestor` (strict or same class returns false for strict).
pub fn is_subclass_of(
    class_name: &str,
    ancestor: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    let mut current = classes
        .get(class_name)
        .or_else(|| find_class_ignore_case(classes, class_name))
        .and_then(|class| class.prefix.clone());
    while let Some(name) = current {
        if name.eq_ignore_ascii_case(ancestor) {
            return true;
        }
        current = classes
            .get(&name)
            .or_else(|| find_class_ignore_case(classes, &name))
            .and_then(|class| class.prefix.clone());
    }
    false
}

/// Whether a protected/hidden attribute is hidden from `access_class`.
pub fn is_hidden_from(
    access_class: &str,
    protection: &AttributeProtection,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    if !protection.hidden {
        return false;
    }
    if access_class == protection.defining_class {
        return false;
    }
    is_subclass_of(access_class, &protection.defining_class, classes)
}

/// Whether `access_class` may access a protected attribute (same class or subclass).
pub fn in_protection_hierarchy(
    access_class: Option<&str>,
    protection: &AttributeProtection,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    let Some(access_class) = access_class else {
        return false;
    };
    access_class == protection.defining_class
        || is_subclass_of(access_class, &protection.defining_class, classes)
}

/// Whether `name` is declared as a virtual quantity on the concatenated class.
pub fn is_virtual_quantity(class: &ClassDeclaration, name: &str) -> bool {
    class
        .virtual_part
        .iter()
        .any(|spec| spec.names.iter().any(|entry| entry == name))
}

/// The virtual spec entry for `name`, if any.
pub fn virtual_spec_for<'a>(class: &'a ClassDeclaration, name: &str) -> Option<&'a VirtualSpec> {
    class
        .virtual_part
        .iter()
        .find(|spec| spec.names.iter().any(|entry| entry == name))
}

/// Kind of a matching class-body attribute (?5.5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeMatch {
    Variable(Type),
    Procedure,
}

/// Prefix chain from outermost ancestor to `class_name` (inclusive).
pub fn prefix_chain_ordered(
    class_name: &str,
    classes: &HashMap<String, ClassDeclaration>,
) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = Some(class_name.to_string());
    while let Some(name) = current {
        chain.push(name.clone());
        current = classes.get(&name).and_then(|class| class.prefix.clone());
    }
    chain.reverse();
    chain
}

/// Find the innermost body attribute matching `name` in a concatenated class body.
pub fn find_innermost_attribute_match(
    class: &ClassDeclaration,
    name: &str,
) -> Option<AttributeMatch> {
    find_innermost_in_block(&class.body, name)
}

/// Resolve a remote attribute per ?5.5.6.5: innermost match not inner to `access_class`.
pub fn find_remote_attribute_match(
    object_class: &str,
    name: &str,
    access_class: Option<&str>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Option<AttributeMatch> {
    find_remote_attribute_level(object_class, name, access_class, raw_classes).map(|(_, m)| m)
}

/// Like [`find_remote_attribute_match`], also returning the defining class name.
pub fn find_remote_attribute_level(
    object_class: &str,
    name: &str,
    access_class: Option<&str>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Option<(String, AttributeMatch)> {
    let chain = prefix_chain_ordered(object_class, raw_classes);
    for class_name in chain.iter().rev() {
        if let Some(access) = access_class
            && class_name != access
            && is_subclass_of(class_name, access, raw_classes)
        {
            continue;
        }
        let class = raw_classes.get(class_name).or_else(|| {
            raw_classes
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(class_name))
                .map(|(_, c)| c)
        })?;
        if let Some(matched) = find_attribute_in_raw_class(class, name) {
            return Some((class.name.clone(), matched));
        }
    }
    None
}

/// Resolve a remote procedure attribute per ?5.5.6.5 access-level rules.
pub fn find_remote_procedure_match(
    object_class: &str,
    name: &str,
    access_class: Option<&str>,
    raw_classes: &HashMap<String, ClassDeclaration>,
) -> Option<ProcedureDeclaration> {
    let chain = prefix_chain_ordered(object_class, raw_classes);
    for class_name in chain.iter().rev() {
        if let Some(access) = access_class
            && class_name != access
            && is_subclass_of(class_name, access, raw_classes)
        {
            continue;
        }
        let class = raw_classes.get(class_name)?;
        if let Some(procedure) = class
            .body
            .procedures
            .iter()
            .rev()
            .find(|procedure| procedure.name == name)
        {
            return Some(procedure.clone());
        }
    }
    None
}

pub fn find_attribute_in_raw_class(class: &ClassDeclaration, name: &str) -> Option<AttributeMatch> {
    for param in &class.parameters {
        if param.name.eq_ignore_ascii_case(name) {
            return Some(AttributeMatch::Variable(param.ty.clone()));
        }
    }
    for spec in &class.specifications {
        if spec
            .names
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
        {
            return Some(specifier_to_attribute_match(&spec.specifier));
        }
    }
    if let Some(matched) = find_innermost_in_block(&class.body, name) {
        return Some(matched);
    }
    // §5.6.7: unmatched virtual quantities remain visible as attributes.
    for spec in &class.virtual_part {
        if spec
            .names
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
        {
            return Some(specifier_to_attribute_match(&spec.specifier));
        }
    }
    None
}

fn specifier_to_attribute_match(specifier: &Specifier) -> AttributeMatch {
    match specifier {
        Specifier::Type(ty) => AttributeMatch::Variable(ty.clone()),
        Specifier::TypeArray(ty) => AttributeMatch::Variable(Type::Array {
            element: Box::new(ty.clone()),
            dims: 0,
        }),
        Specifier::Array => AttributeMatch::Variable(Type::Array {
            element: Box::new(Type::Real { long: false }),
            dims: 0,
        }),
        Specifier::Label | Specifier::Switch => {
            AttributeMatch::Variable(Type::Integer { short: false })
        }
        Specifier::Procedure => AttributeMatch::Procedure,
        Specifier::TypeProcedure(ty) => AttributeMatch::Variable(ty.clone()),
    }
}

fn find_innermost_in_block(block: &Block, name: &str) -> Option<AttributeMatch> {
    let mut found = None;

    for declaration in &block.declarations {
        for item in &declaration.items {
            if item.name.eq_ignore_ascii_case(name) {
                found = Some(AttributeMatch::Variable(declaration.ty.clone()));
            }
        }
    }

    for procedure in &block.procedures {
        if procedure.name.eq_ignore_ascii_case(name) {
            found = Some(match &procedure.result_type {
                Some(result_type) => AttributeMatch::Variable(result_type.clone()),
                None => AttributeMatch::Procedure,
            });
        }
    }

    for array in &block.arrays {
        for segment in &array.segments {
            if segment
                .names
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case(name))
            {
                found = Some(AttributeMatch::Variable(Type::Array {
                    element: Box::new(array.element_type.clone()),
                    dims: segment.bounds.len(),
                }));
            }
        }
    }

    for switch in &block.switches {
        if switch.name.eq_ignore_ascii_case(name) {
            found = Some(AttributeMatch::Variable(Type::Integer { short: false }));
        }
    }

    found
}

/// Innermost matching procedure attribute for a virtual procedure quantity.
pub fn find_innermost_procedure_match(
    class: &ClassDeclaration,
    name: &str,
) -> Option<ProcedureDeclaration> {
    class
        .body
        .procedures
        .iter()
        .rev()
        .find(|procedure| procedure.name == name)
        .cloned()
}

/// Basic virtual-spec vs matching-attribute kind correspondence (?5.5.3).
pub fn virtual_kind_matches(specifier: &Specifier, matched: &AttributeMatch) -> bool {
    match (specifier, matched) {
        (Specifier::Type(expected), AttributeMatch::Variable(actual)) => {
            actual.accepts_assignment_from(expected) || expected.accepts_assignment_from(actual)
        }
        (Specifier::Procedure, AttributeMatch::Procedure) => true,
        // A typed procedure attribute still matches a bare `Procedure` virtual spec.
        (Specifier::Procedure, AttributeMatch::Variable(_)) => true,
        (Specifier::TypeProcedure(expected), AttributeMatch::Variable(actual)) => {
            actual.accepts_assignment_from(expected) || expected.accepts_assignment_from(actual)
        }
        // An untyped procedure attribute still matches a typed procedure virtual spec.
        (Specifier::TypeProcedure(_), AttributeMatch::Procedure) => true,
        (Specifier::Label | Specifier::Switch, AttributeMatch::Variable(_)) => true,
        (
            Specifier::Array | Specifier::TypeArray(_),
            AttributeMatch::Variable(Type::Array { .. }),
        ) => true,
        _ => false,
    }
}

/// Virtual-spec matching with subclass ref subordination (?5.5.3 / ?2.4.2).
///
/// For non-procedure type specs, a matching attribute whose type subordinates to
/// the virtual type is accepted (e.g. virtual `ref(Point)` matched by `ref(Polar)`).
pub fn virtual_kind_matches_in_class(
    specifier: &Specifier,
    matched: &AttributeMatch,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    if virtual_kind_matches(specifier, matched) {
        return true;
    }
    match (specifier, matched) {
        (
            Specifier::Type(expected) | Specifier::TypeProcedure(expected),
            AttributeMatch::Variable(actual),
        ) => ref_type_subordinates(actual, expected, classes),
        _ => false,
    }
}

/// Whether a reference of type `source` may be assigned to `target` (?2.4.2).
pub fn ref_type_subordinates(
    source: &Type,
    target: &Type,
    classes: &HashMap<String, ClassDeclaration>,
) -> bool {
    match (source, target) {
        (Type::ObjectRef(source_q), Type::ObjectRef(target_q)) => {
            source_q.eq_ignore_ascii_case("none")
                || source_q.eq_ignore_ascii_case(target_q)
                || is_subclass_of(source_q, target_q, classes)
        }
        _ => false,
    }
}

fn check_virtual_uniqueness(virtual_part: &[VirtualSpec]) -> Result<(), CompileError> {
    let mut seen = HashMap::new();
    for spec in virtual_part {
        for name in &spec.names {
            if seen.insert(name.clone(), ()).is_some() {
                return Err(crate::diagnostics::duplicate_virtual(name, None));
            }
        }
    }
    Ok(())
}

/// Detect `inner` marker in a class body and record split-body metadata.
pub fn detect_inner_marker(class: &mut ClassDeclaration) {
    let (has_inner, inner_label, initial, tail) = split_at_inner(&class.body.statements);
    class.has_inner = has_inner;
    class.inner_label = inner_label;
    class.body.statements = initial;
    class.tail_statements = tail;
}

fn split_at_inner(
    statements: &[Statement],
) -> (bool, Option<String>, Vec<Statement>, Vec<Statement>) {
    for (index, statement) in statements.iter().enumerate() {
        match &statement.kind {
            StatementKind::Inner { label } => {
                return (
                    true,
                    label.clone(),
                    statements[..index].to_vec(),
                    statements[index + 1..].to_vec(),
                );
            }
            StatementKind::Labeled {
                label: stmt_label,
                statement: inner,
            } => {
                if matches!(inner.kind, StatementKind::Inner { .. }) {
                    let inner_label = if let StatementKind::Inner { label } = &inner.kind {
                        label.clone().or(Some(stmt_label.clone()))
                    } else {
                        None
                    };
                    return (
                        true,
                        inner_label,
                        statements[..index].to_vec(),
                        statements[index + 1..].to_vec(),
                    );
                }
            }
            _ => {}
        }
    }

    (false, None, statements.to_vec(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Block;
    use crate::types::{Declaration, DeclarationItem, Type};

    fn simple_class(name: &str, prefix: Option<&str>, decl_name: &str) -> ClassDeclaration {
        ClassDeclaration {
            prefix: prefix.map(str::to_string),
            name: name.to_string(),
            parameters: Vec::new(),
            specifications: Vec::new(),
            virtual_part: Vec::new(),
            protection_part: Vec::new(),
            protection_map: BTreeMap::new(),
            body: Block {
                prefix: None,
                name: String::new(),
                directives: Vec::new(),
                externals: Vec::new(),
                declarations: vec![Declaration {
                    ty: Type::Integer { short: false },
                    items: vec![DeclarationItem {
                        name: decl_name.to_string(),
                        initializer: None,
                        is_constant: false,
                    }],
                    span: 0..0,
                }],
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

    #[test]
    fn real_class_overrides_external_stub() {
        let stub = ClassDeclaration {
            prefix: None,
            name: "Chess".into(),
            parameters: Vec::new(),
            specifications: Vec::new(),
            virtual_part: Vec::new(),
            protection_part: Vec::new(),
            protection_map: BTreeMap::new(),
            body: Block {
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
            identifier_substitutions: BTreeMap::new(),
            span: 0..0,
        };
        let mut externals = HashMap::new();
        externals.insert("Chess".into(), stub);
        let real = simple_class("Chess", None, "Mate");
        let map = concatenate_classes_with_externals(&[real], &externals).unwrap();
        let chess = map.get("Chess").unwrap();
        assert!(
            chess
                .body
                .declarations
                .iter()
                .flat_map(|d| d.items.iter())
                .any(|item| item.name == "Mate"),
            "real Chess body should replace the external stub"
        );
    }

    #[test]
    fn remote_attribute_respects_access_level() {
        let point = simple_class("Point", None, "x");
        let mut polar = simple_class("Polar", Some("Point"), "r");
        polar.body.declarations = vec![Declaration {
            ty: Type::Real { long: false },
            items: vec![DeclarationItem {
                name: "x".into(),
                initializer: None,
                is_constant: false,
            }],
            span: 0..0,
        }];
        let raw = HashMap::from([
            ("Point".into(), point.clone()),
            ("Polar".into(), polar.clone()),
        ]);
        let outer = find_remote_attribute_match("Polar", "x", None, &raw).unwrap();
        assert!(matches!(outer, AttributeMatch::Variable(Type::Real { .. })));
        let point_level = find_remote_attribute_match("Polar", "x", Some("Point"), &raw).unwrap();
        assert!(matches!(
            point_level,
            AttributeMatch::Variable(Type::Integer { .. })
        ));
    }

    #[test]
    fn injects_fictitious_detach_stub() {
        let worker = simple_class("Worker", None, "x");
        let map = concatenate_classes(&[worker]).unwrap();
        let worker = map.get("Worker").unwrap();
        assert!(is_virtual_quantity(worker, FICTITIOUS_DETACH_NAME));
        assert!(find_innermost_procedure_match(worker, FICTITIOUS_DETACH_NAME).is_some());
    }

    #[test]
    fn concatenates_prefix_attributes() {
        let point = simple_class("point", None, "x");
        let polar = simple_class("polar", Some("point"), "r");
        let map = concatenate_classes(&[point, polar]).unwrap();
        let polar = map.get("polar").unwrap();
        assert_eq!(polar.parameters.len(), 0);
        let names: Vec<_> = polar
            .body
            .declarations
            .iter()
            .flat_map(|d| d.items.iter().map(|i| i.name.as_str()))
            .collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"r"));
    }

    #[test]
    fn renames_main_attribute_conflicting_with_prefix() {
        let point = simple_class("Point", None, "x");
        let mut polar = simple_class("Polar", Some("Point"), "r");
        polar.body.declarations = vec![Declaration {
            ty: Type::Integer { short: false },
            items: vec![DeclarationItem {
                name: "x".into(),
                initializer: None,
                is_constant: false,
            }],
            span: 0..0,
        }];
        let map = concatenate_classes(&[point, polar]).unwrap();
        let polar = map.get("Polar").unwrap();
        assert_eq!(
            polar.identifier_substitutions.get("x").map(String::as_str),
            Some("x$Polar")
        );
        let names: Vec<_> = polar
            .body
            .declarations
            .iter()
            .flat_map(|d| d.items.iter().map(|i| i.name.as_str()))
            .collect();
        assert!(
            names.contains(&"x$Polar"),
            "expected renamed main attribute, got {names:?}"
        );
        assert!(names.contains(&"x"), "prefix attribute x should remain");
        assert_eq!(substitute_remote_attribute("Polar", "x", &map), "x$Polar");
        assert_eq!(substitute_remote_attribute("Point", "x", &map), "x");
    }

    #[test]
    fn accessible_remote_skips_protected_subclass_attr() {
        let a = simple_class("A", None, "i");
        let mut b = simple_class("B", Some("A"), "j");
        b.body.declarations = vec![Declaration {
            ty: Type::Integer { short: false },
            items: vec![DeclarationItem {
                name: "i".into(),
                initializer: None,
                is_constant: false,
            }],
            span: 0..0,
        }];
        b.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: true,
            hidden: false,
            span: None,
        }];
        let map = concatenate_classes(&[a, b]).unwrap();
        assert_eq!(substitute_remote_attribute("B", "i", &map), "i$B");
        assert_eq!(
            accessible_remote_storage_name("B", "i", None, &map),
            "i",
            "outside access should fall through to unprotected prefix i"
        );
        assert_eq!(
            accessible_remote_storage_name("B", "i", Some("B"), &map),
            "i$B",
            "inside B, protected B.i remains visible for remote access"
        );
    }

    #[test]
    fn rejects_prefix_cycle() {
        let a = ClassDeclaration {
            prefix: Some("b".into()),
            ..simple_class("a", None, "x")
        };
        let b = ClassDeclaration {
            prefix: Some("a".into()),
            ..simple_class("b", None, "y")
        };
        assert!(concatenate_classes(&[a, b]).is_err());
    }

    #[test]
    fn builds_protection_map() {
        use crate::ast::ProtectionSpec;
        let mut point = simple_class("Point", None, "x");
        point.protection_part = vec![ProtectionSpec {
            hidden: false,
            protected: true,
            names: vec!["x".into()],
            span: None,
        }];
        let map = concatenate_classes(&[point]).unwrap();
        let point = map.get("Point").unwrap();
        let protection = point.protection_map.get("x").unwrap();
        assert!(protection.protected);
        assert!(!protection.hidden);
        assert_eq!(protection.defining_class, "Point");
    }

    #[test]
    fn bare_hidden_without_protection_is_rejected() {
        // §5.5.4: only a protected attribute may be specified hidden.
        let mut class = simple_class("C", None, "x");
        class.protection_part = vec![ProtectionSpec {
            hidden: true,
            protected: false,
            names: vec!["x".into()],
            span: None,
        }];
        let error = concatenate_classes(&[class]).unwrap_err();
        assert!(
            error.to_string().contains("protected"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn hidden_at_inner_prefix_level_of_protected_is_allowed() {
        // §5.5.4: the hidden specification may occur at a prefix level inner
        // to the protected specification.
        let mut base = simple_class("A", None, "i");
        base.protection_part = vec![ProtectionSpec {
            hidden: false,
            protected: true,
            names: vec!["i".into()],
            span: None,
        }];
        let mut derived = simple_class("B", Some("A"), "j");
        derived.protection_part = vec![ProtectionSpec {
            hidden: true,
            protected: false,
            names: vec!["i".into()],
            span: None,
        }];
        let map = concatenate_classes(&[base, derived]).unwrap();
        let protection = map.get("B").unwrap().protection_map.get("i").unwrap();
        assert!(protection.protected);
        assert!(protection.hidden);
    }

    #[test]
    fn split_body_separates_initial_and_tail_statements() {
        let mut class = simple_class("C", None, "x");
        class.body.statements = vec![
            Statement::dummy(StatementKind::Dummy),
            Statement::dummy(StatementKind::Inner { label: None }),
            Statement::dummy(StatementKind::Dummy),
        ];
        detect_inner_marker(&mut class);
        assert!(class.has_inner);
        assert_eq!(class.body.statements.len(), 1);
        assert_eq!(class.tail_statements.len(), 1);
    }

    #[test]
    fn non_split_prefix_merges_before_split_main_sections() {
        let mut prefix = simple_class("prefix", None, "a");
        prefix.body.statements = vec![Statement::dummy(StatementKind::Dummy)];

        let mut main = simple_class("main", Some("prefix"), "b");
        main.body.statements = vec![Statement::dummy(StatementKind::Dummy)];
        main.body
            .statements
            .push(Statement::dummy(StatementKind::Inner { label: None }));
        main.body
            .statements
            .push(Statement::dummy(StatementKind::Dummy));
        detect_inner_marker(&mut main);

        let map = concatenate_classes(&[prefix, main]).unwrap();
        let merged = map.get("main").unwrap();
        assert_eq!(merged.body.statements.len(), 2);
        assert_eq!(merged.tail_statements.len(), 1);
    }

    #[test]
    fn prefixed_block_appends_statements_after_prefix_initials() {
        let mut prefix = simple_class("c59", None, "i");
        prefix.body.statements = vec![
            Statement::dummy(StatementKind::Dummy),
            Statement::dummy(StatementKind::Dummy),
        ];
        let mut block = empty_block();
        block.statements = vec![Statement::dummy(StatementKind::Dummy)];
        let (initial, finals) = prefixed_block_statements(&prefix, &block);
        assert_eq!(initial.len(), 3);
        assert!(finals.is_empty());
    }

    #[test]
    fn removes_prefix_virtual_match_when_main_redefines() {
        use crate::ast::{Specifier, VirtualSpec};

        let mut point = simple_class("point", None, "x");
        point.virtual_part = vec![VirtualSpec {
            specifier: Specifier::Procedure,
            names: vec!["plus".into()],
            procedure_heading: None,
        }];
        point.body.procedures.push(ProcedureDeclaration {
            result_type: None,
            name: "plus".into(),
            parameters: Vec::new(),
            body: Block {
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
            is_external: false,
            identification: None,
            span: 0..0,
        });

        let mut polar = simple_class("polar", Some("point"), "r");
        polar.body.procedures.push(ProcedureDeclaration {
            result_type: None,
            name: "plus".into(),
            parameters: Vec::new(),
            body: Block {
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
            is_external: false,
            identification: None,
            span: 0..0,
        });

        let map = concatenate_classes(&[point, polar]).unwrap();
        let merged = map.get("polar").unwrap();
        assert_eq!(merged.body.procedures.len(), 2);
        assert!(
            merged
                .body
                .procedures
                .iter()
                .any(|procedure| procedure.name == FICTITIOUS_DETACH_NAME)
        );
        assert_eq!(merged.body.procedures.last().unwrap().name, "plus");
    }

    #[test]
    fn ref_subordination_allows_subclass_and_none() {
        let point = simple_class("Point", None, "x");
        let polar = simple_class("Polar", Some("Point"), "r");
        let map = concatenate_classes(&[point, polar]).unwrap();

        let target = Type::ObjectRef("Point".into());
        let subclass = Type::ObjectRef("Polar".into());
        let none = Type::ObjectRef("none".into());
        let unrelated = Type::ObjectRef("Other".into());

        assert!(ref_type_subordinates(&subclass, &target, &map));
        assert!(ref_type_subordinates(&none, &target, &map));
        assert!(!ref_type_subordinates(&unrelated, &target, &map));
        assert!(!ref_type_subordinates(&target, &subclass, &map));
    }

    #[test]
    fn virtual_type_spec_accepts_subordinate_ref_match() {
        let point = simple_class("Point", None, "x");
        let polar = simple_class("Polar", Some("Point"), "r");
        let map = concatenate_classes(&[point, polar]).unwrap();

        let virtual_spec = Specifier::Type(Type::ObjectRef("Point".into()));
        let matched = AttributeMatch::Variable(Type::ObjectRef("Polar".into()));
        assert!(virtual_kind_matches_in_class(&virtual_spec, &matched, &map));

        let too_weak = AttributeMatch::Variable(Type::ObjectRef("Point".into()));
        let tight_virtual = Specifier::Type(Type::ObjectRef("Polar".into()));
        assert!(!virtual_kind_matches_in_class(
            &tight_virtual,
            &too_weak,
            &map
        ));
    }

    #[test]
    fn fixture_style_polar_gets_substitution() {
        use crate::parse::test_support::parse_program;
        let program = parse_program(
            r#"begin
    class Point(x); integer x;
    begin
    end;
    Point class Polar(r); integer x; real r;
    begin
        x := 5;
    end;
end"#,
        );
        let map = concatenate_classes(&program.blocks[0].classes).unwrap();
        let polar = map.get("Polar").unwrap();
        assert_eq!(
            polar.identifier_substitutions.get("x").map(String::as_str),
            Some("x$Polar"),
            "subst={:?} specs={:?} stmts={:?}",
            polar.identifier_substitutions,
            polar
                .specifications
                .iter()
                .map(|s| &s.names)
                .collect::<Vec<_>>(),
            polar
                .body
                .statements
                .iter()
                .map(|s| &s.kind)
                .collect::<Vec<_>>()
        );
        assert_eq!(substitute_remote_attribute("Polar", "x", &map), "x$Polar");
    }

    fn raw_map(source: &str) -> HashMap<String, ClassDeclaration> {
        use crate::parse::test_support::parse_program;
        let program = parse_program(source);
        program.blocks[0]
            .classes
            .iter()
            .map(|class| (class.name.clone(), class.clone()))
            .collect()
    }

    /// The simtst98 virtual-protection chain: `a`→`b`→`c`→`x`→`y`→`z`, where
    /// `b` and `y` hide the virtual quantity visible at their level.
    fn simtst98_virtual_chain() -> HashMap<String, ClassDeclaration> {
        raw_map(
            r#"begin
                procedure virtproc; begin end;
                class a; protected virtproc; virtual: procedure virtproc;
                begin procedure virtproc; begin end; end;
                a class b; hidden virtproc;
                begin procedure virtproc; begin end; end;
                b class c;
                begin procedure virtproc; begin end; end;
                c class x; protected virtproc; virtual: procedure virtproc;
                begin procedure virtproc; begin end; end;
                x class y; hidden virtproc;
                begin procedure virtproc; begin end; end;
                y class z;
                begin procedure virtproc; begin end; end;
            end"#,
        )
    }

    #[test]
    fn hidden_virtual_stops_matching_in_subclasses() {
        let raw = simtst98_virtual_chain();
        assert_eq!(
            virtual_match_level("z", "a", "virtproc", &raw).as_deref(),
            Some("b"),
            "`b` hid `a`'s virtual: no further matching below `b`"
        );
        assert_eq!(
            virtual_match_level("c", "a", "virtproc", &raw).as_deref(),
            Some("b")
        );
        assert_eq!(
            virtual_match_level("z", "x", "virtproc", &raw).as_deref(),
            Some("y")
        );
        assert_eq!(
            virtual_match_level("b", "a", "virtproc", &raw).as_deref(),
            Some("b")
        );
        assert_eq!(
            virtual_match_level("a", "a", "virtproc", &raw).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn bare_virtual_name_binds_per_prefix_level() {
        let raw = simtst98_virtual_chain();
        let binding = |access: AccessLevel<'_>, qual: &str| {
            visible_attribute_binding(access, qual, "virtproc", &raw)
        };
        // Own text of the hider still sees the quantity it hid.
        assert_eq!(
            binding(AccessLevel::class_text("b"), "b"),
            Some(AttributeBinding {
                level: "a".into(),
                kind: AttributeKind::Virtual
            })
        );
        // Subclasses of the hider get their own ordinary procedure instead.
        assert_eq!(
            binding(AccessLevel::class_text("c"), "c"),
            Some(AttributeBinding {
                level: "c".into(),
                kind: AttributeKind::Procedure
            })
        );
        assert_eq!(
            binding(AccessLevel::class_text("z"), "z"),
            Some(AttributeBinding {
                level: "z".into(),
                kind: AttributeKind::Procedure
            })
        );
        assert_eq!(
            binding(AccessLevel::class_text("y"), "y"),
            Some(AttributeBinding {
                level: "x".into(),
                kind: AttributeKind::Virtual
            })
        );
        // Connection block in `c`'s text on `this a`: `a`'s virtproc is hidden
        // from `c`, so the name falls through to `c`'s own procedure (simtst98).
        assert_eq!(binding(AccessLevel::class_text("c"), "a"), None);
        assert_eq!(
            binding(AccessLevel::class_text("b"), "a"),
            Some(AttributeBinding {
                level: "a".into(),
                kind: AttributeKind::Virtual
            })
        );
        // Outside every class: protected quantities are invisible, so the
        // innermost *accessible* binding wins.
        assert_eq!(binding(AccessLevel::outside(), "b"), None);
        assert_eq!(
            binding(AccessLevel::outside(), "y"),
            Some(AttributeBinding {
                level: "c".into(),
                kind: AttributeKind::Procedure
            })
        );
        assert_eq!(
            binding(AccessLevel::outside(), "z"),
            Some(AttributeBinding {
                level: "z".into(),
                kind: AttributeKind::Procedure
            })
        );
    }

    #[test]
    fn stacked_hides_peel_one_binding_each() {
        // simtst98 `a`→`d`→`e`→`f`→`g`: `e` hides `d`'s `i`, `f` hides `a`'s.
        let raw = raw_map(
            r#"begin integer i;
                class a; protected i; begin integer i; end;
                a class d; protected i; begin integer i; end;
                d class e; hidden i; begin end;
                e class f; hidden i; begin end;
                f class g; begin end;
            end"#,
        );
        let level = |access: AccessLevel<'_>, qual: &str| {
            visible_attribute_binding(access, qual, "i", &raw).map(|binding| binding.level)
        };
        assert_eq!(
            level(AccessLevel::class_text("a"), "a").as_deref(),
            Some("a")
        );
        assert_eq!(
            level(AccessLevel::class_text("d"), "d").as_deref(),
            Some("d")
        );
        // The hider's own text keeps seeing the attribute it hid.
        assert_eq!(
            level(AccessLevel::class_text("e"), "e").as_deref(),
            Some("d")
        );
        assert_eq!(
            level(AccessLevel::class_text("f"), "f").as_deref(),
            Some("a")
        );
        assert_eq!(level(AccessLevel::class_text("g"), "g"), None);
        // A block prefixed by `e` is a fictitious subclass level: `e`'s hide applies.
        assert_eq!(
            level(
                AccessLevel {
                    class: Some("e"),
                    prefixed_block: true
                },
                "e"
            )
            .as_deref(),
            Some("a")
        );
        assert_eq!(
            level(
                AccessLevel {
                    class: Some("f"),
                    prefixed_block: true
                },
                "f"
            ),
            None
        );
        // Protected attributes are invisible from outside every class.
        assert_eq!(level(AccessLevel::outside(), "d"), None);
        assert_eq!(level(AccessLevel::outside(), "g"), None);
    }

    #[test]
    fn hide_fallthrough_rewrites_subclass_body_only() {
        let mut a = simple_class("a", None, "i");
        a.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: true,
            hidden: false,
            span: None,
        }];
        let mut b = simple_class("b", Some("a"), "k");
        b.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: false,
            hidden: true,
            span: None,
        }];
        let mut c = simple_class("c", Some("b"), "ci");
        c.body.declarations.push(Declaration {
            ty: Type::Integer { short: false },
            items: vec![DeclarationItem {
                name: "seen".into(),
                initializer: Some(Expr {
                    kind: ExprKind::Variable(Variable::Simple("i".into())),
                    span: 0..0,
                }),
                is_constant: false,
            }],
            span: 0..0,
        });
        let map = concatenate_classes(&[a, b, c]).unwrap();
        let c = map.get("c").unwrap();
        assert_eq!(
            c.identifier_substitutions.get("i").map(String::as_str),
            Some("__simrt_encl_i")
        );
        let init = c
            .body
            .declarations
            .iter()
            .find_map(|d| d.items.iter().find(|item| item.name == "seen"))
            .and_then(|item| item.initializer.as_ref())
            .expect("seen initializer");
        assert!(
            matches!(
                &init.kind,
                ExprKind::Variable(Variable::Simple(name)) if name == "__simrt_encl_i"
            ),
            "subclass text must fall through: {init:?}"
        );
        // The hider itself still sees attribute `i`.
        let b = map.get("b").unwrap();
        assert!(!b.identifier_substitutions.contains_key("i"));
    }

    #[test]
    fn hide_peels_redeclarations_across_prefix_levels() {
        // a.i protected; d redeclares i; e hides d'i → a'i; f hides a'i → encl.
        let mut a = simple_class("a", None, "i");
        a.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: true,
            hidden: false,
            span: None,
        }];
        let mut d = simple_class("d", Some("a"), "i");
        d.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: true,
            hidden: false,
            span: None,
        }];
        let mut e = simple_class("e", Some("d"), "e_only");
        e.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: false,
            hidden: true,
            span: None,
        }];
        let mut f = simple_class("f", Some("e"), "f_only");
        f.protection_part = vec![ProtectionSpec {
            names: vec!["i".into()],
            protected: false,
            hidden: true,
            span: None,
        }];
        let g = simple_class("g", Some("f"), "g_only");
        let map = concatenate_classes(&[a, d, e, f, g]).unwrap();

        let e = map.get("e").unwrap();
        assert!(e.protection_map.get("i$d").is_some_and(|p| p.hidden));
        assert!(e.protection_map.get("i").is_some_and(|p| !p.hidden));
        // Hider still sees d'i.
        assert_eq!(
            e.identifier_substitutions.get("i").map(String::as_str),
            Some("i$d")
        );

        let f = map.get("f").unwrap();
        assert!(
            f.protection_map
                .get("i")
                .is_some_and(|p| p.hidden && p.defining_class == "f")
        );
        assert!(
            f.protection_map
                .get("i$d")
                .is_some_and(|p| p.hidden && p.defining_class == "e")
        );
        // Subclass of e: d'i hidden → fall through to a'i; f's own hide does not
        // apply inside f.
        assert_eq!(
            f.identifier_substitutions.get("i").map(String::as_str),
            Some("i")
        );

        let g = map.get("g").unwrap();
        assert_eq!(
            g.identifier_substitutions.get("i").map(String::as_str),
            Some("__simrt_encl_i")
        );
    }

    #[test]
    fn outside_access_hides_protected_a_i() {
        use crate::parse::test_support::parse_program;
        let src = include_str!("../tests/testbatch/simtst98.sim");
        let program = parse_program(src);
        let raw: HashMap<_, _> = program.blocks[0]
            .classes
            .iter()
            .map(|c| (c.name.clone(), c.clone()))
            .collect();
        let a = raw.get("a").unwrap();
        eprintln!("a protection_part={:?}", a.protection_part);
        let b = visible_attribute_binding(AccessLevel::outside(), "a", "i", &raw);
        eprintln!("outside/a/i => {:?}", b);
        let b2 = visible_attribute_binding(AccessLevel::class_text("c"), "c", "i", &raw);
        eprintln!("c/c/i => {:?}", b2);
        assert!(b.is_none(), "expected None, got {b:?}");
    }
}
