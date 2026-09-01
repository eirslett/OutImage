//! Procedure and class declaration parsing (§5.4, §5.5).

use chumsky::prelude::*;

use super::Cursor;
use super::array::parser as array_parser;
use super::block::{class_members_parser, is_block_start};
use super::bridge::cursor_bridge;
use super::declaration::parser as type_declaration_parser;
use super::form::{
    apply_class_mode_part, apply_class_param_default_modes, apply_procedure_mode_part,
    apply_specifications_to_params, class_mode_part_parser, formal_parameters_parser,
    is_class_start, is_prefixed_class_start, is_procedure_start, optional_class_prefix_parser,
    procedure_header_parser, procedure_specification_section_for_formals, protection_part_parser,
    specification_part_parser, validate_formal_parameters, virtual_part_parser,
    virtual_spec_parser,
};
use super::form::{is_array_start, is_type_declaration_start};
use super::stash_parse_error;
use super::switch::parser as switch_parser;
use super::tokens::{
    ParseExtra, emit_prefix, keyword, kind, name_identifier, optional_semicolon, rich_err,
    semicolon, string_literal,
};
use crate::ast::{Block, ClassDeclaration, ProcedureDeclaration, Statement, StatementKind};
use crate::concatenate::detect_inner_marker;
use crate::error::CompileError;
use crate::lex::{Keyword, Token, TokenKind};
use crate::types::Type;

impl<'a> Cursor<'a> {
    pub(in crate::parse) fn check_procedure_start(&self) -> bool {
        is_procedure_start(&self.tokens[self.index..])
    }

    pub(in crate::parse) fn check_class_start(&self) -> bool {
        is_class_start(&self.tokens[self.index..])
    }

    pub(in crate::parse) fn parse_class_members(&mut self) -> Result<Block, CompileError> {
        let mut block = empty_class_body();

        loop {
            if self.is_at_end() {
                break;
            }

            let before = self.index;

            if matches!(
                self.tokens.get(self.index).map(|token| &token.kind),
                Some(TokenKind::Keyword(Keyword::Virtual))
            ) {
                self.expect_keyword(Keyword::Virtual)?;
                if !self.match_kind(&TokenKind::Colon) {
                    return Err(crate::diagnostics::unexpected_token(
                        &crate::diagnostics::token_english(
                            &self
                                .tokens
                                .get(self.index)
                                .map(|t| t.kind.clone())
                                .unwrap_or(TokenKind::Semicolon),
                        ),
                        Some("expected `:` after `virtual`".into()),
                        self.tokens
                            .get(self.index)
                            .map(|t| t.span.clone())
                            .unwrap_or(0..0),
                        &["a virtual specification".into()],
                    ));
                }
                loop {
                    if !super::form::is_virtual_spec_start(&self.tokens[self.index..]) {
                        break;
                    }
                    let before = self.index;
                    let (_, consumed) = emit_prefix(
                        &self.tokens[self.index..],
                        self.index,
                        virtual_spec_parser(),
                    )?;
                    if consumed == 0 {
                        break;
                    }
                    self.index += consumed;
                    while matches!(
                        self.tokens.get(self.index).map(|token| &token.kind),
                        Some(TokenKind::Semicolon)
                    ) {
                        self.index += 1;
                    }
                    if self.index == before {
                        break;
                    }
                }
                continue;
            }
            if self.match_kind(&TokenKind::Semicolon) {
                // Empty statement as the (entire) unbracketed class body —
                // e.g. `Class A;;` before a sibling `A Begin …` (§5.5.1).
                block
                    .statements
                    .push(Statement::dummy(StatementKind::Dummy));
                break;
            }
            if self.check_external_start() {
                block.externals.push(self.parse_external_declaration()?);
                self.consume_optional_semicolon();
                continue;
            }
            if is_block_start(&self.tokens[self.index..]) {
                // A `begin`/`prefix begin` block is the whole unbracketed body.
                if !block.statements.is_empty() {
                    break;
                }
                block.body.push(self.parse_block()?);
                break;
            }
            if self.check_procedure_start() {
                let (procedure, consumed) =
                    emit_prefix(&self.tokens[self.index..], self.index, procedure_parser())?;
                self.index += consumed;
                block.procedures.push(procedure);
                continue;
            }
            if self.check_class_start() {
                // Unbracketed class bodies are a single statement (§5.5.1). After
                // that statement, a following class belongs to the enclosing block
                // (e.g. `Class Coroutine; detach; Coroutine Class Reader;`).
                if !block.statements.is_empty() {
                    break;
                }
                let (class, consumed) =
                    emit_prefix(&self.tokens[self.index..], self.index, class_parser())?;
                self.index += consumed;
                block.classes.push(class);
                continue;
            }
            if is_array_start(&self.tokens[self.index..]) {
                let (array, consumed) = emit_prefix(
                    &self.tokens[self.index..],
                    self.index,
                    array_parser().boxed(),
                )?;
                self.index += consumed;
                block.arrays.push(array);
                continue;
            }
            if is_type_declaration_start(&self.tokens[self.index..]) {
                let (decl, consumed) = emit_prefix(
                    &self.tokens[self.index..],
                    self.index,
                    type_declaration_parser(),
                )?;
                self.index += consumed;
                block.declarations.push(decl);
                continue;
            }
            if matches!(
                self.tokens.get(self.index).map(|token| &token.kind),
                Some(TokenKind::Keyword(Keyword::Switch))
            ) {
                let (switch, consumed) =
                    emit_prefix(&self.tokens[self.index..], self.index, switch_parser())?;
                self.index += consumed;
                block.switches.push(switch);
                continue;
            }
            if let Ok(statement) = self.parse_labeled_statement() {
                block.statements.push(statement);
                // Single-statement unbracketed body (§5.5.1).
                break;
            }

            if self.index == before {
                break;
            }
        }

        if block.directives.is_empty()
            && block.externals.is_empty()
            && block.declarations.is_empty()
            && block.arrays.is_empty()
            && block.switches.is_empty()
            && block.procedures.is_empty()
            && block.classes.is_empty()
            && block.statements.is_empty()
            && block.body.is_empty()
        {
            return Err(crate::diagnostics::unexpected_eof(Some(
                "expected a class member".into(),
            )));
        }

        Ok(block)
    }
}

