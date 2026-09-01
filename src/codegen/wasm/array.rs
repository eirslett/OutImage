//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// Largest element count an array may declare. `count * 8` then stays a
/// positive `i32` with room to spare for the descriptor header, so neither the
/// byte size nor the zero-fill loop can wrap. Nothing is lost by capping here:
/// a 2 GiB payload already exceeds what `memory.grow` can hand back.
pub(in crate::codegen::wasm) const MAX_ARRAY_ELEMENTS: i32 = 0x0FF0_0000;

/// Traps unless the `i64` bound in `bound` is representable as an `i32`. Bounds
/// are stored in the descriptor as `i64` but sizes are computed in `i32`, so a
/// bound outside that range would wrap into a short allocation that the `i64`
/// bounds checks in [`emit_array_linear_index`] then happily index past.
pub(in crate::codegen::wasm) fn emit_bound_fits_i32_or_trap(body: &mut Function, bound: LocalId) {
    body.instruction(&Instruction::LocalGet(local_index(bound)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I64ExtendI32S);
    body.instruction(&Instruction::LocalGet(local_index(bound)));
    body.instruction(&Instruction::I64Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
}

/// N-D array descriptor matching `runtime/runtime.c`:
/// `{ ndims:i64, bounds[2*ndims]:i64, data[count]:i64 }` (text slots store frame ptrs).
///
/// Leaves `high >= low ? high - low + 1 : 0` on the stack as an `i32`, trapping
/// when that does not fit: the subtraction runs in `i64` so a legal `i32` bound
/// pair cannot wrap it.
pub(in crate::codegen::wasm) fn emit_dim_size_i32(
    body: &mut Function,
    low: LocalId,
    high: LocalId,
) {
    emit_bound_fits_i32_or_trap(body, low);
    emit_bound_fits_i32_or_trap(body, high);
    body.instruction(&Instruction::LocalGet(local_index(high)));
    body.instruction(&Instruction::LocalGet(local_index(low)));
    body.instruction(&Instruction::I64GeS);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::LocalGet(local_index(high)));
    body.instruction(&Instruction::LocalGet(local_index(low)));
    body.instruction(&Instruction::I64Sub);
    body.instruction(&Instruction::I64Const(MAX_ARRAY_ELEMENTS as i64 - 1));
    body.instruction(&Instruction::I64GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(high)));
    body.instruction(&Instruction::LocalGet(local_index(low)));
    body.instruction(&Instruction::I64Sub);
    body.instruction(&Instruction::I64Const(1));
    body.instruction(&Instruction::I64Add);
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::End);
}

/// `count = product of the size of each dimension in bounds`, trapping instead
/// of wrapping. `size` and `prev` are scratch `i32` locals; `count` ends up
/// within [`MAX_ARRAY_ELEMENTS`], so `count * 8 + header` is a safe `i32` add.
pub(in crate::codegen::wasm) fn emit_array_count_checked(
    body: &mut Function,
    bounds: &[(LocalId, LocalId)],
    count: u32,
    size: u32,
    prev: u32,
) {
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::LocalSet(count));
    for &(low, high) in bounds {
        emit_dim_size_i32(body, low, high);
        body.instruction(&Instruction::LocalSet(size));
        body.instruction(&Instruction::LocalGet(count));
        body.instruction(&Instruction::LocalSet(prev));
        body.instruction(&Instruction::LocalGet(count));
        body.instruction(&Instruction::LocalGet(size));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::LocalSet(count));
        emit_mul_did_not_wrap_or_trap(body, count, size, prev);
    }
    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I32Const(MAX_ARRAY_ELEMENTS));
    body.instruction(&Instruction::I32GtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
}

/// Traps when `product` is not the exact unsigned product of `prev` and `size`,
/// i.e. when the `i32.mul` that produced it wrapped. A zero `size` makes the
/// product trivially exact and the division undefined, so it is skipped.
pub(in crate::codegen::wasm) fn emit_mul_did_not_wrap_or_trap(
    body: &mut Function,
    product: u32,
    size: u32,
    prev: u32,
) {
    body.instruction(&Instruction::LocalGet(size));
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(product));
    body.instruction(&Instruction::LocalGet(size));
    body.instruction(&Instruction::I32DivU);
    body.instruction(&Instruction::LocalGet(prev));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// Which kind of element an array descriptor holds under WasmGC, and thus
/// which scratch ref-local (`ae0`/`afe0`/`atxe0`/`aoe0`) a given array op
/// should route its `elems` traffic through.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::codegen::wasm) enum ArrayElemKind {
    I64,
    F64,
    Text,
    Object,
}

impl ArrayElemKind {
    /// Descriptor field holding this kind's element spine — `ref(T) array`s
    /// carry theirs in `array_object`'s extra field, everyone else in the
    /// shared `elems` field (see [`crate::codegen::wasm_gc::array_object`]).
    ///
    /// [`crate::codegen::wasm_gc::array_object`]: crate::codegen::wasm_gc::GcTypeRegistry::array_object
    fn elems_field(self) -> u32 {
        match self {
            ArrayElemKind::Object => crate::codegen::wasm_gc::ARRAY_DESC_FIELD_OBJECT_ELEMS,
            _ => crate::codegen::wasm_gc::ARRAY_DESC_FIELD_ELEMS,
        }
    }
}

