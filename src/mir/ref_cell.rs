//! Phase 4-R4: give every address-taken [`MirType::ObjectRef`] a heap
//! [`crate::layout::REF_CELL_CLASS_NAME`] home.
//!
//! Wasm locals have no addresses, and a linear 8-byte cell cannot hold a
//! WasmGC reference. A `ref_cell` is an ordinary one-field object, so
//! interpreter, native, and wasm all share the same MIR: [`Op::LocalAddr`]
//! of an ObjectRef becomes a copy of the cell, and loads/stores go through
//! the cell's value field.

use std::collections::{HashMap, HashSet};

use crate::layout::{REF_CELL_CLASS_ID, REF_CELL_CLASS_NAME, REF_CELL_SIZE, REF_CELL_VALUE_OFFSET};

use super::{Function, Local, LocalId, MirType, Module, Op, Span, SpannedOp};

/// Rewrite every function so address-taken ObjectRef locals live in a
/// `ref_cell` rather than a linear home.
pub fn install_ref_cell_homes(module: &mut Module) {
    for function in &mut module.functions {
        install_in_function(function);
    }
}

fn install_in_function(function: &mut Function) {
    let taken = addr_taken_object_refs(function);
    if taken.is_empty() {
        rewrite_ref_cell_memory_ops(function);
        return;
    }

    let mut homes = HashMap::new();
    for local in &taken {
        let ptr = LocalId(function.params.len() + function.locals.len());
        function.locals.push(Local {
            name: format!("__ref_cell_{}", local.0),
            ty: MirType::ObjectRef,
            class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
            debug_scope: None,
        });
        homes.insert(*local, ptr);
    }

    let span: Span = 0..0;
    let mut preface = Vec::new();
    for local in &taken {
        let ptr = homes[local];
        preface.push(SpannedOp {
            op: Op::NewObject {
                dest: ptr,
                class_id: REF_CELL_CLASS_ID,
                size: REF_CELL_SIZE,
            },
            span: span.clone(),
        });
        preface.push(SpannedOp {
            op: Op::FieldStoreI64 {
                object: ptr,
                offset: REF_CELL_VALUE_OFFSET,
                value: *local,
                class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
            },
            span: span.clone(),
        });
    }
    function.blocks[function.entry.0].ops.splice(0..0, preface);

    // Collect dest locals that need ObjectRef typing before mutating blocks,
    // so we never borrow `function` mutably twice.
    let mut retyped_dests = Vec::new();
    for block in &function.blocks {
        for spanned in &block.ops {
            if let Op::LocalAddr { dest, local } = &spanned.op
                && homes.contains_key(local)
            {
                retyped_dests.push(*dest);
            }
        }
    }
    for dest in retyped_dests {
        set_local_ty(
            function,
            dest,
            MirType::ObjectRef,
            Some(REF_CELL_CLASS_NAME.to_string()),
        );
    }

    for block in &mut function.blocks {
        let ops = std::mem::take(&mut block.ops);
        let mut rewritten = Vec::with_capacity(ops.len() * 2);
        for spanned in ops {
            let span = spanned.span.clone();
            match spanned.op {
                Op::LocalAddr { dest, local } if homes.contains_key(&local) => {
                    rewritten.push(SpannedOp {
                        op: Op::Copy {
                            dest,
                            src: homes[&local],
                        },
                        span,
                    });
                }
                op => {
                    let written = written_local(&op);
                    let reload = reloads_object_ref_homes(&op);
                    rewritten.push(SpannedOp {
                        op,
                        span: span.clone(),
                    });
                    if let Some(local) = written
                        && let Some(&ptr) = homes.get(&local)
                    {
                        rewritten.push(SpannedOp {
                            op: Op::FieldStoreI64 {
                                object: ptr,
                                offset: REF_CELL_VALUE_OFFSET,
                                value: local,
                                class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                            },
                            span: span.clone(),
                        });
                    }
                    if reload {
                        append_reloads(&mut rewritten, &homes, &span);
                    }
                }
            }
        }
        block.ops = rewritten;
    }

    rewrite_ref_cell_memory_ops(function);
}

/// `LoadRefI64`/`StoreRefI64` through an ObjectRef pointer is a `ref_cell`
/// field access (the pointer *is* the cell).
fn rewrite_ref_cell_memory_ops(function: &mut Function) {
    // Snapshot which ptr locals are ObjectRef before mutating ops.
    let object_ptrs: HashSet<LocalId> = (0..(function.params.len() + function.locals.len()))
        .map(LocalId)
        .filter(|&id| function.local(id).ty == MirType::ObjectRef)
        .collect();

    for block in &mut function.blocks {
        for spanned in &mut block.ops {
            match &spanned.op {
                Op::LoadRefI64 { dest, ptr, offset }
                    if *offset == 0 && object_ptrs.contains(ptr) =>
                {
                    spanned.op = Op::FieldLoadI64 {
                        dest: *dest,
                        object: *ptr,
                        offset: REF_CELL_VALUE_OFFSET,
                        class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                    };
                }
                Op::StoreRefI64 { ptr, src, offset }
                    if *offset == 0 && object_ptrs.contains(ptr) =>
                {
                    spanned.op = Op::FieldStoreI64 {
                        object: *ptr,
                        offset: REF_CELL_VALUE_OFFSET,
                        value: *src,
                        class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                    };
                }
                _ => {}
            }
        }
    }
}

