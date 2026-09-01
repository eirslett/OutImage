//! Shared token matchers for Simula parsers.

use chumsky::DefaultExpected;
use chumsky::error::{LabelError, Rich};
use chumsky::input::InputRef;
use chumsky::prelude::*;
use chumsky::util::MaybeRef;

use crate::error::{CompileError, Span};
use crate::lex::{Keyword, Token, TokenKind};

pub type ParseExtra<'a> = extra::Err<Rich<'a, Token>>;

/// Drop-in replacement for the old `Simple::new`, building a [`Rich`] error.
///
/// Prefer this in `custom` parsers so [`combinator_errors_to_compile`] can read
/// `found()` (and, for filter/choice failures, `expected()`).
pub(in crate::parse) fn rich_err<'a>(
    found: Option<MaybeRef<'a, Token>>,
    span: <&'a [Token] as chumsky::prelude::Input<'a>>::Span,
) -> Rich<'a, Token> {
    <Rich<'a, Token> as LabelError<'a, &'a [Token], DefaultExpected<'a, Token>>>::expected_found(
        [],
        found,
        span,
    )
}

/// The source byte span covering a (possibly empty) slice of consumed tokens.
///
/// Chumsky's `Input` impl for `&[Token]` derives spans from *token indices*,
/// not source byte offsets, so anywhere we want a real source span we
/// recover it from the first/last token's own `span` field instead.
pub fn span_of_slice(tokens: &[Token]) -> Span {
    match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => first.span.start..last.span.end,
        _ => 0..0,
    }
}

/// Convenience for `map_with` closures: recover the real source span of
/// whatever token slice the wrapped parser consumed.
pub fn span_with<'a, I, E>(extra: &mut chumsky::input::MapExtra<'a, '_, I, E>) -> Span
where
    I: chumsky::input::Input<'a, Token = Token>
        + chumsky::input::SliceInput<'a, Slice = &'a [Token]>,
    E: chumsky::extra::ParserExtra<'a, I>,
{
    span_of_slice(extra.slice())
}

pub fn token_kinds_match(actual: &TokenKind, expected: &TokenKind) -> bool {
    match (actual, expected) {
        (TokenKind::Keyword(_), TokenKind::Keyword(_))
        | (TokenKind::Identifier(_), TokenKind::Identifier(_))
        | (TokenKind::StringLiteral(_), TokenKind::StringLiteral(_))
        | (TokenKind::CharacterLiteral(_), TokenKind::CharacterLiteral(_))
        | (TokenKind::NumberLiteral { .. }, TokenKind::NumberLiteral { .. }) => true,
        (a, b) => a == b,
    }
}

pub fn is_keyword_token(kind: &TokenKind, keyword: Keyword) -> bool {
    matches!(kind, TokenKind::Keyword(k) if *k == keyword)
}

pub fn any_token<'a>() -> impl Parser<'a, &'a [Token], Token, ParseExtra<'a>> + Clone {
    any::<&'a [Token], ParseExtra<'a>>()
}

pub fn kind<'a>(
    expected: TokenKind,
) -> impl Parser<'a, &'a [Token], Token, ParseExtra<'a>> + Clone {
    any_token()
        .filter(move |token: &Token| token_kinds_match(&token.kind, &expected))
        .labelled("a token")
}

pub fn keyword<'a>(
    keyword: Keyword,
) -> impl Parser<'a, &'a [Token], Keyword, ParseExtra<'a>> + Clone {
    any_token()
        .filter(move |token: &Token| is_keyword_token(&token.kind, keyword))
        .map(move |_| keyword)
        .labelled("a keyword")
}

pub fn identifier<'a>() -> impl Parser<'a, &'a [Token], String, ParseExtra<'a>> + Clone {
    select! {
        Token { kind: TokenKind::Identifier(name), .. } => name.clone(),
    }
}

pub fn string_literal<'a>() -> impl Parser<'a, &'a [Token], String, ParseExtra<'a>> + Clone {
    select! {
        Token { kind: TokenKind::StringLiteral(value), .. } => value.clone(),
    }
}

/// Identifiers that may be spelled as keywords (`value`, `name`, `inner`, …).
pub fn name_identifier<'a>() -> impl Parser<'a, &'a [Token], String, ParseExtra<'a>> + Clone {
    select! {
        Token { kind: TokenKind::Identifier(name), .. } => name.clone(),
        Token { kind: TokenKind::Keyword(keyword @ (Keyword::Value | Keyword::Name | Keyword::Inner)), .. } => {
            keyword.as_str().to_string()
        },
    }
}

