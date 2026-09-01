//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// `frame.<offset> = value`, where `frame` is an i32 local holding the address.
pub(in crate::codegen::wasm) fn emit_frame_store_const(
    body: &mut Function,
    frame: u32,
    offset: u64,
    value: i32,
) {
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::I32Const(value));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

/// `frame.<offset> = value`, where both are i32 locals.
pub(in crate::codegen::wasm) fn emit_frame_store_local(
    body: &mut Function,
    frame: u32,
    offset: u64,
    value: u32,
) {
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::LocalGet(value));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

/// Pushes `frame.<offset>`.
pub(in crate::codegen::wasm) fn emit_frame_load(body: &mut Function, frame: u32, offset: u64) {
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

/// `dest.<offset> = src.<offset>` for two i32 frame-address locals.
pub(in crate::codegen::wasm) fn emit_frame_copy_field(
    body: &mut Function,
    dest: u32,
    src: u32,
    offset: u64,
) {
    body.instruction(&Instruction::LocalGet(dest));
    emit_frame_load(body, src, offset);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

pub(in crate::codegen::wasm) fn emit_text_notext(body: &mut Function, dest: LocalId, scratch: u32) {
    emit_bump_alloc(body, FRAME_SIZE, scratch);
    // notext: empty view, and constant per the text attribute `constant`.
    emit_frame_store_const(body, scratch, FRAME_OFF_PTR, 0);
    emit_frame_store_const(body, scratch, FRAME_OFF_LEN, 0);
    emit_frame_store_const(body, scratch, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, scratch, FRAME_OFF_PAD, 1);
    emit_frame_store_const(body, scratch, FRAME_OFF_START, 1);
    emit_frame_store_const(body, scratch, FRAME_OFF_MAIN_LEN, 0);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

// ---------------------------------------------------------------------------
// WasmGC Text lowering.
//
// `text_frame = struct { chars: (array i8), start: i32, length: i32,
// pos: i32, constant: i32 }` (`src/codegen/wasm_gc.rs`). `chars` is the
// *whole* owning text object's character storage; a `sub`-text shares the
// same `chars` ref with its parent and only shifts `start`/`length`, which is
// what makes `t.sub(..)` a real TEXTOBJ share instead of a copy.
// ---------------------------------------------------------------------------

pub(in crate::codegen::wasm) fn text_frame_field_types() -> Result<(u32, HeapType), CompileError> {
    gc_ctx(|ctx| (ctx.text_frame_ty, ctx.text_chars_heap()))
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))
}

/// Pushes a `notext` `text_frame` value onto the stack: empty (null) `chars`,
/// `start = pos = constant = 1`, `length = 0` — mirrors [`emit_text_notext`]'s
/// bump layout field-for-field. Shared by [`emit_text_notext_gc`] (MIR local
/// destination) and callers that need a notext *value* mid-expression (e.g.
/// filling a fresh `ArrayText` with per-element notext frames).
pub(in crate::codegen::wasm) fn emit_push_notext_frame(
    body: &mut Function,
) -> Result<(), CompileError> {
    let (frame_ty, chars_heap) = text_frame_field_types()?;
    body.instruction(&Instruction::RefNull(chars_heap));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::I32Const(0)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(1)); // constant
    body.instruction(&Instruction::StructNew(frame_ty));
    Ok(())
}

/// Builds a fresh, mutable WasmGC `text_frame` (`start = pos = 1`,
/// `constant = 0`) whose content is a byte-for-byte copy of the
/// linear-memory range `[ptr, ptr+len)`, then leaves it on the stack.
/// `len < 1` yields a notext frame ([`emit_push_notext_frame`]) — this is
/// the WasmGC counterpart of pointing a bump Text local straight at a
/// `{ptr, len}` linear buffer, used by host-originated text content
/// (`CallInLine`'s stdin line read, `CallBasicioFilename`/
/// `CallBasicioInText`'s disk-file host bridge). `ptr`/`len` are raw `i32`
/// locals; `idx` is a scratch `i32` loop counter; `ch0` is a scratch
/// `text_chars` ref.
pub(in crate::codegen::wasm) fn emit_push_text_frame_from_linear_bytes(
    body: &mut Function,
    ptr: u32,
    len: u32,
    idx: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Result(
        crate::codegen::wasm_gc::concrete_ref_null(frame_ty),
    )));
    emit_push_notext_frame(body)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::ArrayNewDefault(chars_ty));
    body.instruction(&Instruction::LocalSet(ch0));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::LocalGet(len)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(0)); // constant
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::End);
    Ok(())
}

/// Decode UTF-8 of ranks 0–255 from linear memory into a WasmGC text frame.
/// Invalid UTF-8 or a codepoint above 255 traps. `idx` is the source byte
/// cursor; `dst` the destination character index; `nchars` the decoded
/// length; `tmp` the current lead byte.
pub(in crate::codegen::wasm) fn emit_push_text_frame_from_utf8_bytes(
    body: &mut Function,
    ptr: u32,
    len: u32,
    idx: u32,
    ch0: u32,
    dst: u32,
    nchars: u32,
    tmp: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Result(
        crate::codegen::wasm_gc::concrete_ref_null(frame_ty),
    )));
    emit_push_notext_frame(body)?;
    body.instruction(&Instruction::Else);
    emit_utf8_count_chars(body, ptr, len, idx, nchars, tmp);
    body.instruction(&Instruction::LocalGet(nchars));
    body.instruction(&Instruction::ArrayNewDefault(chars_ty));
    body.instruction(&Instruction::LocalSet(ch0));
    emit_utf8_fill_chars(body, ptr, len, idx, ch0, dst, nchars, tmp, chars_ty);
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::LocalGet(nchars)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(0)); // constant
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::End);
    Ok(())
}

fn emit_utf8_count_chars(body: &mut Function, ptr: u32, len: u32, idx: u32, nchars: u32, tmp: u32) {
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(nchars));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    emit_utf8_load_byte(body, ptr, idx, tmp);
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(128));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::Else);
    emit_utf8_require_c2_c3(body, ptr, len, idx, tmp);
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(nchars));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(nchars));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