pub fn procedure_parser<'a>() -> Boxed<'a, 'a, &'a [Token], ProcedureDeclaration, ParseExtra<'a>> {
    procedure_parts_parser()
        .map_with(|parts, extra| (parts, super::tokens::span_with(extra)))
        .try_map(|(parts, span), chumsky_span| {
            assemble_procedure(parts, span).map_err(|error| {
                stash_parse_error(error);
                rich_err(None, chumsky_span)
            })
        })
        .labelled("a procedure declaration")
        .boxed()
}

pub fn class_parser<'a>() -> Boxed<'a, 'a, &'a [Token], ClassDeclaration, ParseExtra<'a>> {
    class_parts_parser()
        .map_with(|parts, extra| (parts, super::tokens::span_with(extra)))
        .try_map(|(parts, span), chumsky_span| {
            assemble_class(parts, span).map_err(|error| {
                stash_parse_error(error);
                rich_err(None, chumsky_span)
            })
        })
        .labelled("a class declaration")
        .boxed()
}

struct ProcedureParts {
    result_type: Option<Type>,
    name: String,
    parameters: Vec<crate::ast::FormalParameter>,
    identification: Option<String>,
    mode_applications: Vec<super::form::ModeApplication>,
    specifications: Vec<crate::ast::Specification>,
    body: ProcedureBodyPart,
}

enum ProcedureBodyPart {
    External,
    Block(Block),
}

fn procedure_parts_parser<'a>()
-> impl Parser<'a, &'a [Token], ProcedureParts, ParseExtra<'a>> + Clone {
    procedure_header_parser()
        .then(name_identifier())
        .then(custom(
            |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                let before = inp.cursor();
                let parameters = formal_parameters_parser()
                    .go_emit(inp)
                    .map_err(|_| rich_err(None, inp.span_since(&before)))?;
                let identification = optional_procedure_identification_parser()
                    .go_emit(inp)
                    .map_err(|_| rich_err(None, inp.span_since(&before)))?;
                let names: Vec<String> = parameters.iter().map(|p| p.name.clone()).collect();
                let (mode_applications, specifications) =
                    procedure_specification_section_for_formals(names)
                        .go_emit(inp)
                        .map_err(|_| rich_err(None, inp.span_since(&before)))?;
                Ok((
                    parameters,
                    identification,
                    mode_applications,
                    specifications,
                ))
            },
        ))
        .then(procedure_body_parser())
        .map(
            |(
                (
                    (result_type, name),
                    (parameters, identification, mode_applications, specifications),
                ),
                body,
            )| {
                ProcedureParts {
                    result_type,
                    name,
                    parameters,
                    identification,
                    mode_applications,
                    specifications,
                    body,
                }
            },
        )
}

