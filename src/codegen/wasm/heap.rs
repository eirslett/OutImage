//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// How much a bump allocation asks for: a size known when the code is emitted,
/// or one computed at run time into a local.
#[derive(Clone, Copy)]
pub(in crate::codegen::wasm) enum BumpSize {
    Fixed(i32),
    Dynamic(u32),
}

/// Pages taken beyond what an allocation needs, so a run of small allocations
/// does not call `memory.grow` once each.
pub(in crate::codegen::wasm) const HEAP_GROW_SLACK_PAGES: i32 = 32;

pub(in crate::codegen::wasm) fn emit_alloc_end(body: &mut Function, base: u32, size: BumpSize) {
    body.instruction(&Instruction::LocalGet(base));
    match size {
        BumpSize::Fixed(bytes) => body.instruction(&Instruction::I32Const(bytes)),
        BumpSize::Dynamic(local) => body.instruction(&Instruction::LocalGet(local)),
    };
    body.instruction(&Instruction::I32Add);
}

pub(in crate::codegen::wasm) fn emit_memory_bytes(body: &mut Function) {
    body.instruction(&Instruction::MemorySize(0));
    body.instruction(&Instruction::I32Const(16));
    body.instruction(&Instruction::I32Shl);
}

/// Grows linear memory when an allocation at `base` would end past it. The
/// module reserves what a small program needs up front, but a program with
/// components allocates a spill buffer per component and cannot be sized ahead
/// of time. Stack-neutral, and it recomputes the end rather than holding it, so
/// it drops into call sites that have no scratch local to spare.
///
/// A refused grow traps here rather than letting the caller bump its cursor
/// past the end of memory, which would either fault on an unrelated access
/// later or silently alias live data once the cursor wrapped.
pub(in crate::codegen::wasm) fn emit_heap_grow_if_needed(
    body: &mut Function,
    base: u32,
    size: BumpSize,
) {
    emit_alloc_end(body, base, size);
    emit_memory_bytes(body);
    body.instruction(&Instruction::I32GtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_alloc_end(body, base, size);
    emit_memory_bytes(body);
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Const(0xFFFF));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(16));
    body.instruction(&Instruction::I32ShrU);
    body.instruction(&Instruction::I32Const(HEAP_GROW_SLACK_PAGES));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::MemoryGrow(0));
    // memory.grow yields the previous page count, or -1 when the host refuses.
    body.instruction(&Instruction::I32Const(-1));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable); // out of memory
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_bump_alloc(body: &mut Function, size: i32, scratch: u32) {
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    // `local.set` (not `tee`): callers reload via `local.get`; leaving the
    // pointer on the stack breaks `if` arms typed as empty (e.g. text.copy).
    body.instruction(&Instruction::LocalSet(scratch));
    emit_heap_grow_if_needed(body, scratch, BumpSize::Fixed(size));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Const(size));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
}

/// Zero-fill `size` bytes starting at address in `base`, using `cursor`/`remaining`
/// as scratch (matching array alloc / `calloc` semantics for objects).
pub(in crate::codegen::wasm) fn emit_zero_fill(
    body: &mut Function,
    base: u32,
    size: i32,
    cursor: u32,
    remaining: u32,
) {
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::LocalSet(cursor));
    body.instruction(&Instruction::I32Const(size));
    body.instruction(&Instruction::LocalSet(remaining));
    emit_zero_fill_loop(body, cursor, remaining);
}

/// Zero-fill `len` bytes at `base`, where both are i32 locals (dynamic `HeapAlloc`).
pub(in crate::codegen::wasm) fn emit_zero_fill_dynamic(
    body: &mut Function,
    base: u32,
    len: u32,
    cursor: u32,
    remaining: u32,
) {
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::LocalSet(cursor));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::LocalSet(remaining));
    emit_zero_fill_loop(body, cursor, remaining);
}

pub(in crate::codegen::wasm) fn emit_zero_fill_loop(
    body: &mut Function,
    cursor: u32,
    remaining: u32,
) {
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(remaining));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(cursor));
    body.instruction(&Instruction::LocalGet(remaining));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(remaining));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}
