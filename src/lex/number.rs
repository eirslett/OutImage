//! Unsigned number literals per Simula Standard §1.5.

use crate::error::CompileError;
use crate::lex::token::{NumberKind, Token, TokenKind};

use super::Lexer;

impl<'a> Lexer<'a> {
    pub(super) fn read_number_from_digit(&mut self, start: usize) -> Result<Token, CompileError> {
        let mut is_integer_only = true;
        let mut has_double_ampersand = false;

        self.read_unsigned_integer()?;

        if self.peek().is_some_and(|(_, ch)| ch == '.')
            && self.peek_next_char().is_some_and(|ch| ch.is_ascii_digit())
        {
            is_integer_only = false;
            self.advance();
            self.read_unsigned_integer()?;
        }

        if self.peek().is_some_and(|(_, ch)| ch == '&') {
            let before_exponent = self.offset();
            has_double_ampersand = self.read_exponent_part()?;
            if self.offset() > before_exponent {
                is_integer_only = false;
            }
        }

        self.finish_number_token(start, is_integer_only, has_double_ampersand)
    }

    pub(super) fn read_number_from_dot(&mut self, start: usize) -> Result<Token, CompileError> {
        self.advance();

        if !self.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) {
            return Err(crate::diagnostics::invalid_number(
                "expected a digit after `.` in this number",
                start..self.offset(),
            ));
        }

        self.read_unsigned_integer()?;

        let mut has_double_ampersand = false;
        if self.peek().is_some_and(|(_, ch)| ch == '&') {
            let before_exponent = self.offset();
            has_double_ampersand = self.read_exponent_part()?;
            if self.offset() == before_exponent {
                has_double_ampersand = false;
            }
        }