fn optional_procedure_identification_parser<'a>()
-> impl Parser<'a, &'a [Token], Option<String>, ParseExtra<'a>> + Clone {
    kind(TokenKind::Eq)
        .ignore_then(string_literal())
        .then_ignore(semicolon().or_not())
        .or_not()
}

fn procedure_body_parser<'a>()
-> impl Parser<'a, &'a [Token], ProcedureBodyPart, ParseExtra<'a>> + Clone {
    choice((
        keyword(Keyword::External)
            .then(semicolon())
            .map(|_| ProcedureBodyPart::External),
        custom(
            |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                let before = inp.cursor();
                if !matches!(
                    inp.peek().map(|token| token.kind.clone()),
                    Some(TokenKind::Keyword(Keyword::Begin))
                ) {
                    return Err(rich_err(None, inp.span_since(&before)));
                }
                super::block::parser()
                    .go_emit(inp)
                    .map(ProcedureBodyPart::Block)
                    .map_err(|_| rich_err(None, inp.span_since(&before)))
            },
        ),
        cursor_bridge(|parser| parser.parse_labeled_statement())
            .map(|statement| ProcedureBodyPart::Block(wrap_single_statement(statement))),
    ))
}

fn assemble_procedure(
    parts: ProcedureParts,
    span: crate::error::Span,
) -> Result<ProcedureDeclaration, CompileError> {
    validate_formal_parameters(&parts.parameters, &parts.name)?;

    let mut parameters = parts.parameters;
    apply_procedure_mode_part(&mut parameters, &parts.mode_applications)?;
    let specifications = parts.specifications;
    apply_specifications_to_params(&mut parameters, &specifications);

    let (body, is_external) = match parts.body {
        ProcedureBodyPart::External => (empty_procedure_body(), true),
        ProcedureBodyPart::Block(body) => (body, false),
    };

    Ok(ProcedureDeclaration {
        result_type: parts.result_type,
        name: parts.name,
        parameters,
        body,
        is_external,
        identification: parts.identification,
        span,
    })
}

struct ClassParts {
    prefix: Option<String>,
    name: String,
    parameters: Vec<crate::ast::FormalParameter>,
    mode_entries: Vec<(Keyword, Option<Vec<String>>, crate::error::Span)>,
    specifications: Vec<crate::ast::Specification>,
    protection_part: Vec<crate::ast::ProtectionSpec>,
    virtual_part: Vec<crate::ast::VirtualSpec>,
    body: Block,
}

fn class_parts_parser<'a>() -> impl Parser<'a, &'a [Token], ClassParts, ParseExtra<'a>> + Clone {
    optional_class_prefix_parser()
        .then(keyword(Keyword::Class))
        .then(name_identifier())
        .then(formal_parameters_parser())
        .then(class_mode_part_parser())
        .then(optional_semicolon())
        .then(specification_part_parser())
        // Do not consume a second heading `;` here — `class A;;` uses the
        // trailing `;` as an empty class-body (simtst62 nested `class A;;`).
        .then(protection_part_parser())
        .then(virtual_part_parser())
        .then(class_body_parser())
        .map(
            |(
                (
                    (
                        ((((((prefix, _), name), parameters), mode_entries), _), specifications),
                        protection_part,
                    ),
                    virtual_part,
                ),
                body,
            )| {
                ClassParts {
                    prefix,
                    name,
                    parameters,
                    mode_entries,
                    specifications,
                    protection_part,
                    virtual_part,
                    body,
                }
            },
        )
}

