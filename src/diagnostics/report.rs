//! Hand-written English for each catalogued situation.

use super::{DiagId, Diagnostic};
use crate::ast::AssignOperator;
use crate::error::{CompileError, Span};
use crate::lex::Keyword;
use crate::types::Type;

use super::typeset::type_english;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    MachineApplicable,
    MaybeIncorrect,
    Unspecified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub message: String,
    pub replacement: Option<String>,
    pub span: Option<Span>,
    pub applicability: Applicability,
}

impl Suggestion {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            replacement: None,
            span: None,
            applicability: Applicability::Unspecified,
        }
    }

    pub fn replace(span: Span, replacement: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            replacement: Some(replacement.into()),
            span: Some(span),
            applicability: Applicability::MachineApplicable,
        }
    }
}

/// Why a type was expected — drives the lead sentence of E0204.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectRole {
    IfCondition,
    WhileCondition,
    NotOperand,
    BooleanOp {
        op: &'static str,
    },
    ArithmeticOp {
        op: &'static str,
    },
    TextConcat,
    CallArgument {
        callee: String,
        index: usize,
        formal: Option<String>,
    },
    ArrayBound,
    SwitchSubscript,
    Generic {
        wanted: &'static str,
    },
}

impl ExpectRole {
    fn lead(&self, found: &str, expected: &str) -> String {
        match self {
            Self::IfCondition => {
                format!("this `if` needs a boolean condition, but this expression is {found}")
            }
            Self::WhileCondition => {
                format!("this `while` needs a boolean condition, but this expression is {found}")
            }
            Self::NotOperand => {
                format!("`not` only applies to boolean, but this expression is {found}")
            }
            Self::BooleanOp { op } => {
                format!("`{op}` needs boolean operands, but this expression is {found}")
            }
            Self::ArithmeticOp { op } => {
                format!("`{op}` needs arithmetic operands, but this expression is {found}")
            }
            Self::TextConcat => format!(
                "text concatenation (`&`) needs text operands, but this expression is {found}"
            ),
            Self::CallArgument {
                callee,
                index,
                formal,
            } => {
                let nth = ordinal(*index);
                match formal {
                    Some(name) => format!(
                        "the {nth} argument `{name}` to `{callee}` needs {expected}, but this is {found}"
                    ),
                    None => format!(
                        "the {nth} argument to `{callee}` needs {expected}, but this is {found}"
                    ),
                }
            }
            Self::ArrayBound => format!("array bounds must be arithmetic, but this is {found}"),
            Self::SwitchSubscript => {
                format!("a switch designator needs an integer subscript, but this is {found}")
            }
            Self::Generic { wanted } => {
                format!("this position needs {wanted}, but this expression is {found}")
            }
        }
    }

    fn hint(&self, expected: &str) -> Option<String> {
        match self {
            Self::IfCondition | Self::WhileCondition => {
                Some("write a boolean (`true` / `false`) or a relation such as `x > 0`".into())
            }
            Self::NotOperand | Self::BooleanOp { .. } => {
                Some("use a boolean expression, or compare with a relation".into())
            }
            Self::ArithmeticOp { .. } => Some("use an integer or real expression".into()),
            Self::TextConcat => Some("use text values, or convert with `PutInt` / similar".into()),
            Self::CallArgument { .. } => Some(format!("pass a {expected} expression")),
            Self::ArrayBound => Some("use an integer or real bound expression".into()),
            Self::SwitchSubscript => Some("use a positive integer subscript".into()),
            Self::Generic { wanted } => Some(format!("provide a {wanted} expression")),
        }
    }

    fn role_param(&self) -> &'static str {
        match self {
            Self::IfCondition => "if-condition",
            Self::WhileCondition => "while-condition",
            Self::NotOperand => "not-operand",
            Self::BooleanOp { .. } => "boolean-op",
            Self::ArithmeticOp { .. } => "arithmetic-op",
            Self::TextConcat => "text-concat",
            Self::CallArgument { .. } => "call-arg",
            Self::ArrayBound => "array-bound",
            Self::SwitchSubscript => "switch-subscript",
            Self::Generic { .. } => "expression",
        }
    }
}