fn emit_utf8_fill_chars(
    body: &mut Function,
    ptr: u32,
    len: u32,
    idx: u32,
    ch0: u32,
    dst: u32,
    nchars: u32,
    tmp: u32,
    chars_ty: u32,
) {
    let _ = nchars;
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(dst));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    emit_utf8_load_byte(body, ptr, idx, tmp);
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::LocalGet(dst));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(128));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::Else);
    emit_utf8_require_c2_c3(body, ptr, len, idx, tmp);
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(0x1F));
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::I32Const(6));
    body.instruction(&Instruction::I32Shl);
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(0x3F));
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(dst));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(dst));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

fn emit_utf8_load_byte(body: &mut Function, ptr: u32, idx: u32, tmp: u32) {
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(tmp));
}

fn emit_utf8_require_c2_c3(body: &mut Function, ptr: u32, len: u32, idx: u32, tmp: u32) {
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(0xC2));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(0xC3));
    body.instruction(&Instruction::I32GtU);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(ptr));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(0xC0));
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::I32Const(0x80));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
}

/// `notext`: empty (null) `chars`, `start = pos = constant = 1`, `length = 0`
/// — mirrors [`emit_text_notext`]'s bump layout field-for-field.
pub(in crate::codegen::wasm) fn emit_text_notext_gc(
    body: &mut Function,
    dest: LocalId,
) -> Result<(), CompileError> {
    emit_push_notext_frame(body)?;
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// Pushes a deep copy of the `text_frame` value held in local `src_idx`
/// (a raw wasm local index — either a real MIR [`LocalId`] via
/// [`local_index`] or a scratch holding an array element) onto the stack: a
/// fresh `chars` array holding a byte-for-byte copy of the source's *view*
/// (via `array.copy`, not the whole backing array), fresh `pos = 1`, and the
/// source's own `constant` flag — matching [`emit_text_copy_gc`]'s semantics.
/// `s0`/`s1` are scratch `i32` locals; `ch0` is a scratch `text_chars` ref.
pub(in crate::codegen::wasm) fn emit_push_text_copy_from(
    body: &mut Function,
    src_idx: u32,
    s0: u32,
    s1: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(src_idx));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Result(
        crate::codegen::wasm_gc::concrete_ref_null(frame_ty),
    )));
    emit_push_notext_frame(body)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(src_idx));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::ArrayNewDefault(chars_ty));
    body.instruction(&Instruction::LocalSet(ch0));
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalGet(src_idx));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: chars_ty,
        array_type_index_src: chars_ty,
    });
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::LocalGet(s0)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(0)); // constant: copies are always fresh/mutable
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::End);
    Ok(())
}

/// Builds a fresh `(array i8)` directly from the module's dedicated passive
/// literal-payload data segment (index 1 — see `emit_mir_inner`) via
/// `array.new_data` — no linear scratch buffer or byte-copy loop needed,
/// since the literal's bytes already sit in that segment. `array.new_data`
/// only works against a *passive* segment at runtime (V8 traps with "data
/// segment out of bounds" targeting the active segment 0, even when the
/// offset/length are in-bounds for it), so `ptr` (a [`TEXT_BASE`]-relative
/// *linear-memory* address, from `iovecs`) must be rebased to segment-1's own
/// indexing by subtracting `TEXT_BASE` before use.
///
/// Each call still allocates a fresh array, so two occurrences of the same
/// literal keep distinct identities (`==`/`=/=`), matching bump semantics.
pub(in crate::codegen::wasm) fn emit_text_from_literal_gc(
    body: &mut Function,
    dest: LocalId,
    ptr: u32,
    len: u32,
) -> Result<(), CompileError> {
    if len == 0 {
        return emit_text_notext_gc(body, dest);
    }
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    let data_offset = ptr.checked_sub(TEXT_BASE).ok_or_else(|| {
        CompileError::codegen("MIR wasm: text literal pointer precedes TEXT_BASE")
    })?;
    body.instruction(&Instruction::I32Const(data_offset as i32));
    body.instruction(&Instruction::I32Const(len as i32));
    body.instruction(&Instruction::ArrayNewData {
        array_type_index: chars_ty,
        array_data_index: 1,
    });
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::I32Const(len as i32)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(1)); // constant
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// `dest = frame.<field>` (all four text-frame `i32` scalar attributes —
/// `start`/`length`/`pos`/`constant` — share this shape).
pub(in crate::codegen::wasm) fn emit_text_field_i32_gc(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    field_index: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index,
    });
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// `.main`: a view of the *whole* owning object — same `chars`, `start = 1`,
/// `length = chars.length`, fresh `pos = 1`, same `constant`. No stored
/// `main_len` field is needed (unlike bump's [`FRAME_OFF_MAIN_LEN`]): the
/// shared `chars` array's own length already is that value.
pub(in crate::codegen::wasm) fn emit_text_main_gc(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    chars_scratch: u32,
    len_scratch: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalTee(chars_scratch));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Empty));
    // `notext.main` is `notext` itself (bump-mode falls back to this when the
    // underlying buffer is empty/absent — see `emit_text_main`); avoid
    // `array.len` on a null `chars` ref, which would trap.
    emit_text_notext_gc(body, dest)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(chars_scratch));
    body.instruction(&Instruction::ArrayLen);
    body.instruction(&Instruction::LocalSet(len_scratch));
    body.instruction(&Instruction::LocalGet(chars_scratch));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::LocalGet(len_scratch)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    });
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    Ok(())
}

