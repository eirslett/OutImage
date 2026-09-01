//! Token definitions shared by the Simula lexer and parser.

use super::keyword::Keyword;
use std::ops::Range;

pub type Span = Range<usize>;

/// Simula numeric type for an unsigned number literal (Standard §1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberKind {
    Integer,
    Real,
    LongReal,
}

/// A classified lexical token with source location.
///
/// Chumsky and other parser generators typically consume `(TokenKind, Span)` pairs;
/// this struct keeps that contract stable while the lexer backend changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }

    #[cfg(test)]
    pub fn kind_only(kind: TokenKind) -> Self {
        Self { kind, span: 0..0 }
    }
}

/// The semantic category of a token.
///
/// Parser code should depend on this enum, not on lexer internals.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword(Keyword),
    Identifier(String),
    StringLiteral(String),
    /// A character constant (Standard §1.7).
    CharacterLiteral(char),
    /// An unsigned number literal (Standard §1.5).
    NumberLiteral {
        kind: NumberKind,
        lexeme: String,
    },
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `//`
    SlashSlash,
    /// `**`
    StarStar,
    /// `&` — text concatenation or exponent mark
    Ampersand,
    /// `&&` — exponent mark in long real numbers
    AmpersandAmpersand,
    /// `:=`
    Assign,
    /// `:-`
    AssignAlt,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `=`
    Eq,
    /// `>=`
    Ge,
    /// `>`
    Gt,
    /// `<>`
    Ne,
    /// `==`
    RefEq,
    /// `=/=`
    RefNe,
    /// `'` — character quote
    CharacterQuote,
    LeftParen,
    RightParen,
    /// `[` — allowed for array subscripts when enabled by compiler options
    LeftBracket,
    /// `]` — allowed for array subscripts when enabled by compiler options
    RightBracket,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `,`
    Comma,
    Semicolon,
}

/// Output of lexical analysis, ready for the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenStream {
    pub tokens: Vec<Token>,
}

impl TokenStream {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens }
    }

    pub fn as_slice(&self) -> &[Token] {
        &self.tokens
    }

    pub fn kinds(&self) -> impl Iterator<Item = &TokenKind> {
        self.tokens.iter().map(|token| &token.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_stream_exposes_kinds() {
        let stream = TokenStream::new(vec![
            Token::kind_only(TokenKind::Keyword(Keyword::Begin)),
            Token::kind_only(TokenKind::Semicolon),
        ]);

        let kinds: Vec<_> = stream.kinds().cloned().collect();
        assert_eq!(
            kinds,
            vec![TokenKind::Keyword(Keyword::Begin), TokenKind::Semicolon,]
        );
    }
}