fn ordinal(index: usize) -> String {
    let n = index + 1;
    let suffix = match n % 100 {
        11 | 12 | 13 => "th",
        _ => match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{n}{suffix}")
}

fn finish(diag: Diagnostic) -> CompileError {
    diag.into_error()
}

pub fn unexpected_character(ch: char, span: Span) -> CompileError {
    let display = if ch.is_control() {
        format!("U+{:04X}", ch as u32)
    } else {
        ch.to_string()
    };
    finish(
        Diagnostic::new(
            DiagId::UnexpectedCharacter,
            format!("`{display}` is not a legal character here"),
        )
        .at(span)
        .primary("unexpected character")
        .help("remove it, or put it inside a string or a comment")
        .param("character", display),
    )
}

pub fn unterminated_string(span: Span) -> CompileError {
    finish(
        Diagnostic::new(DiagId::UnterminatedString, "this string never ended")
            .at(span)
            .primary("string starts here")
            .help("add a closing `\"` before the end of the line")
            .note("string literals cannot cross a newline (Standard §1.6)"),
    )
}

pub fn directive_not_at_column_zero(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::DirectiveNotAtColumnZero,
            "a `%` directive must be the first character of the line",
        )
        .at(span)
        .primary("not at the start of the line")
        .help("move this `%` to column 0, or write a comment instead"),
    )
}

pub fn invalid_number(message: impl Into<String>, span: Span) -> CompileError {
    finish(
        Diagnostic::new(DiagId::InvalidNumber, message)
            .at(span)
            .primary("this number")
            .help("check the digits, the `.` fraction, and any `&` / `&&` exponent")
            .note("unsigned numbers follow Standard §1.5"),
    )
}

pub fn invalid_iso_code(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::InvalidIsoCode,
            "this ISO character code is not a valid character",
        )
        .at(span)
        .primary("ISO-code")
        .help("use `!ddd!` with `0 ≤ ddd < 256`"),
    )
}

pub fn missing_token_separator(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::MissingTokenSeparator,
            "these two tokens need a space (or another separator) between them",
        )
        .at(span)
        .primary("missing separator")
        .help("insert a space or a newline")
        .note("identifiers, keywords, numbers, and strings must be separated (§1.9)"),
    )
}

pub fn unexpected_token(
    found: &str,
    expected: Option<String>,
    span: Span,
    context: &[String],
) -> CompileError {
    let message = match &expected {
        Some(exp) => format!("{exp}, but found {found}"),
        None => format!("found {found} here"),
    };
    let mut diag = Diagnostic::new(DiagId::UnexpectedToken, message)
        .at(span)
        .primary("unexpected token")
        .param("found", found);
    if let Some(exp) = &expected {
        diag = diag.note(exp.clone()).param("expected", exp.clone());
    }
    for ctx in context.iter().take(2) {
        diag = diag.note(format!("while parsing {ctx}"));
    }
    finish(diag)
}

pub fn unexpected_eof(expected: Option<String>) -> CompileError {
    let message = match &expected {
        Some(exp) => format!("{exp}, but the file ended"),
        None => "the file ended while a construct was still open".to_string(),
    };
    let mut diag = Diagnostic::new(DiagId::UnexpectedEof, message)
        .help("check for a missing `end`, `)`, or `;` before the end of the file");
    if let Some(exp) = expected {
        diag = diag.note(exp);
    }
    finish(diag)
}

pub fn missing_end(span: Option<Span>, begin_span: Option<Span>) -> CompileError {
    let mut diag = Diagnostic::new(DiagId::MissingEnd, "this block is missing `end`")
        .help("add `end` to close the `begin`");
    if let Some(span) = span {
        diag = diag.at(span).primary("expected `end` here");
    }
    if let Some(begin) = begin_span {
        diag = diag.label(begin, "this `begin` was never closed");
    }
    finish(diag)
}

pub fn wrong_assign_operator(found: AssignOperator, span: Span) -> CompileError {
    let (found_op, expected_op) = match found {
        AssignOperator::AssignAlt => ("`:-`", "`:=`"),
        AssignOperator::Assign => ("`:=`", "`:-`"),
    };
    finish(
        Diagnostic::new(
            DiagId::WrongAssignOperator,
            format!("expected {expected_op}, but found {found_op}"),
        )
        .at(span.clone())
        .note("`:=` assigns values; `:-` assigns object references and text")
        .help(format!("write {expected_op} here"))
        .suggest(Suggestion::replace(
            span,
            expected_op.trim_matches('`'),
            format!("replace {found_op} with {expected_op}"),
        )),
    )
}