/// Each occurrence of a string literal is a distinct constant text object, so
/// two identical literals compare unequal under `==` / `=/=`.
pub(in crate::codegen::wasm) fn emit_text_from_literal(
    body: &mut Function,
    dest: LocalId,
    ptr: u32,
    len: u32,
    frame: u32,
    buf: u32,
    index: u32,
) {
    if len == 0 {
        emit_text_notext(body, dest, frame);
        return;
    }
    // Copy the static payload into a fresh buffer so this occurrence has its
    // own identity (the data-section bytes are shared across all uses).
    emit_bump_alloc(body, len as i32, buf);
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(index));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Const(len as i32));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(ptr as i32));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Add);
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
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(index));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);

    emit_bump_alloc(body, FRAME_SIZE, frame);
    emit_frame_store_local(body, frame, FRAME_OFF_PTR, buf);
    emit_frame_store_const(body, frame, FRAME_OFF_LEN, len as i32);
    emit_frame_store_const(body, frame, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, frame, FRAME_OFF_PAD, 1);
    emit_frame_store_const(body, frame, FRAME_OFF_START, 1);
    emit_frame_store_const(body, frame, FRAME_OFF_MAIN_LEN, len as i32);
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `dest :- src` (Text reference assignment, §4.7 — not a value copy) under
/// WasmGC. Mirrors [`emit_text_share_assign`]'s bump semantics: `dest` (often
/// itself a value just loaded from a field/array element, not a bare
/// variable — see the MIR `text.ref_assign` producer) names an *existing*
/// `text_frame` identity whose fields get overwritten in place to alias
/// `src`'s view, rather than rebinding the `dest` local to a different ref.
/// Since `dest` is a live GC reference, any other holder of that same
/// reference (e.g. the class field `dest` was just loaded from) observes the
/// mutation too — unlike a bump-mode local, no separate field/array
/// write-back is needed.
///
/// Wasm locals of GC-ref type start as `ref.null`. When `dest` is still null
/// (no MIR `notext` init yet, or a never-written class `image` slot), allocate
/// a notext frame first so there is an identity to mutate; callers that
/// write the local back to a field then publish that frame.
pub(in crate::codegen::wasm) fn emit_text_ref_assign_gc(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_push_notext_frame(body)?;
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::RefIsNull);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    for field_index in [
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    ] {
        body.instruction(&Instruction::LocalGet(local_index(dest)));
        body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(frame_ty)));
        body.instruction(&Instruction::LocalGet(local_index(src)));
        body.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(frame_ty)));
        body.instruction(&Instruction::StructGet {
            struct_type_index: frame_ty,
            field_index,
        });
        body.instruction(&Instruction::StructSet {
            struct_type_index: frame_ty,
            field_index,
        });
    }
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_text_share_assign(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
    scratch0: u32,
    scratch1: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch0));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch1));
    for offset in [
        FRAME_OFF_PTR,
        FRAME_OFF_LEN,
        FRAME_OFF_POS,
        FRAME_OFF_PAD,
        FRAME_OFF_START,
        FRAME_OFF_MAIN_LEN,
    ] {
        emit_frame_copy_field(body, scratch0, scratch1, offset);
    }
}

/// Copies a WasmGC `text_frame`'s view of `chars` into a fresh linear-memory
/// scratch buffer (bump-allocated for exactly `length` bytes) and leaves the
/// buffer address in `s_addr` and its length in `s_len`. Text/Array *values*
/// stay WasmGC refs; only this transient host-IO bridge touches linear
/// memory — linear memory stays for WASI/IO scratch only.
/// WASI/BASICIO still take `(ptr, len)` into linear memory, and JS/Host `text`
/// FFI uses the same scratch only inside compiler glue (`simula.text_from_bytes`)
/// so application JS sees a `String`. Both [`emit_out_text_local_gc`] (`fd_write`)
/// and [`emit_disk_out_text_local_gc`] (`basicio_out_text`) share this bridge.
pub(in crate::codegen::wasm) fn emit_text_to_linear_scratch_gc(
    body: &mut Function,
    src: LocalId,
    s_addr: u32,
    s_idx: u32,
    s_len: u32,
    s_start: u32,
    chars_scratch: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;

    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s_len));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalSet(s_start));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalSet(chars_scratch));

    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s_addr));
    emit_heap_grow_if_needed(body, s_addr, BumpSize::Dynamic(s_len));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s_idx));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(chars_scratch));
    body.instruction(&Instruction::LocalGet(s_start));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::ArrayGetU(chars_ty));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s_idx));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

/// Same bridge as [`emit_text_to_linear_scratch_gc`], but each rank 0–255 is
/// written as UTF-8 (`U+0000`..`U+00FF`). Scratch is sized `2 * nchars`;
/// `s_len` is the encoded byte length on return. `s_dst` is the write cursor;
/// `s_ch` holds the current rank across the 1-byte / 2-byte branch.
pub(in crate::codegen::wasm) fn emit_text_to_linear_scratch_utf8_gc(
    body: &mut Function,
    src: LocalId,
    s_addr: u32,
    s_idx: u32,
    s_len: u32,
    s_start: u32,
    chars_scratch: u32,
    s_dst: u32,
    s_ch: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;

    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s_len));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalSet(s_start));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalSet(chars_scratch));

    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s_addr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Shl);
    body.instruction(&Instruction::LocalSet(s_dst));
    emit_heap_grow_if_needed(body, s_addr, BumpSize::Dynamic(s_dst));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s_idx));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(s_dst));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(chars_scratch));
    body.instruction(&Instruction::LocalGet(s_start));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::ArrayGetU(chars_ty));
    body.instruction(&Instruction::LocalTee(s_ch));
    body.instruction(&Instruction::I32Const(128));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(s_ch));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s_dst));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(s_ch));
    body.instruction(&Instruction::I32Const(6));
    body.instruction(&Instruction::I32ShrU);
    body.instruction(&Instruction::I32Const(0xC0));
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(s_ch));
    body.instruction(&Instruction::I32Const(0x3F));
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::I32Const(0x80));
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s_dst));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s_idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s_idx));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s_dst));
    body.instruction(&Instruction::LocalSet(s_len));
    Ok(())
}

/// Bytewise content equality on two linear buffers (`left_ptr`/`right_ptr` +
/// matching lengths). Shared by bump `TextContentEq` and the WasmGC scratch
/// bridge in [`emit_text_content_eq_gc`].
pub(in crate::codegen::wasm) fn emit_linear_content_eq(
    body: &mut Function,
    dest: LocalId,
    left_ptr: u32,
    left_len: u32,
    right_ptr: u32,
    right_len: u32,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    body.instruction(&Instruction::LocalGet(left_len));
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(right_len));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I64Const(1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(left_ptr));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(right_ptr));
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::I64Const(1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Br(2));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// Lexicographic content compare on two linear buffers → i64 in {-1, 0, 1}.
pub(in crate::codegen::wasm) fn emit_linear_content_cmp(
    body: &mut Function,
    dest: LocalId,
    left_ptr: u32,
    left_len: u32,
    right_ptr: u32,
    right_len: u32,
    remaining: u32,
) {
    body.instruction(&Instruction::LocalGet(left_len));
    body.instruction(&Instruction::LocalGet(right_len));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(left_len));
    body.instruction(&Instruction::LocalSet(remaining));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(right_len));
    body.instruction(&Instruction::LocalSet(remaining));
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(remaining));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(left_ptr));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(right_ptr));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(left_ptr));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(right_ptr));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I64Const(-1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I64Const(1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Br(3));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(left_ptr));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(left_ptr));
    body.instruction(&Instruction::LocalGet(right_ptr));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(right_ptr));
    body.instruction(&Instruction::LocalGet(remaining));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(remaining));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::LocalGet(left_len));
    body.instruction(&Instruction::LocalGet(right_len));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(left_len));
    body.instruction(&Instruction::LocalGet(right_len));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I64Const(-1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I64Const(1));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// `TextContentEq` under WasmGC: copy both frames to linear scratch via
