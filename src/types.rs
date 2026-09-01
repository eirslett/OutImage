//! Simula type system (Standard Chapter 2).

use std::fmt;

/// Classification of an unsigned number literal (Standard §1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticLiteralKind {
    Integer,
    Real,
    LongReal,
}

/// A Simula type (Standard Chapter 2).
///
/// ```text
/// type = value-type | reference-type
/// value-type = arithmetic-type | boolean | character
/// arithmetic-type = integer-type | real-type
/// integer-type = [ short ] integer
/// real-type = [ long ] real
/// reference-type = object-reference-type | text
/// object-reference-type = ref "(" qualification ")"
/// qualification = class-identifier
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Integer {
        short: bool,
    },
    Real {
        long: bool,
    },
    Boolean,
    Character,
    ObjectRef(String),
    Text,
    /// Array type with element type and dimension count (§5.2).
    Array {
        element: Box<Type>,
        dims: usize,
    },
}

impl Type {
    /// The type of an integer number literal.
    pub fn integer_literal() -> Self {
        Self::Integer { short: false }
    }

    /// The type of a real number literal.
    pub fn real_literal(long: bool) -> Self {
        Self::Real { long }
    }

    /// Whether `self` is assignment-compatible with `other` per Chapter 2 rules.
    ///
    /// `short integer` and `integer` are fully compatible (§2.1.1).
    /// `long real` and `real` are fully compatible (§2.1.2).
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Integer { short: s1 }, Self::Integer { short: s2 }) => s1 == s2,
            (Self::Real { long: l1 }, Self::Real { long: l2 }) => l1 == l2,
            (Self::Boolean, Self::Boolean) => true,
            (Self::Character, Self::Character) => true,
            (Self::Text, Self::Text) => true,
            (Self::ObjectRef(q1), Self::ObjectRef(q2)) => q1.eq_ignore_ascii_case(q2),
            (
                Self::Array {
                    element: e1,
                    dims: d1,
                },
                Self::Array {
                    element: e2,
                    dims: d2,
                },
            ) => d1 == d2 && e1.is_compatible_with(e2),
            _ => false,
        }
    }

    /// Whether a value of `source` may be assigned to a variable of `target`,
    /// including the Chapter 2 widening rules for arithmetic types.
    pub fn accepts_assignment_from(&self, source: &Self) -> bool {
        match (self, source) {
            (Self::Integer { .. }, Self::Integer { .. }) => true,
            (Self::Real { .. }, Self::Real { .. }) => true,
            (Self::Real { .. }, Self::Integer { .. }) => true,
            (Self::Integer { .. }, Self::Real { .. }) => true,
            (Self::Boolean, Self::Boolean) => true,
            (Self::Character, Self::Character) => true,
            (Self::Text, Self::Text) => true,
            (Self::ObjectRef(target), Self::ObjectRef(source)) => {
                target.eq_ignore_ascii_case(source) || source.eq_ignore_ascii_case("none")
            }
            _ => false,
        }
    }

    /// Whether this is a value type (§2.1): arithmetic, boolean, or character.
    pub fn is_value_type(&self) -> bool {
        matches!(
            self,
            Self::Integer { .. } | Self::Real { .. } | Self::Boolean | Self::Character
        )
    }

    /// Whether this is a reference type (§2.4): object reference or text.
    pub fn is_reference_type(&self) -> bool {
        matches!(self, Self::ObjectRef(_) | Self::Text)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer { short: true } => write!(f, "short integer"),
            Self::Integer { short: false } => write!(f, "integer"),
            Self::Real { long: true } => write!(f, "long real"),
            Self::Real { long: false } => write!(f, "real"),
            Self::Boolean => write!(f, "boolean"),
            Self::Character => write!(f, "character"),
            Self::ObjectRef(qual) => write!(f, "ref({qual})"),
            Self::Text => write!(f, "text"),
            Self::Array { element, dims } => write!(f, "{element} array({dims})"),
        }
    }
}

/// A typed variable declaration (Standard Chapter 2 + declaration syntax from Ch. 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub ty: Type,
    pub items: Vec<DeclarationItem>,
    /// Source span of the full declaration (including terminating `;`).
    pub span: crate::error::Span,
}

/// One name (and optional initializer) in a declaration list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationItem {
    pub name: String,
    pub initializer: Option<crate::ast::Expr>,
    /// `true` when declared via `identifier = expression` (§5.8).
    pub is_constant: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_integer_compatible_with_integer() {
        let short = Type::Integer { short: true };
        let integer = Type::Integer { short: false };
        assert!(integer.accepts_assignment_from(&short));
        assert!(short.accepts_assignment_from(&integer));
    }

    #[test]
    fn long_real_compatible_with_real() {
        let long = Type::Real { long: true };
        let real = Type::Real { long: false };
        assert!(real.accepts_assignment_from(&long));
        assert!(long.accepts_assignment_from(&real));
    }

    #[test]
    fn integer_assignable_to_real() {
        let integer = Type::Integer { short: false };
        let real = Type::Real { long: false };
        assert!(real.accepts_assignment_from(&integer));
    }

    #[test]
    fn real_assignable_to_integer() {
        let integer = Type::Integer { short: false };
        let real = Type::Real { long: false };
        assert!(integer.accepts_assignment_from(&real));
    }

    #[test]
    fn incompatible_types_reject_assignment() {
        let integer = Type::Integer { short: false };
        let boolean = Type::Boolean;
        assert!(!integer.accepts_assignment_from(&boolean));
        assert!(!boolean.accepts_assignment_from(&integer));
    }

    #[test]
    fn ref_qualification_is_case_insensitive() {
        let a = Type::ObjectRef("File".into());
        let b = Type::ObjectRef("file".into());
        assert!(a.is_compatible_with(&b));
        assert!(a.accepts_assignment_from(&b));
    }

    #[test]
    fn display_formats_all_types() {
        assert_eq!(
            format!("{}", Type::Integer { short: true }),
            "short integer"
        );
        assert_eq!(format!("{}", Type::Integer { short: false }), "integer");
        assert_eq!(format!("{}", Type::Real { long: true }), "long real");
        assert_eq!(format!("{}", Type::Real { long: false }), "real");
        assert_eq!(format!("{}", Type::Boolean), "boolean");
        assert_eq!(format!("{}", Type::Character), "character");
        assert_eq!(format!("{}", Type::ObjectRef("Node".into())), "ref(Node)");
        assert_eq!(format!("{}", Type::Text), "text");
    }
}
