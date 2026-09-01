//! Submodule of [`crate::mir::lower`].

use super::*;

pub(in crate::mir::lower) fn formal_proc_invoke_name(proc_name: &str) -> String {
    format!("{FORMAL_PROC_INVOKE_PREFIX}{}", proc_name.replace('$', "_"))
}

/// `fn(env: RefI64)` shim that unpacks free-cell pointers and calls `target`.
pub(in crate::mir::lower) fn build_formal_proc_invoke_helper(
    shim_name: &str,
    target: &str,
    signature: &ProcSignature,
) -> Function {
    let env = LocalId(0);
    let mut locals = Vec::new();
    let mut ops = Vec::new();
    let mut args = Vec::new();
    // Outlined procedures used as formal-proc actuals are parameterless aside
    // from trailing free-cell envs (simtst34 `Q1`/`Q2`).
    for (index, _) in signature.free_cell_params.iter().enumerate() {
        let dest = LocalId(1 + locals.len());
        locals.push(Local {
            name: format!("free{index}"),
            ty: MirType::RefI64,
            class_qual: None,
            debug_scope: None,
        });
        ops.push(SpannedOp {
            op: Op::LoadRefI64 {
                dest,
                ptr: env,
                offset: (index as i64) * 8,
            },
            span: 0..0,
        });
        args.push(dest);
    }
    let call_dest = signature.result.map(|ty| {
        let dest = LocalId(1 + locals.len());
        locals.push(Local {
            name: "%result".to_string(),
            ty,
            class_qual: None,
            debug_scope: None,
        });
        dest
    });
    ops.push(SpannedOp {
        op: Op::Call {
            dest: call_dest,
            name: target.to_string(),
            args,
        },
        span: 0..0,
    });
    ops.push(SpannedOp {
        op: Op::Return { value: None },
        span: 0..0,
    });
    Function {
        name: shim_name.to_string(),
        params: vec![Local {
            name: "env".to_string(),
            ty: MirType::RefI64,
            class_qual: None,
            debug_scope: None,
        }],
        locals,
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops,
        }],
        labels: Default::default(),
        result: None,
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// `fn(env: ObjectRef) -> I64` get thunk for a read-only call-by-name actual
/// that names an *outlined* parameterless integer type procedure: `env` is a
/// [`NAME_INT_ENV_CLASS_NAME`] box around the address vector of free cell
/// pointers `target` needs, so every read of the formal re-runs the procedure
/// (simtst35).
pub(in crate::mir::lower) fn build_name_thunk_get_call_helper(
    helper_name: &str,
    target: &str,
    signature: &ProcSignature,
) -> Function {
    let env = LocalId(0);
    let cells = LocalId(1);
    let mut locals = vec![Local {
        name: "cells".to_string(),
        ty: MirType::RefI64,
        class_qual: None,
        debug_scope: None,
    }];
    let mut ops = vec![SpannedOp {
        op: Op::FieldLoadI64 {
            dest: cells,
            object: env,
            offset: NAME_INT_ENV_ADDR_OFFSET,
            class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
        },
        span: 0..0,
    }];
    let mut args = Vec::new();
    for (index, _) in signature.free_cell_params.iter().enumerate() {
        let dest = LocalId(1 + locals.len());
        locals.push(Local {
            name: format!("free{index}"),
            ty: MirType::RefI64,
            class_qual: None,
            debug_scope: None,
        });
        ops.push(SpannedOp {
            op: Op::LoadRefI64 {
                dest,
                ptr: cells,
                offset: (index as i64) * 8,
            },
            span: 0..0,
        });
        args.push(dest);
    }
    let result = LocalId(1 + locals.len());
    locals.push(Local {
        name: "%result".to_string(),
        ty: MirType::I64,
        class_qual: None,
        debug_scope: None,
    });
    ops.push(SpannedOp {
        op: Op::Call {
            dest: Some(result),
            name: target.to_string(),
            args,
        },
        span: 0..0,
    });
    ops.push(SpannedOp {
        op: Op::Return {
            value: Some(result),
        },
        span: 0..0,
    });
    Function {
        name: helper_name.to_string(),
        params: vec![Local {
            name: "env".to_string(),
            ty: MirType::ObjectRef,
            class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
            debug_scope: None,
        }],
        locals,
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops,
        }],
        labels: Default::default(),
        result: Some(MirType::I64),
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

