//! Submodule of [`crate::mir::lower`].

use super::*;

pub(in crate::mir::lower) fn block_is_decl_prefix_only(block: &Block) -> bool {
    block.prefix.is_none()
        && block.body.is_empty()
        && block.statements.is_empty()
        && block.arrays.is_empty()
        && block.switches.is_empty()
        && block.externals.is_empty()
        && (!block.declarations.is_empty()
            || !block.classes.is_empty()
            || !block.procedures.is_empty())
}

pub(in crate::mir::lower) fn spanned_error(message: impl Into<String>, span: Span) -> CompileError {
    CompileError::codegen_at(message, span)
}

/// Class-identifier on the right of `is` / `in` (Standard §3.3.4).
pub(in crate::mir::lower) fn class_identifier_from_expr(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => Some(name.as_str()),
        ExprKind::Paren(inner) => class_identifier_from_expr(inner),
        _ => None,
    }
}

/// Simulation builtins / statements not yet lowered for native/wasm.
/// Supported: `hold`, `passivate`, `cancel`, `time`, `current`, `wait`, and
/// `activate`/`reactivate` (direct / delay / at / before / after).
pub(in crate::mir::lower) fn is_deferred_scheduling_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_simulation_builtin(name)
        && !matches!(
            lower.as_str(),
            "hold" | "passivate" | "cancel" | "time" | "current" | "wait"
        )
}

pub(in crate::mir::lower) fn scheduling_unsupported_error(name: &str, span: Span) -> CompileError {
    crate::diagnostics::not_lowered(
        format!(
            "`{name}` is not compiled for native or wasm yet; use `sim run`. \
             Simulation sequencing (`hold`, `passivate`, `cancel`, `time`, `current`, \
             `wait`, `activate`) needs a `Simulation` prefix."
        ),
        Some(span),
    )
}

pub(in crate::mir::lower) fn lower_filesystem_text_arg(
    builder: &mut FunctionBuilder<'_>,
    expr: &Expr,
    what: &str,
) -> Result<LocalId, CompileError> {
    let value = builder.lower_expr(expr)?;
    if builder.local_ty(value) != MirType::Text {
        return Err(spanned_error(
            format!("{what} requires a text argument"),
            expr.span.clone(),
        ));
    }
    Ok(value)
}

/// Simula `&` / `&&` exponent marks → Rust `e` for `f64` parsing.
/// Bare `&n` / `+&n` / `-&n` (no significand) means `1×10^n` (lowten notation).
pub(in crate::mir::lower) fn normalize_real_lexeme(lexeme: &str) -> String {
    let cleaned = lexeme.replace('_', "");
    let with_e = cleaned.replace("&&", "e").replace('&', "e");
    // `e3`, `+e3`, `-e3` → `1e3` / `-1e3`
    if let Some(rest) = with_e.strip_prefix('+') {
        if rest.starts_with('e') || rest.starts_with('E') {
            return format!("1{rest}");
        }
        return with_e;
    }
    if let Some(rest) = with_e.strip_prefix('-') {
        if rest.starts_with('e') || rest.starts_with('E') {
            return format!("-1{rest}");
        }
        return with_e;
    }
    if with_e.starts_with('e') || with_e.starts_with('E') {
        return format!("1{with_e}");
    }
    with_e
}

/// Extracts an assignable [`Variable`] from a call-by-name actual expression.
///
/// Expression-position `a(i)` parses as [`ExprKind::FunctionCall`] (parens),
/// while assignment LHSes use [`Variable::Subscripted`]. Treat call form the
/// same way MIR/interpreter resolve the array/procedure ambiguity on reads.
pub(in crate::mir::lower) fn variable_from_name_actual(
    expr: &Expr,
    span: Span,
) -> Result<Variable, CompileError> {
    match &expr.kind {
        ExprKind::Variable(variable) => Ok(variable.clone()),
        ExprKind::FunctionCall { name, arguments } => Ok(Variable::Subscripted {
            name: name.clone(),
            subscripts: arguments.clone(),
        }),
        ExprKind::Paren(inner) => variable_from_name_actual(inner, span),
        _ => Err(spanned_error(
            "MIR lowering: call-by-name assignment requires a variable actual parameter",
            span,
        )),
    }
}

