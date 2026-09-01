//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// Allocate the SysIn / SysOut WasmGC singletons into their reference globals.
pub(in crate::codegen::wasm) fn emit_gc_terminal_init_body(
    mir: &MirModule,
) -> Result<Function, CompileError> {
    let sysin = mir
        .class_layouts
        .iter()
        .find(|l| l.declared_name.eq_ignore_ascii_case("InFile"))
        .ok_or_else(|| {
            CompileError::codegen("MIR wasm: missing InFile layout for WasmGC SysIn terminal")
        })?;
    let sysout = mir
        .class_layouts
        .iter()
        .find(|l| {
            l.declared_name.eq_ignore_ascii_case("PrintFile")
                || l.declared_name.eq_ignore_ascii_case("OutFile")
        })
        .ok_or_else(|| {
            CompileError::codegen(
                "MIR wasm: missing PrintFile/OutFile layout for WasmGC SysOut terminal",
            )
        })?;
    let sysin_info = gc_ctx(|ctx| ctx.class_info_for_id(sysin.class_id).cloned())
        .flatten()
        .ok_or_else(|| CompileError::codegen("MIR wasm: InFile has no WasmGC struct type"))?;
    let sysout_info = gc_ctx(|ctx| ctx.class_info_for_id(sysout.class_id).cloned())
        .flatten()
        .ok_or_else(|| CompileError::codegen("MIR wasm: PrintFile has no WasmGC struct type"))?;

    // locals: 0 = sysin ref, 1 = sysout ref
    let any = crate::codegen::wasm_gc::anyref_val();
    let mut body = Function::new([(2, any)]);
    // SysIn
    body.instruction(&Instruction::StructNewDefault(sysin_info.wasm_ty));
    body.instruction(&Instruction::LocalTee(0));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(
        sysin_info.wasm_ty,
    )));
    body.instruction(&Instruction::I64Const(sysin.class_id));
    body.instruction(&Instruction::StructSet {
        struct_type_index: sysin_info.wasm_ty,
        field_index: 0,
    });
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::GlobalSet(GLOBAL_SYSIN));
    // SysOut
    body.instruction(&Instruction::StructNewDefault(sysout_info.wasm_ty));
    body.instruction(&Instruction::LocalTee(1));
    body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(
        sysout_info.wasm_ty,
    )));
    body.instruction(&Instruction::I64Const(sysout.class_id));
    body.instruction(&Instruction::StructSet {
        struct_type_index: sysout_info.wasm_ty,
        field_index: 0,
    });
    body.instruction(&Instruction::LocalGet(1));
    body.instruction(&Instruction::GlobalSet(GLOBAL_SYSOUT));
    body.instruction(&Instruction::End);
    Ok(body)
}

/// Phase 4-R4: on wasm a coroutine's argument object, a component's object
/// and a parked frame's Simula references are all host-traced WasmGC values,
/// and linear memory cannot hold one. The `seq_runtime` helpers that reach
/// those three therefore have no MIR-expressible body here — MIR reaches
/// storage only through addresses. Their MIR bodies stay the interpreter /
/// native fallback (where the linear record *is* the storage); wasm replaces
/// them wholesale. `frame_push` / `frame_pop` / `coro_create` /
/// `object_create` keep their MIR-derived bodies and simply call these.
///
/// The storage is a `seq_gc_slot` struct per sequencing record, held in the
/// registry array [`GLOBAL_SEQ_GC_REGISTRY`] names and found by the scalar
/// index in the record's `CORO_GC_SLOT` / `COMP_GC_SLOT` word — no handle
/// table anywhere. `CORO_REF_SP` / `CORO_REF_CAP` /
/// `CORO_REF_CUR` stay linear byte counts, eight bytes to the element;
/// `CORO_ARG` / `CORO_REF_FRAMES` / `COMP_OBJECT` are simply unused here.
pub(in crate::codegen::wasm) fn emit_ref_spine_body(name: &str) -> Option<Function> {
    if !gc_objects_enabled() {
        return None;
    }
    let (spill_ty, slot_ty, registry_ty) = gc_ctx(|ctx| {
        (
            ctx.spill_refs_array_ty,
            ctx.seq_gc_slot_ty,
            ctx.seq_gc_registry_ty,
        )
    })?;
    match name {
        seq_runtime::SEQ_GC_SLOT_NEW => Some(emit_seq_gc_slot_new_gc(slot_ty, registry_ty)),
        seq_runtime::CORO_ARG_STORE => Some(emit_seq_gc_object_store_gc(
            seq_runtime::CORO_GC_SLOT,
            slot_ty,
            registry_ty,
        )),
        seq_runtime::CORO_ARG_LOAD => Some(emit_seq_gc_object_load_gc(
            seq_runtime::CORO_GC_SLOT,
            slot_ty,
            registry_ty,
        )),
        seq_runtime::COMP_OBJECT_STORE => Some(emit_seq_gc_object_store_gc(
            seq_runtime::COMP_GC_SLOT,
            slot_ty,
            registry_ty,
        )),
        seq_runtime::COMP_OBJECT_LOAD => Some(emit_seq_gc_object_load_gc(
            seq_runtime::COMP_GC_SLOT,
            slot_ty,
            registry_ty,
        )),
        seq_runtime::REFS_CREATE => Some(emit_refs_create_gc(spill_ty, slot_ty, registry_ty)),
        seq_runtime::REFS_GROW => Some(emit_refs_grow_gc(spill_ty, slot_ty, registry_ty)),
        seq_runtime::SPILL_STORE_REF => {
            Some(emit_spill_store_ref_gc(spill_ty, slot_ty, registry_ty))
        }
        seq_runtime::SPILL_LOAD_REF => Some(emit_spill_load_ref_gc(spill_ty, slot_ty, registry_ty)),
        _ => None,
    }
}

