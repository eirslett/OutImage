//! Bridge chumsky combinators to cursor-based `Cursor` methods.

use chumsky::input::InputRef;
use chumsky::prelude::*;

use super::tokens::ParseExtra;
use super::tokens::rich_err;
use super::{Cursor, stash_parse_error};
use crate::error::CompileError;
use crate::lex::Token;

pub fn cursor_bridge<'a, O, F>(parse: F) -> Boxed<'a, 'a, &'a [Token], O, ParseExtra<'a>>
where
    O: 'a,
    F: Fn(&mut Cursor<'_>) -> Result<O, CompileError> + Clone + 'a,
{
    custom(
        move |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let start = inp.cursor();
            let tokens = inp.slice_from(&start..);
            let mut parser = Cursor { tokens, index: 0 };
            match parse(&mut parser) {
                Ok(value) => {
                    // Advance by reference — `skip()` goes through ValueInput and
                    // clones each Token (owned strings) once per consumed token.
                    for _ in 0..parser.index {
                        let _ = inp.next_ref();
                    }
                    Ok(value)
                }
                Err(error) => {
                    stash_parse_error(error);
                    Err(rich_err(None, inp.span_since(&start)))
                }
            }
        },
    )
    .boxed()
}

pub fn validated_parser<'a, I, O, P, F>(
    parser: P,
    validate: F,
) -> impl Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a
where
    I: 'a,
    O: 'a,
    P: Parser<'a, &'a [Token], I, ParseExtra<'a>> + Clone + 'a,
    F: Fn(I) -> Result<O, CompileError> + Clone + 'a,
{
    parser.try_map(move |value, span| {
        validate(value).map_err(|error| {
            stash_parse_error(error);
            rich_err(None, span)
        })
    })
}

pub fn guarded_parser<'a, O, C, P>(
    check: C,
    parser: P,
) -> Boxed<'a, 'a, &'a [Token], O, ParseExtra<'a>>
where
    O: 'a,
    C: Fn(&[Token]) -> bool + Clone + 'a,
    P: Parser<'a, &'a [Token], O, ParseExtra<'a>> + Clone + 'a,
{
    custom(
        move |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let start = inp.cursor();
            let remaining = inp.slice_from(&start..);
            if !check(remaining) {
                return Err(rich_err(None, inp.span_since(&start)));
            }
            let checkpoint = inp.save();
            match parser.go_emit(inp) {
                Ok(value) => Ok(value),
                Err(()) => {
                    inp.rewind(checkpoint);
                    Err(rich_err(None, inp.span_since(&start)))
                }
            }
        },
    )
    .boxed()
}
