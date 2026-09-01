//! Statement parsers (Simula Standard §4.1–§4.6).

use chumsky::input::InputRef;
use chumsky::prelude::*;
use chumsky::util::MaybeRef;

use super::assignment::{reference_rhs_parser, value_rhs_parser};
use super::tokens::{
    ParseExtra, assign_operator, identifier, keyword, kind, name_identifier, optional_semicolon,
    rich_err, span_with, subscript_delimited,
};
use super::variable::{fold_remote_chain, fold_remote_expr_chain, remote_attribute_chain};
use crate::ast::{
    ActivateStatement, AssignOperator, Assignment, Block, Expr, ExprKind, ForListElement,
    ForStatement, GotoStatement, IfStatement, InspectStatement, ObjectGenerator, ProcedureCall,
    ReactivateStatement, SimulationTiming, Statement, StatementKind, Variable, WhenClause,
    WhileStatement,
};
use crate::error::Span;
use crate::lex::{Keyword, Token, TokenKind};

use super::expr::{
    designational_prefix, identifier_remote_expression, new_with_postfixes,
    parenthesized_with_postfixes, prefix as expr_parser, this_with_postfixes,
};

pub fn labeled_parser<'a>(
    statement: impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone + 'a,
) -> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    let statement = statement.boxed();
    recursive(|_labeled| {
        choice((
            kind(TokenKind::Semicolon)
                .map_with(|_, extra| Statement::new(StatementKind::Dummy, span_with(extra))),
            prefix_labels()
                .then(choice((
                    trailing_dummy_marker().map_with(|_, extra| {
                        Statement::new(StatementKind::Dummy, span_with(extra))
                    }),
                    statement.clone(),
                )))
                .map_with(|(labels, inner), extra| fold_labels(labels, inner, span_with(extra))),
            statement.clone(),
        ))
    })
    .boxed()
}

pub fn statement_choice<'a>(
    block: impl Parser<'a, &'a [Token], Block, ParseExtra<'a>> + Clone + 'a,
) -> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    let block = block.boxed();

    recursive(|statement| {
        let statement = statement.boxed();
        let labeled_statement = || labeled_parser(statement.clone());

        let loop_body = labeled_parser(
            choice((
                for_statement_with_body(labeled_statement()),
                statement.clone(),
            ))
            .boxed(),
        );

        choice((
            if_statement(loop_body.clone()).labelled("an `if` statement"),
            while_statement(loop_body.clone()).labelled("a `while` statement"),
            for_statement_with_body(loop_body).labelled("a `for` statement"),
            goto_statement().labelled("a `goto` statement"),
            block
                .clone()
                .map_with(|block, extra| {
                    Statement::new(StatementKind::Compound(block), span_with(extra))
                })
                .labelled("a block"),
            object_generator_statement().labelled("an object generator"),
            inspect_statement(labeled_statement()).labelled("an `inspect` statement"),
            keyword(Keyword::Inner).map_with(|_, extra| {
                Statement::new(StatementKind::Inner { label: None }, span_with(extra))
            }),
            activate_statement().labelled("an `activate` statement"),
            reactivate_statement().labelled("a `reactivate` statement"),
            this_expression_statement(),
            parenthesized_expression_statement(),
            identifier_statement(),
        ))
        .boxed()
    })
    .boxed()
}

fn prefix_labels<'a>() -> impl Parser<'a, &'a [Token], Vec<String>, ParseExtra<'a>> + Clone {
    identifier()
        .then_ignore(kind(TokenKind::Colon))
        .repeated()
        .at_least(1)
        .collect()
}

fn trailing_dummy_marker<'a>() -> impl Parser<'a, &'a [Token], (), ParseExtra<'a>> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        if inp.peek().as_ref().is_none() {
            return Ok(());
        }
        let checkpoint = inp.save();
        let before = inp.cursor();
        let Some(token) = inp.next() else {
            return Ok(());
        };
        match token.kind {
            TokenKind::Semicolon => Ok(()),
            TokenKind::Keyword(Keyword::End) => {
                inp.rewind(checkpoint);
                Ok(())
            }
            _ => {
                inp.rewind(checkpoint);
                Err(rich_err(
                    Some(MaybeRef::Val(token)),
                    inp.span_since(&before),
                ))
            }
        }
    })
}

fn fold_labels(labels: Vec<String>, inner: Statement, span: Span) -> Statement {
    labels.into_iter().rev().fold(inner, |statement, label| {
        Statement::new(
            StatementKind::Labeled {
                label,
                statement: Box::new(statement),
            },
            span.clone(),
        )
    })
}

fn if_statement<'a>(
    body: Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    keyword(Keyword::If)
        .ignore_then(expr_parser())
        .then_ignore(keyword(Keyword::Then))
        .then(if_branch(body.clone()))
        .then(keyword(Keyword::Else).ignore_then(if_branch(body)).or_not())
        .then(optional_semicolon())
        .map_with(|(((condition, then_branch), else_branch), _), extra| {
            Statement::new(
                StatementKind::If(IfStatement {
                    condition,
                    then_branch: Box::new(then_branch),
                    else_branch: else_branch.map(Box::new),
                }),
                span_with(extra),
            )
        })
}

fn if_branch<'a>(
    body: Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    choice((body.clone(), empty_if_branch())).boxed()
}

fn empty_if_branch<'a>() -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        let before = inp.cursor();
        let peeked = inp.peek();
        let Some(token) = peeked.as_ref() else {
            return Ok(Statement::dummy(StatementKind::Dummy));
        };
        if matches!(
            token.kind,
            TokenKind::Keyword(Keyword::Else | Keyword::End) | TokenKind::Semicolon
        ) {
            return Ok(Statement::dummy(StatementKind::Dummy));
        }
        Err(rich_err(
            Some(MaybeRef::Val(token.clone())),
            inp.span_since(&before),
        ))
    })
}

