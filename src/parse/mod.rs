//! Parsing for Simula source.
//!
//! Grammar is expressed with [chumsky](https://github.com/zebulon859/chumsky) combinators;
//! a token [`Cursor`] handles disambiguation that backtracking cannot resolve reliably.

pub(in crate::parse) mod tokens;

mod array;
mod assignment;
mod block;
mod bridge;
mod cursor;
mod decl;
mod declaration;
mod expr;
mod external;
mod form;
mod program;
mod recovery;
mod stmt;
mod switch;
mod type_;
mod variable;

#[cfg(test)]
pub(crate) mod test_support;

pub(in crate::parse) use tokens::{
    ParseExtra, identifier, keyword, keyword_not_followed_by, kind, name_identifier, span_with,
    subscript_delimited,
};

pub(in crate::parse) use cursor::Cursor;

use crate::error::CompileError;
use std::cell::RefCell;

thread_local! {
    static STASHED_PARSE_ERROR: RefCell<Option<CompileError>> = const { RefCell::new(None) };
}

pub(in crate::parse) fn stash_parse_error(error: CompileError) {
    STASHED_PARSE_ERROR.with(|cell| *cell.borrow_mut() = Some(error));
}

pub(in crate::parse) fn take_stashed_parse_error() -> Option<CompileError> {
    STASHED_PARSE_ERROR.with(|cell| cell.borrow_mut().take())
}

