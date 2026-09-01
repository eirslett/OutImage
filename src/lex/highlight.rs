//! Lexer-based highlight spans used by the TextMate grammar tests and LSP
//! semantic tokens.
//!
//! Scope names match `editors/vscode/generate-grammar.ts` / the vscode-tmgrammar
//! fixtures under `editors/vscode/test/grammar/`.

use super::{Keyword, LexOptions, Token, TokenKind, Trivia, TriviaKind, tokenize_with_trivia};
use crate::error::CompileError;
use crate::source::SourceFile;
use std::ops::Range;

/// A classified source span. `scope` is a TextMate-style name from the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub span: Range<usize>,
    pub scope: &'static str,
}

/// Classify `source` with the default lexer options.
pub fn highlight_source(source: &str) -> Result<Vec<HighlightSpan>, CompileError> {
    highlight_source_with_options(source, &LexOptions::default())
}

/// Classify `source` using `options` (square-bracket subscripts, etc.).
pub(crate) fn highlight_source_with_options(
    source: &str,
    options: &LexOptions,
) -> Result<Vec<HighlightSpan>, CompileError> {
    let file = SourceFile::anonymous(source);
    let (tokens, trivia) = tokenize_with_trivia(&file, options)?;
    Ok(highlight_spans(source, &tokens.tokens, &trivia))
}

/// Classify already-lexed tokens and trivia.
pub fn highlight_spans(source: &str, tokens: &[Token], trivia: &[Trivia]) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if matches!(&token.kind, TokenKind::StringLiteral(_)) {
            for span in string_simple_spans(source, token.span.clone()) {
                spans.push(HighlightSpan {
                    span,
                    scope: string_scope(tokens, index),
                });
            }
            index += 1;
            continue;
        }
        spans.push(HighlightSpan {
            span: token.span.clone(),
            scope: scope_for_token(source, tokens, index, token),
        });
        index += 1;
    }
    for item in trivia {
        spans.push(HighlightSpan {
            span: item.span.clone(),
            scope: scope_for_trivia(item),
        });
    }

    spans.sort_by_key(|span| (span.span.start, span.span.end));
    spans
}

fn scope_for_trivia(trivia: &Trivia) -> &'static str {
    match trivia.kind {
        TriviaKind::Directive => "comment.directive",
        TriviaKind::Comment | TriviaKind::EndComment => "comment.block",
    }
}

fn scope_for_token(source: &str, tokens: &[Token], index: usize, token: &Token) -> &'static str {
    match &token.kind {
        TokenKind::Keyword(keyword) => keyword_scope(tokens, index, *keyword),
        TokenKind::Identifier(_) => identifier_scope(tokens, index),
        TokenKind::StringLiteral(_) => string_scope(tokens, index),
        TokenKind::CharacterLiteral(_) => "constant.character",
        TokenKind::NumberLiteral { lexeme, .. } => number_scope(lexeme),
        TokenKind::Assign | TokenKind::AssignAlt => "keyword.operator.assignment",
        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::SlashSlash
        | TokenKind::StarStar
        | TokenKind::Ampersand
        | TokenKind::AmpersandAmpersand => "keyword.operator.arithmetic",
        TokenKind::Lt
        | TokenKind::Le
        | TokenKind::Eq
        | TokenKind::Ge
        | TokenKind::Gt
        | TokenKind::Ne
        | TokenKind::RefEq
        | TokenKind::RefNe => {
            if word_operator_spelling(source, token) {
                "keyword.operator"
            } else {
                "keyword.operator.comparison"
            }
        }
        TokenKind::LeftParen | TokenKind::RightParen => "punctuation.section.parens",
        TokenKind::LeftBracket | TokenKind::RightBracket => "punctuation.section.brackets",
        TokenKind::Dot => "punctuation.accessor",
        TokenKind::Comma | TokenKind::Colon => "punctuation.separator",
        TokenKind::Semicolon => "punctuation.terminator.statement",
        TokenKind::CharacterQuote => "punctuation.definition.character",
    }
}

fn number_scope(lexeme: &str) -> &'static str {
    if is_radix_lexeme(lexeme) {
        "constant.numeric.radix"
    } else {
        "constant.numeric.decimal"
    }
}

fn word_operator_spelling(source: &str, token: &Token) -> bool {
    source
        .get(token.span.clone())
        .is_some_and(|spelling| spelling.chars().all(|ch| ch.is_ascii_alphabetic()))
}