fn while_statement<'a>(
    body: Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    keyword(Keyword::While)
        .ignore_then(expr_parser())
        .then_ignore(keyword(Keyword::Do))
        .then(body)
        .then(optional_semicolon())
        .map_with(|((condition, body), _), extra| {
            Statement::new(
                StatementKind::While(WhileStatement {
                    condition,
                    body: Box::new(body),
                }),
                span_with(extra),
            )
        })
}

fn for_statement_with_body<'a>(
    body: Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>>,
) -> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    keyword(Keyword::For)
        .ignore_then(identifier())
        .then(for_list())
        .then_ignore(keyword(Keyword::Do))
        .then(body)
        .then(optional_semicolon())
        .map_with(|(((variable, elements), body), _), extra| {
            Statement::new(
                StatementKind::For(ForStatement {
                    variable,
                    elements,
                    body: Box::new(body),
                }),
                span_with(extra),
            )
        })
        .boxed()
}

fn for_list<'a>() -> impl Parser<'a, &'a [Token], Vec<ForListElement>, ParseExtra<'a>> + Clone {
    choice((
        assign_operator()
            .filter(|op| *op == AssignOperator::Assign)
            .labelled("`:=`")
            .ignore_then(value_for_list().labelled("a `for` value list")),
        assign_operator()
            .filter(|op| *op == AssignOperator::AssignAlt)
            .labelled("`:-`")
            .ignore_then(reference_for_list().labelled("a `for` reference list")),
    ))
}

fn value_for_list<'a>() -> impl Parser<'a, &'a [Token], Vec<ForListElement>, ParseExtra<'a>> + Clone
{
    value_for_element()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .at_least(1)
        .collect()
}

fn reference_for_list<'a>()
-> impl Parser<'a, &'a [Token], Vec<ForListElement>, ParseExtra<'a>> + Clone {
    reference_for_element()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .at_least(1)
        .collect()
}

fn value_for_element<'a>() -> impl Parser<'a, &'a [Token], ForListElement, ParseExtra<'a>> + Clone {
    choice((
        expr_parser()
            .then_ignore(keyword(Keyword::Step).labelled("`step`"))
            .then(expr_parser())
            .then_ignore(keyword(Keyword::Until).labelled("`until`"))
            .then(expr_parser())
            .map(|((start, step), until)| ForListElement::StepUntil { start, step, until }),
        expr_parser()
            .then(while_condition().or_not())
            .map(|(expr, while_cond)| ForListElement::Value { expr, while_cond }),
    ))
}

fn reference_for_element<'a>()
-> impl Parser<'a, &'a [Token], ForListElement, ParseExtra<'a>> + Clone {
    expr_parser()
        .then(while_condition().or_not())
        .map(|(expr, while_cond)| ForListElement::Reference { expr, while_cond })
}

fn while_condition<'a>() -> impl Parser<'a, &'a [Token], Expr, ParseExtra<'a>> + Clone {
    keyword(Keyword::While).ignore_then(expr_parser())
}

fn goto_statement<'a>() -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    choice((
        keyword(Keyword::Goto).ignore_then(designational_prefix()),
        keyword(Keyword::Go)
            .ignore_then(keyword(Keyword::To))
            .ignore_then(designational_prefix()),
    ))
    .then_ignore(optional_statement_terminator())
    .map_with(|target, extra| {
        Statement::new(
            StatementKind::Goto(GotoStatement { target }),
            span_with(extra),
        )
    })
}

fn simulation_timing<'a>() -> impl Parser<'a, &'a [Token], SimulationTiming, ParseExtra<'a>> + Clone
{
    choice((
        keyword(Keyword::At)
            .ignore_then(expr_parser())
            .map(SimulationTiming::At),
        keyword(Keyword::Delay)
            .ignore_then(expr_parser())
            .map(SimulationTiming::Delay),
        keyword(Keyword::After)
            .ignore_then(expr_parser())
            .map(SimulationTiming::After),
        keyword(Keyword::Before)
            .ignore_then(expr_parser())
            .map(SimulationTiming::Before),
    ))
}

fn activate_statement<'a>() -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    keyword(Keyword::Activate)
        .ignore_then(expr_parser())
        .then(
            simulation_timing()
                .then(keyword(Keyword::Prior).or_not())
                .map(|(timing, prior)| (Some(timing), prior.is_some()))
                .or_not(),
        )
        .then_ignore(optional_statement_terminator())
        .map_with(|(target, schedule), extra| {
            let (timing, prior) = schedule.unwrap_or((None, false));
            Statement::new(
                StatementKind::Activate(ActivateStatement {
                    target,
                    timing,
                    prior,
                }),
                span_with(extra),
            )
        })
}

fn reactivate_statement<'a>() -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    keyword(Keyword::Reactivate)
        .ignore_then(expr_parser())
        .then(simulation_timing().or_not())
        .then_ignore(optional_statement_terminator())
        .map_with(|(target, timing), extra| {
            Statement::new(
                StatementKind::Reactivate(ReactivateStatement { target, timing }),
                span_with(extra),
            )
        })
}

fn this_expression_statement<'a>() -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone
{
    this_with_postfixes(expr_parser().boxed())
        .then_ignore(optional_statement_terminator())
        .map_with(|expr, extra| Statement::new(StatementKind::Expr(expr), span_with(extra)))
}

fn parenthesized_expression_statement<'a>()
-> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    parenthesized_with_postfixes(expr_parser().boxed())
        .then_ignore(optional_statement_terminator())
        .map_with(|expr, extra| Statement::new(StatementKind::Expr(expr), span_with(extra)))
}

fn object_generator_statement<'a>()
-> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    new_with_postfixes(expr_parser().boxed())
        .then_ignore(optional_semicolon())
        .map_with(|expr, extra| {
            let span = span_with(extra);
            match expr.kind {
                ExprKind::New {
                    class_name,
                    arguments,
                } => Statement::new(
                    StatementKind::ObjectGenerator(ObjectGenerator {
                        class_name,
                        arguments: arguments.unwrap_or_default(),
                    }),
                    span,
                ),
                kind => Statement::new(StatementKind::Expr(Expr::new(kind, expr.span)), span),
            }
        })
}

