//! §3.1 variable references and assignments, ported from tree-sitter corpus.

mod common;

use outimage::ast::{
    ArithmeticLiteralKind, AssignOperator, Assignment, AssignmentRhs, Expr, ExprKind, Program,
    Statement, StatementKind, Variable,
};
use outimage::source::SourceFile;

fn parse_source(source: &str) -> Program {
    outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(source)).unwrap())
        .unwrap()
}

fn parse_fixture(name: &str) -> Program {
    parse_source(&common::fixture(&format!("variables/{name}")))
}

#[test]
fn simple_variable_assignment() {
    let program = parse_fixture("simple_variable.sim");
    assert_eq!(
        program.blocks[0].statements[0],
        Statement::dummy(StatementKind::Assignment(Assignment {
            lhs: Variable::Simple("A".into()),
            operator: AssignOperator::Assign,
            rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Simple(
                "B".into()
            )))),
        }))
    );
}

#[test]
fn remote_identifier_assignment() {
    let program = parse_fixture("remote_identifier.sim");
    assert_eq!(
        program.blocks[0].statements[0],
        Statement::dummy(StatementKind::Assignment(Assignment {
            lhs: Variable::Simple("A".into()),
            operator: AssignOperator::Assign,
            rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Remote {
                object: Box::new(Variable::Simple("B".into())),
                attribute: "C".into(),
            }))),
        }))
    );
}

#[test]
fn chained_value_assignment_parses() {
    let program = parse_source("begin a := b := 1; end;");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(assignment.lhs, Variable::Simple("a".into()));
    assert!(matches!(assignment.rhs, AssignmentRhs::Chain(_)));
}

#[test]
fn reference_assignment_parses() {
    let program = parse_source("begin r :- p; end;");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(assignment.operator, AssignOperator::AssignAlt);
}

#[test]
fn chained_assignments_evaluate_in_order() {
    let output = outimage::compile_str(
        r#"begin
            integer a, b, c;
            a := b := c := 3;
            OutInt(a, 0); OutImage;
            OutInt(b, 0); OutImage;
            OutInt(c, 0); OutImage;
        end;"#,
    )
    .unwrap();
    assert_eq!(output, "3\n3\n3\n");
}

#[test]
fn subscripted_variable_assignment() {
    let program = parse_fixture("subscripted_variable.sim");
    assert_eq!(
        program.blocks[0].statements[0],
        Statement::dummy(StatementKind::Assignment(Assignment {
            lhs: Variable::Subscripted {
                name: "A".into(),
                subscripts: vec![Expr::dummy(ExprKind::NumberLiteral {
                    lexeme: "1".into(),
                    kind: ArithmeticLiteralKind::Integer,
                })],
            },
            operator: AssignOperator::Assign,
            rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::NumberLiteral {
                lexeme: "1".into(),
                kind: ArithmeticLiteralKind::Integer,
            })),
        }))
    );
}

#[test]
fn variable_assignments_evaluate() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "simple_variable.sim",
            "begin integer A, B; B := 42; A := B; OutInt(A, 0); OutImage; end;",
            "42\n",
        ),
        (
            "remote_identifier.sim",
            r#"begin
                class BClass; integer C; begin end;
                ref(BClass) B;
                integer A;
                B :- new BClass;
                B.C := 7;
                A := B.C;
                OutInt(A, 0); OutImage;
            end;"#,
            "7\n",
        ),
        (
            "subscripted_variable.sim",
            "begin integer array A(1:1); A(1) := 1; OutInt(A(1), 0); OutImage; end;",
            "1\n",
        ),
    ];
    for (name, source, expected) in cases {
        let output = outimage::compile_str(source)
            .unwrap_or_else(|error| panic!("{name} assignment form should compile: {error}"));
        assert_eq!(output, *expected, "{name}");
    }
}

#[test]
fn numbers_fixture_assignments_parse() {
    let source = common::fixture("numbers/numbers.sim");
    let program = parse_source(&source);

    assert_eq!(program.blocks[0].statements.len(), 12);

    for statement in &program.blocks[0].statements {
        let StatementKind::Assignment(assignment) = &statement.kind else {
            panic!("expected assignment statement");
        };
        assert_eq!(assignment.lhs, Variable::Simple("a".into()));
        assert!(matches!(
            assignment.rhs.as_expr().map(|expr| &expr.kind),
            Some(ExprKind::NumberLiteral { .. })
        ));
    }
}
