//! Simulation / SIMSET system-class metadata (Standard Ch.12).
//!
//! Injects `Process` / `Simulation` / SIMSET stubs and name predicates used by
//! semantic analysis, layout, and MIR lowering. Runtime SQS lives in
//! `mir::interp::seq_ops` (interpreter), `mir::sim_runtime` (wasm), and
//! `runtime/runtime.c` (native).

use std::collections::HashMap;

use crate::ast::{
    Block, ClassDeclaration, Expr, ExprKind, ProcedureCall, Statement, StatementKind, Variable,
};
use crate::concatenate::detect_inner_marker;
use crate::types::Type;

/// Maximum length of the sequencing set (SQS).
pub const MAX_SQS_LENGTH: usize = 65_536;

/// Whether `name` is the system Process class (case-insensitive).
pub fn is_process_class(name: &str) -> bool {
    name.eq_ignore_ascii_case("process")
}

/// Whether `name` is the system Simulation class (case-insensitive).
pub fn is_simulation_class(name: &str) -> bool {
    name.eq_ignore_ascii_case("simulation")
}

/// Whether a block prefix designates Simulation.
pub fn block_is_simulation_prefixed(prefix: &Option<Expr>) -> bool {
    let Some(expr) = prefix else {
        return false;
    };
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => is_simulation_class(name),
        ExprKind::FunctionCall { name, .. } => is_simulation_class(name),
        _ => false,
    }
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

fn detach_statement() -> Statement {
    Statement::dummy(StatementKind::ProcedureCall(ProcedureCall {
        name: "detach".into(),
        arguments: Vec::new(),
    }))
}

fn inner_statement() -> Statement {
    Statement::dummy(StatementKind::Inner { label: None })
}

/// System `Process` class: `detach; inner;` (prefix `Link` for SIMSET membership).
pub fn process_system_class() -> ClassDeclaration {
    let mut class = ClassDeclaration {
        prefix: Some("Link".into()),
        name: "Process".into(),
        parameters: Vec::new(),
        specifications: Vec::new(),
        virtual_part: Vec::new(),
        protection_part: Vec::new(),
        protection_map: Default::default(),
        body: empty_block(),
        has_inner: false,
        inner_label: None,
        tail_statements: Vec::new(),
        identifier_substitutions: std::collections::BTreeMap::new(),
        span: 0..0,
    };
    class.body.statements = vec![detach_statement(), inner_statement()];
    detect_inner_marker(&mut class);
    class
}

fn stub_class(name: &str, prefix: Option<&str>) -> ClassDeclaration {
    ClassDeclaration {
        prefix: prefix.map(str::to_string),
        name: name.into(),
        parameters: Vec::new(),
        specifications: Vec::new(),
        virtual_part: Vec::new(),
        protection_part: Vec::new(),
        protection_map: Default::default(),
        body: empty_block(),
        has_inner: false,
        inner_label: None,
        tail_statements: Vec::new(),
        identifier_substitutions: std::collections::BTreeMap::new(),
        span: 0..0,
    }
}

/// System `Simulation` class (attribute/procedure surface is interpreter-built-in).
pub fn simulation_system_class() -> ClassDeclaration {
    stub_class("Simulation", Some("Simset"))
}

pub fn simset_system_class() -> ClassDeclaration {
    stub_class("Simset", None)
}

pub fn linkage_system_class() -> ClassDeclaration {
    stub_class("Linkage", None)
}

pub fn link_system_class() -> ClassDeclaration {
    stub_class("Link", Some("Linkage"))
}

pub fn head_system_class() -> ClassDeclaration {
    stub_class("Head", Some("Linkage"))
}

pub fn is_head_class(name: &str) -> bool {
    name.eq_ignore_ascii_case("head")
}

pub fn is_link_class(name: &str) -> bool {
    name.eq_ignore_ascii_case("link") || is_process_class(name)
}

/// Whether `name` is the system SIMSET class (case-insensitive).
pub fn is_simset_class(name: &str) -> bool {
    name.eq_ignore_ascii_case("simset")
}

/// Whether `name` is the system Linkage class (case-insensitive).
pub fn is_linkage_class(name: &str) -> bool {
    name.eq_ignore_ascii_case("linkage")
}

