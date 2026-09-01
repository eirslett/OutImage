//! Annotated AST dump for compiler debugging (Phase 0).

use crate::ast::{
    ArrayDeclaration, Assignment, Block, ClassDeclaration, Expr, ExprKind, ForStatement,
    IfStatement, InspectStatement, ObjectGenerator, ProcedureCall, ProcedureDeclaration, Program,
    Statement, StatementKind, SwitchDeclaration, Variable, WhileStatement,
};
use crate::error::Span;
use crate::types::{Declaration, Type};
use std::fmt::Write;

/// Pretty-print `program` with source spans on statements and expressions.
pub fn dump_program(program: &Program) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Program");
    for (index, block) in program.blocks.iter().enumerate() {
        dump_block(&mut out, block, 1, &format!("blocks[{index}]"));
    }
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn span_tag(span: &Span) -> String {
    if span.start == 0 && span.end == 0 {
        "@?".to_string()
    } else {
        format!("@{}..{}", span.start, span.end)
    }
}

fn dump_block(out: &mut String, block: &Block, depth: usize, label: &str) {
    indent(out, depth);
    let _ = writeln!(out, "{label} Block(name={:?})", block.name);
    for decl in &block.declarations {
        dump_declaration(out, decl, depth + 1);
    }
    for array in &block.arrays {
        dump_array(out, array, depth + 1);
    }
    for switch in &block.switches {
        dump_switch(out, switch, depth + 1);
    }
    for procedure in &block.procedures {
        dump_procedure(out, procedure, depth + 1);
    }
    for class in &block.classes {
        dump_class(out, class, depth + 1);
    }
    for (index, stmt) in block.statements.iter().enumerate() {
        dump_statement(out, stmt, depth + 1, &format!("stmt[{index}]"));
    }
    for (index, nested) in block.body.iter().enumerate() {
        dump_block(out, nested, depth + 1, &format!("body[{index}]"));
    }
}

fn dump_declaration(out: &mut String, decl: &Declaration, depth: usize) {
    indent(out, depth);
    let names: Vec<&str> = decl.items.iter().map(|item| item.name.as_str()).collect();
    let _ = writeln!(
        out,
        "Decl {} {} {}",
        type_name(&decl.ty),
        names.join(", "),
        span_tag(&decl.span)
    );
}

fn dump_array(out: &mut String, array: &ArrayDeclaration, depth: usize) {
    indent(out, depth);
    let names: Vec<String> = array
        .segments
        .iter()
        .flat_map(|segment| segment.names.iter().cloned())
        .collect();
    let _ = writeln!(
        out,
        "Array {} {} {}",
        type_name(&array.element_type),
        names.join(", "),
        span_tag(&array.span)
    );
}

fn dump_switch(out: &mut String, switch: &SwitchDeclaration, depth: usize) {
    indent(out, depth);
    let _ = writeln!(out, "Switch {} {}", switch.name, span_tag(&switch.span));
}

fn dump_procedure(out: &mut String, procedure: &ProcedureDeclaration, depth: usize) {
    indent(out, depth);
    let _ = writeln!(
        out,
        "Procedure {} {}",
        procedure.name,
        span_tag(&procedure.span)
    );
    dump_block(out, &procedure.body, depth + 1, "body");
}

fn dump_class(out: &mut String, class: &ClassDeclaration, depth: usize) {
    indent(out, depth);
    let _ = writeln!(out, "Class {} {}", class.name, span_tag(&class.span));
    dump_block(out, &class.body, depth + 1, "body");
}

fn dump_statement(out: &mut String, stmt: &Statement, depth: usize, label: &str) {
    indent(out, depth);
    let tag = span_tag(&stmt.span);
    match &stmt.kind {
        StatementKind::Dummy => {
            let _ = writeln!(out, "{label} Dummy {tag}");
        }
        StatementKind::Assignment(assignment) => {
            let _ = writeln!(out, "{label} Assignment {tag}");
            dump_assignment(out, assignment, depth + 1);
        }
        StatementKind::If(if_stmt) => {
            let _ = writeln!(out, "{label} If {tag}");
            dump_if(out, if_stmt, depth + 1);
        }
        StatementKind::While(while_stmt) => {
            let _ = writeln!(out, "{label} While {tag}");
            dump_while(out, while_stmt, depth + 1);
        }
        StatementKind::For(for_stmt) => {
            let _ = writeln!(out, "{label} For {tag}");
            dump_for(out, for_stmt, depth + 1);
        }
        StatementKind::Goto(goto) => {
            let _ = writeln!(out, "{label} Goto {:?} {tag}", goto.target);
        }
        StatementKind::Compound(block) => {
            let _ = writeln!(out, "{label} Compound {tag}");
            dump_block(out, block, depth + 1, "block");
        }
        StatementKind::Labeled {
            label: name,
            statement,
        } => {
            let _ = writeln!(out, "{label} Labeled({name}) {tag}");
            dump_statement(out, statement, depth + 1, "body");
        }
        StatementKind::ProcedureCall(call) => {
            let _ = writeln!(out, "{label} ProcedureCall {tag}");
            dump_procedure_call(out, call, depth + 1);
        }
        StatementKind::Expr(expr) => {
            let _ = writeln!(out, "{label} ExprStmt {tag}");
            dump_expr(out, expr, depth + 1, "expr");
        }
        StatementKind::ObjectGenerator(generator) => {
            let _ = writeln!(out, "{label} ObjectGenerator {tag}");
            dump_object_generator(out, generator, depth + 1);
        }
        StatementKind::Inner { label: inner } => {
            let _ = writeln!(out, "{label} Inner({inner:?}) {tag}");
        }
        StatementKind::Inspect(inspect) => {
            let _ = writeln!(out, "{label} Inspect {tag}");
            dump_inspect(out, inspect, depth + 1);
        }
        StatementKind::Activate(_) => {
            let _ = writeln!(out, "{label} Activate {tag}");
        }
        StatementKind::Reactivate(_) => {
            let _ = writeln!(out, "{label} Reactivate {tag}");
        }
    }
}

