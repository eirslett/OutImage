//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// Append the bytes an iovec points at to the SysOut image buffer, mirroring
/// `simrt_image_out_text`: nothing reaches stdout until `OutImage` /
/// `BreakOutImage` flushes the image.
pub(in crate::codegen::wasm) fn emit_sysout_write_iov(
    body: &mut Function,
    iov_ptr: u32,
    sysout_write: u32,
) {
    body.instruction(&Instruction::I32Const(iov_ptr as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(iov_ptr as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Call(sysout_write));
}

/// `OutImage` (`break_only = false`) / `BreakOutImage` (`break_only = true`).
pub(in crate::codegen::wasm) fn emit_sysout_flush(
    body: &mut Function,
    break_only: bool,
    sysout_flush: u32,
) {
    body.instruction(&Instruction::I32Const(i32::from(break_only)));
    body.instruction(&Instruction::Call(sysout_flush));
}

/// Free `OutChar`: append one ASCII/low byte to the SysOut image.
pub(in crate::codegen::wasm) fn emit_out_char(
    body: &mut Function,
    ch: LocalId,
    buf: u32,
    scratch: u32,
    sysout_write: u32,
) {
    emit_bump_alloc(body, 4, buf);
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(local_index(ch)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    let _ = scratch;
    emit_sysout_write_iov(body, SCRATCH_IOV, sysout_write);
}

/// Loads one of the fixed low-memory i32 cells (image base / object pointer).
pub(in crate::codegen::wasm) fn emit_load_cell(body: &mut Function, addr: u32) {
    body.instruction(&Instruction::I32Const(addr as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
}

pub(in crate::codegen::wasm) fn emit_load_sysin_base(body: &mut Function, dest: u32) {
    emit_load_cell(body, SYSIN_BASE_PTR);
    body.instruction(&Instruction::LocalSet(dest));
}

/// Free `InImage`: read one stdin record into the SysIn image.
pub(in crate::codegen::wasm) fn emit_in_image(
    body: &mut Function,
    base: u32,
    len: u32,
    buf: u32,
    scratch: u32,
) {
    emit_load_sysin_base(body, base);
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(buf));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(len));
    emit_image_store_const(body, base, IMAGE_OFF_FLAG, 0);
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(IMAGE_BUF_SIZE as i32));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_host_read(body);
    body.instruction(&Instruction::I32Const(NREAD_PTR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalTee(scratch));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_image_store_const(body, base, IMAGE_OFF_FLAG, 1);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Br(2));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Const(b'\n' as i32));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Br(2));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Const(b'\r' as i32));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Br(2));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    emit_image_store_local(body, base, IMAGE_OFF_LEN, len);
    emit_image_store_local(body, base, IMAGE_OFF_MAIN_LEN, len);
    emit_image_store_const(body, base, IMAGE_OFF_POS, 1);
}

/// Free `InChar`: next SysIn image byte as an i64 codepoint. §10.4.3's
/// `if not more then inimage` refills the image once it is exhausted.
pub(in crate::codegen::wasm) fn emit_in_char(
    body: &mut Function,
    dest: LocalId,
    base: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    emit_load_sysin_base(body, base);
    emit_image_load(body, base, IMAGE_OFF_POS);
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_in_image(body, base, s1, s2, s3);
    body.instruction(&Instruction::End);
    emit_load_sysin_base(body, base);
    // buf[pos - 1]
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s1));
    emit_image_store_local(body, base, IMAGE_OFF_POS, s1);
}

pub(in crate::codegen::wasm) fn emit_endfile(body: &mut Function, dest: LocalId, base: u32) {
    emit_load_sysin_base(body, base);
    emit_image_load(body, base, IMAGE_OFF_FLAG);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `scratch` = host identity for `object` as i32 (linear ptr, or BASICIO id).
pub(in crate::codegen::wasm) fn emit_object_i32(
    body: &mut Function,
    object: LocalId,
    scratch: u32,
) {
    if gc_objects_enabled() {
        emit_object_host_handle_i32(body, object, scratch);
        return;
    }
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(scratch));
}

