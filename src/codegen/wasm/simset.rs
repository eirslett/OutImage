//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// Pushes `ptr.<offset>` as an i32 address.
pub(in crate::codegen::wasm) fn emit_simset_load(body: &mut Function, ptr: u32, offset: i64) {
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32WrapI64);
}

/// `ptr.<offset> = value`, both i32 address locals.
pub(in crate::codegen::wasm) fn emit_simset_store(
    body: &mut Function,
    ptr: u32,
    offset: i64,
    value: u32,
) {
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::LocalGet(value));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }));
}

/// `ptr.<offset> = 0`.
pub(in crate::codegen::wasm) fn emit_simset_store_none(body: &mut Function, ptr: u32, offset: i64) {
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }));
}

/// Pushes 1 when the (non-null) object at `ptr` is a `Head`.
pub(in crate::codegen::wasm) fn emit_simset_is_head(body: &mut Function, ptr: u32) {
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SIMSET_HEAD_CLASS_ID_PTR as i32));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64Eq);
}

pub(in crate::codegen::wasm) fn emit_simset_init_head(
    body: &mut Function,
    head: LocalId,
    ptr: u32,
) {
    emit_ref_ptr(body, head);
    body.instruction(&Instruction::LocalTee(ptr));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_store(body, ptr, SIMSET_SUC_OFFSET, ptr);
    emit_simset_store(body, ptr, SIMSET_PRED_OFFSET, ptr);
    body.instruction(&Instruction::End);
}

/// `out` (§12.3): unlink `x` from whatever ring it is in. A member always has a
/// non-null `SUC`, so a null one means it is already outside a list.
pub(in crate::codegen::wasm) fn emit_simset_out(body: &mut Function, x: u32, suc: u32, pred: u32) {
    body.instruction(&Instruction::LocalGet(x));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_load(body, x, SIMSET_SUC_OFFSET);
    body.instruction(&Instruction::LocalTee(suc));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_load(body, x, SIMSET_PRED_OFFSET);
    body.instruction(&Instruction::LocalSet(pred));
    emit_simset_store(body, suc, SIMSET_PRED_OFFSET, pred);
    body.instruction(&Instruction::LocalGet(pred));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_store(body, pred, SIMSET_SUC_OFFSET, suc);
    body.instruction(&Instruction::End);
    emit_simset_store_none(body, x, SIMSET_SUC_OFFSET);
    emit_simset_store_none(body, x, SIMSET_PRED_OFFSET);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// Pushes 1 when `ptr` can take a neighbour: a Head always can, a Link only
/// while it is itself a member of some ring.
pub(in crate::codegen::wasm) fn emit_simset_can_link(body: &mut Function, ptr: u32, ptr_suc: u32) {
    emit_simset_load(body, ptr, SIMSET_SUC_OFFSET);
    body.instruction(&Instruction::LocalTee(ptr_suc));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::Else);
    emit_simset_is_head(body, ptr);
    body.instruction(&Instruction::End);
}

/// `precede(x, ptr)` — and `into(x, head)`, which is `precede` against the Head
/// (§12.3): the Head's predecessor is the ring's last member.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_simset_precede(
    body: &mut Function,
    object: LocalId,
    ptr: LocalId,
    x: u32,
    s1: u32,
    s2: u32,
    p: u32,
    pred: u32,
) {
    emit_ref_ptr(body, object);
    body.instruction(&Instruction::LocalTee(x));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_out(body, x, s1, s2);
    emit_ref_ptr(body, ptr);
    body.instruction(&Instruction::LocalTee(p));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_can_link(body, p, s1);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_load(body, p, SIMSET_PRED_OFFSET);
    body.instruction(&Instruction::LocalSet(pred));
    emit_simset_store(body, x, SIMSET_SUC_OFFSET, p);
    emit_simset_store(body, x, SIMSET_PRED_OFFSET, pred);
    body.instruction(&Instruction::LocalGet(pred));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_store(body, pred, SIMSET_SUC_OFFSET, x);
    body.instruction(&Instruction::End);
    emit_simset_store(body, p, SIMSET_PRED_OFFSET, x);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// `follow(x, ptr)` (§12.3).
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_simset_follow(
    body: &mut Function,
    object: LocalId,
    ptr: LocalId,
    x: u32,
    s1: u32,
    s2: u32,
    p: u32,
    p_suc: u32,
) {
    emit_ref_ptr(body, object);
    body.instruction(&Instruction::LocalTee(x));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_out(body, x, s1, s2);
    emit_ref_ptr(body, ptr);
    body.instruction(&Instruction::LocalTee(p));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_can_link(body, p, p_suc);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_store(body, x, SIMSET_PRED_OFFSET, p);
    emit_simset_store(body, x, SIMSET_SUC_OFFSET, p_suc);
    body.instruction(&Instruction::LocalGet(p_suc));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_store(body, p_suc, SIMSET_PRED_OFFSET, x);
    body.instruction(&Instruction::End);
    emit_simset_store(body, p, SIMSET_SUC_OFFSET, x);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// `dest = src.<offset>`, but none once the ring wraps round to the Head — the
/// `Suc` / `Pred` (and `First` / `Last`) attributes of §12.2.
pub(in crate::codegen::wasm) fn emit_simset_step(
    body: &mut Function,
    src: u32,
    dest: u32,
    offset: i64,
    scratch: u32,
) {
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(src));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_load(body, src, offset);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_is_head(body, scratch);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::LocalSet(dest));
}

