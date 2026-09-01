//! Token cursor for Simula-specific disambiguation that pure combinators cannot express.

use chumsky::Parser;

use super::block::{block_extent, parser as block_parser};
use super::expr::parser as expr_parser;
use super::stmt::{labeled_parser, statement_choice};
use super::tokens::{
    combinator_errors_to_compile, emit_prefix, is_keyword_token, token_kinds_match,
};
use super::type_::parser as type_parser;
use super::variable::parser as variable_parser;
use crate::ast::{AssignOperator, Block, Expr, Statement, StatementKind, Variable};
use crate::error::CompileError;
use crate::lex::{Keyword, Token, TokenKind, TokenStream};
use crate::types::Type;

pub(in crate::parse) struct Cursor<'a> {
    pub(in crate::parse) tokens: &'a [Token],
    pub(in crate::parse) index: usize,
}

impl<'a> Cursor<'a> {
    pub(in crate::parse) fn new(stream: &'a TokenStream) -> Self {
        Self {
            tokens: stream.as_slice(),
            index: 0,
        }
    }

    pub(in crate::parse) fn parse_labeled_statement(&mut self) -> Result<Statement, CompileError> {
        let block = block_parser();
        let stmt = labeled_parser(statement_choice(block));
        let slice = &self.tokens[self.index..];
        let (statement, consumed) = emit_prefix(slice, self.index, stmt)?;

        if matches!(statement.kind, StatementKind::Assignment(_))
            && matches!(
                self.tokens
                    .get(self.index + consumed)
                    .map(|token| &token.kind),
                Some(TokenKind::AssignAlt)
            )
        {
            return Err(self
                .expect_assign_operator(AssignOperator::Assign)
                .unwrap_err());
        }

        self.index += consumed;
        Ok(statement)
    }

    pub(in crate::parse) fn parse_block(&mut self) -> Result<Block, CompileError> {
        let begin = self.index;
        let end = block_extent(self.tokens, begin)?;
        let slice = &self.tokens[begin..end];
        let (block, errors) = block_parser().parse(slice).into_output_errors();
        match block {
            Some(block) if errors.is_empty() => {
                self.index = end;
                Ok(block)
            }
            _ => Err(combinator_errors_to_compile(errors, slice, begin)),
        }
    }

    pub(in crate::parse) fn parse_expr(&mut self) -> Result<Expr, CompileError> {
        let slice = &self.tokens[self.index..];
        let (expr, consumed) = emit_prefix(slice, self.index, expr_parser())?;
        self.index += consumed;
        Ok(expr)
    }

    pub(in crate::parse) fn parse_type(&mut self) -> Result<Type, CompileError> {
        let slice = &self.tokens[self.index..];
        let (ty, consumed) = emit_prefix(slice, self.index, type_parser())?;
        self.index += consumed;
        Ok(ty)
    }

    pub(in crate::parse) fn parse_identifier_variable(&mut self) -> Result<Variable, CompileError> {
        let slice = &self.tokens[self.index..];
        let (variable, consumed) = emit_prefix(slice, self.index, variable_parser())?;
        self.index += consumed;
        Ok(variable)
    }

    pub(in crate::parse) fn expect_identifier(&mut self) -> Result<String, CompileError> {
        match self.peek() {
            Some(token) if matches!(token.kind, TokenKind::Identifier(_)) => {
                let TokenKind::Identifier(name) = self.tokens[self.index].kind.clone() else {
                    unreachable!("checked in guard");
                };
                self.index += 1;
                Ok(name)
            }
            Some(token)
                if matches!(
                    token.kind,
                    TokenKind::Keyword(Keyword::Value | Keyword::Name | Keyword::Inner)
                ) =>
            {
                let TokenKind::Keyword(keyword) = self.tokens[self.index].kind.clone() else {
                    unreachable!("checked in guard");
                };
                self.index += 1;
                Ok(keyword.as_str().to_string())
            }
            Some(token) => Err(crate::diagnostics::unexpected_token(
                &crate::diagnostics::token_english(&token.kind),
                Some("expected an identifier".into()),
                token.span.clone(),
                &[],
            )),
            None => Err(crate::diagnostics::unexpected_eof(Some(
                "expected an identifier".into(),
            ))),
        }
    }

    pub(in crate::parse) fn expect_keyword(
        &mut self,
        keyword: Keyword,
    ) -> Result<(), CompileError> {
        match self.peek() {
            Some(token) if is_keyword_token(&token.kind, keyword) => {
                self.index += 1;
                Ok(())
            }
            Some(token) => Err(crate::diagnostics::unexpected_token(
                &crate::diagnostics::token_english(&token.kind),
                Some(format!("expected `{}`", keyword.as_str())),
                token.span.clone(),
                &[],
            )),
            None => Err(crate::diagnostics::unexpected_eof(Some(format!(
                "expected `{}`",
                keyword.as_str()
            )))),
        }
    }

    pub(in crate::parse) fn match_kind(&mut self, expected: &TokenKind) -> bool {
        let matched = self
            .peek_kind()
            .is_some_and(|actual| token_kinds_match(actual, expected));

        if matched {
            self.index += 1;
        }

        matched
    }

    pub(in crate::parse) fn peek_assign_operator(&self) -> Option<AssignOperator> {
        match self.peek_kind() {
            Some(TokenKind::Assign) => Some(AssignOperator::Assign),
            Some(TokenKind::AssignAlt) => Some(AssignOperator::AssignAlt),
            _ => None,
        }
    }

    pub(in crate::parse) fn match_assign_operator(&mut self) -> Option<AssignOperator> {
        let operator = self.peek_assign_operator()?;
        self.index += 1;
        Some(operator)
    }

    pub(in crate::parse) fn expect_assign_operator(
        &mut self,
        expected: AssignOperator,
    ) -> Result<(), CompileError> {
        match self.match_assign_operator() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(crate::diagnostics::wrong_assign_operator(
                actual,
                self.peek_span().unwrap_or(0..0),
            )),
            None => Err(crate::diagnostics::unexpected_eof(Some(format!(
                "expected `{}`",
                expected.as_str()
            )))),
        }
    }

    pub(in crate::parse) fn check_keyword(&self, keyword: Keyword) -> bool {
        self.peek_kind()
            .is_some_and(|kind| is_keyword_token(kind, keyword))
    }

    pub(in crate::parse) fn consume_optional_semicolon(&mut self) {
        if matches!(self.peek(), Some(token) if token.kind == TokenKind::Semicolon) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    pub(in crate::parse) fn peek_kind(&self) -> Option<&TokenKind> {
        self.peek().map(|token| &token.kind)
    }

    pub(in crate::parse) fn peek_span(&self) -> Option<std::ops::Range<usize>> {
        self.peek().map(|token| token.span.clone())
    }

    pub(in crate::parse) fn is_at_end(&self) -> bool {
        self.index >= self.tokens.len()
    }
}

impl AssignOperator {
    fn as_str(self) -> &'static str {
        match self {
            Self::Assign => "':='",
            Self::AssignAlt => "':-'",
        }
    }
}