fn class_body_parser<'a>() -> impl Parser<'a, &'a [Token], Block, ParseExtra<'a>> + Clone {
    choice((
        custom(
            |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                let before = inp.cursor();
                if !is_block_start(inp.slice_from(&inp.cursor()..)) {
                    return Err(rich_err(None, inp.span_since(&before)));
                }
                cursor_bridge(|parser| parser.parse_block())
                    .go_emit(inp)
                    .map_err(|_| rich_err(None, inp.span_since(&before)))
            },
        ),
        // Empty body: `class A;;` — the body is a lone semicolon.
        semicolon().map(|_| empty_class_body()),
        custom(
            |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                // Empty body after a single heading `;` when the next token is
                // a sibling declaration (`class A; class B;`).
                let before = inp.cursor();
                let tokens = inp.slice_from(&inp.cursor()..);
                if is_prefixed_class_start(tokens)
                    || is_class_start(tokens)
                    || is_procedure_start(tokens)
                    || is_type_declaration_start(tokens)
                    || is_array_start(tokens)
                {
                    return Ok(empty_class_body());
                }
                Err(rich_err(None, inp.span_since(&before)))
            },
        ),
        custom(
            |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                let before = inp.cursor();
                if is_block_start(inp.slice_from(&inp.cursor()..)) {
                    return Err(rich_err(None, inp.span_since(&before)));
                }
                class_members_parser()
                    .go_emit(inp)
                    .map_err(|_| rich_err(None, inp.span_since(&before)))
            },
        ),
        custom(
            |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                let before = inp.cursor();
                if is_block_start(inp.slice_from(&inp.cursor()..))
                    || is_procedure_start(inp.slice_from(&inp.cursor()..))
                    || is_class_start(inp.slice_from(&inp.cursor()..))
                    || matches!(
                        inp.peek().map(|token| token.kind.clone()),
                        Some(TokenKind::Keyword(Keyword::Virtual | Keyword::External))
                    )
                    || is_type_declaration_start(inp.slice_from(&inp.cursor()..))
                    || is_array_start(inp.slice_from(&inp.cursor()..))
                {
                    return Err(rich_err(None, inp.span_since(&before)));
                }
                Ok(empty_class_body())
            },
        ),
    ))
}

fn empty_class_body() -> Block {
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
        statements: Vec::new(),
        body: Vec::new(),
    }
}