pub(in crate::codegen::wasm) fn emit_simset_neighbour(
    body: &mut Function,
    dest: LocalId,
    object: LocalId,
    offset: i64,
    x: u32,
    scratch: u32,
) {
    emit_ref_ptr(body, object);
    body.instruction(&Instruction::LocalSet(x));
    emit_simset_step(body, x, x, offset, scratch);
    body.instruction(&Instruction::LocalGet(x));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `empty` (§12.2): a Head with no members points its `SUC` at itself.
pub(in crate::codegen::wasm) fn emit_simset_empty(
    body: &mut Function,
    dest: LocalId,
    head: LocalId,
    h: u32,
    suc: u32,
) {
    emit_ref_ptr(body, head);
    body.instruction(&Instruction::LocalTee(h));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_simset_load(body, h, SIMSET_SUC_OFFSET);
    body.instruction(&Instruction::LocalTee(suc));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::LocalGet(suc));
    body.instruction(&Instruction::LocalGet(h));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `cardinal` (§12.2): walk `Suc` from the Head until it reports none.
pub(in crate::codegen::wasm) fn emit_simset_cardinal(
    body: &mut Function,
    dest: LocalId,
    head: LocalId,
    cur: u32,
    count: u32,
    scratch: u32,
) {
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(count));
    emit_ref_ptr(body, head);
    body.instruction(&Instruction::LocalSet(cur));
    emit_simset_step(body, cur, cur, SIMSET_SUC_OFFSET, scratch);
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(cur));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(count));
    emit_simset_step(body, cur, cur, SIMSET_SUC_OFFSET, scratch);
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// Rejects a MIR value flowing between a WasmGC reference and a plain `i64`
/// word (`Copy`, `Compare`, a call argument/result, a `Return`).
///
/// Phase 4-R4: a Simula reference has no integer encoding any more, so there is
/// nothing left to convert — the two sides of every such transfer have to agree
/// in MIR. The lowerer keeps them agreeing by giving each by-reference home a
/// typed one (`ref_cell` for `ref`s, `name_int_env` for integer cells), so
/// reaching here means a lowering path invented an untyped `env` and the fix
/// belongs there, not in a codegen bridge.
pub(in crate::codegen::wasm) fn gc_reject_ref_word_mix(
    site: &str,
    function: &MirFunction,
    a: MirType,
    b: MirType,
) -> Result<(), CompileError> {
    if a == b || gc_ref_home_ty(a) == gc_ref_home_ty(b) {
        return Ok(());
    }
    Err(CompileError::codegen(format!(
        "MIR wasm: {site} in '{}' mixes a WasmGC reference with an i64 word \
         ({a} vs {b}); references have no integer encoding under WasmGC — give \
         the value a typed home in MIR lowering",
        function.name,
    )))
}

pub(in crate::codegen::wasm) fn linkage_base_ty() -> Result<u32, CompileError> {
    gc_ctx(|ctx| ctx.linkage_base_ty).flatten().ok_or_else(|| {
        CompileError::codegen("MIR wasm: linkage_base WasmGC type missing for SIMSET lowering")
    })
}

