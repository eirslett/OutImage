//! §1.5 number literal tokenization (Simula Standard).

mod common;

use outimage::lex::{NumberKind, TokenKind};
use outimage::source::SourceFile;

fn tokenize(source: &str) -> Vec<TokenKind> {
    outimage::lex::tokenize(&SourceFile::anonymous(source))
        .expect("test input should tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

fn number_token(kind: NumberKind, lexeme: &str) -> TokenKind {
    TokenKind::NumberLiteral {
        kind,
        lexeme: lexeme.into(),
    }
}

#[test]
fn tokenizes_integer_literals() {
    assert_eq!(tokenize("0"), vec![number_token(NumberKind::Integer, "0")]);
    assert_eq!(
        tokenize("2R1010"),
        vec![number_token(NumberKind::Integer, "2R1010")]
    );
    assert_eq!(
        tokenize("16RFFFE"),
        vec![number_token(NumberKind::Integer, "16RFFFE")]
    );
    assert_eq!(
        tokenize("8r76501"),
        vec![number_token(NumberKind::Integer, "8r76501")]
    );
    assert_eq!(
        tokenize("16R000a"),
        vec![number_token(NumberKind::Integer, "16R000a")]
    );
}

#[test]
fn tokenizes_real_literals() {
    assert_eq!(tokenize("2&1"), vec![number_token(NumberKind::Real, "2&1")]);
    assert_eq!(
        tokenize("2.0&+1"),
        vec![number_token(NumberKind::Real, "2.0&+1")]
    );
    assert_eq!(
        tokenize(".2&2"),
        vec![number_token(NumberKind::Real, ".2&2")]
    );
    assert_eq!(
        tokenize("20.0"),
        vec![number_token(NumberKind::Real, "20.0")]
    );
    assert_eq!(
        tokenize("200&-1"),
        vec![number_token(NumberKind::Real, "200&-1")]
    );
}

#[test]
fn tokenizes_long_real_literals() {
    assert_eq!(
        tokenize("2.345_678&&0"),
        vec![number_token(NumberKind::LongReal, "2.345_678&&0")]
    );
}

#[test]
fn tokenizes_negative_integer_as_minus_and_unsigned_number() {
    assert_eq!(
        tokenize("-1"),
        vec![TokenKind::Minus, number_token(NumberKind::Integer, "1"),]
    );
}

#[test]
fn tokenizes_numbers_fixture() {
    let source = common::fixture("numbers/numbers.sim");
    let kinds = tokenize(&source);

    let numbers: Vec<_> = kinds
        .iter()
        .filter_map(|kind| match kind {
            TokenKind::NumberLiteral { kind, lexeme } => Some((*kind, lexeme.as_str())),
            _ => None,
        })
        .collect();

    assert_eq!(
        numbers,
        vec![
            (NumberKind::Integer, "0"),
            (NumberKind::Integer, "1"),
            (NumberKind::Real, "2&1"),
            (NumberKind::Real, "2.0&+1"),
            (NumberKind::Real, ".2&2"),
            (NumberKind::Real, "20.0"),
            (NumberKind::Real, "200&-1"),
            (NumberKind::LongReal, "2.345_678&&0"),
            (NumberKind::Integer, "2R1010"),
            (NumberKind::Integer, "16RFFFE"),
            (NumberKind::Integer, "8r76501"),
            (NumberKind::Integer, "16R000a"),
        ]
    );
}

#[test]
fn decimal_point_without_fraction_digit_is_dot_token() {
    assert_eq!(
        tokenize("2. x"),
        vec![
            number_token(NumberKind::Integer, "2"),
            TokenKind::Dot,
            TokenKind::Identifier("x".into()),
        ]
    );
}

#[test]
fn bare_ampersand_operators_remain_operators() {
    assert_eq!(
        tokenize("& &&"),
        vec![TokenKind::Ampersand, TokenKind::AmpersandAmpersand]
    );
}

#[test]
fn rejects_invalid_radix_digit() {
    let error = outimage::lex::tokenize(&SourceFile::anonymous("2R2")).unwrap_err();
    assert!(error.message.contains("radix-2"));
}

#[test]
fn rejects_empty_radix_digits() {
    let error = outimage::lex::tokenize(&SourceFile::anonymous("8R")).unwrap_err();
    assert!(error.message.contains("radix-8"));
}
