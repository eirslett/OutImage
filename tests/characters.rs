//! §1.7 character constant support (Simula Standard), ported from tree-sitter corpus.

mod common;

use outimage::ast::{AssignmentRhs, Expr, ExprKind, StatementKind};
use outimage::lex::TokenKind;
use outimage::source::SourceFile;

fn tokenize(source: &str) -> Vec<TokenKind> {
    outimage::lex::tokenize(&SourceFile::anonymous(source))
        .expect("test input should tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

fn character_literals(source: &str) -> Vec<char> {
    tokenize(source)
        .into_iter()
        .filter_map(|kind| match kind {
            TokenKind::CharacterLiteral(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn first_character(source: &str) -> char {
    character_literals(source)
        .into_iter()
        .next()
        .expect("source should contain a character literal")
}

#[test]
fn simple_character() {
    let source = common::fixture("characters/simple_character.sim");
    assert_eq!(first_character(&source), 'H');
}

#[test]
fn iso_code_character() {
    let source = common::fixture("characters/iso_code.sim");
    assert_eq!(first_character(&source), 'o');
}

#[test]
fn non_ascii_character() {
    let source = common::fixture("characters/non_ascii.sim");
    assert_eq!(first_character(&source), 'Æ');
}

#[test]
fn single_quote_character() {
    let source = common::fixture("characters/single_quote.sim");
    assert_eq!(first_character(&source), '\'');
}

#[test]
fn double_quote_character() {
    assert_eq!(first_character("'\"'"), '"');
}

#[test]
fn exclamation_in_constant_is_not_a_direct_comment() {
    assert_eq!(first_character("'!'"), '!');
    assert_eq!(first_character("C := '!';"), '!');
}

#[test]
fn lone_character_quote_remains_a_symbol() {
    assert_eq!(
        tokenize("' : . ,"),
        vec![
            TokenKind::CharacterQuote,
            TokenKind::Colon,
            TokenKind::Dot,
            TokenKind::Comma,
        ]
    );
}

#[test]
fn rejects_unterminated_character_constant() {
    let error = outimage::lex::tokenize(&SourceFile::anonymous("'H")).unwrap_err();
    assert!(error.message.contains("unterminated character constant"));
}

#[test]
fn rejects_extra_characters_in_constant() {
    let error = outimage::lex::tokenize(&SourceFile::anonymous("'!256!'")).unwrap_err();
    assert!(error.message.contains("exactly one character"));
}

#[test]
fn assignment_parses_character_literal() {
    let source = common::fixture("characters/simple_character.sim");
    let program =
        outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(&source)).unwrap())
            .unwrap();

    let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
        panic!("expected assignment");
    };
    assert_eq!(
        assignment.rhs,
        AssignmentRhs::Expr(Expr::dummy(ExprKind::CharacterLiteral('H')))
    );
}