/// Match a keyword token only when it is not immediately followed by another keyword.
pub fn keyword_not_followed_by<'a>(
    keyword: Keyword,
    not_next: Keyword,
) -> impl Parser<'a, &'a [Token], Keyword, ParseExtra<'a>> + Clone {
    custom::<_, &'a [Token], Keyword, ParseExtra<'a>>(
        move |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let before = inp.cursor();
            let checkpoint = inp.save();
            let Some(token) = inp.next() else {
                return Err(rich_err(None, inp.span_since(&before)));
            };
            if !is_keyword_token(&token.kind, keyword) {
                inp.rewind(checkpoint);
                return Err(rich_err(
                    Some(MaybeRef::Val(token)),
                    inp.span_since(&before),
                ));
            }
            if inp
                .peek()
                .is_some_and(|next| is_keyword_token(&next.kind, not_next))
            {
                inp.rewind(checkpoint);
                return Err(rich_err(
                    Some(MaybeRef::Val(token)),
                    inp.span_since(&before),
                ));
            }
            Ok(keyword)
        },
    )
}

pub(in crate::parse) fn combinator_errors_to_compile(
    errors: Vec<Rich<'_, Token>>,
    tokens: &[Token],
    _offset: usize,
) -> CompileError {
    if let Some(error) = super::take_stashed_parse_error() {
        return error;
    }

    if let Some(err) = errors.first() {
        let slice_index = err.span().start;
        let span = err
            .found()
            .map(|token| token.span.clone())
            .or_else(|| tokens.get(slice_index).map(|token| token.span.clone()));

        if let Some(token) = err.found()
            && matches!(token.kind, TokenKind::AssignAlt)
        {
            return crate::diagnostics::wrong_assign_operator(
                crate::ast::AssignOperator::AssignAlt,
                token.span.clone(),
            );
        }

        if let Some(error) = incomplete_type_prefix_error(tokens, slice_index, err.found()) {
            return error;
        }

        let expected = expected_list(err);
        let contexts = context_names(err);
        let expected_note = crate::diagnostics::expected_list_english(&expected);

        if err.found().is_none() {
            if expected.iter().any(|item| item.contains("`end`")) {
                let begin_span = tokens.iter().rev().find_map(|token| {
                    if matches!(token.kind, TokenKind::Keyword(Keyword::Begin)) {
                        Some(token.span.clone())
                    } else {
                        None
                    }
                });
                return crate::diagnostics::missing_end(span, begin_span);
            }
            let mut error = crate::diagnostics::unexpected_eof(expected_note);
            for ctx in contexts {
                error = error.with_note(format!("while parsing {ctx}"));
            }
            return error;
        }

        let found = err
            .found()
            .map(|token| crate::diagnostics::token_english(&token.kind))
            .unwrap_or_else(|| "end of file".to_string());
        let mut error = crate::diagnostics::unexpected_token(
            &found,
            expected_note,
            span.unwrap_or(0..0),
            &contexts,
        );
        if errors.len() > 1 {
            error = error.with_note(format!(
                "{} additional parse issue(s) were suppressed; fixing this may reveal more",
                errors.len() - 1
            ));
        }
        return error;
    }

    crate::diagnostics::unexpected_eof(None)
}

fn expected_list(err: &Rich<'_, Token>) -> Vec<String> {
    err.expected().filter_map(rich_pattern_english).collect()
}

fn rich_pattern_english(pattern: &chumsky::error::RichPattern<'_, Token>) -> Option<String> {
    use chumsky::error::RichPattern;
    match pattern {
        RichPattern::Token(token) => Some(crate::diagnostics::token_english(&token.kind)),
        RichPattern::Label(label) => {
            let label = label.to_string();
            if label.is_empty() { None } else { Some(label) }
        }
        RichPattern::Identifier(_) => Some("an identifier".into()),
        RichPattern::Any => None,
        RichPattern::SomethingElse => None,
        RichPattern::EndOfInput => Some("end of file".into()),
        _ => None,
    }
}

fn context_names(err: &Rich<'_, Token>) -> Vec<String> {
    err.contexts()
        .filter_map(|(pattern, _)| rich_pattern_english(pattern))
        .collect()
}

fn incomplete_type_prefix_error(
    tokens: &[Token],
    index: usize,
    found: Option<&Token>,
) -> Option<CompileError> {
    let at = found
        .map(|token| {
            tokens
                .iter()
                .position(|t| t.span == token.span)
                .unwrap_or(index)
        })
        .unwrap_or(index);

    let prefix_at = match tokens.get(at).map(|t| &t.kind) {
        Some(TokenKind::Keyword(Keyword::Short | Keyword::Long | Keyword::Ref)) => at,
        _ if at > 0 => match tokens.get(at - 1).map(|t| &t.kind) {
            Some(TokenKind::Keyword(Keyword::Short | Keyword::Long | Keyword::Ref)) => at - 1,
            _ => at,
        },
        _ => at,
    };

    let (prefix, needed) = match tokens.get(prefix_at).map(|t| &t.kind) {
        Some(TokenKind::Keyword(Keyword::Short))
            if !matches!(
                tokens.get(prefix_at + 1).map(|t| &t.kind),
                Some(TokenKind::Keyword(Keyword::Integer))
            ) =>
        {
            (Keyword::Short, "`integer`")
        }
        Some(TokenKind::Keyword(Keyword::Long))
            if !matches!(
                tokens.get(prefix_at + 1).map(|t| &t.kind),
                Some(TokenKind::Keyword(Keyword::Real))
            ) =>
        {
            (Keyword::Long, "`real`")
        }
        Some(TokenKind::Keyword(Keyword::Ref))
            if !matches!(
                tokens.get(prefix_at + 1).map(|t| &t.kind),
                Some(TokenKind::LeftParen)
            ) =>
        {
            (Keyword::Ref, "`(Class)`")
        }
        _ => return None,
    };
    let span = tokens
        .get(prefix_at)
        .map(|t| t.span.clone())
        .unwrap_or(0..0);
    Some(crate::diagnostics::incomplete_type_prefix(
        prefix, needed, span,
    ))
}

