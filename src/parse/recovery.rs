//! Best-effort parse recovery for IDE / analysis paths.
//!
//! [`parse_lenient`] always returns a [`Program`] (possibly empty or incomplete)
//! plus every parse diagnostic encountered. The strict [`super::parse`] entry
//! remains fail-fast for the CLI / codegen pipeline.

use crate::ast::{Block, Expr, Program, Statement, StatementKind};
use crate::error::CompileError;
use crate::lex::{Keyword, TokenKind, TokenStream};
use chumsky::Parser;

use super::Cursor;
use super::array::parser as array_parser;
use super::block::{block_prefix, is_block_start, locate_block_begin};
use super::decl::{class_parser, procedure_parser};
use super::declaration::parser as type_declaration_parser;
use super::external::is_external_start;
use super::form::{is_array_start, is_class_start, is_procedure_start, is_type_declaration_start};
use super::switch::parser as switch_parser;
use super::take_stashed_parse_error;
use super::tokens::emit_prefix;

/// Parse with statement / block-item recovery, returning a partial AST.
pub fn parse_lenient(stream: &TokenStream) -> (Program, Vec<CompileError>) {
    let _ = take_stashed_parse_error();
    Cursor::new(stream).parse_program_lenient()
}

impl<'a> Cursor<'a> {
    pub(in crate::parse) fn parse_program_lenient(mut self) -> (Program, Vec<CompileError>) {
        let mut errors = Vec::new();
        let mut external_head = Vec::new();
        let mut blocks = Vec::new();

        while !self.is_at_end() {
            let before = self.index;
            if self.check_external_start() {
                match self.parse_external_declaration() {
                    Ok(ext) => {
                        external_head.push(ext);
                        self.consume_optional_semicolon();
                    }
                    Err(error) => self.recover_top_level(&mut errors, error, before),
                }
            } else if self.check_procedure_start() {
                match emit_prefix(&self.tokens[self.index..], self.index, procedure_parser()) {
                    Ok((procedure, consumed)) => {
                        self.index += consumed;
                        blocks.push(Self::wrap_top_level_procedure(procedure));
                    }
                    Err(error) => self.recover_top_level(&mut errors, error, before),
                }
            } else if self.check_class_start() {
                match emit_prefix(&self.tokens[self.index..], self.index, class_parser()) {
                    Ok((class, consumed)) => {
                        self.index += consumed;
                        blocks.push(Self::wrap_top_level_class(class));
                    }
                    Err(error) => self.recover_top_level(&mut errors, error, before),
                }
            } else if self.check_keyword(Keyword::Begin)
                || is_block_start(&self.tokens[self.index..])
            {
                match self.parse_block_lenient(&mut errors) {
                    Ok(block) => blocks.push(block),
                    Err(error) => self.recover_top_level(&mut errors, error, before),
                }
            } else if blocks.is_empty() {
                match self.parse_labeled_statement() {
                    Ok(statement) => {
                        blocks.push(Self::wrap_main_program_statement(statement));
                        break;
                    }
                    Err(error) => self.recover_top_level(&mut errors, error, before),
                }
            } else {
                let token = &self.tokens[self.index];
                let error = crate::diagnostics::unexpected_token(
                    &crate::diagnostics::token_english(&token.kind),
                    None,
                    token.span.clone(),
                    &["a top-level construct".into()],
                );
                self.recover_top_level(&mut errors, error, before);
            }

            if self.index == before && !self.is_at_end() {
                self.index += 1;
            }
        }

        (
            Program {
                directives: Vec::new(),
                external_head,
                blocks,
            },
            errors,
        )
    }