/// Names from `editors/vscode/generate-grammar.ts`.
fn keyword_scope(tokens: &[Token], index: usize, keyword: Keyword) -> &'static str {
    if (keyword == Keyword::Then && prev_is_keyword(tokens, index, Keyword::And))
        || (keyword == Keyword::Else && prev_is_keyword(tokens, index, Keyword::Or))
    {
        return "keyword.operator";
    }
    if (keyword == Keyword::To && prev_is_keyword(tokens, index, Keyword::Go))
        || (keyword == Keyword::Go && next_is_keyword(tokens, index, Keyword::To))
    {
        return "keyword.control.goto";
    }
    if keyword == Keyword::Ref
        && matches!(
            tokens.get(index + 1).map(|t| &t.kind),
            Some(TokenKind::LeftParen)
        )
    {
        return "storage.modifier";
    }

    match keyword {
        Keyword::Begin => "keyword.control.begin",
        Keyword::End => "keyword.control.end",
        Keyword::If => "keyword.control.if",
        Keyword::Then => "keyword.control.then",
        Keyword::Else => "keyword.control.else",
        Keyword::While => "keyword.control.while",
        Keyword::Do => "keyword.control.do",
        Keyword::For => "keyword.control.for",
        Keyword::Step => "keyword.control.step",
        Keyword::Until => "keyword.control.until",
        Keyword::To => "keyword.control.to",
        Keyword::Goto => "keyword.control.goto",
        Keyword::Go => "keyword.control.go",
        Keyword::Inspect => "keyword.control.inspect",
        Keyword::When => "keyword.control.when",
        Keyword::Otherwise => "keyword.control.otherwise",
        Keyword::Switch => "keyword.control.switch",
        Keyword::Activate => "keyword.control.activate",
        Keyword::Reactivate => "keyword.control.reactivate",
        Keyword::At => "keyword.control.at",
        Keyword::Delay => "keyword.control.delay",
        Keyword::Before => "keyword.control.before",
        Keyword::After => "keyword.control.after",
        Keyword::Prior => "keyword.control.prior",
        Keyword::New => "keyword.control.new",
        Keyword::True | Keyword::False => "constant.language.bool",
        Keyword::None | Keyword::Notext => "constant.language.null",
        Keyword::Integer
        | Keyword::Boolean
        | Keyword::Character
        | Keyword::Text
        | Keyword::Real
        | Keyword::Short
        | Keyword::Long
        | Keyword::Array
        | Keyword::Procedure => "storage.type",
        Keyword::Ref => "keyword.other.ref",
        Keyword::This => "keyword.other.this",
        Keyword::Class => "keyword.other.class",
        Keyword::Inner => "keyword.other.inner",
        Keyword::Name => "keyword.other.name",
        Keyword::Value => "keyword.other.value",
        Keyword::External => "keyword.other.external",
        Keyword::Virtual => "keyword.other.virtual",
        Keyword::Hidden => "keyword.other.hidden",
        Keyword::Protected => "keyword.other.protected",
        Keyword::Is => {
            if has_keyword_since_statement_start(tokens, index, Keyword::External) {
                "keyword.control.is"
            } else {
                "keyword.other.is"
            }
        }
        Keyword::Label => "keyword.other.label",
        Keyword::And
        | Keyword::Or
        | Keyword::Eqv
        | Keyword::Imp
        | Keyword::Not
        | Keyword::In
        | Keyword::Qua
        | Keyword::Lt
        | Keyword::Le
        | Keyword::Eq
        | Keyword::Ge
        | Keyword::Gt
        | Keyword::Ne => "keyword.operator",
        Keyword::Comment => "comment.block",
    }
}

fn identifier_scope(tokens: &[Token], index: usize) -> &'static str {
    if prev_is_keyword(tokens, index, Keyword::End) {
        return "comment.block";
    }
    if is_ref_qualification(tokens, index) {
        return "entity.name.class";
    }
    if is_formal_parameter(tokens, index) {
        return "variable.parameter";
    }
    if next_is_keyword(tokens, index, Keyword::Begin)
        || next_is_keyword(tokens, index, Keyword::Class)
        || prev_is_keyword(tokens, index, Keyword::Class)
        || prev_is_keyword(tokens, index, Keyword::New)
        || prev_is_keyword(tokens, index, Keyword::When)
        || ident_in_list_after(tokens, index, Keyword::Class)
    {
        return "entity.name.class";
    }
    if prev_is_keyword(tokens, index, Keyword::External)
        && next_is_keyword(tokens, index, Keyword::Procedure)
    {
        return "entity.name.other";
    }
    if next_is_keyword(tokens, index, Keyword::Procedure)
        || prev_is_keyword(tokens, index, Keyword::Procedure)
        || ident_in_list_after(tokens, index, Keyword::Procedure)
    {
        return "entity.name.function";
    }
    if prev_is_keyword(tokens, index, Keyword::Goto)
        || (prev_is_keyword(tokens, index, Keyword::To)
            && index >= 2
            && prev_is_keyword(tokens, index - 1, Keyword::Go))
    {
        return "entity.name.label";
    }
    if matches!(
        tokens.get(index + 1).map(|t| &t.kind),
        Some(TokenKind::LeftParen)
    ) {
        return "entity.name.function";
    }
    if is_statement_label(tokens, index) {
        return "entity.name.label";
    }
    "variable"
}