/// Whether a block prefix designates SIMSET.
pub fn block_is_simset_prefixed(prefix: &Option<Expr>) -> bool {
    let Some(expr) = prefix else {
        return false;
    };
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => is_simset_class(name),
        ExprKind::FunctionCall { name, .. } => is_simset_class(name),
        _ => false,
    }
}

/// Whether `name` names any class in the SIMSET/Simulation system family
/// (`Simset`, `Linkage`, `Head`, `Link`, `Process`, `Simulation`).
pub fn is_simset_family_class(name: &str) -> bool {
    is_simset_class(name)
        || is_linkage_class(name)
        || is_head_class(name)
        || is_link_class(name)
        || is_process_class(name)
        || is_simulation_class(name)
}

/// Whether `block` (recursively) needs the injected SIMSET/Simulation system
/// classes (`Simset`, `Linkage`, `Head`, `Link`, `Process`, `Simulation`) for
/// concatenation — mirrors [`crate::layout::program_needs_simulation_system_classes`]
/// but also covers plain SIMSET usage (no `Simulation` prefix required).
pub fn block_needs_system_classes(block: &Block) -> bool {
    if block_is_simulation_prefixed(&block.prefix) || block_is_simset_prefixed(&block.prefix) {
        return true;
    }
    if block.classes.iter().any(|class| {
        class.prefix.as_deref().is_some_and(is_simset_family_class)
            || is_simset_family_class(&class.name)
    }) {
        return true;
    }
    if block.declarations.iter().any(|decl| {
        matches!(
            &decl.ty,
            Type::ObjectRef(q) if is_simset_family_class(q)
        )
    }) {
        return true;
    }
    block.body.iter().any(block_needs_system_classes)
        || block.statements.iter().any(statement_needs_system_classes)
}

fn statement_needs_system_classes(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Compound(block) => block_needs_system_classes(block),
        StatementKind::If(if_stmt) => {
            statement_needs_system_classes(&if_stmt.then_branch)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|s| statement_needs_system_classes(s))
        }
        StatementKind::While(while_stmt) => statement_needs_system_classes(&while_stmt.body),
        StatementKind::For(for_stmt) => statement_needs_system_classes(&for_stmt.body),
        StatementKind::Labeled { statement, .. } => statement_needs_system_classes(statement),
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|clause| statement_needs_system_classes(&clause.body))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|s| statement_needs_system_classes(s))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|s| statement_needs_system_classes(s))
        }
        _ => false,
    }
}

/// Inject SIMSET + Process / Simulation unless the user already declared them.
pub fn inject_system_classes(classes: &mut HashMap<String, ClassDeclaration>) {
    let inject = |classes: &mut HashMap<String, ClassDeclaration>, class: ClassDeclaration| {
        let exists = classes.keys().any(|k| k.eq_ignore_ascii_case(&class.name));
        if !exists {
            classes.insert(class.name.clone(), class);
        }
    };
    inject(classes, simset_system_class());
    inject(classes, linkage_system_class());
    inject(classes, link_system_class());
    inject(classes, head_system_class());
    inject(classes, process_system_class());
    inject(classes, simulation_system_class());
}

/// Resolve a hold/passivate/time/current/wait call when Simulation is active.
pub fn is_simulation_builtin(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "hold" | "passivate" | "time" | "current" | "main" | "nextev" | "cancel" | "wait"
    )
}

pub fn is_simset_method(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "into"
            | "out"
            | "follow"
            | "precede"
            | "suc"
            | "pred"
            | "prev"
            | "first"
            | "last"
            | "empty"
            | "cardinal"
            | "clear"
    )
}

/// Result type for a SIMSET attribute/procedure used in expression position.
pub fn simset_method_result_type(name: &str) -> Type {
    match name.to_ascii_lowercase().as_str() {
        "empty" => Type::Boolean,
        "cardinal" => Type::Integer { short: false },
        "suc" | "pred" | "prev" | "first" | "last" => Type::ObjectRef("Link".into()),
        // Side-effecting methods still yield a dummy integer when used as exprs.
        _ => Type::Integer { short: false },
    }
}
