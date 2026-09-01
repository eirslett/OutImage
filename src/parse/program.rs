//! Program-level parser.

use crate::ast::{Block, Program, Statement};
use crate::error::CompileError;
use crate::lex::{Keyword, TokenStream};

use super::Cursor;
use super::block::is_block_start;
use super::decl::{class_parser, procedure_parser};
use super::take_stashed_parse_error;
use super::tokens::emit_prefix;

pub fn parse(stream: &TokenStream) -> Result<Program, CompileError> {
    let _ = take_stashed_parse_error();
    Cursor::new(stream).parse_program()
}

impl<'a> Cursor<'a> {
    pub(in crate::parse) fn parse_program(mut self) -> Result<Program, CompileError> {
        let directives = Vec::new();
        let mut external_head = Vec::new();
        let mut blocks = Vec::new();

        while !self.is_at_end() {
            if self.check_external_start() {
                external_head.push(self.parse_external_declaration()?);
                self.consume_optional_semicolon();
            } else if self.check_procedure_start() {
                let (procedure, consumed) =
                    emit_prefix(&self.tokens[self.index..], self.index, procedure_parser())?;
                self.index += consumed;
                blocks.push(Self::wrap_top_level_procedure(procedure));
            } else if self.check_class_start() {
                let (class, consumed) =
                    emit_prefix(&self.tokens[self.index..], self.index, class_parser())?;
                self.index += consumed;
                blocks.push(Self::wrap_top_level_class(class));
            } else if self.check_keyword(Keyword::Begin)
                || is_block_start(&self.tokens[self.index..])
            {
                blocks.push(self.parse_block()?);
            } else if blocks.is_empty() {
                let statement = self.parse_labeled_statement()?;
                blocks.push(Self::wrap_main_program_statement(statement));
                break;
            } else {
                let token = &self.tokens[self.index];
                return Err(crate::diagnostics::unexpected_token(
                    &crate::diagnostics::token_english(&token.kind),
                    None,
                    token.span.clone(),
                    &["a top-level construct".into()],
                ));
            }
        }

        Ok(Program {
            directives,
            external_head,
            blocks,
        })
    }

    pub(in crate::parse) fn wrap_main_program_statement(statement: Statement) -> Block {
        Block {
            prefix: None,
            name: String::new(),
            directives: Vec::new(),
            externals: Vec::new(),
            declarations: Vec::new(),
            arrays: Vec::new(),
            switches: Vec::new(),
            procedures: Vec::new(),
            classes: Vec::new(),
            statements: vec![statement],
            body: Vec::new(),
        }
    }

    pub(in crate::parse) fn wrap_top_level_procedure(
        procedure: crate::ast::ProcedureDeclaration,
    ) -> Block {
        Block {
            prefix: None,
            name: String::new(),
            directives: Vec::new(),
            externals: Vec::new(),
            declarations: Vec::new(),
            arrays: Vec::new(),
            switches: Vec::new(),
            procedures: vec![procedure],
            classes: Vec::new(),
            statements: Vec::new(),
            body: Vec::new(),
        }
    }

    pub(in crate::parse) fn wrap_top_level_class(class: crate::ast::ClassDeclaration) -> Block {
        Block {
            prefix: None,
            name: String::new(),
            directives: Vec::new(),
            externals: Vec::new(),
            declarations: Vec::new(),
            arrays: Vec::new(),
            switches: Vec::new(),
            procedures: Vec::new(),
            classes: vec![class],
            statements: Vec::new(),
            body: Vec::new(),
        }
    }
}