fn is_statement_label(tokens: &[Token], index: usize) -> bool {
    if !matches!(
        tokens.get(index + 1).map(|t| &t.kind),
        Some(TokenKind::Colon)
    ) {
        return false;
    }
    let mut depth = 0i32;
    for token in tokens[..index].iter().rev() {
        match &token.kind {
            TokenKind::RightParen | TokenKind::RightBracket => depth += 1,
            TokenKind::LeftParen | TokenKind::LeftBracket => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            TokenKind::Semicolon | TokenKind::Keyword(Keyword::Begin | Keyword::End) => {
                return true;
            }
            _ => {}
        }
    }
    true
}

fn string_scope(tokens: &[Token], index: usize) -> &'static str {
    if matches!(
        tokens.get(index.wrapping_sub(1)).map(|t| &t.kind),
        Some(TokenKind::Eq)
    ) && has_keyword_since_statement_start(tokens, index, Keyword::External)
    {
        "string"
    } else {
        "string.quoted.double"
    }
}

fn string_simple_spans(source: &str, span: Range<usize>) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let Some(slice) = source.get(span.clone()) else {
        return vec![span];
    };
    let bytes = slice.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = span.start + i;
        i += 1;
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                i += 1;
                spans.push(start..span.start + i);
                break;
            }
            i += 1;
        }
    }
    if spans.is_empty() { vec![span] } else { spans }
}

fn has_keyword_since_statement_start(tokens: &[Token], index: usize, keyword: Keyword) -> bool {
    for token in tokens[..index].iter().rev() {
        match &token.kind {
            TokenKind::Keyword(k) if *k == keyword => return true,
            TokenKind::Semicolon | TokenKind::Keyword(Keyword::Begin | Keyword::End) => {
                return false;
            }
            _ => {}
        }
    }
    false
}

fn ident_in_list_after(tokens: &[Token], index: usize, keyword: Keyword) -> bool {
    let mut i = index;
    while i > 0 {
        i -= 1;
        match &tokens[i].kind {
            TokenKind::Comma | TokenKind::Identifier(_) => {}
            TokenKind::Keyword(k) if *k == keyword => return true,
            _ => return false,
        }
    }
    false
}

fn is_formal_parameter(tokens: &[Token], index: usize) -> bool {
    let mut depth = 0usize;
    let mut i = index;
    while i > 0 {
        i -= 1;
        match &tokens[i].kind {
            TokenKind::RightParen => depth += 1,
            TokenKind::LeftParen => {
                if depth == 0 {
                    return matches!(
                        tokens.get(i.wrapping_sub(1)).map(|t| &t.kind),
                        Some(TokenKind::Identifier(_))
                    ) && matches!(
                        tokens.get(i.wrapping_sub(2)).map(|t| &t.kind),
                        Some(TokenKind::Keyword(Keyword::Procedure | Keyword::Class))
                    );
                }
                depth -= 1;
            }
            TokenKind::Semicolon | TokenKind::Keyword(Keyword::Begin | Keyword::End) => {
                return false;
            }
            _ => {}
        }
    }
    false
}

fn is_ref_qualification(tokens: &[Token], index: usize) -> bool {
    matches!(
        tokens.get(index.wrapping_sub(1)).map(|t| &t.kind),
        Some(TokenKind::LeftParen)
    ) && matches!(
        tokens.get(index.wrapping_sub(2)).map(|t| &t.kind),
        Some(TokenKind::Keyword(Keyword::Ref))
    )
}

fn next_is_keyword(tokens: &[Token], index: usize, keyword: Keyword) -> bool {
    matches!(
        tokens.get(index + 1).map(|t| &t.kind),
        Some(TokenKind::Keyword(k)) if *k == keyword
    )
}

fn prev_is_keyword(tokens: &[Token], index: usize, keyword: Keyword) -> bool {
    matches!(
        tokens.get(index.wrapping_sub(1)).map(|t| &t.kind),
        Some(TokenKind::Keyword(k)) if *k == keyword
    )
}

fn is_radix_lexeme(lexeme: &str) -> bool {
    lexeme.bytes().any(|byte| byte == b'R' || byte == b'r')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_grammar_test_scope_names() {
        let spans = highlight_source("%d\nbegin integer x; !c;\n-- line\nend;\n").expect("lex");
        let scopes: Vec<_> = spans.iter().map(|span| span.scope).collect();
        for expected in [
            "comment.directive",
            "keyword.control.begin",
            "storage.type",
            "variable",
            "punctuation.terminator.statement",
            "comment.block",
            "keyword.control.end",
        ] {
            assert!(
                scopes.contains(&expected),
                "missing {expected} in {scopes:?}"
            );
        }
    }
}
