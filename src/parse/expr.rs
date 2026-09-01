//! Precedence-based expression parser (Simula Standard Chapter 3).

use chumsky::prelude::*;

use super::variable::{fold_remote_chain, fold_remote_expr_chain, remote_attribute_chain};
use super::{
    ParseExtra, identifier, keyword, keyword_not_followed_by, kind, name_identifier, span_with,
    subscript_delimited,
};
use crate::ast::{BinaryOp, DesignationalExpr, Expr, ExprKind, RelationOp, UnaryOp, Variable};
use crate::lex::{Keyword, NumberKind, Token, TokenKind};
use crate::types::ArithmeticLiteralKind;

pub fn arithmetic_literal_kind(kind: NumberKind) -> ArithmeticLiteralKind {
    match kind {
        NumberKind::Integer => ArithmeticLiteralKind::Integer,
        NumberKind::Real => ArithmeticLiteralKind::Real,
        NumberKind::LongReal => ArithmeticLiteralKind::LongReal,
    }
}

fn number_literal<'a>(
    sign: Option<char>,
) -> impl Parser<'a, &'a [Token], Expr, ParseExtra<'a>> + Clone {
    select! {
        Token { kind: TokenKind::NumberLiteral { kind, lexeme }, .. } => {
            (kind, lexeme)
        },
    }
    .map_with(move |(kind, lexeme), extra| {
        let lexeme = match sign {
            Some(sign) => format!("{sign}{lexeme}"),
            None => lexeme,
        };
        Expr::new(
            ExprKind::NumberLiteral {
                lexeme,
                kind: arithmetic_literal_kind(kind),
            },
            span_with(extra),
        )
    })
}

fn relation_op<'a>() -> impl Parser<'a, &'a [Token], RelationOp, ParseExtra<'a>> + Clone {
    choice((
        kind(TokenKind::Lt).map(|_| RelationOp::Lt),
        kind(TokenKind::Le).map(|_| RelationOp::Le),
        kind(TokenKind::Eq).map(|_| RelationOp::Eq),
        kind(TokenKind::Ge).map(|_| RelationOp::Ge),
        kind(TokenKind::Gt).map(|_| RelationOp::Gt),
        kind(TokenKind::Ne).map(|_| RelationOp::Ne),
        kind(TokenKind::RefEq).map(|_| RelationOp::RefEq),
        kind(TokenKind::RefNe).map(|_| RelationOp::RefNe),
        keyword(Keyword::Is).map(|_| RelationOp::Is),
        keyword(Keyword::In).map(|_| RelationOp::In),
    ))
}

fn argument_list<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], Vec<Expr>, ParseExtra<'a>> + Clone {
    expr.separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
}

/// Merge the spans of an expression's first and last token into a single span.
fn span_from_to(start: &Expr, end: &Expr) -> crate::error::Span {
    start.span.start..end.span.end
}

fn apply_qua(expr: Expr, class_name: Option<String>) -> Expr {
    match class_name {
        Some(class_name) => {
            let span = expr.span.clone();
            Expr::new(
                ExprKind::Qua {
                    object: Box::new(expr),
                    class_name,
                },
                span,
            )
        }
        None => expr,
    }
}

enum PostfixSuffix {
    Qua(String),
    Remote {
        attributes: Vec<String>,
        call: Option<Vec<Expr>>,
    },
}

fn apply_remote_to_expr(object: Expr, attributes: &[String], call: Option<Vec<Expr>>) -> Expr {
    if let Some(arguments) = call {
        let attribute = attributes.last().expect("remote call").clone();
        let object = fold_remote_expr_chain(object, &attributes[..attributes.len() - 1]);
        let span = object.span.start
            ..arguments
                .last()
                .map(|a| a.span.end)
                .unwrap_or(object.span.end);
        return Expr::new(
            ExprKind::RemoteCall {
                object: Box::new(object),
                attribute,
                arguments,
            },
            span,
        );
    }

    fold_remote_expr_chain(object, attributes)
}

