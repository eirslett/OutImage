//! Simula lexer with a driver for context-sensitive Standard rules (end-comments,
//! directives, token separators, comment forms) and longest-match special symbols.

use crate::error::CompileError;
use crate::source::SourceFile;

#[path = "character.rs"]
mod character;
#[path = "number.rs"]
mod number;
#[path = "string.rs"]
mod string;

use super::LexOptions;
use super::keyword::Keyword;
use super::special::match_special_symbol;
use super::token::{Token, TokenKind, TokenStream};
use super::trivia::{Trivia, TriviaKind, trim_span};

pub fn tokenize(source: &SourceFile) -> Result<TokenStream, CompileError> {
    tokenize_with_options(source, &LexOptions::default())
}

pub fn tokenize_with_options(
    source: &SourceFile,
    options: &LexOptions,
) -> Result<TokenStream, CompileError> {
    let (tokens, _, errors) = tokenize_recovering(source, options, false);
    finish_lex(tokens, errors)
}

/// Tokenize while retaining comments, directives, and end-comments.
pub fn tokenize_with_trivia(
    source: &SourceFile,
    options: &LexOptions,
) -> Result<(TokenStream, Vec<Trivia>), CompileError> {
    let (tokens, trivia, errors) = tokenize_recovering(source, options, true);
    match finish_lex(tokens, errors) {
        Ok(tokens) => Ok((tokens, trivia)),
        Err(error) => Err(error),
    }
}

/// Tokenize even after recoverable errors (invalid characters, missing
/// separators). Callers such as the LSP keep the token stream and report
/// every collected diagnostic.
pub fn tokenize_recovering(
    source: &SourceFile,
    options: &LexOptions,
    collect_trivia: bool,
) -> (TokenStream, Vec<Trivia>, Vec<CompileError>) {
    Lexer::new(&source.text, options, collect_trivia).tokenize_collecting()
}

fn finish_lex(tokens: TokenStream, errors: Vec<CompileError>) -> Result<TokenStream, CompileError> {
    if errors.is_empty() {
        Ok(tokens)
    } else {
        Err(crate::error::CompileErrors::new(errors).into_bundled())
    }
}

/// Whitespace for Simula lexing: Unicode whitespace plus U+001A SUBSTITUTE
/// (DOS Ctrl-Z / SUB), which historic corpora sometimes embed as a separator.
fn is_simula_whitespace(ch: char) -> bool {
    ch.is_whitespace() || ch == '\u{001A}'
}

/// Tracks nested `#ifdef` / `#ifndef` branches while lexing C-preprocessor lines.
#[derive(Clone, Copy, Debug)]
struct CppIfFrame {
    /// Whether the current branch is being skipped (not emitted as Simula tokens).
    skipping: bool,
    /// Whether a branch has already been taken in this `#if` block.
    taken: bool,
}

