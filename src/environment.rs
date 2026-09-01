//! Registry for Simula Standard Chapter 9 ENVIRONMENT builtins.

use crate::types::Type;

const ENVIRONMENT_PROCEDURES: &[&str] = &[
    // §9.1 Basic operations
    "mod",
    "rem",
    "abs",
    "sign",
    "entier",
    "addepsilon",
    "subepsilon",
    // §9.2 Text utilities
    "char",
    "isochar",
    "rank",
    "isorank",
    "digit",
    "letter",
    "lowten",
    "decimalmark",
    "upcase",
    "lowcase",
    // §9.4 Mathematical functions
    "sqrt",
    "sin",
    "cos",
    "tan",
    "cotan",
    "arcsin",
    "arccos",
    "arctan",
    "arctan2",
    "sinh",
    "cosh",
    "tanh",
    "ln",
    "log10",
    "exp",
    // §9.5 Extremum functions
    "max",
    "min",
    // §9.6 Environmental enquiries
    "sourceline",
    // §9.7 Error control
    "error",
    // §9.8 Array quantities
    "lowerbound",
    "upperbound",
    // §9.9 Random drawing
    "draw",
    "randint",
    "uniform",
    "normal",
    "negexp",
    "poisson",
    "erlang",
    "discrete",
    "linear",
    "histd",
    // §9.10 Calendar and timing
    "datetime",
    "cputime",
    "clocktime",
    // §9.11 Miscellaneous
    "histo",
    // §9.3 / Ch.7 coroutine sequencing
    "call",
    "resume",
];

const ENVIRONMENT_CONSTANTS: &[&str] = &[
    "maxrank",
    "maxint",
    "minint",
    "maxreal",
    "minreal",
    "maxlongreal",
    "minlongreal",
    "simulaid",
];

/// Whether `name` is a standard ENVIRONMENT procedure (case-insensitive).
pub fn is_environment_procedure(name: &str) -> bool {
    ENVIRONMENT_PROCEDURES
        .iter()
        .any(|proc| proc.eq_ignore_ascii_case(name))
}

/// Whether `name` is a standard ENVIRONMENT constant (case-insensitive).
pub fn is_environment_constant(name: &str) -> bool {
    ENVIRONMENT_CONSTANTS
        .iter()
        .any(|constant| constant.eq_ignore_ascii_case(name))
}

/// Standard ENVIRONMENT procedure names (lowercase spellings).
pub fn environment_procedures() -> &'static [&'static str] {
    ENVIRONMENT_PROCEDURES
}

/// Standard ENVIRONMENT constant names (lowercase spellings).
pub fn environment_constants() -> &'static [&'static str] {
    ENVIRONMENT_CONSTANTS
}

/// Whether the ENVIRONMENT procedure returns a value usable as an expression.
pub fn environment_procedure_returns_value(name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "error" | "histo" | "call" | "resume" => false,
        _ => is_environment_procedure(name),
    }
}

/// Types of ENVIRONMENT constants for semantic analysis.
pub fn environment_constant_type(name: &str) -> Option<Type> {
    match name.to_ascii_lowercase().as_str() {
        "maxrank" | "maxint" | "minint" => Some(Type::Integer { short: false }),
        "maxreal" | "minreal" => Some(Type::Real { long: false }),
        "maxlongreal" | "minlongreal" => Some(Type::Real { long: true }),
        "simulaid" => Some(Type::Text),
        _ => None,
    }
}

/// Static result type hints for semantic analysis when inference is unambiguous.
pub fn builtin_result_type(name: &str) -> Option<Type> {
    match name.to_ascii_lowercase().as_str() {
        // §9.1
        "mod" | "rem" | "sign" | "entier" => Some(Type::Integer { short: false }),
        // §9.2
        "char" | "isochar" | "lowten" | "decimalmark" => Some(Type::Character),
        "rank" | "isorank" => Some(Type::Integer { short: false }),
        "digit" | "letter" => Some(Type::Boolean),
        // §9.4 — long real when any parameter is long; default to real.
        "sqrt" | "sin" | "cos" | "tan" | "cotan" | "arcsin" | "arccos" | "arctan" | "arctan2"
        | "sinh" | "cosh" | "tanh" | "ln" | "log10" | "exp" => Some(Type::Real { long: false }),
        // §9.5 — type depends on operands
        "max" | "min" | "abs" => None,
        // §9.6
        "sourceline" => Some(Type::Integer { short: false }),
        // §9.8
        "lowerbound" | "upperbound" => Some(Type::Integer { short: false }),
        // §9.9
        "draw" => Some(Type::Boolean),
        "randint" | "discrete" | "histd" | "poisson" => Some(Type::Integer { short: false }),
        "uniform" | "normal" | "negexp" | "erlang" | "linear" => Some(Type::Real { long: true }),
        // §9.10
        "datetime" => Some(Type::Text),
        "cputime" | "clocktime" => Some(Type::Real { long: true }),
        // Statements and polymorphic builtins
        "error" | "histo" | "call" | "resume" | "addepsilon" | "subepsilon" | "upcase"
        | "lowcase" => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_environment_procedures() {
        assert!(is_environment_procedure("mod"));
        assert!(is_environment_procedure("RANDINT"));
        assert!(is_environment_procedure("Poisson"));
        assert!(!is_environment_procedure("OutText"));
    }

    #[test]
    fn recognizes_environment_constants() {
        assert!(is_environment_constant("maxint"));
        assert!(is_environment_constant("SIMULAID"));
        assert!(!is_environment_constant("pi"));
    }

    #[test]
    fn statement_procedures_do_not_return_values() {
        assert!(!environment_procedure_returns_value("error"));
        assert!(!environment_procedure_returns_value("histo"));
        assert!(environment_procedure_returns_value("draw"));
    }

    #[test]
    fn provides_result_type_hints() {
        assert_eq!(
            builtin_result_type("mod"),
            Some(Type::Integer { short: false })
        );
        assert_eq!(builtin_result_type("draw"), Some(Type::Boolean));
        assert_eq!(builtin_result_type("datetime"), Some(Type::Text));
        assert_eq!(builtin_result_type("max"), None);
    }
}