/// [`emit_text_to_linear_scratch_gc`], then [`emit_linear_content_eq`].
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_content_eq_gc(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s_addr: u32,
    s_idx: u32,
    s_len: u32,
    s_start: u32,
    left_ptr: u32,
    left_len: u32,
    right_ptr: u32,
    right_len: u32,
    chars_scratch: u32,
) -> Result<(), CompileError> {
    emit_text_to_linear_scratch_gc(body, left, s_addr, s_idx, s_len, s_start, chars_scratch)?;
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalSet(left_ptr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::LocalSet(left_len));
    emit_text_to_linear_scratch_gc(body, right, s_addr, s_idx, s_len, s_start, chars_scratch)?;
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalSet(right_ptr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::LocalSet(right_len));
    emit_linear_content_eq(
        body, dest, left_ptr, left_len, right_ptr, right_len, s_idx, s_start, s_addr, s_len,
    );
    Ok(())
}

/// `TextContentCmp` under WasmGC: same scratch bridge as
/// [`emit_text_content_eq_gc`], then [`emit_linear_content_cmp`].
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_content_cmp_gc(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s_addr: u32,
    s_idx: u32,
    s_len: u32,
    s_start: u32,
    left_ptr: u32,
    left_len: u32,
    right_ptr: u32,
    right_len: u32,
    chars_scratch: u32,
) -> Result<(), CompileError> {
    emit_text_to_linear_scratch_gc(body, left, s_addr, s_idx, s_len, s_start, chars_scratch)?;
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalSet(left_ptr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::LocalSet(left_len));
    emit_text_to_linear_scratch_gc(body, right, s_addr, s_idx, s_len, s_start, chars_scratch)?;
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalSet(right_ptr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::LocalSet(right_len));
    emit_linear_content_cmp(body, dest, left_ptr, left_len, right_ptr, right_len, s_idx);
    Ok(())
}

/// `OutText(t)` under WasmGC: bridges `t` to linear memory via
/// [`emit_text_to_linear_scratch_gc`] and hands the buffer to the same
/// `fd_write` iovec path as the bump build.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_out_text_local_gc(
    body: &mut Function,
    src: LocalId,
    s_addr: u32,
    s_idx: u32,
    s_len: u32,
    s_start: u32,
    chars_scratch: u32,
    sysout_write: u32,
) -> Result<(), CompileError> {
    emit_text_to_linear_scratch_gc(body, src, s_addr, s_idx, s_len, s_start, chars_scratch)?;
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_sysout_write_iov(body, SCRATCH_IOV, sysout_write);
    Ok(())
}

/// `basicio.OutText(t)` (disk-file branch of [`Op::CallBasicioOutText`])
/// under WasmGC: bridges `t` to linear memory via
/// [`emit_text_to_linear_scratch_gc`] and calls the same `basicio_out_text`
/// host import as the bump build. Disk files never reach codegen today
/// (`ensure_supported_subset` rejects `RegisterFile`), so this is
/// unreachable at runtime but must still type-check.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_disk_out_text_local_gc(
    body: &mut Function,
    object_host_i32: u32,
    src: LocalId,
    s_addr: u32,
    s_idx: u32,
    s_len: u32,
    s_start: u32,
    chars_scratch: u32,
    basicio_out_text: u32,
) -> Result<(), CompileError> {
    emit_text_to_linear_scratch_gc(body, src, s_addr, s_idx, s_len, s_start, chars_scratch)?;
    body.instruction(&Instruction::LocalGet(object_host_i32));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalGet(s_addr));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::Call(basicio_out_text));
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_out_text_local(
    body: &mut Function,
    src: LocalId,
    scratch0: u32,
    scratch1: u32,
    sysout_write: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch0));
    // content ptr
    body.instruction(&Instruction::LocalGet(scratch0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(scratch1));
    // length
    body.instruction(&Instruction::LocalGet(scratch0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(scratch0));
    // scratch iovec
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(scratch1));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(scratch0));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_sysout_write_iov(body, SCRATCH_IOV, sysout_write);
}

/// Bytewise memcpy: copies `len` bytes from `src` to `dest` (all i32 locals).
/// Text *value* assignment (`T := U`): characters are written into the
/// destination's own frame and blank-padded, rather than the destination being
/// re-pointed at the source. Assigning into notext adopts the source instead,
/// and an over-long source or a constant destination traps.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_assign_value(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
    d: u32,
    s: u32,
    d_len: u32,
    s_len: u32,
    cursor: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(d));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s));
    emit_frame_load(body, d, FRAME_OFF_LEN);
    body.instruction(&Instruction::LocalSet(d_len));
    emit_frame_load(body, s, FRAME_OFF_LEN);
    body.instruction(&Instruction::LocalSet(s_len));

    body.instruction(&Instruction::LocalGet(d_len));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    // notext destination: adopt the source view (both notext leaves pos = 1).
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_frame_store_const(body, d, FRAME_OFF_POS, 1);
    body.instruction(&Instruction::Else);
    for offset in [
        FRAME_OFF_PTR,
        FRAME_OFF_LEN,
        FRAME_OFF_POS,
        FRAME_OFF_PAD,
        FRAME_OFF_START,
        FRAME_OFF_MAIN_LEN,
    ] {
        emit_frame_copy_field(body, d, s, offset);
    }
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Else);

    // Longer source, or writing through a constant view, is an error.
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::LocalGet(d_len));
    body.instruction(&Instruction::I32GtS);
    emit_frame_load(body, d, FRAME_OFF_PAD);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);

    // cursor = dest content; copy s_len bytes, then blank-fill the remainder.
    emit_frame_load(body, d, FRAME_OFF_PTR);
    body.instruction(&Instruction::LocalSet(cursor));
    emit_frame_load(body, s, FRAME_OFF_PTR);
    body.instruction(&Instruction::LocalSet(s));
    body.instruction(&Instruction::LocalGet(d_len));
    body.instruction(&Instruction::LocalGet(s_len));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(d_len));
    emit_memcpy(body, cursor, s, s_len);
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(d_len));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(cursor));
    body.instruction(&Instruction::LocalGet(d_len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(d_len));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_memcpy(body: &mut Function, dest: u32, src: u32, len: u32) {
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(dest));
    body.instruction(&Instruction::LocalGet(src));
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
    body.instruction(&Instruction::LocalGet(dest));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(dest));
    body.instruction(&Instruction::LocalGet(src));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(src));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// Builds a frame that is the whole of a freshly allocated buffer, so it is its
