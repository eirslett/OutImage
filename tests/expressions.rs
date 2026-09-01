mod common;

use outimage::ast::{BinaryOp, Expr, ExprKind, RelationOp, Statement, StatementKind, UnaryOp};
use outimage::source::SourceFile;

fn parse_source(source: &str) -> outimage::ast::Program {
    outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(source)).unwrap())
        .unwrap()
}

fn parse_fixture(name: &str) -> outimage::ast::Program {
    parse_source(&common::fixture(&format!("expressions/{name}")))
}

fn assignment_rhs(statement: &Statement) -> &Expr {
    let StatementKind::Assignment(assignment) = &statement.kind else {
        panic!("expected assignment");
    };
    assignment
        .rhs
        .as_expr()
        .unwrap_or_else(|| panic!("expected expression rhs, found chained assignment"))
}

fn assignment_rhs_opt(statement: &Statement) -> Option<&Expr> {
    match &statement.kind {
        StatementKind::Assignment(assignment) => assignment.rhs.as_expr(),
        _ => None,
    }
}

fn relation_ops(program: &outimage::ast::Program) -> Vec<RelationOp> {
    program.blocks[0]
        .statements
        .iter()
        .map(|statement| {
            let ExprKind::Relation { op, .. } = &assignment_rhs(statement).kind else {
                panic!("expected relation expression");
            };
            *op
        })
        .collect()
}

#[test]
fn arithmetic_precedence_fixture_parses() {
    let program = parse_fixture("arithmetic_precedence.sim");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert!(matches!(
        assignment.rhs.as_expr().map(|expr| &expr.kind),
        Some(ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        })
    ));
}

#[test]
fn boolean_operators_fixture_parses() {
    let program = parse_fixture("boolean_operators.sim");
    assert_eq!(program.blocks[0].statements.len(), 3);
}

#[test]
fn relations_fixture_parses() {
    let program = parse_fixture("relations.sim");
    assert_eq!(
        relation_ops(&program),
        vec![
            RelationOp::Lt,
            RelationOp::Le,
            RelationOp::Eq,
            RelationOp::Ge,
            RelationOp::Gt,
            RelationOp::Ne,
        ]
    );
}

#[test]
fn relation_alternates_fixture_parses() {
    let program = parse_fixture("relation_alternates.sim");
    assert_eq!(
        relation_ops(&program),
        vec![
            RelationOp::Lt,
            RelationOp::Le,
            RelationOp::Eq,
            RelationOp::Ge,
            RelationOp::Gt,
            RelationOp::Ne,
        ]
    );
}

#[test]
fn reference_relations_fixture_parses() {
    let program = parse_fixture("reference_relations.sim");
    assert_eq!(
        relation_ops(&program),
        vec![RelationOp::RefEq, RelationOp::RefNe]
    );
}

#[test]
fn is_relation_fixture_parses_and_runs() {
    let program = parse_fixture("is_relation.sim");
    assert!(
        program.blocks[0].statements.iter().any(|statement| {
            matches!(
                assignment_rhs_opt(statement).map(|expr| &expr.kind),
                Some(ExprKind::Relation {
                    op: RelationOp::Is,
                    ..
                })
            )
        }),
        "expected an `is` relation assignment"
    );
    let source = common::fixture("expressions/is_relation.sim");
    let output = outimage::compile_str(&source).expect("is_relation fixture should run");
    assert_eq!(output, "is-node\nnot-other\nnone\n");
}

#[test]
fn in_relation_fixture_parses_and_runs() {
    let program = parse_fixture("in_relation.sim");
    let ops: Vec<_> = program.blocks[0]
        .statements
        .iter()
        .filter_map(assignment_rhs_opt)
        .filter_map(|expr| match &expr.kind {
            ExprKind::Relation { op, .. } => Some(*op),
            _ => None,
        })
        .collect();
    assert!(ops.contains(&RelationOp::In));
    assert!(ops.contains(&RelationOp::Is));
    let source = common::fixture("expressions/in_relation.sim");
    let output = outimage::compile_str(&source).expect("in_relation fixture should run");
    assert_eq!(output, "in-polar\nin-point\nnot-is-point\nnone\n");
}

#[test]
fn text_concat_fixture_runs() {
    let source = common::fixture("expressions/text_concat.sim");
    let output = outimage::compile_str(&source).expect("text concat fixture should run");
    assert_eq!(output, "hello world\n");
}

#[test]
fn conditional_fixture_parses_nested_if() {
    let program = parse_fixture("conditional.sim");
    assert!(matches!(
        assignment_rhs(&program.blocks[0].statements[0]).kind,
        ExprKind::If { .. }
    ));
}

#[test]
fn object_expressions_fixture_parses() {
    let program = parse_fixture("object_exprs.sim");
    assert_eq!(program.blocks[0].statements.len(), 3);
}

#[test]
fn expression_fixtures_compile() {
    for name in [
        "arithmetic_precedence.sim",
        "boolean_operators.sim",
        "relations.sim",
        "relation_alternates.sim",
        "is_relation.sim",
        "in_relation.sim",
        "conditional.sim",
    ] {
        let source = common::fixture(&format!("expressions/{name}"));
        outimage::compile_str(&source).unwrap_or_else(|error| {
            panic!("expression fixture {name} should compile: {error}");
        });
    }
}

#[test]
fn integer_division_evaluates() {
    let output =
        outimage::compile_str(r#"begin integer q; q := 7 // 3; OutText("ok"); OutImage; end;"#)
            .unwrap();
    assert_eq!(output, "ok\n");
}

#[test]
fn parenthesized_expression_parses() {
    let program = parse_source("begin x := (1 + 2) * 3; end;");
    let assignment = &program.blocks[0].statements[0];
    let ExprKind::Binary {
        op: BinaryOp::Mul,
        left,
        ..
    } = &assignment_rhs(assignment).kind
    else {
        panic!("expected multiplication");
    };
    assert!(matches!(left.kind, ExprKind::Paren(_)));
}

#[test]
fn unary_minus_on_variable_parses() {
    let program = parse_source("begin x := -a + 1; end;");
    let assignment = &program.blocks[0].statements[0];
    let ExprKind::Binary { left, .. } = &assignment_rhs(assignment).kind else {
        panic!("expected addition");
    };
    assert!(matches!(
        left.kind,
        ExprKind::Unary {
            op: UnaryOp::Minus,
            ..
        }
    ));
}