pub fn incomplete_type_prefix(prefix: Keyword, needed: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::IncompleteTypePrefix,
            format!("`{}` must be followed by {needed}", prefix.as_str()),
        )
        .at(span)
        .primary("incomplete type")
        .help(format!("write `{} {needed}`", prefix.as_str())),
    )
}

pub fn type_mismatch_assign(
    operator: AssignOperator,
    found: &Type,
    expected: &Type,
    found_span: Span,
    assign_span: Span,
) -> CompileError {
    let found_s = type_english(found);
    let expected_s = type_english(expected);
    let op = match operator {
        AssignOperator::Assign => "`:=`",
        AssignOperator::AssignAlt => "`:-`",
    };
    let mut diag = Diagnostic::new(
        DiagId::TypeMismatchAssign,
        format!("this assignment needs {expected_s}, but the value is {found_s}"),
    )
    .at(found_span.clone())
    .primary(format!("this is {found_s}"))
    .note(format!("destination expects {expected_s}"))
    .help("change the expression, or declare the variable with a matching type")
    .param("found", &found_s)
    .param("expected", &expected_s)
    .param("operator", op);
    if assign_span != found_span {
        diag = diag.label(
            assign_span,
            format!("in this {op} assignment (expects {expected_s})"),
        );
    }
    finish(diag)
}

pub fn value_assign_to_ref(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::ValueAssignToRef,
            "value assignment (`:=`) cannot target an object reference",
        )
        .at(span)
        .primary("this is a `ref` destination")
        .note("`:=` copies values; object references are bound with `:-` (Standard §3.6)")
        .help("write `:-` instead of `:=`")
        .suggest(Suggestion::message("replace `:=` with `:-`")),
    )
}

pub fn ref_assign_to_value(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::RefAssignToValue,
            "reference assignment (`:-`) needs a `ref` or `text` on both sides",
        )
        .at(span)
        .primary("this is not a reference")
        .help("write `:=` for integer, real, boolean, or character")
        .suggest(Suggestion::message("replace `:-` with `:=`")),
    )
}

pub fn type_mismatch(role: ExpectRole, found: &Type, expected: &Type, span: Span) -> CompileError {
    let found_s = type_english(found);
    let expected_s = type_english(expected);
    let lead = role.lead(&found_s, &expected_s);
    let mut diag = Diagnostic::new(DiagId::TypeMismatch, lead)
        .at(span)
        .primary(format!("this is {found_s}"))
        .param("found", &found_s)
        .param("expected", &expected_s)
        .param("role", role.role_param());
    if let Some(hint) = role.hint(&expected_s) {
        diag = diag.help(hint);
    }
    finish(diag)
}

pub fn plus_on_text(example: &str, span: Span, plus_span: Option<Span>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::TypeMismatch,
        "`+` adds numbers; to concatenate text, use `&`",
    )
    .at(span)
    .primary(format!("try `{example}`"))
    .note("text concatenation is `&` (Standard §3.5)")
    .help(format!("write `{example}`"))
    .param("found", "text")
    .param("expected", "arithmetic")
    .param("role", "text-plus")
    .param("example", example);
    diag = match plus_span {
        Some(plus_span) => diag.suggest(Suggestion::replace(
            plus_span,
            " & ",
            "replace `+` with `&`",
        )),
        None => diag.suggest(Suggestion::message("replace `+` with `&`")),
    };
    finish(diag)
}

pub fn arity_mismatch(callee: &str, expected: usize, found: usize, span: Span) -> CompileError {
    let noun = if expected == 1 {
        "argument"
    } else {
        "arguments"
    };
    finish(
        Diagnostic::new(
            DiagId::ArityMismatch,
            format!("`{callee}` expects {expected} {noun}, but this call has {found}"),
        )
        .at(span)
        .primary("this call")
        .help(format!("pass exactly {expected} {noun}"))
        .param("callee", callee)
        .param("expected", expected.to_string())
        .param("found", found.to_string()),
    )
}

/// Which arm of `if … then … else …` failed to match a known result type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfBranch {
    Then,
    Else,
}

impl IfBranch {
    fn english(self) -> &'static str {
        match self {
            Self::Then => "`then`",
            Self::Else => "`else`",
        }
    }
}