pub(in crate::codegen::wasm) fn coro_word() -> wasm_encoder::MemArg {
    wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }
}

pub(in crate::codegen::wasm) fn emit_coro_load(body: &mut Function, coro: u32, offset: i64) {
    body.instruction(&Instruction::LocalGet(coro));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: offset as u64,
        ..coro_word()
    }));
}

pub(in crate::codegen::wasm) fn emit_coro_store_const(
    body: &mut Function,
    coro: u32,
    offset: i64,
    value: i64,
) {
    body.instruction(&Instruction::LocalGet(coro));
    body.instruction(&Instruction::I64Const(value));
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        ..coro_word()
    }));
}

pub(in crate::codegen::wasm) fn emit_coro_store_local(
    body: &mut Function,
    coro: u32,
    offset: i64,
    value: u32,
) {
    body.instruction(&Instruction::LocalGet(coro));
    body.instruction(&Instruction::LocalGet(value));
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        ..coro_word()
    }));
}

/// The running coroutine's record address (`STATE_CURRENT`) as an i32.
pub(in crate::codegen::wasm) fn emit_load_current_coro(body: &mut Function, dest: u32) {
    body.instruction(&Instruction::I32Const(seq_runtime::STATE_BASE as i32));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: seq_runtime::STATE_CURRENT as u64,
        ..coro_word()
    }));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(dest));
}

/// Pushes the `seq_gc_slot` record of the sequencing record at linear address
/// `record` (an `i32` local), whose registry index sits at `slot_offset`.
///
/// Traps (`array.get` on a null / out-of-range registry) if the record never
/// reserved a slot, which would be a codegen bug rather than a program one.
pub(in crate::codegen::wasm) fn emit_seq_gc_slot(
    body: &mut Function,
    record: u32,
    slot_offset: i64,
    registry_ty: u32,
) {
    body.instruction(&Instruction::GlobalGet(GLOBAL_SEQ_GC_REGISTRY));
    body.instruction(&Instruction::LocalGet(record));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: slot_offset as u64,
        ..coro_word()
    }));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::ArrayGet(registry_ty));
}

/// `coro`'s ref spine, reached through its GC side record.
pub(in crate::codegen::wasm) fn emit_load_ref_spine(
    body: &mut Function,
    coro: u32,
    slot_ty: u32,
    registry_ty: u32,
    dest: u32,
) {
    emit_seq_gc_slot(body, coro, seq_runtime::CORO_GC_SLOT, registry_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: slot_ty,
        field_index: crate::codegen::wasm_gc::SEQ_GC_SLOT_FIELD_SPINE,
    });
    body.instruction(&Instruction::LocalSet(dest));
}

/// Pushes `bytes / 8` as an i32 element count for a byte-counted field.
pub(in crate::codegen::wasm) fn emit_bytes_to_elems(body: &mut Function) {
    body.instruction(&Instruction::I64Const(3));
    body.instruction(&Instruction::I64ShrU);
    body.instruction(&Instruction::I32WrapI64);
}