pub fn semicolon<'a>() -> impl Parser<'a, &'a [Token], (), ParseExtra<'a>> + Clone {
    kind(TokenKind::Semicolon).map(|_| ())
}

pub fn optional_semicolon<'a>() -> impl Parser<'a, &'a [Token], (), ParseExtra<'a>> + Clone {
    semicolon().or_not().map(|_| ())
}

/// Delimit a subscript list with `( )` or, when lexed, `[ ]`.
pub fn subscript_delimited<'a, O>(
    inner: impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a {
    choice((
        kind(TokenKind::LeftParen)
            .ignore_then(inner.clone())
            .then_ignore(kind(TokenKind::RightParen)),
        kind(TokenKind::LeftBracket)
            .ignore_then(inner)
            .then_ignore(kind(TokenKind::RightBracket)),
    ))
}

pub fn assign_operator<'a>()
-> impl Parser<'a, &'a [Token], crate::ast::AssignOperator, ParseExtra<'a>> + Clone {
    choice((
        kind(TokenKind::Assign).map(|_| crate::ast::AssignOperator::Assign),
        kind(TokenKind::AssignAlt).map(|_| crate::ast::AssignOperator::AssignAlt),
    ))
}

pub fn identifier_list<'a>() -> impl Parser<'a, &'a [Token], Vec<String>, ParseExtra<'a>> + Clone {
    name_identifier()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .collect()
}

/// Wrap a parser so it consumes a matching prefix of the remaining input.
///
/// Runs `parser` once via [`Parser::go_emit`] so it can stop at the first token
/// that is not part of the match, without re-parsing growing prefixes.
pub fn prefix_parser<'a, O: 'a>(
    parser: Boxed<'a, 'a, &'a [Token], O, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], O, ParseExtra<'a>> {
    custom(
        move |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let start = inp.cursor();
            let checkpoint = inp.save();
            match parser.clone().go_emit(inp) {
                Ok(value) => Ok(value),
                Err(()) => {
                    inp.rewind(checkpoint);
                    Err(rich_err(None, inp.span_since(&start)))
                }
            }
        },
    )
    .boxed()
}

/// Run `parser` once on a token slice and return how many tokens it consumed.
///
/// [`Parser::parse`] always requires end-of-input, so after a successful prefix
/// match we drain any remainder. Use [`any_ref`] (not [`any`]) for that drain:
/// `&[Token]`'s [`ValueInput`] implementation clones each `Token` (including
/// owned strings), and class/program member loops call this once per member on
/// the full remainder — cloning with [`any`] is O(n²) allocation traffic.
pub(in crate::parse) fn emit_prefix<'a, O>(
    tokens: &'a [Token],
    offset: usize,
    parser: impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
) -> Result<(O, usize), CompileError> {
    let (output, errors) = custom(
        move |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let before = inp.cursor();
            let value = parser
                .clone()
                .go_emit(inp)
                .map_err(|_| rich_err(None, inp.span_since(&before)))?;
            let span = inp.span_since(&before);
            Ok((value, span.end - span.start))
        },
    )
    .then_ignore(any_ref().repeated())
    .parse(tokens)
    .into_output_errors();

    match output {
        Some(result) if errors.is_empty() => Ok(result),
        _ => Err(combinator_errors_to_compile(errors, tokens, offset)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::Token;
    use chumsky::Parser;

    use crate::parse::test_support::{assert_combinator_err, parse_prefix_slice, tokens};

    #[test]
    fn keyword_parser_matches_begin() {
        let token_list = vec![Token::kind_only(TokenKind::Keyword(Keyword::Begin))];
        assert_eq!(
            keyword(Keyword::Begin).parse(&token_list).into_result(),
            Ok(Keyword::Begin)
        );
    }

    #[test]
    fn emit_prefix_reports_consumed_token_count() {
        let stream = tokens("begin end;");
        let (_, consumed) = parse_prefix_slice(stream.as_slice(), keyword(Keyword::Begin));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn emit_prefix_stops_before_unexpected_token() {
        let stream = tokens("begin ;");
        let (_, consumed) = parse_prefix_slice(stream.as_slice(), keyword(Keyword::Begin));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn combinator_reports_error_on_invalid_input() {
        let stream = tokens(";");
        assert_combinator_err(stream.as_slice(), keyword(Keyword::Begin));
    }
}