/// own main object. Views into a larger object fix up `start`/`main_len` after.
pub(in crate::codegen::wasm) fn emit_frame_from_buf(
    body: &mut Function,
    dest: LocalId,
    buf: u32,
    len: u32,
    frame_scratch: u32,
) {
    emit_bump_alloc(body, FRAME_SIZE, frame_scratch);
    emit_frame_store_local(body, frame_scratch, FRAME_OFF_PTR, buf);
    emit_frame_store_local(body, frame_scratch, FRAME_OFF_LEN, len);
    emit_frame_store_const(body, frame_scratch, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, frame_scratch, FRAME_OFF_PAD, 0);
    emit_frame_store_const(body, frame_scratch, FRAME_OFF_START, 1);
    emit_frame_store_local(body, frame_scratch, FRAME_OFF_MAIN_LEN, len);
    body.instruction(&Instruction::LocalGet(frame_scratch));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `blanks(n)` under WasmGC: fresh `(array i8)` filled with spaces and a new
/// variable (`constant = 0`) `text_frame` — mirrors [`emit_text_blanks`].
pub(in crate::codegen::wasm) fn emit_text_blanks_gc(
    body: &mut Function,
    dest: LocalId,
    n: LocalId,
    count: u32,
    idx: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;

    body.instruction(&Instruction::LocalGet(local_index(n)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalTee(count));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext_gc(body, dest)?;
    body.instruction(&Instruction::Else);

    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::ArrayNewDefault(chars_ty));
    body.instruction(&Instruction::LocalSet(ch0));

    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::LocalGet(count));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(idx));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::LocalGet(count)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(0)); // constant (variable)
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    Ok(())
}

/// Builds a temporary linear bump [`FRAME_SIZE`] header pointing at content
/// copied from a WasmGC `text_frame`, for host imports (`text_putreal`, …).
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_prepare_host_frame_gc(
    body: &mut Function,
    frame: LocalId,
    content_ptr: u32,
    loop_idx: u32,
    content_len: u32,
    start_field: u32,
    host_frame: u32,
    pos_field: u32,
    chars_scratch: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    emit_text_to_linear_scratch_gc(
        body,
        frame,
        content_ptr,
        loop_idx,
        content_len,
        start_field,
        chars_scratch,
    )?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    body.instruction(&Instruction::LocalSet(pos_field));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalSet(start_field));
    emit_bump_alloc(body, FRAME_SIZE, host_frame);
    emit_frame_store_local(body, host_frame, FRAME_OFF_PTR, content_ptr);
    emit_frame_store_local(body, host_frame, FRAME_OFF_LEN, content_len);
    emit_frame_store_local(body, host_frame, FRAME_OFF_POS, pos_field);
    // Host `editNumeric` treats pad != 0 as constant (same as bump-mode frames).
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    });
    body.instruction(&Instruction::LocalSet(loop_idx));
    emit_frame_store_local(body, host_frame, FRAME_OFF_PAD, loop_idx);
    emit_frame_store_local(body, host_frame, FRAME_OFF_START, start_field);
    emit_frame_store_local(body, host_frame, FRAME_OFF_MAIN_LEN, content_len);
    Ok(())
}

/// Writes host-edited linear content and `pos` back into a WasmGC `text_frame`
/// after a mutating host text import.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_finish_host_frame_gc(
    body: &mut Function,
    frame: LocalId,
    content_ptr: u32,
    content_len: u32,
    loop_idx: u32,
    start_field: u32,
    host_frame: u32,
    chars_scratch: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;

    body.instruction(&Instruction::LocalGet(local_index(frame)));
    emit_frame_load(body, host_frame, FRAME_OFF_POS);
    body.instruction(&Instruction::StructSet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });

    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalSet(chars_scratch));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalSet(start_field));

    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(loop_idx));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(loop_idx));
    body.instruction(&Instruction::LocalGet(content_len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(chars_scratch));
    body.instruction(&Instruction::LocalGet(start_field));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(loop_idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(content_ptr));
    body.instruction(&Instruction::LocalGet(loop_idx));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(loop_idx));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(loop_idx));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

/// `putreal` under WasmGC: bridge to the linear host import, then sync back.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_putreal_gc(
    body: &mut Function,
    frame: LocalId,
    value: LocalId,
    places: LocalId,
    exp_digits: i64,
    content_ptr: u32,
    content_len: u32,
    loop_idx: u32,
    start_field: u32,
    host_frame: u32,
    pos_field: u32,
    chars_scratch: u32,
    text_putreal: u32,
) -> Result<(), CompileError> {
    emit_text_prepare_host_frame_gc(
        body,
        frame,
        content_ptr,
        loop_idx,
        content_len,
        start_field,
        host_frame,
        pos_field,
        chars_scratch,
    )?;
    body.instruction(&Instruction::LocalGet(host_frame));
    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::LocalGet(local_index(places)));
    body.instruction(&Instruction::I64Const(exp_digits));
    body.instruction(&Instruction::Call(text_putreal));
    emit_text_finish_host_frame_gc(
        body,
        frame,
        content_ptr,
        content_len,
        loop_idx,
        start_field,
        host_frame,
        chars_scratch,
    )
}

/// `dest :- copy(src)` (§4.6.2 value text formal) under WasmGC: a fresh
/// `chars` array holding a byte-for-byte copy of `src`'s *view* — via
/// `array.copy` from `src`'s `chars` at `start - 1` for `length` bytes, not a
/// copy of `src`'s whole backing array — matching [`emit_text_copy`]'s bump
/// semantics (which only copies the `ptr`/`len` window). Later mutation of
/// the copy's characters therefore never aliases the caller's text.
pub(in crate::codegen::wasm) fn emit_text_copy_gc(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
    s0: u32,
    s1: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    emit_push_text_copy_from(body, local_index(src), s0, s1, ch0)?;
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// `frame.more` — `pos <= length` — under WasmGC: mirrors [`emit_text_more`]
/// field-for-field via `StructGet`.
pub(in crate::codegen::wasm) fn emit_text_more_gc(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::I32LeS);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

/// `frame.setpos(index)` under WasmGC: mirrors [`emit_text_setpos`]
/// field-for-field via `StructGet`/`StructSet`.
pub(in crate::codegen::wasm) fn emit_text_setpos_gc(
    body: &mut Function,
    frame: LocalId,
    index: LocalId,
    s0: u32,
    s1: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s0)); // length
    body.instruction(&Instruction::LocalGet(local_index(index)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s1)); // index as i32
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    // if index < 1 || index > length+1 then pos = length+1 else pos = index
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::StructSet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    Ok(())
}