fn apply_postfix_suffix(expr: Expr, suffix: PostfixSuffix) -> Expr {
    match suffix {
        PostfixSuffix::Qua(class_name) => apply_qua(expr, Some(class_name)),
        PostfixSuffix::Remote { attributes, call } => apply_remote_to_expr(expr, &attributes, call),
    }
}

fn postfix_suffixes<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], PostfixSuffix, ParseExtra<'a>> + Clone {
    let args = argument_list(expr);
    choice((
        keyword(Keyword::Qua)
            .ignore_then(identifier())
            .map(PostfixSuffix::Qua),
        remote_attribute_chain()
            .then(
                kind(TokenKind::LeftParen)
                    .ignore_then(args)
                    .then_ignore(kind(TokenKind::RightParen))
                    .or_not(),
            )
            .map(|(attributes, call)| PostfixSuffix::Remote { attributes, call }),
    ))
}

fn with_postfixes<'a, P>(
    base: P,
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>
where
    P: Parser<'a, &'a [Token], Expr, ParseExtra<'a>> + Clone + 'a,
{
    base.then(custom(
        move |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let mut suffixes = Vec::new();
            loop {
                let checkpoint = inp.save();
                match postfix_suffixes(expr.clone()).go_emit(inp) {
                    Ok(suffix) => suffixes.push(suffix),
                    Err(_) => {
                        inp.rewind(checkpoint);
                        break;
                    }
                }
            }
            Ok(suffixes)
        },
    ))
    .map(|(base, suffixes)| suffixes.into_iter().fold(base, apply_postfix_suffix))
    .boxed()
}

fn new_primary<'a>(
    if_boolean: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    with_postfixes(
        keyword(Keyword::New)
            .ignore_then(identifier())
            .then(
                kind(TokenKind::LeftParen)
                    .ignore_then(argument_list(if_boolean.clone()))
                    .then_ignore(kind(TokenKind::RightParen))
                    .or_not(),
            )
            .map_with(|(class_name, arguments), extra| {
                Expr::new(
                    ExprKind::New {
                        class_name,
                        arguments,
                    },
                    span_with(extra),
                )
            }),
        if_boolean,
    )
}

pub(in crate::parse) fn new_with_postfixes<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    new_primary(expr)
}

pub(in crate::parse) fn this_with_postfixes<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    with_postfixes(
        keyword(Keyword::This)
            .ignore_then(identifier())
            .map_with(|name, extra| Expr::new(ExprKind::This(name), span_with(extra))),
        expr,
    )
}

pub(in crate::parse) fn parenthesized_with_postfixes<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    with_postfixes(
        expr.clone()
            .delimited_by(kind(TokenKind::LeftParen), kind(TokenKind::RightParen))
            .map_with(|inner, extra| Expr::new(ExprKind::Paren(Box::new(inner)), span_with(extra))),
        expr,
    )
}

enum NameSuffix {
    Call(Vec<Expr>),
    Remote {
        attributes: Vec<String>,
        call: Option<Vec<Expr>>,
    },
    Subscripted(Vec<Expr>),
    Plain,
}

