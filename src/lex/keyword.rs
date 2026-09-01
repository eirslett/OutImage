macro_rules! define_keywords {
    ($($variant:ident => $text:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Keyword {
            $($variant,)*
        }

        impl Keyword {
            pub const ALL: &'static [Keyword] = &[$(Self::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text.to_ascii_lowercase().as_str() {
                    $($text => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

define_keywords! {
    Activate => "activate",
    After => "after",
    And => "and",
    Array => "array",
    At => "at",
    Before => "before",
    Begin => "begin",
    Boolean => "boolean",
    Character => "character",
    Class => "class",
    Comment => "comment",
    Delay => "delay",
    Do => "do",
    Else => "else",
    End => "end",
    Eq => "eq",
    Eqv => "eqv",
    External => "external",
    False => "false",
    For => "for",
    Ge => "ge",
    Go => "go",
    Goto => "goto",
    Gt => "gt",
    Hidden => "hidden",
    If => "if",
    Imp => "imp",
    In => "in",
    Inner => "inner",
    Inspect => "inspect",
    Integer => "integer",
    Is => "is",
    Label => "label",
    Le => "le",
    Long => "long",
    Lt => "lt",
    Name => "name",
    Ne => "ne",
    New => "new",
    None => "none",
    Not => "not",
    Notext => "notext",
    Or => "or",
    Otherwise => "otherwise",
    Prior => "prior",
    Procedure => "procedure",
    Protected => "protected",
    Qua => "qua",
    Reactivate => "reactivate",
    Real => "real",
    Ref => "ref",
    Short => "short",
    Step => "step",
    Switch => "switch",
    Text => "text",
    Then => "then",
    This => "this",
    To => "to",
    True => "true",
    Until => "until",
    Value => "value",
    Virtual => "virtual",
    When => "when",
    While => "while",
}

#[cfg(test)]
mod tests {
    use super::Keyword;

    #[test]
    fn parses_keywords_case_insensitively() {
        assert_eq!(Keyword::parse("BEGIN"), Some(Keyword::Begin));
        assert_eq!(Keyword::parse("End"), Some(Keyword::End));
    }

    #[test]
    fn reference_is_not_a_reserved_word() {
        // §5.4.2 mode identifiers are only `value` and `name`. Transmission by
        // reference is the default for text/ref/arrays, not a spelled keyword.
        assert_eq!(Keyword::parse("reference"), None);
    }
}