/// `frame.getchar` under WasmGC: mirrors [`emit_text_getchar`] — traps when
/// `pos > length`; the byte at 0-based `chars[start - 1 + pos - 1]` (view
/// index `pos - 1`), then `pos += 1`.
pub(in crate::codegen::wasm) fn emit_text_getchar_gc(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    s1: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    body.instruction(&Instruction::LocalSet(s0)); // pos
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s1)); // start + pos - 2
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::ArrayGetU(chars_ty));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::StructSet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    Ok(())
}

/// `frame.putchar(ch)` under WasmGC: mirrors [`emit_text_putchar`] — traps on
/// constant / notext / `pos` out of range, then `array.set`s the byte at the
/// same 0-based index as [`emit_text_getchar_gc`] and advances `pos`.
pub(in crate::codegen::wasm) fn emit_text_putchar_gc(
    body: &mut Function,
    frame: LocalId,
    ch: LocalId,
    s0: u32,
    s1: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    });
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    body.instruction(&Instruction::LocalSet(s0)); // pos
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s1)); // length
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(local_index(ch)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::StructSet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    Ok(())
}

/// `frame.strip` under WasmGC: trims trailing blanks by shortening `length`
/// (0-based index `start - 1 + length - 1`), sharing `chars`/`start` with the
/// parent view — mirrors [`emit_text_strip`]'s bump semantics (a `sub`-style
/// view, not a byte copy).
pub(in crate::codegen::wasm) fn emit_text_strip_gc(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    s1: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalSet(s0)); // start
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s1)); // remaining length
    // while len > 0 && chars[start - 1 + len - 1] == ' ': len--
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::ArrayGetU(chars_ty));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext_gc(body, dest)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(s0)); // start (unchanged)
    body.instruction(&Instruction::LocalGet(s1)); // trimmed length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    });
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    Ok(())
}

/// ENVIRONMENT `upcase`/`lowcase` under WasmGC: mutates `chars` in place over
/// the view's window (0-based `start - 1 + i` for `i` in `[0, length)`) and
/// resets `pos` to 1 — mirrors [`emit_text_case_fold`]. Traps on notext
/// (`length == 0`) or constant frames.
pub(in crate::codegen::wasm) fn emit_text_case_fold_gc(
    body: &mut Function,
    frame: LocalId,
    upcase: bool,
    s0: u32,
    s1: u32,
    s2: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    });
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::StructSet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s1)); // idx cursor (0-based)
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s2)); // remaining
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::ArrayGetU(chars_ty));
    if upcase {
        body.instruction(&Instruction::LocalTee(s0));
        body.instruction(&Instruction::I32Const(b'a' as i32));
        body.instruction(&Instruction::I32GeU);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(b'z' as i32));
        body.instruction(&Instruction::I32LeU);
        body.instruction(&Instruction::I32And);
        body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(0x20));
        body.instruction(&Instruction::I32Xor);
        body.instruction(&Instruction::Else);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::End);
    } else {
        body.instruction(&Instruction::LocalTee(s0));
        body.instruction(&Instruction::I32Const(b'A' as i32));
        body.instruction(&Instruction::I32GeU);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(b'Z' as i32));
        body.instruction(&Instruction::I32LeU);
        body.instruction(&Instruction::I32And);
        body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(0x20));
        body.instruction(&Instruction::I32Or);
        body.instruction(&Instruction::Else);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::End);
    }
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

/// Simula `:=` text value assignment under WasmGC: mutates `dest`'s *own*
/// `text_frame` fields in place (so any `t :- dest` alias sees the update,
/// matching the bump path's shared-buffer mutation) rather than rebinding the
/// `dest` local to a fresh object.
///
/// - `dest` notext (length 0): adopts `src`'s view (`chars`/`start`/`length`/
///   `pos`/`constant`) when `src` is non-notext; a no-op when both are notext
///   (mirrors [`emit_text_assign_value`]'s `pos := 1` on an already-notext
///   frame).
/// - `dest` non-notext: `array.set`s `dest`'s own window (same `chars`
///   identity, so other views sharing it observe the write) from `src`'s
///   content, then blank-pads the remainder. Traps when `src` is longer than
///   `dest` or `dest` is constant.
pub(in crate::codegen::wasm) fn emit_text_assign_gc(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    let get = |body: &mut Function, who: LocalId, field: u32| {
        body.instruction(&Instruction::LocalGet(local_index(who)));
        body.instruction(&Instruction::StructGet {
            struct_type_index: frame_ty,
            field_index: field,
        });
    };
    get(body, dest, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH);
    body.instruction(&Instruction::LocalSet(s0)); // dest length
    get(body, src, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH);
    body.instruction(&Instruction::LocalSet(s1)); // src length

    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    // dest is notext: adopt src's view (no-op when src is also notext).
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::StructSet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
    });
    body.instruction(&Instruction::Else);
    for field in [
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    ] {
        body.instruction(&Instruction::LocalGet(local_index(dest)));
        get(body, src, field);
        body.instruction(&Instruction::StructSet {
            struct_type_index: frame_ty,
            field_index: field,
        });
    }
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Else);

    // Longer source, or writing through a constant destination, traps.
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32GtS);
    get(
        body,
        dest,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    );
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);

    // dest window base (0-based): dest.start - 1. Copy src's s1 bytes, then
    // blank-fill the remaining s0 - s1 bytes.
    get(body, dest, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s2)); // dest write cursor (0-based index)
    // Skip the `array.copy` entirely when `src` is `notext` (s1 == 0): its
    // `chars` field is null, and `array.copy` traps on a null operand even
    // with a zero count.
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    get(body, dest, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    body.instruction(&Instruction::LocalGet(s2));
    get(body, src, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    get(body, src, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: chars_ty,
        array_type_index_src: chars_ty,
    });
    body.instruction(&Instruction::End);
    // blank-fill: cursor = dest_base + src_len, remaining = dest_len - src_len.
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s3)); // remaining blanks
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    get(body, dest, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::ArraySet(chars_ty));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    Ok(())
}

