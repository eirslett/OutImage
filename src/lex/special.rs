//! Simula special symbols (Standard §1.2) with longest-match recognition.

use crate::lex::token::TokenKind;

/// Longest-match special symbol at the start of `source`.
///
/// Returns the token kind and byte length when `source` begins with a special
/// symbol other than bare `:` (handled by the driver because of `:=` / `:-`).
pub fn match_special_symbol(source: &str) -> Option<(TokenKind, usize)> {
    if source.is_empty() {
        return None;
    }

    let bytes = source.as_bytes();

    if bytes.len() >= 3 && &bytes[..3] == b"=/=" {
        return Some((TokenKind::RefNe, 3));
    }

    if bytes.len() >= 2 {
        let pair = &bytes[..2];
        let kind = match pair {
            b":=" => TokenKind::Assign,
            b":-" => TokenKind::AssignAlt,
            b"//" => TokenKind::SlashSlash,
            b"**" => TokenKind::StarStar,
            b"&&" => TokenKind::AmpersandAmpersand,
            b"<=" => TokenKind::Le,
            b"<>" => TokenKind::Ne,
            b">=" => TokenKind::Ge,
            b"==" => TokenKind::RefEq,
            _ => return match_single_special(bytes[0]),
        };
        return Some((kind, 2));
    }

    match_single_special(bytes[0])
}

