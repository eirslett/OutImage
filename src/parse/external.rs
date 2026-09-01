//! External declaration parsing (Simula Standard §6.1–§6.5).

use super::bridge::{cursor_bridge, validated_parser};
use super::form::assemble_external_procedure_spec;
use super::form::external_procedure_spec_parts_parser;
use super::form::is_type_start;
use super::tokens::{ParseExtra, emit_prefix};
use crate::ast::{
    ExternalClassDeclaration, ExternalDeclaration, ExternalItem, ExternalProcedureDeclaration,
};
use crate::error::CompileError;
use crate::lex::{Keyword, Token, TokenKind};
use chumsky::prelude::*;

use super::Cursor;

pub fn is_external_start(tokens: &[Token]) -> bool {
    tokens
        .first()
        .is_some_and(|token| matches!(token.kind, TokenKind::Keyword(Keyword::External)))
}

pub fn declaration_parser<'a>() -> Boxed<'a, 'a, &'a [Token], ExternalDeclaration, ParseExtra<'a>> {
    cursor_bridge(|parser| parser.parse_external_declaration())
}

impl<'a> Cursor<'a> {
    pub(in crate::parse) fn check_external_start(&self) -> bool {
        is_external_start(&self.tokens[self.index..])
    }

    pub(in crate::parse) fn parse_external_declaration(
        &mut self,
    ) -> Result<ExternalDeclaration, CompileError> {
        let start = self
            .tokens
            .get(self.index)
            .map(|token| token.span.start)
            .unwrap_or(0);
        self.expect_keyword(Keyword::External)?;

        if self.check_keyword(Keyword::Class) {
            return Ok(ExternalDeclaration::Class(
                self.parse_external_class_tail()?,
            ));
        }

        Ok(ExternalDeclaration::Procedure(
            self.parse_external_procedure_tail(start)?,
        ))
    }

    fn parse_external_class_tail(&mut self) -> Result<ExternalClassDeclaration, CompileError> {
        self.expect_keyword(Keyword::Class)?;
        let items = self.parse_external_list()?;
        Ok(ExternalClassDeclaration { items })
    }

    fn parse_external_procedure_tail(
        &mut self,
        start: usize,
    ) -> Result<ExternalProcedureDeclaration, CompileError> {
        let mut kind = None;
        let mut result_type = None;

        if self.check_keyword(Keyword::Procedure) {
            // `external procedure ...`
        } else if matches!(self.peek_kind(), Some(TokenKind::Identifier(_))) {
            let kind_name = self.expect_identifier()?;
            kind = Some(kind_name);
        }

        if kind.is_some() && !self.check_keyword(Keyword::Procedure) {
            self.expect_keyword(Keyword::Procedure)?;
            let item = self.parse_external_item()?;
            self.expect_keyword(Keyword::Is)?;
            let specification = Some(self.parse_external_procedure_specification()?);
            return Ok(self.finish_external_procedure(
                start,
                kind,
                None,
                vec![item],
                specification,
            ));
        }

        if is_type_start(&self.tokens[self.index..]) && !self.check_keyword(Keyword::Procedure) {
            result_type = Some(self.parse_type()?);
        }

        self.expect_keyword(Keyword::Procedure)?;

        if self.check_keyword(Keyword::Is) {
            self.expect_keyword(Keyword::Is)?;
            let specification = Some(self.parse_external_procedure_specification()?);
            let name = specification
                .as_ref()
                .map(|proc| proc.name.clone())
                .unwrap_or_default();
            return Ok(self.finish_external_procedure(
                start,
                kind,
                result_type,
                vec![ExternalItem {
                    name,
                    identification: None,
                }],
                specification,
            ));
        }

        let items = self.parse_external_list()?;
        let specification = if items.len() == 1 && self.check_keyword(Keyword::Is) {
            self.expect_keyword(Keyword::Is)?;
            Some(self.parse_external_procedure_specification()?)
        } else {
            None
        };
        Ok(self.finish_external_procedure(start, kind, result_type, items, specification))
    }