/// `t1 & t2` under WasmGC: fresh `chars` array of combined length, filled via
/// two `array.copy`s from each operand's view — mirrors [`emit_text_concat`].
pub(in crate::codegen::wasm) fn emit_text_concat_gc(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let chars_ty = gc_ctx(|ctx| ctx.text_chars_ty)
        .ok_or_else(|| CompileError::codegen("MIR wasm: WasmGC context missing for text op"))?;
    let get = |body: &mut Function, who: LocalId, field: u32| {
        body.instruction(&Instruction::LocalGet(local_index(who)));
        body.instruction(&Instruction::StructGet {
            struct_type_index: frame_ty,
            field_index: field,
        });
    };
    get(body, left, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH);
    body.instruction(&Instruction::LocalSet(s0)); // left length
    get(
        body,
        right,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    );
    body.instruction(&Instruction::LocalSet(s1)); // right length
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2)); // total length
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext_gc(body, dest)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::ArrayNewDefault(chars_ty));
    body.instruction(&Instruction::LocalSet(ch0));
    // copy left into [0, left.len) — skip when left is notext (s0 == 0):
    // its `chars` field is null, and `array.copy` traps on a null operand
    // even with a zero count.
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(0));
    get(body, left, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    get(body, left, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: chars_ty,
        array_type_index_src: chars_ty,
    });
    body.instruction(&Instruction::End);
    // copy right into [left.len, total) — same null-guard as above.
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::LocalGet(s0));
    get(body, right, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    get(body, right, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: chars_ty,
        array_type_index_src: chars_ty,
    });
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(ch0));
    body.instruction(&Instruction::I32Const(1)); // start
    body.instruction(&Instruction::LocalGet(s2)); // length
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::I32Const(0)); // constant
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_text_copy(
    body: &mut Function,
    dest: LocalId,
    src: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    // s0 = src frame
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // s1 = content ptr, s2 = len
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, s0);
    body.instruction(&Instruction::Else);
    // s3 = new buffer of length s2
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s3));
    emit_heap_grow_if_needed(body, s3, BumpSize::Dynamic(s2));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalSet(s0)); // dest write cursor
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    emit_memcpy(body, s0, s1, s2);
    // restore len from src frame again
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    emit_frame_from_buf(body, dest, s3, s2, s0);
    body.instruction(&Instruction::End); // if
}

pub(in crate::codegen::wasm) fn emit_text_concat(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    // s0 = left.len, s1 = right.len, s2 = total
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, s0);
    body.instruction(&Instruction::Else);
    // s3 = buffer
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s3));
    emit_heap_grow_if_needed(body, s3, BumpSize::Dynamic(s2));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    // copy left into s3
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    emit_memcpy(body, s0, s1, s2);
    // s0 is now s3 + left.len; copy right
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    emit_memcpy(body, s0, s1, s2);
    // total len = right.frame start? compute from heap - s3
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s2));
    emit_frame_from_buf(body, dest, s3, s2, s0);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_text_content_eq(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s3));
    emit_linear_content_eq(body, dest, s2, s0, s3, s1, s0, s1, s2, s3);
}

/// Lexicographic content compare → i64 in {-1, 0, 1}.
pub(in crate::codegen::wasm) fn emit_text_content_cmp(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    left_len: u32,
    right_len: u32,
    left_ptr: u32,
    right_ptr: u32,
    remaining: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(left_len));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(right_len));
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(left_ptr));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(right_ptr));
    emit_linear_content_cmp(
        body, dest, left_ptr, left_len, right_ptr, right_len, remaining,
    );
}

pub(in crate::codegen::wasm) fn emit_text_length(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    scratch: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64ExtendI32S);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// Wasm frames store the constant flag in pad (offset 12).
pub(in crate::codegen::wasm) fn emit_text_constant(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    scratch: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 12,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `start` is stored at [`FRAME_OFF_START`] (1-based index into the main buffer).
pub(in crate::codegen::wasm) fn emit_text_start(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    _s1: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: FRAME_OFF_START,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64ExtendI32S);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `main` is the whole object this view belongs to: the buffer starts `start-1`
/// bytes before the view and runs for `main_len`.
pub(in crate::codegen::wasm) fn emit_text_main(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // s1 = ptr - (start - 1)
    emit_frame_load(body, s0, FRAME_OFF_PTR);
    emit_frame_load(body, s0, FRAME_OFF_START);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s1));
    emit_frame_load(body, s0, FRAME_OFF_MAIN_LEN);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, s3);
    body.instruction(&Instruction::Else);
    emit_frame_from_buf(body, dest, s1, s2, s3);
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s3));
    emit_frame_copy_field(body, s3, s0, FRAME_OFF_PAD);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_text_pos(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    scratch: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64ExtendI32S);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

pub(in crate::codegen::wasm) fn emit_text_more(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    s1: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // pos
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    // pos <= length
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32LeS);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

pub(in crate::codegen::wasm) fn emit_text_setpos(
    body: &mut Function,
    frame: LocalId,
    index: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // s1 = length
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    // s2 = index as i32
    body.instruction(&Instruction::LocalGet(local_index(index)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s2));
    // if index < 1 || index > length+1 then pos = length+1 else pos = index
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_text_blanks(
    body: &mut Function,
    dest: LocalId,
    n: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(n)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalTee(s0));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, s1);
    body.instruction(&Instruction::Else);
    // s1 = buffer
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    emit_heap_grow_if_needed(body, s1, BumpSize::Dynamic(s0));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    // fill with spaces: s2 = cursor, s3 = remaining
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s3));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    emit_frame_from_buf(body, dest, s1, s0, s2);
    body.instruction(&Instruction::End);
}

