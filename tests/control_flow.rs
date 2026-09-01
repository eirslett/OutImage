//! §4.2–§4.5 control-flow statements.

mod common;

use outimage::ast::StatementKind;
use outimage::source::SourceFile;

fn parse_source(source: &str) -> outimage::ast::Program {
    outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(source)).unwrap())
        .unwrap()
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

fn compile_fixture(name: &str, output: &str) -> String {
    let source = common::fixture(&format!("control_flow/{name}"));
    outimage::compile_str(&inject_before_program_end(&source, output)).unwrap()
}

#[test]
fn goto_labelled_if_branch_fixture_evaluates() {
    let output = compile_fixture("goto_labelled_if_branch.sim", "    OutInt(x, 0); OutImage;");
    assert_eq!(output, "1\n");
}

#[test]
fn if_positive_fixture_evaluates() {
    let output = compile_fixture("if_positive.sim", "  OutInt(n, 0); OutImage;");
    assert_eq!(output, "1\n");
}

#[test]
fn while_sum_fixture_evaluates() {
    let output = compile_fixture(
        "while_sum.sim",
        "  OutInt(sum, 0); OutImage;\n  OutInt(i, 0); OutImage;",
    );
    assert_eq!(output, "15\n6\n");
}

#[test]
fn for_step_until_fixture_evaluates() {
    let output = compile_fixture(
        "for_step_until.sim",
        "  OutInt(sum, 0); OutImage;\n  OutInt(i, 0); OutImage;",
    );
    assert_eq!(output, "15\n6\n");
}

#[test]
fn for_value_list_fixture_evaluates() {
    let output = compile_fixture(
        "for_value_list.sim",
        "  OutInt(total, 0); OutImage;\n  OutInt(i, 0); OutImage;",
    );
    assert_eq!(output, "6\n3\n");
}

#[test]
fn goto_label_fixture_evaluates() {
    let output = compile_fixture("goto_label.sim", "  OutInt(x, 0); OutImage;");
    assert_eq!(output, "42\n");
}

#[test]
fn goto_outside_procedure_abandons_call() {
    let output = compile_fixture("goto_outside_procedure.sim", "    OutInt(x, 0); OutImage;");
    assert_eq!(output, "2\n");
}

#[test]
fn switch_subclass_visibility_fixture_evaluates() {
    let output = compile_fixture(
        "switch_subclass_visibility.sim",
        "    OutText(\"ok\"); OutImage;",
    );
    assert_eq!(output, "ok\n");
}

#[test]
fn switch_class_attribute_fixture_evaluates() {
    let source = common::fixture("control_flow/switch_class_attribute.sim");
    let output = outimage::compile_str(&source).expect("switch class attribute fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn trailing_label_fixture_evaluates() {
    let output = compile_fixture("trailing_label.sim", "  OutInt(x, 0); OutImage;");
    assert_eq!(output, "0\n");
}

#[test]
fn multiple_trailing_labels_fixture_evaluates() {
    let output = compile_fixture("multiple_trailing_labels.sim", "  OutInt(x, 0); OutImage;");
    assert_eq!(output, "0\n");
}

#[test]
fn if_then_else_parses_and_evaluates() {
    let output = outimage::compile_str(
        "begin integer n; n := 0; if false then n := 1 else n := 2; OutInt(n, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(output, "2\n");
}

#[test]
fn nested_if_else_fixture_parses() {
    let program = parse_source("begin if false then abort: x := 1 else if true then y := 2; end;");
    assert!(matches!(
        program.blocks[0].statements[0].kind,
        StatementKind::If(_)
    ));
}