fn inspect_statement<'a>(
    body: Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    keyword(Keyword::Inspect)
        .ignore_then(expr_parser())
        .then(when_clauses(body.clone()))
        .then(keyword(Keyword::Do).ignore_then(body.clone()).or_not())
        .then(keyword(Keyword::Otherwise).ignore_then(body).or_not())
        .then(optional_semicolon())
        .map_with(
            |((((object, when_clauses), do_clause), otherwise), _), extra| {
                Statement::new(
                    StatementKind::Inspect(InspectStatement {
                        object,
                        when_clauses,
                        do_clause: do_clause.map(Box::new),
                        otherwise: otherwise.map(Box::new),
                    }),
                    span_with(extra),
                )
            },
        )
}

fn when_clauses<'a>(
    stmt: Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>>,
) -> impl Parser<'a, &'a [Token], Vec<WhenClause>, ParseExtra<'a>> + Clone {
    custom(
        move |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let mut clauses = Vec::new();
            loop {
                let checkpoint = inp.save();
                match keyword(Keyword::When)
                    .ignore_then(identifier())
                    .then_ignore(keyword(Keyword::Do))
                    .then(choice((stmt.clone(), empty_when_body())))
                    .map(|(class_name, body)| WhenClause {
                        class_name,
                        body: Box::new(body),
                    })
                    .go_emit(inp)
                {
                    Ok(clause) => clauses.push(clause),
                    Err(_) => {
                        inp.rewind(checkpoint);
                        break;
                    }
                }
            }
            Ok(clauses)
        },
    )
}

fn empty_when_body<'a>() -> impl Parser<'a, &'a [Token], Statement, ParseExtra<'a>> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        let before = inp.cursor();
        let peeked = inp.peek();
        let Some(token) = peeked.as_ref() else {
            return Ok(Statement::dummy(StatementKind::Dummy));
        };
        if matches!(
            token.kind,
            TokenKind::Keyword(Keyword::When | Keyword::Otherwise | Keyword::End | Keyword::Else)
                | TokenKind::Semicolon
        ) {
            return Ok(Statement::dummy(StatementKind::Dummy));
        }
        Err(rich_err(
            Some(MaybeRef::Val(token.clone())),
            inp.span_since(&before),
        ))
    })
}

fn identifier_statement<'a>() -> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    choice((
        qualified_remote_expression_statement(),
        simple_identifier_statement(),
    ))
    .boxed()
}

fn qualified_remote_expression_statement<'a>()
-> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    remote_chain_statement_start()
        .ignore_then(
            identifier_remote_expression(expr_parser().boxed())
                .filter(|expr| {
                    matches!(
                        expr.kind,
                        ExprKind::RemoteCall { .. } | ExprKind::RemoteAccess { .. }
                    )
                })
                .labelled("a remote attribute or call")
                .map_with(|expr, extra| {
                    Statement::new(StatementKind::Expr(expr), span_with(extra))
                }),
        )
        .then_ignore(optional_statement_terminator())
        .boxed()
}

fn remote_chain_statement_start<'a>() -> impl Parser<'a, &'a [Token], (), ParseExtra<'a>> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        let checkpoint = inp.save();
        let before = inp.cursor();
        let Some(token) = inp.next() else {
            return Err(rich_err(None, inp.span_since(&before)));
        };
        if !matches!(token.kind, TokenKind::Identifier(_)) {
            inp.rewind(checkpoint);
            return Err(rich_err(
                Some(MaybeRef::Val(token)),
                inp.span_since(&before),
            ));
        }
        let Some(next) = inp.peek() else {
            inp.rewind(checkpoint);
            return Err(rich_err(None, inp.span_since(&before)));
        };
        if !matches!(next.kind, TokenKind::Dot) {
            inp.rewind(checkpoint);
            return Err(rich_err(
                Some(MaybeRef::Val(next.clone())),
                inp.span_since(&before),
            ));
        }
        inp.rewind(checkpoint);
        Ok(())
    })
}

fn simple_identifier_statement<'a>() -> Boxed<'a, 'a, &'a [Token], Statement, ParseExtra<'a>> {
    name_identifier()
        .then(
            keyword(Keyword::Qua)
                .ignore_then(name_identifier())
                .or_not(),
        )
        .then(identifier_suffix())
        .then_ignore(optional_statement_terminator())
        .map_with(|((name, qua), suffix), extra| suffix.finish(name, qua, span_with(extra)))
        .boxed()
}

enum IdentifierSuffix {
    RemoteCall {
        attributes: Vec<String>,
        arguments: Vec<Expr>,
        chained: Vec<(String, Vec<Expr>)>,
    },
    RemoteReference {
        attributes: Vec<String>,
    },
    RemoteAssign {
        attributes: Vec<String>,
        operator: AssignOperator,
        rhs: crate::ast::AssignmentRhs,
    },
    RemoteCallAssign {
        attributes: Vec<String>,
        arguments: Vec<Expr>,
        operator: AssignOperator,
        rhs: crate::ast::AssignmentRhs,
    },
    SubscriptedAssign {
        subscripts: Vec<Expr>,
        operator: AssignOperator,
        rhs: crate::ast::AssignmentRhs,
    },
    SubscriptedRemoteCall {
        subscripts: Vec<Expr>,
        attribute: String,
        arguments: Vec<Expr>,
        chained: Vec<(String, Vec<Expr>)>,
    },
    SubscriptedRemoteAssign {
        subscripts: Vec<Expr>,
        attribute: String,
        operator: AssignOperator,
        rhs: crate::ast::AssignmentRhs,
    },
    SubscriptedRemoteCallAssign {
        subscripts: Vec<Expr>,
        attribute: String,
        arguments: Vec<Expr>,
        operator: AssignOperator,
        rhs: crate::ast::AssignmentRhs,
    },
    SubscriptedRemoteReference {
        subscripts: Vec<Expr>,
        attribute: String,
    },
    ProcedureCall {
        subscripts: Vec<Expr>,
    },
    Assign {
        operator: AssignOperator,
        rhs: crate::ast::AssignmentRhs,
    },
    BareCall,
}