/// Pushes the spine element index of ref slot `index` of the current frame.
pub(in crate::codegen::wasm) fn emit_ref_slot_index(body: &mut Function, coro: u32, index: u32) {
    emit_coro_load(body, coro, seq_runtime::CORO_REF_CUR);
    body.instruction(&Instruction::I64Const(3));
    body.instruction(&Instruction::I64ShrU);
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I64Add);
    body.instruction(&Instruction::I32WrapI64);
}

/// `seq_gc_slot_new()`: reserve a fresh side record and return its index.
///
/// Growth replaces the registry array wholesale, so an index handed out here
/// stays valid for the rest of the run.
pub(in crate::codegen::wasm) fn emit_seq_gc_slot_new_gc(
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // No params; 0 = count, 1 = capacity, 2 = new capacity, 3 = new registry.
    let (count, capacity, new_capacity, new_registry) = (0, 1, 2, 3);
    let mut body = Function::new([
        (3, ValType::I32),
        (1, crate::codegen::wasm_gc::concrete_ref_null(registry_ty)),
    ]);
    body.instruction(&Instruction::GlobalGet(GLOBAL_SEQ_GC_COUNT));
    body.instruction(&Instruction::LocalSet(count));

    // `array.len` traps on null, and the registry starts null.
    body.instruction(&Instruction::GlobalGet(GLOBAL_SEQ_GC_REGISTRY));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::GlobalGet(GLOBAL_SEQ_GC_REGISTRY));
    body.instruction(&Instruction::ArrayLen);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(capacity));

    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::LocalSet(new_capacity));
    body.instruction(&Instruction::LocalGet(new_capacity));
    body.instruction(&Instruction::I32Const(SEQ_GC_REGISTRY_INITIAL_SLOTS));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(SEQ_GC_REGISTRY_INITIAL_SLOTS));
    body.instruction(&Instruction::LocalSet(new_capacity));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(new_capacity));
    body.instruction(&Instruction::ArrayNewDefault(registry_ty));
    body.instruction(&Instruction::LocalSet(new_registry));
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(new_registry));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::GlobalGet(GLOBAL_SEQ_GC_REGISTRY));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: registry_ty,
        array_type_index_src: registry_ty,
    });
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(new_registry));
    body.instruction(&Instruction::GlobalSet(GLOBAL_SEQ_GC_REGISTRY));
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::GlobalGet(GLOBAL_SEQ_GC_REGISTRY));
    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::StructNewDefault(slot_ty));
    body.instruction(&Instruction::ArraySet(registry_ty));

    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::GlobalSet(GLOBAL_SEQ_GC_COUNT));

    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::End);
    body
}

/// `coro_arg_store(coro, value)` / `component_object_store(comp, value)` —
/// one `struct.set` on the record's GC side record.
pub(in crate::codegen::wasm) fn emit_seq_gc_object_store_gc(
    slot_offset: i64,
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // 0 = record address (i64), 1 = value (eqref); 2 = record address as i32.
    let record = 2;
    let mut body = Function::new([(1, ValType::I32)]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(record));
    emit_seq_gc_slot(&mut body, record, slot_offset, registry_ty);
    body.instruction(&Instruction::LocalGet(1));
    body.instruction(&Instruction::StructSet {
        struct_type_index: slot_ty,
        field_index: crate::codegen::wasm_gc::SEQ_GC_SLOT_FIELD_OBJECT,
    });
    body.instruction(&Instruction::End);
    body
}

/// `coro_arg_load(coro)` / `component_object_load(comp)` — one `struct.get`.
pub(in crate::codegen::wasm) fn emit_seq_gc_object_load_gc(
    slot_offset: i64,
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // 0 = record address (i64); 1 = record address as i32.
    let record = 1;
    let mut body = Function::new([(1, ValType::I32)]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(record));
    emit_seq_gc_slot(&mut body, record, slot_offset, registry_ty);
    body.instruction(&Instruction::StructGet {
        struct_type_index: slot_ty,
        field_index: crate::codegen::wasm_gc::SEQ_GC_SLOT_FIELD_OBJECT,
    });
    body.instruction(&Instruction::End);
    body
}

