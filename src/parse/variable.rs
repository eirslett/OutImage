//! Variable parsers (§4.1 destinations).

use chumsky::prelude::*;

use super::tokens::{ParseExtra, kind, name_identifier, subscript_delimited};
use crate::ast::{Expr, ExprKind, Variable};
use crate::lex::{Token, TokenKind};

use super::expr::prefix as expr_parser;

/// Fold `base.attr1.attr2...` into nested [`ExprKind::RemoteAccess`] nodes.
///
/// Each fold widens the span to cover through the attribute name; since we
/// don't have the attribute tokens' own spans here, the outer span is reused
/// (still non-empty and correctly ordered, just not maximally tight).
pub(in crate::parse) fn fold_remote_expr_chain(mut object: Expr, attributes: &[String]) -> Expr {
    for attribute in attributes {
        let span = object.span.clone();
        object = Expr::new(
            ExprKind::RemoteAccess {
                object: Box::new(object),
                attribute: attribute.clone(),
            },
            span,
        );
    }
    object
}

/// Fold `base.attr1.attr2...` into nested [`Variable::Remote`] nodes.
pub(in crate::parse) fn fold_remote_chain(base: String, attributes: &[String]) -> Variable {
    let mut variable = Variable::Simple(base);
    for attribute in attributes {
        variable = Variable::Remote {
            object: Box::new(variable),
            attribute: attribute.clone(),
        };
    }
    variable
}

/// One or more `.identifier` suffixes.
pub(in crate::parse) fn remote_attribute_chain<'a>()
-> impl Parser<'a, &'a [Token], Vec<String>, ParseExtra<'a>> + Clone {
    kind(TokenKind::Dot)
        .ignore_then(name_identifier())
        .repeated()
        .at_least(1)
        .collect()
}

pub fn parser<'a>() -> impl Parser<'a, &'a [Token], Variable, ParseExtra<'a>> + Clone {
    name_identifier()
        .then(suffix())
        .map(|(name, suffix)| match suffix {
            VarSuffix::Plain => Variable::Simple(name),
            VarSuffix::Remote(attributes) => fold_remote_chain(name, &attributes),
            VarSuffix::Subscripted(subscripts) => Variable::Subscripted { name, subscripts },
            VarSuffix::SubscriptedRemote {
                subscripts,
                attributes,
            } => fold_remote_chain_on_subscripted(name, subscripts, &attributes),
            VarSuffix::SubscriptedRemoteCall {
                subscripts,
                attribute,
                arguments,
            } => Variable::RemoteCall {
                object: Box::new(Variable::Subscripted { name, subscripts }),
                attribute,
                arguments,
            },
        })
}

fn fold_remote_chain_on_subscripted(
    name: String,
    subscripts: Vec<Expr>,
    attributes: &[String],
) -> Variable {
    let mut variable = Variable::Subscripted { name, subscripts };
    for attribute in attributes {
        variable = Variable::Remote {
            object: Box::new(variable),
            attribute: attribute.clone(),
        };
    }
    variable
}

enum VarSuffix {
    Plain,
    Remote(Vec<String>),
    Subscripted(Vec<crate::ast::Expr>),
    SubscriptedRemote {
        subscripts: Vec<crate::ast::Expr>,
        attributes: Vec<String>,
    },
    SubscriptedRemoteCall {
        subscripts: Vec<crate::ast::Expr>,
        attribute: String,
        arguments: Vec<crate::ast::Expr>,
    },
}

fn suffix<'a>() -> impl Parser<'a, &'a [Token], VarSuffix, ParseExtra<'a>> + Clone {
    let expr = expr_parser();
    let arg_list = expr
        .clone()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>();
    choice((
        kind(TokenKind::LeftParen)
            .ignore_then(arg_list.clone())
            .then_ignore(kind(TokenKind::RightParen))
            .then(
                remote_attribute_chain()
                    .then(
                        kind(TokenKind::LeftParen)
                            .ignore_then(arg_list.clone())
                            .then_ignore(kind(TokenKind::RightParen))
                            .or_not(),
                    )
                    .map(|(attributes, call)| {
                        let _attribute = attributes.last().expect("remote chain").clone();
                        (attributes, call)
                    })
                    .or_not(),
            )
            .map(|(subscripts, remote)| match remote {
                Some((attributes, Some(arguments))) => VarSuffix::SubscriptedRemoteCall {
                    subscripts,
                    attribute: attributes.last().expect("remote chain").clone(),
                    arguments,
                },
                Some((attributes, None)) => VarSuffix::SubscriptedRemote {
                    subscripts,
                    attributes,
                },
                None => VarSuffix::Subscripted(subscripts),
            }),
        remote_attribute_chain().map(VarSuffix::Remote),
        subscript_delimited(arg_list).map(VarSuffix::Subscripted),
        empty().map(|_| VarSuffix::Plain),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::tokenize;
    use crate::parse::parse;
    use crate::source::SourceFile;

    fn parse_remote_from_rhs(source: &str) -> Variable {
        let stream = tokenize(&SourceFile::anonymous(source)).expect("tokenize");
        let program = parse(&stream).expect("parse");
        let crate::ast::StatementKind::Assignment(assignment) =
            &program.blocks[0].statements[0].kind
        else {
            panic!("expected assignment");
        };
        let crate::ast::AssignmentRhs::Expr(expr) = &assignment.rhs else {
            panic!("expected variable expression rhs");
        };
        let crate::ast::ExprKind::Variable(variable) = &expr.kind else {
            panic!("expected variable expression rhs");
        };
        variable.clone()
    }

    #[test]
    fn parses_single_level_remote_variable() {
        assert_eq!(
            parse_remote_from_rhs("begin x := a.b; end;"),
            Variable::Remote {
                object: Box::new(Variable::Simple("a".into())),
                attribute: "b".into(),
            }
        );
    }

    #[test]
    fn parses_multi_level_remote_variable() {
        assert_eq!(
            parse_remote_from_rhs("begin x := r.last.next; end;"),
            Variable::Remote {
                object: Box::new(Variable::Remote {
                    object: Box::new(Variable::Simple("r".into())),
                    attribute: "last".into(),
                }),
                attribute: "next".into(),
            }
        );
    }

    #[test]
    fn parses_subscripted_remote_reference_lhs() {
        let stream =
            tokenize(&SourceFile::anonymous("begin ra1(z).t := \"XXX\"; end;")).expect("tokenize");
        let program = parse(&stream).expect("parse");
        let crate::ast::StatementKind::Assignment(assignment) =
            &program.blocks[0].statements[0].kind
        else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment.lhs,
            Variable::Remote {
                object: Box::new(Variable::Subscripted {
                    name: "ra1".into(),
                    subscripts: vec![Expr::dummy(ExprKind::Variable(Variable::Simple(
                        "z".into()
                    )))],
                }),
                attribute: "t".into(),
            }
        );
    }

    #[test]
    fn parses_multi_level_remote_reference_lhs() {
        let stream =
            tokenize(&SourceFile::anonymous("begin r.last.next :- none; end;")).expect("tokenize");
        let program = parse(&stream).expect("parse");
        let crate::ast::StatementKind::Assignment(assignment) =
            &program.blocks[0].statements[0].kind
        else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment.lhs,
            Variable::Remote {
                object: Box::new(Variable::Remote {
                    object: Box::new(Variable::Simple("r".into())),
                    attribute: "last".into(),
                }),
                attribute: "next".into(),
            }
        );
    }
}