fn dump_assignment(out: &mut String, assignment: &Assignment, depth: usize) {
    dump_variable(out, &assignment.lhs, depth, "lhs");
    indent(out, depth);
    let _ = writeln!(out, "op {:?}", assignment.operator);
    match &assignment.rhs {
        crate::ast::AssignmentRhs::Expr(expr) => dump_expr(out, expr, depth, "rhs"),
        crate::ast::AssignmentRhs::Chain(inner) => {
            indent(out, depth);
            let _ = writeln!(out, "rhs Chain");
            dump_assignment(out, inner, depth + 1);
        }
    }
}

fn dump_variable(out: &mut String, variable: &Variable, depth: usize, label: &str) {
    indent(out, depth);
    match variable {
        Variable::Simple(name) => {
            let _ = writeln!(out, "{label} Simple({name})");
        }
        Variable::Subscripted { name, subscripts } => {
            let _ = writeln!(out, "{label} Subscripted({name})");
            for (index, subscript) in subscripts.iter().enumerate() {
                dump_expr(out, subscript, depth + 1, &format!("sub[{index}]"));
            }
        }
        Variable::Qua { object, class_name } => {
            let _ = writeln!(out, "{label} Qua({class_name})");
            dump_variable(out, object, depth + 1, "object");
        }
        Variable::Remote { object, attribute } => {
            let _ = writeln!(out, "{label} Remote(.{attribute})");
            dump_variable(out, object, depth + 1, "object");
        }
        Variable::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            let _ = writeln!(out, "{label} RemoteCall(.{attribute})");
            dump_variable(out, object, depth + 1, "object");
            for (index, arg) in arguments.iter().enumerate() {
                dump_expr(out, arg, depth + 1, &format!("arg[{index}]"));
            }
        }
    }
}

fn dump_if(out: &mut String, if_stmt: &IfStatement, depth: usize) {
    dump_expr(out, &if_stmt.condition, depth, "cond");
    dump_statement(out, &if_stmt.then_branch, depth, "then");
    if let Some(else_branch) = &if_stmt.else_branch {
        dump_statement(out, else_branch, depth, "else");
    }
}

fn dump_while(out: &mut String, while_stmt: &WhileStatement, depth: usize) {
    dump_expr(out, &while_stmt.condition, depth, "cond");
    dump_statement(out, &while_stmt.body, depth, "body");
}

fn dump_for(out: &mut String, for_stmt: &ForStatement, depth: usize) {
    indent(out, depth);
    let _ = writeln!(out, "var {}", for_stmt.variable);
    dump_statement(out, &for_stmt.body, depth, "body");
}

fn dump_procedure_call(out: &mut String, call: &ProcedureCall, depth: usize) {
    indent(out, depth);
    let _ = writeln!(out, "name {}", call.name);
    for (index, arg) in call.arguments.iter().enumerate() {
        dump_expr(out, arg, depth, &format!("arg[{index}]"));
    }
}

fn dump_object_generator(out: &mut String, generator: &ObjectGenerator, depth: usize) {
    indent(out, depth);
    let _ = writeln!(out, "class {}", generator.class_name);
    for (index, arg) in generator.arguments.iter().enumerate() {
        dump_expr(out, arg, depth, &format!("arg[{index}]"));
    }
}

fn dump_inspect(out: &mut String, inspect: &InspectStatement, depth: usize) {
    dump_expr(out, &inspect.object, depth, "object");
    for (index, when) in inspect.when_clauses.iter().enumerate() {
        indent(out, depth);
        let _ = writeln!(out, "when[{index}] {}", when.class_name);
        dump_statement(out, &when.body, depth + 1, "body");
    }
    if let Some(do_clause) = &inspect.do_clause {
        dump_statement(out, do_clause, depth, "do");
    }
    if let Some(otherwise) = &inspect.otherwise {
        dump_statement(out, otherwise, depth, "otherwise");
    }
}

