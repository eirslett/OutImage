//! Simple variable declaration parsers (Chapter 2).

use chumsky::prelude::*;

use super::expr::prefix as expr_parser;
use super::tokens::{ParseExtra, assign_operator, kind, name_identifier, rich_err, span_with};
use super::type_::parser as type_parser;
use crate::ast::Expr;
use crate::lex::{Token, TokenKind};
use crate::types::{Declaration, DeclarationItem};

fn expr<'a>() -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    expr_parser()
}

pub fn parser<'a>() -> Boxed<'a, 'a, &'a [Token], Declaration, ParseExtra<'a>> {
    type_parser()
        .then(declaration_items())
        .then_ignore(kind(TokenKind::Semicolon))
        .map_with(|(ty, items), extra| Declaration {
            ty,
            items,
            span: span_with(extra),
        })
        .boxed()
}

fn declaration_items<'a>()
-> impl Parser<'a, &'a [Token], Vec<DeclarationItem>, ParseExtra<'a>> + Clone {
    custom(
        |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let mut items = Vec::new();
            loop {
                let before = inp.cursor();
                let Ok(item) = declaration_item().go_emit(inp) else {
                    if items.is_empty() {
                        return Err(rich_err(None, inp.span_since(&before)));
                    }
                    break;
                };
                items.push(item);
                match inp.peek().map(|token| token.kind.clone()) {
                    Some(TokenKind::Comma) => {
                        inp.skip();
                    }
                    _ => break,
                }
            }
            Ok(items)
        },
    )
}

fn declaration_item<'a>() -> impl Parser<'a, &'a [Token], DeclarationItem, ParseExtra<'a>> + Clone {
    name_identifier()
        .then(initializer().or_not())
        .map(|(name, init)| {
            let (initializer, is_constant) = init.unwrap_or((None, false));
            DeclarationItem {
                name,
                initializer,
                is_constant,
            }
        })
}

/// `(initializer, is_constant)` after the name.
fn initializer<'a>() -> impl Parser<'a, &'a [Token], (Option<Expr>, bool), ParseExtra<'a>> + Clone {
    choice((
        kind(TokenKind::Eq)
            .ignore_then(expr().clone())
            .map(|expr| (Some(expr), true)),
        assign_operator()
            .ignore_then(expr())
            .map(|expr| (Some(expr), false)),
    ))
}