fn wrap_single_statement(statement: Statement) -> Block {
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

fn assemble_class(
    parts: ClassParts,
    span: crate::error::Span,
) -> Result<ClassDeclaration, CompileError> {
    validate_formal_parameters(&parts.parameters, &parts.name)?;

    let mut parameters = parts.parameters;
    apply_class_mode_part(&parts.name, &mut parameters, &parts.mode_entries)?;
    let specifications = parts.specifications;
    apply_specifications_to_params(&mut parameters, &specifications);
    apply_class_param_default_modes(&mut parameters);

    let mut class = ClassDeclaration {
        prefix: parts.prefix,
        name: parts.name,
        parameters,
        specifications,
        virtual_part: parts.virtual_part,
        protection_part: parts.protection_part,
        protection_map: std::collections::BTreeMap::new(),
        body: parts.body,
        has_inner: false,
        inner_label: None,
        tail_statements: Vec::new(),
        identifier_substitutions: std::collections::BTreeMap::new(),
        span,
    };
    detect_inner_marker(&mut class);
    Ok(class)
}

fn empty_procedure_body() -> Block {
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
        statements: Vec::new(),
        body: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::ParamMode;
    use crate::parse::test_support::{parse_prefix, parse_prefix_range, parse_program, tokens};
    use chumsky::Parser;

    fn parse_source(source: &str) -> crate::ast::Program {
        parse_program(source)
    }

    #[test]
    fn procedure_heading_consumes_through_formal_parameter_semicolon() {
        let stream = tokens("PROCEDURE p(x); NAME x; EXTERNAL;");
        let (_, consumed) = parse_prefix_range(
            stream.as_slice(),
            0..6,
            super::procedure_header_parser()
                .then(super::super::tokens::identifier())
                .then(super::super::form::formal_parameters_parser()),
        );
        assert_eq!(consumed, 6);
    }

    #[test]
    fn procedure_parts_parses_name_mode() {
        parse_prefix!(
            "PROCEDURE p(x); NAME x; EXTERNAL;",
            super::procedure_parts_parser()
        );
    }

    #[test]
    fn procedure_spec_section_matches_name_and_types() {
        let stream = tokens("PROCEDURE draw(a, u); NAME u; REAL a; INTEGER u; EXTERNAL;");
        let ((modes, specs), consumed) = parse_prefix_range(
            stream.as_slice(),
            8..17,
            super::super::form::procedure_specification_section_parser(),
        );
        assert_eq!(modes.len(), 1);
        assert_eq!(specs.len(), 2);
        assert_eq!(consumed, 9);
    }

    #[test]
    fn procedure_mode_part_matches_name_in_procedure_context() {
        let stream = tokens("PROCEDURE p(x); NAME x; EXTERNAL;");
        let (modes, _) = parse_prefix_range(
            stream.as_slice(),
            6..8,
            super::super::form::procedure_mode_part_parser(),
        );
        assert_eq!(modes.len(), 1);
    }

    #[test]
    fn procedure_mode_part_matches_name() {
        let (modes, consumed) =
            parse_prefix!("name x", super::super::form::procedure_mode_part_parser());
        assert_eq!(modes.len(), 1);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn parses_procedure_with_value_mode_after_directive() {
        parse_prefix!(
            "PROCEDURE p(x);\n% comment\nVALUE x;\nINTEGER x;\nEXTERNAL;",
            super::procedure_parser(),
        );
    }

    #[test]
    fn parses_procedure_with_name_mode_spec() {
        parse_prefix!(
            "PROCEDURE p(x); NAME x; EXTERNAL;",
            super::procedure_parser()
        );

        let program = parse_source("begin procedure p(x); name x; external; end;");
        let proc = &program.blocks[0].procedures[0];
        assert_eq!(proc.name, "p");
        assert_eq!(proc.parameters[0].mode, ParamMode::Name);
        assert!(proc.is_external);
    }

    #[test]
    fn parses_top_level_class_with_external_name_procedure() {
        parse_program(
            r#"CLASS Environment;
               BEGIN
                   BOOLEAN PROCEDURE draw(a, u); NAME u; REAL a; INTEGER u; EXTERNAL;
                   LONG REAL maxreal;
               END;"#,
        );
    }

    #[test]
    fn parses_parameterless_procedure_declaration() {
        let program = parse_source("begin procedure p; begin OutImage; end; end;");
        assert_eq!(program.blocks[0].procedures.len(), 1);
        assert_eq!(program.blocks[0].procedures[0].name, "p");
        assert!(program.blocks[0].procedures[0].parameters.is_empty());
    }

    #[test]
    fn parses_formal_procedure_spec_immediately_before_body() {
        let program = parse_source(
            "begin procedure P(F); procedure F; begin F; end; procedure Q; begin end; P(Q); end;",
        );
        let proc = &program.blocks[0].procedures[0];
        assert_eq!(proc.name, "P");
        assert_eq!(proc.parameters.len(), 1);
        assert!(proc.parameters[0].is_procedure);
        assert_eq!(program.blocks[0].procedures.len(), 2);
    }

    #[test]
    fn parses_defining_procedure_export_identification() {
        let program = parse_source(
            r#"begin integer procedure tick = "export:tick"; tick := 1; OutInt(tick, 0); OutImage; end;"#,
        );
        let proc = &program.blocks[0].procedures[0];
        assert_eq!(proc.name, "tick");
        assert_eq!(proc.identification.as_deref(), Some("export:tick"));
        assert!(!proc.is_external);
    }

    #[test]
    fn parses_parameterless_external_procedure() {
        let program = parse_source("begin class E; begin procedure time; external; end E; end;");
        let proc = &program.blocks[0].classes[0].body.procedures[0];
        assert_eq!(proc.name, "time");
        assert!(proc.is_external);
        assert!(proc.parameters.is_empty());
    }

    #[test]
    fn parses_procedure_with_empty_statement_body_after_specs() {
        use crate::ast::StatementKind;

        let program =
            parse_source("begin class C; begin procedure hold(t); long real t; ; end C; end;");
        let proc = &program.blocks[0].classes[0].body.procedures[0];
        assert_eq!(proc.name, "hold");
        assert_eq!(proc.parameters.len(), 1);
        assert!(
            matches!(proc.body.statements.as_slice(), [stmt] if matches!(stmt.kind, StatementKind::Dummy))
        );
    }

    #[test]
    fn parses_procedure_with_single_statement_body() {
        let program = parse_source(
            "begin class Environment; begin procedure inimage; SysIn.inimage; end Environment; end;",
        );
        let proc = &program.blocks[0].classes[0].body.procedures[0];
        assert_eq!(proc.name, "inimage");
        assert!(!proc.is_external);
        assert_eq!(proc.body.statements.len(), 1);
    }

    #[test]
    fn parses_typed_procedure_with_value_parameter() {
        let program = parse_source(
            "begin integer procedure square(x) value; begin square := x * x; end; end;",
        );
        let proc = &program.blocks[0].procedures[0];
        assert_eq!(proc.name, "square");
        assert_eq!(proc.parameters.len(), 1);
        assert_eq!(proc.parameters[0].mode, ParamMode::Value);
    }

    #[test]
    fn parses_class_with_multiple_virtual_specs() {
        let program = parse_source(
            "begin class A; virtual: procedure P; integer procedure iP; real procedure rP; begin end; end;",
        );
        assert_eq!(program.blocks[0].classes[0].virtual_part.len(), 3);
    }

    #[test]
    fn parses_typed_virtual_procedure_specs() {
        parse_source(
            "class A;\nvirtual:\n procedure P;\n integer procedure iP;\n real procedure rP;\n ref(A) procedure cP;\nbegin\n end;",
        );
    }

    #[test]
    fn parses_virtual_find_absolute_pos_spec() {
        parse_source(
            "class C;\nVirtual:\n procedure FindAbsolutePos Is procedure FindAbsolutePos( x, y ); name x, y; integer x, y;\n;\nbegin\n end;",
        );
    }

    #[test]
    fn parses_virtual_create_window_spec() {
        parse_source(
            "class C;\nVirtual:\n procedure CreateWindow Is procedure CreateWindow;\n;\nbegin\n end;",
        );
    }

    #[test]
    fn parses_prefixed_class_with_virtual_is_after_externals() {
        parse_source(
            "Class Toolkit;\nBEGIN\n Real version = 5.0;\nEND;\nExternal Class core;\nExternal Class directory;\nXWindow Class Window;\nVirtual:\n procedure Handle_ButtonClick Is procedure Handle_ButtonClick( B ); ref(Button) B;\n;\nbegin\n end;",
        );
    }

    #[test]
    fn parses_virtual_window_kind_spec() {
        parse_source(
            "class C;\nVirtual:\n procedure window_kind Is text procedure window_kind;\n;\n procedure CreateWindow Is procedure CreateWindow;\n;\nbegin\n end;",
        );
    }

    #[test]
    fn parses_class_with_mid_body_virtual() {
        parse_source(
            "Class XDisplay;\nBEGIN\n integer x;\n element Class XWindow;\nVirtual:\n procedure p is procedure p;;\nBEGIN\n end;\nend;",
        );
    }

    #[test]
    fn parses_external_getcwd_in_block() {
        parse_source(
            "begin EXTERNAL c PROCEDURE getcwd IS PROCEDURE getcwd( dir, BufLen ); TEXT dir; integer BufLen; ; end;",
        );
    }

    #[test]
    fn parses_external_opendir_in_block() {
        parse_source(
            "begin EXTERNAL C PROCEDURE opendir IS integer procedure opendir(File);TEXT File;; end;",
        );
    }

    #[test]
    fn parses_class_with_procedure_heading_before_begin() {
        parse_source("class UNIX;\ninteger procedure ARGC;\nbegin\n ARGC := 1;\nend;");
    }

    #[test]
    fn parses_top_level_class_after_external_classes() {
        parse_source(
            "Class Core;\nBEGIN\n Real version = 4.9;\nEND;\nExternal Class containers;\nExternal Class Utilities;\nClass XDisplay;\nbegin\nend;",
        );
    }

    #[test]
    fn parses_top_level_class_then_typed_procedure() {
        parse_source(
            "External CLASS TextUtil;\nclass CONTAINERS;\nbegin\n Text version = \"v6.4\";\nend;\ninteger procedure DEFAULT_HASH(t); text t;\nbegin\n DEFAULT_HASH := 1;\nend;",
        );
    }

    #[test]
    fn parses_class_declaration() {
        let program = parse_source("begin class Node; begin integer x; end Node; end;");
        assert_eq!(program.blocks[0].classes.len(), 1);
        assert_eq!(program.blocks[0].classes[0].name, "Node");
    }

    #[test]
    fn parses_prefixed_class_declaration() {
        let program = parse_source(
            "begin class Point; begin integer x; end; Point class Polar; begin real r; end; end;",
        );
        let polar = &program.blocks[0].classes[1];
        assert_eq!(polar.prefix.as_deref(), Some("Point"));
        assert_eq!(polar.name, "Polar");
    }

    #[test]
    fn parses_empty_prefixed_class_declaration() {
        let program = parse_source("begin tally class notally;; end;");
        assert_eq!(program.blocks[0].classes.len(), 1);
        let notally = &program.blocks[0].classes[0];
        assert_eq!(notally.prefix.as_deref(), Some("tally"));
        assert_eq!(notally.name, "notally");
        assert!(notally.body.declarations.is_empty());
        assert!(notally.body.statements.is_empty());
    }

    #[test]
    fn empty_class_body_inside_class_does_not_swallow_following_statements() {
        // simtst62: `class X; begin class A;; trace(...); detach; end;`
        let program = parse_source(
            r#"begin
                class X; begin
                    class A;;
                    OutText("new X");
                    detach;
                end;
               end;"#,
        );
        let x = program.blocks[0]
            .classes
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("X"))
            .expect("class X");
        assert_eq!(x.body.classes.len(), 1);
        assert!(
            x.body.classes[0].body.statements.is_empty(),
            "empty Class A;; must not swallow X's statements"
        );
        assert!(
            !x.body.statements.is_empty(),
            "OutText/detach must remain in X's body"
        );
    }

    #[test]
    fn empty_class_body_does_not_swallow_following_class() {
        let program = parse_source(
            r#"begin
                class A;;
                class X; begin
                    A begin end;
                end;
               end;"#,
        );
        let block = &program.blocks[0];
        assert_eq!(
            block.classes.len(),
            2,
            "class X must be a sibling of empty class A, not nested inside it"
        );
        let a = block
            .classes
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("A"))
            .expect("class A");
        assert!(
            a.body.classes.is_empty(),
            "empty Class A;; must not swallow following Class X"
        );
        assert!(
            block
                .classes
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case("X"))
        );
    }

    #[test]
    fn empty_class_body_does_not_swallow_following_procedure() {
        let program = parse_source(
            r#"begin
                class A; begin integer x; end;
                A class B;;
                procedure Setvariables; begin end;
                boolean bv;
               end;"#,
        );
        let block = &program.blocks[0];
        assert_eq!(block.classes.len(), 2);
        let b = block
            .classes
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("B"))
            .expect("class B");
        assert!(
            b.body.procedures.is_empty(),
            "Setvariables must stay outer, not inside B"
        );
        assert_eq!(block.procedures.len(), 1);
        assert!(
            block.procedures[0]
                .name
                .eq_ignore_ascii_case("Setvariables")
        );
        assert_eq!(block.declarations.len(), 1);
    }

    #[test]
    fn parses_prefixed_class_with_semicolon_before_body() {
        let program = parse_source("begin tab class count; begin integer obs; end; end;");
        assert_eq!(program.blocks[0].classes.len(), 1);
        let count = &program.blocks[0].classes[0];
        assert_eq!(count.prefix.as_deref(), Some("tab"));
        assert_eq!(count.name, "count");
        assert_eq!(count.body.declarations.len(), 1);
    }

    #[test]
    fn parses_virtual_procedure_is_spec() {
        let program = parse_source(
            "begin class C; virtual: procedure hash is integer procedure hash(t); text t;; begin end; end;",
        );
        let class = &program.blocks[0].classes[0];
        assert_eq!(class.virtual_part.len(), 1);
        assert_eq!(class.virtual_part[0].names, vec!["hash"]);
    }

    #[test]
    fn parses_virtual_part() {
        let program = parse_source(
            "begin class Hashing(n); integer n; virtual: integer procedure hash; begin end; end;",
        );
        let class = &program.blocks[0].classes[0];
        assert_eq!(class.virtual_part.len(), 1);
        assert_eq!(class.virtual_part[0].names, vec!["hash"]);
    }

    #[test]
    fn parses_protection_part() {
        let program =
            parse_source("begin class Secret; protected hidden key; begin integer key; end; end;");
        let class = &program.blocks[0].classes[0];
        assert_eq!(class.protection_part.len(), 1);
        assert!(class.protection_part[0].protected);
        assert!(class.protection_part[0].hidden);
    }
}