fn identifier_base<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], Expr, ParseExtra<'a>> + Clone {
    let args = argument_list(expr);

    name_identifier()
        .then(choice((
            kind(TokenKind::LeftParen)
                .ignore_then(args.clone())
                .then_ignore(kind(TokenKind::RightParen))
                .map(NameSuffix::Call),
            remote_attribute_chain()
                .then(
                    kind(TokenKind::LeftParen)
                        .ignore_then(args.clone())
                        .then_ignore(kind(TokenKind::RightParen))
                        .or_not(),
                )
                .map(|(attributes, call)| NameSuffix::Remote { attributes, call }),
            subscript_delimited(args).map(NameSuffix::Subscripted),
            empty().map(|_| NameSuffix::Plain),
        )))
        .then(keyword(Keyword::Qua).ignore_then(identifier()).or_not())
        .map_with(|((name, suffix), qua), extra| {
            let span = span_with(extra);
            let expr = match suffix {
                NameSuffix::Call(arguments) => {
                    Expr::new(ExprKind::FunctionCall { name, arguments }, span.clone())
                }
                NameSuffix::Remote {
                    attributes,
                    call: Some(arguments),
                } => {
                    let attribute = attributes.last().expect("remote chain").clone();
                    let object = fold_remote_chain(name, &attributes[..attributes.len() - 1]);
                    Expr::new(
                        ExprKind::RemoteCall {
                            object: Box::new(Expr::new(ExprKind::Variable(object), span.clone())),
                            attribute,
                            arguments,
                        },
                        span.clone(),
                    )
                }
                NameSuffix::Remote {
                    attributes,
                    call: None,
                } => Expr::new(
                    ExprKind::Variable(fold_remote_chain(name, &attributes)),
                    span.clone(),
                ),
                NameSuffix::Subscripted(subscripts) => Expr::new(
                    ExprKind::Variable(Variable::Subscripted { name, subscripts }),
                    span.clone(),
                ),
                NameSuffix::Plain => {
                    Expr::new(ExprKind::Variable(Variable::Simple(name)), span.clone())
                }
            };
            apply_qua(expr, qua)
        })
}

pub(in crate::parse) fn identifier_remote_expression<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    with_postfixes(
        name_identifier().map_with(|name, extra| {
            Expr::new(ExprKind::Variable(Variable::Simple(name)), span_with(extra))
        }),
        expr,
    )
}

fn identifier_primary<'a>(
    expr: Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    with_postfixes(identifier_base(expr.clone()), expr)
}

enum DesignationalSuffix {
    Switch(Expr),
    Label,
}