/// Pin `object` in the BASICIO table (or find its existing slot) and leave that
/// slot index — its host identity — in `scratch` as i32. Terminals answer with
/// their fixed ids straight from the reference globals, so a `sysout.OutText`
/// never touches a table at all.
pub(in crate::codegen::wasm) fn emit_object_host_handle_i32(
    body: &mut Function,
    object: LocalId,
    scratch: u32,
) {
    // Terminals first (fixed host ids; no pin needed, the globals root them).
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::GlobalGet(GLOBAL_SYSIN));
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(BASICIO_HOST_ID_SYSIN));
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::GlobalGet(GLOBAL_SYSOUT));
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(BASICIO_HOST_ID_SYSOUT));
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::Else);
    // Scan existing slots, else grow.
    body.instruction(&Instruction::I32Const(BASICIO_HOST_ID_FIRST_DISK));
    body.instruction(&Instruction::LocalSet(scratch)); // reuse as cursor
    body.instruction(&Instruction::Block(BlockType::Empty)); // done
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::TableSize(BASICIO_HANDLE_TABLE));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::If(BlockType::Empty));
    // not found → grow
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::TableGrow(BASICIO_HANDLE_TABLE));
    body.instruction(&Instruction::LocalTee(scratch));
    body.instruction(&Instruction::I32Const(-1));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    // `table.grow` already filled the new slot with `object` as its init
    // value, and `scratch` holds that slot's index (the previous size) —
    // just exit to `done`. NB: `Br(1)` here would only target the enclosing
    // `loop` (i.e. restart it), not exit the outer `block`; branching to a
    // `loop` label re-enters the loop rather than leaving it, so this must
    // be `Br(2)` to actually reach `done`.
    body.instruction(&Instruction::Br(2)); // exit done
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::TableGet(BASICIO_HANDLE_TABLE));
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Br(2)); // found; scratch already holds index; exit done (see note above)
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End); // loop
    body.instruction(&Instruction::End); // done
    body.instruction(&Instruction::End); // else sysout
    body.instruction(&Instruction::End); // else sysin
}

/// Push host `i64` identity for `object` (BASICIO id under GC, linear ptr otherwise).
pub(in crate::codegen::wasm) fn emit_object_host_i64(
    body: &mut Function,
    object: LocalId,
    scratch: u32,
) {
    if gc_objects_enabled() {
        emit_object_host_handle_i32(body, object, scratch);
        body.instruction(&Instruction::LocalGet(scratch));
        body.instruction(&Instruction::I64ExtendI32U);
    } else {
        body.instruction(&Instruction::LocalGet(local_index(object)));
    }
}

/// `flag` = 1 when `object` is `sysin` or `sysout`.
pub(in crate::codegen::wasm) fn emit_is_terminal_flag(
    body: &mut Function,
    object: LocalId,
    obj_i32: u32,
    flag: u32,
) {
    if gc_objects_enabled() {
        emit_is_terminal_flag_gc(body, object, flag);
        return;
    }
    body.instruction(&Instruction::LocalGet(obj_i32));
    emit_load_cell(body, SYSIN_OBJ_PTR);
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::LocalGet(obj_i32));
    emit_load_cell(body, SYSOUT_OBJ_PTR);
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::LocalSet(flag));
}

/// `ref.eq` against the terminal globals — object identity, not a host id, so
/// the answer stays right however the receiver reached this call.
pub(in crate::codegen::wasm) fn emit_is_terminal_flag_gc(
    body: &mut Function,
    object: LocalId,
    flag: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::GlobalGet(GLOBAL_SYSIN));
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::GlobalGet(GLOBAL_SYSOUT));
    body.instruction(&Instruction::RefEq);
    body.instruction(&Instruction::I32Or);
    body.instruction(&Instruction::LocalSet(flag));
}

/// Host `i64` truthy (1/0) → Simula bool local.
pub(in crate::codegen::wasm) fn emit_host_i64_to_bool(body: &mut Function, dest: LocalId) {
    body.instruction(&Instruction::I64Eqz);
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

pub(in crate::codegen::wasm) fn emit_disk_out_text_local(
    body: &mut Function,
    object_host_i32: u32,
    src: LocalId,
    frame: u32,
    ptr_scratch: u32,
    basicio_out_text: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(src)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(frame));
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(ptr_scratch));
    body.instruction(&Instruction::LocalGet(object_host_i32));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalGet(ptr_scratch));
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Call(basicio_out_text));
}

/// `flag` = 1 when `object` is `sysin` (checked after terminal detection,
/// so a 0 means `sysout`).
pub(in crate::codegen::wasm) fn emit_is_sysin(body: &mut Function, object: LocalId, flag: u32) {
    if gc_objects_enabled() {
        body.instruction(&Instruction::LocalGet(local_index(object)));
        body.instruction(&Instruction::GlobalGet(GLOBAL_SYSIN));
        body.instruction(&Instruction::RefEq);
        body.instruction(&Instruction::LocalSet(flag));
        return;
    }
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32WrapI64);
    emit_load_cell(body, SYSIN_OBJ_PTR);
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::LocalSet(flag));
}