impl IdentifierSuffix {
    /// Build the final `Statement`, using `span` (the whole matched token
    /// range) both for the statement itself and for any expression nodes
    /// synthesized here that don't have a narrower span available.
    fn finish(self, name: String, qua: Option<String>, span: Span) -> Statement {
        let has_qua = qua.is_some();
        let base_expr = || {
            Expr::new(
                ExprKind::Variable(Variable::Simple(name.clone())),
                span.clone(),
            )
        };
        let with_qua = |expr: Expr| -> Expr {
            match &qua {
                Some(class_name) => {
                    let span = expr.span.clone();
                    Expr::new(
                        ExprKind::Qua {
                            object: Box::new(expr),
                            class_name: class_name.clone(),
                        },
                        span,
                    )
                }
                None => expr,
            }
        };
        let with_qua_variable = |variable: Variable| -> Variable {
            match &qua {
                Some(class_name) => Variable::Qua {
                    object: Box::new(variable),
                    class_name: class_name.clone(),
                },
                None => variable,
            }
        };
        let remote_object = |attributes: &[String]| -> Expr {
            if has_qua {
                fold_remote_expr_chain(with_qua(base_expr()), &attributes[..attributes.len() - 1])
            } else {
                Expr::new(
                    ExprKind::Variable(fold_remote_chain(
                        name.clone(),
                        &attributes[..attributes.len() - 1],
                    )),
                    span.clone(),
                )
            }
        };

        match self {
            Self::RemoteCall {
                attributes,
                arguments,
                chained,
            } => {
                let attribute = attributes.last().expect("remote chain").clone();
                let mut expr = Expr::new(
                    ExprKind::RemoteCall {
                        object: Box::new(remote_object(&attributes)),
                        attribute,
                        arguments,
                    },
                    span.clone(),
                );
                for (attribute, arguments) in chained {
                    expr = Expr::new(
                        ExprKind::RemoteCall {
                            object: Box::new(expr),
                            attribute,
                            arguments,
                        },
                        span.clone(),
                    );
                }
                Statement::new(StatementKind::Expr(expr), span)
            }
            Self::RemoteReference { attributes } => {
                let attribute = attributes.last().expect("remote chain").clone();
                Statement::new(
                    StatementKind::Expr(Expr::new(
                        ExprKind::RemoteAccess {
                            object: Box::new(remote_object(&attributes)),
                            attribute,
                        },
                        span.clone(),
                    )),
                    span,
                )
            }
            Self::RemoteAssign {
                attributes,
                operator,
                rhs,
            } => {
                let attribute = attributes.last().expect("remote chain").clone();
                let object = if has_qua {
                    let mut object = with_qua_variable(Variable::Simple(name));
                    for attr in &attributes[..attributes.len() - 1] {
                        object = Variable::Remote {
                            object: Box::new(object),
                            attribute: attr.clone(),
                        };
                    }
                    object
                } else if attributes.len() > 1 {
                    fold_remote_chain(name, &attributes[..attributes.len() - 1])
                } else {
                    Variable::Simple(name)
                };
                Statement::new(
                    StatementKind::Assignment(Assignment {
                        lhs: Variable::Remote {
                            object: Box::new(object),
                            attribute,
                        },
                        operator,
                        rhs,
                    }),
                    span,
                )
            }
            Self::RemoteCallAssign {
                attributes,
                arguments,
                operator,
                rhs,
            } => {
                let attribute = attributes.last().expect("remote chain").clone();
                Statement::new(
                    StatementKind::Assignment(Assignment {
                        lhs: Variable::RemoteCall {
                            object: Box::new(if attributes.len() > 1 {
                                fold_remote_chain(name, &attributes[..attributes.len() - 1])
                            } else {
                                Variable::Simple(name)
                            }),
                            attribute,
                            arguments,
                        },
                        operator,
                        rhs,
                    }),
                    span,
                )
            }
            Self::SubscriptedAssign {
                subscripts,
                operator,
                rhs,
            } => Statement::new(
                StatementKind::Assignment(Assignment {
                    lhs: Variable::Subscripted { name, subscripts },
                    operator,
                    rhs,
                }),
                span,
            ),
            Self::SubscriptedRemoteCall {
                subscripts,
                attribute,
                arguments,
                chained,
            } => {
                let mut expr = Expr::new(
                    ExprKind::RemoteCall {
                        object: Box::new(Expr::new(
                            ExprKind::Variable(Variable::Subscripted { name, subscripts }),
                            span.clone(),
                        )),
                        attribute,
                        arguments,
                    },
                    span.clone(),
                );
                for (attribute, arguments) in chained {
                    expr = Expr::new(
                        ExprKind::RemoteCall {
                            object: Box::new(expr),
                            attribute,
                            arguments,
                        },
                        span.clone(),
                    );
                }
                Statement::new(StatementKind::Expr(expr), span)
            }
            Self::SubscriptedRemoteAssign {
                subscripts,
                attribute,
                operator,
                rhs,
            } => Statement::new(
                StatementKind::Assignment(Assignment {
                    lhs: Variable::Remote {
                        object: Box::new(Variable::Subscripted { name, subscripts }),
                        attribute,
                    },
                    operator,
                    rhs,
                }),
                span,
            ),
            Self::SubscriptedRemoteCallAssign {
                subscripts,
                attribute,
                arguments,
                operator,
                rhs,
            } => Statement::new(
                StatementKind::Assignment(Assignment {
                    lhs: Variable::RemoteCall {
                        object: Box::new(Variable::Subscripted { name, subscripts }),
                        attribute,
                        arguments,
                    },
                    operator,
                    rhs,
                }),
                span,
            ),
            Self::SubscriptedRemoteReference {
                subscripts,
                attribute,
            } => Statement::new(
                StatementKind::Expr(Expr::new(
                    ExprKind::RemoteAccess {
                        object: Box::new(Expr::new(
                            ExprKind::Variable(Variable::Subscripted { name, subscripts }),
                            span.clone(),
                        )),
                        attribute,
                    },
                    span.clone(),
                )),
                span,
            ),
            Self::ProcedureCall { subscripts } => Statement::new(
                StatementKind::ProcedureCall(ProcedureCall {
                    name,
                    arguments: subscripts,
                }),
                span,
            ),
            Self::Assign { operator, rhs } => Statement::new(
                StatementKind::Assignment(Assignment {
                    lhs: Variable::Simple(name),
                    operator,
                    rhs,
                }),
                span,
            ),
            Self::BareCall => Statement::new(
                StatementKind::ProcedureCall(ProcedureCall {
                    name,
                    arguments: Vec::new(),
                }),
                span,
            ),
        }
    }
}

