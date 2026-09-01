use outimage::lex::{Keyword, TokenKind};
use outimage::source::SourceFile;

fn token_kinds(source: &str) -> Vec<TokenKind> {
    outimage::lex::tokenize(&SourceFile::anonymous(source))
        .expect("test input should tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

#[test]
fn tokenizes_begin_keyword() {
    assert_eq!(
        token_kinds("begin"),
        vec![TokenKind::Keyword(Keyword::Begin)]
    );
}

#[test]
fn tokenizes_identifier() {
    assert_eq!(
        token_kinds("MyClass"),
        vec![TokenKind::Identifier("MyClass".into())]
    );
}

#[test]
fn tokenizes_begin_end_fixture() {
    insta::assert_debug_snapshot!(token_kinds("begin\nend;"));
}
