//! §1.6 string literal support (Simula Standard), ported from tree-sitter corpus.

mod common;

use outimage::ast::{Expr, ExprKind, Program, Statement, StatementKind};
use outimage::lex::{Keyword, TokenKind};
use outimage::source::SourceFile;

fn tokenize(source: &str) -> Vec<TokenKind> {
    outimage::lex::tokenize(&SourceFile::anonymous(source))
        .expect("test input should tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

fn string_literals(source: &str) -> Vec<String> {
    tokenize(source)
        .into_iter()
        .filter_map(|kind| match kind {
            TokenKind::StringLiteral(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn first_string(source: &str) -> String {
    string_literals(source)
        .into_iter()
        .next()
        .expect("source should contain a string literal")
}

#[test]
fn simple_string() {
    let source = common::fixture("strings/simple_string.sim");
    assert_eq!(first_string(&source), "Hello");
}

#[test]
fn shorthand_comment_in_string_is_not_a_comment() {
    let source = common::fixture("strings/shorthand_comment_in_string.sim");
    assert_eq!(first_string(&source), "!test;");
}

#[test]
fn comment_keyword_in_string_is_not_a_comment() {
    let source = common::fixture("strings/block_comment_in_string.sim");
    assert_eq!(first_string(&source), "comment test;");
}

#[test]
fn several_strings_are_joined() {
    let source = common::fixture("strings/several_strings_joined.sim");
    assert_eq!(first_string(&source), "Hello World");
}

#[test]
fn escaped_quotes_produce_literal_quote() {
    let source = common::fixture("strings/escaped_quote.sim");
    assert_eq!(first_string(&source), r#"Hello with escaped" quote"#);
}

#[test]
fn iso_codes_are_decoded() {
    let source = common::fixture("strings/iso_codes.sim");
    assert_eq!(
        first_string(&source),
        format!("Hello with {} escaped {} codes", '\u{6f}', '\u{de}')
    );
}

#[test]
fn incomplete_iso_code_is_literal_characters() {
    let source = common::fixture("strings/one_exclamation_mark.sim");
    assert_eq!(first_string(&source), "!2");
}

#[test]
fn non_ascii_characters_are_preserved() {
    let source = common::fixture("strings/non_ascii.sim");
    assert_eq!(first_string(&source), "ÆØÅ");
}

#[test]
fn spec_examples() {
    let source = common::fixture("strings/spec_examples.sim");
    assert_eq!(
        string_literals(&source),
        vec![
            String::from("Abcde"),
            String::from("ABCDE"),
            String::from("\x02ABCDE\x03"),
            String::from("!2!ABCDE!3!"),
            String::from(r#"AB" C"DE"#),
        ]
    );
}

#[test]
fn strings_may_be_separated_by_comments() {
    assert_eq!(first_string(r#""Ab" ! join; "cde""#), "Abcde");
    assert_eq!(first_string(r#""left" comment join; "right""#), "leftright");
    assert_eq!(first_string("\"Ab\" -- join\n\"cde\""), "Abcde");
}

#[test]
fn invalid_iso_codes_remain_literal() {
    assert_eq!(first_string(r#""!256!""#), "!256!");
    assert_eq!(first_string(r#""!1234!""#), "!1234!");
}

#[test]
fn empty_string() {
    assert_eq!(first_string(r#""""#), "");
}

#[test]
fn notext_is_a_keyword_not_a_string() {
    let kinds = tokenize("notext");
    assert_eq!(kinds, vec![TokenKind::Keyword(Keyword::Notext)]);
}

#[test]
fn outtext_accepts_notext() {
    let program = outimage::parse::parse(
        &outimage::lex::tokenize(&SourceFile::anonymous(
            r#"begin OutText(notext); OutImage; end;"#,
        ))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        program.blocks[0].statements[0],
        Statement::dummy(StatementKind::ProcedureCall(outimage::ast::ProcedureCall {
            name: "OutText".into(),
            arguments: vec![Expr::dummy(ExprKind::Notext)],
        }))
    );

    assert_eq!(
        outimage::compile_str(r#"begin OutText(notext); OutImage; end;"#).unwrap(),
        "\n"
    );
}

#[test]
fn outtext_accepts_spec_string_literals() {
    let cases = [
        (r#""Ab" "cde""#, "Abcde"),
        (r#""AB"" C""DE""#, r#"AB" C"DE"#),
        (r#""!2!ABCDE!3!""#, "\x02ABCDE\x03"),
    ];

    for (literal, expected) in cases {
        let source = format!(r#"begin OutText({literal}); OutImage; end;"#);
        let program: Program = outimage::parse::parse(
            &outimage::lex::tokenize(&SourceFile::anonymous(&source)).unwrap(),
        )
        .unwrap();

        let StatementKind::ProcedureCall(call) = &program.blocks[0].statements[0].kind else {
            panic!("expected procedure call");
        };
        assert_eq!(
            call.arguments,
            vec![Expr::dummy(ExprKind::StringLiteral(expected.into()))],
            "literal {literal:?}"
        );
    }
}