pub fn incompatible_branches(then_ty: &Type, else_ty: &Type, span: Span) -> CompileError {
    let then_s = type_english(then_ty);
    let else_s = type_english(else_ty);
    finish(
        Diagnostic::new(
            DiagId::IncompatibleBranches,
            format!("the `then` branch is {then_s}, but the `else` branch is {else_s}"),
        )
        .at(span)
        .primary("this branch")
        .note("both branches of a conditional expression must have a common type")
        .help("change one branch, or split the `if` into statements")
        .param("then", then_s)
        .param("else", else_s),
    )
}

/// A conditional is used as a known type (assignment destination, argument, …)
/// and one branch does not yield that type.
pub fn if_branch_should_be(
    branch: IfBranch,
    found: &Type,
    expected: &Type,
    span: Span,
) -> CompileError {
    let found_s = type_english(found);
    let expected_s = type_english(expected);
    let branch_s = branch.english();
    finish(
        Diagnostic::new(
            DiagId::IncompatibleBranches,
            format!("this {branch_s} branch is {found_s}, but it should be {expected_s}"),
        )
        .at(span)
        .primary(format!("this is {found_s}"))
        .note(format!(
            "this `if` is used where {expected_s} is required, so both branches must yield {expected_s}"
        ))
        .help(format!("change this branch so it yields {expected_s}"))
        .param("found", &found_s)
        .param("expected", &expected_s)
        .param("branch", branch_s.trim_matches('`')),
    )
}

pub fn unknown_name(name: &str, span: Span, suggestion: Option<&str>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::UnknownName,
        format!("I cannot find a declaration for `{name}`"),
    )
    .at(span.clone())
    .primary("this name")
    .help("declare it in an enclosing block, or pass it as a parameter")
    .note("names are matched without regard to case")
    .param("name", name);
    if let Some(hint) = suggestion {
        diag = diag
            .help(format!("did you mean `{hint}`?"))
            .suggest(Suggestion::replace(
                span,
                hint,
                format!("replace `{name}` with `{hint}`"),
            ))
            .param("suggestion", hint);
    }
    finish(diag)
}

pub fn unknown_procedure(name: &str, span: Span, suggestion: Option<&str>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::UnknownProcedure,
        format!("I cannot find a procedure named `{name}`"),
    )
    .at(span.clone())
    .help("declare it, import it with `external procedure`, or prefix the block with a class that provides it")
    .note("names are matched without regard to case")
    .param("name", name);
    if let Some(hint) = suggestion {
        diag = diag
            .help(format!("did you mean `{hint}`?"))
            .suggest(Suggestion::replace(
                span,
                hint,
                format!("replace `{name}` with `{hint}`"),
            ))
            .param("suggestion", hint);
    }
    finish(diag)
}

pub fn unknown_attribute(
    class_name: &str,
    attribute: &str,
    kind: &str,
    span: Option<Span>,
    suggestion: Option<&str>,
) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::UnknownAttribute,
        format!("class `{class_name}` has no {kind} `{attribute}` visible here"),
    )
    .help("check the class heading, `hidden` / `protected`, and spelling")
    .param("class", class_name)
    .param("attribute", attribute);
    if let Some(span) = span {
        diag = diag.at(span).primary("this name");
    }
    if let Some(hint) = suggestion {
        diag = diag.help(format!("did you mean `{hint}`?"));
    }
    finish(diag)
}

pub fn undefined_label(name: &str, span: Option<Span>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::UndefinedLabel,
        format!("there is no label `{name}` visible here"),
    )
    .help("declare the label in this block, or pass a `label` parameter")
    .param("name", name);
    if let Some(span) = span {
        diag = diag.at(span).primary("this label");
    }
    finish(diag)
}

pub fn undefined_switch(name: &str, span: Option<Span>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::UndefinedSwitch,
        format!("there is no switch `{name}` visible here"),
    )
    .help("declare `switch {name} := …` in an enclosing block")
    .param("name", name);
    if let Some(span) = span {
        diag = diag.at(span).primary("this switch");
    }
    finish(diag)
}