/// `refs_create(coro)`: a fresh spine, hung off the coroutine's GC side
/// record so the host traces it directly.
pub(in crate::codegen::wasm) fn emit_refs_create_gc(
    spill_ty: u32,
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // 0 = coro (param); 1 = coro address.
    let coro = 1;
    let mut body = Function::new([(1, ValType::I32)]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(coro));

    emit_seq_gc_slot(&mut body, coro, seq_runtime::CORO_GC_SLOT, registry_ty);
    body.instruction(&Instruction::I32Const(
        (seq_runtime::FRAMES_INITIAL_BYTES / 8) as i32,
    ));
    body.instruction(&Instruction::ArrayNewDefault(spill_ty));
    body.instruction(&Instruction::StructSet {
        struct_type_index: slot_ty,
        field_index: crate::codegen::wasm_gc::SEQ_GC_SLOT_FIELD_SPINE,
    });

    emit_coro_store_const(&mut body, coro, seq_runtime::CORO_REF_SP, 0);
    emit_coro_store_const(
        &mut body,
        coro,
        seq_runtime::CORO_REF_CAP,
        seq_runtime::FRAMES_INITIAL_BYTES,
    );
    emit_coro_store_const(&mut body, coro, seq_runtime::CORO_REF_CUR, 0);
    body.instruction(&Instruction::End);
    body
}

/// `refs_grow(coro, needed)`: a wider spine holding the live prefix, written
/// back into the same side record so nothing else has to be told about the
/// move.
pub(in crate::codegen::wasm) fn emit_refs_grow_gc(
    spill_ty: u32,
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // 0 = coro, 1 = needed (params); 2 = coro address; 3 = old cap;
    // 4 = new cap; 5 = side record; 6 = new spine.
    let (coro, old_cap, new_cap, slot, new_spine) = (2, 3, 4, 5, 6);
    let mut body = Function::new([
        (1, ValType::I32),
        (2, ValType::I64),
        (1, crate::codegen::wasm_gc::concrete_ref_null(slot_ty)),
        (1, crate::codegen::wasm_gc::concrete_ref_null(spill_ty)),
    ]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(coro));

    // Double, or jump straight to what was asked for; never shrink.
    emit_coro_load(&mut body, coro, seq_runtime::CORO_REF_CAP);
    body.instruction(&Instruction::LocalSet(old_cap));
    body.instruction(&Instruction::LocalGet(old_cap));
    body.instruction(&Instruction::I64Const(2));
    body.instruction(&Instruction::I64Mul);
    body.instruction(&Instruction::LocalSet(new_cap));
    body.instruction(&Instruction::LocalGet(new_cap));
    body.instruction(&Instruction::LocalGet(1));
    body.instruction(&Instruction::I64LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(1));
    body.instruction(&Instruction::LocalSet(new_cap));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(new_cap));
    body.instruction(&Instruction::LocalGet(old_cap));
    body.instruction(&Instruction::I64LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(old_cap));
    body.instruction(&Instruction::LocalSet(new_cap));
    body.instruction(&Instruction::End);

    emit_seq_gc_slot(&mut body, coro, seq_runtime::CORO_GC_SLOT, registry_ty);
    body.instruction(&Instruction::LocalSet(slot));

    body.instruction(&Instruction::LocalGet(new_cap));
    emit_bytes_to_elems(&mut body);
    body.instruction(&Instruction::ArrayNewDefault(spill_ty));
    body.instruction(&Instruction::LocalSet(new_spine));

    body.instruction(&Instruction::LocalGet(new_spine));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalGet(slot));
    body.instruction(&Instruction::StructGet {
        struct_type_index: slot_ty,
        field_index: crate::codegen::wasm_gc::SEQ_GC_SLOT_FIELD_SPINE,
    });
    body.instruction(&Instruction::I32Const(0));
    emit_coro_load(&mut body, coro, seq_runtime::CORO_REF_SP);
    emit_bytes_to_elems(&mut body);
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: spill_ty,
        array_type_index_src: spill_ty,
    });

    body.instruction(&Instruction::LocalGet(slot));
    body.instruction(&Instruction::LocalGet(new_spine));
    body.instruction(&Instruction::StructSet {
        struct_type_index: slot_ty,
        field_index: crate::codegen::wasm_gc::SEQ_GC_SLOT_FIELD_SPINE,
    });
    emit_coro_store_local(&mut body, coro, seq_runtime::CORO_REF_CAP, new_cap);
    body.instruction(&Instruction::End);
    body
}

