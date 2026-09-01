//! Lexical analysis for Simula source.

pub(crate) mod highlight;
mod keyword;
mod lexer;
mod special;
mod token;
mod trivia;

pub use highlight::{HighlightSpan, highlight_source, highlight_spans};
pub use keyword::Keyword;
pub use lexer::{tokenize, tokenize_recovering, tokenize_with_options, tokenize_with_trivia};
pub use token::{NumberKind, Token, TokenKind, TokenStream};
pub use trivia::{Trivia, TriviaKind};

/// Lexer configuration shared with the compiler driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexOptions {
    /// When true, `[` and `]` may appear as array subscript delimiters.
    pub allow_square_bracket_subscripts: bool,
    /// When true, `--` starts a line comment through the end of the line
    /// (sim extension; Standard treats consecutive minuses as operators).
    pub allow_double_dash_comments: bool,
}

impl Default for LexOptions {
    fn default() -> Self {
        Self {
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
        }
    }
}