/// `left = right` / `left =/= right` (TEXTOBJ view equality) under WasmGC:
/// same underlying `chars` array (`ref.eq`, not deep content — matches the
/// bump path's `ptr`/`len` comparison, which is also identity-of-view rather
/// than a byte-for-byte scan) and the same `start`/`length` window into it.
/// A `t.sub(i, n)` taken twice from the same `t` shares `chars` and computes
/// the same `start`/`length`, so it compares equal here — the WasmGC analogue
/// of bump's TEXTOBJ-share check.
pub(in crate::codegen::wasm) fn emit_text_ref_eq_gc(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s0: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    let get_field = |body: &mut Function, who: LocalId, field: u32| {
        body.instruction(&Instruction::LocalGet(local_index(who)));
        body.instruction(&Instruction::StructGet {
            struct_type_index: frame_ty,
            field_index: field,
        });
    };
    get_field(body, left, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    get_field(body, right, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS);
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::LocalSet(s0));
    get_field(body, left, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START);
    get_field(body, right, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START);
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::LocalSet(s0));
    get_field(body, left, crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH);
    get_field(
        body,
        right,
        crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    );
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_text_ref_eq(
    body: &mut Function,
    dest: LocalId,
    left: LocalId,
    right: LocalId,
    s0: u32,
    s1: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(left)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(local_index(right)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s1));
    // Compare ptr
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Eq);
    // Compare len
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::I32And);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `t.sub(i, n)` under WasmGC: a fresh frame sharing `frame`'s `chars` ref
/// (real TEXTOBJ sharing — no bytes are copied), `start` shifted by `i - 1`,
/// `length = n`, fresh `pos = 1`, and `frame`'s own `constant` flag inherited
/// (mirrors [`emit_text_sub`]'s bump semantics field-for-field, modulo the
/// `chars`-relative-offset representation described on [`emit_text_notext_gc`]'s
/// module docs).
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_text_sub_gc(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    i: LocalId,
    n: LocalId,
    s1: u32,
    s2: u32,
    s3: u32,
    s4: u32,
    frame_scratch: u32,
) -> Result<(), CompileError> {
    let (frame_ty, _) = text_frame_field_types()?;
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::LocalSet(frame_scratch));
    body.instruction(&Instruction::LocalGet(local_index(i)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(local_index(n)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(frame_scratch));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
    });
    body.instruction(&Instruction::LocalSet(s3)); // len
    // abort if i < 0 || n < 0 || i + n > len + 1
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext_gc(body, dest)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(frame_scratch));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CHARS,
    });
    body.instruction(&Instruction::LocalGet(frame_scratch));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
    });
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Add); // new start
    body.instruction(&Instruction::LocalGet(s2)); // length = n
    body.instruction(&Instruction::I32Const(1)); // pos
    body.instruction(&Instruction::LocalGet(frame_scratch));
    body.instruction(&Instruction::StructGet {
        struct_type_index: frame_ty,
        field_index: crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
    });
    body.instruction(&Instruction::StructNew(frame_ty));
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    let _ = s4;
    Ok(())
}

pub(in crate::codegen::wasm) fn emit_text_sub(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    i: LocalId,
    n: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
    s4: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(local_index(i)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s1));
    body.instruction(&Instruction::LocalGet(local_index(n)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s3)); // len
    // abort if i < 0 || n < 0 || i + n > len + 1
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(s3));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, s4);
    body.instruction(&Instruction::Else);
    // new ptr = old_ptr + (i - 1)
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s4));
    emit_frame_from_buf(body, dest, s4, s2, s3);
    // A subtext stays inside the parent's object: same constant flag and
    // main extent, with start shifted by i - 1.
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s4));
    emit_frame_copy_field(body, s4, s0, FRAME_OFF_PAD);
    emit_frame_copy_field(body, s4, s0, FRAME_OFF_MAIN_LEN);
    body.instruction(&Instruction::LocalGet(s4));
    emit_frame_load(body, s0, FRAME_OFF_START);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: FRAME_OFF_START,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_text_strip(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1)); // ptr
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2)); // len
    // while len > 0 && content[len-1] == ' ': len--
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::I32Ne);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, s3);
    body.instruction(&Instruction::Else);
    emit_frame_from_buf(body, dest, s1, s2, s3);
    // strip trims the tail of the same view, so it keeps the parent's object.
    body.instruction(&Instruction::LocalGet(local_index(dest)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s3));
    emit_frame_copy_field(body, s3, s0, FRAME_OFF_PAD);
    emit_frame_copy_field(body, s3, s0, FRAME_OFF_START);
    emit_frame_copy_field(body, s3, s0, FRAME_OFF_MAIN_LEN);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_text_case_fold(
    body: &mut Function,
    frame: LocalId,
    upcase: bool,
    s0: u32,
    s1: u32,
    s2: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // trap on notext (len == 0) or constant (pad != 0)
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 12,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    // pos := 1
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1)); // ptr cursor
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2)); // remaining
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    // store8(addr, fold(load8(addr)))
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    if upcase {
        body.instruction(&Instruction::LocalTee(s0));
        body.instruction(&Instruction::I32Const(b'a' as i32));
        body.instruction(&Instruction::I32GeU);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(b'z' as i32));
        body.instruction(&Instruction::I32LeU);
        body.instruction(&Instruction::I32And);
        body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(0x20));
        body.instruction(&Instruction::I32Xor);
        body.instruction(&Instruction::Else);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::End);
    } else {
        body.instruction(&Instruction::LocalTee(s0));
        body.instruction(&Instruction::I32Const(b'A' as i32));
        body.instruction(&Instruction::I32GeU);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(b'Z' as i32));
        body.instruction(&Instruction::I32LeU);
        body.instruction(&Instruction::I32And);
        body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(0x20));
        body.instruction(&Instruction::I32Or);
        body.instruction(&Instruction::Else);
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::End);
    }
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
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

pub(in crate::codegen::wasm) fn emit_text_getchar(
    body: &mut Function,
    dest: LocalId,
    frame: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // s1 = pos
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    // if pos > length -> unreachable
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    // byte = *(ptr + pos - 1)
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    // pos += 1
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    let _ = s2;
}

pub(in crate::codegen::wasm) fn emit_text_putchar(
    body: &mut Function,
    frame: LocalId,
    ch: LocalId,
    s0: u32,
    s1: u32,
    s2: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(frame)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(s0));
    // trap on constant (pad != 0)
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 12,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    // s1 = pos
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s1));
    // if pos > length or length == 0 -> unreachable
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s2));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::LocalGet(s2));
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    // *(ptr + pos - 1) = ch
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(local_index(ch)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    // pos += 1
    body.instruction(&Instruction::LocalGet(s0));
    body.instruction(&Instruction::LocalGet(s1));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 8,
        align: 2,
        memory_index: 0,
    }));
}
