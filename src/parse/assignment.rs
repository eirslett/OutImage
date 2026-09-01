//! Assignment right/left part parsers (§4.1).
//!
//! Value/reference RHS parsing uses a cursor bridge for mixed-operator rejection;
//! pure chumsky `choice` would backtrack to the expression fallback.

use super::Cursor;
use super::bridge::cursor_bridge;
use super::tokens::ParseExtra;
use crate::ast::{AssignOperator, Assignment, AssignmentRhs, Variable};
use crate::error::CompileError;
use crate::lex::{Token, TokenKind};

pub fn value_rhs_parser<'a>() -> Boxed<'a, 'a, &'a [Token], AssignmentRhs, ParseExtra<'a>> {
    cursor_bridge(|parser| parser.parse_value_right_part())
}

pub fn reference_rhs_parser<'a>() -> Boxed<'a, 'a, &'a [Token], AssignmentRhs, ParseExtra<'a>> {
    cursor_bridge(|parser| parser.parse_reference_right_part())
}

impl<'a> Cursor<'a> {
    pub(in crate::parse) fn parse_value_right_part(
        &mut self,
    ) -> Result<AssignmentRhs, CompileError> {
        let checkpoint = self.index;
        if matches!(self.peek_kind(), Some(TokenKind::Identifier(_))) {
            if let Ok(lhs) = self.parse_value_left_part() {
                match self.peek_assign_operator() {
                    Some(AssignOperator::Assign) => {
                        self.index = checkpoint;
                    }
                    Some(AssignOperator::AssignAlt) => {
                        let _ = lhs;
                        return Err(self
                            .expect_assign_operator(AssignOperator::Assign)
                            .unwrap_err());
                    }
                    None => {
                        self.index = checkpoint;
                    }
                }
            } else {
                self.index = checkpoint;
            }
        }

        if self.check_nested_assignment(AssignOperator::Assign) {
            let lhs = self.parse_value_left_part()?;
            self.expect_assign_operator(AssignOperator::Assign)?;
            let rhs = self.parse_value_right_part()?;
            return Ok(AssignmentRhs::Chain(Box::new(Assignment {
                lhs,
                operator: AssignOperator::Assign,
                rhs,
            })));
        }

        Ok(AssignmentRhs::Expr(self.parse_expr()?))
    }

    pub(in crate::parse) fn parse_reference_right_part(
        &mut self,
    ) -> Result<AssignmentRhs, CompileError> {
        if self.check_nested_assignment(AssignOperator::AssignAlt) {
            let lhs = self.parse_reference_left_part()?;
            self.expect_assign_operator(AssignOperator::AssignAlt)?;
            let rhs = self.parse_reference_right_part()?;
            return Ok(AssignmentRhs::Chain(Box::new(Assignment {
                lhs,
                operator: AssignOperator::AssignAlt,
                rhs,
            })));
        }

        Ok(AssignmentRhs::Expr(self.parse_expr()?))
    }

    fn parse_value_left_part(&mut self) -> Result<Variable, CompileError> {
        if let Some(variable) = self.try_parse_simple_text_left_part()? {
            return Ok(variable);
        }

        self.parse_identifier_variable()
    }

    fn parse_reference_left_part(&mut self) -> Result<Variable, CompileError> {
        self.parse_identifier_variable()
    }

    fn try_parse_simple_text_left_part(&mut self) -> Result<Option<Variable>, CompileError> {
        let checkpoint = self.index;
        if !matches!(self.peek_kind(), Some(TokenKind::Identifier(_))) {
            return Ok(None);
        }

        match self.parse_identifier_variable() {
            Ok(variable) => Ok(Some(variable)),
            Err(error) => {
                self.index = checkpoint;
                Err(error)
            }
        }
    }

    fn check_nested_assignment(&self, operator: AssignOperator) -> bool {
        let checkpoint = self.index;
        let mut parser = Cursor {
            tokens: self.tokens,
            index: checkpoint,
        };

        let Ok(lhs) = parser.parse_value_left_part() else {
            return false;
        };

        let _ = lhs;
        parser.peek_assign_operator() == Some(operator)
    }
}

use chumsky::prelude::*;