fn match_single_special(byte: u8) -> Option<(TokenKind, usize)> {
    let kind = match byte {
        b'+' => TokenKind::Plus,
        b'-' => TokenKind::Minus,
        b'*' => TokenKind::Star,
        b'/' => TokenKind::Slash,
        b'&' => TokenKind::Ampersand,
        b'<' => TokenKind::Lt,
        b'>' => TokenKind::Gt,
        b'=' => TokenKind::Eq,
        b'\'' => TokenKind::CharacterQuote,
        b'.' => TokenKind::Dot,
        b',' => TokenKind::Comma,
        b';' => TokenKind::Semicolon,
        _ => return None,
    };
    Some((kind, 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::Keyword;
    use crate::lex::token::TokenKind;
    use crate::source::SourceFile;

    fn match_kind(source: &str) -> Option<TokenKind> {
        match_special_symbol(source).map(|(kind, _)| kind)
    }

    fn match_len(source: &str) -> Option<usize> {
        match_special_symbol(source).map(|(_, len)| len)
    }

    fn tokenize_kinds(source: &str) -> Vec<TokenKind> {
        crate::lex::tokenize(&SourceFile::anonymous(source))
            .expect("tokenize")
            .tokens
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    fn tokenize_spans(source: &str) -> Vec<(TokenKind, std::ops::Range<usize>)> {
        crate::lex::tokenize(&SourceFile::anonymous(source))
            .expect("tokenize")
            .tokens
            .into_iter()
            .map(|token| (token.kind, token.span))
            .collect()
    }

    // --- direct matcher unit tests ---

    #[test]
    fn matches_assignment_operators() {
        assert_eq!(match_kind(":="), Some(TokenKind::Assign));
        assert_eq!(match_len(":="), Some(2));
        assert_eq!(match_kind(":-"), Some(TokenKind::AssignAlt));
        assert_eq!(match_len(":-"), Some(2));
    }

    #[test]
    fn colon_is_not_matched_by_special_symbol_matcher() {
        assert_eq!(match_kind(":"), None);
    }

    #[test]
    fn matches_relational_operators() {
        assert_eq!(match_kind("<"), Some(TokenKind::Lt));
        assert_eq!(match_kind("<="), Some(TokenKind::Le));
        assert_eq!(match_kind("<>"), Some(TokenKind::Ne));
        assert_eq!(match_kind(">"), Some(TokenKind::Gt));
        assert_eq!(match_kind(">="), Some(TokenKind::Ge));
        assert_eq!(match_kind("="), Some(TokenKind::Eq));
    }

    #[test]
    fn matches_ref_equality_operators() {
        assert_eq!(match_kind("=="), Some(TokenKind::RefEq));
        assert_eq!(match_kind("=/="), Some(TokenKind::RefNe));
        assert_eq!(match_len("=/="), Some(3));
    }

    #[test]
    fn matches_arithmetic_operators() {
        assert_eq!(match_kind("+"), Some(TokenKind::Plus));
        assert_eq!(match_kind("-"), Some(TokenKind::Minus));
        assert_eq!(match_kind("*"), Some(TokenKind::Star));
        assert_eq!(match_kind("**"), Some(TokenKind::StarStar));
        assert_eq!(match_kind("/"), Some(TokenKind::Slash));
        assert_eq!(match_kind("//"), Some(TokenKind::SlashSlash));
    }

    #[test]
    fn matches_ampersand_variants() {
        assert_eq!(match_kind("&"), Some(TokenKind::Ampersand));
        assert_eq!(match_kind("&&"), Some(TokenKind::AmpersandAmpersand));
    }

    #[test]
    fn matches_punctuation() {
        assert_eq!(match_kind("'"), Some(TokenKind::CharacterQuote));
        assert_eq!(match_kind("."), Some(TokenKind::Dot));
        assert_eq!(match_kind(","), Some(TokenKind::Comma));
        assert_eq!(match_kind(";"), Some(TokenKind::Semicolon));
    }

    #[test]
    fn longest_match_prefers_two_char_operators() {
        assert_eq!(match_len(":="), Some(2));
        assert_eq!(match_len(":-"), Some(2));
        assert_eq!(match_len("<="), Some(2));
        assert_eq!(match_len("<>"), Some(2));
        assert_eq!(match_len(">="), Some(2));
        assert_eq!(match_len("=="), Some(2));
        assert_eq!(match_len("=/="), Some(3));
        assert_eq!(match_len("//"), Some(2));
        assert_eq!(match_len("**"), Some(2));
        assert_eq!(match_len("&&"), Some(2));
    }

    #[test]
    fn longest_match_does_not_over_consume_prefixes() {
        assert_eq!(match_kind("=x"), Some(TokenKind::Eq));
        assert_eq!(match_len("=x"), Some(1));
        assert_eq!(match_kind("<x"), Some(TokenKind::Lt));
        assert_eq!(match_len("<x"), Some(1));
        assert_eq!(match_kind("/*"), Some(TokenKind::Slash));
        assert_eq!(match_len("/*"), Some(1));
    }

    #[test]
    fn rejects_non_special_prefixes() {
        assert_eq!(match_kind("("), None);
        assert_eq!(match_kind("a"), None);
        assert_eq!(match_kind(":"), None);
    }

    // --- integration tests through tokenize ---

    #[test]
    fn tokenizes_bare_colon_separately_from_assignments() {
        assert_eq!(
            tokenize_kinds("a: b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Colon,
                TokenKind::Identifier("b".into()),
            ]
        );
        assert_eq!(
            tokenize_kinds("a:=b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Assign,
                TokenKind::Identifier("b".into()),
            ]
        );
        assert_eq!(
            tokenize_kinds("a:-b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::AssignAlt,
                TokenKind::Identifier("b".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_slash_vs_slash_slash() {
        assert_eq!(
            tokenize_kinds("a/b//c"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Slash,
                TokenKind::Identifier("b".into()),
                TokenKind::SlashSlash,
                TokenKind::Identifier("c".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_star_vs_star_star() {
        assert_eq!(
            tokenize_kinds("a*b**c"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Star,
                TokenKind::Identifier("b".into()),
                TokenKind::StarStar,
                TokenKind::Identifier("c".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_relational_operator_sequences() {
        assert_eq!(
            tokenize_kinds("a<=b<>c>=d"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Le,
                TokenKind::Identifier("b".into()),
                TokenKind::Ne,
                TokenKind::Identifier("c".into()),
                TokenKind::Ge,
                TokenKind::Identifier("d".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_ref_equality_operators() {
        assert_eq!(
            tokenize_kinds("p==q =/= r"),
            vec![
                TokenKind::Identifier("p".into()),
                TokenKind::RefEq,
                TokenKind::Identifier("q".into()),
                TokenKind::RefNe,
                TokenKind::Identifier("r".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_mixed_operator_stream() {
        assert_eq!(
            tokenize_kinds("x:=y+-z;"),
            vec![
                TokenKind::Identifier("x".into()),
                TokenKind::Assign,
                TokenKind::Identifier("y".into()),
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Identifier("z".into()),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn tokenizes_keyword_relational_operators_from_spelling() {
        assert_eq!(
            tokenize_kinds("lt le eq ge gt ne"),
            vec![
                TokenKind::Lt,
                TokenKind::Le,
                TokenKind::Eq,
                TokenKind::Ge,
                TokenKind::Gt,
                TokenKind::Ne,
            ]
        );
    }

    #[test]
    fn preserves_operator_spans_in_source() {
        let source = "a:=b";
        let spans = tokenize_spans(source);
        assert_eq!(spans[1].0, TokenKind::Assign);
        assert_eq!(spans[1].1, 1..3);
    }

    #[test]
    fn preserves_three_char_operator_span() {
        let source = "a=/=b";
        let spans = tokenize_spans(source);
        assert_eq!(spans[1].0, TokenKind::RefNe);
        assert_eq!(spans[1].1, 1..4);
    }

    #[test]
    fn ampersand_in_expression_is_not_confused_with_exponent() {
        assert_eq!(tokenize_kinds("&"), vec![TokenKind::Ampersand]);
        assert_eq!(
            tokenize_kinds("a&b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Ampersand,
                TokenKind::Identifier("b".into()),
            ]
        );
    }

    #[test]
    fn double_ampersand_in_expression_is_operator_not_number() {
        assert_eq!(
            tokenize_kinds("a&&b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::AmpersandAmpersand,
                TokenKind::Identifier("b".into()),
            ]
        );
    }

    #[test]
    fn operators_survive_whitespace_and_separators() {
        assert_eq!(
            tokenize_kinds("begin x := 1; end;"),
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Identifier("x".into()),
                TokenKind::Assign,
                TokenKind::NumberLiteral {
                    kind: crate::lex::token::NumberKind::Integer,
                    lexeme: "1".into(),
                },
                TokenKind::Semicolon,
                TokenKind::Keyword(Keyword::End),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn consecutive_operators_tokenize_independently() {
        assert_eq!(
            tokenize_kinds("+-*/"),
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
            ]
        );
        assert_eq!(
            tokenize_kinds(":=:-"),
            vec![TokenKind::Assign, TokenKind::AssignAlt]
        );
    }

    #[test]
    fn assignment_operators_do_not_merge_across_boundary() {
        assert_eq!(
            tokenize_kinds(":=:-"),
            vec![TokenKind::Assign, TokenKind::AssignAlt]
        );
        assert_eq!(
            tokenize_kinds("<=<>>="),
            vec![TokenKind::Le, TokenKind::Ne, TokenKind::Ge]
        );
    }
}