pub fn duplicate_declaration(name: &str, span: Span, first: Option<Span>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::DuplicateDeclaration,
        format!("`{name}` is already declared in this block"),
    )
    .at(span)
    .primary("second declaration")
    .help("rename one of the declarations, or move one into a nested block")
    .param("name", name);
    if let Some(first) = first {
        diag = diag.label(first, "first declared here");
    }
    finish(diag)
}

pub fn unused_binding(kind: &str, name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(DiagId::UnusedBinding, format!("unused {kind} `{name}`"))
            .at(span)
            .primary("never referenced")
            .help("remove it, or use it")
            .param("name", name)
            .param("kind", kind),
    )
}

pub fn array_extent_overflow() -> CompileError {
    finish(
        Diagnostic::new(DiagId::ArrayExtentOverflow, "array extent overflow")
            .help("narrow the bounds, or split the array into smaller pieces"),
    )
}

pub fn none_dereference(message: &str) -> CompileError {
    finish(
        Diagnostic::new(DiagId::NoneDereference, message)
            .help("`none` has no attributes; test `x =/= none` before a remote access"),
    )
}

pub fn undefined_label_runtime(name: &str) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::UndefinedLabelRuntime,
            format!("undefined label '{name}'"),
        )
        .help("the `goto` target was not found on the call stack")
        .param("name", name),
    )
}

pub fn array_subscript(index: i64, lo: i64, hi: i64) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::ArraySubscript,
            format!("subscript {index} is outside [{lo}:{hi}]"),
        )
        .help("use an index in the declared bounds")
        .param("index", index.to_string())
        .param("low", lo.to_string())
        .param("high", hi.to_string()),
    )
}

pub fn array_subscript_count(expected: usize, found: usize) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::ArraySubscript,
            format!("this array expects {expected} subscripts, but the access has {found}"),
        )
        .help(format!("write {expected} indices"))
        .param("expected", expected.to_string())
        .param("found", found.to_string()),
    )
}

pub fn division_by_zero() -> CompileError {
    finish(
        Diagnostic::new(DiagId::DivisionByZero, "integer division by zero")
            .help("test that the divisor is not 0 before dividing"),
    )
}

pub fn exponentiation_undefined() -> CompileError {
    finish(
        Diagnostic::new(DiagId::ExponentiationUndefined, "exponentiation undefined")
            .help("use a positive base, or an integer exponent on a negative base"),
    )
}

pub fn protected_attribute(
    attribute: &str,
    span: Option<Span>,
    spec_span: Option<Span>,
) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::ProtectedAttribute,
        format!("protected attribute `{attribute}` is not accessible from this context"),
    )
    .help("access it from the class body, a subclass, or an `inspect` of that class")
    .param("attribute", attribute);
    if let Some(span) = span {
        diag = diag.at(span).primary("this access");
    }
    if let Some(spec) = spec_span {
        diag = diag.label(spec, "this `protected` specification");
    }
    finish(diag)
}

pub fn hidden_attribute(
    attribute: &str,
    access_class: &str,
    span: Option<Span>,
    spec_span: Option<Span>,
) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::HiddenAttribute,
        format!("hidden attribute `{attribute}` is not visible in class `{access_class}`"),
    )
    .help("use a public name, or access the object at a prefix that still sees this attribute")
    .param("attribute", attribute)
    .param("class", access_class);
    if let Some(span) = span {
        diag = diag.at(span).primary("this access");
    }
    if let Some(spec) = spec_span {
        diag = diag.label(spec, "this `hidden` specification");
    }
    finish(diag)
}

pub fn illegal_param_mode(class: &str, param: &str, message: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::IllegalParamMode,
            format!("class `{class}` parameter `{param}`: {message}"),
        )
        .at(span)
        .note("class parameters cannot be transmitted by name; use `value` for arithmetic/boolean/character; object references, `text`, and arrays default to transmission by reference")
        .param("class", class)
        .param("param", param),
    )
}

pub fn simulation_not_active() -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::SimulationNotActive,
            "`hold` / `activate` / `time` require an active Simulation",
        )
        .help("prefix the program or enclosing block with `Simulation`"),
    )
}

pub fn linker_failed(target: &str, summary: impl Into<String>) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::LinkerFailed,
            format!("linker failed for target {target}"),
        )
        .note(summary.into())
        .help("install a C toolchain / SDK, or set SIM_LINKER")
        .param("target", target),
    )
}