fn identifier_suffix<'a>() -> Boxed<'a, 'a, &'a [Token], IdentifierSuffix, ParseExtra<'a>> {
    choice((
        remote_attribute_chain()
            .then(
                kind(TokenKind::LeftParen)
                    .ignore_then(argument_list())
                    .then_ignore(kind(TokenKind::RightParen))
                    .or_not(),
            )
            .then(assign_with_rhs().or_not())
            .then(
                remote_attribute_chain()
                    .then(
                        kind(TokenKind::LeftParen)
                            .ignore_then(argument_list())
                            .then_ignore(kind(TokenKind::RightParen)),
                    )
                    .map(|(attributes, arguments)| {
                        (attributes.last().expect("remote chain").clone(), arguments)
                    })
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(
                |(((attributes, call_args), assign), chained)| match (call_args, assign) {
                    (Some(arguments), Some((operator, rhs))) => {
                        IdentifierSuffix::RemoteCallAssign {
                            attributes,
                            arguments,
                            operator,
                            rhs,
                        }
                    }
                    (Some(arguments), None) => IdentifierSuffix::RemoteCall {
                        attributes,
                        arguments,
                        chained,
                    },
                    (None, Some((operator, rhs))) => IdentifierSuffix::RemoteAssign {
                        attributes,
                        operator,
                        rhs,
                    },
                    (None, None) => IdentifierSuffix::RemoteReference { attributes },
                },
            ),
        kind(TokenKind::LeftParen)
            .ignore_then(argument_list())
            .then_ignore(kind(TokenKind::RightParen))
            .then(
                remote_attribute_chain()
                    .then(
                        kind(TokenKind::LeftParen)
                            .ignore_then(argument_list())
                            .then_ignore(kind(TokenKind::RightParen))
                            .or_not(),
                    )
                    .map(|(attributes, call)| {
                        let attribute = attributes.last().expect("remote chain").clone();
                        (attribute, call)
                    })
                    .or_not(),
            )
            .then(
                remote_attribute_chain()
                    .then(
                        kind(TokenKind::LeftParen)
                            .ignore_then(argument_list())
                            .then_ignore(kind(TokenKind::RightParen)),
                    )
                    .map(|(attributes, arguments)| {
                        (attributes.last().expect("remote chain").clone(), arguments)
                    })
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .then(assign_with_rhs().or_not())
            .map(|(((subscripts, remote), chained), assign)| {
                if let Some((operator, rhs)) = assign {
                    return match remote {
                        Some((attribute, Some(arguments))) => {
                            IdentifierSuffix::SubscriptedRemoteCallAssign {
                                subscripts,
                                attribute,
                                arguments,
                                operator,
                                rhs,
                            }
                        }
                        Some((attribute, None)) => IdentifierSuffix::SubscriptedRemoteAssign {
                            subscripts,
                            attribute,
                            operator,
                            rhs,
                        },
                        None => IdentifierSuffix::SubscriptedAssign {
                            subscripts,
                            operator,
                            rhs,
                        },
                    };
                }
                match remote {
                    Some((attribute, Some(arguments))) => IdentifierSuffix::SubscriptedRemoteCall {
                        subscripts,
                        attribute,
                        arguments,
                        chained,
                    },
                    Some((attribute, None)) => IdentifierSuffix::SubscriptedRemoteReference {
                        subscripts,
                        attribute,
                    },
                    None => IdentifierSuffix::ProcedureCall { subscripts },
                }
            }),
        subscript_delimited(argument_list())
            .then(assign_with_rhs())
            .map(
                |(subscripts, (operator, rhs))| IdentifierSuffix::SubscriptedAssign {
                    subscripts,
                    operator,
                    rhs,
                },
            ),
        assign_with_rhs().map(|(operator, rhs)| IdentifierSuffix::Assign { operator, rhs }),
        empty().map(|_| IdentifierSuffix::BareCall),
    ))
    .boxed()
}

fn assign_with_rhs<'a>()
-> Boxed<'a, 'a, &'a [Token], (AssignOperator, crate::ast::AssignmentRhs), ParseExtra<'a>> {
    choice((
        assign_operator()
            .filter(|op| *op == AssignOperator::Assign)
            .labelled("`:=`")
            .ignore_then(value_rhs_parser())
            .map(|rhs| (AssignOperator::Assign, rhs)),
        assign_operator()
            .filter(|op| *op == AssignOperator::AssignAlt)
            .labelled("`:-`")
            .ignore_then(reference_rhs_parser())
            .map(|rhs| (AssignOperator::AssignAlt, rhs)),
    ))
    .boxed()
}

fn argument_list<'a>() -> Boxed<'a, 'a, &'a [Token], Vec<Expr>, ParseExtra<'a>> {
    expr_parser()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .collect()
        .boxed()
}

fn optional_statement_terminator<'a>() -> impl Parser<'a, &'a [Token], (), ParseExtra<'a>> + Clone {
    custom::<_, &'a [Token], (), ParseExtra<'a>>(
        |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            if inp.peek().as_ref().is_none_or(statement_boundary_follows) {
                return Ok(());
            }
            let before = inp.cursor();
            let Some(token) = inp.next() else {
                return Ok(());
            };
            if token.kind == TokenKind::Semicolon {
                return Ok(());
            }
            Err(rich_err(
                Some(MaybeRef::Val(token)),
                inp.span_since(&before),
            ))
        },
    )
}