/// Load SUC (field 1) or PRED (field 2) from a linkage object into `dest_ref`.
///
/// Phase 4-R2: `linkage_base`'s ring pointers are `(ref null eq)` fields, so
/// this is a plain `struct.get` — no handle-table hop, and the host
/// collector traces the ring itself.
///
/// Both `ref.test`s stay, though. The *outer* one is load-bearing: callers
/// reach here with an arbitrary eqref — `none`, or a `Ref(Linkage)` local
/// that a `:-` left holding some non-ring object — and a bare `ref.cast`
/// would trap where Simula wants `none`. It must be the **non-nullable**
/// test, since `ref.test (ref null $t)` accepts `null` and the following
/// `struct.get` would then trap on it. The *inner* one only defends against
/// a SUC slot that somehow holds a non-ring object; it is cheap and keeps
/// `empty` / `first` / `Suc` answering consistently instead of trapping
/// later inside `is_head` (simtst96).
pub(in crate::codegen::wasm) fn emit_simset_link_load_gc(
    body: &mut Function,
    object_ref: u32,
    field_index: u32,
    dest_ref: u32,
    linkage_ty: u32,
) {
    body.instruction(&Instruction::LocalGet(object_ref));
    body.instruction(&Instruction::RefTestNonNull(HeapType::Concrete(linkage_ty)));
    body.instruction(&Instruction::If(BlockType::Result(
        crate::codegen::wasm_gc::anyref_val(),
    )));
    body.instruction(&Instruction::LocalGet(object_ref));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(linkage_ty)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: linkage_ty,
        field_index,
    });
    // `ref.test` consumes its candidate, so stash it before testing.
    body.instruction(&Instruction::LocalTee(dest_ref));
    body.instruction(&Instruction::RefTestNonNull(HeapType::Concrete(linkage_ty)));
    body.instruction(&Instruction::If(BlockType::Result(
        crate::codegen::wasm_gc::anyref_val(),
    )));
    body.instruction(&Instruction::LocalGet(dest_ref));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(dest_ref));
}

/// `object.SUC/PRED :- none`.
pub(in crate::codegen::wasm) fn emit_simset_link_store_none_gc(
    body: &mut Function,
    object_ref: u32,
    field_index: u32,
    linkage_ty: u32,
) {
    body.instruction(&Instruction::LocalGet(object_ref));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(linkage_ty)));
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::StructSet {
        struct_type_index: linkage_ty,
        field_index,
    });
}

/// `object.SUC/PRED :- value` — a direct `struct.set` of one eqref field.
pub(in crate::codegen::wasm) fn emit_simset_link_store_gc(
    body: &mut Function,
    object_ref: u32,
    value_ref: u32,
    field_index: u32,
    linkage_ty: u32,
) {
    body.instruction(&Instruction::LocalGet(object_ref));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(linkage_ty)));
    body.instruction(&Instruction::LocalGet(value_ref));
    body.instruction(&Instruction::StructSet {
        struct_type_index: linkage_ty,
        field_index,
    });
}

/// Pushes 1 when the (non-null) linkage object at `object_ref` is a `Head`.
pub(in crate::codegen::wasm) fn emit_simset_is_head_gc(
    body: &mut Function,
    object_ref: u32,
    linkage_ty: u32,
) {
    body.instruction(&Instruction::LocalGet(object_ref));
    body.instruction(&Instruction::RefTestNullable(HeapType::Concrete(
        linkage_ty,
    )));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::LocalGet(object_ref));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(linkage_ty)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: linkage_ty,
        field_index: 0,
    });
    body.instruction(&Instruction::I32Const(SIMSET_HEAD_CLASS_ID_PTR as i32));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64Eq);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_simset_init_head_gc(
    body: &mut Function,
    _function: &MirFunction,
    head: LocalId,
    _scratch0: u32,
    _scratch1: u32,
    ref0: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    body.instruction(&Instruction::LocalGet(local_index(head)));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(local_index(head)));
    body.instruction(&Instruction::LocalSet(ref0));
    emit_simset_link_store_gc(body, ref0, ref0, 1, linkage_ty);
    emit_simset_link_store_gc(body, ref0, ref0, 2, linkage_ty);
    body.instruction(&Instruction::End);
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_simset_out_gc(
    body: &mut Function,
    _function: &MirFunction,
    object: LocalId,
    _scratch0: u32,
    _scratch1: u32,
    _scratch2: u32,
    ref0: u32,
    ref1: u32,
    ref2: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::LocalSet(ref0));
    body.instruction(&Instruction::LocalGet(ref0));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_load_gc(body, ref0, 1, ref1, linkage_ty);
    body.instruction(&Instruction::LocalGet(ref1));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_load_gc(body, ref0, 2, ref2, linkage_ty);
    emit_simset_link_store_gc(body, ref1, ref2, 2, linkage_ty);
    body.instruction(&Instruction::LocalGet(ref2));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_store_gc(body, ref2, ref1, 1, linkage_ty);
    body.instruction(&Instruction::End);
    emit_simset_link_store_none_gc(body, ref0, 1, linkage_ty);
    emit_simset_link_store_none_gc(body, ref0, 2, linkage_ty);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