    /// Parse one `begin`…`end` block, recovering past broken items.
    pub(in crate::parse) fn parse_block_lenient(
        &mut self,
        errors: &mut Vec<CompileError>,
    ) -> Result<Block, CompileError> {
        let prefix = self.consume_optional_block_prefix(errors)?;
        self.expect_keyword(Keyword::Begin)?;

        let mut block = Block {
            prefix,
            name: String::new(),
            directives: Vec::new(),
            externals: Vec::new(),
            declarations: Vec::new(),
            arrays: Vec::new(),
            switches: Vec::new(),
            procedures: Vec::new(),
            classes: Vec::new(),
            statements: Vec::new(),
            body: Vec::new(),
        };

        loop {
            if self.is_at_end() {
                errors.push(crate::diagnostics::missing_end(
                    self.tokens.last().map(|token| token.span.clone()),
                    self.tokens.iter().rev().find_map(|token| {
                        if matches!(token.kind, TokenKind::Keyword(Keyword::Begin)) {
                            Some(token.span.clone())
                        } else {
                            None
                        }
                    }),
                ));
                break;
            }
            if self.check_keyword(Keyword::End) {
                self.index += 1;
                if let Some(TokenKind::Identifier(name)) = self.peek_kind().cloned() {
                    block.name = name;
                    self.index += 1;
                }
                self.consume_optional_semicolon();
                break;
            }

            let before = self.index;
            if self.match_kind(&TokenKind::Semicolon) {
                let span = self.tokens[before].span.clone();
                block
                    .statements
                    .push(Statement::new(StatementKind::Dummy, span));
                continue;
            }

            if is_block_start(&self.tokens[self.index..]) {
                match self.parse_block_lenient(errors) {
                    Ok(nested) => block.statements.push(crate::ast::Statement::new(
                        crate::ast::StatementKind::Compound(nested),
                        0..0,
                    )),
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            if self.check_external_start() {
                match self.parse_external_declaration() {
                    Ok(ext) => {
                        block.externals.push(ext);
                        self.consume_optional_semicolon();
                    }
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            if is_procedure_start(&self.tokens[self.index..]) {
                match emit_prefix(&self.tokens[self.index..], self.index, procedure_parser()) {
                    Ok((procedure, consumed)) => {
                        self.index += consumed;
                        block.procedures.push(procedure);
                    }
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            if is_class_start(&self.tokens[self.index..]) {
                match emit_prefix(&self.tokens[self.index..], self.index, class_parser()) {
                    Ok((class, consumed)) => {
                        self.index += consumed;
                        block.classes.push(class);
                    }
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            if is_array_start(&self.tokens[self.index..]) {
                match emit_prefix(
                    &self.tokens[self.index..],
                    self.index,
                    array_parser().boxed(),
                ) {
                    Ok((array, consumed)) => {
                        self.index += consumed;
                        block.arrays.push(array);
                    }
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            if is_type_declaration_start(&self.tokens[self.index..]) {
                match emit_prefix(
                    &self.tokens[self.index..],
                    self.index,
                    type_declaration_parser(),
                ) {
                    Ok((decl, consumed)) => {
                        self.index += consumed;
                        block.declarations.push(decl);
                    }
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            if matches!(self.peek_kind(), Some(TokenKind::Keyword(Keyword::Switch))) {
                match emit_prefix(&self.tokens[self.index..], self.index, switch_parser()) {
                    Ok((switch, consumed)) => {
                        self.index += consumed;
                        block.switches.push(switch);
                    }
                    Err(error) => self.recover_block_item(errors, error, before),
                }
                continue;
            }

            match self.parse_labeled_statement() {
                Ok(statement) => block.statements.push(statement),
                Err(error) => self.recover_block_item(errors, error, before),
            }

            if self.index == before && !self.is_at_end() {
                self.index += 1;
            }
        }

        Ok(block)
    }

    fn consume_optional_block_prefix(
        &mut self,
        errors: &mut Vec<CompileError>,
    ) -> Result<Option<Expr>, CompileError> {
        if self.check_keyword(Keyword::Begin) {
            return Ok(None);
        }

        let begin = locate_block_begin(self.tokens, self.index)?;
        if begin == self.index {
            return Ok(None);
        }

        let slice = &self.tokens[self.index..begin];
        match emit_prefix(slice, self.index, block_prefix()) {
            Ok((expr, consumed)) if consumed == slice.len() => {
                self.index = begin;
                Ok(Some(expr))
            }
            Ok(_) | Err(_) => {
                errors.push(crate::diagnostics::unexpected_token(
                    &crate::diagnostics::token_english(&self.tokens[self.index].kind),
                    Some("expected a `begin` block".into()),
                    self.tokens[self.index].span.clone(),
                    &["a block prefix".into()],
                ));
                self.index = begin;
                Ok(None)
            }
        }
    }

    fn recover_block_item(
        &mut self,
        errors: &mut Vec<CompileError>,
        error: CompileError,
        before: usize,
    ) {
        errors.push(error);
        self.sync_to_semicolon_or_block_boundary();
        if self.index == before && !self.is_at_end() {
            self.index += 1;
        }
    }

    fn recover_top_level(
        &mut self,
        errors: &mut Vec<CompileError>,
        error: CompileError,
        before: usize,
    ) {
        errors.push(error);
        self.sync_top_level();
        if self.index == before && !self.is_at_end() {
            self.index += 1;
        }
    }

    /// Skip tokens until `;`, `end`, or `begin` (leaving the latter two for the caller).
    fn sync_to_semicolon_or_block_boundary(&mut self) {
        while !self.is_at_end() {
            match self.peek_kind() {
                Some(TokenKind::Semicolon) => {
                    self.index += 1;
                    return;
                }
                Some(TokenKind::Keyword(Keyword::End | Keyword::Begin)) => return,
                _ => self.index += 1,
            }
        }
    }

    /// Skip until the next plausible top-level construct.
    fn sync_top_level(&mut self) {
        if !self.is_at_end() {
            self.index += 1;
        }
        while !self.is_at_end() {
            if self.check_keyword(Keyword::Begin)
                || is_block_start(&self.tokens[self.index..])
                || self.check_procedure_start()
                || self.check_class_start()
                || is_external_start(&self.tokens[self.index..])
            {
                return;
            }
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ExprKind, StatementKind, Variable};
    use crate::lex::tokenize;
    use crate::source::SourceFile;

    fn lenient(source: &str) -> (Program, Vec<CompileError>) {
        let source = SourceFile {
            name: "<test>".into(),
            text: source.to_owned(),
        };
        let tokens = tokenize(&source).expect("lex");
        parse_lenient(&tokens)
    }

    #[test]
    fn missing_end_keeps_declarations_and_statements() {
        let (program, errors) = lenient("begin integer x; x := 1;");
        assert!(
            errors.iter().any(|e| e.message.contains("end")),
            "{errors:?}"
        );
        assert_eq!(program.blocks.len(), 1);
        assert_eq!(program.blocks[0].declarations.len(), 1);
        assert_eq!(program.blocks[0].statements.len(), 1);
        assert_eq!(program.blocks[0].declarations[0].items[0].name, "x");
    }

    #[test]
    fn bad_statement_recovers_to_following_statement() {
        let (program, errors) = lenient("begin integer x; x := ; OutImage; end;");
        assert!(!errors.is_empty(), "expected parse error for empty RHS");
        assert_eq!(program.blocks.len(), 1);
        assert_eq!(program.blocks[0].declarations.len(), 1);
        assert!(
            program.blocks[0].statements.iter().any(|s| {
                matches!(
                    &s.kind,
                    StatementKind::ProcedureCall(call) if call.name.eq_ignore_ascii_case("OutImage")
                )
            }),
            "statements={:?}",
            program.blocks[0].statements
        );
    }

    #[test]
    fn truncated_declaration_still_yields_block() {
        let (program, errors) = lenient("begin integer");
        assert!(!errors.is_empty());
        assert_eq!(program.blocks.len(), 1);
        assert!(program.blocks[0].declarations.is_empty());
    }

    #[test]
    fn nested_missing_end_recovers_outer() {
        let (program, errors) = lenient("begin begin integer y; end;");
        assert!(
            errors.iter().any(|e| e.message.contains("end")),
            "{errors:?}"
        );
        assert_eq!(program.blocks.len(), 1);
        let StatementKind::Compound(inner) = &program.blocks[0].statements[0].kind else {
            panic!("expected compound nested block");
        };
        assert_eq!(inner.declarations.len(), 1);
    }

    #[test]
    fn clean_program_has_no_recovery_errors() {
        let (program, errors) = lenient("begin integer x; x := 1; end");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(program.blocks.len(), 1);
        assert_eq!(program.blocks[0].declarations.len(), 1);
    }

    #[test]
    fn prefixed_block_still_parses_leniently() {
        let (program, errors) = lenient("begin demos begin OutImage; end; end;");
        assert!(errors.is_empty(), "{errors:?}");
        let StatementKind::Compound(inner) = &program.blocks[0].statements[0].kind else {
            panic!("expected compound nested block");
        };
        assert_eq!(
            inner.prefix.as_ref().map(|e| &e.kind),
            Some(&ExprKind::Variable(Variable::Simple("demos".into())))
        );
    }

    #[test]
    fn keeps_earlier_top_level_block_when_later_fails() {
        let (program, errors) = lenient("begin integer a; a := 1; end; begin integer");
        assert!(!errors.is_empty());
        assert!(
            !program.blocks.is_empty(),
            "expected recovered first block, got {program:?}"
        );
        assert_eq!(program.blocks[0].declarations.len(), 1);
        assert_eq!(program.blocks[0].declarations[0].items[0].name, "a");
    }

    #[test]
    fn strict_parse_still_fails_on_missing_end() {
        let source = SourceFile {
            name: "<test>".into(),
            text: "begin integer x; x := 1;".into(),
        };
        let tokens = tokenize(&source).unwrap();
        assert!(crate::parse::parse(&tokens).is_err());
    }
}
