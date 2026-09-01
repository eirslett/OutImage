//! §1.4 identifier tokenization (Simula Standard).

mod common;

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

fn identifiers(source: &str) -> Vec<String> {
    tokenize(source)
        .into_iter()
        .filter_map(|kind| match kind {
            TokenKind::Identifier(name) => Some(name),
            _ => None,
        })
        .collect()
}

#[test]
fn identifier_allows_letters_digits_and_underscores() {
    assert_eq!(
        identifiers("foo_bar a17 delta"),
        vec![
            "foo_bar".to_string(),
            "a17".to_string(),
            "delta".to_string(),
        ]
    );
}

#[test]
fn keywords_are_not_identifiers() {
    assert_eq!(
        tokenize("begin integer real procedure"),
        vec![
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::Integer),
            TokenKind::Keyword(Keyword::Real),
            TokenKind::Keyword(Keyword::Procedure),
        ]
    );
}

#[test]
fn keyword_spelling_is_case_insensitive() {
    assert_eq!(tokenize("Begin"), vec![TokenKind::Keyword(Keyword::Begin)]);
    assert_eq!(tokenize("BEGIN"), vec![TokenKind::Keyword(Keyword::Begin)]);
}

#[test]
fn identifiers_preserve_spelling() {
    assert_eq!(
        identifiers("MyClass myclass"),
        vec!["MyClass".to_string(), "myclass".to_string()]
    );
}

#[test]
fn identifier_suffix_after_keyword_spelling_is_not_a_keyword() {
    assert_eq!(
        identifiers("BeginX endcomment"),
        vec!["BeginX".to_string(), "endcomment".to_string()]
    );
}

#[test]
fn identifier_fixture_from_lex_corpus() {
    assert_eq!(identifiers("MyClass"), vec!["MyClass".to_string()]);
}