pub fn linker_not_found(how: impl Into<String>) -> CompileError {
    finish(Diagnostic::new(DiagId::LinkerNotFound, "no host linker was found").help(how.into()))
}

fn at_opt(diag: Diagnostic, span: Option<Span>) -> Diagnostic {
    match span {
        Some(span) => diag.at(span),
        None => diag,
    }
}

pub fn empty_array_bounds(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::EmptyArrayBounds,
            "an array declaration needs at least one bound pair",
        )
        .at(span)
        .help("write bounds such as `a(1:n)`"),
    )
}

pub fn array_bound_name(message: impl Into<String>, span: Span) -> CompileError {
    finish(
        Diagnostic::new(DiagId::ArrayBoundName, message)
            .at(span)
            .primary("this bound")
            .help("use a simple identifier from an enclosing block, or a constant"),
    )
}

pub fn empty_switch(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::EmptySwitch,
            format!("switch `{name}` needs at least one designational expression"),
        )
        .at(span)
        .help("write `switch {name} := L1, L2, …`")
        .param("name", name),
    )
}

pub fn statement_as_expression(name: &str, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::StatementAsExpression,
            format!("`{name}` is a statement and cannot be used as an expression"),
        )
        .help("call it as a statement, or use a typed procedure that returns a value")
        .param("name", name),
        span,
    ))
}

pub fn assign_to_constant(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::AssignToConstant,
            format!("cannot assign to constant `{name}`"),
        )
        .at(span)
        .help("assign to a variable, or change the declaration so it is not constant")
        .param("name", name),
    )
}

pub fn constant_initializer(message: impl Into<String>, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(DiagId::ConstantInitializer, message)
            .help("use a compile-time constant expression of identifiers from outer scope"),
        span,
    ))
}

pub fn prefix_cycle(name: &str, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::PrefixCycle,
            format!("class `{name}` occurs in its own prefix sequence"),
        )
        .help("remove the cycle so each class prefixes a strictly outer class")
        .param("name", name),
        span,
    ))
}

pub fn undefined_class(name: &str, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::UndefinedClass,
            format!("I cannot find a class named `{name}`"),
        )
        .help("declare the class, or import it with `external class`")
        .param("name", name),
        span,
    ))
}

pub fn prefix_not_local(prefix: &str, class: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::PrefixNotLocal,
            format!("prefix class `{prefix}` is not local to the block declaring class `{class}`"),
        )
        .at(span)
        .help("declare it in the same block, or list it in an `external` head")
        .param("prefix", prefix)
        .param("class", class),
    )
}

pub fn virtual_mismatch(kind: &str, name: &str, class: &str, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::VirtualMismatch,
            format!("virtual {kind} `{name}` heading does not match in class `{class}`"),
        )
        .help(
            "make the virtual specification and the matching attribute or procedure heading agree",
        )
        .param("name", name)
        .param("class", class),
        span,
    ))
}

pub fn illegal_this(span: Span) -> CompileError {
    finish(
        Diagnostic::new(DiagId::IllegalThis, "`this` is illegal in a block prefix")
            .at(span)
            .help("name the prefix class, or use `this` only inside a class body"),
    )
}

pub fn not_prefix_class(named: &str, current: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::NotPrefixClass,
            format!("`{named}` is not a prefix class of `{current}`"),
        )
        .at(span)
        .help("`this`, `qua`, `is`, and `in` need a class on the object's prefix chain")
        .param("named", named)
        .param("current", current),
    )
}

pub fn hidden_requires_protected(name: &str, class: &str, span: Option<Span>) -> CompileError {
    let mut diag = Diagnostic::new(
        DiagId::HiddenRequiresProtected,
        format!("only a protected attribute may be specified `hidden`: `{name}` is not protected in class `{class}`"),
    )
    .help("write `protected hidden`, or protect the attribute in a prefix class")
    .param("name", name)
    .param("class", class);
    if let Some(span) = span {
        diag = diag.at(span).primary("this `hidden` specification");
    }
    finish(diag)
}

pub fn duplicate_virtual(name: &str, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::DuplicateVirtual,
            format!("duplicate virtual quantity `{name}`"),
        )
        .help("list each virtual name once in the class heading")
        .param("name", name),
        span,
    ))
}

pub fn detach_needs_object(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::DetachNeedsObject,
            "`detach` requires object context",
        )
        .at(span)
        .help("call `detach` from a class body, or as a remote procedure on an object"),
    )
}

