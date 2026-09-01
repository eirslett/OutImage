//! Block parsers (Simula Standard §4.6, §5).

use chumsky::prelude::*;

use super::array::parser as array_parser;
use super::declaration::parser as declaration_parser;
use super::expr::prefix as expr_parser;
use super::form::{is_array_start, is_class_start, is_procedure_start, is_type_declaration_start};
use super::stmt::{labeled_parser, statement_choice};
use super::switch::parser as switch_parser;
use super::tokens::{
    ParseExtra, identifier, keyword, kind, name_identifier, optional_semicolon, span_with,
};
use crate::ast::{Block, Declaration, Expr, ExprKind, Statement, StatementKind, Variable};
use crate::lex::{Keyword, Token, TokenKind};

use super::bridge::{cursor_bridge, guarded_parser};
use super::decl::{class_parser, procedure_parser};
use super::external::{declaration_parser as external_declaration_parser, is_external_start};
use crate::error::CompileError;

pub(in crate::parse) fn is_block_start(tokens: &[Token]) -> bool {
    matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::Begin))
    ) || locate_block_begin(tokens, 0).is_ok()
}

fn is_begin_start(tokens: &[Token]) -> bool {
    is_block_start(tokens)
}

pub fn parser<'a>() -> Boxed<'a, 'a, &'a [Token], Block, ParseExtra<'a>> {
    recursive(|block| {
        let statement = labeled_parser(statement_choice(block.clone()));
        let block_item = choice((
            kind(TokenKind::Semicolon).map_with(|_, extra| BlockPart::Dummy(span_with(extra))),
            guarded_parser(is_begin_start, block.clone()).map(BlockPart::Nested),
            external_declaration_item(),
            procedure_declaration_item(),
            class_declaration_item(),
            array_declaration_item(),
            type_declaration_item(),
            switch_parser().map(BlockPart::Switch),
            statement.map(BlockPart::Statement),
        ));

        block_prefix()
            .or_not()
            .then(
                keyword(Keyword::Begin)
                    .ignore_then(block_item.repeated().collect::<Vec<_>>())
                    .then_ignore(keyword(Keyword::End))
                    .then(identifier().or_not())
                    .then(optional_semicolon()),
            )
            .map(|(prefix, ((parts, name), _))| {
                assemble_block(parts, prefix, name.unwrap_or_default())
            })
            .labelled("a `begin` … `end` block")
    })
    .boxed()
}

pub(in crate::parse) fn class_members_parser<'a>()
-> Boxed<'a, 'a, &'a [Token], Block, ParseExtra<'a>> {
    cursor_bridge(|parser| parser.parse_class_members()).boxed()
}

pub(in crate::parse) fn block_prefix<'a>()
-> impl Parser<'a, &'a [Token], Expr, ParseExtra<'a>> + Clone {
    let expr = expr_parser();
    let argument_list = expr
        .clone()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>();

    let simple_or_call = name_identifier()
        .then(
            kind(TokenKind::LeftParen)
                .ignore_then(argument_list)
                .then_ignore(kind(TokenKind::RightParen))
                .or_not(),
        )
        .map_with(|(name, arguments), extra| {
            let kind = match arguments {
                Some(arguments) => ExprKind::FunctionCall { name, arguments },
                None => ExprKind::Variable(Variable::Simple(name)),
            };
            Expr::new(kind, span_with(extra))
        });

    // `this Class` is parsed so semantic analysis can reject it (§5.5.1.6).
    let this_prefix = keyword(Keyword::This)
        .ignore_then(name_identifier())
        .map_with(|class_name, extra| Expr::new(ExprKind::This(class_name), span_with(extra)));

    choice((this_prefix, simple_or_call))
}

enum BlockPart {
    Dummy(crate::error::Span),
    Nested(Block),
    External(crate::ast::ExternalDeclaration),
    Procedure(crate::ast::ProcedureDeclaration),
    Class(crate::ast::ClassDeclaration),
    Array(crate::ast::ArrayDeclaration),
    Declaration(Declaration),
    Switch(crate::ast::SwitchDeclaration),
    Statement(Statement),
}

fn assemble_block(parts: Vec<BlockPart>, prefix: Option<Expr>, name: String) -> Block {
    let mut block = Block {
        prefix,
        name,
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

    for part in parts {
        match part {
            BlockPart::Dummy(span) => block
                .statements
                .push(Statement::new(StatementKind::Dummy, span)),
            // Keep nested `begin`…`end` in statement order. Parking them in
            // `body` made later statements run first (DosTestBatch simtst08).
            BlockPart::Nested(nested) => block
                .statements
                .push(Statement::new(StatementKind::Compound(nested), 0..0)),
            BlockPart::External(ext) => block.externals.push(ext),
            BlockPart::Procedure(proc) => block.procedures.push(proc),
            BlockPart::Class(class) => block.classes.push(class),
            BlockPart::Array(array) => block.arrays.push(array),
            BlockPart::Declaration(decl) => block.declarations.push(decl),
            BlockPart::Switch(sw) => block.switches.push(sw),
            BlockPart::Statement(stmt) => block.statements.push(stmt),
        }
    }

    block
}

fn external_declaration_item<'a>() -> impl Parser<'a, &'a [Token], BlockPart, ParseExtra<'a>> + Clone
{
    guarded_parser(is_external_start, external_declaration_parser())
        .then_ignore(optional_semicolon())
        .map(BlockPart::External)
}

fn procedure_declaration_item<'a>()
-> impl Parser<'a, &'a [Token], BlockPart, ParseExtra<'a>> + Clone {
    guarded_parser(is_procedure_start, procedure_parser()).map(BlockPart::Procedure)
}