pub fn parser<'a>() -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    recursive(|if_boolean| {
        let if_boolean = if_boolean.boxed();

        let primary = {
            let parenthesized = parenthesized_with_postfixes(if_boolean.clone());

            choice((
                parenthesized,
                select! {
                    Token { kind: TokenKind::StringLiteral(value), .. } => {
                        ExprKind::StringLiteral(value)
                    },
                    Token { kind: TokenKind::CharacterLiteral(value), .. } => {
                        ExprKind::CharacterLiteral(value)
                    },
                }
                .map_with(|kind, extra| Expr::new(kind, span_with(extra))),
                keyword(Keyword::True).map_with(|_, extra| {
                    Expr::new(ExprKind::BooleanLiteral(true), span_with(extra))
                }),
                keyword(Keyword::False).map_with(|_, extra| {
                    Expr::new(ExprKind::BooleanLiteral(false), span_with(extra))
                }),
                keyword(Keyword::Notext)
                    .map_with(|_, extra| Expr::new(ExprKind::Notext, span_with(extra))),
                keyword(Keyword::None)
                    .map_with(|_, extra| Expr::new(ExprKind::None, span_with(extra))),
                new_primary(if_boolean.clone()),
                with_postfixes(
                    keyword(Keyword::This)
                        .ignore_then(identifier())
                        .map_with(|name, extra| Expr::new(ExprKind::This(name), span_with(extra))),
                    if_boolean.clone(),
                ),
                number_literal(None),
                identifier_primary(if_boolean.clone()),
            ))
            .boxed()
        };

        let unary_arithmetic = recursive(|unary| {
            let unary = unary.boxed();
            choice((
                kind(TokenKind::Plus).ignore_then(choice((
                    number_literal(Some('+')),
                    unary.clone().map_with(|operand, extra| {
                        Expr::new(
                            ExprKind::Unary {
                                op: UnaryOp::Plus,
                                operand: Box::new(operand),
                            },
                            span_with(extra),
                        )
                    }),
                ))),
                kind(TokenKind::Minus).ignore_then(choice((
                    number_literal(Some('-')),
                    unary.map_with(|operand, extra| {
                        Expr::new(
                            ExprKind::Unary {
                                op: UnaryOp::Minus,
                                operand: Box::new(operand),
                            },
                            span_with(extra),
                        )
                    }),
                ))),
                primary.clone(),
            ))
            .boxed()
        });

        let power = unary_arithmetic
            .clone()
            .foldl(
                kind(TokenKind::StarStar).then(unary_arithmetic).repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::Pow,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let multiplicative = power
            .clone()
            .foldl(
                choice((
                    kind(TokenKind::Star).map(|_| BinaryOp::Mul),
                    kind(TokenKind::Slash).map(|_| BinaryOp::Div),
                    kind(TokenKind::SlashSlash).map(|_| BinaryOp::IntDiv),
                ))
                .then(power)
                .repeated(),
                |left, (op, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let additive = multiplicative
            .clone()
            .foldl(
                choice((
                    kind(TokenKind::Plus).map(|_| BinaryOp::Add),
                    kind(TokenKind::Minus).map(|_| BinaryOp::Sub),
                ))
                .then(multiplicative)
                .repeated(),
                |left, (op, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let concat = additive
            .clone()
            .foldl(
                kind(TokenKind::Ampersand).then(additive).repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::TextConcat,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let relation = concat
            .clone()
            .then(relation_op().then(concat).or_not())
            .map(|(left, relation)| match relation {
                Some((op, right)) => {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Relation {
                            op,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                }
                None => left,
            })
            .boxed();

        let not_expr = keyword(Keyword::Not)
            .repeated()
            .foldr_with(relation.clone(), |_, operand, extra| {
                Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        operand: Box::new(operand),
                    },
                    span_with(extra),
                )
            })
            .boxed();

        let and_expr = not_expr
            .clone()
            .foldl(
                keyword_not_followed_by(Keyword::And, Keyword::Then)
                    .then(not_expr)
                    .repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::And,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let or_expr = and_expr
            .clone()
            .foldl(
                keyword_not_followed_by(Keyword::Or, Keyword::Else)
                    .then(and_expr)
                    .repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::Or,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let imp = or_expr
            .clone()
            .foldl(
                keyword(Keyword::Imp).then(or_expr).repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::Imp,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let eqv = imp
            .clone()
            .foldl(
                keyword(Keyword::Eqv).then(imp).repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::Eqv,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let and_then = eqv
            .clone()
            .foldl(
                keyword(Keyword::And)
                    .ignore_then(keyword(Keyword::Then))
                    .then(eqv)
                    .repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::AndThen,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        let or_else = and_then
            .clone()
            .foldl(
                keyword(Keyword::Or)
                    .ignore_then(keyword(Keyword::Else))
                    .then(and_then)
                    .repeated(),
                |left, (_, right)| {
                    let span = span_from_to(&left, &right);
                    Expr::new(
                        ExprKind::Binary {
                            op: BinaryOp::OrElse,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    )
                },
            )
            .boxed();

        keyword(Keyword::If)
            .ignore_then(if_boolean.clone())
            .then_ignore(keyword(Keyword::Then))
            .then(if_boolean.clone())
            .then_ignore(keyword(Keyword::Else))
            .then(if_boolean)
            .map_with(|((condition, then_expr), else_expr), extra| {
                Expr::new(
                    ExprKind::If {
                        condition: Box::new(condition),
                        then_expr: Box::new(then_expr),
                        else_expr: Box::new(else_expr),
                    },
                    span_with(extra),
                )
            })
            .or(or_else)
            .boxed()
    })
    .boxed()
}

pub fn prefix<'a>() -> Boxed<'a, 'a, &'a [Token], Expr, ParseExtra<'a>> {
    super::tokens::prefix_parser(parser())
}

pub fn designational_parser<'a>() -> Boxed<'a, 'a, &'a [Token], DesignationalExpr, ParseExtra<'a>> {
    recursive(|if_designational| {
        let if_designational = if_designational.boxed();
        let expr = prefix();
        let simple = choice((
            if_designational
                .clone()
                .delimited_by(kind(TokenKind::LeftParen), kind(TokenKind::RightParen))
                .map(|inner| DesignationalExpr::Paren(Box::new(inner))),
            identifier()
                .then(choice((
                    subscript_delimited(expr.clone()).map(DesignationalSuffix::Switch),
                    empty().map(|_| DesignationalSuffix::Label),
                )))
                .map(|(name, suffix)| match suffix {
                    DesignationalSuffix::Switch(subscript) => DesignationalExpr::SwitchDesignator {
                        name,
                        subscript: Box::new(subscript),
                    },
                    DesignationalSuffix::Label => DesignationalExpr::Label(name),
                }),
        ))
        .boxed();

        keyword(Keyword::If)
            .ignore_then(expr.clone())
            .then_ignore(keyword(Keyword::Then))
            .then(simple.clone().map(Box::new))
            .then_ignore(keyword(Keyword::Else))
            .then(if_designational)
            .map(
                |((condition, then_expr), else_expr)| DesignationalExpr::If {
                    condition: Box::new(condition),
                    then_expr,
                    else_expr: Box::new(else_expr),
                },
            )
            .or(simple)
            .boxed()
    })
    .boxed()
}

pub fn designational_prefix<'a>() -> Boxed<'a, 'a, &'a [Token], DesignationalExpr, ParseExtra<'a>> {
    super::tokens::prefix_parser(designational_parser())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Assignment, StatementKind};
    use crate::parse::test_support::parse_prefix;

    fn parse_source(source: &str) -> crate::ast::Program {
        crate::parse::test_support::parse_program(source)
    }

    fn parse_expr(source: &str) -> Expr {
        parse_prefix!(source, parser()).0
    }

    #[test]
    fn expression_precedence_table() {
        let cases: &[(&str, fn(&Expr) -> bool, &str)] = &[
            (
                "1 + 2 * 3",
                |expr| {
                    matches!(
                        &expr.kind,
                        ExprKind::Binary {
                            op: BinaryOp::Add,
                            right,
                            ..
                        } if matches!(right.kind, ExprKind::Binary { op: BinaryOp::Mul, .. })
                    )
                },
                "addition should bind less tightly than multiplication",
            ),
            (
                "2 * 3 + 4",
                |expr| {
                    matches!(
                        &expr.kind,
                        ExprKind::Binary {
                            op: BinaryOp::Add,
                            left,
                            ..
                        } if matches!(left.kind, ExprKind::Binary { op: BinaryOp::Mul, .. })
                    )
                },
                "multiplication should bind to the left operand of addition",
            ),
            (
                "not a and b or c",
                |expr| {
                    matches!(
                        &expr.kind,
                        ExprKind::Binary {
                            op: BinaryOp::Or,
                            ..
                        }
                    )
                },
                "or should be the weakest boolean operator in this chain",
            ),
            (
                "a ** b ** c",
                |expr| {
                    matches!(
                        &expr.kind,
                        ExprKind::Binary {
                            op: BinaryOp::Pow,
                            left,
                            ..
                        } if matches!(left.kind, ExprKind::Binary { op: BinaryOp::Pow, .. })
                    )
                },
                "exponentiation should associate left-to-left in this grammar",
            ),
            (
                "\"a\" & \"b\" & \"c\"",
                |expr| {
                    matches!(
                        &expr.kind,
                        ExprKind::Binary {
                            op: BinaryOp::TextConcat,
                            ..
                        }
                    )
                },
                "text concatenation should fold as a binary chain",
            ),
            (
                "if a then b else c",
                |expr| matches!(&expr.kind, ExprKind::If { .. }),
                "conditional expression should parse as ExprKind::If",
            ),
        ];

        for (source, check, message) in cases {
            assert!(check(&parse_expr(source)), "{message}: {source}");
        }
    }

    #[test]
    fn expression_parser_consumes_prefix_only() {
        let (_, consumed) = parse_prefix!("1 + 2 ;", parser());
        assert_eq!(consumed, 3, "should stop before the trailing semicolon");
    }

    #[test]
    fn conditional_expression_with_postfix_parsed_directly() {
        assert!(matches!(
            parse_expr("( if a then b else c ).max_x + 1").kind,
            ExprKind::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn chained_postfix_skips_directive_between_calls() {
        let expr = parse_expr("new TextItemWindow(x).SetMaxChars(1)\n% .SetMaxChars(6)\n.Show");
        assert!(matches!(
            &expr.kind,
            ExprKind::RemoteAccess {
                attribute,
                object,
                ..
            } if attribute == "Show"
                && matches!(
                    &object.kind,
                    ExprKind::RemoteCall {
                        attribute: inner,
                        ..
                    } if inner == "SetMaxChars"
                )
        ));
    }

    #[test]
    fn parses_arithmetic_precedence() {
        let program = parse_source("begin x := 1 + 2 * 3; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment.rhs.as_expr().map(|e| &e.kind),
            Some(ExprKind::Binary {
                op: BinaryOp::Add,
                ..
            })
        ));
    }

    fn assignment_rhs(assignment: &Assignment) -> &Expr {
        assignment.rhs.as_expr().expect("expected expression rhs")
    }

    #[test]
    fn parses_exponentiation_left_associative_in_chain() {
        let program = parse_source("begin x := 2 ** 3 ** 4; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        let ExprKind::Binary {
            op: BinaryOp::Pow,
            left,
            ..
        } = &assignment_rhs(assignment).kind
        else {
            panic!("expected power");
        };
        assert!(matches!(
            left.kind,
            ExprKind::Binary {
                op: BinaryOp::Pow,
                ..
            }
        ));
    }

    #[test]
    fn parses_boolean_operators() {
        let program = parse_source("begin b := not a and b or c; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::Binary {
                op: BinaryOp::Or,
                ..
            }
        ));
    }

    #[test]
    fn parses_and_then_and_or_else() {
        let program = parse_source("begin b := a and then b or else c; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::Binary {
                op: BinaryOp::OrElse,
                ..
            }
        ));
    }

    #[test]
    fn parses_relational_expression() {
        let program = parse_source("begin b := a + 1 < b; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::Relation {
                op: RelationOp::Lt,
                ..
            }
        ));
    }

    #[test]
    fn parses_text_concatenation() {
        let program = parse_source(r#"begin t := "a" & "b" & "c"; end;"#);
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::Binary {
                op: BinaryOp::TextConcat,
                ..
            }
        ));
    }

    #[test]
    fn parses_conditional_expression() {
        let program = parse_source("begin x := if a < b then 1 else 2; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::If { .. }
        ));
    }

    #[test]
    fn parses_chained_call_with_directive_between() {
        parse_source(
            "begin answer_wnd :- new TextItemWindow(this PromptWindow).SetMaxChars(max(1, max_length))\n% .SetMaxChars(6)\n.Show; end;",
        );
    }

    #[test]
    fn parses_conditional_expression_with_postfix_access() {
        let program = parse_source(
            "begin newwidth := ( if hide_button =/= none then hide_button else answer_window ).max_x + 1; end;",
        );
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn parses_object_expressions() {
        let program = parse_source("begin r :- none; r :- new Node; r :- p qua File; end;");
        assert_eq!(program.blocks[0].statements.len(), 3);
        let StatementKind::Assignment(a0) = &program.blocks[0].statements[0].kind else {
            panic!();
        };
        assert!(matches!(assignment_rhs(a0).kind, ExprKind::None));
        let StatementKind::Assignment(a1) = &program.blocks[0].statements[1].kind else {
            panic!();
        };
        assert!(matches!(assignment_rhs(a1).kind, ExprKind::New { .. }));
        let StatementKind::Assignment(a2) = &program.blocks[0].statements[2].kind else {
            panic!();
        };
        assert!(matches!(assignment_rhs(a2).kind, ExprKind::Qua { .. }));
    }

    #[test]
    fn parses_parenthesized_expression() {
        let program = parse_source("begin x := (1 + 2) * 3; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        let ExprKind::Binary {
            op: BinaryOp::Mul,
            left,
            ..
        } = &assignment_rhs(assignment).kind
        else {
            panic!("expected mul");
        };
        assert!(matches!(left.kind, ExprKind::Paren(_)));
    }

    #[test]
    fn parses_integer_division() {
        let program = parse_source("begin x := 7 // 3; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            assignment_rhs(assignment).kind,
            ExprKind::Binary {
                op: BinaryOp::IntDiv,
                ..
            }
        ));
    }

    #[test]
    fn parses_unary_minus_on_expression() {
        let program = parse_source("begin x := -a + 1; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        let ExprKind::Binary { left, .. } = &assignment_rhs(assignment).kind else {
            panic!("expected add");
        };
        assert!(matches!(
            left.kind,
            ExprKind::Unary {
                op: UnaryOp::Minus,
                ..
            }
        ));
    }

    #[test]
    fn parses_this_qua_remote_attribute() {
        let program = parse_source("begin x := this dist qua randint.a; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment_rhs(assignment).kind,
            ExprKind::RemoteAccess {
                object: Box::new(Expr::dummy(ExprKind::Qua {
                    object: Box::new(Expr::dummy(ExprKind::This("dist".into()))),
                    class_name: "randint".into(),
                })),
                attribute: "a".into(),
            }
        );
    }

    #[test]
    fn parses_variable_qua_remote_attribute() {
        let program = parse_source("begin x := q qua histogram.lower; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment_rhs(assignment).kind,
            ExprKind::RemoteAccess {
                object: Box::new(Expr::dummy(ExprKind::Qua {
                    object: Box::new(Expr::dummy(ExprKind::Variable(Variable::Simple(
                        "q".into()
                    )))),
                    class_name: "histogram".into(),
                })),
                attribute: "lower".into(),
            }
        );
    }

    #[test]
    fn parses_this_qua_remote_procedure_reference() {
        let program = parse_source("begin x := this fifo_h qua fifo.takefirst; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment_rhs(assignment).kind,
            ExprKind::RemoteAccess {
                object: Box::new(Expr::dummy(ExprKind::Qua {
                    object: Box::new(Expr::dummy(ExprKind::This("fifo_h".into()))),
                    class_name: "fifo".into(),
                })),
                attribute: "takefirst".into(),
            }
        );
    }

    #[test]
    fn parses_variable_qua_remote_procedure_call() {
        let program = parse_source("begin x := xf qua Infile.Open(\"f\"); end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment_rhs(assignment).kind,
            ExprKind::RemoteCall {
                object: Box::new(Expr::dummy(ExprKind::Qua {
                    object: Box::new(Expr::dummy(ExprKind::Variable(Variable::Simple(
                        "xf".into()
                    )))),
                    class_name: "Infile".into(),
                })),
                attribute: "Open".into(),
                arguments: vec![Expr::dummy(ExprKind::StringLiteral("f".into()))],
            }
        );
    }

    #[test]
    fn parses_remote_access_qua_remote_attribute_in_expression() {
        parse_source("begin k := k * 10 + chain.pred qua Bead.i; end;");
    }

    #[test]
    fn parses_remote_qua_class_in_reference_assignment() {
        parse_source("begin r :- x qua Bead; end;");
    }

    #[test]
    fn parses_remote_variable_qua_remote_attribute() {
        let program = parse_source("begin x := chain.first qua bead.i; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment_rhs(assignment).kind,
            ExprKind::RemoteAccess {
                object: Box::new(Expr::dummy(ExprKind::Qua {
                    object: Box::new(Expr::dummy(ExprKind::Variable(Variable::Remote {
                        object: Box::new(Variable::Simple("chain".into())),
                        attribute: "first".into(),
                    }))),
                    class_name: "bead".into(),
                })),
                attribute: "i".into(),
            }
        );
    }
}
