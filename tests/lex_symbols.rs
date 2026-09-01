//! §1.2 special symbol tokenization (Simula Standard Table 1.2).

use outimage::lex::{TokenKind, tokenize};
use outimage::source::SourceFile;

fn kinds(source: &str) -> Vec<TokenKind> {
    tokenize(&SourceFile::anonymous(source))
        .expect("test input should tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

#[test]
fn tokenizes_arithmetic_operators() {
    assert_eq!(
        kinds("+ - * / // **"),
        vec![
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::SlashSlash,
            TokenKind::StarStar,
        ]
    );
}

#[test]
fn tokenizes_ampersand_operators() {
    assert_eq!(
        kinds("& &&"),
        vec![TokenKind::Ampersand, TokenKind::AmpersandAmpersand]
    );
}

#[test]
fn tokenizes_assignment_operators() {
    assert_eq!(
        kinds(":= :-"),
        vec![TokenKind::Assign, TokenKind::AssignAlt]
    );
}

#[test]
fn tokenizes_value_relational_operators() {
    assert_eq!(
        kinds("< <= = >= > <>"),
        vec![
            TokenKind::Lt,
            TokenKind::Le,
            TokenKind::Eq,
            TokenKind::Ge,
            TokenKind::Gt,
            TokenKind::Ne,
        ]
    );
}

#[test]
fn tokenizes_reference_relational_operators() {
    assert_eq!(kinds("== =/="), vec![TokenKind::RefEq, TokenKind::RefNe]);
}

#[test]
fn tokenizes_punctuation_symbols() {
    assert_eq!(
        kinds("' : . ,"),
        vec![
            TokenKind::CharacterQuote,
            TokenKind::Colon,
            TokenKind::Dot,
            TokenKind::Comma,
        ]
    );
}

#[test]
fn max_munch_prefers_longer_operators() {
    assert_eq!(
        kinds("<= <> := :- // ** && == =/= >= =/= =="),
        vec![
            TokenKind::Le,
            TokenKind::Ne,
            TokenKind::Assign,
            TokenKind::AssignAlt,
            TokenKind::SlashSlash,
            TokenKind::StarStar,
            TokenKind::AmpersandAmpersand,
            TokenKind::RefEq,
            TokenKind::RefNe,
            TokenKind::Ge,
            TokenKind::RefNe,
            TokenKind::RefEq,
        ]
    );
}

#[test]
fn shorter_operators_split_when_not_combined() {
    assert_eq!(
        kinds("< = > / * & = / ="),
        vec![
            TokenKind::Lt,
            TokenKind::Eq,
            TokenKind::Gt,
            TokenKind::Slash,
            TokenKind::Star,
            TokenKind::Ampersand,
            TokenKind::Eq,
            TokenKind::Slash,
            TokenKind::Eq,
        ]
    );
}

#[test]
fn colon_minus_without_assign_alt_is_two_tokens() {
    assert_eq!(kinds(": -"), vec![TokenKind::Colon, TokenKind::Minus]);
}

#[test]
fn tokenizes_alternate_relational_operators() {
    assert_eq!(
        kinds("lt le eq ge gt ne"),
        vec![
            TokenKind::Lt,
            TokenKind::Le,
            TokenKind::Eq,
            TokenKind::Ge,
            TokenKind::Gt,
            TokenKind::Ne,
        ]
    );
}

#[test]
fn alternate_relational_operators_are_case_insensitive() {
    assert_eq!(
        kinds("LT Le eQ"),
        vec![TokenKind::Lt, TokenKind::Le, TokenKind::Eq]
    );
}

#[test]
fn alternate_relational_operators_match_standard_symbols() {
    assert_eq!(kinds("lt"), kinds("<"));
    assert_eq!(kinds("le"), kinds("<="));
    assert_eq!(kinds("eq"), kinds("="));
    assert_eq!(kinds("ge"), kinds(">="));
    assert_eq!(kinds("gt"), kinds(">"));
    assert_eq!(kinds("ne"), kinds("<>"));
}

#[test]
fn longer_keywords_are_not_split_into_alternate_operators() {
    assert_eq!(
        kinds("eqv"),
        vec![TokenKind::Keyword(outimage::lex::Keyword::Eqv)]
    );
    assert_eq!(kinds("ltgt"), vec![TokenKind::Identifier("ltgt".into())]);
    assert_eq!(
        kinds("letter"),
        vec![TokenKind::Identifier("letter".into())]
    );
}