pub use program::parse;
pub use recovery::parse_lenient;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        AssignOperator, Assignment, AssignmentRhs, Expr, ExprKind, ProcedureCall, Program,
        Statement, StatementKind, Variable,
    };
    use crate::lex::{Keyword, Token, TokenKind, TokenStream, tokenize};
    use crate::parse::test_support::parse_program;
    use crate::source::SourceFile;
    use crate::types::{ArithmeticLiteralKind, Type};
    use chumsky::Parser;

    fn parse_source(source: &str) -> Program {
        parse_program(source)
    }

    #[test]
    fn empty_token_stream_produces_empty_program() {
        let program = parse(&TokenStream::new(vec![])).unwrap();
        assert!(program.directives.is_empty());
        assert!(program.blocks.is_empty());
    }

    #[test]
    fn parses_begin_end_block() {
        let stream = TokenStream::new(vec![
            Token::kind_only(TokenKind::Keyword(Keyword::Begin)),
            Token::kind_only(TokenKind::Keyword(Keyword::End)),
            Token::kind_only(TokenKind::Semicolon),
        ]);
        let program = parse(&stream).unwrap();
        assert_eq!(program.blocks.len(), 1);
        assert!(program.blocks[0].body.is_empty());
        assert!(program.blocks[0].statements.is_empty());
    }

    #[test]
    fn parses_nested_blocks() {
        let program = parse_source("begin begin end; end;");
        assert_eq!(program.blocks.len(), 1);
        assert!(matches!(
            program.blocks[0].statements[0].kind,
            StatementKind::Compound(_)
        ));
    }

    #[test]
    fn parses_nested_end_comment() {
        let program = parse_source("begin begin end end;");
        assert_eq!(program.blocks.len(), 1);
        let StatementKind::Compound(inner) = &program.blocks[0].statements[0].kind else {
            panic!("expected compound nested block");
        };
        assert!(
            inner
                .statements
                .iter()
                .all(|s| !matches!(s.kind, StatementKind::Compound(_)))
        );
    }

    #[test]
    fn parses_outtext_and_outimage() {
        let program = parse_source(r#"begin OutText("hello world"); OutImage; end;"#);
        let statements = &program.blocks[0].statements;

        assert_eq!(statements.len(), 2);
        assert_eq!(
            statements[0],
            Statement::dummy(StatementKind::ProcedureCall(ProcedureCall {
                name: "OutText".into(),
                arguments: vec![Expr::dummy(ExprKind::StringLiteral("hello world".into()))],
            }))
        );
        assert_eq!(
            statements[1],
            Statement::dummy(StatementKind::ProcedureCall(ProcedureCall {
                name: "OutImage".into(),
                arguments: vec![],
            }))
        );
    }

    #[test]
    fn parses_source_with_directives_elided_at_lex_time() {
        let program = parse_source("% first\nbegin\n% second\noutimage;\nend");
        assert!(program.directives.is_empty());
        assert_eq!(program.blocks.len(), 1);
        assert!(program.blocks[0].directives.is_empty());
        assert_eq!(program.blocks[0].statements.len(), 1);
    }

    #[test]
    fn parses_simple_variable_assignment() {
        let program = parse_source("begin A := B; end;");
        assert_eq!(
            program.blocks[0].statements[0],
            Statement::dummy(StatementKind::Assignment(Assignment {
                lhs: Variable::Simple("A".into()),
                operator: AssignOperator::Assign,
                rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Simple(
                    "B".into()
                )))),
            }))
        );
    }

    #[test]
    fn parses_remote_identifier_assignment() {
        let program = parse_source("begin A := B.C; end;");
        assert_eq!(
            program.blocks[0].statements[0],
            Statement::dummy(StatementKind::Assignment(Assignment {
                lhs: Variable::Simple("A".into()),
                operator: AssignOperator::Assign,
                rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Remote {
                    object: Box::new(Variable::Simple("B".into())),
                    attribute: "C".into(),
                }))),
            }))
        );
    }

    #[test]
    fn parses_subscripted_variable_assignment() {
        let program = parse_source("begin A(1) := 1; end;");
        assert_eq!(
            program.blocks[0].statements[0],
            Statement::dummy(StatementKind::Assignment(Assignment {
                lhs: Variable::Subscripted {
                    name: "A".into(),
                    subscripts: vec![Expr::dummy(ExprKind::NumberLiteral {
                        lexeme: "1".into(),
                        kind: ArithmeticLiteralKind::Integer,
                    })],
                },
                operator: AssignOperator::Assign,
                rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::NumberLiteral {
                    lexeme: "1".into(),
                    kind: ArithmeticLiteralKind::Integer,
                })),
            }))
        );
    }

    #[test]
    fn parses_square_bracket_subscript_assignment_by_default() {
        let program = parse_source("begin A[1] := 2; end;");
        assert_eq!(
            program.blocks[0].statements[0],
            Statement::dummy(StatementKind::Assignment(Assignment {
                lhs: Variable::Subscripted {
                    name: "A".into(),
                    subscripts: vec![Expr::dummy(ExprKind::NumberLiteral {
                        lexeme: "1".into(),
                        kind: ArithmeticLiteralKind::Integer,
                    })],
                },
                operator: AssignOperator::Assign,
                rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::NumberLiteral {
                    lexeme: "2".into(),
                    kind: ArithmeticLiteralKind::Integer,
                })),
            }))
        );
    }

    #[test]
    fn parses_square_bracket_subscript_in_expression() {
        let program = parse_source("begin X := A[1]; end;");
        assert_eq!(
            program.blocks[0].statements[0],
            Statement::dummy(StatementKind::Assignment(Assignment {
                lhs: Variable::Simple("X".into()),
                operator: AssignOperator::Assign,
                rhs: AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Subscripted {
                    name: "A".into(),
                    subscripts: vec![Expr::dummy(ExprKind::NumberLiteral {
                        lexeme: "1".into(),
                        kind: ArithmeticLiteralKind::Integer,
                    })],
                }))),
            }))
        );
    }

    #[test]
    fn rejects_square_brackets_in_procedure_calls() {
        let stream = tokenize(&SourceFile::anonymous("begin P[1]; end;")).expect("tokenize");
        assert!(parse(&stream).is_err());
    }

    #[test]
    fn square_brackets_require_compiler_flag_at_lex_time() {
        use crate::lex::{LexOptions, tokenize_with_options};

        let options = LexOptions {
            allow_square_bracket_subscripts: false,
            ..LexOptions::default()
        };
        let error = tokenize_with_options(&SourceFile::anonymous("begin A[1]; end;"), &options)
            .unwrap_err();
        assert!(
            error.to_string().contains('[') && error.to_string().contains("legal character"),
            "{}",
            error
        );
    }

    #[test]
    fn parenthesis_subscripts_still_work_when_brackets_disabled() {
        use crate::lex::{LexOptions, tokenize_with_options};

        let options = LexOptions {
            allow_square_bracket_subscripts: false,
            ..LexOptions::default()
        };
        let stream = tokenize_with_options(&SourceFile::anonymous("begin A(1); end;"), &options)
            .expect("tokenize");
        parse(&stream).expect("parse");
    }

    #[test]
    fn parses_integer_declaration() {
        let program = parse_source("begin integer i; end;");
        assert_eq!(program.blocks[0].declarations.len(), 1);
        assert_eq!(
            program.blocks[0].declarations[0].ty,
            Type::Integer { short: false }
        );
        assert_eq!(program.blocks[0].declarations[0].items[0].name, "i");
        assert!(
            program.blocks[0].declarations[0].items[0]
                .initializer
                .is_none()
        );
    }

    #[test]
    fn parses_short_integer_declaration() {
        let program = parse_source("begin short integer si; end;");
        assert_eq!(
            program.blocks[0].declarations[0].ty,
            Type::Integer { short: true }
        );
    }

    #[test]
    fn parses_long_real_declaration() {
        let program = parse_source("begin long real lr; end;");
        assert_eq!(
            program.blocks[0].declarations[0].ty,
            Type::Real { long: true }
        );
    }

    #[test]
    fn parses_boolean_declaration_with_initializer() {
        let program = parse_source("begin boolean b := true; end;");
        assert_eq!(program.blocks[0].declarations[0].ty, Type::Boolean);
        assert_eq!(
            program.blocks[0].declarations[0].items[0].initializer,
            Some(Expr::dummy(ExprKind::BooleanLiteral(true)))
        );
    }

    #[test]
    fn parses_character_declaration() {
        let program = parse_source("begin character c := 'A'; end;");
        assert_eq!(program.blocks[0].declarations[0].ty, Type::Character);
        assert_eq!(
            program.blocks[0].declarations[0].items[0].initializer,
            Some(Expr::dummy(ExprKind::CharacterLiteral('A')))
        );
    }

    #[test]
    fn parses_text_declaration() {
        let program = parse_source(r#"begin text t := "hello"; end;"#);
        assert_eq!(program.blocks[0].declarations[0].ty, Type::Text);
        assert_eq!(
            program.blocks[0].declarations[0].items[0].initializer,
            Some(Expr::dummy(ExprKind::StringLiteral("hello".into())))
        );
    }

    #[test]
    fn parses_text_declaration_with_notext() {
        let program = parse_source("begin text t := notext; end;");
        assert_eq!(
            program.blocks[0].declarations[0].items[0].initializer,
            Some(Expr::dummy(ExprKind::Notext))
        );
    }

    #[test]
    fn parses_ref_declaration() {
        let program = parse_source("begin ref(Node) r; end;");
        assert_eq!(
            program.blocks[0].declarations[0].ty,
            Type::ObjectRef("Node".into())
        );
    }

    #[test]
    fn parses_comma_separated_declaration_names() {
        let program = parse_source("begin integer i, j, k; end;");
        let items = &program.blocks[0].declarations[0].items;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "i");
        assert_eq!(items[1].name, "j");
        assert_eq!(items[2].name, "k");
    }

    #[test]
    fn parses_declarations_before_statements() {
        let program = parse_source(r#"begin integer i; OutText("ok"); end;"#);
        assert_eq!(program.blocks[0].declarations.len(), 1);
        assert_eq!(program.blocks[0].statements.len(), 1);
    }

    fn parse_source_result(source: &str) -> Result<Program, CompileError> {
        let stream = tokenize(&SourceFile::anonymous(source)).expect("test input should tokenize");
        parse(&stream)
    }

    #[test]
    fn rejects_short_without_integer() {
        let error = parse_source_result("begin short x; end;").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("integer") || message.contains("short"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn keyword_parser_matches_begin() {
        let tokens = vec![Token::kind_only(TokenKind::Keyword(Keyword::Begin))];
        assert_eq!(
            tokens::keyword(Keyword::Begin).parse(&tokens).into_result(),
            Ok(Keyword::Begin)
        );
    }
}
