//! DAP literal parsing independent of the AST evaluator.

use crate::runtime::text::TextFrame;

/// A value entered in the debug adapter (`setVariable` / evaluate literals).
#[derive(Debug, Clone, PartialEq)]
pub enum DebugLiteral {
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Character(char),
    Text(TextFrame),
    None,
}

impl DebugLiteral {
    pub fn display(&self) -> String {
        match self {
            Self::Integer(n) => n.to_string(),
            Self::Real(n) => {
                let s = format!("{n}");
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{n}.0")
                }
            }
            Self::Boolean(b) => {
                if *b {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            Self::Character(c) => format!("'{c}'"),
            Self::Text(text) => {
                if text.is_notext() {
                    "notext".into()
                } else {
                    let escaped = text
                        .content()
                        .chars()
                        .flat_map(|c| match c {
                            '"' => vec!['\\', '"'],
                            '\\' => vec!['\\', '\\'],
                            '\n' => vec!['\\', 'n'],
                            '\r' => vec!['\\', 'r'],
                            '\t' => vec!['\\', 't'],
                            other => vec![other],
                        })
                        .collect::<String>();
                    format!("\"{escaped}\"")
                }
            }
            Self::None => "none".into(),
        }
    }
}

/// Parse a DAP `setVariable` / evaluate literal.
pub fn parse_debug_value(text: &str) -> Result<DebugLiteral, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("true") {
        return Ok(DebugLiteral::Boolean(true));
    }
    if text.eq_ignore_ascii_case("false") {
        return Ok(DebugLiteral::Boolean(false));
    }
    if text.eq_ignore_ascii_case("none") {
        return Ok(DebugLiteral::None);
    }
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        let inner = &text[1..text.len() - 1];
        return Ok(DebugLiteral::Text(TextFrame::from_literal(inner, false)));
    }
    if text.len() >= 3 && text.starts_with('\'') && text.ends_with('\'') {
        let mut chars = text[1..text.len() - 1].chars();
        let Some(c) = chars.next() else {
            return Err("empty character literal".into());
        };
        if chars.next().is_some() {
            return Err("character literal must be a single character".into());
        }
        return Ok(DebugLiteral::Character(c));
    }
    if let Ok(n) = text.parse::<i64>() {
        return Ok(DebugLiteral::Integer(n));
    }
    if let Ok(n) = text.parse::<f64>() {
        return Ok(DebugLiteral::Real(n));
    }
    Err(format!(
        "cannot parse `{text}` as a value (use integer/real/boolean/text literal)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_literals() {
        assert_eq!(parse_debug_value("42").unwrap(), DebugLiteral::Integer(42));
        assert_eq!(
            parse_debug_value("true").unwrap(),
            DebugLiteral::Boolean(true)
        );
        assert_eq!(parse_debug_value("none").unwrap(), DebugLiteral::None);
        assert!(matches!(
            parse_debug_value("\"hi\"").unwrap(),
            DebugLiteral::Text(_)
        ));
    }
}