/// `spill_store_ref(frame, scalar_slots, index, value)` as an `array.set`.
pub(in crate::codegen::wasm) fn emit_spill_store_ref_gc(
    spill_ty: u32,
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // 0..3 = params; 4 = coro address; 5 = spine.
    let (coro, spine) = (4, 5);
    let mut body = Function::new([
        (1, ValType::I32),
        (1, crate::codegen::wasm_gc::concrete_ref_null(spill_ty)),
    ]);
    emit_load_current_coro(&mut body, coro);
    emit_load_ref_spine(&mut body, coro, slot_ty, registry_ty, spine);
    body.instruction(&Instruction::LocalGet(spine));
    emit_ref_slot_index(&mut body, coro, 2);
    body.instruction(&Instruction::LocalGet(3));
    body.instruction(&Instruction::ArraySet(spill_ty));
    body.instruction(&Instruction::End);
    body
}

/// `spill_load_ref(frame, scalar_slots, index)` as an `array.get`.
pub(in crate::codegen::wasm) fn emit_spill_load_ref_gc(
    spill_ty: u32,
    slot_ty: u32,
    registry_ty: u32,
) -> Function {
    // 0..2 = params; 3 = coro address; 4 = spine.
    let (coro, spine) = (3, 4);
    let mut body = Function::new([
        (1, ValType::I32),
        (1, crate::codegen::wasm_gc::concrete_ref_null(spill_ty)),
    ]);
    emit_load_current_coro(&mut body, coro);
    emit_load_ref_spine(&mut body, coro, slot_ty, registry_ty, spine);
    body.instruction(&Instruction::LocalGet(spine));
    emit_ref_slot_index(&mut body, coro, 2);
    body.instruction(&Instruction::ArrayGet(spill_ty));
    body.instruction(&Instruction::End);
    body
}

/// Phase 4-R4: chapter 12's four reference locations — CURRENT, RUNNING,
/// MAIN and the SQS's `process` column — are host-traced WasmGC storage on
/// wasm, so the `sim_runtime` accessors that reach them have no
/// MIR-expressible body here (the MIR bodies remain the interpreter / native
/// fallback, where the linear simulation state *is* the storage).
///
/// CURRENT / RUNNING / MAIN are mutable `(ref null eq)` globals: a store is
/// one `global.set` and a load one `global.get`. The event notices keep
/// `evtime` and `seq` in linear memory — the set is sorted and compacted by
/// those two, and the shift loops stay plain word copies — while their
/// processes live in the parallel [`GLOBAL_SIM_NOTICE_PROCS`] spine under the
/// *same* notice index, so the two halves move together by construction.
pub(in crate::codegen::wasm) fn emit_sim_ref_body(name: &str) -> Option<Function> {
    if !gc_objects_enabled() {
        return None;
    }
    let procs_ty = gc_ctx(|ctx| ctx.sim_notice_procs_ty)?;
    if std::env::var("SIM_SIM_GC_OFF").is_ok() {
        let _ = procs_ty;
        return None;
    }
    if std::env::var("SIM_SIM_GC_GLOBALS_ONLY").is_ok()
        && matches!(
            name,
            sim_runtime::SIM_NOTICE_PROCESS_STORE | sim_runtime::SIM_NOTICE_PROCESS_LOAD
        )
    {
        return None;
    }
    match name {
        sim_runtime::SIM_CURRENT_STORE => Some(emit_sim_global_store_gc(GLOBAL_SIM_CURRENT)),
        sim_runtime::SIM_CURRENT_LOAD => Some(emit_sim_global_load_gc(GLOBAL_SIM_CURRENT)),
        sim_runtime::SIM_RUNNING_STORE => Some(emit_sim_global_store_gc(GLOBAL_SIM_RUNNING)),
        sim_runtime::SIM_RUNNING_LOAD => Some(emit_sim_global_load_gc(GLOBAL_SIM_RUNNING)),
        sim_runtime::SIM_MAIN_STORE => Some(emit_sim_global_store_gc(GLOBAL_SIM_MAIN)),
        sim_runtime::SIM_MAIN_LOAD => Some(emit_sim_global_load_gc(GLOBAL_SIM_MAIN)),
        sim_runtime::SIM_NOTICE_PROCESS_STORE => Some(emit_sim_notice_process_store_gc(procs_ty)),
        sim_runtime::SIM_NOTICE_PROCESS_LOAD => Some(emit_sim_notice_process_load_gc(procs_ty)),
        _ => None,
    }
}