/// `base` = the image header address backing `object`; traps for non-terminals.
pub(in crate::codegen::wasm) fn emit_terminal_image_base(
    body: &mut Function,
    object: LocalId,
    base: u32,
) {
    if gc_objects_enabled() {
        body.instruction(&Instruction::LocalGet(local_index(object)));
        body.instruction(&Instruction::GlobalGet(GLOBAL_SYSIN));
        body.instruction(&Instruction::RefEq);
        body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        emit_load_cell(body, SYSIN_BASE_PTR);
        body.instruction(&Instruction::Else);
        emit_load_cell(body, SYSOUT_BASE_PTR);
        body.instruction(&Instruction::End);
        body.instruction(&Instruction::LocalSet(base));
        return;
    }
    body.instruction(&Instruction::LocalGet(local_index(object)));
    body.instruction(&Instruction::I32WrapI64);
    emit_load_cell(body, SYSIN_OBJ_PTR);
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_cell(body, SYSIN_BASE_PTR);
    body.instruction(&Instruction::Else);
    emit_load_cell(body, SYSOUT_BASE_PTR);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(base));
}

/// Pushes `image.<offset>` for an image header address held in `base`.
pub(in crate::codegen::wasm) fn emit_image_load(body: &mut Function, base: u32, offset: u64) {
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

pub(in crate::codegen::wasm) fn emit_image_store_local(
    body: &mut Function,
    base: u32,
    offset: u64,
    value: u32,
) {
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::LocalGet(value));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

pub(in crate::codegen::wasm) fn emit_image_store_const(
    body: &mut Function,
    base: u32,
    offset: u64,
    value: i32,
) {
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(value));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}

/// A text frame viewing `[from, image.len)` of an image buffer, so the host's
/// `text_get*` (which always parses from the start of the view) resumes where
/// the file's position left off.
pub(in crate::codegen::wasm) fn emit_image_view_frame(
    body: &mut Function,
    base: u32,
    from: u32,
    frame: u32,
    tmp: u32,
) {
    // tmp = max(len - from, 0)
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::LocalGet(from));
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::LocalGet(from));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(tmp));
    emit_bump_alloc(body, FRAME_SIZE, frame);
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(from));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: FRAME_OFF_PTR,
        align: 2,
        memory_index: 0,
    }));
    emit_frame_store_local(body, frame, FRAME_OFF_LEN, tmp);
    emit_frame_store_const(body, frame, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, frame, FRAME_OFF_PAD, 0);
    emit_frame_store_const(body, frame, FRAME_OFF_START, 1);
    emit_frame_store_local(body, frame, FRAME_OFF_MAIN_LEN, tmp);
}

/// Item-oriented input (§10.4): skip blanks, pulling in fresh records while the
/// image is exhausted, and stop at end of file. Mirrors `basicio_skip_spaces`.
pub(in crate::codegen::wasm) fn emit_sysin_skip_blanks(
    body: &mut Function,
    base: u32,
    s0: u32,
    s1: u32,
    s2: u32,
) {
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    emit_load_sysin_base(body, base);
    emit_image_load(body, base, IMAGE_OFF_FLAG);
    body.instruction(&Instruction::BrIf(1)); // endfile
    // pos > len → read the next record and retry.
    emit_image_load(body, base, IMAGE_OFF_POS);
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_in_image(body, base, s0, s1, s2);
    body.instruction(&Instruction::Else);
    // ch = buf[pos - 1]
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(s0));
    // A record separator counts as a blank here: stdin arrives as a block, so
    // one image can span several external records.
    body.instruction(&Instruction::I32Const(0));
    for byte in *b" \t\n\r" {
        body.instruction(&Instruction::LocalGet(s0));
        body.instruction(&Instruction::I32Const(byte as i32));
        body.instruction(&Instruction::I32Eq);
        body.instruction(&Instruction::I32Or);
    }
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(2)); // non-blank item start
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s0));
    emit_image_store_local(body, base, IMAGE_OFF_POS, s0);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End); // loop
    body.instruction(&Instruction::End); // block
}

