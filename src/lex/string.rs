//! String literals per Simula Standard §1.6.

use crate::error::CompileError;
use crate::lex::Keyword;

use super::Lexer;

impl<'a> Lexer<'a> {
    /// Reads a full §1.6 string: one or more simple-strings separated by token separators.
    pub(super) fn read_string(
        &mut self,
        start: usize,
    ) -> Result<(String, std::ops::Range<usize>), CompileError> {
        let (mut value, first_span) = self.read_simple_string(start)?;
        let mut span_end = first_span.end;

        loop {
            self.skip_token_separators();

            if self.peek().is_some_and(|(_, ch)| ch == '"') {
                let part_start = self.offset();
                let (part, part_end) = self.read_simple_string(part_start)?;
                value.push_str(&part);
                span_end = part_end.end;
            } else {
                break;
            }
        }

        Ok((value, start..span_end))
    }

    fn read_simple_string(
        &mut self,
        start: usize,
    ) -> Result<(String, std::ops::Range<usize>), CompileError> {
        self.advance();

        let mut literal = String::new();

        while let Some((offset, ch)) = self.peek() {
            if ch == '"' {
                let next = self.peek_next_char();
                if next == Some('"') {
                    self.advance();
                    self.advance();
                    literal.push('"');
                    continue;
                }

                self.advance();
                return Ok((literal, start..offset + 1));
            }

            if ch == '\n' || ch == '\r' {
                return Err(crate::diagnostics::unterminated_string(start..offset));
            }

            if ch == '!'
                && let Some(iso_char) = self.try_read_iso_code()?
            {
                literal.push(iso_char);
                continue;
            }

            literal.push(self.advance().unwrap().1);
        }

        Err(crate::diagnostics::unterminated_string(
            start..self.source.len(),
        ))
    }

    /// Attempts to read an ISO-code (`!` digit [digit] [digit] `!`) with value `< 256`.
    ///
    /// Returns `Ok(Some(ch))` when a valid code was consumed, `Ok(None)` when the leading
    /// `!` is not part of a valid ISO-code (the caller should treat it as a literal `!`).
    pub(super) fn try_read_iso_code(&mut self) -> Result<Option<char>, CompileError> {
        debug_assert!(self.peek().is_some_and(|(_, ch)| ch == '!'));

        let mut digits = String::new();
        let mut cursor = self.position + '!'.len_utf8();

        while digits.len() < 3 {
            let Some((_, ch)) = self.source[cursor..].char_indices().next() else {
                break;
            };

            if ch.is_ascii_digit() {
                digits.push(ch);
                cursor += ch.len_utf8();
            } else {
                break;
            }
        }

        let closing = self.source[cursor..].chars().next();
        if digits.is_empty() || closing != Some('!') {
            return Ok(None);
        }

        let value: u32 = digits.parse().expect("digits are ascii");
        if value >= 256 {
            return Ok(None);
        }

        self.position = cursor + '!'.len_utf8();
        char::from_u32(value)
            .ok_or_else(|| crate::diagnostics::invalid_iso_code(self.position - 1..self.position))
            .map(Some)
    }

    fn skip_token_separators(&mut self) {
        loop {
            self.skip_whitespace();

            if self.peek().is_some_and(|(_, ch)| ch == '!') {
                self.skip_direct_comment();
                continue;
            }

            if self.options.allow_double_dash_comments
                && self.peek().is_some_and(|(_, ch)| ch == '-')
                && self.peek_next_char() == Some('-')
            {
                let start = self.offset();
                self.skip_double_dash_comment();
                self.push_trivia(crate::lex::TriviaKind::Comment, start..self.offset());
                continue;
            }

            if self.starts_keyword(Keyword::Comment) {
                let keyword = Keyword::Comment.as_str();
                for _ in keyword.chars() {
                    self.advance();
                }
                self.skip_comment_keyword_body();
                continue;
            }

            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lex::TokenKind;
    use crate::lex::tokenize;
    use crate::source::SourceFile;

    fn string_value(source: &str) -> String {
        let kind = tokenize(&SourceFile::anonymous(source))
            .expect("source should tokenize")
            .tokens
            .into_iter()
            .next()
            .expect("source should produce one token")
            .kind;
        let TokenKind::StringLiteral(value) = kind else {
            panic!("expected string literal, found {kind:?}");
        };
        value
    }

    #[test]
    fn decodes_iso_codes() {
        assert_eq!(string_value(r#""!2!ABCDE!3!""#), "\x02ABCDE\x03");
    }

    #[test]
    fn leaves_invalid_iso_codes_as_characters() {
        assert_eq!(string_value(r#""!2""#), "!2");
        assert_eq!(string_value(r#""!256!""#), "!256!");
    }

    #[test]
    fn joins_adjacent_simple_strings() {
        assert_eq!(string_value(r#""Ab" "cde""#), "Abcde");
    }

    #[test]
    fn joins_simple_strings_separated_by_double_dash_comment() {
        assert_eq!(string_value("\"Ab\" -- note\n\"cde\""), "Abcde");
        let source = "\"Ab\" -- note\n\"cde\"";
        let file = SourceFile::anonymous(source);
        let (_, trivia) =
            crate::lex::tokenize_with_trivia(&file, &crate::lex::LexOptions::default())
                .expect("tokenize");
        assert!(
            trivia.iter().any(|item| {
                item.kind == crate::lex::TriviaKind::Comment
                    && &source[item.span.clone()] == "-- note"
            }),
            "{trivia:?}"
        );
    }

    #[test]
    fn escaped_quotes_produce_literal_quote() {
        assert_eq!(string_value(r#""AB"" C""DE""#), r#"AB" C"DE"#);
    }
}
