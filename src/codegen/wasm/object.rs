//! Submodule of [`crate::codegen::wasm`].

use super::*;

pub(in crate::codegen::wasm) fn emit_new_object_gc(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    class_id: i64,
) -> Result<(), CompileError> {
    let _ = function;
    let Some(info) = gc_ctx(|ctx| ctx.class_info_for_id(class_id).cloned()).flatten() else {
        return Err(CompileError::codegen(format!(
            "MIR wasm: NewObject class_id {class_id} has no WasmGC struct type"
        )));
    };
    let ty = info.wasm_ty;
    body.instruction(&Instruction::StructNewDefault(ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
    body.instruction(&Instruction::I64Const(class_id));
    body.instruction(&Instruction::StructSet {
        struct_type_index: ty,
        field_index: 0,
    });
    Ok(())
}

/// Resolve the WasmGC class for `object`. `qual_override` — when given — takes
/// priority over `function.local(object).class_qual`. Callers that hold a
/// point-in-time qualifier snapshot from [`Op::FieldLoadI64`] /
/// [`Op::FieldStoreI64`] (see that type's doc comment) must pass it here:
/// `Local::class_qual` is a single mutable slot per `LocalId` for the whole
/// function and may have been overwritten by a later `:-` reassignment by
/// the time codegen runs.
pub(in crate::codegen::wasm) fn resolve_gc_object_info_with_qual(
    function: &MirFunction,
    object: LocalId,
    qual_override: Option<&str>,
) -> Result<crate::codegen::wasm_gc::ClassGcInfo, CompileError> {
    let qual = qual_override
        .or(function.local(object).class_qual.as_deref())
        .ok_or_else(|| {
            CompileError::codegen(
                "MIR wasm: ObjectRef local missing class_qual for WasmGC field access",
            )
        })?;
    gc_ctx(|ctx| ctx.class_info_for_name(qual).cloned())
        .flatten()
        .ok_or_else(|| {
            CompileError::codegen(format!(
                "MIR wasm: no WasmGC struct type for class '{qual}'"
            ))
        })
}

/// Whether a layout field can hold a MIR value of `ty` under the current
/// WasmGC foothold. `Text`/`ArrayI64`/`ArrayBool`/`ArrayF64`/`ArrayText`
/// attrs are typed WasmGC references exactly like `ObjectRef` (see
/// `wasm_gc.rs::layout_field_type`), so each accepts its own MIR type as a
/// reference *and* `RefI64`/`I64` writeback addresses for by-reference
/// ("name" parameter / enclosing-capture) aliasing — see
/// [`field_ref_type_match`].
///
/// `Bool`/`F64` fields are genuinely native-typed (`i32`/`f64`, not i64
/// words), but a captured-by-reference enclosing local — any scalar type,
/// not just `ObjectRef`/`Text`/array — stores its **address** (`RefI64`,
/// always wasm32-address-sized) into the very same offset the value would
/// otherwise occupy (§4.6.3/§4.7). `emit_field_store_gc`/`emit_field_load_gc`
/// already round-trip that address losslessly through the native slot
/// (`I32WrapI64`/`I64ExtendI32U` for `Bool`, `F64Reinterpret`/`I64Reinterpret`
/// for `F64`), so `RefI64` must be accepted here too or [`resolve_gc_field`]
/// falls back to guessing an unrelated class by offset alone.
pub(in crate::codegen::wasm) fn gc_field_compatible(
    field_ty: LayoutFieldType,
    ty: MirType,
) -> bool {
    match field_ty {
        // ObjectRef slots hold live refs (`ref_cell` or a Simula object),
        // never a linear address. I64/RefI64 compatibility was handle-table
        // debt and would let `dec(r.x)` pick `__simrt_ref_cell.value`
        // (eqref at offset 8) instead of `C.x` (i64 at offset 8).
        LayoutFieldType::ObjectRef => {
            // Arrays may live in an ObjectRef slot (`NAME_ARR1_ENV.array`).
            // Text must not: `town.nam_` and `townpoint.t` share offset 24
            // (simtst96), and treating the ObjectRef as Text-compatible made
            // fallback pick `townpoint` and `ref.cast` a town to `text_frame`.
            matches!(ty, MirType::ObjectRef)
                || matches!(
                    ty,
                    MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
                )
        }
        LayoutFieldType::Bool => matches!(ty, MirType::Bool | MirType::I64 | MirType::RefI64),
        LayoutFieldType::F64 => ty.is_float() || matches!(ty, MirType::RefI64 | MirType::I64),
        LayoutFieldType::I64 => matches!(
            ty,
            MirType::I64 | MirType::Bool | MirType::RefI64 | MirType::FuncRef
        ),
        LayoutFieldType::Text => matches!(ty, MirType::Text),
        LayoutFieldType::ArrayI64 | LayoutFieldType::ArrayBool => {
            matches!(ty, MirType::ArrayI64)
        }
        LayoutFieldType::ArrayF64 => matches!(ty, MirType::ArrayF64),
        LayoutFieldType::ArrayText => matches!(ty, MirType::ArrayText),
    }
}

/// Whether a reference-typed `field_ty` is being accessed with its own
/// *matching* MIR type (so the slot moves a live WasmGC ref) as opposed to
/// `RefI64`/`I64` — a raw linear-memory writeback address, stored and loaded
/// verbatim as a word.
pub(in crate::codegen::wasm) fn field_ref_type_match(
    field_ty: LayoutFieldType,
    ty: MirType,
) -> bool {
    matches!(
        (field_ty, ty),
        (LayoutFieldType::ObjectRef, MirType::ObjectRef)
            // Uniform eqref slots (linkage trailing attrs, name-thunk arr1 env)
            // can hold any GC ref; the load path `ref.cast`s back to `ty`.
            | (
                LayoutFieldType::ObjectRef,
                MirType::Text | MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
            )
            | (LayoutFieldType::Text, MirType::Text)
            | (
                LayoutFieldType::ArrayI64 | LayoutFieldType::ArrayBool,
                MirType::ArrayI64
            )
            | (LayoutFieldType::ArrayF64, MirType::ArrayF64)
            | (LayoutFieldType::ArrayText, MirType::ArrayText)
    )
}

/// Whether a linkage-family class can serve an access at `field_index` with
/// MIR type `prefer`.
///
/// SUC/PRED are real `(ref null eq)` struct fields (Phase 4-R2), not `i64`
/// handle words, so they can only satisfy an `ObjectRef` access. Anything
/// asking for a word at those slots — a `name`-parameter writeback address,
/// or [`resolve_gc_field`]'s offset-only search landing on a ring class by
/// coincidence — has to keep looking for a class whose slot really is one.
pub(in crate::codegen::wasm) fn gc_link_field_serves(
    info: &crate::codegen::wasm_gc::ClassGcInfo,
    field_index: u32,
    prefer: Option<MirType>,
) -> bool {
    if !crate::codegen::wasm_gc::is_linkage_ref_field(field_index) || !info.is_linkage_family() {
        return true;
    }
    matches!(prefer, Some(MirType::ObjectRef))
}

/// Resolve the concrete WasmGC struct + field for an access at `offset`.
///
/// Prefers the object's `class_qual`, but falls back to any registered layout
/// that owns `offset` (and matches `prefer` when provided). Needed when a
/// `ref(A)` local holds a subclass instance and MIR uses the subclass field
/// offset, or when captures make the static qual thinner than the instance.
///
/// `qual_override`, when `Some`, is the point-in-time `class_qual` snapshot
/// carried on the triggering [`Op::FieldLoadI64`]/[`Op::FieldStoreI64`] and
/// takes priority over `function.local(object).class_qual` — see that
/// field's doc comment for why the two can legitimately disagree.
pub(in crate::codegen::wasm) fn resolve_gc_field(
    function: &MirFunction,
    object: LocalId,
    offset: i64,
    prefer: Option<MirType>,
    qual_override: Option<&str>,
) -> Result<(crate::codegen::wasm_gc::ClassGcInfo, u32, LayoutFieldType), CompileError> {
    let primary = resolve_gc_object_info_with_qual(function, object, qual_override).ok();
    if let Some(info) = primary.as_ref() {
        if let Some(&(field_index, field_ty)) = info.fields_by_offset.get(&offset) {
            if prefer.is_none_or(|ty| gc_field_compatible(field_ty, ty))
                && gc_link_field_serves(info, field_index, prefer)
            {
                // `class_id`/`suc`/`pred` (offsets 0/8/16) live at the same
                // field index on every linkage-family class's own struct
                // *and* on the shared `linkage_base` they're all a subtype
                // of. When the qualifier is only a supertype hint (any
                // sibling ring member may be the real runtime value — the
                // whole point of a SIMSET ring), casting to `linkage_base`
                // instead of this qualifier's own final struct type is just
                // as correct for these three fields and, unlike the exact
                // type, never traps on a differently-typed sibling
                // (`x.prev`/`chain.pred` walking a ring of `Bead`s while
                // qualified `Ref(Linkage)` — simtst93).
                if field_index <= 2 && info.is_linkage_family() {
                    if let Some(base_info) = gc_ctx(|ctx| ctx.linkage_base_info()).flatten() {
                        return Ok((base_info, field_index, field_ty));
                    }
                }
                return Ok((info.clone(), field_index, field_ty));
            }
        }
    }

    let fallback = gc_ctx(|ctx| {
        let mut best: Option<(crate::codegen::wasm_gc::ClassGcInfo, u32, LayoutFieldType)> = None;
        for (class_id, info) in &ctx.by_class_id {
            // Runtime helper structs (`ref_cell`, name-thunk envs, …) share
            // the same byte offsets as user attributes. They are only valid
            // when `class_qual` names them — never as a size-based fallback
            // (NAME_PACK_ENV would otherwise steal every ObjectRef at 8+).
            if *class_id >= crate::layout::REF_CELL_CLASS_ID {
                continue;
            }
            let Some(&(field_index, field_ty)) = info.fields_by_offset.get(&offset) else {
                continue;
            };
            if let Some(ty) = prefer {
                if !gc_field_compatible(field_ty, ty) {
                    continue;
                }
            }
            if !gc_link_field_serves(info, field_index, prefer) {
                continue;
            }
            let candidate = (info.clone(), field_index, field_ty);
            best = Some(match best {
                None => candidate,
                Some(prev) => {
                    if info.fields_by_offset.len() >= prev.0.fields_by_offset.len() {
                        candidate
                    } else {
                        prev
                    }
                }
            });
        }
        best
    })
    .flatten();

    if let Some(found) = fallback {
        return Ok(found);
    }

    // Last resort: any layout with this offset, ignoring MIR type preference.
    // Captures sometimes store RefI64/Array handles into slots whose static
    // layout type is ObjectRef (or the reverse) after prefix/inspect rewriting.
    // A ring class's typed SUC/PRED can't hold a word, so it is only taken
    // here when nothing else in the program owns the offset at all.
    let loose = gc_ctx(|ctx| {
        let mut best: Option<(crate::codegen::wasm_gc::ClassGcInfo, u32, LayoutFieldType)> = None;
        for (class_id, info) in &ctx.by_class_id {
            if *class_id >= crate::layout::REF_CELL_CLASS_ID {
                continue;
            }
            let Some(&(field_index, field_ty)) = info.fields_by_offset.get(&offset) else {
                continue;
            };
            let serves = gc_link_field_serves(info, field_index, prefer);
            let candidate = (info.clone(), field_index, field_ty);
            best = Some(match best {
                None => candidate,
                Some(prev) => {
                    let prev_serves = gc_link_field_serves(&prev.0, prev.1, prefer);
                    let better = match (serves, prev_serves) {
                        (true, false) => true,
                        (false, true) => false,
                        _ => info.fields_by_offset.len() >= prev.0.fields_by_offset.len(),
                    };
                    if better { candidate } else { prev }
                }
            });
        }
        best
    })
    .flatten();

    loose.ok_or_else(|| {
        let detail = gc_ctx(|ctx| {
            let mut offs: Vec<i64> = ctx
                .by_class_id
                .values()
                .flat_map(|info| info.fields_by_offset.keys().copied())
                .collect();
            offs.sort_unstable();
            offs.dedup();
            format!(
                "known offsets among {} classes: {:?} prefer={prefer:?}",
                ctx.by_class_id.len(),
                offs
            )
        })
        .unwrap_or_else(|| "GC_CTX missing".into());
        CompileError::codegen(format!(
            "MIR wasm: no WasmGC field at byte offset {offset} ({detail})"
        ))
    })
}

/// WasmGC type to name in the `ref.cast` guarding a `field_index` access on
/// `info`'s class.
///
/// [`resolve_gc_field`]'s answer is a *guess* at the object's class — a
/// stale `class_qual`, the declaring class of an outlined prefix method, or
/// (when there is no qualifier at all, as in `lower.rs`'s per-offset `name`
/// parameter thunks) whichever registered class happens to own the offset.
/// Casting to that guess traps on every object that isn't it. Field indices
/// are byte-offset slots that mean the same thing in every class, so
/// widening to the shallowest ancestor still declaring the slot reads the
/// intended value while accepting siblings, subclasses, and — via the slot
/// ladder — unrelated classes that are merely wide enough.
pub(in crate::codegen::wasm) fn gc_field_cast_target(
    info: &crate::codegen::wasm_gc::ClassGcInfo,
    field_index: u32,
) -> u32 {
    gc_ctx(|ctx| ctx.cast_target(info.wasm_ty, field_index)).unwrap_or(info.wasm_ty)
}

/// Whether `struct.get`/`struct.set` on `(ty, field_index)` moves one of
/// `linkage_base`'s `(ref null eq)` ring pointers instead of an `i64` slot.
pub(in crate::codegen::wasm) fn gc_is_direct_link_field(ty: u32, field_index: u32) -> bool {
    gc_ctx(|ctx| ctx.is_direct_link_field(ty, field_index)).unwrap_or(false)
}

/// Whether field `field_index` of WasmGC struct `ty` is a typed ref
/// (eqref / text-frame / array descriptor) rather than an `i64` word.
pub(in crate::codegen::wasm) fn gc_field_is_wasm_ref(ty: u32, field_index: u32) -> bool {
    gc_ctx(|ctx| ctx.field_is_wasm_ref(ty, field_index)).unwrap_or(false)
}

/// `dest := object.SUC/PRED` where those are direct WasmGC references
/// (Phase 4-R2) — a plain `struct.get`, no handle-table hop.
pub(in crate::codegen::wasm) fn emit_link_field_load_gc(
    body: &mut Function,
    dest: LocalId,
    object: LocalId,
    field_index: u32,
    dest_ty: MirType,
    ty: u32,
) -> Result<(), CompileError> {
    if !matches!(dest_ty, MirType::ObjectRef) {
        // `resolve_gc_field` only lands here when *no* class in the program
        // owns this byte offset as a word, so there is no slot to read and
        // nothing meaningful to hand back but `none`.
        body.instruction(&Instruction::I64Const(0));
        body.instruction(&Instruction::LocalSet(local_index(dest)));
        return Ok(());
    }
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: ty,
        field_index,
    });
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn gc_field_debug(
    what: &str,
    function: &MirFunction,
    ty: u32,
    field_index: u32,
    field_ty: LayoutFieldType,
    mir_ty: MirType,
    offset: i64,
    qual_override: Option<&str>,
) {
    if std::env::var_os("SIMRT_GC_FIELD_DEBUG").is_none() {
        return;
    }
    let field_is_ref = gc_field_is_wasm_ref(ty, field_index);
    let mir_is_ref = matches!(wasm_val_type(mir_ty), ValType::Ref(_));
    if field_is_ref == mir_is_ref {
        return;
    }
    eprintln!(
        "GCFIELD {what} fn={} off={offset} fi={field_index} ty={ty} \
         field_ty={field_ty:?} mir_ty={mir_ty} field_is_ref={field_is_ref} \
         link={} qual={qual_override:?}",
        function.name,
        gc_is_direct_link_field(ty, field_index),
    );
}

pub(in crate::codegen::wasm) fn emit_field_load_gc(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    object: LocalId,
    offset: i64,
    dest_ty: MirType,
    qual_override: Option<&str>,
    _scratch0: u32,
    scratch1: u32,
) -> Result<(), CompileError> {
    let (info, field_index, field_ty) =
        resolve_gc_field(function, object, offset, Some(dest_ty), qual_override)?;
    let ty = gc_field_cast_target(&info, field_index);
    gc_field_debug(
        "load",
        function,
        ty,
        field_index,
        field_ty,
        dest_ty,
        offset,
        qual_override,
    );
    if gc_is_direct_link_field(ty, field_index) {
        return emit_link_field_load_gc(body, dest, object, field_index, dest_ty, ty);
    }
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: ty,
        field_index,
    });
    match field_ty {
        LayoutFieldType::Bool => {
            // Slot-indexed class structs store booleans as i64 words.
            body.instruction(&Instruction::LocalSet(local_index(dest)));
        }
        LayoutFieldType::F64 => {
            // Slot-indexed class structs store reals as i64 bit patterns.
            if dest_ty.is_float() {
                body.instruction(&Instruction::F64ReinterpretI64);
            }
            body.instruction(&Instruction::LocalSet(local_index(dest)));
        }
        LayoutFieldType::I64 => {
            if dest_ty.is_float() {
                body.instruction(&Instruction::F64ReinterpretI64);
            }
            body.instruction(&Instruction::LocalSet(local_index(dest)));
        }
        LayoutFieldType::Text
        | LayoutFieldType::ArrayI64
        | LayoutFieldType::ArrayBool
        | LayoutFieldType::ArrayF64
        | LayoutFieldType::ArrayText
        | LayoutFieldType::ObjectRef => {
            if gc_field_is_wasm_ref(ty, field_index) && field_ref_type_match(field_ty, dest_ty) {
                // Typed WasmGC field: `struct.get` already produced the ref.
                if let Some(heap) = gc_heap_for(dest_ty) {
                    body.instruction(&Instruction::RefCastNullable(heap));
                }
                body.instruction(&Instruction::LocalSet(local_index(dest)));
            } else if field_ref_type_match(field_ty, dest_ty) {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: field {field_index} of type {ty} is still an i64 \
                     slot but dest is {dest_ty}; reference-typed attributes \
                     must be typed WasmGC fields"
                )));
            } else {
                // RefI64 / I64 writeback cell address (or raw handle word)
                body.instruction(&Instruction::LocalSet(local_index(dest)));
            }
        }
    }
    let _ = scratch1;
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_field_store_gc(
    body: &mut Function,
    function: &MirFunction,
    object: LocalId,
    offset: i64,
    value: LocalId,
    value_ty: MirType,
    qual_override: Option<&str>,
    scratch0: u32,
    scratch1: u32,
    scratch2: u32,
) -> Result<(), CompileError> {
    let (info, field_index, field_ty) =
        resolve_gc_field(function, object, offset, Some(value_ty), qual_override)?;
    let ty = gc_field_cast_target(&info, field_index);
    gc_field_debug(
        "store",
        function,
        ty,
        field_index,
        field_ty,
        value_ty,
        offset,
        qual_override,
    );

    if gc_is_direct_link_field(ty, field_index) {
        // See `emit_link_field_load_gc`: a non-`ObjectRef` value has no ring
        // pointer to land in, and writing one would corrupt the ring.
        if matches!(value_ty, MirType::ObjectRef) {
            body.instruction(&Instruction::LocalGet(local_index(object)));
            body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
            body.instruction(&Instruction::LocalGet(local_index(value)));
            body.instruction(&Instruction::StructSet {
                struct_type_index: ty,
                field_index,
            });
        }
        return Ok(());
    }

    if gc_field_is_wasm_ref(ty, field_index) && field_ref_type_match(field_ty, value_ty) {
        body.instruction(&Instruction::LocalGet(local_index(object)));
        body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
        body.instruction(&Instruction::LocalGet(local_index(value)));
        body.instruction(&Instruction::StructSet {
            struct_type_index: ty,
            field_index,
        });
        return Ok(());
    }

    if field_ref_type_match(field_ty, value_ty) {
        return Err(CompileError::codegen(format!(
            "MIR wasm: field {field_index} of type {ty} is still an i64 slot \
             but value is {value_ty}; reference-typed attributes must be typed \
             WasmGC fields"
        )));
    }

    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
    body.instruction(&Instruction::LocalGet(local_index(value)));
    match field_ty {
        LayoutFieldType::Bool => {
            // Slot-indexed class structs store booleans as i64 words.
        }
        LayoutFieldType::F64 => {
            // Slot-indexed class structs store reals as i64 bit patterns.
            if value_ty.is_float() {
                body.instruction(&Instruction::I64ReinterpretF64);
            }
        }
        LayoutFieldType::I64
        | LayoutFieldType::ObjectRef
        | LayoutFieldType::Text
        | LayoutFieldType::ArrayI64
        | LayoutFieldType::ArrayBool
        | LayoutFieldType::ArrayF64
        | LayoutFieldType::ArrayText => {
            // RefI64/I64 writeback address (or raw handle word): store the
            // raw i64 word. The matching-ref case is handled above.
            if value_ty.is_float() {
                body.instruction(&Instruction::I64ReinterpretF64);
            }
        }
    }
    body.instruction(&Instruction::StructSet {
        struct_type_index: ty,
        field_index,
    });
    let _ = (scratch0, scratch1, scratch2);
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_object_class_id_safe_gc(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    object: LocalId,
) -> Result<(), CompileError> {
    // Every registered class extends the universal `class_base` (`[class_id]`),
    // so `IS`/`IN`/`QUA`/`inspect` can always read `class_id` by casting to
    // that root — never to the local's (often-wrong) static class_qual.
    let _ = function;
    let ty = gc_ctx(|ctx| ctx.class_base_ty).flatten().ok_or_else(|| {
        CompileError::codegen(
            "MIR wasm: ObjectClassIdSafe requires a WasmGC class_base type \
                 (no class registered)",
        )
    })?;
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    body.instruction(&Instruction::I64Const(-1));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ty)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: ty,
        field_index: 0,
    });
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_new_object(
    body: &mut Function,
    dest: LocalId,
    class_id: i64,
    size: i32,
    s0: u32,
    s1: u32,
    s2: u32,
) {
    emit_bump_alloc(body, size, s0);
    emit_zero_fill(body, s0, size, s1, s2);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I64Const(class_id));
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