/// `(descriptor wasm type, elems wasm type, element kind)` for `array`'s
/// descriptor under WasmGC.
///
/// The MIR array type alone does not settle this: a `ref(T) array` is an
/// `ArrayI64` whose elements are `ObjectRef`, and those get the `array_object`
/// descriptor (a subtype of `array_i64`) with a `(ref null eq)` spine rather
/// than the plain `i64` one — see [`crate::mir::Function::array_elem_ty`].
pub(in crate::codegen::wasm) fn array_gc_type_info(
    function: &MirFunction,
    array: LocalId,
) -> Result<(u32, u32, ArrayElemKind), CompileError> {
    let missing = || CompileError::codegen("MIR wasm: WasmGC context missing for array op");
    match function.local(array).ty {
        MirType::ArrayI64 if function.array_elem_ty(array) == MirType::ObjectRef => gc_ctx(|ctx| {
            (
                ctx.array_object_ty,
                ctx.array_object_elems_ty,
                ArrayElemKind::Object,
            )
        })
        .ok_or_else(missing),
        MirType::ArrayI64 => {
            gc_ctx(|ctx| (ctx.array_i64_ty, ctx.array_i64_elems_ty, ArrayElemKind::I64))
                .ok_or_else(missing)
        }
        MirType::ArrayF64 => {
            gc_ctx(|ctx| (ctx.array_f64_ty, ctx.array_f64_elems_ty, ArrayElemKind::F64))
                .ok_or_else(missing)
        }
        MirType::ArrayText => gc_ctx(|ctx| {
            (
                ctx.array_text_ty,
                ctx.array_text_elems_ty,
                ArrayElemKind::Text,
            )
        })
        .ok_or_else(missing),
        other => Err(CompileError::codegen(format!(
            "MIR wasm: unexpected array type {other:?} in WasmGC array op"
        ))),
    }
}

/// Pushes `array`'s descriptor ref, narrowed to `descriptor_ty`.
///
/// `MirType::ArrayI64` locals and parameters are all typed
/// `(ref null $array_i64)`, so a `ref(T) array` — whose descriptor is the
/// `array_object` subtype — has to be cast back down before its extra
/// element-spine field is reachable. Every other kind already has its exact
/// descriptor type in hand.
pub(in crate::codegen::wasm) fn emit_array_descriptor_ref(
    body: &mut Function,
    array: LocalId,
    kind: ArrayElemKind,
    descriptor_ty: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(array)));
    if kind == ArrayElemKind::Object {
        body.instruction(&Instruction::RefCastNullable(
            crate::codegen::wasm_gc::concrete_heap(descriptor_ty),
        ));
    }
}

/// Row-major linear index into `bounds_ref`'s described elems array, mirroring
/// [`emit_array_linear_index`]'s bump algorithm field-for-field but reading
/// `(low, high)` pairs via `array.get` on the `bounds_array` ref (index
/// `2*dim`/`2*dim+1`) instead of `i64.load` on a linear-memory descriptor.
/// Traps (via `unreachable`) on an empty dimension or an out-of-bounds index,
/// same as the bump path.
pub(in crate::codegen::wasm) fn emit_array_linear_index_gc(
    body: &mut Function,
    bounds_ref: u32,
    bounds_ty: u32,
    indices: &[LocalId],
    s_lin: u32,
    s_stride: u32,
    s_tmp: u32,
) {
    let ndims = indices.len();
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s_lin));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::LocalSet(s_stride));
    for dim in (0..ndims).rev() {
        let low_idx = (2 * dim) as i32;
        let high_idx = (2 * dim + 1) as i32;
        let index = indices[dim];
        let get_low = |body: &mut Function| {
            body.instruction(&Instruction::LocalGet(bounds_ref));
            body.instruction(&Instruction::I32Const(low_idx));
            body.instruction(&Instruction::ArrayGet(bounds_ty));
        };
        let get_high = |body: &mut Function| {
            body.instruction(&Instruction::LocalGet(bounds_ref));
            body.instruction(&Instruction::I32Const(high_idx));
            body.instruction(&Instruction::ArrayGet(bounds_ty));
        };
        // empty dim: low > high
        get_low(body);
        get_high(body);
        body.instruction(&Instruction::I64GtS);
        body.instruction(&Instruction::If(BlockType::Empty));
        body.instruction(&Instruction::Unreachable);
        body.instruction(&Instruction::End);
        // index < low
        body.instruction(&Instruction::LocalGet(local_index(index)));
        get_low(body);
        body.instruction(&Instruction::I64LtS);
        body.instruction(&Instruction::If(BlockType::Empty));
        body.instruction(&Instruction::Unreachable);
        body.instruction(&Instruction::End);
        // index > high
        body.instruction(&Instruction::LocalGet(local_index(index)));
        get_high(body);
        body.instruction(&Instruction::I64GtS);
        body.instruction(&Instruction::If(BlockType::Empty));
        body.instruction(&Instruction::Unreachable);
        body.instruction(&Instruction::End);
        // linear += (index - low) * stride
        body.instruction(&Instruction::LocalGet(local_index(index)));
        get_low(body);
        body.instruction(&Instruction::I64Sub);
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::LocalGet(s_stride));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::LocalGet(s_lin));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s_lin));
        // stride *= size (size = high - low + 1)
        get_high(body);
        body.instruction(&Instruction::I32WrapI64);
        get_low(body);
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::I32Sub);
        body.instruction(&Instruction::I32Const(1));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s_tmp));
        body.instruction(&Instruction::LocalGet(s_stride));
        body.instruction(&Instruction::LocalGet(s_tmp));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::LocalSet(s_stride));
    }
}

