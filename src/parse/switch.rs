//! Switch declaration parsers (Simula Standard §5.3).

use chumsky::prelude::*;

use super::tokens::{ParseExtra, assign_operator, identifier, keyword, kind, semicolon, span_with};
use crate::ast::{DesignationalExpr, SwitchDeclaration};
use crate::lex::{Keyword, Token, TokenKind};

use super::expr::designational_prefix;

pub fn parser<'a>() -> impl Parser<'a, &'a [Token], SwitchDeclaration, ParseExtra<'a>> + Clone {
    keyword(Keyword::Switch)
        .ignore_then(identifier())
        .then_ignore(assign_operator())
        .then(designational_elements())
        .then_ignore(semicolon())
        .map_with(|(name, elements), extra| SwitchDeclaration {
            name,
            elements,
            span: span_with(extra),
        })
}

fn designational_elements<'a>()
-> impl Parser<'a, &'a [Token], Vec<DesignationalExpr>, ParseExtra<'a>> + Clone {
    designational_prefix()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .collect()
}