struct Lexer<'a> {
    source: &'a str,
    position: usize,
    options: LexOptions,
    cpp_stack: Vec<CppIfFrame>,
    collect_trivia: bool,
    trivia: Vec<Trivia>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, options: &LexOptions, collect_trivia: bool) -> Self {
        Self {
            source,
            position: 0,
            options: *options,
            cpp_stack: Vec::new(),
            collect_trivia,
            trivia: Vec::new(),
        }
    }

    fn push_trivia(&mut self, kind: TriviaKind, span: std::ops::Range<usize>) {
        if self.collect_trivia && span.start < span.end {
            self.trivia.push(Trivia::new(kind, span));
        }
    }

    fn cpp_skipping(&self) -> bool {
        self.cpp_stack.last().is_some_and(|frame| frame.skipping)
    }

    fn tokenize_collecting(mut self) -> (TokenStream, Vec<Trivia>, Vec<CompileError>) {
        let mut tokens = Vec::new();
        let mut errors = Vec::new();

        while let Some((offset, ch)) = self.peek() {
            if self.is_directive_line_at(offset) {
                // `%` lines are line-oriented trivia (Standard §1.1). Elide them
                // (and corpus-style continuations) so the parser never sees them.
                if let Err(error) = self.skip_directive_block() {
                    errors.push(error);
                    break;
                }
                continue;
            }

            if self.is_cpp_line_at(offset) {
                if let Err(error) = self.handle_cpp_line() {
                    errors.push(error);
                    break;
                }
                continue;
            }

            if self.cpp_skipping() {
                let line_end = self.line_end_at(offset);
                self.push_trivia(TriviaKind::Comment, offset..line_end);
                self.skip_to_end_of_line();
                continue;
            }

            if is_simula_whitespace(ch) {
                self.advance();
                continue;
            }

            if ch == '%' {
                errors.push(crate::diagnostics::directive_not_at_column_zero(
                    offset..offset + 1,
                ));
                self.advance();
                continue;
            }

            if ch == '!' {
                let start = offset;
                self.skip_direct_comment();
                self.push_trivia(TriviaKind::Comment, start..self.offset());
                continue;
            }

            if ch == '-'
                && self.options.allow_double_dash_comments
                && self.peek_next_char() == Some('-')
            {
                let start = offset;
                self.skip_double_dash_comment();
                self.push_trivia(TriviaKind::Comment, start..self.offset());
                continue;
            }

            if ch == '.'
                && self
                    .peek_next_char()
                    .is_some_and(|next| next.is_ascii_digit())
            {
                match self.read_number_from_dot(offset) {
                    Ok(token) => {
                        self.note_separator(&mut errors, token.span.end, false);
                        tokens.push(token);
                    }
                    Err(error) => {
                        errors.push(error);
                        self.skip_until_whitespace();
                    }
                }
                continue;
            }

            if ch == '&' {
                match self.try_read_exponent_number(offset) {
                    Ok(Some(token)) => {
                        self.note_separator(&mut errors, token.span.end, false);
                        tokens.push(token);
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        errors.push(error);
                        self.skip_until_whitespace();
                        continue;
                    }
                }
            }

            if ch == '\'' {
                match self.try_read_character_constant(offset) {
                    Ok(Some((value, span))) => {
                        tokens.push(Token::new(TokenKind::CharacterLiteral(value), span));
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        errors.push(error);
                        self.skip_until_whitespace();
                        continue;
                    }
                }
            }

            if let Some(token) = self.read_special_symbol(offset, ch) {
                tokens.push(token);
                continue;
            }

            if ch == '(' {
                self.advance();
                tokens.push(Token::new(TokenKind::LeftParen, offset..offset + 1));
                continue;
            }

            if ch == ')' {
                self.advance();
                tokens.push(Token::new(TokenKind::RightParen, offset..offset + 1));
                continue;
            }

            if ch == '[' || ch == ']' {
                if !self.options.allow_square_bracket_subscripts {
                    errors.push(crate::diagnostics::unexpected_character(
                        ch,
                        offset..offset + ch.len_utf8(),
                    ));
                    self.advance();
                    continue;
                }
                self.advance();
                let kind = if ch == '[' {
                    TokenKind::LeftBracket
                } else {
                    TokenKind::RightBracket
                };
                tokens.push(Token::new(kind, offset..offset + 1));
                continue;
            }

            if ch == '"' {
                match self.read_string(offset) {
                    Ok((literal, span)) => {
                        self.note_separator(&mut errors, span.end, false);
                        tokens.push(Token::new(TokenKind::StringLiteral(literal), span));
                    }
                    Err(error) => {
                        errors.push(error);
                        self.skip_until_whitespace();
                    }
                }
                continue;
            }

            if ch.is_ascii_alphabetic() || ch == '_' {
                let start = offset;
                let spelling = self.read_identifier();
                let end = self.offset();

                let mut after_keyword = false;
                if let Some(keyword) = Keyword::parse(&spelling) {
                    if keyword == Keyword::Comment {
                        self.skip_comment_keyword_body();
                        self.push_trivia(TriviaKind::Comment, start..self.offset());
                        continue;
                    }

                    if let Some(operator) = Self::relational_operator_token(keyword) {
                        tokens.push(Token::new(operator, start..end));
                        continue;
                    }

                    tokens.push(Token::new(TokenKind::Keyword(keyword), start..end));
                    after_keyword = true;

                    if keyword == Keyword::End {
                        if let Some(name) = self.take_block_end_name() {
                            tokens.push(name);
                        }
                        let comment_start = self.offset();
                        self.skip_end_comment();
                        if let Some(span) = trim_span(self.source, comment_start..self.offset()) {
                            self.push_trivia(TriviaKind::EndComment, span);
                        }
                    }
                } else {
                    tokens.push(Token::new(TokenKind::Identifier(spelling), start..end));
                }

                if !self.is_end_comment_delimiter_at(self.offset()) {
                    // Historic Simula sources omit a space before `"` after
                    // keywords (`else"…"`, `then"…"`); identifiers still need a
                    // separator (`foo"bar"` is illegal per §1.9).
                    self.note_separator(&mut errors, self.offset(), after_keyword);
                }
                continue;
            }

            if ch.is_ascii_digit() {
                match self.read_number_from_digit(offset) {
                    Ok(token) => {
                        self.note_separator(&mut errors, token.span.end, false);
                        tokens.push(token);
                    }
                    Err(error) => {
                        errors.push(error);
                        self.skip_until_whitespace();
                    }
                }
                continue;
            }

            errors.push(crate::diagnostics::unexpected_character(
                ch,
                offset..offset + ch.len_utf8(),
            ));
            self.advance();
        }

        (TokenStream::new(tokens), self.trivia, errors)
    }

    fn note_separator(&self, errors: &mut Vec<CompileError>, offset: usize, allow_string: bool) {
        if let Err(error) = self.ensure_token_separator_after_word_token(offset, allow_string) {
            errors.push(error);
        }
    }

    fn skip_until_whitespace(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if is_simula_whitespace(ch) {
                break;
            }
            self.advance();
        }
    }

    fn offset(&self) -> usize {
        self.position
    }

    fn peek(&mut self) -> Option<(usize, char)> {
        let (relative, ch) = self.source[self.position..].char_indices().next()?;
        Some((self.position + relative, ch))
    }

    fn advance(&mut self) -> Option<(usize, char)> {
        let (offset, ch) = self.peek()?;
        self.position = offset + ch.len_utf8();
        Some((offset, ch))
    }

    fn peek_next_char(&mut self) -> Option<char> {
        self.source[self.position..]
            .char_indices()
            .nth(1)
            .map(|(_, ch)| ch)
    }

    /// Reads a §1.2 special symbol using longest-match rules.
    fn read_special_symbol(&mut self, offset: usize, ch: char) -> Option<Token> {
        if ch == ':' && !matches!(self.peek_next_char(), Some('=') | Some('-')) {
            self.advance();
            return Some(Token::new(TokenKind::Colon, offset..self.offset()));
        }

        let (kind, len) = match_special_symbol(&self.source[offset..])?;
        self.position = offset + len;
        Some(Token::new(kind, offset..self.position))
    }

    fn read_identifier(&mut self) -> String {
        let mut spelling = String::new();

        while let Some((_, ch)) = self.peek() {
            if ch.is_ascii_alphabetic() || ch.is_ascii_digit() || ch == '_' {
                spelling.push(self.advance().unwrap().1);
            } else {
                break;
            }
        }

        spelling
    }

    fn skip_direct_comment(&mut self) {
        self.advance();

        while let Some((_, ch)) = self.peek() {
            if ch == ';' {
                self.advance();
                return;
            }
            self.advance();
        }
    }

    fn skip_comment_keyword_body(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if ch == ';' {
                self.advance();
                return;
            }
            self.advance();
        }
    }

    /// `--` through the last character of the line (newline is left unconsumed).
    fn skip_double_dash_comment(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if ch == '\n' || ch == '\r' {
                return;
            }
            self.advance();
        }
    }

    /// Optional block-end identifier (§4.10.4) immediately after `end`.
    ///
    /// Emitted only when a single identifier is followed by an end-comment
    /// delimiter (`;` / `end` / `else` / `when` / `otherwise`) or EOF. Otherwise
    /// the text remains part of the end-comment skipped by [`Self::skip_end_comment`].
    fn take_block_end_name(&mut self) -> Option<Token> {
        self.skip_whitespace();
        let start = self.offset();
        let (_, ch) = self.peek()?;
        if !ch.is_ascii_alphabetic() && ch != '_' {
            return None;
        }

        let spelling = self.read_identifier();
        if Keyword::parse(&spelling).is_some() {
            // Keywords after `end` belong to the end-comment / following syntax
            // (e.g. nested `end`), not a block-end name.
            self.position = start;
            return None;
        }

        let after_name = self.offset();
        self.skip_whitespace();
        if self.is_end_comment_delimiter_at(self.offset()) || self.peek().is_none() {
            return Some(Token::new(
                TokenKind::Identifier(spelling),
                start..after_name,
            ));
        }

        // More end-comment text follows — leave everything for skip_end_comment.
        self.position = start;
        None
    }

    /// Skips an end-comment per Simula Standard §1.8.1.
    ///
    /// After the keyword `end` (and an optional block-end name), any following
    /// characters and line separations form an end-comment until a delimiter is
    /// seen. Delimiters are the special symbols `;`, `end`, `else`, `when`, and
    /// `otherwise`. The delimiter itself is left unconsumed.
    fn skip_end_comment(&mut self) {
        const KEYWORD_TERMINATORS: [Keyword; 4] = [
            Keyword::End,
            Keyword::Else,
            Keyword::When,
            Keyword::Otherwise,
        ];

        loop {
            self.skip_whitespace();

            if self.peek().is_some_and(|(_, ch)| ch == ';') {
                return;
            }

            if KEYWORD_TERMINATORS
                .iter()
                .any(|keyword| self.starts_keyword(*keyword))
            {
                return;
            }

            if self.peek().is_none() {
                return;
            }

            self.advance();
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if is_simula_whitespace(ch) {
                self.advance();
            } else {
                break;
            }
        }
    }

    /// Per Simula Standard §1.10.1, maps alternate spellings to relational operators.
    fn relational_operator_token(keyword: Keyword) -> Option<TokenKind> {
        match keyword {
            Keyword::Lt => Some(TokenKind::Lt),
            Keyword::Le => Some(TokenKind::Le),
            Keyword::Eq => Some(TokenKind::Eq),
            Keyword::Ge => Some(TokenKind::Ge),
            Keyword::Gt => Some(TokenKind::Gt),
            Keyword::Ne => Some(TokenKind::Ne),
            _ => None,
        }
    }

    /// Per Simula Standard §1.9, consecutive identifiers, keywords, simple strings,
    /// and unsigned numbers must be separated by a token separator.
    ///
    /// When `allow_string_delimiter` is set (after a keyword), an immediately
    /// following `"` / `'` is accepted — common in historic corpora and
    /// unambiguous because the delimiter cannot extend the keyword spelling.
    fn ensure_token_separator_after_word_token(
        &self,
        offset: usize,
        allow_string_delimiter: bool,
    ) -> Result<(), CompileError> {
        let Some(ch) = self.source[offset..].chars().next() else {
            return Ok(());
        };

        if allow_string_delimiter && matches!(ch, '"' | '\'') {
            return Ok(());
        }

        // Strings and character constants are word-tokens for separator purposes
        // even though their opening delimiter is punctuation.
        let starts_word_token = ch.is_ascii_alphabetic()
            || ch == '_'
            || ch.is_ascii_digit()
            || ch == '"'
            || ch == '\''
            || (ch == '.'
                && self.source[offset..]
                    .chars()
                    .nth(1)
                    .is_some_and(|next| next.is_ascii_digit()));

        if !starts_word_token {
            return Ok(());
        }

        Err(crate::diagnostics::missing_token_separator(
            offset..offset + ch.len_utf8(),
        ))
    }

    /// Whether `offset` begins an §1.8.1 end-comment delimiter.
    fn is_end_comment_delimiter_at(&self, offset: usize) -> bool {
        if self.source[offset..].starts_with(';') {
            return true;
        }

        const KEYWORD_TERMINATORS: [Keyword; 4] = [
            Keyword::End,
            Keyword::Else,
            Keyword::When,
            Keyword::Otherwise,
        ];

        KEYWORD_TERMINATORS
            .iter()
            .any(|keyword| Self::starts_keyword_at(self.source, offset, *keyword))
    }

    /// Returns whether `offset` begins a directive line per Simula Standard §1.1.
    fn is_directive_line_at(&self, offset: usize) -> bool {
        self.source.as_bytes().get(self.line_start(offset)) == Some(&b'%')
    }

    /// Returns whether `offset` begins a C-preprocessor line (`#ifdef`, `#include`, …).
    fn is_cpp_line_at(&self, offset: usize) -> bool {
        let line_start = self.line_start(offset);
        let line = self.source[line_start..].lines().next().unwrap_or("");
        line.trim_start().starts_with('#')
    }

    fn skip_to_end_of_line(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if ch == '\n' || ch == '\r' {
                break;
            }
            self.advance();
        }
        self.skip_line_terminator();
    }

    fn handle_cpp_line(&mut self) -> Result<(), CompileError> {
        let line_start = self.line_start(self.offset());
        let mut end = self.offset();
        while end < self.source.len() {
            let ch = self.source[end..].chars().next().unwrap();
            if ch == '\n' || ch == '\r' {
                break;
            }
            end += ch.len_utf8();
        }

        let line = self.source[line_start..end].trim();
        let directive = line
            .strip_prefix('#')
            .map(str::trim)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("")
            .to_ascii_lowercase();

        self.push_trivia(TriviaKind::Directive, line_start..end);

        match directive.as_str() {
            "ifdef" => self.cpp_stack.push(CppIfFrame {
                skipping: true,
                taken: false,
            }),
            "ifndef" => self.cpp_stack.push(CppIfFrame {
                skipping: false,
                taken: true,
            }),
            "else" => {
                if let Some(frame) = self.cpp_stack.last_mut() {
                    if frame.taken {
                        frame.skipping = true;
                    } else {
                        frame.skipping = false;
                        frame.taken = true;
                    }
                }
            }
            "endif" => {
                self.cpp_stack.pop();
            }
            _ => {}
        }

        self.index_to(end);
        self.skip_line_terminator();
        Ok(())
    }

    fn line_end_at(&self, offset: usize) -> usize {
        self.source[offset..]
            .find(['\n', '\r'])
            .map(|index| offset + index)
            .unwrap_or(self.source.len())
    }

    fn line_start(&self, offset: usize) -> usize {
        self.source[..offset]
            .rfind(['\n', '\r'])
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    /// Skip a `%` directive line and any following corpus-repair continuation lines.
    ///
    /// Continuations cover incompletely commented-out blocks in real sources
    /// (a non-`%` line sandwiched between `%` lines, or broken across a trailing
    /// comma/`(`/`else`). This is lex-time elision only — not parser syntax.
    fn skip_directive_block(&mut self) -> Result<(), CompileError> {
        let mut previous = self.skip_directive_line()?;
        while self.is_directive_continuation_line(&previous) {
            previous = self.skip_directive_continuation_line()?;
        }
        Ok(())
    }

    fn directive_text_suggests_following_line(text: &str) -> bool {
        let trimmed = text.trim_end();
        if trimmed.contains('=') {
            return trimmed.ends_with(',') || trimmed.ends_with('(');
        }
        trimmed.ends_with(',') || trimmed.ends_with('(') || trimmed.ends_with(" else")
    }

    fn is_directive_continuation_line(&self, previous_directive: &str) -> bool {
        let offset = self.offset();
        if offset >= self.source.len() {
            return false;
        }
        if self.is_directive_line_at(offset) {
            return false;
        }
        let line = self.line_text_at(offset);
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.contains(":=") || trimmed.contains(":-") || trimmed.contains('=') {
            return false;
        }
        if Self::directive_text_suggests_following_line(previous_directive) {
            return true;
        }
        let next_line = self.next_line_start(offset);
        if next_line >= self.source.len() || !self.is_directive_line_at(next_line) {
            return false;
        }
        trimmed.contains("))") || trimmed.contains(" else")
    }

    fn line_text_at(&self, offset: usize) -> &str {
        let line_start = self.line_start(offset);
        let line_end = self.source[line_start..]
            .find(['\n', '\r'])
            .map(|index| line_start + index)
            .unwrap_or(self.source.len());
        &self.source[line_start..line_end]
    }

    fn next_line_start(&self, offset: usize) -> usize {
        let line_end = self.source[offset..]
            .find(['\n', '\r'])
            .map(|index| offset + index)
            .unwrap_or(self.source.len());
        let mut next = line_end;
        if self.source.as_bytes().get(next) == Some(&b'\r') {
            next += 1;
        }
        if self.source.as_bytes().get(next) == Some(&b'\n') {
            next += 1;
        }
        next
    }

    fn skip_directive_continuation_line(&mut self) -> Result<String, CompileError> {
        let line_start = self.offset();
        let mut end = line_start;
        while end < self.source.len() {
            let ch = self.source[end..].chars().next().unwrap();
            if ch == '\n' || ch == '\r' {
                break;
            }
            end += ch.len_utf8();
        }
        let text = self.source[line_start..end].to_string();
        self.push_trivia(TriviaKind::Directive, line_start..end);
        self.index_to(end);
        self.skip_line_terminator();
        Ok(text)
    }

    fn skip_directive_line(&mut self) -> Result<String, CompileError> {
        let start = self.offset();
        let line_start = self.line_start(start);
        let mut end = start;

        while end < self.source.len() {
            let ch = self.source[end..].chars().next().unwrap();
            if ch == '\n' || ch == '\r' {
                break;
            }
            end += ch.len_utf8();
        }

        let text = self.source[line_start + 1..end].to_string();
        self.push_trivia(TriviaKind::Directive, line_start..end);
        self.index_to(end);
        self.skip_line_terminator();
        Ok(text)
    }

    fn skip_line_terminator(&mut self) {
        if self.peek().is_some_and(|(_, ch)| ch == '\r') {
            self.advance();
        }
        if self.peek().is_some_and(|(_, ch)| ch == '\n') {
            self.advance();
        }
    }

    fn seek_to(&mut self, offset: usize) {
        self.position = offset.min(self.source.len());
    }

    fn index_to(&mut self, offset: usize) {
        self.seek_to(offset);
    }

    fn starts_keyword(&mut self, keyword: Keyword) -> bool {
        Self::starts_keyword_at(self.source, self.offset(), keyword)
    }

    fn starts_keyword_at(source: &str, offset: usize, keyword: Keyword) -> bool {
        if offset > 0 {
            let previous = source[..offset].chars().next_back();
            if previous
                .is_some_and(|ch| ch.is_ascii_alphabetic() || ch.is_ascii_digit() || ch == '_')
            {
                return false;
            }
        }

        let mut chars = source[offset..].char_indices();
        for expected in keyword.as_str().chars() {
            match chars.next() {
                Some((_, ch)) if ch.eq_ignore_ascii_case(&expected) => {}
                _ => return false,
            }
        }

        match chars.next() {
            None => true,
            Some((_, ch)) => !(ch.is_ascii_alphabetic() || ch.is_ascii_digit() || ch == '_'),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexOptions, NumberKind, TriviaKind};
    use crate::source::SourceFile;

    fn tokenize(source: &str) -> TokenStream {
        super::tokenize(&SourceFile::anonymous(source)).expect("test input should tokenize")
    }

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .tokens
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn empty_source_produces_no_tokens() {
        assert!(tokenize("").tokens.is_empty());
    }

    #[test]
    fn treats_substitute_as_whitespace() {
        assert_eq!(
            kinds("begin\u{001A}end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn tokenizes_begin_keyword() {
        assert_eq!(kinds("begin"), vec![TokenKind::Keyword(Keyword::Begin)]);
    }

    #[test]
    fn tokenizes_keywords_case_insensitively() {
        assert_eq!(
            kinds("BEGIN End;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn tokenizes_identifier() {
        assert_eq!(
            kinds("MyClass"),
            vec![TokenKind::Identifier("MyClass".into())]
        );
    }

    #[test]
    fn skips_direct_comments() {
        assert_eq!(
            kinds("begin ! hello; end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn skips_double_dash_line_comments() {
        assert_eq!(
            kinds("begin -- hello\nend;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
        assert_eq!(
            kinds("begin integer x; x := 1; -- trailing\nend;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::Integer),
                TokenKind::Identifier("x".into()),
                TokenKind::Semicolon,
                TokenKind::Identifier("x".into()),
                TokenKind::Assign,
                TokenKind::NumberLiteral {
                    kind: NumberKind::Integer,
                    lexeme: "1".into(),
                },
                TokenKind::Semicolon,
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn double_dash_inside_string_is_not_a_comment() {
        assert_eq!(
            kinds(r#"begin OutText("--- START"); end;"#),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Identifier("OutText".into()),
                TokenKind::LeftParen,
                TokenKind::StringLiteral("--- START".into()),
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn double_dash_is_two_minuses_when_disabled() {
        let kinds = super::tokenize_with_options(
            &SourceFile::anonymous("begin x := 1--2; end;"),
            &super::LexOptions {
                allow_double_dash_comments: false,
                ..super::LexOptions::default()
            },
        )
        .expect("tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Identifier("x".into()),
                TokenKind::Assign,
                TokenKind::NumberLiteral {
                    kind: NumberKind::Integer,
                    lexeme: "1".into(),
                },
                TokenKind::Minus,
                TokenKind::Minus,
                TokenKind::NumberLiteral {
                    kind: NumberKind::Integer,
                    lexeme: "2".into(),
                },
                TokenKind::Semicolon,
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn skips_end_comments() {
        assert_eq!(
            kinds("begin end of program;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn skips_comment_keyword_comments() {
        assert_eq!(
            kinds("begin comment end else; end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn end_comment_terminates_at_following_end_keyword() {
        assert_eq!(
            kinds("begin begin end end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn end_comment_terminates_at_else_without_consuming_it() {
        // A lone identifier before `else` is the §4.10.4 block-end name.
        assert_eq!(
            kinds("begin end note else"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Identifier("note".into()),
                TokenKind::Keyword(Keyword::Else),
            ]
        );
        // Multi-word text before `else` remains an end-comment.
        assert_eq!(
            kinds("begin end of block else"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Keyword(Keyword::Else),
            ]
        );
    }

    #[test]
    fn end_comment_does_not_terminate_at_suffix_end_in_identifier() {
        // A single identifier before `;` is the §4.10.4 block-end name; the
        // `end` substring inside `Have_weekend` must not act as a terminator.
        assert_eq!(
            kinds("begin end Have_weekend; end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Identifier("Have_weekend".into()),
                TokenKind::Semicolon,
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
        assert_eq!(
            kinds("begin end note Have_weekend; end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn emits_block_end_name_identifier() {
        assert_eq!(
            kinds("begin end myblock;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Identifier("myblock".into()),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn end_comment_may_contain_exclamation_marks() {
        assert_eq!(
            kinds("begin end !then;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn tokenizes_square_brackets_when_enabled() {
        assert_eq!(
            kinds("A[1]"),
            vec![
                TokenKind::Identifier("A".into()),
                TokenKind::LeftBracket,
                TokenKind::NumberLiteral {
                    kind: NumberKind::Integer,
                    lexeme: "1".into(),
                },
                TokenKind::RightBracket,
            ]
        );
    }

    #[test]
    fn rejects_square_brackets_when_disabled() {
        let error = super::tokenize_with_options(
            &SourceFile::anonymous("A[1]"),
            &super::LexOptions {
                allow_square_bracket_subscripts: false,
                ..super::LexOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains('[') && error.to_string().contains("legal character"),
            "{}",
            error
        );
    }

    #[test]
    fn tokenizes_mixed_subscript_and_call_delimiters() {
        assert_eq!(
            kinds("A[1](2)"),
            vec![
                TokenKind::Identifier("A".into()),
                TokenKind::LeftBracket,
                TokenKind::NumberLiteral {
                    kind: NumberKind::Integer,
                    lexeme: "1".into(),
                },
                TokenKind::RightBracket,
                TokenKind::LeftParen,
                TokenKind::NumberLiteral {
                    kind: NumberKind::Integer,
                    lexeme: "2".into(),
                },
                TokenKind::RightParen,
            ]
        );
    }

    #[test]
    fn tokenizes_string_literals() {
        assert_eq!(
            kinds(r#""hello world""#),
            vec![TokenKind::StringLiteral("hello world".into())]
        );
    }

    #[test]
    fn rejects_percent_not_at_line_start() {
        let error = super::tokenize(&SourceFile::anonymous(
            "begin\n    outimage; % directive\nend",
        ))
        .unwrap_err();
        assert!(error.message.contains("first character"));
    }

    #[test]
    fn elides_top_level_directives() {
        assert_eq!(
            kinds("% first directive\n% second directive\nbegin\nend"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
            ]
        );
    }

    #[test]
    fn elides_block_directive() {
        assert_eq!(
            kinds("begin\n% second directive\nend"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
            ]
        );
    }

    #[test]
    fn allows_keyword_immediately_before_string_literal() {
        assert_eq!(
            kinds(r#"else"false""#),
            vec![
                TokenKind::Keyword(Keyword::Else),
                TokenKind::StringLiteral("false".into()),
            ]
        );
    }

    #[test]
    fn elides_directive_continuations_between_percent_lines() {
        assert_eq!(
            kinds(
                "% if fillname = \"gray25\" then new Pixmap(if WhitePixel = 1\n\
                 %                       then XGray25Pixmap(windowID) else\n\
                 XGray75Pixmap(windowID)) else\n\
                 %          if fillname = \"gray75\" then new Pixmap\n\
                 begin\nend"
            ),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
            ]
        );
    }

    #[test]
    fn elides_directive_continuations_after_trailing_comma() {
        assert_eq!(
            kinds(
                "% if inside_top_window then TopWindow.PlacePointer(pointer_x,\n\
                 pointer_y);\n\
                 begin\nend"
            ),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
            ]
        );
    }

    fn trivia(source: &str) -> Vec<(TriviaKind, String)> {
        let file = SourceFile::anonymous(source);
        let (_, trivia) =
            super::tokenize_with_trivia(&file, &LexOptions::default()).expect("tokenize");
        trivia
            .into_iter()
            .map(|item| (item.kind, source[item.span.clone()].to_string()))
            .collect()
    }

    #[test]
    fn trivia_captures_comments_directives_and_end_comments() {
        let source = "% dir\n! hi;\ncomment x;\nbegin end-note;\n";
        let items = trivia(source);
        assert!(
            items
                .iter()
                .any(|(kind, text)| *kind == TriviaKind::Directive && text == "% dir")
        );
        assert!(
            items
                .iter()
                .any(|(kind, text)| *kind == TriviaKind::Comment && text == "! hi;")
        );
        assert!(
            items
                .iter()
                .any(|(kind, text)| *kind == TriviaKind::Comment && text == "comment x;")
        );
        assert!(
            items
                .iter()
                .any(|(kind, text)| *kind == TriviaKind::EndComment && text == "-note")
        );
    }

    #[test]
    fn trivia_captures_double_dash_line_comments() {
        let source = "begin\n-- whole line\nx := 1; -- trailing\nend;\n";
        let items = trivia(source);
        assert!(
            items
                .iter()
                .any(|(kind, text)| *kind == TriviaKind::Comment && text == "-- whole line"),
            "{items:?}"
        );
        assert!(
            items
                .iter()
                .any(|(kind, text)| *kind == TriviaKind::Comment && text == "-- trailing"),
            "{items:?}"
        );
    }

    #[test]
    fn trivia_does_not_change_parser_tokens() {
        let source = "% d\nbegin !c; integer a; end foo;\n";
        let file = SourceFile::anonymous(source);
        let plain = super::tokenize(&file).unwrap();
        let (with_trivia, _) = super::tokenize_with_trivia(&file, &LexOptions::default()).unwrap();
        assert_eq!(plain.tokens, with_trivia.tokens);
    }

    #[test]
    fn recovers_past_unexpected_characters() {
        let file = SourceFile::anonymous("begin integer x; x := @1; y := #2; end;");
        let (tokens, _, errors) = super::tokenize_recovering(&file, &LexOptions::default(), false);
        assert!(
            errors.len() >= 2,
            "expected one diagnostic per stray character, got {errors:?}"
        );
        assert!(
            errors.iter().all(|error| error.report_code() == "E0001"),
            "{errors:?}"
        );
        assert!(
            tokens
                .tokens
                .iter()
                .any(|token| matches!(token.kind, TokenKind::Keyword(Keyword::End))),
            "recovery should keep tokenizing after `@` / `#`: {:?}",
            tokens.tokens
        );
    }
}