/// `ArrayI64`/`ArrayF64`/`ArrayText` allocation under WasmGC: a fresh
/// descriptor struct `{ elems, ndims, bounds }` — `bounds` is a fixed-size
/// `(array i64)` built directly from the `(low, high)` operands via
/// `array.new_fixed` (no linear-memory bounds record). Element-count
/// overflow checking reuses [`emit_array_count_checked`] verbatim — it is
/// pure `i32` arithmetic over the same `i64` bound locals, with no
/// bump/linear-memory access at all.
///
/// `elems` for `ArrayI64`/`ArrayF64`/`ref(T) array` is a zero- (resp. null-)
/// filled `array.new_default` (mirrors bump's zero-fill loop). `ArrayText`'s
/// `elems` holds non-null `text_frame` refs (there is no zero value for a ref
/// field), so it's filled by a loop storing a fresh notext frame
/// ([`emit_push_notext_frame`]) into every slot — mirrors bump's per-slot
/// notext `FRAME` fill.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_array_alloc_nd_gc(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    bounds: &[(LocalId, LocalId)],
    s0: u32,
    s1: u32,
    s2: u32,
    ab0: u32,
    ae0: u32,
    afe0: u32,
    atxe0: u32,
    aoe0: u32,
) -> Result<(), CompileError> {
    let (descriptor_ty, elems_ty, kind) = array_gc_type_info(function, dest)?;
    let bounds_ty = gc_ctx(|ctx| ctx.bounds_array_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for array op"))?;
    emit_array_count_checked(body, bounds, s0, s1, s2);
    let elems_scratch = match kind {
        ArrayElemKind::F64 => afe0,
        ArrayElemKind::I64 => ae0,
        ArrayElemKind::Text => atxe0,
        ArrayElemKind::Object => aoe0,
    };
    match kind {
        ArrayElemKind::I64 | ArrayElemKind::F64 | ArrayElemKind::Object => {
            body.instruction(&Instruction::LocalGet(s0));
            body.instruction(&Instruction::ArrayNewDefault(elems_ty));
            body.instruction(&Instruction::LocalSet(elems_scratch));
        }
        ArrayElemKind::Text => {
            body.instruction(&Instruction::LocalGet(s0));
            body.instruction(&Instruction::ArrayNewDefault(elems_ty));
            body.instruction(&Instruction::LocalSet(elems_scratch));
            // Fill every slot with a fresh notext frame (no shared aliasing
            // between elements, matching bump's per-slot FRAME allocation).
            body.instruction(&Instruction::I32Const(0));
            body.instruction(&Instruction::LocalSet(s1)); // loop index
            body.instruction(&Instruction::Block(BlockType::Empty));
            body.instruction(&Instruction::Loop(BlockType::Empty));
            body.instruction(&Instruction::LocalGet(s1));
            body.instruction(&Instruction::LocalGet(s0));
            body.instruction(&Instruction::I32GeS);
            body.instruction(&Instruction::BrIf(1));
            body.instruction(&Instruction::LocalGet(elems_scratch));
            body.instruction(&Instruction::LocalGet(s1));
            emit_push_notext_frame(body)?;
            body.instruction(&Instruction::ArraySet(elems_ty));
            body.instruction(&Instruction::LocalGet(s1));
            body.instruction(&Instruction::I32Const(1));
            body.instruction(&Instruction::I32Add);
            body.instruction(&Instruction::LocalSet(s1));
            body.instruction(&Instruction::Br(0));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::End);
        }
    }
    for &(low, high) in bounds {
        body.instruction(&Instruction::LocalGet(local_index(low)));
        body.instruction(&Instruction::LocalGet(local_index(high)));
    }
    body.instruction(&Instruction::ArrayNewFixed {
        array_type_index: bounds_ty,
        array_size: (bounds.len() * 2) as u32,
    });
    body.instruction(&Instruction::LocalSet(ab0));
    let ndims = bounds.len() as i32;
    emit_push_array_descriptor(
        body,
        kind,
        elems_scratch,
        |body| {
            body.instruction(&Instruction::I32Const(ndims));
        },
        ab0,
    );
    body.instruction(&Instruction::StructNew(descriptor_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// Pushes the `struct.new` operands of an array descriptor: `elems`, `ndims`,
/// `bounds` — plus, for `ref(T) array`s, the `(ref null eq)` spine in
/// `array_object`'s extra field, with the inherited `i64` `elems` field left
/// null (nothing reads it on those descriptors).
pub(in crate::codegen::wasm) fn emit_push_array_descriptor(
    body: &mut Function,
    kind: ArrayElemKind,
    elems: u32,
    push_ndims: impl FnOnce(&mut Function),
    bounds: u32,
) {
    if kind == ArrayElemKind::Object {
        body.instruction(&Instruction::RefNull(
            crate::codegen::wasm_gc::concrete_heap(
                gc_ctx(|ctx| ctx.array_i64_elems_ty).expect("GC_CTX set for array ops"),
            ),
        ));
    } else {
        body.instruction(&Instruction::LocalGet(elems));
    }
    push_ndims(body);
    body.instruction(&Instruction::LocalGet(bounds));
    if kind == ArrayElemKind::Object {
        body.instruction(&Instruction::LocalGet(elems));
    }
}

/// `A(i1, ..., in)` read under WasmGC: computes the row-major linear index via
/// [`emit_array_linear_index_gc`] against the descriptor's `bounds` ref, then
/// `array.get`s the element straight out of the descriptor's `elems` ref — no
/// linear-memory address arithmetic at all.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_array_load_nd_gc(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    array: LocalId,
    indices: &[LocalId],
    ab0: u32,
    ae0: u32,
    afe0: u32,
    atxe0: u32,
    aoe0: u32,
    s0: u32,
    s1: u32,
    s2: u32,
) -> Result<(), CompileError> {
    let (descriptor_ty, elems_ty, kind) = array_gc_type_info(function, array)?;
    let bounds_ty = gc_ctx(|ctx| ctx.bounds_array_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for array op"))?;
    emit_array_descriptor_ref(body, array, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: crate::codegen::wasm_gc::ARRAY_DESC_FIELD_BOUNDS,
    });
    body.instruction(&Instruction::LocalSet(ab0));
    emit_array_linear_index_gc(body, ab0, bounds_ty, indices, s0, s1, s2);
    let elems_scratch = match kind {
        ArrayElemKind::F64 => afe0,
        ArrayElemKind::I64 => ae0,
        ArrayElemKind::Text => atxe0,
        ArrayElemKind::Object => aoe0,
    };
    emit_array_descriptor_ref(body, array, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: kind.elems_field(),
    });
    body.instruction(&Instruction::LocalSet(elems_scratch));
    body.instruction(&Instruction::LocalGet(elems_scratch));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::ArrayGet(elems_ty));
    if kind == ArrayElemKind::Object {
        // The spine is `(ref null eq)`; a `ref(T)` local may want a narrower
        // heap type (`Text`/array destinations never come from this kind).
        if let Some(heap) = gc_heap_for(function.local(dest).ty) {
            body.instruction(&Instruction::RefCastNullable(heap));
        }
    }
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// Pushes a fresh `text_frame` struct sharing `src`'s current `chars`
/// (view/alias, not a content copy) plus its own `start`/`length`/`pos`/
/// `constant` snapshot.
///
/// Needed wherever a text value gets stored into a slot with its **own**
/// frame identity (array elements; see [`emit_array_store_nd_gc`]) rather
/// than a slot whose `:-` semantics mutate an existing, persistent frame
/// object in place (locals/attributes — [`emit_text_ref_assign_gc`]). If we
/// instead stored `src`'s own struct *reference* directly, every array slot
/// ever assigned from the same local would alias that local's one
/// persistent frame object, so a later `:-`/`putchar`/etc. on the local
/// would retroactively change *every* previously-stored array element too —
/// this is exactly the bump path's per-slot inline frame storage semantics,
/// just realized as a fresh heap struct instead of an inline copy.
pub(in crate::codegen::wasm) fn emit_push_text_frame_view_copy(
    body: &mut Function,
    src: LocalId,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    for field in [
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    ] {
        body.instruction(&Instruction::LocalGet(local_index(src)));
        body.instruction(&Instruction::StructGet {
            struct_type_index: frame_ty,
            field_index: field,
        });
    }
    body.instruction(&Instruction::StructNew(frame_ty));
    Ok(())
}

/// `A(i1, ..., in) := value` under WasmGC — mirror of
/// [`emit_array_load_nd_gc`] using `array.set` instead of `array.get`.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_array_store_nd_gc(
    body: &mut Function,
    function: &MirFunction,
    array: LocalId,
    indices: &[LocalId],
    value: LocalId,
    ab0: u32,
    ae0: u32,
    afe0: u32,
    atxe0: u32,
    aoe0: u32,
    s0: u32,
    s1: u32,
    s2: u32,
) -> Result<(), CompileError> {
    let (descriptor_ty, elems_ty, kind) = array_gc_type_info(function, array)?;
    let bounds_ty = gc_ctx(|ctx| ctx.bounds_array_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for array op"))?;
    emit_array_descriptor_ref(body, array, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: crate::codegen::wasm_gc::ARRAY_DESC_FIELD_BOUNDS,
    });
    body.instruction(&Instruction::LocalSet(ab0));
    emit_array_linear_index_gc(body, ab0, bounds_ty, indices, s0, s1, s2);
    let elems_scratch = match kind {
        ArrayElemKind::F64 => afe0,
        ArrayElemKind::I64 => ae0,
        ArrayElemKind::Text => atxe0,
        ArrayElemKind::Object => aoe0,
    };
    emit_array_descriptor_ref(body, array, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: kind.elems_field(),
    });
    body.instruction(&Instruction::LocalSet(elems_scratch));
    body.instruction(&Instruction::LocalGet(elems_scratch));
    body.instruction(&Instruction::LocalGet(s0));
    if kind == ArrayElemKind::Text {
        // Give this array slot its own frame identity — see
        // `emit_push_text_frame_view_copy`'s doc comment.
        emit_push_text_frame_view_copy(body, value)?;
    } else {
        body.instruction(&Instruction::LocalGet(local_index(value)));
    }
    body.instruction(&Instruction::ArraySet(elems_ty));
    Ok(())
}

