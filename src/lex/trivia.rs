//! Trivia skipped by the parser-facing lexer (comments, directives, end-comments).

use super::token::Span;

/// Kind of non-token source that the lexer elides for the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriviaKind {
    /// `%` processor directive line (Standard §1.1), or a C-preprocessor line.
    Directive,
    /// `! … ;`, `comment … ;` (Standard §1.8), or `-- …` to end of line.
    Comment,
    /// Text after `end` until a delimiter (Standard §1.8.1).
    EndComment,
}

/// A span of trivia with its classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub span: Span,
}

impl Trivia {
    pub fn new(kind: TriviaKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// Drop leading/trailing Unicode whitespace from a source span.
pub fn trim_span(source: &str, span: Span) -> Option<Span> {
    let slice = source.get(span.clone())?;
    let lead = slice.len() - slice.trim_start().len();
    let trail = slice.len() - slice.trim_end().len();
    let start = span.start + lead;
    let end = span.end.saturating_sub(trail);
    (start < end).then_some(start..end)
}
