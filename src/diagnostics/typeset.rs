//! Print types and tokens the way a Simula programmer writes them — never `Debug`.

use crate::lex::TokenKind;
use crate::types::Type;

pub fn type_english(ty: &Type) -> String {
    ty.to_string()
}

/// Elm-style note when two `ref` types do not share a prefix.
pub fn ref_prefix_note(found: &Type, expected: &Type, common: Option<&str>) -> Option<String> {
    let (Type::ObjectRef(found_q), Type::ObjectRef(expected_q)) = (found, expected) else {
        return None;
    };
    if found_q.eq_ignore_ascii_case("none")
        || expected_q.eq_ignore_ascii_case("none")
        || found_q.eq_ignore_ascii_case(expected_q)
    {
        return None;
    }
    Some(match common {
        Some(prefix) => format!(
            "`ref({found_q})` is not a `ref({expected_q})`; closest common prefix: `{prefix}`"
        ),
        None => format!(
            "`ref({found_q})` is not a `ref({expected_q})`; they share no prefix. Closest common prefix: none."
        ),
    })
}

pub fn token_english(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Keyword(keyword) => format!("`{}`", keyword.as_str()),
        TokenKind::Identifier(name) => format!("identifier `{name}`"),
        TokenKind::StringLiteral(_) => "a string literal".to_string(),
        TokenKind::CharacterLiteral(_) => "a character constant".to_string(),
        TokenKind::NumberLiteral { .. } => "a number".to_string(),
        TokenKind::Plus => "`+`".into(),
        TokenKind::Minus => "`-`".into(),
        TokenKind::Star => "`*`".into(),
        TokenKind::Slash => "`/`".into(),
        TokenKind::SlashSlash => "`//`".into(),
        TokenKind::StarStar => "`**`".into(),
        TokenKind::Ampersand => "`&`".into(),
        TokenKind::AmpersandAmpersand => "`&&`".into(),
        TokenKind::Assign => "`:=`".into(),
        TokenKind::AssignAlt => "`:-`".into(),
        TokenKind::Lt => "`<`".into(),
        TokenKind::Le => "`<=`".into(),
        TokenKind::Eq => "`=`".into(),
        TokenKind::Ge => "`>=`".into(),
        TokenKind::Gt => "`>`".into(),
        TokenKind::Ne => "`<>`".into(),
        TokenKind::RefEq => "`==`".into(),
        TokenKind::RefNe => "`=/=`".into(),
        TokenKind::CharacterQuote => "`'`".into(),
        TokenKind::LeftParen => "`(`".into(),
        TokenKind::RightParen => "`)`".into(),
        TokenKind::LeftBracket => "`[`".into(),
        TokenKind::RightBracket => "`]`".into(),
        TokenKind::Colon => "`:`".into(),
        TokenKind::Dot => "`.`".into(),
        TokenKind::Comma => "`,`".into(),
        TokenKind::Semicolon => "`;`".into(),
    }
}

/// Ranked, de-duplicated, capped English list of expected tokens.
pub fn expected_list_english(items: &[String]) -> Option<String> {
    let mut seen = Vec::new();
    for item in items {
        if !seen.iter().any(|existing: &String| existing == item) {
            seen.push(item.clone());
        }
        if seen.len() == 3 {
            break;
        }
    }
    match seen.as_slice() {
        [] => None,
        [one] => Some(format!("expected {one}")),
        [a, b] => Some(format!("expected {a} or {b}")),
        [a, b, c] => Some(format!("expected {a}, {b}, or {c}")),
        _ => None,
    }
}