pub(in crate::mir::lower) fn name_thunk_get_field_name(offset: i64) -> String {
    format!("{NAME_THUNK_GET_FIELD_PREFIX}{offset}")
}

pub(in crate::mir::lower) fn name_thunk_set_field_name(offset: i64) -> String {
    format!("{NAME_THUNK_SET_FIELD_PREFIX}{offset}")
}

/// Builds the shared `__simrt_name_get_ref(env: ObjectRef) -> I64` MIR
/// function: `env` is a [`NAME_INT_ENV_CLASS_NAME`] holding the integer
/// cell's address; this helper loads that address and returns `*addr`.
pub(in crate::mir::lower) fn build_name_thunk_get_helper() -> Function {
    let env = LocalId(0);
    let addr = LocalId(1);
    let dest = LocalId(2);
    Function {
        name: NAME_THUNK_GET_HELPER.to_string(),
        params: vec![Local {
            name: "env".to_string(),
            ty: MirType::ObjectRef,
            class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
            debug_scope: None,
        }],
        locals: vec![
            Local {
                name: "%t0".to_string(),
                ty: MirType::RefI64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t1".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: addr,
                        object: env,
                        offset: NAME_INT_ENV_ADDR_OFFSET,
                        class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::LoadRefI64 {
                        dest,
                        ptr: addr,
                        offset: 0,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::Return { value: Some(dest) },
                    span: 0..0,
                },
            ],
        }],
        labels: Default::default(),
        result: Some(MirType::I64),
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Builds the shared `__simrt_name_set_ref(env: ObjectRef, v: I64)` MIR
/// function: unpacks `env` like [`build_name_thunk_get_helper`], then
/// `*addr := v`.
pub(in crate::mir::lower) fn build_name_thunk_set_helper() -> Function {
    let env = LocalId(0);
    let value = LocalId(1);
    let addr = LocalId(2);
    Function {
        name: NAME_THUNK_SET_HELPER.to_string(),
        params: vec![
            Local {
                name: "env".to_string(),
                ty: MirType::ObjectRef,
                class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
                debug_scope: None,
            },
            Local {
                name: "v".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        locals: vec![Local {
            name: "%t0".to_string(),
            ty: MirType::RefI64,
            class_qual: None,
            debug_scope: None,
        }],
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: addr,
                        object: env,
                        offset: NAME_INT_ENV_ADDR_OFFSET,
                        class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::StoreRefI64 {
                        ptr: addr,
                        src: value,
                        offset: 0,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::Return { value: None },
                    span: 0..0,
                },
            ],
        }],
        labels: Default::default(),
        result: None,
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Builds the shared `__simrt_name_get_arr1(env: RefI64) -> I64` MIR
/// function for an outlined call-by-name integer formal whose actual is a
/// 1-D integer array element (`a(i)`): `env` packs `(array_bits, index_ptr_bits)`
/// as two adjacent `i64` words (see
/// [`FunctionBuilder::name_thunk_triple_for_arr1_elem`]); this helper unpacks
/// both, re-reads the index through `index_ptr` (so a shared `env` — e.g.
/// across a whole recursive call chain — always sees the *current* index and
/// array contents), and returns `array[index]`.
pub(in crate::mir::lower) fn build_name_thunk_get_arr1_helper() -> Function {
    let env = LocalId(0);
    let array = LocalId(1);
    let index_ptr = LocalId(2);
    let index = LocalId(3);
    let dest = LocalId(4);
    Function {
        name: NAME_THUNK_GET_ARR1.to_string(),
        params: vec![Local {
            name: "env".to_string(),
            ty: MirType::ObjectRef,
            class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
            debug_scope: None,
        }],
        locals: vec![
            Local {
                name: "%t0".to_string(),
                ty: MirType::ArrayI64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t1".to_string(),
                ty: MirType::RefI64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t2".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t3".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: array,
                        object: env,
                        offset: NAME_ARR1_ENV_ARRAY_OFFSET,
                        class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: index_ptr,
                        object: env,
                        offset: NAME_ARR1_ENV_INDEX_OFFSET,
                        class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::LoadRefI64 {
                        dest: index,
                        ptr: index_ptr,
                        offset: 0,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::ArrayLoad {
                        dest,
                        array,
                        indices: vec![index],
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::Return { value: Some(dest) },
                    span: 0..0,
                },
            ],
        }],
        labels: Default::default(),
        result: Some(MirType::I64),
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Builds the shared `__simrt_name_set_arr1(env: ObjectRef, v: I64)` MIR
/// function: unpacks `env` exactly like
/// [`build_name_thunk_get_arr1_helper`], then stores `v` into `array[index]`.
pub(in crate::mir::lower) fn build_name_thunk_set_arr1_helper() -> Function {
    let env = LocalId(0);
    let value = LocalId(1);
    let array = LocalId(2);
    let index_ptr = LocalId(3);
    let index = LocalId(4);
    Function {
        name: NAME_THUNK_SET_ARR1.to_string(),
        params: vec![
            Local {
                name: "env".to_string(),
                ty: MirType::ObjectRef,
                class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
                debug_scope: None,
            },
            Local {
                name: "v".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        locals: vec![
            Local {
                name: "%t0".to_string(),
                ty: MirType::ArrayI64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t1".to_string(),
                ty: MirType::RefI64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t2".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: array,
                        object: env,
                        offset: NAME_ARR1_ENV_ARRAY_OFFSET,
                        class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: index_ptr,
                        object: env,
                        offset: NAME_ARR1_ENV_INDEX_OFFSET,
                        class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::LoadRefI64 {
                        dest: index,
                        ptr: index_ptr,
                        offset: 0,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::ArrayStore {
                        array,
                        indices: vec![index],
                        value,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::Return { value: None },
                    span: 0..0,
                },
            ],
        }],
        labels: Default::default(),
        result: None,
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Builds `__simrt_name_get_field_{offset}(env: ObjectRef) -> I64` for an
/// outlined call-by-name integer formal whose actual is a simple object field
/// `r.x`: `env` is the `ref_cell` home of `r`; this helper loads the current
/// object and returns `FieldLoad(object, offset)`.
pub(in crate::mir::lower) fn build_name_thunk_get_field_helper(offset: i64) -> Function {
    let env = LocalId(0);
    let object = LocalId(1);
    let dest = LocalId(2);
    Function {
        name: name_thunk_get_field_name(offset),
        params: vec![Local {
            name: "env".to_string(),
            ty: MirType::ObjectRef,
            class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
            debug_scope: None,
        }],
        locals: vec![
            Local {
                name: "%t0".to_string(),
                ty: MirType::ObjectRef,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "%t1".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: object,
                        object: env,
                        offset: REF_CELL_VALUE_OFFSET,
                        class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest,
                        object,
                        offset,
                        class_qual: None,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::Return { value: Some(dest) },
                    span: 0..0,
                },
            ],
        }],
        labels: Default::default(),
        result: Some(MirType::I64),
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Builds `__simrt_name_set_field_{offset}(env: ObjectRef, v: I64)` — unpacks
/// `env` like [`build_name_thunk_get_field_helper`], then
/// `FieldStore(object, offset, v)`.
pub(in crate::mir::lower) fn build_name_thunk_set_field_helper(offset: i64) -> Function {
    let env = LocalId(0);
    let value = LocalId(1);
    let object = LocalId(2);
    Function {
        name: name_thunk_set_field_name(offset),
        params: vec![
            Local {
                name: "env".to_string(),
                ty: MirType::ObjectRef,
                class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                debug_scope: None,
            },
            Local {
                name: "v".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        locals: vec![Local {
            name: "%t0".to_string(),
            ty: MirType::ObjectRef,
            class_qual: None,
            debug_scope: None,
        }],
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                SpannedOp {
                    op: Op::FieldLoadI64 {
                        dest: object,
                        object: env,
                        offset: REF_CELL_VALUE_OFFSET,
                        class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::FieldStoreI64 {
                        object,
                        offset,
                        value,
                        class_qual: None,
                    },
                    span: 0..0,
                },
                SpannedOp {
                    op: Op::Return { value: None },
                    span: 0..0,
                },
            ],
        }],
        labels: Default::default(),
        result: None,
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Builds `__simrt_name_set_readonly(env, v)` — a no-op set used for
/// get-only expression name thunks (the formal is never assigned).
pub(in crate::mir::lower) fn build_name_thunk_set_readonly_helper() -> Function {
    Function {
        name: NAME_THUNK_SET_READONLY.to_string(),
        params: vec![
            Local {
                name: "env".to_string(),
                ty: MirType::ObjectRef,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "v".to_string(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ],
        locals: Vec::new(),
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![SpannedOp {
                op: Op::Return { value: None },
                span: 0..0,
            }],
        }],
        labels: Default::default(),
        result: None,
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    }
}

/// Keeps the first definition of each function name (per-offset field name
/// thunks may be emitted from more than one [`FunctionBuilder`]).
pub(in crate::mir::lower) fn dedupe_functions_by_name(functions: &mut Vec<Function>) {
    let mut seen = HashSet::new();
    functions.retain(|function| seen.insert(function.name.clone()));
}

/// How a free name is captured into an expression-thunk env cell. Only
/// [`Self::Cell`] can be packed: the env is a linear word vector, and the other
/// two are WasmGC references (see [`build_expr_reeval_get_helper`]).
#[derive(Debug, Clone, Copy)]
pub(in crate::mir::lower) enum ExprCapture {
    /// `env` holds `&cell`.
    Cell(LocalId),
    /// The free name is an object reference.
    Object(LocalId),
    /// The free name is an outlined name formal's `(get, env)` pair.
    Thunk { get: LocalId, env: LocalId },
}

/// Binding of a free name inside a synthesized expression get helper.
#[derive(Debug, Clone, Copy)]
pub(in crate::mir::lower) enum ExprHelperBinding {
    Value(LocalId),
    Object(LocalId),
    Thunk { get: LocalId, env: LocalId },
}

/// Collects free simple names from an integer expression actual, in
/// first-seen order. Returns `None` if the expression shape is not supported
/// for re-eval thunks (caller should fall back to a temp-cell snapshot).
///
/// Supported shapes: integer literals; unary `+/-`; binary `+`/`-`/`*`/`//`/`/`;
/// simple integer / name-thunk locals; simple `ref(C).int_attr`; integer
/// relations (`<`/`<=`/`=`/`>=`/`>`/`<>`) used inside `if` conditions;
/// `if … then … else …` expressions whose branches are supported integer
/// exprs and whose condition is a supported boolean expr.
///
/// `resolve_int_field(object_name, attribute)` returns the field byte offset
/// when `object_name` is a `ref(C)` local with an integer attribute.
pub(in crate::mir::lower) fn collect_expr_captures(
    expr: &Expr,
    scope: &HashMap<String, LocalId>,
    name_thunks: &HashMap<String, (LocalId, LocalId, LocalId)>,
    local_ty: impl Fn(LocalId) -> MirType,
    resolve_int_field: impl Fn(&str, &str) -> Option<i64>,
) -> Option<(Vec<(String, ExprCapture)>, HashMap<(String, String), i64>)> {
    let mut captures = Vec::new();
    let mut seen = HashSet::new();
    let mut field_offsets = HashMap::new();
    if !walk_integer_expr_captures(
        expr,
        scope,
        name_thunks,
        &local_ty,
        &resolve_int_field,
        &mut captures,
        &mut seen,
        &mut field_offsets,
    ) {
        return None;
    }
    Some((captures, field_offsets))
}

pub(in crate::mir::lower) fn walk_integer_expr_captures(
    expr: &Expr,
    scope: &HashMap<String, LocalId>,
    name_thunks: &HashMap<String, (LocalId, LocalId, LocalId)>,
    local_ty: &impl Fn(LocalId) -> MirType,
    resolve_int_field: &impl Fn(&str, &str) -> Option<i64>,
    captures: &mut Vec<(String, ExprCapture)>,
    seen: &mut HashSet<String>,
    field_offsets: &mut HashMap<(String, String), i64>,
) -> bool {
    match &expr.kind {
        ExprKind::Paren(inner) => walk_integer_expr_captures(
            inner,
            scope,
            name_thunks,
            local_ty,
            resolve_int_field,
            captures,
            seen,
            field_offsets,
        ),
        ExprKind::NumberLiteral {
            kind: ArithmeticLiteralKind::Integer,
            ..
        } => true,
        ExprKind::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            operand,
        } => walk_integer_expr_captures(
            operand,
            scope,
            name_thunks,
            local_ty,
            resolve_int_field,
            captures,
            seen,
            field_offsets,
        ),
        ExprKind::Binary {
            op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::IntDiv | BinaryOp::Div,
            left,
            right,
        } => {
            walk_integer_expr_captures(
                left,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            ) && walk_integer_expr_captures(
                right,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            )
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_bool_expr_captures(
                condition,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            ) && walk_integer_expr_captures(
                then_expr,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            ) && walk_integer_expr_captures(
                else_expr,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            )
        }
        ExprKind::RemoteAccess { object, attribute } => {
            let ExprKind::Variable(Variable::Simple(object_name)) = &object.kind else {
                return false;
            };
            capture_remote_int_field(
                object_name,
                attribute,
                scope,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            )
        }
        ExprKind::Variable(Variable::Remote { object, attribute }) => {
            let Variable::Simple(object_name) = object.as_ref() else {
                return false;
            };
            capture_remote_int_field(
                object_name,
                attribute,
                scope,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            )
        }
        ExprKind::Variable(Variable::Simple(name)) => {
            if seen.contains(name) {
                return true;
            }
            if let Some(&(get, _, env)) = name_thunks.get(name) {
                seen.insert(name.clone());
                captures.push((name.clone(), ExprCapture::Thunk { get, env }));
                return true;
            }
            let Some(&id) = scope.get(name) else {
                return false;
            };
            match local_ty(id) {
                MirType::I64 => {
                    seen.insert(name.clone());
                    captures.push((name.clone(), ExprCapture::Cell(id)));
                    true
                }
                MirType::ObjectRef => false,
                _ => false,
            }
        }
        _ => false,
    }
}

pub(in crate::mir::lower) fn walk_bool_expr_captures(
    expr: &Expr,
    scope: &HashMap<String, LocalId>,
    name_thunks: &HashMap<String, (LocalId, LocalId, LocalId)>,
    local_ty: &impl Fn(LocalId) -> MirType,
    resolve_int_field: &impl Fn(&str, &str) -> Option<i64>,
    captures: &mut Vec<(String, ExprCapture)>,
    seen: &mut HashSet<String>,
    field_offsets: &mut HashMap<(String, String), i64>,
) -> bool {
    match &expr.kind {
        ExprKind::Paren(inner) => walk_bool_expr_captures(
            inner,
            scope,
            name_thunks,
            local_ty,
            resolve_int_field,
            captures,
            seen,
            field_offsets,
        ),
        ExprKind::BooleanLiteral(_) => true,
        ExprKind::Unary {
            op: UnaryOp::Not,
            operand,
        } => walk_bool_expr_captures(
            operand,
            scope,
            name_thunks,
            local_ty,
            resolve_int_field,
            captures,
            seen,
            field_offsets,
        ),
        ExprKind::Relation {
            op:
                RelationOp::Lt
                | RelationOp::Le
                | RelationOp::Eq
                | RelationOp::Ge
                | RelationOp::Gt
                | RelationOp::Ne,
            left,
            right,
        } => {
            walk_integer_expr_captures(
                left,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            ) && walk_integer_expr_captures(
                right,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            )
        }
        ExprKind::Binary {
            op: BinaryOp::And | BinaryOp::Or | BinaryOp::AndThen | BinaryOp::OrElse,
            left,
            right,
        } => {
            walk_bool_expr_captures(
                left,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            ) && walk_bool_expr_captures(
                right,
                scope,
                name_thunks,
                local_ty,
                resolve_int_field,
                captures,
                seen,
                field_offsets,
            )
        }
        // Bare boolean locals are not captured yet (would need typed cell
        // unpack); conditions must be relations / boolean ops / literals.
        _ => false,
    }
}

pub(in crate::mir::lower) fn capture_remote_int_field(
    object_name: &str,
    attribute: &str,
    scope: &HashMap<String, LocalId>,
    local_ty: &impl Fn(LocalId) -> MirType,
    resolve_int_field: &impl Fn(&str, &str) -> Option<i64>,
    captures: &mut Vec<(String, ExprCapture)>,
    seen: &mut HashSet<String>,
    field_offsets: &mut HashMap<(String, String), i64>,
) -> bool {
    let Some(offset) = resolve_int_field(object_name, attribute) else {
        return false;
    };
    field_offsets.insert((object_name.to_string(), attribute.to_string()), offset);
    if seen.contains(object_name) {
        return true;
    }
    let Some(&id) = scope.get(object_name) else {
        return false;
    };
    if local_ty(id) != MirType::ObjectRef {
        return false;
    }
    seen.insert(object_name.to_string());
    captures.push((object_name.to_string(), ExprCapture::Object(id)));
    true
}

/// Builds a per-call-site `__simrt_name_get_expr_N(env) -> I64` that
/// unpacks `captures` from `env` and re-evaluates `expr` (possibly across
/// multiple basic blocks when the expression contains `if`).
pub(in crate::mir::lower) fn build_expr_reeval_get_helper(
    helper_name: String,
    expr: &Expr,
    captures: &[(String, ExprCapture)],
    field_offsets: &HashMap<(String, String), i64>,
) -> Option<Function> {
    if captures.len() > NAME_PACK_ENV_SLOT_COUNT {
        return None;
    }
    let env = LocalId(0);
    let mut locals = vec![Local {
        name: "env".to_string(),
        ty: MirType::ObjectRef,
        class_qual: Some(NAME_PACK_ENV_CLASS_NAME.to_string()),
        debug_scope: None,
    }];
    let mut blocks = vec![BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        ops: Vec::new(),
    }];
    let mut current = BlockId(0);
    let mut bindings: HashMap<String, ExprHelperBinding> = HashMap::new();
    let mut next_local = 1usize;

    let mut alloc = |name: String, ty: MirType| -> LocalId {
        let id = LocalId(next_local);
        next_local += 1;
        locals.push(Local {
            name,
            ty,
            class_qual: None,
            debug_scope: None,
        });
        id
    };

    let push = |blocks: &mut Vec<BasicBlock>, current: BlockId, op: Op, span: Span| {
        blocks[current.0].ops.push(SpannedOp { op, span });
    };

    for (index, (name, capture)) in captures.iter().enumerate() {
        let slot = alloc(format!("{name}$slot"), MirType::ObjectRef);
        push(
            &mut blocks,
            current,
            Op::FieldLoadI64 {
                dest: slot,
                object: env,
                offset: name_pack_env_slot_offset(index),
                class_qual: Some(NAME_PACK_ENV_CLASS_NAME.to_string()),
            },
            0..0,
        );
        match *capture {
            ExprCapture::Cell(_) => {
                let ptr = alloc(format!("{name}$ptr"), MirType::RefI64);
                let value = alloc(name.clone(), MirType::I64);
                push(
                    &mut blocks,
                    current,
                    Op::FieldLoadI64 {
                        dest: ptr,
                        object: slot,
                        offset: NAME_INT_ENV_ADDR_OFFSET,
                        class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
                    },
                    0..0,
                );
                push(
                    &mut blocks,
                    current,
                    Op::LoadRefI64 {
                        dest: value,
                        ptr,
                        offset: 0,
                    },
                    0..0,
                );
                bindings.insert(name.clone(), ExprHelperBinding::Value(value));
            }
            ExprCapture::Object(_) => {
                bindings.insert(name.clone(), ExprHelperBinding::Object(slot));
            }
            ExprCapture::Thunk { .. } => {
                let get = alloc(format!("{name}$get"), MirType::FuncRef);
                let thunk_env = alloc(format!("{name}$env"), MirType::ObjectRef);
                push(
                    &mut blocks,
                    current,
                    Op::FieldLoadI64 {
                        dest: get,
                        object: slot,
                        offset: NAME_THUNK_PAIR_GET_OFFSET,
                        class_qual: Some(NAME_THUNK_PAIR_CLASS_NAME.to_string()),
                    },
                    0..0,
                );
                push(
                    &mut blocks,
                    current,
                    Op::FieldLoadI64 {
                        dest: thunk_env,
                        object: slot,
                        offset: NAME_THUNK_PAIR_ENV_OFFSET,
                        class_qual: Some(NAME_THUNK_PAIR_CLASS_NAME.to_string()),
                    },
                    0..0,
                );
                bindings.insert(
                    name.clone(),
                    ExprHelperBinding::Thunk {
                        get,
                        env: thunk_env,
                    },
                );
            }
        }
    }

    let mut helper = ExprHelperEmit {
        locals: &mut locals,
        next_local: &mut next_local,
        blocks: &mut blocks,
        current: &mut current,
        bindings: &bindings,
        field_offsets,
    };
    let result = helper.emit_integer(expr)?;
    helper.push(
        Op::Return {
            value: Some(result),
        },
        0..0,
    );

    let params = locals.drain(..1).collect();
    Some(Function {
        name: helper_name,
        params,
        locals,
        entry: BlockId(0),
        blocks,
        labels: Default::default(),
        result: Some(MirType::I64),
        array_elem_kinds: std::collections::HashMap::new(),
        foreign: None,
        export: None,
        debug_scopes: Vec::new(),
    })
}

pub(in crate::mir::lower) struct ExprHelperEmit<'a> {
    locals: &'a mut Vec<Local>,
    next_local: &'a mut usize,
    blocks: &'a mut Vec<BasicBlock>,
    current: &'a mut BlockId,
    bindings: &'a HashMap<String, ExprHelperBinding>,
    field_offsets: &'a HashMap<(String, String), i64>,
}

impl ExprHelperEmit<'_> {
    fn push(&mut self, op: Op, span: Span) {
        self.blocks[self.current.0].ops.push(SpannedOp { op, span });
    }

    fn alloc(&mut self, ty: MirType) -> LocalId {
        alloc_helper_local(self.locals, self.next_local, ty)
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len());
        self.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            ops: Vec::new(),
        });
        id
    }

    fn switch_to(&mut self, id: BlockId) {
        *self.current = id;
    }

    fn emit_integer(&mut self, expr: &Expr) -> Option<LocalId> {
        match &expr.kind {
            ExprKind::Paren(inner) => self.emit_integer(inner),
            ExprKind::NumberLiteral {
                lexeme,
                kind: ArithmeticLiteralKind::Integer,
            } => {
                let value: i64 = lexeme.parse().ok()?;
                let dest = self.alloc(MirType::I64);
                self.push(Op::ConstI64 { dest, value }, expr.span.clone());
                Some(dest)
            }
            ExprKind::RemoteAccess { object, attribute } => {
                let ExprKind::Variable(Variable::Simple(object_name)) = &object.kind else {
                    return None;
                };
                self.emit_remote_int_field(object_name, attribute, expr.span.clone())
            }
            ExprKind::Variable(Variable::Remote { object, attribute }) => {
                let Variable::Simple(object_name) = object.as_ref() else {
                    return None;
                };
                self.emit_remote_int_field(object_name, attribute, expr.span.clone())
            }
            ExprKind::Variable(Variable::Simple(name)) => match self.bindings.get(name)? {
                ExprHelperBinding::Value(id) => Some(*id),
                ExprHelperBinding::Object(_) => None,
                ExprHelperBinding::Thunk { get, env } => {
                    let dest = self.alloc(MirType::I64);
                    self.push(
                        Op::CallIndirect {
                            dest: Some(dest),
                            callee: *get,
                            args: vec![*env],
                            sig: CallSig {
                                params: vec![MirType::ObjectRef],
                                result: Some(MirType::I64),
                            },
                        },
                        expr.span.clone(),
                    );
                    Some(dest)
                }
            },
            ExprKind::Unary {
                op: UnaryOp::Plus,
                operand,
            } => self.emit_integer(operand),
            ExprKind::Unary {
                op: UnaryOp::Minus,
                operand,
            } => {
                let operand = self.emit_integer(operand)?;
                let dest = self.alloc(MirType::I64);
                self.push(
                    Op::Unary {
                        dest,
                        op: UnOp::Neg,
                        src: operand,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            ExprKind::Binary { op, left, right } => {
                let mir_op = match op {
                    BinaryOp::Add => BinOp::Add,
                    BinaryOp::Sub => BinOp::Sub,
                    BinaryOp::Mul => BinOp::Mul,
                    BinaryOp::IntDiv | BinaryOp::Div => BinOp::IntDiv,
                    _ => return None,
                };
                let left = self.emit_integer(left)?;
                let right = self.emit_integer(right)?;
                let dest = self.alloc(MirType::I64);
                self.push(
                    Op::Binary {
                        dest,
                        op: mir_op,
                        left,
                        right,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            ExprKind::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let cond = self.emit_bool(condition)?;
                let then_block = self.new_block();
                let else_block = self.new_block();
                let merge_block = self.new_block();
                let result = self.alloc(MirType::I64);
                self.push(
                    Op::Branch {
                        cond,
                        then_block,
                        else_block,
                    },
                    expr.span.clone(),
                );

                self.switch_to(then_block);
                let then_value = self.emit_integer(then_expr)?;
                self.push(
                    Op::Copy {
                        dest: result,
                        src: then_value,
                    },
                    expr.span.clone(),
                );
                self.push(
                    Op::Jump {
                        target: merge_block,
                    },
                    0..0,
                );

                self.switch_to(else_block);
                let else_value = self.emit_integer(else_expr)?;
                self.push(
                    Op::Copy {
                        dest: result,
                        src: else_value,
                    },
                    expr.span.clone(),
                );
                self.push(
                    Op::Jump {
                        target: merge_block,
                    },
                    0..0,
                );

                self.switch_to(merge_block);
                Some(result)
            }
            _ => None,
        }
    }

    fn emit_bool(&mut self, expr: &Expr) -> Option<LocalId> {
        match &expr.kind {
            ExprKind::Paren(inner) => self.emit_bool(inner),
            ExprKind::BooleanLiteral(value) => {
                let dest = self.alloc(MirType::Bool);
                self.push(
                    Op::ConstBool {
                        dest,
                        value: *value,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } => {
                let operand = self.emit_bool(operand)?;
                let dest = self.alloc(MirType::Bool);
                self.push(
                    Op::Unary {
                        dest,
                        op: UnOp::Not,
                        src: operand,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            ExprKind::Relation { op, left, right } => {
                let cmp = match op {
                    RelationOp::Lt => CmpOp::Lt,
                    RelationOp::Le => CmpOp::Le,
                    RelationOp::Eq => CmpOp::Eq,
                    RelationOp::Ge => CmpOp::Ge,
                    RelationOp::Gt => CmpOp::Gt,
                    RelationOp::Ne => CmpOp::Ne,
                    _ => return None,
                };
                let left = self.emit_integer(left)?;
                let right = self.emit_integer(right)?;
                let dest = self.alloc(MirType::Bool);
                self.push(
                    Op::Compare {
                        dest,
                        op: cmp,
                        left,
                        right,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            ExprKind::Binary {
                op: BinaryOp::And | BinaryOp::AndThen,
                left,
                right,
            } => {
                // Eager And (AndThen short-circuit not modeled in helpers).
                let left = self.emit_bool(left)?;
                let right = self.emit_bool(right)?;
                let dest = self.alloc(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::And,
                        left,
                        right,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            ExprKind::Binary {
                op: BinaryOp::Or | BinaryOp::OrElse,
                left,
                right,
            } => {
                let left = self.emit_bool(left)?;
                let right = self.emit_bool(right)?;
                let dest = self.alloc(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::Or,
                        left,
                        right,
                    },
                    expr.span.clone(),
                );
                Some(dest)
            }
            _ => None,
        }
    }

    fn emit_remote_int_field(
        &mut self,
        object_name: &str,
        attribute: &str,
        span: Span,
    ) -> Option<LocalId> {
        let ExprHelperBinding::Object(object) = *self.bindings.get(object_name)? else {
            return None;
        };
        let &offset = self
            .field_offsets
            .get(&(object_name.to_string(), attribute.to_string()))?;
        let dest = self.alloc(MirType::I64);
        self.push(
            Op::FieldLoadI64 {
                dest,
                object,
                offset,
                class_qual: None,
            },
            span,
        );
        Some(dest)
    }
}

pub(in crate::mir::lower) fn alloc_helper_local(
    locals: &mut Vec<Local>,
    next_local: &mut usize,
    ty: MirType,
) -> LocalId {
    let id = LocalId(*next_local);
    *next_local += 1;
    locals.push(Local {
        name: format!("%t{}", id.0),
        ty,
        class_qual: None,
        debug_scope: None,
    });
    id
}

/// Whether any local procedure (outlined) or class method in the program has
/// an outlined call-by-name integer formal, i.e. whether the shared
/// `__simrt_name_get_ref` / `__simrt_name_set_ref` helper functions need
/// to be added to the module.
pub(in crate::mir::lower) fn needs_name_thunk_helpers(
    value_procedures: &[&ProcedureDeclaration],
    methods: &[ClassMethod<'_>],
) -> bool {
    value_procedures
        .iter()
        .any(|procedure| procedure_has_name_thunk_param(procedure))
        || methods
            .iter()
            .any(|method| procedure_has_name_thunk_param(method.procedure))
}

pub(in crate::mir::lower) fn procedure_has_name_thunk_param(
    procedure: &ProcedureDeclaration,
) -> bool {
    procedure
        .parameters
        .iter()
        .any(|param| is_name_thunk_formal(param).unwrap_or(false))
}