/// Deep-copies an array descriptor under WasmGC for call-by-value
/// transmission — mirror of [`emit_array_copy_nd`]'s semantics (fresh
/// `bounds` and `elems`, so mutating the copy never affects `src`), but
/// built from ref-typed structs instead of a linear-memory blob:
///
/// - `bounds`: fresh `(array i64)` of the same length, filled via a single
///   `array.copy` (dimension count isn't known at compile time here, so the
///   length comes from `array.len` on `src`'s bounds ref at runtime).
/// - `elems` for `ArrayI64`/`ArrayF64`/`ref(T) array`: fresh array of the
///   same length, filled via a single `array.copy` (plain values — or, for
///   `ref(T)`, references whose *identity* is exactly what a copy must
///   preserve — so bulk-copy is safe).
/// - `elems` for `ArrayText`: fresh array of the same length, filled
///   element-by-element with an independent [`emit_push_text_copy_from`]
///   copy of each source frame (bulk `array.copy` would alias the same
///   `text_frame` structs between `src` and `dest`, breaking by-value
///   semantics — matches bump's per-slot `simrt_array_copy_text`).
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_array_copy_nd_gc(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    src: LocalId,
    ab0: u32,
    ae0: u32,
    afe0: u32,
    atxe0: u32,
    aoe0: u32,
    tf1: u32,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    let (descriptor_ty, elems_ty, kind) = array_gc_type_info(function, src)?;
    let bounds_ty = gc_ctx(|ctx| ctx.bounds_array_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for array op"))?;

    // Fresh bounds array, bulk-copied from src (plain i64 pairs).
    emit_array_descriptor_ref(body, src, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: crate::codegen::wasm_gc::ARRAY_DESC_FIELD_BOUNDS,
    });
    body.instruction(&Instruction::ArrayLen);
    body.instruction(&Instruction::LocalSet(s0)); // bounds length (ndims*2)
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::ArrayNewDefault(bounds_ty));
    body.instruction(&Instruction::LocalSet(ab0));
    body.instruction(&Instruction::LocalGet(ab0));
    body.instruction(&Instruction::I32Const(0));
    emit_array_descriptor_ref(body, src, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: crate::codegen::wasm_gc::ARRAY_DESC_FIELD_BOUNDS,
    });
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: bounds_ty,
        array_type_index_src: bounds_ty,
    });

    // Fresh elems array of the same length.
    emit_array_descriptor_ref(body, src, kind, descriptor_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: descriptor_ty,
        field_index: kind.elems_field(),
    });
    body.instruction(&Instruction::ArrayLen);
    body.instruction(&Instruction::LocalSet(s1)); // element count
    let elems_scratch = match kind {
        ArrayElemKind::F64 => afe0,
        ArrayElemKind::I64 => ae0,
        ArrayElemKind::Text => atxe0,
        ArrayElemKind::Object => aoe0,
    };
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::ArrayNewDefault(elems_ty));
    body.instruction(&Instruction::LocalSet(elems_scratch));
    match kind {
        ArrayElemKind::I64 | ArrayElemKind::F64 | ArrayElemKind::Object => {
            body.instruction(&Instruction::LocalGet(elems_scratch));
            body.instruction(&Instruction::I32Const(0));
            emit_array_descriptor_ref(body, src, kind, descriptor_ty);
            body.instruction(&Instruction::StructGet {
                struct_type_index: descriptor_ty,
                field_index: kind.elems_field(),
            });
            body.instruction(&Instruction::I32Const(0));
            body.instruction(&Instruction::LocalGet(s1));
            body.instruction(&Instruction::ArrayCopy {
                array_type_index_dst: elems_ty,
                array_type_index_src: elems_ty,
            });
        }
        ArrayElemKind::Text => {
            body.instruction(&Instruction::I32Const(0));
            body.instruction(&Instruction::LocalSet(s2)); // loop index
            body.instruction(&Instruction::Block(BlockType::Empty));
            body.instruction(&Instruction::Loop(BlockType::Empty));
            body.instruction(&Instruction::LocalGet(s2));
            body.instruction(&Instruction::LocalGet(s1));
            body.instruction(&Instruction::I32GeS);
            body.instruction(&Instruction::BrIf(1));
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::StructGet {
                struct_type_index: descriptor_ty,
                field_index: crate::codegen::wasm_gc::ARRAY_DESC_FIELD_ELEMS,
            });
            body.instruction(&Instruction::LocalGet(s2));
            body.instruction(&Instruction::ArrayGet(elems_ty));
            body.instruction(&Instruction::LocalSet(tf1));
            body.instruction(&Instruction::LocalGet(elems_scratch));
            body.instruction(&Instruction::LocalGet(s2));
            emit_push_text_copy_from(body, tf1, s0, s3, ch0)?;
            body.instruction(&Instruction::ArraySet(elems_ty));
            body.instruction(&Instruction::LocalGet(s2));
            body.instruction(&Instruction::I32Const(1));
            body.instruction(&Instruction::I32Add);
            body.instruction(&Instruction::LocalSet(s2));
            body.instruction(&Instruction::Br(0));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::End);
        }
    }

    emit_push_array_descriptor(
        body,
        kind,
        elems_scratch,
        |body| {
            emit_array_descriptor_ref(body, src, kind, descriptor_ty);
            body.instruction(&Instruction::StructGet {
                struct_type_index: descriptor_ty,
                field_index: crate::codegen::wasm_gc::ARRAY_DESC_FIELD_NDIMS,
            });
        },
        ab0,
    );
    body.instruction(&Instruction::StructNew(descriptor_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_array_alloc_nd(
    body: &mut Function,
    dest: LocalId,
    bounds: &[(LocalId, LocalId)],
    is_text: bool,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    s4: u32,
    s5: u32,
) {
    let ndims = bounds.len();
    let header = 8 + ndims * 16;
    // s2 = element count (product of per-dim sizes), s3/s4 scratch
    emit_array_count_checked(body, bounds, s2, s3, s4);
    // s3 = bytes = header + count*8
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(8));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::I32Const(header as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s3));
    // s0 = bump base
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalTee(s0));
    emit_heap_grow_if_needed(body, s0, BumpSize::Dynamic(s3));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    // zero block
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    // ndims
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I64Const(ndims as i64));
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    for (dim, &(low, high)) in bounds.iter().enumerate() {
        let low_off = 8 + dim * 16;
        let high_off = 16 + dim * 16;
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::LocalGet(local_index(low)));
        body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
            offset: low_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::LocalGet(local_index(high)));
        body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
            offset: high_off as u64,
            align: 3,
            memory_index: 0,
        }));
    }
    if is_text {
        // Recompute count into s2 (bounds locals still valid).
        emit_array_count_checked(body, bounds, s2, s3, s4);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(header as i32));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s1));
        body.instruction(&Instruction::Block(BlockType::Empty));
        body.instruction(&Instruction::Loop(BlockType::Empty));
        body.instruction(&Instruction::LocalGet(s2));
        body.instruction(&Instruction::I32Eqz);
        body.instruction(&Instruction::BrIf(1));
        emit_bump_alloc(body, FRAME_SIZE, s3);
        emit_frame_store_const(body, s3, FRAME_OFF_PTR, 0);
        emit_frame_store_const(body, s3, FRAME_OFF_LEN, 0);
        emit_frame_store_const(body, s3, FRAME_OFF_POS, 1);
        emit_frame_store_const(body, s3, FRAME_OFF_PAD, 1);
        emit_frame_store_const(body, s3, FRAME_OFF_START, 1);
        emit_frame_store_const(body, s3, FRAME_OFF_MAIN_LEN, 0);
        body.instruction(&Instruction::LocalGet(s1));
        body.instruction(&Instruction::LocalGet(s3));
        body.instruction(&Instruction::I64ExtendI32U);
        body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::LocalGet(s1));
        body.instruction(&Instruction::I32Const(8));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s1));
        body.instruction(&Instruction::LocalGet(s2));
        body.instruction(&Instruction::I32Const(1));
        body.instruction(&Instruction::I32Sub);
        body.instruction(&Instruction::LocalSet(s2));
        body.instruction(&Instruction::Br(0));
        body.instruction(&Instruction::End);
        body.instruction(&Instruction::End);
    }
    let _ = s5;
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// Deep-copies an array descriptor for call-by-value transmission.
///
/// Layout matches [`emit_array_alloc_nd`]: `{ ndims:i64, bounds[ndims](lo,hi):i64, data… }`.
/// Integer and text elements are fully isolated (text slots get fresh frames via
/// content copy, matching native `simrt_array_copy_text`).
pub(in crate::codegen::wasm) fn emit_array_copy_nd(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
    is_text: bool,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    s4: u32,
    s5: u32,
    s6: u32,
    s7: u32,
) {
    // s0 = src base
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // s1 = ndims
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    // s2 = header = 8 + ndims*16
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(16));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::I32Const(8));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    // s3 = element count
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s4)); // dim index
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I32Const(16));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 8,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s5)); // low
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I32Const(16));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 16,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalGet(s5));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s5));
    body.instruction(&Instruction::LocalGet(s5));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s5));
    body.instruction(&Instruction::End);
    // s6 is a free temp until the deep-copy pass claims it below, so the
    // running count survives long enough to check the multiply for wrap.
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalSet(s6));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalGet(s5));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::LocalSet(s3));
    emit_mul_did_not_wrap_or_trap(body, s3, s5, s6);
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s4));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(MAX_ARRAY_ELEMENTS));
    body.instruction(&Instruction::I32GtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    // Preserve count/header for the text deep-copy pass (s6=count, s7=header).
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalSet(s6));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::LocalSet(s7));
    // s4 = total bytes = header + count*8
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(8));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s4));
    // s5 = dest base (bump)
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s5));
    emit_heap_grow_if_needed(body, s5, BumpSize::Dynamic(s4));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s5));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    // byte-copy src → dest
    body.instruction(&Instruction::LocalGet(s5));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s4));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);

    if is_text {
        // Publish dest early so s5 can be used as a copy temp.
        body.instruction(&Instruction::LocalGet(s5));
        body.instruction(&Instruction::I64ExtendI32U);
        body.instruction(&Instruction::LocalSet(local_index(dest)));
        // s0 = data base, s1 = index, s6 = count
        // temps: s2=src frame, s3=dest frame, s4/s5/s7=copy helpers
        body.instruction(&Instruction::LocalGet(s5));
        body.instruction(&Instruction::LocalGet(s7));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s0));
        body.instruction(&Instruction::I32Const(0));
        body.instruction(&Instruction::LocalSet(s1));
        body.instruction(&Instruction::Block(BlockType::Empty));
        body.instruction(&Instruction::Loop(BlockType::Empty));
        body.instruction(&Instruction::LocalGet(s1));
        body.instruction(&Instruction::LocalGet(s6));
        body.instruction(&Instruction::I32GeU);
        body.instruction(&Instruction::BrIf(1));
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::LocalGet(s1));
        body.instruction(&Instruction::I32Const(8));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::LocalSet(s2));
        emit_text_copy_frame_i32(body, s2, s3, s4, s5, s7);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::LocalGet(s1));
        body.instruction(&Instruction::I32Const(8));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalGet(s3));
        body.instruction(&Instruction::I64ExtendI32U);
        body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::LocalGet(s1));
        body.instruction(&Instruction::I32Const(1));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s1));
        body.instruction(&Instruction::Br(0));
        body.instruction(&Instruction::End);
        body.instruction(&Instruction::End);
        return;
    }

    body.instruction(&Instruction::LocalGet(s5));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// Deep-copy text frame at `src_frame` (i32) into a fresh frame in `dest_frame`.