/// Mirrors [`emit_simset_can_link`]'s truthiness check: `ptr.SUC` truthy
/// (non-null / non-zero, i.e. `ptr` already has a successor) always
/// "can-link" (`1`); only a *null* SUC needs the extra `is_head` check.
/// `RefIsNull` yields the *opposite* polarity of a raw-handle truthiness
/// test, so its `if`/`else` arms must be swapped relative to the non-GC
/// version — getting this backwards silently corrupts every
/// `Follow`/`Precede` past the first ring member (simtst93 tests 8+: it
/// always took the `is_head` branch when `ptr` *did* have a successor).
pub(in crate::codegen::wasm) fn emit_simset_can_link_gc(
    body: &mut Function,
    ptr_ref: u32,
    ptr_suc_ref: u32,
    linkage_ty: u32,
) {
    emit_simset_link_load_gc(body, ptr_ref, 1, ptr_suc_ref, linkage_ty);
    body.instruction(&Instruction::LocalGet(ptr_suc_ref));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_simset_is_head_gc(body, ptr_ref, linkage_ty);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::End);
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_simset_precede_gc(
    body: &mut Function,
    _function: &MirFunction,
    object: LocalId,
    ptr: LocalId,
    scratch0: u32,
    scratch1: u32,
    _scratch2: u32,
    _scratch3: u32,
    _scratch4: u32,
    ref0: u32,
    ref1: u32,
    ref2: u32,
    ref3: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::LocalSet(ref0));
    body.instruction(&Instruction::LocalGet(ref0));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_out_gc(
        body, _function, object, scratch0, scratch1, scratch0, ref0, ref1, ref2,
    )?;
    body.instruction(&Instruction::LocalGet(local_index(ptr)));
    body.instruction(&Instruction::LocalSet(ref3));
    body.instruction(&Instruction::LocalGet(ref3));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_can_link_gc(body, ref3, ref1, linkage_ty);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_load_gc(body, ref3, 2, ref2, linkage_ty);
    emit_simset_link_store_gc(body, ref0, ref3, 1, linkage_ty);
    emit_simset_link_store_gc(body, ref0, ref2, 2, linkage_ty);
    body.instruction(&Instruction::LocalGet(ref2));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_store_gc(body, ref2, ref0, 1, linkage_ty);
    body.instruction(&Instruction::End);
    emit_simset_link_store_gc(body, ref3, ref0, 2, linkage_ty);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_simset_follow_gc(
    body: &mut Function,
    _function: &MirFunction,
    object: LocalId,
    ptr: LocalId,
    scratch0: u32,
    scratch1: u32,
    _scratch2: u32,
    _scratch3: u32,
    _scratch4: u32,
    ref0: u32,
    ref1: u32,
    ref2: u32,
    ref3: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::LocalSet(ref0));
    body.instruction(&Instruction::LocalGet(ref0));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_out_gc(
        body, _function, object, scratch0, scratch1, scratch0, ref0, ref1, ref2,
    )?;
    body.instruction(&Instruction::LocalGet(local_index(ptr)));
    body.instruction(&Instruction::LocalSet(ref3));
    body.instruction(&Instruction::LocalGet(ref3));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_can_link_gc(body, ref3, ref1, linkage_ty);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_store_gc(body, ref0, ref3, 2, linkage_ty);
    emit_simset_link_store_gc(body, ref0, ref1, 1, linkage_ty);
    body.instruction(&Instruction::LocalGet(ref1));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_simset_link_store_gc(body, ref1, ref0, 2, linkage_ty);
    body.instruction(&Instruction::End);
    emit_simset_link_store_gc(body, ref3, ref0, 1, linkage_ty);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