    fn finish_external_procedure(
        &self,
        start: usize,
        kind: Option<String>,
        result_type: Option<crate::types::Type>,
        items: Vec<ExternalItem>,
        specification: Option<crate::ast::ProcedureDeclaration>,
    ) -> ExternalProcedureDeclaration {
        let end = self
            .tokens
            .get(self.index.saturating_sub(1))
            .map(|token| token.span.end)
            .unwrap_or(start);
        ExternalProcedureDeclaration {
            kind,
            result_type,
            items,
            specification,
            span: start..end,
        }
    }

    fn parse_external_procedure_specification(
        &mut self,
    ) -> Result<crate::ast::ProcedureDeclaration, CompileError> {
        let (parts, consumed) = emit_prefix(
            &self.tokens[self.index..],
            self.index,
            validated_parser(
                external_procedure_spec_parts_parser(),
                assemble_external_procedure_spec,
            ),
        )?;
        self.index += consumed;
        Ok(parts)
    }

    fn parse_external_list(&mut self) -> Result<Vec<ExternalItem>, CompileError> {
        let mut items = vec![self.parse_external_item()?];
        while self.match_kind(&TokenKind::Comma) {
            items.push(self.parse_external_item()?);
        }
        Ok(items)
    }

    fn parse_external_item(&mut self) -> Result<ExternalItem, CompileError> {
        let name = self.expect_identifier()?;
        let identification = if self.match_kind(&TokenKind::Eq) {
            Some(self.parse_string_literal()?)
        } else {
            None
        };
        Ok(ExternalItem {
            name,
            identification,
        })
    }

    fn parse_string_literal(&mut self) -> Result<String, CompileError> {
        match self.tokens.get(self.index) {
            Some(token) if matches!(token.kind, TokenKind::StringLiteral(_)) => {
                let TokenKind::StringLiteral(value) = self.tokens[self.index].kind.clone() else {
                    unreachable!("checked in guard");
                };
                self.index += 1;
                Ok(value)
            }
            Some(token) => Err(crate::diagnostics::unexpected_token(
                &crate::diagnostics::token_english(&token.kind),
                Some("expected a string literal".into()),
                token.span.clone(),
                &[],
            )),
            None => Err(crate::diagnostics::unexpected_eof(Some(
                "expected a string literal".into(),
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parse::test_support::parse_program;

    fn parse_source(source: &str) -> crate::ast::Program {
        parse_program(source)
    }

    #[test]
    fn parses_external_class_head() {
        let program = parse_source("external class b, c; begin end;");
        assert_eq!(program.external_head.len(), 1);
        let crate::ast::ExternalDeclaration::Class(class) = &program.external_head[0] else {
            panic!("expected external class");
        };
        assert_eq!(class.items[0].name, "b");
        assert_eq!(class.items[1].name, "c");
    }

    #[test]
    fn parses_external_procedure_list() {
        let program = parse_source("external procedure aproc, bproc; begin end;");
        let crate::ast::ExternalDeclaration::Procedure(proc) = &program.external_head[0] else {
            panic!("expected external procedure");
        };
        assert_eq!(proc.items.len(), 2);
        assert!(proc.specification.is_none());
    }

    #[test]
    fn parses_external_procedure_with_specification() {
        let program = parse_source(
            "external procedure OutText is procedure OutText(text value); begin end; begin end;",
        );
        let crate::ast::ExternalDeclaration::Procedure(proc) = &program.external_head[0] else {
            panic!("expected external procedure");
        };
        let spec = proc.specification.as_ref().unwrap();
        assert_eq!(spec.name, "OutText");
        assert_eq!(spec.parameters.len(), 1);
    }

    #[test]
    fn parses_block_level_external_declarations() {
        let program =
            parse_source("begin external class d; external procedure aproc; integer x; end;");
        assert_eq!(program.blocks[0].externals.len(), 2);
    }

    #[test]
    fn parses_external_identification() {
        let program = parse_source(r#"external procedure foo = "libfoo"; begin end;"#);
        let crate::ast::ExternalDeclaration::Procedure(proc) = &program.external_head[0] else {
            panic!("expected external procedure");
        };
        assert_eq!(proc.items[0].identification.as_deref(), Some("libfoo"));
    }

    #[test]
    fn parses_single_statement_main_program() {
        let program = parse_source(r#"OutText("hi");"#);
        assert_eq!(program.blocks.len(), 1);
        assert_eq!(program.blocks[0].statements.len(), 1);
    }
}
