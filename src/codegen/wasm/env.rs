//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// ENVIRONMENT helpers: portable subset inline; others reject until host imports land.
#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::wasm) fn emit_call_env(
    body: &mut Function,
    function: &MirFunction,
    host: HostImports,
    dest: LocalId,
    name: &str,
    args: &[LocalId],
    scratch0: u32,
    scratch1: u32,
    scratch2: u32,
    scratch3: u32,
    scratch4: u32,
    scratch5: u32,
    chars_scratch: u32,
) -> Result<bool, CompileError> {
    match name {
        "abs_int" if args.len() == 1 => {
            // dest = src < 0 ? -src : src
            let src = args[0];
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::I64LtS);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Sub);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "abs_real" if args.len() == 1 => {
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::F64Abs);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "sign" if args.len() == 1 => {
            let src = args[0];
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
            body.instruction(&Instruction::F64Gt);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::I64Const(1));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
            body.instruction(&Instruction::F64Lt);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::I64Const(-1));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "mod" if args.len() == 2 => {
            // Simula mathematical modulo: rem adjusted so sign matches divisor.
            let left = args[0];
            let right = args[1];
            body.instruction(&Instruction::LocalGet(local_index(left)));
            body.instruction(&Instruction::LocalGet(local_index(left)));
            body.instruction(&Instruction::LocalGet(local_index(right)));
            body.instruction(&Instruction::I64DivS);
            body.instruction(&Instruction::LocalGet(local_index(right)));
            body.instruction(&Instruction::I64Mul);
            body.instruction(&Instruction::I64Sub);
            // rem in tmp via tee into dest, then adjust
            body.instruction(&Instruction::LocalTee(local_index(dest)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::I64Eq);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::Else);
            // if sign(rem) != sign(j) then rem + j else rem
            body.instruction(&Instruction::LocalGet(local_index(dest)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::I64LtS);
            body.instruction(&Instruction::LocalGet(local_index(right)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::I64LtS);
            body.instruction(&Instruction::I32Ne);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::LocalGet(local_index(dest)));
            body.instruction(&Instruction::LocalGet(local_index(right)));
            body.instruction(&Instruction::I64Add);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(dest)));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "rem" if args.len() == 2 => {
            let left = args[0];
            let right = args[1];
            body.instruction(&Instruction::LocalGet(local_index(left)));
            body.instruction(&Instruction::LocalGet(local_index(left)));
            body.instruction(&Instruction::LocalGet(local_index(right)));
            body.instruction(&Instruction::I64DivS);
            body.instruction(&Instruction::LocalGet(local_index(right)));
            body.instruction(&Instruction::I64Mul);
            body.instruction(&Instruction::I64Sub);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "sqrt" if args.len() == 1 => {
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::F64Sqrt);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "digit" if args.len() == 1 => {
            // '0'..='9'
            let src = args[0];
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(b'0' as i64));
            body.instruction(&Instruction::I64GeS);
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(b'9' as i64));
            body.instruction(&Instruction::I64LeS);
            body.instruction(&Instruction::I32And);
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "letter" if args.len() == 1 => {
            let src = args[0];
            // ('A'..='Z') || ('a'..='z')
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(b'A' as i64));
            body.instruction(&Instruction::I64GeS);
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(b'Z' as i64));
            body.instruction(&Instruction::I64LeS);
            body.instruction(&Instruction::I32And);
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(b'a' as i64));
            body.instruction(&Instruction::I64GeS);
            body.instruction(&Instruction::LocalGet(local_index(src)));
            body.instruction(&Instruction::I64Const(b'z' as i64));
            body.instruction(&Instruction::I64LeS);
            body.instruction(&Instruction::I32And);
            body.instruction(&Instruction::I32Or);
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "char" | "isochar" | "rank" | "isorank" if args.len() == 1 => {
            // Identity on the codepoint/rank integer (range checks are native-only).
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "max_int" if args.len() == 2 => {
            let a = args[0];
            let b = args[1];
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::I64GeS);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "min_int" if args.len() == 2 => {
            let a = args[0];
            let b = args[1];
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::I64LeS);
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "max_real" if args.len() == 2 => {
            let a = args[0];
            let b = args[1];
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::F64Ge);
            body.instruction(&Instruction::If(BlockType::Result(ValType::F64)));
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "min_real" if args.len() == 2 => {
            let a = args[0];
            let b = args[1];
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::F64Le);
            body.instruction(&Instruction::If(BlockType::Result(ValType::F64)));
            body.instruction(&Instruction::LocalGet(local_index(a)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(local_index(b)));
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "ln" | "exp" | "sin" | "cos" | "arctan" | "addepsilon" | "subepsilon"
            if args.len() == 1 =>
        {
            let index = match name {
                "ln" => host.ln,
                "exp" => host.exp,
                "sin" => host.sin,
                "cos" => host.cos,
                "arctan" => host.arctan,
                "addepsilon" => host.addepsilon,
                "subepsilon" => host.subepsilon,
                _ => unreachable!(),
            };
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::Call(index));
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "randint" if args.len() == 3 => {
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::LocalGet(local_index(args[1])));
            body.instruction(&Instruction::LocalGet(local_index(args[2])));
            body.instruction(&Instruction::Call(host.randint));
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "uniform" | "normal" if args.len() == 3 => {
            let index = if name == "uniform" {
                host.uniform
            } else {
                host.normal
            };
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::LocalGet(local_index(args[1])));
            body.instruction(&Instruction::LocalGet(local_index(args[2])));
            body.instruction(&Instruction::Call(index));
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "negexp" | "draw" if args.len() == 2 => {
            let index = if name == "negexp" {
                host.negexp
            } else {
                host.draw
            };
            body.instruction(&Instruction::LocalGet(local_index(args[0])));
            body.instruction(&Instruction::LocalGet(local_index(args[1])));
            body.instruction(&Instruction::Call(index));
            body.instruction(&Instruction::LocalSet(local_index(dest)));
            Ok(false)
        }
        "error" if args.len() == 1 => {
            if gc_objects_enabled() {
                // `args[0]` is a WasmGC `text_frame` ref; the host reads its
                // full content for the diagnostic message, read-only.
                emit_text_prepare_host_frame_gc(
                    body,
                    args[0],
                    scratch0,
                    scratch1,
                    scratch2,
                    scratch3,
                    scratch4,
                    scratch5,
                    chars_scratch,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
            } else {
                body.instruction(&Instruction::LocalGet(local_index(args[0])));
                body.instruction(&Instruction::I32WrapI64);
            }
            body.instruction(&Instruction::Call(host.error));
            body.instruction(&Instruction::Unreachable);
            Ok(true)
        }
        other => {
            let _ = function;
            Err(CompileError::codegen(format!(
                "MIR wasm: ENVIRONMENT helper '{other}' is not supported yet (native only)"
            )))
        }
    }
}

/// Formats `value` (i64) as decimal ASCII with Standard `outint(i,w)` field
/// width, then writes through the host `fd_write` path.
///
/// Layout of the bump slab: `[mag:i64][digit/pad bytes …]`.
pub(in crate::codegen::wasm) fn emit_out_int(
    body: &mut Function,
    value: LocalId,
    width: LocalId,
    buf: u32,
    cursor: u32,
    end: u32,
    tmp: u32,
    neg: u32,
    sysout_write: u32,
) {
    const SLAB: i32 = 96; // mag spill + room for padded field
    const DIGIT_AREA: i32 = 88;
    emit_bump_alloc(body, SLAB, buf);
    // end/cursor point into the digit area (buf+8 .. buf+96)
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Const(8 + DIGIT_AREA));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalTee(end));
    body.instruction(&Instruction::LocalSet(cursor));

    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::I64Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalTee(cursor));
    body.instruction(&Instruction::I32Const(b'0' as i32));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Else);

    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::I64Const(i64::MIN));
    body.instruction(&Instruction::I64Eq);
    body.instruction(&Instruction::If(BlockType::Empty));
    {
        let lit = b"-9223372036854775808";
        for (i, &byte) in lit.iter().enumerate() {
            body.instruction(&Instruction::LocalGet(buf));
            body.instruction(&Instruction::I32Const(8));
            body.instruction(&Instruction::I32Add);
            body.instruction(&Instruction::I32Const(byte as i32));
            body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
                offset: i as u64,
                align: 0,
                memory_index: 0,
            }));
        }
        body.instruction(&Instruction::LocalGet(buf));
        body.instruction(&Instruction::I32Const(8));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(cursor));
        body.instruction(&Instruction::LocalGet(cursor));
        body.instruction(&Instruction::I32Const(lit.len() as i32));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(end));
    }
    body.instruction(&Instruction::Else);

    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::I64LtS);
    body.instruction(&Instruction::LocalSet(neg));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(neg));
    body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::I64Sub);
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(value)));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));

    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64Const(10));
    body.instruction(&Instruction::I64RemU);
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(tmp));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I64Const(10));
    body.instruction(&Instruction::I64DivU);
    body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalTee(cursor));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Const(b'0' as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::LocalGet(neg));
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalTee(cursor));
    body.instruction(&Instruction::I32Const(b'-' as i32));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::End);

    body.instruction(&Instruction::End); // else MIN
    body.instruction(&Instruction::End); // else zero

    // tmp = dig_len = end - cursor
    body.instruction(&Instruction::LocalGet(end));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalSet(tmp));

    // Width handling (§10.5.8): w==0 exact; otherwise pad to |w|.
    // Spill dig_start into the mag slot (buf+0 as i32) — magnitude no longer needed.
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    body.instruction(&Instruction::LocalGet(local_index(width)));
    body.instruction(&Instruction::I64Eqz);
    body.instruction(&Instruction::If(BlockType::Empty));
    // w == 0: write digits as-is
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_sysout_write_iov(body, SCRATCH_IOV, sysout_write);
    body.instruction(&Instruction::Else);

    // neg := (w < 0)
    body.instruction(&Instruction::LocalGet(local_index(width)));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::I64LtS);
    body.instruction(&Instruction::LocalSet(neg));

    // end := abs(w) as i32
    body.instruction(&Instruction::LocalGet(neg));
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(local_index(width)));
    body.instruction(&Instruction::I64Const(0));
    body.instruction(&Instruction::I64Sub);
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(end));
    body.instruction(&Instruction::Else);
    body.instruction(&Instruction::LocalGet(local_index(width)));
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::LocalSet(end));
    body.instruction(&Instruction::End);

    // trap if dig_len > abs(w)
    body.instruction(&Instruction::LocalGet(end));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32LtU);
    body.instruction(&Instruction::If(BlockType::Empty));
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);

    // pad = abs(w) - dig_len, spilled at buf+4
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(end));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));

    body.instruction(&Instruction::LocalGet(neg));
    body.instruction(&Instruction::If(BlockType::Empty));
    // Left-adjust: fill spaces at dig_start + dig_len
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::LocalGet(tmp));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(cursor));
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
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
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    // Restore dig_start for write
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalSet(cursor));
    body.instruction(&Instruction::Else);
    // Right-adjust: prepend spaces
    body.instruction(&Instruction::Block(BlockType::Empty));
    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Eqz);
    body.instruction(&Instruction::BrIf(1));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::LocalTee(cursor));
    body.instruction(&Instruction::I32Const(b' ' as i32));
    body.instruction(&Instruction::I32Store8(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::LocalGet(buf));
    body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(1));
    body.instruction(&Instruction::I32Sub);
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::Br(0));
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End);
    body.instruction(&Instruction::End); // left vs right

    // Write field of length abs(w) starting at cursor
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(cursor));
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    body.instruction(&Instruction::I32Const(SCRATCH_IOV as i32));
    body.instruction(&Instruction::LocalGet(end)); // abs(w)
    body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    emit_sysout_write_iov(body, SCRATCH_IOV, sysout_write);
    body.instruction(&Instruction::End); // else w != 0
}