fn append_reloads(dest: &mut Vec<SpannedOp>, homes: &HashMap<LocalId, LocalId>, span: &Span) {
    let mut pairs: Vec<_> = homes.iter().map(|(local, ptr)| (*local, *ptr)).collect();
    pairs.sort_by_key(|(local, _)| local.0);
    for (local, ptr) in pairs {
        dest.push(SpannedOp {
            op: Op::FieldLoadI64 {
                dest: local,
                object: ptr,
                offset: REF_CELL_VALUE_OFFSET,
                class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
            },
            span: span.clone(),
        });
    }
}

fn addr_taken_object_refs(function: &Function) -> Vec<LocalId> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for spanned in function.blocks.iter().flat_map(|block| &block.ops) {
        let Op::LocalAddr { local, .. } = &spanned.op else {
            continue;
        };
        if function.local(*local).ty != MirType::ObjectRef {
            continue;
        }
        if seen.insert(*local) {
            out.push(*local);
        }
    }
    out
}

fn set_local_ty(function: &mut Function, id: LocalId, ty: MirType, class_qual: Option<String>) {
    let local = if id.0 < function.params.len() {
        &mut function.params[id.0]
    } else {
        &mut function.locals[id.0 - function.params.len()]
    };
    local.ty = ty;
    local.class_qual = class_qual;
}

fn written_local(op: &Op) -> Option<LocalId> {
    match op {
        Op::StoreLocal { local, .. } => Some(*local),
        Op::ConstNone { dest }
        | Op::Copy { dest, .. }
        | Op::NewObject { dest, .. }
        | Op::FieldLoadI64 { dest, .. }
        | Op::LoadRefI64 { dest, .. }
        | Op::Call {
            dest: Some(dest), ..
        }
        | Op::CallIndirect {
            dest: Some(dest), ..
        } => Some(*dest),
        Op::TextRefAssign { dest, .. } => Some(*dest),
        _ => None,
    }
}

fn reloads_object_ref_homes(op: &Op) -> bool {
    matches!(
        op,
        Op::Call { .. }
            | Op::CallIndirect { .. }
            | Op::SeqDetach { .. }
            | Op::SeqCall { .. }
            | Op::SeqResume { .. }
            | Op::SeqTerminate { .. }
            | Op::SeqObjectStart { .. }
            | Op::SimHold { .. }
            | Op::SimActivateDirect { .. }
            | Op::SimActivateTimed { .. }
            | Op::SimActivateRelative { .. }
            | Op::SimPassivate
            | Op::SimTransferToHead
            | Op::SimTerminateCurrent { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BasicBlock, BlockId};

    fn object_ref_local(name: &str) -> Local {
        Local {
            name: name.to_string(),
            ty: MirType::ObjectRef,
            class_qual: None,
            debug_scope: None,
        }
    }

    fn ref_i64_local(name: &str) -> Local {
        Local {
            name: name.to_string(),
            ty: MirType::RefI64,
            class_qual: None,
            debug_scope: None,
        }
    }

    fn function_with(ops: Vec<Op>, params: Vec<Local>, locals: Vec<Local>) -> Function {
        Function {
            name: "f".into(),
            params,
            locals,
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                ops: ops
                    .into_iter()
                    .map(|op| SpannedOp { op, span: 0..0 })
                    .collect(),
            }],
            labels: Default::default(),
            result: None,
            array_elem_kinds: HashMap::new(),
            foreign: None,
            export: None,
            debug_scopes: Vec::new(),
        }
    }

    #[test]
    fn local_addr_of_object_ref_becomes_ref_cell_copy() {
        let mut function = function_with(
            vec![
                Op::LocalAddr {
                    dest: LocalId(1),
                    local: LocalId(0),
                },
                Op::Return { value: None },
            ],
            vec![object_ref_local("r")],
            vec![ref_i64_local("env")],
        );
        install_in_function(&mut function);
        assert_eq!(function.local(LocalId(1)).ty, MirType::ObjectRef);
        assert!(function.blocks[0].ops.iter().any(|spanned| matches!(
            spanned.op,
            Op::NewObject {
                class_id: REF_CELL_CLASS_ID,
                ..
            }
        )));
        assert!(function.blocks[0].ops.iter().any(|spanned| matches!(
            spanned.op,
            Op::Copy {
                dest: LocalId(1),
                ..
            }
        )));
        assert!(
            !function.blocks[0]
                .ops
                .iter()
                .any(|spanned| matches!(spanned.op, Op::LocalAddr { .. }))
        );
    }

    #[test]
    fn load_through_object_ref_ptr_becomes_field_load() {
        let mut function = function_with(
            vec![Op::LoadRefI64 {
                dest: LocalId(1),
                ptr: LocalId(0),
                offset: 0,
            }],
            vec![object_ref_local("cell")],
            vec![object_ref_local("value")],
        );
        install_in_function(&mut function);
        assert!(matches!(
            function.blocks[0].ops[0].op,
            Op::FieldLoadI64 {
                dest: LocalId(1),
                object: LocalId(0),
                offset: REF_CELL_VALUE_OFFSET,
                ..
            }
        ));
    }
}
