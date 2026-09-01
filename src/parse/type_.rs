//! Type parsers (Simula Standard Chapter 2).

use chumsky::input::InputRef;
use chumsky::prelude::*;
use chumsky::util::MaybeRef;

use super::tokens::{ParseExtra, identifier, keyword, kind, rich_err};
use crate::lex::{Keyword, Token, TokenKind};
use crate::types::Type;

pub fn parser<'a>() -> impl Parser<'a, &'a [Token], Type, ParseExtra<'a>> + Clone {
    choice((
        short_integer_type(),
        keyword(Keyword::Long)
            .ignore_then(keyword(Keyword::Real))
            .map(|_| Type::Real { long: true }),
        keyword(Keyword::Integer).map(|_| Type::Integer { short: false }),
        keyword(Keyword::Real).map(|_| Type::Real { long: false }),
        keyword(Keyword::Boolean).map(|_| Type::Boolean),
        keyword(Keyword::Character).map(|_| Type::Character),
        keyword(Keyword::Text).map(|_| Type::Text),
        keyword(Keyword::Ref)
            .ignore_then(kind(TokenKind::LeftParen))
            .ignore_then(identifier())
            .then_ignore(kind(TokenKind::RightParen))
            .map(Type::ObjectRef),
    ))
}

fn short_integer_type<'a>() -> impl Parser<'a, &'a [Token], Type, ParseExtra<'a>> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        let before = inp.cursor();
        let Some(token) = inp.next() else {
            return Err(rich_err(None, inp.span_since(&before)));
        };
        if !matches!(token.kind, TokenKind::Keyword(Keyword::Short)) {
            return Err(rich_err(
                Some(MaybeRef::Val(token)),
                inp.span_since(&before),
            ));
        }

        let integer_start = inp.cursor();
        let Some(token) = inp.next() else {
            return Err(rich_err(None, inp.span_since(&integer_start)));
        };
        if matches!(token.kind, TokenKind::Keyword(Keyword::Integer)) {
            Ok(Type::Integer { short: true })
        } else {
            Err(rich_err(
                Some(MaybeRef::Val(token)),
                inp.span_since(&integer_start),
            ))
        }
    })
}
