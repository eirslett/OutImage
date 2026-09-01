//! §4.6–§4.11 remaining statements: procedures, object generator, inspect, dummy.

mod common;

use outimage::ast::StatementKind;
use outimage::source::SourceFile;

fn parse_source(source: &str) -> outimage::ast::Program {
    outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(source)).unwrap())
        .unwrap()
}

fn parse_fixture(name: &str) -> outimage::ast::Program {
    parse_source(&common::fixture(&format!("statements/{name}")))
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

#[test]
fn dummy_statement_fixture_parses() {
    let program = parse_fixture("dummy_statement.sim");
    assert!(matches!(
        program.blocks[0].statements[1].kind,
        StatementKind::Dummy
    ));
}

#[test]
fn labeled_procedure_call_fixture_parses() {
    let program = parse_fixture("labeled_procedure_call.sim");
    let StatementKind::Labeled { label, statement } = &program.blocks[0].statements[0].kind else {
        panic!("expected labeled procedure call");
    };
    assert_eq!(label, "fanfare");
    assert!(matches!(statement.kind, StatementKind::ProcedureCall(_)));
}

#[test]
fn procedure_call_fixture_evaluates() {
    let output =
        compile_fixture_with_output("statements/procedure_call.sim", "  OutInt(r, 0); OutImage;");
    assert_eq!(output, "25\n");
}

#[test]
fn object_generator_fixture_runs_class_body() {
    let source = common::fixture("statements/object_generator.sim");
    outimage::compile_str(&source).unwrap();
}

#[test]
fn inspect_when_fixture_evaluates() {
    let output = compile_fixture_with_output(
        "statements/inspect_when.sim",
        "  OutInt(picked, 0); OutImage;",
    );
    assert_eq!(output, "1\n");
}

#[test]
fn inspect_otherwise_fixture_evaluates() {
    let output = compile_fixture_with_output(
        "statements/inspect_otherwise.sim",
        "  OutInt(picked, 0); OutImage;",
    );
    assert_eq!(output, "0\n");
}

#[test]
fn inspect_do_remote_attribute_fixture_evaluates() {
    let output = compile_fixture_with_output(
        "statements/inspect_do_remote_attribute.sim",
        "    OutInt(result, 0); OutImage;",
    );
    assert_eq!(output, "2\n");
}

#[test]
fn nested_inspect_scoping_fixture_evaluates() {
    let output = compile_fixture_with_output(
        "statements/nested_inspect_scoping.sim",
        "    OutInt(seen_inner, 0); OutImage;\n    OutInt(seen_outer, 0); OutImage;",
    );
    assert_eq!(output, "2\n1\n");
}

#[test]
fn weekend_in_comment_fixture_compiles() {
    let source = common::fixture("end-comment/weekend_in_comment.sim");
    assert!(outimage::compile_str(&source).is_ok());
}

#[test]
fn formal_procedure_parameter_fixture_evaluates() {
    let source = common::fixture("procedures/procedure_formal_restriction.sim");
    let output = outimage::compile_str(&source).unwrap();
    assert!(output.contains("10"), "expected 10, got {output:?}");
}

#[test]
fn prefixed_block_fixture_evaluates() {
    let source = common::fixture("blocks/prefixed_block.sim");
    let output = outimage::compile_str(&source).unwrap();
    assert!(output.contains('7'), "output was: {output:?}");
}

#[test]
fn prefixed_block_with_arguments_fixture_evaluates() {
    let source = common::fixture("blocks/prefixed_block_with_arguments.sim");
    let output = outimage::compile_str(&source).unwrap();
    assert!(output.contains('1'), "output was: {output:?}");
}

#[test]
fn named_block_end_fixture_compiles() {
    // §1.8.1: everything after `end` up to a delimiter is comment, so a block
    // name is only recognised when the identifier stands alone before the
    // delimiter. The fixture's `end myblock szdf` is all comment text.
    let source = common::fixture("blocks/named_block_end.sim");
    let program = parse_source(&source);
    assert_eq!(program.blocks[0].name, "");
    assert!(outimage::compile_str(&source).is_ok());

    let named = parse_source("begin integer x; x := 1; end myblock;");
    assert_eq!(named.blocks[0].name, "myblock");
}

#[test]
fn class_name_mode_parameter_fixture_rejects() {
    let source = common::fixture("objects/class_name_mode_parameter.sim");
    assert!(outimage::compile_str(&source).is_err());
}

#[test]
fn class_attribute_init_fixture_evaluates() {
    let output = compile_fixture_with_output(
        "objects/class_attribute_init.sim",
        "    OutInt(v, 0); OutImage;",
    );
    assert_eq!(output, "0\n");
}

#[test]
fn text_constant_fixture_evaluates() {
    let source = common::fixture("declarations/text_constant.sim");
    let output = outimage::compile_str(&source).unwrap();
    assert!(output.contains("hello"), "output was: {output:?}");
}

#[test]
fn array_formal_restriction_fixture_mutates_caller() {
    let source = common::fixture("procedures/array_formal_restriction.sim");
    let output = outimage::compile_str(&source).unwrap();
    assert!(output.contains("99"), "output was: {output:?}");
}

#[test]
fn full_parameter_correspondence_fixture_evaluates() {
    let source = common::fixture("procedures/full_parameter_correspondence.sim");
    let output = outimage::compile_str(&source).unwrap();
    assert!(output.contains("ok"), "output was: {output:?}");
}