fn dump_expr(out: &mut String, expr: &Expr, depth: usize, label: &str) {
    indent(out, depth);
    let tag = span_tag(&expr.span);
    match &expr.kind {
        ExprKind::StringLiteral(value) => {
            let _ = writeln!(out, "{label} String({value:?}) {tag}");
        }
        ExprKind::CharacterLiteral(value) => {
            let _ = writeln!(out, "{label} Character({value:?}) {tag}");
        }
        ExprKind::BooleanLiteral(value) => {
            let _ = writeln!(out, "{label} Boolean({value}) {tag}");
        }
        ExprKind::Notext => {
            let _ = writeln!(out, "{label} Notext {tag}");
        }
        ExprKind::NumberLiteral { lexeme, kind } => {
            let _ = writeln!(out, "{label} Number({lexeme:?}, {kind:?}) {tag}");
        }
        ExprKind::Variable(variable) => {
            let _ = writeln!(out, "{label} Variable {tag}");
            dump_variable(out, variable, depth + 1, "var");
        }
        ExprKind::Unary { op, operand } => {
            let _ = writeln!(out, "{label} Unary({op:?}) {tag}");
            dump_expr(out, operand, depth + 1, "operand");
        }
        ExprKind::Binary { op, left, right } => {
            let _ = writeln!(out, "{label} Binary({op:?}) {tag}");
            dump_expr(out, left, depth + 1, "left");
            dump_expr(out, right, depth + 1, "right");
        }
        ExprKind::Relation { op, left, right } => {
            let _ = writeln!(out, "{label} Relation({op:?}) {tag}");
            dump_expr(out, left, depth + 1, "left");
            dump_expr(out, right, depth + 1, "right");
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            let _ = writeln!(out, "{label} IfExpr {tag}");
            dump_expr(out, condition, depth + 1, "cond");
            dump_expr(out, then_expr, depth + 1, "then");
            dump_expr(out, else_expr, depth + 1, "else");
        }
        ExprKind::Paren(inner) => {
            let _ = writeln!(out, "{label} Paren {tag}");
            dump_expr(out, inner, depth + 1, "inner");
        }
        ExprKind::FunctionCall { name, arguments } => {
            let _ = writeln!(out, "{label} FunctionCall({name}) {tag}");
            for (index, arg) in arguments.iter().enumerate() {
                dump_expr(out, arg, depth + 1, &format!("arg[{index}]"));
            }
        }
        ExprKind::RemoteAccess { object, attribute } => {
            let _ = writeln!(out, "{label} RemoteAccess(.{attribute}) {tag}");
            dump_expr(out, object, depth + 1, "object");
        }
        ExprKind::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            let _ = writeln!(out, "{label} RemoteCall(.{attribute}) {tag}");
            dump_expr(out, object, depth + 1, "object");
            for (index, arg) in arguments.iter().enumerate() {
                dump_expr(out, arg, depth + 1, &format!("arg[{index}]"));
            }
        }
        ExprKind::None => {
            let _ = writeln!(out, "{label} None {tag}");
        }
        ExprKind::New {
            class_name,
            arguments,
        } => {
            let _ = writeln!(out, "{label} New({class_name}) {tag}");
            if let Some(arguments) = arguments {
                for (index, arg) in arguments.iter().enumerate() {
                    dump_expr(out, arg, depth + 1, &format!("arg[{index}]"));
                }
            }
        }
        ExprKind::This(class_name) => {
            let _ = writeln!(out, "{label} This({class_name}) {tag}");
        }
        ExprKind::Qua { object, class_name } => {
            let _ = writeln!(out, "{label} Qua({class_name}) {tag}");
            dump_expr(out, object, depth + 1, "object");
        }
    }
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Integer { short: true } => "short integer".into(),
        Type::Integer { short: false } => "integer".into(),
        Type::Real { long: true } => "long real".into(),
        Type::Real { long: false } => "real".into(),
        Type::Boolean => "boolean".into(),
        Type::Character => "character".into(),
        Type::Text => "text".into(),
        Type::ObjectRef(name) => format!("ref({name})"),
        Type::Array { element, dims } => format!("array[{dims}] {}", type_name(element)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::test_support::parse_program;

    #[test]
    fn dump_includes_assignment_span() {
        let program = parse_program("begin integer x; x := 1; end;");
        let dump = dump_program(&program);
        assert!(dump.contains("Assignment @"), "{dump}");
        assert!(dump.contains("Number("), "{dump}");
        assert!(
            dump.contains("Decl integer x @"),
            "expected spanned decl, got {dump}"
        );
    }
}