fn class_declaration_item<'a>() -> impl Parser<'a, &'a [Token], BlockPart, ParseExtra<'a>> + Clone {
    guarded_parser(is_class_start, class_parser()).map(BlockPart::Class)
}

fn type_declaration_item<'a>() -> impl Parser<'a, &'a [Token], BlockPart, ParseExtra<'a>> + Clone {
    guarded_parser(is_type_declaration_start, declaration_parser()).map(BlockPart::Declaration)
}

fn array_declaration_item<'a>() -> impl Parser<'a, &'a [Token], BlockPart, ParseExtra<'a>> + Clone {
    guarded_parser(is_array_start, array_parser().boxed()).map(BlockPart::Array)
}

pub(in crate::parse) fn locate_block_begin(
    tokens: &[Token],
    start: usize,
) -> Result<usize, CompileError> {
    match tokens.get(start).map(|token| &token.kind) {
        Some(TokenKind::Keyword(Keyword::Begin)) => Ok(start),
        Some(TokenKind::Identifier(_)) => {
            let mut index = start + 1;
            if matches!(
                tokens.get(index).map(|token| &token.kind),
                Some(TokenKind::LeftParen)
            ) {
                index = skip_balanced_parens(tokens, index)?;
            }
            match tokens.get(index).map(|token| &token.kind) {
                Some(TokenKind::Keyword(Keyword::Begin)) => Ok(index),
                Some(kind) => Err(crate::diagnostics::unexpected_token(
                    &crate::diagnostics::token_english(kind),
                    Some("expected `begin`".into()),
                    tokens[index].span.clone(),
                    &["a block".into()],
                )),
                None => Err(crate::diagnostics::unexpected_eof(Some(
                    "expected `begin`".into(),
                ))),
            }
        }
        Some(kind) => Err(crate::diagnostics::unexpected_token(
            &crate::diagnostics::token_english(kind),
            Some("expected a block".into()),
            tokens[start].span.clone(),
            &["a block".into()],
        )),
        None => Err(crate::diagnostics::unexpected_eof(Some(
            "expected a block".into(),
        ))),
    }
}

