//! Shared helpers for parse unit tests.
//!
//! Prefer these over ad-hoc `tokenize` + `parse` wrappers so combinator-level
//! tests stay cheap and consistent across modules.

use chumsky::Parser;
use chumsky::error::Rich;

use crate::ast::Program;
use crate::lex::{Token, TokenStream, tokenize};
use crate::source::SourceFile;

use super::parse;
use super::tokens::{ParseExtra, emit_prefix};

pub fn tokens(source: &str) -> TokenStream {
    tokenize(&SourceFile::anonymous(source)).expect("tokenize")
}

pub fn parse_program(source: &str) -> Program {
    parse(&tokens(source)).expect("parse program")
}

pub fn parse_program_result(source: &str) -> Result<Program, crate::error::CompileError> {
    parse(&tokens(source))
}

/// Tokenize `source` and parse with `parser`, returning `(output, consumed)`.
#[macro_export]
macro_rules! parse_prefix {
    ($source:expr, $parser:expr $(,)?) => {{
        let __stream = $crate::parse::test_support::tokens($source);
        $crate::parse::test_support::parse_prefix_slice(__stream.as_slice(), $parser)
    }};
}

/// Assert that parsing `source` with `parser` fails at the prefix layer.
#[macro_export]
macro_rules! assert_prefix_err {
    ($source:expr, $parser:expr $(,)?) => {{
        let __stream = $crate::parse::test_support::tokens($source);
        let __err = $crate::parse::tokens::emit_prefix(__stream.as_slice(), 0, $parser);
        assert!(
            __err.is_err(),
            "expected prefix parse to fail for {:?}",
            $source
        );
    }};
}

/// Tokenize `source` and run a whole-slice combinator parse.
#[macro_export]
macro_rules! parse_combinator_source {
    ($source:expr, $parser:expr $(,)?) => {{
        let __stream = $crate::parse::test_support::tokens($source);
        $crate::parse::test_support::parse_combinator(__stream.as_slice(), $parser).unwrap_or_else(
            |errors| panic!("combinator parse failed for {:?}: {errors:?}", $source),
        )
    }};
}

/// Assert that a whole-slice combinator parse of `source` fails.
#[macro_export]
macro_rules! assert_combinator_source_err {
    ($source:expr, $parser:expr $(,)?) => {{
        let __stream = $crate::parse::test_support::tokens($source);
        $crate::parse::test_support::assert_combinator_err(__stream.as_slice(), $parser);
    }};
}

pub(crate) use assert_combinator_source_err;
pub(crate) use parse_combinator_source;
pub(crate) use parse_prefix;

pub fn parse_prefix_slice<'a, O>(
    tokens: &'a [Token],
    parser: impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
) -> (O, usize) {
    emit_prefix(tokens, 0, parser).expect("parse prefix")
}

pub fn parse_prefix_range<'a, O>(
    tokens: &'a [Token],
    range: std::ops::Range<usize>,
    parser: impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
) -> (O, usize) {
    emit_prefix(&tokens[range], 0, parser).expect("parse prefix")
}

pub fn parse_combinator<'a, O>(
    tokens: &'a [Token],
    parser: impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
) -> Result<O, Vec<Rich<'a, Token>>> {
    let (output, errors) = parser.parse(tokens).into_output_errors();
    match output {
        Some(value) if errors.is_empty() => Ok(value),
        _ => Err(errors),
    }
}

pub fn assert_combinator_err<'a, O>(
    tokens: &'a [Token],
    parser: impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
) {
    assert!(
        parse_combinator(tokens, parser).is_err(),
        "expected combinator parse to fail"
    );
}