/// Temps `t0`/`t1`/`t2` are clobbered.
pub(in crate::codegen::wasm) fn emit_text_copy_frame_i32(
    body: &mut Function,
    src_frame: u32,
    dest_frame: u32,
    t0: u32,
    t1: u32,
    t2: u32,
) {
    // t0 = length
    body.instruction(&Instruction::LocalGet(src_frame));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(t0));
    body.instruction(&Instruction::LocalGet(t0));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_bump_alloc(body, FRAME_SIZE, dest_frame);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_PTR, 0);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_LEN, 0);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_PAD, 1);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_START, 1);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_MAIN_LEN, 0);
    body.instruction(&Instruction::Else);
    // t1 = new content buffer (also saved in dest_frame across memcpy)
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(t1));
    body.instruction(&Instruction::LocalGet(t1));
    body.instruction(&Instruction::LocalSet(dest_frame));
    emit_heap_grow_if_needed(body, t1, BumpSize::Dynamic(t0));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(t1));
    body.instruction(&Instruction::LocalGet(t0));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    // t2 = content source pointer
    body.instruction(&Instruction::LocalGet(src_frame));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(t2));
    // memcpy into t1 from t2 for t0 bytes (mutates t1/t2/t0)
    emit_memcpy(body, t1, t2, t0);
    // restore len and buf start
    body.instruction(&Instruction::LocalGet(src_frame));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(t0));
    body.instruction(&Instruction::LocalGet(dest_frame));
    body.instruction(&Instruction::LocalSet(t1)); // buf start
    emit_bump_alloc(body, FRAME_SIZE, dest_frame);
    emit_frame_store_local(body, dest_frame, FRAME_OFF_PTR, t1);
    emit_frame_store_local(body, dest_frame, FRAME_OFF_LEN, t0);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_PAD, 0);
    emit_frame_store_const(body, dest_frame, FRAME_OFF_START, 1);
    emit_frame_store_local(body, dest_frame, FRAME_OFF_MAIN_LEN, t0);
    body.instruction(&Instruction::End); // if/else
}