        self.finish_number_token(start, false, has_double_ampersand)
    }

    pub(super) fn try_read_exponent_number(
        &mut self,
        start: usize,
    ) -> Result<Option<Token>, CompileError> {
        if !self.peek().is_some_and(|(_, ch)| ch == '&') {
            return Ok(None);
        }

        let checkpoint = self.offset();
        let has_double_ampersand = self.read_exponent_part()?;

        if self.offset() == checkpoint {
            return Ok(None);
        }

        self.finish_number_token(start, false, has_double_ampersand)
            .map(Some)
    }

    fn finish_number_token(
        &self,
        start: usize,
        is_integer_only: bool,
        has_double_ampersand: bool,
    ) -> Result<Token, CompileError> {
        let end = self.offset();
        let kind = if is_integer_only {
            NumberKind::Integer
        } else if has_double_ampersand {
            NumberKind::LongReal
        } else {
            NumberKind::Real
        };

        Ok(Token::new(
            TokenKind::NumberLiteral {
                kind,
                lexeme: self.source[start..end].to_string(),
            },
            start..end,
        ))
    }

    /// Reads `unsigned-integer` and returns whether it used radix notation.
    fn read_unsigned_integer(&mut self) -> Result<bool, CompileError> {
        let start = self.offset();

        let Some((_, first)) = self.peek() else {
            return Err(crate::diagnostics::invalid_number(
                "expected a digit in this number",
                start..start,
            ));
        };

        if !first.is_ascii_digit() {
            return Err(crate::diagnostics::invalid_number(
                "expected a digit in this number",
                start..start,
            ));
        }

        let mut digits = String::new();
        self.advance();
        digits.push(first);

        while let Some((offset, ch)) = self.peek() {
            if ch.is_ascii_digit() {
                digits.push(self.advance().unwrap().1);
            } else if ch == '_' {
                if digits.is_empty() {
                    return Err(crate::diagnostics::invalid_number(
                        "an underscore in a number must follow a digit",
                        offset..offset + 1,
                    ));
                }
                self.advance();
            } else {
                break;
            }
        }

        if self
            .peek()
            .is_some_and(|(_, ch)| ch.eq_ignore_ascii_case(&'R'))
        {
            let radix = digits.parse::<u32>().map_err(|_| {
                crate::diagnostics::invalid_number(
                    format!("`{digits}` is not a valid radix"),
                    start..self.offset(),
                )
            })?;

            if matches!(radix, 2 | 4 | 8 | 16) {
                self.advance();
                self.read_radix_digits(radix, start)?;
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn read_radix_digits(&mut self, radix: u32, start: usize) -> Result<(), CompileError> {
        let mut saw_digit = false;

        while let Some((offset, ch)) = self.peek() {
            if ch == '_' {
                if !saw_digit {
                    return Err(crate::diagnostics::invalid_number(
                        "an underscore in a number must follow a digit",
                        offset..offset + 1,
                    ));
                }
                self.advance();
                continue;
            }

            if !is_radix_digit(ch, radix) {
                break;
            }

            saw_digit = true;
            self.advance();
        }

        if !saw_digit {
            return Err(crate::diagnostics::invalid_number(
                format!("expected a radix-{radix} digit after `R`"),
                start..self.offset(),
            ));
        }

        Ok(())
    }

    /// Reads `exponent-part` and returns whether it used `&&`.
    fn read_exponent_part(&mut self) -> Result<bool, CompileError> {
        let start = self.offset();

        if !self.peek().is_some_and(|(_, ch)| ch == '&') {
            return Err(crate::diagnostics::invalid_number(
                "this exponent is missing `&`",
                start..start,
            ));
        }

        self.advance();
        let long = if self.peek().is_some_and(|(_, ch)| ch == '&') {
            self.advance();
            true
        } else {
            false
        };

        if let Some((_, ch)) = self.peek()
            && (ch == '+' || ch == '-')
        {
            self.advance();
        }

        if !self.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) {
            self.rewind_to(start);
            return Ok(long);
        }

        self.read_unsigned_integer()?;
        Ok(long)
    }

    fn rewind_to(&mut self, offset: usize) {
        self.seek_to(offset);
    }
}

fn is_radix_digit(ch: char, radix: u32) -> bool {
    let value = match ch.to_ascii_uppercase() {
        '0'..='9' => ch.to_digit(10),
        'A'..='F' => Some((ch.to_ascii_uppercase() as u32) - ('A' as u32) + 10),
        _ => None,
    };

    value.is_some_and(|value| value < radix)
}

#[cfg(test)]
mod tests {
    use crate::lex::token::{NumberKind, TokenKind};
    use crate::lex::tokenize;
    use crate::source::SourceFile;

    fn number_kind(source: &str) -> (NumberKind, String) {
        let token = tokenize(&SourceFile::anonymous(source))
            .expect("test input should tokenize")
            .tokens
            .into_iter()
            .next()
            .expect("expected one token");

        match token.kind {
            TokenKind::NumberLiteral { kind, lexeme } => (kind, lexeme),
            other => panic!("expected number literal, found {other:?}"),
        }
    }

    #[test]
    fn classifies_integer_literals() {
        assert_eq!(number_kind("0"), (NumberKind::Integer, "0".into()));
        assert_eq!(
            number_kind("2R1010"),
            (NumberKind::Integer, "2R1010".into())
        );
    }

    #[test]
    fn classifies_real_literals() {
        assert_eq!(number_kind("2&1"), (NumberKind::Real, "2&1".into()));
        assert_eq!(number_kind(".2&2"), (NumberKind::Real, ".2&2".into()));
        assert_eq!(number_kind("20.0"), (NumberKind::Real, "20.0".into()));
    }

    #[test]
    fn classifies_long_real_literals() {
        assert_eq!(
            number_kind("2.345_678&&0"),
            (NumberKind::LongReal, "2.345_678&&0".into())
        );
    }

    #[test]
    fn leaves_bare_ampersand_as_operator() {
        let kinds: Vec<_> = tokenize(&SourceFile::anonymous("&"))
            .unwrap()
            .tokens
            .into_iter()
            .map(|token| token.kind)
            .collect();

        assert_eq!(kinds, vec![TokenKind::Ampersand]);
    }
}