/// Shared prologue for `inint` / `inreal` / `infrac`: skip blanks, trap at end
/// of file, then build a view frame in `frame` starting at the current
/// position. `base` keeps the SysIn image address for the write-back.
pub(in crate::codegen::wasm) fn emit_sysin_item_frame(
    body: &mut Function,
    base: u32,
    frame: u32,
    s1: u32,
    s2: u32,
    s3: u32,
) {
    emit_sysin_skip_blanks(body, base, s1, s2, s3);
    emit_load_sysin_base(body, base);
    emit_image_load(body, base, IMAGE_OFF_FLAG);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(s1));
    emit_image_view_frame(body, base, s1, frame, s2);
}

/// BASICIO `setpos(i)` (§10.3): clamped into `[1, length + 1]`, like
/// `text.setpos` against the image.
pub(in crate::codegen::wasm) fn emit_basicio_setpos(
    body: &mut Function,
    index: LocalId,
    base: u32,
    tmp: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(index)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(tmp));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::LocalSet(tmp));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(tmp));
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(tmp));
    body.instruction(&Instruction::End);
    emit_image_store_local(body, base, IMAGE_OFF_POS, tmp);
}

/// BASICIO `image :- t`: the image takes the source's content and length
/// (capped at the buffer), and the position rewinds to 1.
pub(in crate::codegen::wasm) fn emit_basicio_set_image(
    body: &mut Function,
    text: LocalId,
    base: u32,
    frame: u32,
    len: u32,
    src: u32,
    dst: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(text)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(frame));
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_frame_load(body, frame, FRAME_OFF_LEN);
    body.instruction(&Instruction::I32Const(IMAGE_BUF_SIZE as i32));
    emit_frame_load(body, frame, FRAME_OFF_LEN);
    body.instruction(&Instruction::I32Const(IMAGE_BUF_SIZE as i32));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::Select);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_frame_load(body, frame, FRAME_OFF_PTR);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalSet(src));
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(dst));
    emit_image_store_local(body, base, FRAME_OFF_PTR, dst);
    emit_image_store_local(body, base, IMAGE_OFF_LEN, len);
    emit_image_store_local(body, base, IMAGE_OFF_MAIN_LEN, len);
    emit_image_store_const(body, base, IMAGE_OFF_POS, 1);
    // `emit_memcpy` consumes its cursors, so it has to come last.
    emit_memcpy(body, dst, src, len);
}

/// BASICIO `image :- t` under WasmGC: mirror of [`emit_basicio_set_image`]
/// with `text`'s content reaching linear memory via
/// [`emit_text_to_linear_scratch_gc`] (the same host-IO bridge `OutText`
/// uses) instead of `I32WrapI64` on a bump frame pointer.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_basicio_set_image_gc(
    body: &mut Function,
    text: LocalId,
    base: u32,
    content_ptr: u32,
    loop_idx: u32,
    content_len: u32,
    start_field: u32,
    chars_scratch: u32,
    len: u32,
    dst: u32,
) -> Result<(), CompileError> {
    emit_text_to_linear_scratch_gc(
        body,
        text,
        content_ptr,
        loop_idx,
        content_len,
        start_field,
        chars_scratch,
    )?;
    body.instruction(&Instruction::LocalGet(content_len));
    body.instruction(&Instruction::I32Const(IMAGE_BUF_SIZE as i32));
    body.instruction(&Instruction::LocalGet(content_len));
    body.instruction(&Instruction::I32Const(IMAGE_BUF_SIZE as i32));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::Select);
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(dst));
    emit_image_store_local(body, base, FRAME_OFF_PTR, dst);
    emit_image_store_local(body, base, IMAGE_OFF_LEN, len);
    emit_image_store_local(body, base, IMAGE_OFF_MAIN_LEN, len);
    emit_image_store_const(body, base, IMAGE_OFF_POS, 1);
    // `emit_memcpy` consumes its cursors, so it has to come last; `dst` was
    // already captured above, so recomputing it into a throwaway is fine.
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(dst));
    emit_memcpy(body, dst, content_ptr, len);
    Ok(())
}

