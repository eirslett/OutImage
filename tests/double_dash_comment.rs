//! `--` line comments (sim extension; `--no-double-dash-comments` disables).

mod common;

use outimage::lex::{Keyword, LexOptions, TokenKind, tokenize_with_options};
use outimage::source::SourceFile;
use outimage::{CompileOptions, compile_str, compile_with_options};

fn kinds(source: &str) -> Vec<TokenKind> {
    outimage::lex::tokenize(&SourceFile::anonymous(source))
        .expect("tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

#[test]
fn trailing_fixture_strips_comment() {
    let source = common::fixture("double-dash-comment/trailing.sim");
    let kinds = kinds(&source);
    assert!(
        !kinds.iter().any(|kind| matches!(kind, TokenKind::Minus)),
        "default lexer should not emit minus tokens for `--` comments: {kinds:?}"
    );
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::Integer),
            TokenKind::Identifier(_),
            TokenKind::Semicolon,
            TokenKind::Identifier(_),
            TokenKind::Assign,
            TokenKind::NumberLiteral { .. },
            TokenKind::Semicolon,
            TokenKind::Identifier(_),
            TokenKind::LeftParen,
            TokenKind::StringLiteral(_),
            TokenKind::RightParen,
            TokenKind::Semicolon,
            TokenKind::Identifier(_),
            TokenKind::Semicolon,
            TokenKind::Keyword(Keyword::End),
            ..
        ]
    ));
}

#[test]
fn fixtures_compile_and_run() {
    for file in ["trailing.sim", "whole_line.sim"] {
        let source = common::fixture(&format!("double-dash-comment/{file}"));
        let output =
            compile_str(&source).unwrap_or_else(|error| panic!("{file} should compile: {error}"));
        assert!(output.contains("ok"), "{file} output was {output:?}");
    }
}

#[test]
fn string_dashes_are_not_comments() {
    let output = compile_str(r#"begin OutText("--- START"); OutImage; end;"#)
        .expect("string with dashes should compile");
    assert!(output.contains("--- START"), "got {output:?}");
}

#[test]
fn disabled_flag_treats_double_dash_as_minuses() {
    let options = CompileOptions {
        allow_double_dash_comments: false,
        ..CompileOptions::for_run()
    };
    let source = SourceFile::anonymous("begin integer x; x := 1--2; OutInt(x, 1); OutImage; end;");
    match compile_with_options(&source, &options) {
        Ok(outimage::CompileResult::Interpreted(output)) => {
            assert!(output.contains("3"), "1 - -2 should be 3, got {output:?}");
        }
        other => panic!("expected interpreted result, got {other:?}"),
    }
}

#[test]
fn disabled_flag_tokenizes_two_minuses() {
    let kinds = tokenize_with_options(
        &SourceFile::anonymous("1--2"),
        &LexOptions {
            allow_double_dash_comments: false,
            ..LexOptions::default()
        },
    )
    .expect("tokenize")
    .tokens
    .into_iter()
    .map(|token| token.kind)
    .collect::<Vec<_>>();
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::NumberLiteral { .. },
            TokenKind::Minus,
            TokenKind::Minus,
            TokenKind::NumberLiteral { .. },
        ]
    ));
}