fn statement_boundary_follows(token: &Token) -> bool {
    matches!(
        token.kind,
        TokenKind::Keyword(Keyword::Else | Keyword::Otherwise | Keyword::When | Keyword::End)
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::DesignationalExpr;
    use crate::ast::{
        AssignmentRhs, Expr, ExprKind, ForListElement, Statement, StatementKind, Variable,
    };
    use crate::parse::test_support::{parse_prefix, parse_program, parse_program_result};
    use crate::types::ArithmeticLiteralKind;

    fn parse_source(source: &str) -> crate::ast::Program {
        parse_program(source)
    }

    fn parse_statement(source: &str) -> Statement {
        parse_prefix!(source, statement_choice(crate::parse::block::parser())).0
    }

    fn int_literal(value: &str) -> Expr {
        Expr::dummy(ExprKind::NumberLiteral {
            lexeme: value.into(),
            kind: ArithmeticLiteralKind::Integer,
        })
    }

    #[test]
    fn parses_activate_statement() {
        let program = parse_source("begin activate client; end;");
        assert!(matches!(
            program.blocks[0].statements[0].kind,
            StatementKind::Activate(_)
        ));
    }

    #[test]
    fn parses_activate_with_delay() {
        let program = parse_source("begin activate occupier delay 0; end;");
        let StatementKind::Activate(activate) = &program.blocks[0].statements[0].kind else {
            panic!("expected activate");
        };
        assert!(matches!(
            activate.timing,
            Some(crate::ast::SimulationTiming::Delay(_))
        ));
    }

    #[test]
    fn parses_activate_with_after() {
        let program = parse_source("begin activate wait_monitor after nextev; end;");
        let StatementKind::Activate(activate) = &program.blocks[0].statements[0].kind else {
            panic!("expected activate");
        };
        assert!(matches!(
            activate.timing,
            Some(crate::ast::SimulationTiming::After(_))
        ));
    }

    #[test]
    fn parses_activate_this_process() {
        let program = parse_source("begin activate this process; end;");
        let StatementKind::Activate(activate) = &program.blocks[0].statements[0].kind else {
            panic!("expected activate");
        };
        assert!(matches!(activate.target.kind, ExprKind::This(_)));
    }

    #[test]
    fn parses_reactivate_with_at() {
        parse_source("begin reactivate main at time + 20.0; end;");
    }

    #[test]
    fn parses_activate_with_at_prior() {
        let program = parse_source("begin activate pa(i) at time + getime prior; end;");
        let StatementKind::Activate(activate) = &program.blocks[0].statements[0].kind else {
            panic!("expected activate");
        };
        assert!(matches!(activate.timing, Some(SimulationTiming::At(_))));
        assert!(activate.prior);
    }

    #[test]
    fn parses_activate_with_before() {
        parse_source("begin activate pa(i) before pa(i - 1); end;");
    }

    #[test]
    fn parses_activate_with_after_process() {
        parse_source("begin activate pa(i) after pa(i - 1); end;");
    }

    #[test]
    fn parses_reactivate_with_after() {
        let program = parse_source("begin reactivate current after nextev; end;");
        assert!(matches!(
            program.blocks[0].statements[0].kind,
            StatementKind::Reactivate(_)
        ));
    }

    #[test]
    fn parses_this_remote_procedure_statement() {
        let program = parse_source("begin this transaction . out; end;");
        let StatementKind::Expr(expr) = &program.blocks[0].statements[0].kind else {
            panic!("expected remote access statement");
        };
        let ExprKind::RemoteAccess { attribute, .. } = &expr.kind else {
            panic!("expected remote access statement");
        };
        assert_eq!(attribute, "out");
    }

    #[test]
    fn parses_chained_value_assignment() {
        let program = parse_source("begin a := b := c := 1; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(assignment.lhs, Variable::Simple("a".into()));
        assert_eq!(assignment.operator, AssignOperator::Assign);

        let AssignmentRhs::Chain(inner) = &assignment.rhs else {
            panic!("expected chained rhs");
        };
        assert_eq!(inner.lhs, Variable::Simple("b".into()));

        let AssignmentRhs::Chain(inner2) = &inner.rhs else {
            panic!("expected second chain link");
        };
        assert_eq!(inner2.lhs, Variable::Simple("c".into()));
        assert_eq!(inner2.rhs, AssignmentRhs::Expr(int_literal("1")));
    }

    #[test]
    fn parses_reference_assignment() {
        let program = parse_source("begin r :- p; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(assignment.lhs, Variable::Simple("r".into()));
        assert_eq!(assignment.operator, AssignOperator::AssignAlt);
        assert_eq!(
            assignment.rhs,
            AssignmentRhs::Expr(Expr::dummy(ExprKind::Variable(Variable::Simple(
                "p".into()
            ))))
        );
    }

    #[test]
    fn parses_chained_reference_assignment() {
        let program = parse_source("begin a :- b :- none; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(assignment.operator, AssignOperator::AssignAlt);

        let AssignmentRhs::Chain(inner) = &assignment.rhs else {
            panic!("expected chained rhs");
        };
        assert_eq!(inner.lhs, Variable::Simple("b".into()));
        assert_eq!(inner.rhs, AssignmentRhs::Expr(Expr::dummy(ExprKind::None)));
    }

    #[test]
    fn parses_subscripted_chained_assignment() {
        let program = parse_source("begin a(1) := b(2) := 3; end;");
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert_eq!(
            assignment.lhs,
            Variable::Subscripted {
                name: "a".into(),
                subscripts: vec![int_literal("1")],
            }
        );

        let AssignmentRhs::Chain(inner) = &assignment.rhs else {
            panic!("expected chained rhs");
        };
        assert_eq!(
            inner.lhs,
            Variable::Subscripted {
                name: "b".into(),
                subscripts: vec![int_literal("2")],
            }
        );
    }

    #[test]
    fn rejects_mixed_assignment_operators_in_chain() {
        let error = parse_program_result("begin a := b :- 1; end;").unwrap_err();
        assert!(
            error.to_string().contains("expected ':=' operator")
                || error.to_string().contains("expected `:=`")
                || error.to_string().contains("expected ';'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parses_if_condition_with_directive_line_in_chain() {
        let source = "begin\nif 200 // i2 = -1 and\n%   minint//minint  = 1       and\n-1000 // i2 = 9 and i2 // (-11200) = 0\nthen else begin end;\nend;";
        parse_source(source);
    }

    #[test]
    fn parses_simtst82_intdiv_if_chain() {
        parse_source(
            r#"begin
               if 0 // i1 = 0 and
                  i1 // 25 = 1 and
                  i1 // 26 = 0 and
                  i2 // 1 = -111 and
                 -56 // i1 = -2 and
                 s2 // (-7) = -3 and
                  200 // i2 = -1 and
                 -1000 // i2 = 9 and
              i2 // (-11200) = 0
               then else begin end;
               end;"#,
        );
    }

    #[test]
    fn parses_multiline_if_with_intdiv_and_and() {
        parse_source(
            r#"begin
               if 200 // i2 = -1 and i2 // (-11200) = 0 then else begin end;
               end;"#,
        );
    }

    #[test]
    fn parses_subscripted_remote_assignment() {
        parse_source(r#"begin ra1(z).t := "XXX"; end;"#);
    }

    #[test]
    fn parses_subscripted_remote_assignment_chain() {
        parse_source(r#"begin ra1(z).t := rb1(z).t := t1(z) := "XXX"; end;"#);
    }

    #[test]
    fn parses_nested_if_expression_condition() {
        parse_source(
            "begin if if if true then false else true then true else false then x := 1; end;",
        );
    }

    #[test]
    fn parses_if_with_empty_then_branch() {
        let program = parse_source("begin if true then else found_error := true; end;");
        let StatementKind::If(if_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected if");
        };
        assert!(matches!(if_stmt.then_branch.kind, StatementKind::Dummy));
    }

    #[test]
    fn parses_if_with_empty_else_branch() {
        let program = parse_source("begin if false then x := 1 else; end;");
        let StatementKind::If(if_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected if");
        };
        assert!(matches!(
            if_stmt.else_branch.as_deref().map(|s| &s.kind),
            Some(StatementKind::Dummy)
        ));
    }

    #[test]
    fn parses_if_then_statement() {
        let program = parse_source("begin if true then x := 1; end;");
        let StatementKind::If(if_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected if statement");
        };
        assert_eq!(if_stmt.condition.kind, ExprKind::BooleanLiteral(true));
        assert!(if_stmt.else_branch.is_none());
    }

    #[test]
    fn parses_if_then_else_statement() {
        let program = parse_source("begin if x > 0 then n := 1 else n := 0; end;");
        let StatementKind::If(if_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected if statement");
        };
        assert!(if_stmt.else_branch.is_some());
    }

    #[test]
    fn parses_while_statement() {
        let program = parse_source("begin while x > 0 do x := x - 1; end;");
        let StatementKind::While(while_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected while statement");
        };
        assert!(matches!(while_stmt.body.kind, StatementKind::Assignment(_)));
    }

    #[test]
    fn parses_for_value_list() {
        let program = parse_source("begin for i := 1, 2, 3 do x := i; end;");
        let StatementKind::For(for_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected for statement");
        };
        assert_eq!(for_stmt.variable, "i");
        assert_eq!(for_stmt.elements.len(), 3);
    }

    #[test]
    fn parses_for_step_until() {
        let program = parse_source("begin for i := 1 step 2 until 10 do x := i; end;");
        let StatementKind::For(for_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected for statement");
        };
        assert!(matches!(
            for_stmt.elements[0],
            ForListElement::StepUntil { .. }
        ));
    }

    #[test]
    fn parses_for_while_element() {
        let program = parse_source("begin for i := 1 while x > 0 do x := i; end;");
        let StatementKind::For(for_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected for statement");
        };
        let ForListElement::Value {
            while_cond: Some(_),
            ..
        } = &for_stmt.elements[0]
        else {
            panic!("expected value while element");
        };
    }

    #[test]
    fn parses_goto_switch_with_square_bracket_subscript() {
        let program = parse_source("begin goto case[type]; end;");
        let StatementKind::Goto(goto_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected goto");
        };
        assert!(matches!(
            goto_stmt.target,
            DesignationalExpr::SwitchDesignator { .. }
        ));
    }

    #[test]
    fn parses_goto_before_end_without_semicolon() {
        parse_source("begin if true then begin goto LOOP end; end;");
    }

    #[test]
    fn parses_remote_call_assignment() {
        let program =
            parse_source(r#"begin heading.sub(1, 5):= if true then " up" else " down"; end;"#);
        let StatementKind::Assignment(assignment) = &program.blocks[0].statements[0].kind else {
            panic!("expected assignment");
        };
        assert!(matches!(assignment.lhs, Variable::RemoteCall { .. }));
    }

    #[test]
    fn parses_goto_statement() {
        let program = parse_source("begin goto L8; end;");
        let StatementKind::Goto(goto_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected goto statement");
        };
        assert_eq!(goto_stmt.target, DesignationalExpr::Label("L8".into()));
    }

    #[test]
    fn parses_go_to_statement() {
        let program = parse_source("begin go to exit; end;");
        let StatementKind::Goto(goto_stmt) = &program.blocks[0].statements[0].kind else {
            panic!("expected goto statement");
        };
        assert_eq!(goto_stmt.target, DesignationalExpr::Label("exit".into()));
    }

    #[test]
    fn parses_remote_procedure_reference_statement() {
        let program = parse_source("begin outf.outimage; end;");
        assert_eq!(
            program.blocks[0].statements[0].kind,
            StatementKind::Expr(Expr::dummy(ExprKind::RemoteAccess {
                object: Box::new(Expr::dummy(ExprKind::Variable(Variable::Simple(
                    "outf".into()
                )))),
                attribute: "outimage".into(),
            }))
        );
    }

    #[test]
    fn parses_chained_remote_procedure_call_statement() {
        let program = parse_source("begin s.sub(1, 2).putint(3); end;");
        assert_eq!(
            program.blocks[0].statements[0].kind,
            StatementKind::Expr(Expr::dummy(ExprKind::RemoteCall {
                object: Box::new(Expr::dummy(ExprKind::RemoteCall {
                    object: Box::new(Expr::dummy(ExprKind::Variable(Variable::Simple(
                        "s".into()
                    )))),
                    attribute: "sub".into(),
                    arguments: vec![int_literal("1"), int_literal("2")],
                })),
                attribute: "putint".into(),
                arguments: vec![int_literal("3")],
            }))
        );
    }

    #[test]
    fn parses_remote_qua_parameterless_call_statement() {
        parse_source("begin h.first.suc qua townpoint.write; end;");
    }

    #[test]
    fn parses_remote_qua_remote_call_statement() {
        parse_source("begin chain.prev.pred.prev qua link.follow(chain.prev.pred); end;");
    }

    #[test]
    fn parses_qualified_remote_procedure_reference_statement() {
        let program = parse_source("begin q qua queue.list; end;");
        assert_eq!(
            program.blocks[0].statements[0].kind,
            StatementKind::Expr(Expr::dummy(ExprKind::RemoteAccess {
                object: Box::new(Expr::dummy(ExprKind::Qua {
                    object: Box::new(Expr::dummy(ExprKind::Variable(Variable::Simple(
                        "q".into()
                    )))),
                    class_name: "queue".into(),
                })),
                attribute: "list".into(),
            }))
        );
    }

    #[test]
    fn parses_labeled_statement() {
        let program = parse_source("begin abort: x := 1; end;");
        let StatementKind::Labeled { label, .. } = &program.blocks[0].statements[0].kind else {
            panic!("expected labeled statement");
        };
        assert_eq!(label, "abort");
    }

    #[test]
    fn parses_trailing_label_as_labeled_dummy() {
        let program = parse_source("begin DoSomething; John: end;");
        assert_eq!(program.blocks[0].statements.len(), 2);
        let StatementKind::Labeled { label, statement } = &program.blocks[0].statements[1].kind
        else {
            panic!("expected trailing labeled dummy");
        };
        assert_eq!(label, "John");
        assert!(matches!(statement.kind, StatementKind::Dummy));
    }

    #[test]
    fn parses_multiple_trailing_labels_as_labeled_dummy() {
        let program = parse_source("begin DoSomething; A: B: C: end;");
        assert_eq!(program.blocks[0].statements.len(), 2);
        let StatementKind::Labeled { label, statement } = &program.blocks[0].statements[1].kind
        else {
            panic!("expected trailing labeled dummy");
        };
        assert_eq!(label, "A");
        let StatementKind::Labeled { label, statement } = &statement.kind else {
            panic!("expected nested label B");
        };
        assert_eq!(label, "B");
        let StatementKind::Labeled { label, statement } = &statement.kind else {
            panic!("expected nested label C");
        };
        assert_eq!(label, "C");
        assert!(matches!(statement.kind, StatementKind::Dummy));
    }

    #[test]
    fn parses_procedure_named_value_keyword() {
        parse_source("begin class c; begin integer procedure value; value := 1; end; end;");
    }

    #[test]
    fn parses_if_with_directive_before_else() {
        parse_source(
            "begin if ScreenDepth < 4 then SetBlackonWhite\n% SetWhiteonBlack\nelse SetForeground(\"x\"); end;",
        );
    }

    #[test]
    fn if_statement_skips_directive_between_then_and_else_directly() {
        assert!(matches!(
            parse_statement(
                "if ScreenDepth < 4 then SetBlackonWhite\n% SetWhiteonBlack\nelse SetForeground(\"x\")"
            )
            .kind,
            StatementKind::If(_)
        ));
    }

    #[test]
    fn inspect_empty_when_clauses_parsed_directly() {
        assert!(matches!(
            parse_statement("inspect w when HeadWindow do when SubWindow do begin end").kind,
            StatementKind::Inspect(inspect) if inspect.when_clauses.len() == 2
        ));
    }

    #[test]
    fn if_statement_consumes_prefix_only() {
        let (_, consumed) = parse_prefix!(
            "if x then y else z ; next",
            statement_choice(crate::parse::block::parser()),
        );
        assert!(consumed >= 6, "if x then y else z");
        assert!(consumed < 9, "must stop before identifier next");
    }

    #[test]
    fn parses_inspect_with_empty_when_clauses() {
        parse_source(
            "begin procedure find_max(w); ref(element) w; inspect w when HeadWindow do when SubWindow do begin end; end;",
        );
    }

    #[test]
    fn parses_if_with_directives_in_then_branch() {
        parse_source("begin if x then\n% comment\nx := 1; end;");
    }

    #[test]
    fn parses_label_before_directives_and_end() {
        parse_source("begin quit:\n% commented\nend;");
    }

    #[test]
    fn parses_prefix_labels_on_statement() {
        let program = parse_source("begin A: B: C: DoSomething; end;");
        let StatementKind::Labeled { label, statement } = &program.blocks[0].statements[0].kind
        else {
            panic!("expected labeled statement");
        };
        assert_eq!(label, "A");
        let StatementKind::Labeled { label, statement } = &statement.kind else {
            panic!("expected nested label B");
        };
        assert_eq!(label, "B");
        let StatementKind::Labeled { label, statement } = &statement.kind else {
            panic!("expected nested label C");
        };
        assert_eq!(label, "C");
        assert!(matches!(statement.kind, StatementKind::ProcedureCall(_)));
    }

    #[test]
    fn parses_labeled_procedure_call() {
        let program = parse_source("begin fanfare: OutText(\"TADA\"); end;");
        let StatementKind::Labeled { label, statement } = &program.blocks[0].statements[0].kind
        else {
            panic!("expected labeled procedure call");
        };
        assert_eq!(label, "fanfare");
        assert!(matches!(statement.kind, StatementKind::ProcedureCall(_)));
    }
}