/// Computes row-major linear index into `s_lin` (i32). `s0` holds array base.
pub(in crate::codegen::wasm) fn emit_array_linear_index(
    body: &mut Function,
    array_base: u32,
    indices: &[LocalId],
    s_lin: u32,
    s_stride: u32,
    s_tmp: u32,
) {
    let ndims = indices.len();
    let header = 8 + ndims * 16;
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s_lin));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::LocalSet(s_stride));
    for dim in (0..ndims).rev() {
        let low_off = 8 + dim * 16;
        let high_off = 16 + dim * 16;
        let index = indices[dim];
        // empty dim: low > high
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: low_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: high_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I64GtS);
        body.instruction(&Instruction::If(BlockType::Empty));
        body.instruction(&Instruction::Unreachable);
        body.instruction(&Instruction::End);
        // index < low
        body.instruction(&Instruction::LocalGet(local_index(index)));
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: low_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I64LtS);
        body.instruction(&Instruction::If(BlockType::Empty));
        body.instruction(&Instruction::Unreachable);
        body.instruction(&Instruction::End);
        // index > high
        body.instruction(&Instruction::LocalGet(local_index(index)));
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: high_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I64GtS);
        body.instruction(&Instruction::If(BlockType::Empty));
        body.instruction(&Instruction::Unreachable);
        body.instruction(&Instruction::End);
        // linear += (index - low) * stride
        body.instruction(&Instruction::LocalGet(local_index(index)));
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: low_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I64Sub);
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::LocalGet(s_stride));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::LocalGet(s_lin));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s_lin));
        // stride *= size
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: high_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::LocalGet(array_base));
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: low_off as u64,
            align: 3,
            memory_index: 0,
        }));
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::I32Sub);
        body.instruction(&Instruction::I32Const(1));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(s_tmp));
        body.instruction(&Instruction::LocalGet(s_stride));
        body.instruction(&Instruction::LocalGet(s_tmp));
        body.instruction(&Instruction::I32Mul);
        body.instruction(&Instruction::LocalSet(s_stride));
    }
    let _ = header;
}