fn skip_balanced_parens(tokens: &[Token], open: usize) -> Result<usize, CompileError> {
    let Some(token) = tokens.get(open) else {
        return Err(crate::diagnostics::unexpected_eof(Some(
            "expected `(`".into(),
        )));
    };
    if !matches!(token.kind, TokenKind::LeftParen) {
        return Err(crate::diagnostics::unexpected_token(
            &crate::diagnostics::token_english(&token.kind),
            Some("expected `(`".into()),
            token.span.clone(),
            &[],
        ));
    }

    let mut depth = 0;
    let mut index = open;
    while index < tokens.len() {
        match &tokens[index].kind {
            TokenKind::LeftParen => depth += 1,
            TokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return Ok(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }

    Err(crate::diagnostics::unexpected_eof(Some(
        "expected `)`".into(),
    )))
}

/// Token index after a `begin`…`end` block, including optional block-prefix,
/// optional block name, and semicolon.
pub(in crate::parse) fn block_extent(
    tokens: &[Token],
    start: usize,
) -> Result<usize, CompileError> {
    let begin = locate_block_begin(tokens, start)?;

    let mut depth = 0;
    let mut index = begin;
    while index < tokens.len() {
        match &tokens[index].kind {
            TokenKind::Keyword(Keyword::Begin) => depth += 1,
            TokenKind::Keyword(Keyword::End) => {
                depth -= 1;
                if depth == 0 {
                    index += 1;
                    if matches!(
                        tokens.get(index).map(|token| &token.kind),
                        Some(TokenKind::Identifier(_))
                    ) {
                        index += 1;
                    }
                    if matches!(
                        tokens.get(index).map(|token| &token.kind),
                        Some(TokenKind::Semicolon)
                    ) {
                        index += 1;
                    }
                    return Ok(index);
                }
            }
            _ => {}
        }
        index += 1;
    }

    Err(crate::diagnostics::missing_end(
        tokens.last().map(|token| token.span.clone()),
        tokens.iter().rev().find_map(|token| {
            if matches!(token.kind, TokenKind::Keyword(Keyword::Begin)) {
                Some(token.span.clone())
            } else {
                None
            }
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expr;
    use crate::parse::test_support::{parse_combinator_source, parse_program};
    use crate::types::ArithmeticLiteralKind;

    #[test]
    fn parses_block_with_declaration() {
        let block = parse_combinator_source!("begin integer i; end", parser());
        assert_eq!(block.declarations.len(), 1);
    }

    fn parse_source(source: &str) -> crate::ast::Program {
        parse_program(source)
    }

    fn first_compound(program: &crate::ast::Program) -> &crate::ast::Block {
        match &program.blocks[0].statements[0].kind {
            StatementKind::Compound(block) => block,
            other => panic!("expected compound statement, got {other:?}"),
        }
    }

    #[test]
    fn parses_prefixed_block_statement() {
        let program = parse_source("begin demos begin outimage; end; end;");
        let block = first_compound(&program);
        assert_eq!(
            block.prefix.as_ref().map(|e| &e.kind),
            Some(&ExprKind::Variable(crate::ast::Variable::Simple(
                "demos".into()
            )))
        );
    }

    #[test]
    fn parses_prefixed_block_with_arguments() {
        let program = parse_source("begin myprefix(1, 2) begin outimage; end; end;");
        let block = first_compound(&program);
        assert!(matches!(
            block.prefix.as_ref().map(|e| &e.kind),
            Some(ExprKind::FunctionCall { name, .. }) if name == "myprefix"
        ));
    }

    #[test]
    fn parses_external_class_with_prefixed_demo_blocks() {
        let program = parse_source("begin external class demos; demos begin outimage; end; end;");
        assert_eq!(program.blocks[0].externals.len(), 1);
        let block = first_compound(&program);
        assert!(block.prefix.is_some());
    }

    #[test]
    fn parses_remote_call_inside_prefixed_block() {
        parse_source("begin demos begin jetties.acquire(1); end; end;");
    }

    #[test]
    fn parses_demosex_program_zero() {
        parse_source("begin external class demos; demos begin outimage; noreport; end; end;");
    }

    #[test]
    fn parses_demosex_program_two() {
        parse_source(
            r#"begin external class demos;
               demos begin
                 ref(rdist)next;
                 entity class boat;
                 begin new boat("x").schedule(next.sample); end***boat***;
               end;
               end;"#,
        );
    }

    #[test]
    fn parses_demosex_program_six_header() {
        parse_source(
            r#"begin external class demos;
               demos begin
                 ref(bin)array q(1:2);
                 entity class ferry;
                 begin
                   while c < 6 and q(side).avail > 0 do
                   begin q(side).take(1); end;
                 end***ferry***;
               end;
               end;"#,
        );
    }

    #[test]
    fn parses_repeat_statement() {
        parse_source("begin repeat; end;");
    }

    #[test]
    fn parses_remote_access_wait_statement() {
        parse_source("begin cpuq.wait; end;");
    }

    #[test]
    fn parses_reference_assignment_without_space_after_colon_dash() {
        parse_source("begin p :-cpuq.coopt; end;");
    }

    #[test]
    fn parses_entity_class_with_formal_parameter() {
        parse_source(
            "begin entity class arrival(side); integer side; begin repeat; end***arrival***; end;",
        );
    }

    #[test]
    fn parses_qualified_ref_array_declaration() {
        parse_source("begin ref(waitq)array requestq(1:6); end;");
    }

    #[test]
    fn parses_remote_access_reference_assignment() {
        parse_source("begin q :- requestq(n).coopt; end;");
    }

    #[test]
    fn parses_double_statement_label() {
        parse_source("begin loop: read: outimage; end;");
    }

    #[test]
    fn parses_subscripted_remote_reference_statement() {
        parse_source("begin requestq(n).wait; end;");
    }

    #[test]
    fn parses_subscripted_remote_call_statement() {
        parse_source("begin q(side).take(1); end;");
    }

    #[test]
    fn parses_subscripted_remote_access_in_expression() {
        parse_source("begin x := q(side).avail; end;");
    }

    #[test]
    fn parses_ref_declaration_without_space_before_name() {
        parse_source("begin ref(res)tugs; end;");
    }

    #[test]
    fn parses_declaration_with_equals_initializer() {
        let program = parse_source("begin integer signpos=17; end;");
        assert_eq!(
            program.blocks[0].declarations[0].items[0].initializer,
            Some(Expr::dummy(ExprKind::NumberLiteral {
                lexeme: "17".into(),
                kind: ArithmeticLiteralKind::Integer,
            }))
        );
        assert!(program.blocks[0].declarations[0].items[0].is_constant);
    }

    #[test]
    fn parses_subscripted_remote_assign_with_conditional() {
        parse_source(r#"begin heading.sub(1, 5):= if true then " up" else " down"; end;"#);
    }

    #[test]
    fn parses_demosex_program_one() {
        parse_source(
            r#"begin external class demos;
               demos begin
                 ref(res)tugs, jetties;
                 entity class boat;
                 begin
                   jetties.acquire(1);
                 end***boat***;
                 new boat("boat").schedule(0.0);
               end;
               end;"#,
        );
    }
}
