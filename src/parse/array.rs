//! Array declaration parsers (Simula Standard §5.2).

use chumsky::prelude::*;

use super::tokens::{
    ParseExtra, identifier, keyword, kind, semicolon, span_with, subscript_delimited,
};
use super::type_::parser as type_parser;
use crate::ast::{ArrayDeclaration, ArraySegment, BoundPair};
use crate::lex::{Keyword, Token, TokenKind};
use crate::types::Type;

use super::expr::prefix as expr_parser;

pub fn parser<'a>() -> impl Parser<'a, &'a [Token], ArrayDeclaration, ParseExtra<'a>> + Clone {
    choice((
        type_parser()
            .then_ignore(keyword(Keyword::Array))
            .then(array_segments())
            .then_ignore(semicolon())
            .map_with(|(element_type, segments), extra| ArrayDeclaration {
                element_type,
                segments,
                span: span_with(extra),
            }),
        keyword(Keyword::Array)
            .ignore_then(array_segments())
            .then_ignore(semicolon())
            .map_with(|segments, extra| ArrayDeclaration {
                element_type: Type::Real { long: false },
                segments,
                span: span_with(extra),
            }),
    ))
}

fn array_segments<'a>() -> impl Parser<'a, &'a [Token], Vec<ArraySegment>, ParseExtra<'a>> + Clone {
    array_segment()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .at_least(1)
        .collect()
}

fn array_segment<'a>() -> impl Parser<'a, &'a [Token], ArraySegment, ParseExtra<'a>> + Clone {
    array_names()
        .then(bound_pair_list())
        .map(|(names, bounds)| ArraySegment { names, bounds })
}

fn array_names<'a>() -> impl Parser<'a, &'a [Token], Vec<String>, ParseExtra<'a>> + Clone {
    identifier()
        .separated_by(kind(TokenKind::Comma))
        .allow_trailing()
        .at_least(1)
        .collect()
}

fn bound_pair_list<'a>() -> impl Parser<'a, &'a [Token], Vec<BoundPair>, ParseExtra<'a>> + Clone {
    subscript_delimited(
        bound_pair()
            .separated_by(kind(TokenKind::Comma))
            .allow_trailing()
            .collect(),
    )
}

fn bound_pair<'a>() -> impl Parser<'a, &'a [Token], BoundPair, ParseExtra<'a>> + Clone {
    let expr = expr_parser();
    expr.clone()
        .then_ignore(kind(TokenKind::Colon))
        .then(expr)
        .map(|(lower, upper)| BoundPair { lower, upper })
}
#[cfg(test)]
mod tests {
    use crate::ast::ArrayDeclaration;
    use crate::ast::ExprKind;
    use crate::parse::test_support::{parse_prefix, parse_program};
    use crate::types::Type;

    use super::parser;

    fn parse_source(source: &str) -> crate::ast::Program {
        parse_program(source)
    }

    fn parse_array(source: &str) -> ArrayDeclaration {
        parse_prefix!(source, parser()).0
    }

    #[test]
    fn array_parser_matches_typed_declaration_directly() {
        let decl = parse_array("integer array a(1:10);");
        assert_eq!(decl.element_type, Type::Integer { short: false });
        assert_eq!(decl.segments[0].names, ["a"]);
    }

    #[test]
    fn array_parser_consumes_prefix_only() {
        let (_, consumed) = parse_prefix!("integer array a(1:10); next", parser());
        assert!(consumed >= 8);
        assert!(consumed < 12, "must stop before identifier next");
    }

    #[test]
    fn parses_typed_one_dimensional_array() {
        let program = parse_source("begin integer array a(1:10); end;");
        let arrays = &program.blocks[0].arrays;
        assert_eq!(arrays.len(), 1);
        assert_eq!(arrays[0].element_type, Type::Integer { short: false });
        assert_eq!(arrays[0].segments.len(), 1);
        assert_eq!(arrays[0].segments[0].names, vec!["a"]);
        assert_eq!(arrays[0].segments[0].bounds.len(), 1);
    }

    #[test]
    fn parses_untyped_array_defaulting_to_real() {
        let program = parse_source("begin array a(0:5); end;");
        assert_eq!(
            program.blocks[0].arrays[0].element_type,
            Type::Real { long: false }
        );
    }

    #[test]
    fn parses_multiple_names_in_one_segment() {
        let program = parse_source("begin integer array a, b(1:10); end;");
        assert_eq!(
            program.blocks[0].arrays[0].segments[0].names,
            vec!["a", "b"]
        );
    }

    #[test]
    fn parses_multiple_segments() {
        let program = parse_source("begin integer array a(1:5), b(1:5); end;");
        assert_eq!(program.blocks[0].arrays[0].segments.len(), 2);
        assert_eq!(program.blocks[0].arrays[0].segments[0].names, vec!["a"]);
        assert_eq!(program.blocks[0].arrays[0].segments[1].names, vec!["b"]);
    }

    #[test]
    fn parses_multi_dimensional_bounds() {
        let program = parse_source("begin integer array m(1:10, 2:20); end;");
        assert_eq!(program.blocks[0].arrays[0].segments[0].bounds.len(), 2);
    }

    #[test]
    fn parses_square_bracket_array_bounds() {
        let program = parse_source("begin integer array table[0 : ncells + 1]; end;");
        let arrays = &program.blocks[0].arrays;
        assert_eq!(arrays.len(), 1);
        assert_eq!(arrays[0].segments[0].names, vec!["table"]);
        assert_eq!(arrays[0].segments[0].bounds.len(), 1);
        assert!(matches!(
            arrays[0].segments[0].bounds[0].upper.kind,
            ExprKind::Binary { .. }
        ));
    }

    #[test]
    fn parses_square_bracket_bounds_with_multiple_names() {
        let program = parse_source("begin real array x, p[1 : size]; end;");
        assert_eq!(
            program.blocks[0].arrays[0].segments[0].names,
            vec!["x", "p"]
        );
        assert_eq!(program.blocks[0].arrays[0].segments[0].bounds.len(), 1);
    }

    #[test]
    fn parses_bound_expressions() {
        let program = parse_source("begin integer n; integer array a(1:n + 1); end;");
        let upper = &program.blocks[0].arrays[0].segments[0].bounds[0].upper;
        assert!(matches!(upper.kind, ExprKind::Binary { .. }));
    }
}