/// `dest_ref := SUC/PRED-step(src_ref)` (`none` if `src_ref` is `none` or the
/// neighbour is the ring's `Head` sentinel). Some callers intentionally alias
/// `src_ref`/`dest_ref` to advance a walking pointer in place
/// (`emit_simset_cardinal_gc`), so this must read `src_ref` in full *before*
/// writing `dest_ref` anywhere — writing the `none` default into `dest_ref`
/// first (as a prior version did) clobbers an aliased `src_ref` before its
/// SUC/PRED field is ever loaded, making every step read `none` (simtst93 &
/// co: `for l:-l.Suc while l=/=none do …` never saw a non-none `l`).
pub(in crate::codegen::wasm) fn emit_simset_step_gc(
    body: &mut Function,
    src_ref: u32,
    dest_ref: u32,
    field_index: u32,
    temp_ref: u32,
    linkage_ty: u32,
) {
    body.instruction(&Instruction::LocalGet(src_ref));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::LocalSet(dest_ref));
    body.instruction(&Instruction::Else);
    emit_simset_link_load_gc(body, src_ref, field_index, temp_ref, linkage_ty);
    body.instruction(&Instruction::LocalGet(temp_ref));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::LocalSet(dest_ref));
    body.instruction(&Instruction::Else);
    emit_simset_is_head_gc(body, temp_ref, linkage_ty);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::LocalSet(dest_ref));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(temp_ref));
    body.instruction(&Instruction::LocalSet(dest_ref));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_simset_neighbour_gc(
    body: &mut Function,
    _function: &MirFunction,
    dest: LocalId,
    object: LocalId,
    offset: i64,
    _scratch0: u32,
    ref0: u32,
    ref1: u32,
    ref2: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    let field_index = if offset == SIMSET_SUC_OFFSET { 1 } else { 2 };
    // `emit_simset_step_gc` writes its `null` default into `dest_ref` *before*
    // reading `src_ref`, so `src_ref` and `dest_ref` must be distinct locals —
    // aliasing them (as a single `ref0` used to be) clobbers the object being
    // stepped from before its SUC/PRED field is ever loaded, making every
    // `l.Suc`/`l.Pred` read `none` (simtst93 et al.).
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::LocalSet(ref0));
    emit_simset_step_gc(body, ref0, ref1, field_index, ref2, linkage_ty);
    body.instruction(&Instruction::LocalGet(ref1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_simset_empty_gc(
    body: &mut Function,
    _function: &MirFunction,
    dest: LocalId,
    head: LocalId,
    _scratch0: u32,
    ref0: u32,
    ref1: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    body.instruction(&Instruction::LocalGet(local_index(head)));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(head)));
    body.instruction(&Instruction::LocalSet(ref0));
    emit_simset_link_load_gc(body, ref0, 1, ref1, linkage_ty);
    body.instruction(&Instruction::LocalGet(ref1));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::Else);
    // Empty when SUC is this head *or* any Head (class_id match). The latter
    // covers a corrupt ring where SUC is a different Head instance — `first`
    // already returns `none` for that case via `is_head`, so without this
    // check `empty` is false while `first` is none (simtst96).
    body.instruction(&Instruction::LocalGet(ref1));
    body.instruction(&Instruction::LocalGet(local_index(head)));
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::Else);
    emit_simset_is_head_gc(body, ref1, linkage_ty);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_simset_cardinal_gc(
    body: &mut Function,
    _function: &MirFunction,
    dest: LocalId,
    head: LocalId,
    _scratch0: u32,
    scratch1: u32,
    _scratch2: u32,
    ref0: u32,
    ref1: u32,
) -> Result<(), CompileError> {
    let linkage_ty = linkage_base_ty()?;
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(scratch1));
    body.instruction(&Instruction::LocalGet(local_index(head)));
    body.instruction(&Instruction::LocalSet(ref0));
    emit_simset_step_gc(body, ref0, ref0, 1, ref1, linkage_ty);
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(ref0));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(scratch1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(scratch1));
    emit_simset_step_gc(body, ref0, ref0, 1, ref1, linkage_ty);
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(scratch1));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}