/// `sim_current_store(value)` and friends — one `global.set`.
pub(in crate::codegen::wasm) fn emit_sim_global_store_gc(global: u32) -> Function {
    // 0 = value (eqref).
    let mut body = Function::new([]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::GlobalSet(global));
    body.instruction(&Instruction::End);
    body
}

/// `sim_current_load()` and friends — one `global.get`.
pub(in crate::codegen::wasm) fn emit_sim_global_load_gc(global: u32) -> Function {
    let mut body = Function::new([]);
    body.instruction(&Instruction::GlobalGet(global));
    body.instruction(&Instruction::End);
    body
}

/// `notice_process_store(index, value)` as an `array.set`, widening the
/// process column first when `index` is past its end.
///
/// The column is replaced wholesale on growth (allocate, `array.copy`,
/// `global.set`) rather than resized in place, which WasmGC has no operation
/// for. Every reader goes through [`GLOBAL_SIM_NOTICE_PROCS`], so nothing
/// holds a stale array.
pub(in crate::codegen::wasm) fn emit_sim_notice_process_store_gc(procs_ty: u32) -> Function {
    // 0 = index (i64), 1 = value (eqref); 2 = index as i32, 3 = capacity,
    // 4 = new capacity, 5 = the wider column.
    let (index, capacity, new_capacity, wider) = (2, 3, 4, 5);
    let mut body = Function::new([
        (3, ValType::I32),
        (1, crate::codegen::wasm_gc::concrete_ref_null(procs_ty)),
    ]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(index));

    emit_sim_notice_procs_len(&mut body, capacity);

    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::If(BlockType::Empty));

    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Mul);
    body.instruction(&Instruction::LocalSet(new_capacity));
    body.instruction(&Instruction::LocalGet(new_capacity));
    body.instruction(&Instruction::I32Const(SIM_NOTICE_PROCS_INITIAL_SLOTS));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(SIM_NOTICE_PROCS_INITIAL_SLOTS));
    body.instruction(&Instruction::LocalSet(new_capacity));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(new_capacity));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32LeU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(new_capacity));
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::LocalGet(new_capacity));
    body.instruction(&Instruction::ArrayNewDefault(procs_ty));
    body.instruction(&Instruction::LocalSet(wider));

    // `array.copy` traps on a null source, and the column starts null.
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(wider));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::GlobalGet(GLOBAL_SIM_NOTICE_PROCS));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalGet(capacity));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: procs_ty,
        array_type_index_src: procs_ty,
    });
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::LocalGet(wider));
    body.instruction(&Instruction::GlobalSet(GLOBAL_SIM_NOTICE_PROCS));
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::GlobalGet(GLOBAL_SIM_NOTICE_PROCS));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::LocalGet(1));
    body.instruction(&Instruction::ArraySet(procs_ty));
    body.instruction(&Instruction::End);
    body
}

/// `notice_process_load(index)` as an `array.get`, `none` past the end.
///
/// Reading past the end is not a program error: `sim_begin` clears the set
/// before anything has been scheduled, so the column can still be null while
/// the linear notices exist.
pub(in crate::codegen::wasm) fn emit_sim_notice_process_load_gc(procs_ty: u32) -> Function {
    // 0 = index (i64); 1 = index as i32, 2 = column length.
    let (index, length) = (1, 2);
    let mut body = Function::new([(2, ValType::I32)]);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(index));

    emit_sim_notice_procs_len(&mut body, length);

    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::LocalGet(length));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::If(BlockType::Result(
        crate::codegen::wasm_gc::anyref_val(),
    )));
    body.instruction(&Instruction::RefNull(
        crate::codegen::wasm_gc::object_ref_heap(),
    ));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::GlobalGet(GLOBAL_SIM_NOTICE_PROCS));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::ArrayGet(procs_ty));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body
}

/// `dest := |process column|`, or 0 while it is still null (`array.len`
/// traps on null).
pub(in crate::codegen::wasm) fn emit_sim_notice_procs_len(body: &mut Function, dest: u32) {
    body.instruction(&Instruction::GlobalGet(GLOBAL_SIM_NOTICE_PROCS));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::GlobalGet(GLOBAL_SIM_NOTICE_PROCS));
    body.instruction(&Instruction::ArrayLen);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(dest));
}