pub(in crate::codegen::wasm) fn emit_object_ptr_or_trap(
    body: &mut Function,
    object: LocalId,
    scratch: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalTee(scratch));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_field_load_i64(
    body: &mut Function,
    dest: LocalId,
    object: LocalId,
    offset: i64,
    scratch: u32,
    dest_ty: MirType,
) {
    emit_object_ptr_or_trap(body, object, scratch);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }));
    if dest_ty.is_float() {
        body.instruction(&Instruction::F64ReinterpretI64);
    }
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

pub(in crate::codegen::wasm) fn emit_field_store_i64(
    body: &mut Function,
    object: LocalId,
    offset: i64,
    value: LocalId,
    scratch: u32,
    value_ty: MirType,
) {
    emit_object_ptr_or_trap(body, object, scratch);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::LocalGet(local_index(value)));
    if value_ty.is_float() {
        body.instruction(&Instruction::I64ReinterpretF64);
    }
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }));
}

pub(in crate::codegen::wasm) fn emit_object_class_id_safe(
    body: &mut Function,
    dest: LocalId,
    object: LocalId,
    scratch: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I64Eqz);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    body.instruction(&Instruction::I64Const(-1));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

// ---------------------------------------------------------------------- SIMSET
//
// A SIMSET list is the doubly linked ring of §12: every `Linkage` carries `SUC`
// and `PRED` at fixed offsets, and the `Head` closes the ring. References are
// the same i64 pointers the rest of the object model uses (0 = none), so these
// helpers just move i32 addresses between the two link slots. `Suc` / `Pred`
// report none when they land on the Head, which is what makes the ring look
// like a list from the outside.

/// Pushes the i32 address of an object-ref local (0 for none).
pub(in crate::codegen::wasm) fn emit_ref_ptr(body: &mut Function, object: LocalId) {
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32WrapI64);
}
