//! Character constants per Simula Standard §1.7.

use crate::error::CompileError;

use super::Lexer;

impl<'a> Lexer<'a> {
    /// If `offset` points at the start of a §1.7 character constant, reads and returns its value.
    ///
    /// Returns `Ok(None)` when `'` is a lone character-quote symbol (not followed by a designator).
    pub(super) fn try_read_character_constant(
        &mut self,
        start: usize,
    ) -> Result<Option<(char, std::ops::Range<usize>)>, CompileError> {
        debug_assert!(self.peek().is_some_and(|(_, ch)| ch == '\''));

        let next = self.peek_next_char();
        if next.is_none() {
            return Ok(None);
        }

        if next.is_some_and(super::is_simula_whitespace) {
            let closes = self.source[self.position..]
                .char_indices()
                .nth(2)
                .is_some_and(|(_, ch)| ch == '\'');
            if !closes {
                return Ok(None);
            }

            self.advance();
            let value = self.advance().unwrap().1;
            self.advance();
            return Ok(Some((value, start..self.offset())));
        }

        self.advance();

        let value = if self.peek().is_some_and(|(_, ch)| ch == '!') {
            if let Some(iso_char) = self.try_read_iso_code()? {
                iso_char
            } else {
                self.advance().unwrap().1
            }
        } else if self.peek().is_some_and(|(_, ch)| ch == '"') {
            self.advance();
            '"'
        } else if let Some((_, ch)) = self.peek() {
            if ch == '\n' || ch == '\r' {
                return Err(CompileError::lex(
                    "unterminated character constant",
                    start..self.offset(),
                ));
            }
            self.advance().unwrap().1
        } else {
            return Err(CompileError::lex(
                "unterminated character constant",
                start..self.source.len(),
            ));
        };

        match self.peek() {
            Some((close_offset, '\'')) => {
                self.advance();
                Ok(Some((value, start..close_offset + 1)))
            }
            Some((offset, _)) => Err(CompileError::lex(
                "character constant must contain exactly one character",
                start..offset + 1,
            )),
            None => Err(CompileError::lex(
                "unterminated character constant",
                start..self.source.len(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lex::TokenKind;
    use crate::lex::tokenize;
    use crate::source::SourceFile;

    fn character_value(source: &str) -> char {
        let kind = tokenize(&SourceFile::anonymous(source))
            .expect("source should tokenize")
            .tokens
            .into_iter()
            .next()
            .expect("source should produce one token")
            .kind;
        let TokenKind::CharacterLiteral(value) = kind else {
            panic!("expected character literal, found {kind:?}");
        };
        value
    }

    #[test]
    fn decodes_iso_codes() {
        assert_eq!(character_value("'!111!'"), 'o');
    }

    #[test]
    fn single_quote_character() {
        assert_eq!(character_value("'''"), '\'');
    }

    #[test]
    fn double_quote_character() {
        assert_eq!(character_value("'\"'"), '"');
    }

    #[test]
    fn non_ascii_character() {
        assert_eq!(character_value("'Æ'"), 'Æ');
    }

    #[test]
    fn space_character() {
        assert_eq!(character_value("' '"), ' ');
    }

    #[test]
    fn exclamation_is_not_a_direct_comment() {
        assert_eq!(character_value("'!'"), '!');
    }
}