/// BASICIO `intext(w)` (§10.4): the next `w` characters as a fresh text, with
/// `inimage` pulling further records as needed.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_basicio_intext(
    body: &mut Function,
    dest: LocalId,
    width: LocalId,
    base: u32,
    w: u32,
    buf: u32,
    i: u32,
    s4: u32,
    s5: u32,
    s6: u32,
) {
    body.instruction(&Instruction::LocalGet(local_index(width)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(w));
    body.instruction(&Instruction::LocalGet(w));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext(body, dest, buf);
    body.instruction(&Instruction::Else);
    // Variable-size bump allocation for the `w` characters.
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(buf));
    emit_heap_grow_if_needed(body, buf, BumpSize::Dynamic(w));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(w));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(i));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(i));
    body.instruction(&Instruction::LocalGet(w));
    body.instruction(&Instruction::I32GeS);
    body.instruction(&Instruction::BrIf(1));
    emit_load_sysin_base(body, base);
    emit_image_load(body, base, IMAGE_OFF_POS);
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_in_image(body, base, s4, s5, s6);
    body.instruction(&Instruction::End);
    emit_load_sysin_base(body, base);
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(i));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
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
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s4));
    emit_image_store_local(body, base, IMAGE_OFF_POS, s4);
    body.instruction(&Instruction::LocalGet(i));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(i));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    emit_bump_alloc(body, FRAME_SIZE, s4);
    emit_frame_store_local(body, s4, FRAME_OFF_PTR, buf);
    emit_frame_store_local(body, s4, FRAME_OFF_LEN, w);
    emit_frame_store_const(body, s4, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, s4, FRAME_OFF_PAD, 0);
    emit_frame_store_const(body, s4, FRAME_OFF_START, 1);
    emit_frame_store_local(body, s4, FRAME_OFF_MAIN_LEN, w);
    body.instruction(&Instruction::LocalGet(s4));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
}

/// `CallBasicioInText`'s terminal (`sysin`) branch under WasmGC: identical
/// image-buffer walk as [`emit_basicio_intext`], but the collected `w`
/// characters are wrapped in a WasmGC `text_frame` via
/// [`emit_push_text_frame_from_linear_bytes`] instead of a bump `FRAME`
/// struct. `s4` doubles as the byte-copy-loop index afterward (its earlier
/// per-character use is done by then).
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_basicio_intext_gc(
    body: &mut Function,
    dest: LocalId,
    width: LocalId,
    base: u32,
    w: u32,
    buf: u32,
    i: u32,
    s4: u32,
    s5: u32,
    s6: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    body.instruction(&Instruction::LocalGet(local_index(width)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(w));
    body.instruction(&Instruction::LocalGet(w));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32LtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_text_notext_gc(body, dest)?;
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(buf));
    emit_heap_grow_if_needed(body, buf, BumpSize::Dynamic(w));
    body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(w));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(i));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(i));
    body.instruction(&Instruction::LocalGet(w));
    body.instruction(&Instruction::I32GeS);
    body.instruction(&Instruction::BrIf(1));
    emit_load_sysin_base(body, base);
    emit_image_load(body, base, IMAGE_OFF_POS);
    emit_image_load(body, base, IMAGE_OFF_LEN);
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    emit_in_image(body, base, s4, s5, s6);
    body.instruction(&Instruction::End);
    emit_load_sysin_base(body, base);
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(i));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
    body.instruction(&Instruction::I32Add);
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
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
    emit_image_load(body, base, IMAGE_OFF_POS);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(s4));
    emit_image_store_local(body, base, IMAGE_OFF_POS, s4);
    body.instruction(&Instruction::LocalGet(i));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(i));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    emit_push_text_frame_from_linear_bytes(body, buf, w, s4, ch0)?;
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    body.instruction(&Instruction::End);
    Ok(())
}

/// After a `text_get*` call: `sysin.pos += frame.pos - 1`.
pub(in crate::codegen::wasm) fn emit_sysin_item_advance(
    body: &mut Function,
    base: u32,
    frame: u32,
    tmp: u32,
) {
    emit_image_load(body, base, IMAGE_OFF_POS);
    emit_frame_load(body, frame, FRAME_OFF_POS);
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(tmp));
    emit_image_store_local(body, base, IMAGE_OFF_POS, tmp);
}

/// `fd_read(fd=0, iovs=READ_IOV, iovs_len=1, nread=NREAD_PTR)`, dropping the
/// `errno` result (host func index 1; see `MIR_FUNC_BASE`'s comment).
pub(in crate::codegen::wasm) fn emit_host_read(body: &mut Function) {
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Const(NREAD_PTR as i32));
    body.instruction(&Instruction::Call(1));
    body.instruction(&Instruction::Drop);
}