pub fn attribute_not_visible(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::AttributeNotVisible,
            format!("attribute `{name}` is not visible outside the class body"),
        )
        .at(span)
        .help("access it as `obj.{name}`, or from inside the class")
        .param("name", name),
    )
}

pub fn duplicate_formal(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::DuplicateFormal,
            format!("duplicate formal parameter `{name}`"),
        )
        .at(span)
        .primary("second occurrence")
        .help("rename one of the formals")
        .param("name", name),
    )
}

pub fn procedure_name_as_formal(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::ProcedureNameAsFormal,
            format!("procedure identifier `{name}` cannot appear in its own formal parameter list"),
        )
        .at(span)
        .help("choose a different formal name")
        .param("name", name),
    )
}

pub fn formal_redeclared(name: &str, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::FormalRedeclared,
            format!("formal parameter `{name}` cannot be redeclared in the procedure body head"),
        )
        .help("remove the inner declaration, or rename it")
        .param("name", name),
        span,
    ))
}

pub fn formal_array_arity(
    name: &str,
    previous: usize,
    arity: usize,
    span: Option<Span>,
) -> CompileError {
    finish(at_opt(
        Diagnostic::new(
            DiagId::FormalArrayArity,
            format!(
                "formal array `{name}` is subscripted with both {previous} and {arity} indices"
            ),
        )
        .help("use the same number of subscripts at every access")
        .param("name", name),
        span,
    ))
}

pub fn formal_not_visible(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::FormalNotVisible,
            format!("formal parameter `{name}` is not visible outside its procedure or class body"),
        )
        .at(span)
        .help("pass it as an argument, or access an attribute through an object")
        .param("name", name),
    )
}

pub fn non_simula_formal_proc(name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::NonSimulaFormalProc,
            format!("non-Simula procedure `{name}` cannot be used as a formal procedure actual"),
        )
        .at(span)
        .help("pass a Simula procedure, not a C/JS/Host import")
        .param("name", name),
    )
}

pub fn unknown_external_kind(kind: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::UnknownExternalKind,
            format!("unknown external kind `{kind}`; sim recognises C, JS, and Host"),
        )
        .at(span)
        .help("write `external C procedure`, `external JS procedure`, or `external Host procedure`")
        .param("kind", kind),
    )
}

pub fn missing_external_spec(kind: &str, name: &str, span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::MissingExternalSpec,
            format!(
                "external {kind} procedure `{name}` requires a specification (`is procedure …`)"
            ),
        )
        .at(span)
        .help("add `is procedure …;` with an empty body")
        .param("kind", kind)
        .param("name", name),
    )
}

pub fn external_body_not_empty(span: Span) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::ExternalBodyNotEmpty,
            "external procedure specification must have an empty body",
        )
        .at(span)
        .help("write `is procedure P; begin end;` — the body is supplied by the host"),
    )
}

pub fn foreign_boundary(message: impl Into<String>, span: Span) -> CompileError {
    finish(
        Diagnostic::new(DiagId::ForeignBoundary, message)
            .at(span)
            .help("keep formal procedures, labels, switches, and name parameters on the Simula side; transmit `text` by value"),
    )
}

pub fn not_lowered(detail: impl Into<String>, span: Option<Span>) -> CompileError {
    let detail = detail.into();
    finish(at_opt(
        Diagnostic::new(DiagId::NotLowered, detail)
            .help("use `sim run` (interpreter), or prefix the block with `Simulation` if this is a sequencing procedure"),
        span,
    ))
}

pub fn toolchain(summary: impl Into<String>, span: Option<Span>) -> CompileError {
    finish(at_opt(
        Diagnostic::new(DiagId::Toolchain, "the host toolchain could not complete this build")
            .note(summary.into())
            .help("install a C compiler / SDK (`xcode-select --install`, `build-essential`, or Visual Studio Build Tools), or set `SIM_LINKER`"),
        span,
    ))
}

pub fn ice(detail: impl Into<String>) -> CompileError {
    finish(
        Diagnostic::new(
            DiagId::InternalError,
            "the compiler hit an unexpected internal condition",
        )
        .note(detail.into())
        .help("this is a sim bug, not a mistake in your Simula; please file a report"),
    )
}