/// Formal procedure actuals must be a simple procedure identifier.
pub(in crate::mir::lower) fn procedure_identifier_actual(
    expr: &Expr,
) -> Result<String, CompileError> {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => Ok(name.clone()),
        ExprKind::Paren(inner) => procedure_identifier_actual(inner),
        _ => Err(spanned_error(
            "formal procedure actual parameter must be a procedure identifier",
            expr.span.clone(),
        )),
    }
}

/// LABEL formal actual: label name, switch designator, or designational `if`.
pub(in crate::mir::lower) fn expr_as_designational(
    expr: &Expr,
) -> Result<DesignationalExpr, CompileError> {
    match &expr.kind {
        ExprKind::Paren(inner) => expr_as_designational(inner),
        ExprKind::Variable(Variable::Simple(name)) => Ok(DesignationalExpr::Label(name.clone())),
        ExprKind::FunctionCall { name, arguments } if arguments.len() == 1 => {
            Ok(DesignationalExpr::SwitchDesignator {
                name: name.clone(),
                subscript: Box::new(arguments[0].clone()),
            })
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => Ok(DesignationalExpr::If {
            condition: condition.clone(),
            then_expr: Box::new(expr_as_designational(then_expr)?),
            else_expr: Box::new(expr_as_designational(else_expr)?),
        }),
        _ => Err(spanned_error(
            "label actual parameter must be a designational expression",
            expr.span.clone(),
        )),
    }
}

/// SWITCH formal actual: a switch identifier.
pub(in crate::mir::lower) fn switch_identifier_actual(expr: &Expr) -> Result<String, CompileError> {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => Ok(name.clone()),
        ExprKind::Paren(inner) => switch_identifier_actual(inner),
        _ => Err(spanned_error(
            "switch actual parameter must be a switch identifier",
            expr.span.clone(),
        )),
    }
}

pub(in crate::mir::lower) fn remote_method_actual(expr: &Expr) -> Option<(&Expr, &str)> {
    match &expr.kind {
        ExprKind::Paren(inner) => remote_method_actual(inner),
        ExprKind::RemoteAccess { object, attribute } => Some((object, attribute.as_str())),
        _ => None,
    }
}

