//! Simula Standard §1.9 token-separator rules.

mod common;

use outimage::lex::tokenize;
use outimage::source::SourceFile;

fn lex_error(source: &str) -> String {
    tokenize(&SourceFile::anonymous(source))
        .unwrap_err()
        .message
}

#[test]
fn rejects_number_followed_by_identifier() {
    let message = lex_error("123abc");
    assert!(
        message.contains("separator"),
        "unexpected message: {message}"
    );
}

#[test]
fn rejects_real_followed_by_identifier() {
    assert!(lex_error("2.0x").contains("separator"));
}

#[test]
fn rejects_string_followed_by_identifier() {
    assert!(lex_error(r#""hello"world"#).contains("separator"));
}

#[test]
fn rejects_string_followed_by_number() {
    assert!(lex_error(r#""x"123"#).contains("separator"));
}

#[test]
fn rejects_identifier_followed_by_string() {
    assert!(lex_error(r#"foo"bar""#).contains("separator"));
}

#[test]
fn accepts_separator_between_word_like_tokens() {
    tokenize(&SourceFile::anonymous("123 abc")).expect("space should separate");
    tokenize(&SourceFile::anonymous(r#""hello" "world""#)).expect("space should separate");
    tokenize(&SourceFile::anonymous("123;abc")).expect("non-word token may sit between");
    tokenize(&SourceFile::anonymous("2. x")).expect("dot may sit between");
}
