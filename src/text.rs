//! Intrinsic text attributes and procedures (Standard Chapter 8).

use crate::types::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextIntrinsic {
    Constant,
    Start,
    Length,
    Main,
    Pos,
    Setpos,
    More,
    Getchar,
    Putchar,
    Sub,
    Strip,
    Getint,
    Getreal,
    Getfrac,
    Putint,
    Putfix,
    Putreal,
    Putfrac,
}

impl TextIntrinsic {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "constant" => Some(Self::Constant),
            "start" => Some(Self::Start),
            "length" => Some(Self::Length),
            "main" => Some(Self::Main),
            "pos" => Some(Self::Pos),
            "setpos" => Some(Self::Setpos),
            "more" => Some(Self::More),
            "getchar" => Some(Self::Getchar),
            "putchar" => Some(Self::Putchar),
            "sub" => Some(Self::Sub),
            "strip" => Some(Self::Strip),
            "getint" => Some(Self::Getint),
            "getreal" => Some(Self::Getreal),
            "getfrac" => Some(Self::Getfrac),
            "putint" => Some(Self::Putint),
            "putfix" => Some(Self::Putfix),
            "putreal" => Some(Self::Putreal),
            "putfrac" => Some(Self::Putfrac),
            _ => None,
        }
    }

    pub fn result_type(self) -> Option<Type> {
        match self {
            Self::Constant | Self::More => Some(Type::Boolean),
            Self::Start | Self::Length | Self::Pos | Self::Getint | Self::Getfrac => {
                Some(Type::Integer { short: false })
            }
            Self::Getchar => Some(Type::Character),
            Self::Getreal => Some(Type::Real { long: true }),
            Self::Main | Self::Sub | Self::Strip => Some(Type::Text),
            Self::Setpos
            | Self::Putchar
            | Self::Putint
            | Self::Putfix
            | Self::Putreal
            | Self::Putfrac => None,
        }
    }
}

pub fn is_text_frame_procedure(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "blanks" | "copy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attribute_names_case_insensitively() {
        assert_eq!(
            TextIntrinsic::parse("GetChar"),
            Some(TextIntrinsic::Getchar)
        );
        assert_eq!(TextIntrinsic::parse("MAIN"), Some(TextIntrinsic::Main));
    }
}