/// `x.T` often parses as [`Variable::Remote`] rather than [`ExprKind::RemoteAccess`].
pub(in crate::mir::lower) fn remote_method_variable_actual(expr: &Expr) -> Option<(&str, &str)> {
    match &expr.kind {
        ExprKind::Paren(inner) => remote_method_variable_actual(inner),
        ExprKind::Variable(Variable::Remote { object, attribute }) => {
            if let Variable::Simple(name) = object.as_ref() {
                Some((name.as_str(), attribute.as_str()))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(in crate::mir::lower) fn mir_type_for(ty: &Type) -> Result<MirType, CompileError> {
    match ty {
        Type::Integer { .. } | Type::Character => Ok(MirType::I64),
        Type::Boolean => Ok(MirType::Bool),
        Type::Real { long: false } => Ok(MirType::F64),
        Type::Real { long: true } => Ok(MirType::LongF64),
        Type::Text => Ok(MirType::Text),
        Type::ObjectRef(_) => Ok(MirType::ObjectRef),
        Type::Array { element, .. } => match element.as_ref() {
            Type::Integer { .. } | Type::Boolean | Type::Character | Type::ObjectRef(_) => {
                Ok(MirType::ArrayI64)
            }
            Type::Real { .. } => Ok(MirType::ArrayF64),
            Type::Text => Ok(MirType::ArrayText),
            other => Err(CompileError::codegen(format!(
                "MIR lowering: unsupported array element type '{other}' (supported: integer/boolean/character/real/text/object-reference)"
            ))),
        },
    }
}

/// Scalar MIR type of an array's element (boolean arrays share the I64
/// descriptor ABI but read/write as [`MirType::Bool`]).
pub(in crate::mir::lower) fn array_element_mir_type(
    element: &Type,
) -> Result<MirType, CompileError> {
    match element {
        Type::Boolean => Ok(MirType::Bool),
        Type::Integer { .. } | Type::Character => Ok(MirType::I64),
        Type::Real { long: false } => Ok(MirType::F64),
        Type::Real { long: true } => Ok(MirType::LongF64),
        Type::Text => Ok(MirType::Text),
        Type::ObjectRef(_) => Ok(MirType::ObjectRef),
        other => Err(CompileError::codegen(format!(
            "MIR lowering: unsupported array element type '{other}'"
        ))),
    }
}

/// The plain (non-thunk) MIR parameter type for `param`. Outlined call-by-name
/// integer formals are handled separately (see [`is_name_thunk_formal`]) and
/// never reach this function with their thunk-triple expansion.
pub(in crate::mir::lower) fn outlined_param_mir_type(
    param: &FormalParameter,
) -> Result<MirType, CompileError> {
    mir_type_for(&param.ty)
}

/// Whether `param` is an outlined call-by-name integer formal — lowered to a
/// `(get: FuncRef, set: FuncRef, env: RefI64)` thunk triple rather than a
/// single scalar parameter (see the module docs).
pub(in crate::mir::lower) fn is_name_thunk_formal(
    param: &FormalParameter,
) -> Result<bool, CompileError> {
    let ty = mir_type_for(&param.ty)?;
    Ok(param.mode == ParamMode::Name && matches!(ty, MirType::I64 | MirType::Bool))
}

/// Whether `mode`/`ty` is allowed for an outlined MIR procedure parameter
/// (any name-thunk formal is filtered out by [`is_name_thunk_formal`] before
/// this runs). Non-outlined call-by-name is handled via call-site inlining
/// instead.
pub(in crate::mir::lower) fn outlined_param_allowed(
    mode: ParamMode,
    ty: MirType,
) -> Result<(), &'static str> {
    match mode {
        ParamMode::Value => Ok(()),
        ParamMode::Reference => {
            if matches!(
                ty,
                MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
            ) {
                // Arrays are descriptor pointers: passing the pointer aliases
                // the callee with the caller (matching the interpreter).
                Ok(())
            } else if matches!(ty, MirType::Text | MirType::ObjectRef) {
                // Outlined (non-inlined) call-by-reference text/object-ref
                // formals — e.g. class methods, which are never call-site
                // inlined — share the call-by-value lowering: both are
                // already pointer-sized handles (text frame / object), so
                // passing the value aliases the underlying data. What is
                // *not* supported yet is write-back when the callee rebinds
                // the formal itself (`:-` to a wholly new frame/object)
                // needing a stack-home pointer; true free-procedure
                // call-by-reference stays on the inlining path instead.
                Ok(())
            } else {
                Err(
                    "call-by-reference is only supported for integer/text array, text, and object-reference parameters so far (rebinding the formal itself needs stack-home pointers)",
                )
            }
        }
        ParamMode::Name => {
            // Integer name formals use the thunk ABI (`is_name_thunk_formal`)
            // and never reach here. Other scalar name formals on outlined
            // procedures (class methods) pass by value for now: the actual is
            // evaluated once at the call site. True Jensen re-eval for real/
            // boolean name formals remains a follow-up.
            if matches!(
                ty,
                MirType::I64 | MirType::F64 | MirType::LongF64 | MirType::Bool
            ) {
                Ok(())
            } else {
                Err(
                    "outlined call-by-name currently supports only integer (thunk), real, or boolean formals",
                )
            }
        }
    }
}
