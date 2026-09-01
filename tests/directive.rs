//! Processor directive fixtures (§1.1).
//!
//! `%` lines are elided at lex time (like comments). Fixtures still parse; no
//! `Directive` tokens appear in the stream and AST `directives` stay empty.

mod common;

use outimage::lex::tokenize;
use outimage::parse::parse;
use outimage::source::SourceFile;

#[test]
fn processor_directives_are_elided_and_program_parses() {
    let source = common::fixture("directive/processor_directives.sim");
    let stream = tokenize(&SourceFile::anonymous(&source)).expect("tokenize");
    let program = parse(&stream).expect("parse");
    assert!(program.directives.is_empty());
    assert_eq!(program.blocks.len(), 1);
    assert!(program.blocks[0].directives.is_empty());
}

#[test]
fn several_top_level_directives_are_elided() {
    let source = common::fixture("directive/several_top_level_directives.sim");
    let stream = tokenize(&SourceFile::anonymous(&source)).expect("tokenize");
    let program = parse(&stream).expect("parse");
    assert!(program.directives.is_empty());
}

#[test]
fn inline_directive_is_lex_error() {
    let source = common::fixture("directive/inline_directive_error.sim");
    let error = tokenize(&SourceFile::anonymous(&source)).unwrap_err();
    assert!(error.message.contains("first character"));
}