pub(in crate::codegen::wasm) fn emit_array_element_addr(
    body: &mut Function,
    array: LocalId,
    indices: &[LocalId],
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    let header = 8 + indices.len() * 16;
    body.instruction(&Instruction::LocalGet(local_index(array)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    emit_array_linear_index(body, s0, indices, s1, s2, s3);
    // addr = base + header + linear*8
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(header as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(8));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::I32Add);
}

pub(in crate::codegen::wasm) fn emit_array_load_nd(
    body: &mut Function,
    function: &MirFunction,
    dest: LocalId,
    array: LocalId,
    indices: &[LocalId],
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    _s4: u32,
) {
    emit_array_element_addr(body, array, indices, s0, s1, s2, s3);
    if function.local(dest).ty.is_float() {
        body.instruction(&Instruction::F64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
    } else {
        body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
    }
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

pub(in crate::codegen::wasm) fn emit_array_store_nd(
    body: &mut Function,
    function: &MirFunction,
    array: LocalId,
    indices: &[LocalId],
    value: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    s4: u32,
) {
    if function.local(value).ty == MirType::Text {
        emit_array_text_store(body, array, indices, value, s0, s1, s2, s3, s4);
        return;
    }
    emit_array_element_addr(body, array, indices, s0, s1, s2, s3);
    body.instruction(&Instruction::LocalGet(local_index(value)));
    if function.local(value).ty.is_float() {
        body.instruction(&Instruction::F64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
    } else {
        body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
    }
}

/// `A(i) :- T` copies the reference *value* (main, start, length, pos) into the
/// element's own frame, which [`emit_array_alloc_nd`] gave it. Storing `T`'s
/// frame pointer instead would alias the two, so a later `T :- U` would rewrite
/// the array element as well.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_array_text_store(
    body: &mut Function,
    array: LocalId,
    indices: &[LocalId],
    value: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    s4: u32,
) {
    emit_array_element_addr(body, array, indices, s0, s1, s2, s3);
    body.instruction(&Instruction::LocalSet(s4));
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // Elements of an array reached through a formal parameter can still be
    // frameless if the array was never allocated here; give them a frame.
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_bump_alloc(body, FRAME_SIZE, s0);
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::If(BlockType::Empty));
    for offset in [
        FRAME_OFF_PTR,
        FRAME_OFF_LEN,
        FRAME_OFF_POS,
        FRAME_OFF_PAD,
        FRAME_OFF_START,
        FRAME_OFF_MAIN_LEN,
    ] {
        emit_frame_copy_field(body, s0, s1, offset);
    }
    body.instruction(&Instruction::Else);
    emit_frame_store_const(body, s0, FRAME_OFF_PTR, 0);
    emit_frame_store_const(body, s0, FRAME_OFF_LEN, 0);
    emit_frame_store_const(body, s0, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, s0, FRAME_OFF_PAD, 1);
    emit_frame_store_const(body, s0, FRAME_OFF_START, 1);
    emit_frame_store_const(body, s0, FRAME_OFF_MAIN_LEN, 0);
    body.instruction(&Instruction::End);
}
