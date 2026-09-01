//! §4.1 assignment statements.

mod common;

use outimage::ast::{AssignOperator, AssignmentRhs, Expr, ExprKind, StatementKind, Variable};
use outimage::source::SourceFile;

fn parse_source(source: &str) -> outimage::ast::Program {
    outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(source)).unwrap())
        .unwrap()
}

fn parse_fixture(name: &str) -> outimage::ast::Program {
    parse_source(&common::fixture(&format!("assignments/{name}")))
}

fn inject_before_program_end(source: &str, stmts: &str) -> String {
    let trimmed = source.trim_end();
    if trimmed.ends_with("end;") {
        let prefix = &source[..source.trim_end().len() - 4];
        format!("{prefix}{stmts}\nend;")
    } else if trimmed.ends_with("end") {
        let prefix = &source[..source.trim_end().len() - 3];
        format!("{prefix}{stmts}\nend")
    } else {
        panic!("fixture missing program end");
    }
}

fn compile_fixture_with_output(relative_path: &str, output_stmts: &str) -> String {
    let source = common::fixture(relative_path);
    outimage::compile_str(&inject_before_program_end(&source, output_stmts)).unwrap()
}

fn chain_links(assignment: &outimage::ast::Assignment) -> Vec<&Variable> {
    let mut links = vec![&assignment.lhs];
    let mut current = assignment;
    while let AssignmentRhs::Chain(inner) = &current.rhs {
        links.push(&inner.lhs);
        current = inner.as_ref();
    }
    links
}

#[test]
fn chained_value_fixture_parses() {
    let program = parse_fixture("chained_value.sim");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(
        chain_links(assignment),
        vec![
            &Variable::Simple("a".into()),
            &Variable::Simple("b".into()),
            &Variable::Simple("c".into()),
        ]
    );
    assert!(matches!(assignment.rhs, AssignmentRhs::Chain(_)));
    if let AssignmentRhs::Chain(inner) = &assignment.rhs {
        assert!(matches!(inner.rhs, AssignmentRhs::Chain(_)));
    }
}

#[test]
fn chained_value_fixture_evaluates() {
    let output = compile_fixture_with_output(
        "assignments/chained_value.sim",
        "  OutInt(a, 0); OutImage;\n  OutInt(b, 0); OutImage;\n  OutInt(c, 0); OutImage;",
    );
    assert_eq!(output, "42\n42\n42\n");
}

#[test]
#[allow(clippy::approx_constant)] // fixture literal is `3.14`, not π
fn chained_mixed_types_fixture_evaluates_with_conversion() {
    let output = compile_fixture_with_output(
        "assignments/chained_mixed_types.sim",
        "  OutFix(y, 2, 8); OutImage;\n  OutInt(i, 0); OutImage;\n  OutFix(x, 1, 8); OutImage;",
    );
    assert_eq!(output, "    3.14\n3\n     3.0\n");
}

#[test]
fn reference_assignment_fixture_parses() {
    let program = parse_fixture("reference_assignment.sim");
    assert_eq!(program.blocks[0].statements.len(), 2);

    let StatementKind::Assignment(first) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(first.operator, AssignOperator::AssignAlt);
    assert_eq!(first.rhs, AssignmentRhs::Expr(Expr::dummy(ExprKind::None)));

    let StatementKind::Assignment(second) = &program.blocks[0].statements[1].kind else {
        panic!("expected assignment");
    };
    assert_eq!(second.operator, AssignOperator::AssignAlt);
}

#[test]
fn chained_reference_fixture_parses() {
    let program = parse_fixture("chained_reference.sim");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(assignment.operator, AssignOperator::AssignAlt);
    assert_eq!(
        chain_links(assignment),
        vec![&Variable::Simple("a".into()), &Variable::Simple("b".into()),]
    );
}

#[test]
fn chained_remote_reference_fixture_parses() {
    let program = parse_fixture("chained_remote_reference.sim");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(assignment.operator, AssignOperator::AssignAlt);
    assert_eq!(
        assignment.lhs,
        Variable::Remote {
            object: Box::new(Variable::Simple("r".into())),
            attribute: "last".into(),
        }
    );
    let AssignmentRhs::Chain(inner) = &assignment.rhs else {
        panic!("expected chained rhs");
    };
    assert_eq!(
        inner.lhs,
        Variable::Remote {
            object: Box::new(Variable::Remote {
                object: Box::new(Variable::Simple("r".into())),
                attribute: "last".into(),
            }),
            attribute: "next".into(),
        }
    );
}

#[test]
fn multi_level_remote_expression_parses() {
    let program = parse_source("begin x := r.last.next; end;");
    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(
        assignment.rhs,
        AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Remote {
            object: Box::new(Variable::Remote {
                object: Box::new(Variable::Simple("r".into())),
                attribute: "last".into(),
            }),
            attribute: "next".into(),
        })))
    );
}

#[test]
fn remote_text_sub_lhs_assignment_evaluates() {
    let output = compile_fixture_with_output(
        "assignments/remote_procedure_lhs.sim",
        "  OutChar(c); OutImage;",
    );
    assert_eq!(output, "T\n");
}

#[test]
fn assignment_fixtures_pass_semantic_analysis() {
    for name in [
        "chained_value.sim",
        "chained_mixed_types.sim",
        "reference_assignment.sim",
        "chained_reference.sim",
        "chained_remote_reference.sim",
        "remote_procedure_lhs.sim",
    ] {
        let source = common::fixture(&format!("assignments/{name}"));
        let tokens = outimage::lex::tokenize(&SourceFile::anonymous(&source)).unwrap();
        let program = outimage::parse::parse(&tokens).unwrap();
        outimage::semantic::analyze(&program).unwrap_or_else(|error| {
            panic!("assignment fixture {name} should pass semantic analysis: {error}");
        });
    }
}