/// Truncate `len` to exclude the first `\n` or `\r` in `buf[0..len)`.
pub(in crate::codegen::wasm) fn emit_truncate_at_newline(
    body: &mut Function,
    buf: u32,
    len: u32,
    index: u32,
    ch: u32,
) {
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::LocalSet(index));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32GeU);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalTee(ch));
    body.instruction(&Instruction::I32Const(b'\n' as i32));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::Br(2));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(ch));
    body.instruction(&Instruction::I32Const(b'\r' as i32));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::Br(2));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::LocalGet(index));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(index));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// If the byte at `base[len - 1]` equals `byte` (and `len > 0`), decrement
/// `len` (a local var index, mutated in place) by one. Used to strip a
/// trailing `\n`/`\r` from a `CallInLine` read.
pub(in crate::codegen::wasm) fn emit_strip_trailing_byte(
    body: &mut Function,
    base: u32,
    len: u32,
    scratch: u32,
    byte: i32,
) {
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(0));
    body.instruction(&Instruction::I32GtS);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(base));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(scratch));
    body.instruction(&Instruction::LocalGet(scratch));
    body.instruction(&Instruction::I32Load8U(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(byte));
    body.instruction(&Instruction::I32Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(len));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(len));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
}

/// `CallInLine`: reads up to `READ_BUF_SIZE` bytes from stdin into a fresh
/// bump-allocated buffer, strips a trailing `\n` (and a preceding `\r`), and
/// wraps the result in a fresh mutable text frame stored in `dest`.
///
/// `buf`/`len`/`frame` are scratch local var indices (i32); `buf` doubles as
/// the frame's content pointer and `frame` doubles as the byte-address
/// scratch for [`emit_strip_trailing_byte`].
pub(in crate::codegen::wasm) fn emit_in_line(
    body: &mut Function,
    dest: LocalId,
    buf: u32,
    len: u32,
    frame: u32,
) {
    // buf = bump_alloc(READ_BUF_SIZE); iovec = {ptr: buf, len: READ_BUF_SIZE}
    emit_bump_alloc(body, READ_BUF_SIZE, buf);
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::I32Const(READ_BUF_SIZE));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_host_read(body);

    // len = nread (from NREAD_PTR); clamp to >= 0 defensively.
    body.instruction(&Instruction::I32Const(NREAD_PTR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(len));

    emit_truncate_at_newline(body, buf, len, frame, frame);

    emit_strip_trailing_byte(body, buf, len, frame, 10); // '\n'
    emit_strip_trailing_byte(body, buf, len, frame, 13); // '\r'

    // The line buffer is its own main object.
    emit_bump_alloc(body, FRAME_SIZE, frame);
    emit_frame_store_local(body, frame, FRAME_OFF_PTR, buf);
    emit_frame_store_local(body, frame, FRAME_OFF_LEN, len);
    emit_frame_store_const(body, frame, FRAME_OFF_POS, 1);
    emit_frame_store_const(body, frame, FRAME_OFF_PAD, 0);
    emit_frame_store_const(body, frame, FRAME_OFF_START, 1);
    emit_frame_store_local(body, frame, FRAME_OFF_MAIN_LEN, len);
    body.instruction(&Instruction::LocalGet(frame));
    body.instruction(&Instruction::I64ExtendI32U);
    body.instruction(&Instruction::LocalSet(local_index(dest)));
}

/// `CallInLine` under WasmGC: identical stdin read / newline-stripping as
/// [`emit_in_line`], but the final line is wrapped in a WasmGC `text_frame`
/// via [`emit_push_text_frame_from_linear_bytes`] instead of a bump `FRAME`
/// struct. `tmp` is reused as the byte-scratch for
/// [`emit_strip_trailing_byte`] and then as the copy-loop index.
pub(in crate::codegen::wasm) fn emit_in_line_gc(
    body: &mut Function,
    dest: LocalId,
    buf: u32,
    len: u32,
    tmp: u32,
    ch0: u32,
) -> Result<(), CompileError> {
    emit_bump_alloc(body, READ_BUF_SIZE, buf);
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(READ_IOV as i32));
    body.instruction(&Instruction::I32Const(READ_BUF_SIZE));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_host_read(body);

    body.instruction(&Instruction::I32Const(NREAD_PTR as i32));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(len));

    emit_truncate_at_newline(body, buf, len, tmp, tmp);
    emit_strip_trailing_byte(body, buf, len, tmp, 10); // '\n'
    emit_strip_trailing_byte(body, buf, len, tmp, 13); // '\r'

    emit_push_text_frame_from_linear_bytes(body, buf, len, tmp, ch0)?;
    body.instruction(&Instruction::LocalSet(local_index(dest)));
    Ok(())
}
