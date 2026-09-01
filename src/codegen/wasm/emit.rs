//! Submodule of [`crate::codegen::wasm`].

use super::*;

/// Unreachable body for a MIR function the wasm graph never calls.
pub(in crate::codegen::wasm) fn emit_trap_stub() -> Function {
    let mut body = Function::new([]);
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End);
    body
}

/// Emit one MIR function as a PC + `br_table` dispatch over its basic blocks.
pub(in crate::codegen::wasm) fn emit_function_body(
    function: &MirFunction,
    string_count: usize,
    iovecs: &[(u32, u32)],
    func_index: &std::collections::HashMap<String, u32>,
    funcref_slot: &std::collections::HashMap<String, u32>,
    indirect_types: &std::collections::HashMap<CallSigKey, u32>,
    mir_by_name: &std::collections::HashMap<&str, &MirFunction>,
    host: HostImports,
    mut debug_markers: Option<&mut Vec<BodyMarker>>,
) -> Result<Function, CompileError> {
    let mir_local_count = function.params.len() + function.locals.len();
    let pc_local = mir_local_count as u32;
    let scratch0 = pc_local + 1;
    let scratch1 = pc_local + 2;
    let scratch2 = pc_local + 3;
    let scratch3 = pc_local + 4;
    let scratch4 = pc_local + 5;
    let scratch5 = pc_local + 6;
    let scratch6 = pc_local + 7;
    let scratch7 = pc_local + 8;
    let home_base = pc_local + 9;
    // Params are declared by the function type; only non-param locals + PC + scratch here.
    let mut local_decls: Vec<(u32, ValType)> = function
        .locals
        .iter()
        .map(|local| (1, wasm_val_type(local.ty)))
        .collect();
    local_decls.push((1, ValType::I32)); // pc
    local_decls.push((8, ValType::I32)); // array/text/object scratch
    local_decls.push((1, ValType::I32)); // addr-taken home slab base
    let (ref0, ref1, ref2, ref3) = if gc_objects_enabled() {
        let base = home_base + 1;
        local_decls.push((4, crate::codegen::wasm_gc::anyref_val()));
        (base, base + 1, base + 2, base + 3)
    } else {
        (0, 0, 0, 0)
    };
    // Concrete-ref scratches for Text/Array\* WasmGC lowering (Workstream 2):
    // two `text_frame` refs, one `text_chars` ref, one `bounds_array` ref,
    // and one elems ref per numeric element kind.
    let (gtf0, gtf1, gch0, gab0, gae0, gafe0, gatxe0, gaoe0) = if gc_objects_enabled() {
        let ctx_tys = gc_ctx(|ctx| {
            (
                ctx.text_frame_ref(),
                crate::codegen::wasm_gc::concrete_ref_null(ctx.text_chars_ty),
                crate::codegen::wasm_gc::concrete_ref_null(ctx.bounds_array_ty),
                crate::codegen::wasm_gc::concrete_ref_null(ctx.array_i64_elems_ty),
                crate::codegen::wasm_gc::concrete_ref_null(ctx.array_f64_elems_ty),
                crate::codegen::wasm_gc::concrete_ref_null(ctx.array_text_elems_ty),
                crate::codegen::wasm_gc::concrete_ref_null(ctx.array_object_elems_ty),
            )
        })
        .expect("GC_CTX set whenever gc_objects_enabled()");
        let (
            frame_ref,
            chars_ref,
            bounds_ref,
            i64_elems_ref,
            f64_elems_ref,
            text_elems_ref,
            object_elems_ref,
        ) = ctx_tys;
        let base = ref3 + 1;
        local_decls.push((2, frame_ref));
        local_decls.push((1, chars_ref));
        local_decls.push((1, bounds_ref));
        local_decls.push((1, i64_elems_ref));
        local_decls.push((1, f64_elems_ref));
        local_decls.push((1, text_elems_ref));
        local_decls.push((1, object_elems_ref));
        (
            base,
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5,
            base + 6,
            base + 7,
        )
    } else {
        (0, 0, 0, 0, 0, 0, 0, 0)
    };
    let mut body = Function::new(local_decls);

    let homes = AddrHomes::for_function(function);

    let n_blocks = function.blocks.len();
    if n_blocks == 0 {
        if let Some(result) = function.result {
            emit_zero_const(&mut body, result);
        }
        body.instruction(&Instruction::End);
        return Ok(body);
    }

    // Allocate memory homes for address-taken locals (recursive name params).
    if let Some(bytes) = homes.slab_bytes() {
        emit_bump_alloc(&mut body, bytes as i32, scratch0);
        body.instruction(&Instruction::LocalGet(scratch0));
        body.instruction(&Instruction::LocalSet(home_base));
        emit_zero_fill(&mut body, home_base, bytes as i32, scratch1, scratch2);
    }

    body.instruction(&Instruction::I32Const(function.entry.0 as i32));
    body.instruction(&Instruction::LocalSet(pc_local));

    body.instruction(&Instruction::Loop(BlockType::Empty));
    body.instruction(&Instruction::Block(BlockType::Empty)); // exit
    for _ in 0..n_blocks {
        body.instruction(&Instruction::Block(BlockType::Empty));
    }

    let targets: Vec<u32> = (0..n_blocks as u32).collect();
    body.instruction(&Instruction::LocalGet(pc_local));
    body.instruction(&Instruction::BrTable(Cow::Owned(targets), n_blocks as u32));
    body.instruction(&Instruction::End); // close bb0 shell → bb0 body follows

    for block_id in 0..n_blocks {
        let block = &function.blocks[block_id];
        let mut terminated = false;
        for (op_index, spanned) in block.ops.iter().enumerate() {
            if terminated {
                break;
            }
            if let Some(markers) = debug_markers.as_mut()
                && !(spanned.span.start == 0 && spanned.span.end == 0)
            {
                markers.push(BodyMarker {
                    op: op_index,
                    block: block_id,
                    span_start: spanned.span.start,
                    span_end: spanned.span.end,
                    body_offset: body.byte_len() as u32,
                });
            }
            terminated = emit_op(
                &mut body,
                function,
                &spanned.op,
                EmitScratch {
                    pc: pc_local,
                    s0: scratch0,
                    s1: scratch1,
                    s2: scratch2,
                    s3: scratch3,
                    s4: scratch4,
                    s5: scratch5,
                    s6: scratch6,
                    s7: scratch7,
                    r0: ref0,
                    r1: ref1,
                    r2: ref2,
                    r3: ref3,
                    tf0: gtf0,
                    tf1: gtf1,
                    ch0: gch0,
                    ab0: gab0,
                    ae0: gae0,
                    afe0: gafe0,
                    atxe0: gatxe0,
                    aoe0: gaoe0,
                    home_base,
                    homes: &homes,
                    host,
                },
                n_blocks,
                block_id,
                string_count,
                iovecs,
                function.result,
                func_index,
                funcref_slot,
                indirect_types,
                mir_by_name,
            )?;
        }
        if !terminated {
            if block_id + 1 < n_blocks {
                body.instruction(&Instruction::I32Const((block_id + 1) as i32));
                body.instruction(&Instruction::LocalSet(pc_local));
                emit_br_dispatch(&mut body, n_blocks, block_id);
            } else if let Some(result) = function.result {
                emit_zero_const(&mut body, result);
                body.instruction(&Instruction::Return);
            } else {
                body.instruction(&Instruction::Return);
            }
        }
        body.instruction(&Instruction::End); // peel bb wrapper or exit
    }
    body.instruction(&Instruction::End); // close loop
    // Falling out of the dispatch loop is unreachable (every path `return`s or
    // branches). Returning functions still need a typed value at `end`.
    body.instruction(&Instruction::Unreachable);
    body.instruction(&Instruction::End); // close function
    Ok(body)
}

pub(in crate::codegen::wasm) fn dispatch_br_depth(n_blocks: usize, block_id: usize) -> u32 {
    // After bb0 shell is closed, open above body `block_id`:
    // bb_{block_id+1}..bb_{n-1}, exit, loop → loop depth = n - block_id.
    (n_blocks - block_id) as u32
}

pub(in crate::codegen::wasm) fn emit_br_dispatch(
    body: &mut Function,
    n_blocks: usize,
    block_id: usize,
) {
    body.instruction(&Instruction::Br(dispatch_br_depth(n_blocks, block_id)));
}

pub(in crate::codegen::wasm) struct AddrHomes {
    /// Byte offset within the home slab for each MIR local index. Scalars only:
    /// a home is a linear-memory word, so a Simula reference must not be parked
    /// there.
    pub(in crate::codegen::wasm) offsets: Vec<Option<u32>>,
}

impl AddrHomes {
    fn for_function(function: &MirFunction) -> Self {
        let total = function.params.len() + function.locals.len();
        let mut offsets = vec![None; total];
        let mut next = 0u32;
        for spanned in function.blocks.iter().flat_map(|b| &b.ops) {
            if let Op::LocalAddr { local, .. } = &spanned.op {
                let idx = local.0;
                if idx < total && offsets[idx].is_none() {
                    let ty = function.local(*local).ty;
                    // Name parameters take LocalAddr of the cell; keep an
                    // 8-byte home for every scalar that can be transmitted by name.
                    if matches!(
                        ty,
                        MirType::I64 | MirType::Bool | MirType::F64 | MirType::LongF64
                    ) {
                        offsets[idx] = Some(next);
                        next += 8;
                    }
                }
            }
        }
        Self { offsets }
    }

    fn slab_bytes(&self) -> Option<u32> {
        let max = self
            .offsets
            .iter()
            .filter_map(|o| o.map(|off| off + 8))
            .max()?;
        Some(max)
    }

    fn offset_of(&self, id: LocalId) -> Option<u32> {
        self.offsets.get(id.0).copied().flatten()
    }
}

pub(in crate::codegen::wasm) struct EmitScratch<'a> {
    pub(in crate::codegen::wasm) pc: u32,
    pub(in crate::codegen::wasm) s0: u32,
    pub(in crate::codegen::wasm) s1: u32,
    pub(in crate::codegen::wasm) s2: u32,
    pub(in crate::codegen::wasm) s3: u32,
    pub(in crate::codegen::wasm) s4: u32,
    pub(in crate::codegen::wasm) s5: u32,
    pub(in crate::codegen::wasm) s6: u32,
    pub(in crate::codegen::wasm) s7: u32,
    /// WasmGC eqref scratches (SIMSET link rewiring); unused when GC is off.
    pub(in crate::codegen::wasm) r0: u32,
    pub(in crate::codegen::wasm) r1: u32,
    pub(in crate::codegen::wasm) r2: u32,
    pub(in crate::codegen::wasm) r3: u32,
    /// WasmGC concrete-ref scratches for Text/Array\* (Workstream 2); unused
    /// when GC is off. `tf0`/`tf1`: `text_frame` refs. `ch0`: `text_chars`
    /// ref. `ab0`: `bounds_array` ref. `ae0`/`afe0`/`atxe0`/`aoe0`:
    /// `array_i64`/`array_f64`/`array_text`/`array_object` elems refs.
    pub(in crate::codegen::wasm) tf0: u32,
    pub(in crate::codegen::wasm) tf1: u32,
    pub(in crate::codegen::wasm) ch0: u32,
    pub(in crate::codegen::wasm) ab0: u32,
    pub(in crate::codegen::wasm) ae0: u32,
    pub(in crate::codegen::wasm) afe0: u32,
    pub(in crate::codegen::wasm) atxe0: u32,
    pub(in crate::codegen::wasm) aoe0: u32,
    pub(in crate::codegen::wasm) home_base: u32,
    pub(in crate::codegen::wasm) homes: &'a AddrHomes,
    pub(in crate::codegen::wasm) host: HostImports,
}

pub(in crate::codegen::wasm) fn emit_op(
    body: &mut Function,
    function: &MirFunction,
    op: &Op,
    scratch: EmitScratch,
    n_blocks: usize,
    block_id: usize,
    string_count: usize,
    iovecs: &[(u32, u32)],
    result_ty: Option<MirType>,
    func_index: &std::collections::HashMap<String, u32>,
    funcref_slot: &std::collections::HashMap<String, u32>,
    indirect_types: &std::collections::HashMap<CallSigKey, u32>,
    mir_by_name: &std::collections::HashMap<&str, &MirFunction>,
) -> Result<bool, CompileError> {
    let EmitScratch {
        pc: pc_local,
        s0: scratch0,
        s1: scratch1,
        s2: scratch2,
        s3: scratch3,
        s4: scratch4,
        s5: scratch5,
        s6: scratch6,
        s7: scratch7,
        r0: ref0,
        r1: ref1,
        r2: ref2,
        r3: ref3,
        tf0: gtf0,
        tf1: gtf1,
        ch0: gch0,
        ab0: gab0,
        ae0: gae0,
        afe0: gafe0,
        atxe0: gatxe0,
        aoe0: gaoe0,
        home_base,
        homes,
        host,
    } = scratch;
    let HostImports {
        f64_pow,
        text_getint,
        text_putint,
        text_getfrac,
        text_putfrac,
        text_getreal,
        text_putfix,
        text_putreal,
        out_real,
        out_fix,
        out_frac,
        sysout_write,
        sysout_flush,
        basicio_register,
        basicio_open,
        basicio_close,
        basicio_isopen,
        basicio_out_text,
        basicio_out_char,
        basicio_out_image,
        basicio_break_out_image,
        basicio_in_image,
        basicio_in_char,
        basicio_endfile,
        basicio_image,
        basicio_set_image,
        basicio_pos,
        basicio_length,
        basicio_setpos,
        basicio_line,
        basicio_filename,
        basicio_lastitem,
        basicio_inint,
        basicio_inreal,
        basicio_infrac,
        basicio_intext,
        basicio_out_real,
        basicio_out_fix,
        basicio_out_frac,
        basicio_out_int,
        error: _error,
        basicio_open_byte,
        basicio_in_byte,
        basicio_out_byte,
        basicio_locate,
        basicio_location,
        basicio_lastloc,
        basicio_setaccess,
        basicio_eject,
        basicio_linesperpage,
        basicio_inrecord,
        ..
    } = host;
    match op {
        Op::Nop => Ok(false),
        Op::ConstI64 { dest, value } => {
            body.instruction(&Instruction::I64Const(*value));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::ConstF64 { dest, value } => {
            body.instruction(&Instruction::F64Const(Ieee64::from(*value)));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::I64ToF64 { dest, src } => {
            body.instruction(&Instruction::LocalGet(local_index(*src)));
            body.instruction(&Instruction::F64ConvertI64S);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::F64ToI64 { dest, src } => {
            // Simula real→integer uses `entier` (floor toward −∞).
            body.instruction(&Instruction::LocalGet(local_index(*src)));
            body.instruction(&Instruction::F64Floor);
            body.instruction(&Instruction::I64TruncF64S);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::ConstBool { dest, value } => {
            body.instruction(&Instruction::I64Const(i64::from(*value)));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::Copy { dest, src } => {
            let dest_ty = function.local(*dest).ty;
            let src_ty = function.local(*src).ty;
            if gc_objects_enabled() {
                gc_reject_ref_word_mix("Copy", function, dest_ty, src_ty)?;
            }
            body.instruction(&Instruction::LocalGet(local_index(*src)));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            sync_addr_home(body, function, homes, home_base, *dest, scratch0);
            Ok(false)
        }
        Op::LoadLocal { dest, local } => {
            body.instruction(&Instruction::LocalGet(local_index(*local)));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::StoreLocal { local, src } => {
            body.instruction(&Instruction::LocalGet(local_index(*src)));
            body.instruction(&Instruction::LocalSet(local_index(*local)));
            sync_addr_home(body, function, homes, home_base, *local, scratch0);
            Ok(false)
        }
        Op::Binary {
            dest,
            op,
            left,
            right,
        } => {
            body.instruction(&Instruction::LocalGet(local_index(*left)));
            body.instruction(&Instruction::LocalGet(local_index(*right)));
            if function.local(*left).ty.is_float() {
                match op {
                    BinOp::Add => {
                        body.instruction(&Instruction::F64Add);
                    }
                    BinOp::Sub => {
                        body.instruction(&Instruction::F64Sub);
                    }
                    BinOp::Mul => {
                        body.instruction(&Instruction::F64Mul);
                    }
                    BinOp::Div => {
                        body.instruction(&Instruction::F64Div);
                    }
                    BinOp::Pow => {
                        body.instruction(&Instruction::Call(f64_pow));
                    }
                    BinOp::IntDiv | BinOp::And | BinOp::Or => {
                        return Err(CompileError::codegen(
                            "MIR wasm: integer/boolean binary op on f64 operands",
                        ));
                    }
                }
            } else {
                body.instruction(&match op {
                    BinOp::Add => Instruction::I64Add,
                    BinOp::Sub => Instruction::I64Sub,
                    BinOp::Mul => Instruction::I64Mul,
                    BinOp::Div | BinOp::IntDiv => Instruction::I64DivS,
                    BinOp::And => Instruction::I64And,
                    BinOp::Or => Instruction::I64Or,
                    BinOp::Pow => {
                        return Err(CompileError::codegen("MIR wasm: pow requires f64 operands"));
                    }
                });
            }
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::Unary { dest, op, src } => {
            match op {
                UnOp::Neg if function.local(*src).ty.is_float() => {
                    body.instruction(&Instruction::LocalGet(local_index(*src)));
                    body.instruction(&Instruction::F64Neg);
                }
                UnOp::Neg => {
                    body.instruction(&Instruction::I64Const(0));
                    body.instruction(&Instruction::LocalGet(local_index(*src)));
                    body.instruction(&Instruction::I64Sub);
                }
                UnOp::Not => {
                    body.instruction(&Instruction::LocalGet(local_index(*src)));
                    body.instruction(&Instruction::I64Eqz);
                    body.instruction(&Instruction::I64ExtendI32U);
                }
            }
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::Compare {
            dest,
            op,
            left,
            right,
        } => {
            let left_ty = function.local(*left).ty;
            let right_ty = function.local(*right).ty;
            if left_ty.is_float() || right_ty.is_float() {
                body.instruction(&Instruction::LocalGet(local_index(*left)));
                body.instruction(&Instruction::LocalGet(local_index(*right)));
                body.instruction(&match op {
                    CmpOp::Eq => Instruction::F64Eq,
                    CmpOp::Ne => Instruction::F64Ne,
                    CmpOp::Lt => Instruction::F64Lt,
                    CmpOp::Le => Instruction::F64Le,
                    CmpOp::Gt => Instruction::F64Gt,
                    CmpOp::Ge => Instruction::F64Ge,
                });
            } else if gc_objects_enabled()
                && gc_ref_home_ty(left_ty)
                && gc_ref_home_ty(right_ty)
                && left_ty == right_ty
            {
                // Reference-identity compare on any WasmGC ref type
                // (`ObjectRef` and, since Text/Array* → WasmGC, also
                // `Text`/`ArrayI64`/`ArrayF64`/`ArrayText` — e.g. `t1 == t2`
                // testing whether two text/array *references* are the same
                // underlying frame/descriptor, not content equality).
                body.instruction(&Instruction::LocalGet(local_index(*left)));
                body.instruction(&Instruction::LocalGet(local_index(*right)));
                match op {
                    CmpOp::Eq => {
                        body.instruction(&Instruction::RefEq);
                    }
                    CmpOp::Ne => {
                        body.instruction(&Instruction::RefEq);
                        body.instruction(&Instruction::I32Eqz);
                    }
                    _ => {
                        return Err(CompileError::codegen(
                            "MIR wasm: ordered compare on a WasmGC ref type is not supported",
                        ));
                    }
                }
            } else if gc_objects_enabled() {
                gc_reject_ref_word_mix("Compare", function, left_ty, right_ty)?;
                body.instruction(&Instruction::LocalGet(local_index(*left)));
                body.instruction(&Instruction::LocalGet(local_index(*right)));
                body.instruction(&match op {
                    CmpOp::Eq => Instruction::I64Eq,
                    CmpOp::Ne => Instruction::I64Ne,
                    CmpOp::Lt => Instruction::I64LtS,
                    CmpOp::Le => Instruction::I64LeS,
                    CmpOp::Gt => Instruction::I64GtS,
                    CmpOp::Ge => Instruction::I64GeS,
                });
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*left)));
                body.instruction(&Instruction::LocalGet(local_index(*right)));
                body.instruction(&match op {
                    CmpOp::Eq => Instruction::I64Eq,
                    CmpOp::Ne => Instruction::I64Ne,
                    CmpOp::Lt => Instruction::I64LtS,
                    CmpOp::Le => Instruction::I64LeS,
                    CmpOp::Gt => Instruction::I64GtS,
                    CmpOp::Ge => Instruction::I64GeS,
                });
            }
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::Jump { target } => {
            body.instruction(&Instruction::I32Const(target.0 as i32));
            body.instruction(&Instruction::LocalSet(pc_local));
            emit_br_dispatch(body, n_blocks, block_id);
            Ok(true)
        }
        Op::GotoEscape { .. } => {
            // Match native: trap if a non-local goto is taken (MIR interp unwinds).
            body.instruction(&Instruction::Unreachable);
            Ok(true)
        }
        Op::Branch {
            cond,
            then_block,
            else_block,
        } => {
            body.instruction(&Instruction::LocalGet(local_index(*cond)));
            body.instruction(&Instruction::I32WrapI64);
            body.instruction(&Instruction::If(BlockType::Empty));
            body.instruction(&Instruction::I32Const(then_block.0 as i32));
            body.instruction(&Instruction::LocalSet(pc_local));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::I32Const(else_block.0 as i32));
            body.instruction(&Instruction::LocalSet(pc_local));
            body.instruction(&Instruction::End);
            emit_br_dispatch(body, n_blocks, block_id);
            Ok(true)
        }
        Op::CallOutText { string_id } => {
            if *string_id >= string_count {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: string id {string_id} out of range"
                )));
            }
            emit_sysout_write_iov(body, IOV_BASE + (*string_id as u32) * 8, sysout_write);
            Ok(false)
        }
        Op::CallOutTextLocal { src } => {
            if gc_objects_enabled() {
                emit_out_text_local_gc(
                    body,
                    *src,
                    scratch0,
                    scratch1,
                    scratch2,
                    scratch3,
                    gch0,
                    sysout_write,
                )?;
            } else {
                emit_out_text_local(body, *src, scratch0, scratch1, sysout_write);
            }
            Ok(false)
        }
        Op::CallOutImage => {
            emit_sysout_flush(body, false, sysout_flush);
            Ok(false)
        }
        Op::CallOutInt { value, width } => {
            emit_out_int(
                body,
                *value,
                *width,
                scratch0,
                scratch1,
                scratch2,
                scratch3,
                scratch4,
                sysout_write,
            );
            Ok(false)
        }
        Op::CallOutChar { ch } => {
            emit_out_char(body, *ch, scratch0, scratch1, sysout_write);
            Ok(false)
        }
        Op::CallBreakOutImage => {
            emit_sysout_flush(body, true, sysout_flush);
            Ok(false)
        }
        Op::CallInImage => {
            emit_in_image(body, scratch0, scratch1, scratch2, scratch3);
            Ok(false)
        }
        Op::CallInChar { dest } => {
            emit_in_char(body, *dest, scratch0, scratch1, scratch2, scratch3);
            Ok(false)
        }
        Op::CallEndfile { dest } => {
            emit_endfile(body, *dest, scratch0);
            Ok(false)
        }
        Op::CallEnv { dest, name, args } => {
            // `randint` / `uniform` mutate the stream through a `LocalAddr`
            // pointer; reload name-homes so the next `addr_of` does not
            // overwrite the updated seed with a stale local (simtst78).
            let result = emit_call_env(
                body, function, host, *dest, name, args, scratch0, scratch1, scratch2, scratch3,
                scratch4, scratch5, gch0,
            )?;
            reload_addr_homes(body, function, homes, home_base, scratch0);
            Ok(result)
        }
        Op::CallInLine { dest } => {
            if gc_objects_enabled() {
                emit_in_line_gc(body, *dest, scratch0, scratch1, scratch2, gch0)?;
            } else {
                emit_in_line(body, *dest, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::CallFileExists { .. } | Op::CallFileRead { .. } | Op::CallFileWrite { .. } => Err(
            CompileError::codegen("MIR wasm: whole-file I/O is not supported yet (native only)"),
        ),
        Op::CallOutReal {
            value,
            digits,
            width,
        } => {
            let exp_digits = if function.local(*value).ty == MirType::LongF64 {
                3
            } else {
                2
            };
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::I64Const(exp_digits));
            body.instruction(&Instruction::Call(out_real));
            Ok(false)
        }
        Op::CallOutFix {
            value,
            digits,
            width,
        } => {
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(out_fix));
            Ok(false)
        }
        Op::CallOutFrac {
            value,
            digits,
            width,
        } => {
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(out_frac));
            Ok(false)
        }
        Op::CallSysIn { dest } => {
            if gc_objects_enabled() {
                body.instruction(&Instruction::GlobalGet(GLOBAL_SYSIN));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            } else {
                emit_load_cell(body, SYSIN_OBJ_PTR);
                body.instruction(&Instruction::I64ExtendI32U);
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            Ok(false)
        }
        Op::CallSysOut { dest } => {
            if gc_objects_enabled() {
                body.instruction(&Instruction::GlobalGet(GLOBAL_SYSOUT));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            } else {
                emit_load_cell(body, SYSOUT_OBJ_PTR);
                body.instruction(&Instruction::I64ExtendI32U);
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            Ok(false)
        }
        // Terminal files are always open; disk files delegate to the host.
        Op::CallBasicioRegisterFile { object, path, mode } => {
            // Modes 0–5 are registered so `filename` / `isopen` work for all
            // File subclasses (simtst78). Open/I/O for byte and Direct still
            // reject at `ensure_supported_subset` / host open.
            emit_object_host_i64(body, *object, scratch0);
            if gc_objects_enabled() {
                // `path` is a WasmGC `text_frame` ref, not a linear-memory
                // pointer — build a throwaway linear bump frame from its
                // content (read-only: the host only reads the filename, so
                // no `emit_text_finish_host_frame_gc` writeback is needed)
                // and pass *that* address, mirroring `emit_text_getint`-style
                // host bridges above.
                emit_text_prepare_host_frame_gc(
                    body, *path, scratch1, scratch2, scratch3, scratch4, scratch5, scratch6, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch5));
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*path)));
                body.instruction(&Instruction::I32WrapI64);
            }
            body.instruction(&Instruction::I64Const(*mode));
            body.instruction(&Instruction::Call(basicio_register));
            Ok(false)
        }
        Op::CallBasicioOpen {
            dest,
            object,
            fileimage,
        } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            body.instruction(&Instruction::I64Const(1));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            if gc_objects_enabled() {
                // `fileimage` is a WasmGC `text_frame` ref; the host only
                // reads its `.len` (via a bump-frame pointer), so bridge it
                // through a throwaway linear frame like
                // `CallBasicioRegisterFile`'s `path` above.
                emit_text_prepare_host_frame_gc(
                    body, *fileimage, scratch1, scratch2, scratch3, scratch4, scratch5, scratch6,
                    gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch5));
                body.instruction(&Instruction::I64ExtendI32U);
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*fileimage)));
            }
            body.instruction(&Instruction::Call(basicio_open));
            emit_host_i64_to_bool(body, *dest);
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioClose { dest, object } | Op::CallBasicioIsOpen { dest, object } => {
            let host_fn = if matches!(op, Op::CallBasicioClose { .. }) {
                basicio_close
            } else {
                basicio_isopen
            };
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            body.instruction(&Instruction::I64Const(1));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(host_fn));
            emit_host_i64_to_bool(body, *dest);
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutText { object, text } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            if gc_objects_enabled() {
                emit_out_text_local_gc(
                    body,
                    *text,
                    scratch0,
                    scratch2,
                    scratch3,
                    scratch4,
                    gch0,
                    sysout_write,
                )?;
            } else {
                emit_out_text_local(body, *text, scratch0, scratch2, sysout_write);
            }
            body.instruction(&Instruction::Else);
            if gc_objects_enabled() {
                emit_disk_out_text_local_gc(
                    body,
                    scratch0,
                    *text,
                    scratch2,
                    scratch3,
                    scratch4,
                    scratch5,
                    gch0,
                    basicio_out_text,
                )?;
            } else {
                emit_disk_out_text_local(
                    body,
                    scratch0,
                    *text,
                    scratch2,
                    scratch3,
                    basicio_out_text,
                );
            }
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutChar { object, ch } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_out_char(body, *ch, scratch0, scratch2, sysout_write);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*ch)));
            body.instruction(&Instruction::Call(basicio_out_char));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutImage { object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_sysout_flush(body, false, sysout_flush);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_out_image));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioBreakOutImage { object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_sysout_flush(body, true, sysout_flush);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_break_out_image));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutInt {
            object,
            value,
            width,
        } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_out_int(
                body,
                *value,
                *width,
                scratch0,
                scratch2,
                scratch3,
                scratch4,
                scratch5,
                sysout_write,
            );
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(basicio_out_int));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutReal {
            object,
            value,
            digits,
            width,
            exp_digits,
        } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::I64Const(*exp_digits));
            body.instruction(&Instruction::Call(out_real));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::I64Const(*exp_digits));
            body.instruction(&Instruction::Call(basicio_out_real));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutFix {
            object,
            value,
            digits,
            width,
        } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(out_fix));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(basicio_out_fix));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOutFrac {
            object,
            value,
            digits,
            width,
        } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(out_frac));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::LocalGet(local_index(*digits)));
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(basicio_out_frac));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioInImage { object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_in_image(body, scratch0, scratch1, scratch2, scratch3);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_in_image));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioInChar { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_in_char(body, *dest, scratch0, scratch2, scratch3, scratch4);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_in_char));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioEndfile { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_endfile(body, *dest, scratch0);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_endfile));
            emit_host_i64_to_bool(body, *dest);
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioLastItem { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_sysin_skip_blanks(body, scratch0, scratch2, scratch3, scratch4);
            emit_load_sysin_base(body, scratch0);
            emit_image_load(body, scratch0, IMAGE_OFF_FLAG);
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_lastitem));
            emit_host_i64_to_bool(body, *dest);
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioInInt { dest, object }
        | Op::CallBasicioInReal { dest, object }
        | Op::CallBasicioInFrac { dest, object } => {
            let (terminal_getter, disk_fn) = match op {
                Op::CallBasicioInInt { .. } => (text_getint, basicio_inint),
                Op::CallBasicioInReal { .. } => (text_getreal, basicio_inreal),
                _ => (text_getfrac, basicio_infrac),
            };
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_sysin_item_frame(body, scratch0, scratch2, scratch3, scratch4, scratch5);
            body.instruction(&Instruction::LocalGet(scratch2));
            body.instruction(&Instruction::Call(terminal_getter));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            emit_sysin_item_advance(body, scratch0, scratch2, scratch3);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(disk_fn));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioInText {
            dest,
            object,
            width,
        } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            if gc_objects_enabled() {
                emit_basicio_intext_gc(
                    body, *dest, *width, scratch0, scratch2, scratch3, scratch4, scratch5,
                    scratch6, scratch7, gch0,
                )?;
            } else {
                emit_basicio_intext(
                    body, *dest, *width, scratch0, scratch2, scratch3, scratch4, scratch5,
                    scratch6, scratch7,
                );
            }
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*width)));
            body.instruction(&Instruction::Call(basicio_intext));
            if gc_objects_enabled() {
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::LocalSet(scratch2)); // host FRAME ptr
                emit_frame_load(body, scratch2, FRAME_OFF_PTR);
                body.instruction(&Instruction::LocalSet(scratch0));
                emit_frame_load(body, scratch2, FRAME_OFF_LEN);
                body.instruction(&Instruction::LocalSet(scratch1));
                emit_push_text_frame_from_linear_bytes(body, scratch0, scratch1, scratch3, gch0)?;
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            } else {
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioImage { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_terminal_image_base(body, *object, scratch0);
            if gc_objects_enabled() {
                // ptr = base + IMAGE_OFF_BUF; len = image's current length.
                body.instruction(&Instruction::LocalGet(scratch0));
                body.instruction(&Instruction::I32Const(IMAGE_OFF_BUF as i32));
                body.instruction(&Instruction::I32Add);
                body.instruction(&Instruction::LocalSet(scratch2));
                emit_image_load(body, scratch0, IMAGE_OFF_LEN);
                body.instruction(&Instruction::LocalSet(scratch3));
                emit_push_text_frame_from_linear_bytes(body, scratch2, scratch3, scratch4, gch0)?;
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            } else {
                body.instruction(&Instruction::LocalGet(scratch0));
                body.instruction(&Instruction::I64ExtendI32U);
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            body.instruction(&Instruction::Else);
            emit_bump_alloc(body, FRAME_SIZE, scratch2);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(scratch2));
            body.instruction(&Instruction::Call(basicio_image));
            if gc_objects_enabled() {
                emit_frame_load(body, scratch2, FRAME_OFF_PTR);
                body.instruction(&Instruction::LocalSet(scratch0));
                emit_frame_load(body, scratch2, FRAME_OFF_LEN);
                body.instruction(&Instruction::LocalSet(scratch1));
                emit_push_text_frame_from_linear_bytes(body, scratch0, scratch1, scratch3, gch0)?;
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            } else {
                body.instruction(&Instruction::LocalGet(scratch2));
                body.instruction(&Instruction::I64ExtendI32U);
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioSetImage { object, text } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_terminal_image_base(body, *object, scratch0);
            if gc_objects_enabled() {
                emit_basicio_set_image_gc(
                    body, *text, scratch0, scratch2, scratch3, scratch4, scratch5, gch0, scratch6,
                    scratch7,
                )?;
            } else {
                emit_basicio_set_image(
                    body, *text, scratch0, scratch2, scratch3, scratch4, scratch5,
                );
            }
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            if gc_objects_enabled() {
                emit_text_to_linear_scratch_gc(
                    body, *text, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                emit_bump_alloc(body, FRAME_SIZE, scratch6);
                emit_frame_store_local(body, scratch6, FRAME_OFF_PTR, scratch2);
                emit_frame_store_local(body, scratch6, FRAME_OFF_LEN, scratch4);
                emit_frame_store_const(body, scratch6, FRAME_OFF_POS, 1);
                emit_frame_store_const(body, scratch6, FRAME_OFF_PAD, 0);
                emit_frame_store_const(body, scratch6, FRAME_OFF_START, 1);
                emit_frame_store_local(body, scratch6, FRAME_OFF_MAIN_LEN, scratch4);
                body.instruction(&Instruction::LocalGet(scratch6));
                body.instruction(&Instruction::I64ExtendI32U);
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*text)));
            }
            body.instruction(&Instruction::Call(basicio_set_image));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioPos { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_terminal_image_base(body, *object, scratch0);
            emit_image_load(body, scratch0, IMAGE_OFF_POS);
            body.instruction(&Instruction::I64ExtendI32S);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_pos));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioLength { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_terminal_image_base(body, *object, scratch0);
            emit_image_load(body, scratch0, IMAGE_OFF_LEN);
            body.instruction(&Instruction::I64ExtendI32S);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_length));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioSetpos { object, index } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_terminal_image_base(body, *object, scratch0);
            emit_basicio_setpos(body, *index, scratch0, scratch2);
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(local_index(*index)));
            body.instruction(&Instruction::Call(basicio_setpos));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioLine { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_is_sysin(body, *object, scratch2);
            body.instruction(&Instruction::LocalGet(scratch2));
            body.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            body.instruction(&Instruction::I64Const(0));
            body.instruction(&Instruction::Else);
            emit_load_cell(body, SYSOUT_BASE_PTR);
            body.instruction(&Instruction::LocalSet(scratch0));
            emit_image_load(body, scratch0, IMAGE_OFF_FLAG);
            body.instruction(&Instruction::I64ExtendI32S);
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::Else);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::Call(basicio_line));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioFilename { dest, object } => {
            emit_object_i32(body, *object, scratch0);
            emit_is_terminal_flag(body, *object, scratch0, scratch1);
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::If(BlockType::Empty));
            emit_is_sysin(body, *object, scratch2);
            body.instruction(&Instruction::LocalGet(scratch2));
            body.instruction(&Instruction::If(BlockType::Empty));
            let (ptr, len) = iovecs[string_count + 1];
            if gc_objects_enabled() {
                emit_text_from_literal_gc(body, *dest, ptr, len)?;
            } else {
                emit_text_from_literal(body, *dest, ptr, len, scratch0, scratch3, scratch4);
            }
            body.instruction(&Instruction::Else);
            let (ptr, len) = iovecs[string_count + 2];
            if gc_objects_enabled() {
                emit_text_from_literal_gc(body, *dest, ptr, len)?;
            } else {
                emit_text_from_literal(body, *dest, ptr, len, scratch0, scratch3, scratch4);
            }
            body.instruction(&Instruction::End);
            body.instruction(&Instruction::Else);
            emit_bump_alloc(body, FRAME_SIZE, scratch2);
            body.instruction(&Instruction::LocalGet(scratch0));

            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalGet(scratch2));
            body.instruction(&Instruction::Call(basicio_filename));
            if gc_objects_enabled() {
                emit_frame_load(body, scratch2, FRAME_OFF_PTR);
                body.instruction(&Instruction::LocalSet(scratch0));
                emit_frame_load(body, scratch2, FRAME_OFF_LEN);
                body.instruction(&Instruction::LocalSet(scratch1));
                emit_push_text_frame_from_linear_bytes(body, scratch0, scratch1, scratch3, gch0)?;
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            } else {
                body.instruction(&Instruction::LocalGet(scratch2));
                body.instruction(&Instruction::I64ExtendI32U);
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            body.instruction(&Instruction::End);
            Ok(false)
        }
        Op::CallBasicioOpenByte { dest, object } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::Call(basicio_open_byte));
            emit_host_i64_to_bool(body, *dest);
            Ok(false)
        }
        Op::CallBasicioInByte { dest, object } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::Call(basicio_in_byte));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallBasicioOutByte { object, value } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::LocalGet(local_index(*value)));
            body.instruction(&Instruction::Call(basicio_out_byte));
            Ok(false)
        }
        Op::CallBasicioLocate { object, loc } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::LocalGet(local_index(*loc)));
            body.instruction(&Instruction::Call(basicio_locate));
            Ok(false)
        }
        Op::CallBasicioLocation { dest, object } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::Call(basicio_location));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallBasicioLastloc { dest, object } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::Call(basicio_lastloc));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallBasicioSetAccess { dest, object, mode } => {
            emit_object_host_i64(body, *object, scratch0);
            if gc_objects_enabled() {
                // `mode` is a WasmGC `text_frame` ref; the host reads its
                // full content (e.g. "append"/"create"), so bridge it
                // through a throwaway linear frame, read-only.
                emit_text_prepare_host_frame_gc(
                    body, *mode, scratch1, scratch2, scratch3, scratch4, scratch5, scratch6, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch5));
                body.instruction(&Instruction::I64ExtendI32U);
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*mode)));
            }
            body.instruction(&Instruction::Call(basicio_setaccess));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallBasicioEject { object, line } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::LocalGet(local_index(*line)));
            body.instruction(&Instruction::Call(basicio_eject));
            Ok(false)
        }
        Op::CallBasicioLinesPerPage { dest, object, n } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::LocalGet(local_index(*n)));
            body.instruction(&Instruction::Call(basicio_linesperpage));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallBasicioInRecord { dest, object } => {
            emit_object_host_i64(body, *object, scratch0);
            body.instruction(&Instruction::Call(basicio_inrecord));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallTerminateProgram => {
            body.instruction(&Instruction::Unreachable);
            Ok(true)
        }
        Op::SimBegin
        | Op::SimEnd
        | Op::SimHold { .. }
        | Op::SimActivateDirect { .. }
        | Op::SimActivateTimed { .. }
        | Op::SimActivateRelative { .. }
        | Op::SimPassivate
        | Op::SimTransferToHead
        | Op::SimTerminateCurrent { .. }
        | Op::SimCancel { .. }
        | Op::SimFinishMain
        | Op::SimTime { .. }
        | Op::SimIsMainCurrent { .. }
        | Op::SimHasCurrent { .. }
        | Op::SimCurrent { .. }
        | Op::SimMain { .. }
        | Op::SimIdle { .. }
        | Op::SimTerminated { .. }
        | Op::SimEvtime { .. }
        | Op::SimNextev { .. } => Err(CompileError::codegen(
            "internal error: a Simulation op reached wasm codegen; \
             `mir::asyncify` should have rewritten it into a call on the spill-buffer runtime",
        )),
        Op::SimsetSetHeadClassId { class_id } => {
            // Linear cell is fine under WasmGC — only an i64 class id.
            body.instruction(&Instruction::I32Const(SIMSET_HEAD_CLASS_ID_PTR as i32));
            body.instruction(&Instruction::I64Const(*class_id));
            body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            }));
            Ok(false)
        }
        Op::SimsetInitHead { head } => {
            if gc_objects_enabled() {
                emit_simset_init_head_gc(body, function, *head, scratch0, scratch1, ref0)?;
            } else {
                emit_simset_init_head(body, *head, scratch0);
            }
            Ok(false)
        }
        Op::SimsetOut { object } => {
            if gc_objects_enabled() {
                emit_simset_out_gc(
                    body, function, *object, scratch0, scratch1, scratch2, ref0, ref1, ref2,
                )?;
            } else {
                emit_ref_ptr(body, *object);
                body.instruction(&Instruction::LocalSet(scratch0));
                emit_simset_out(body, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::SimsetPrecede { object, ptr } => {
            if gc_objects_enabled() {
                emit_simset_precede_gc(
                    body, function, *object, *ptr, scratch0, scratch1, scratch2, scratch3,
                    scratch4, ref0, ref1, ref2, ref3,
                )?;
            } else {
                emit_simset_precede(
                    body, *object, *ptr, scratch0, scratch1, scratch2, scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::SimsetInto { object, head } => {
            if gc_objects_enabled() {
                reload_addr_homes(body, function, homes, home_base, scratch0);
                emit_simset_precede_gc(
                    body, function, *object, *head, scratch0, scratch1, scratch2, scratch3,
                    scratch4, ref0, ref1, ref2, ref3,
                )?;
            } else {
                emit_simset_precede(
                    body, *object, *head, scratch0, scratch1, scratch2, scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::SimsetFollow { object, ptr } => {
            if gc_objects_enabled() {
                emit_simset_follow_gc(
                    body, function, *object, *ptr, scratch0, scratch1, scratch2, scratch3,
                    scratch4, ref0, ref1, ref2, ref3,
                )?;
            } else {
                emit_simset_follow(
                    body, *object, *ptr, scratch0, scratch1, scratch2, scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::SimsetSuc { dest, object } => {
            if gc_objects_enabled() {
                reload_addr_homes(body, function, homes, home_base, scratch0);
                emit_simset_neighbour_gc(
                    body,
                    function,
                    *dest,
                    *object,
                    SIMSET_SUC_OFFSET,
                    scratch0,
                    ref0,
                    ref1,
                    ref2,
                )?;
            } else {
                emit_simset_neighbour(body, *dest, *object, SIMSET_SUC_OFFSET, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::SimsetPred { dest, object } => {
            if gc_objects_enabled() {
                reload_addr_homes(body, function, homes, home_base, scratch0);
                emit_simset_neighbour_gc(
                    body,
                    function,
                    *dest,
                    *object,
                    SIMSET_PRED_OFFSET,
                    scratch0,
                    ref0,
                    ref1,
                    ref2,
                )?;
            } else {
                emit_simset_neighbour(body, *dest, *object, SIMSET_PRED_OFFSET, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::SimsetEmpty { dest, head } => {
            if gc_objects_enabled() {
                // `towns` (and other address-taken ObjectRefs) can go stale in the
                // wasm local while the root-handle home still holds the real
                // Head — BASICIO sequences do not reload. Using the stale ref
                // here `ref.cast`s a non-linkage object to `linkage_base` and
                // traps (simtst96). Refresh before touching the ring.
                reload_addr_homes(body, function, homes, home_base, scratch0);
                emit_simset_empty_gc(body, function, *dest, *head, scratch0, ref0, ref1)?;
            } else {
                emit_simset_empty(body, *dest, *head, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::SimsetCardinal { dest, head } => {
            if gc_objects_enabled() {
                emit_simset_cardinal_gc(
                    body, function, *dest, *head, scratch0, scratch1, scratch2, ref0, ref1,
                )?;
            } else {
                emit_simset_cardinal(body, *dest, *head, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::SeqSystemEnter { .. }
        | Op::SeqSystemExit { .. }
        | Op::SeqObjectCreate { .. }
        | Op::SeqObjectStart { .. }
        | Op::SeqBlockInstance { .. }
        | Op::SeqDetach { .. }
        | Op::SeqCall { .. }
        | Op::SeqResume { .. }
        | Op::SeqTerminate { .. } => Err(CompileError::codegen(
            "internal error: a chapter 7 sequencing op reached wasm codegen; \
             `mir::asyncify` should have rewritten it into a call on the spill-buffer runtime",
        )),
        Op::TextNotext { dest } => {
            if gc_objects_enabled() {
                emit_text_notext_gc(body, *dest)?;
            } else {
                emit_text_notext(body, *dest, scratch0);
            }
            Ok(false)
        }
        Op::TextFromLiteral { dest, string_id } => {
            if *string_id >= string_count {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: string id {string_id} out of range"
                )));
            }
            let (ptr, len) = iovecs[*string_id];
            if gc_objects_enabled() {
                emit_text_from_literal_gc(body, *dest, ptr, len)?;
            } else {
                emit_text_from_literal(body, *dest, ptr, len, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::TextAssign { dest, src } => {
            if gc_objects_enabled() {
                emit_text_assign_gc(body, *dest, *src, scratch0, scratch1, scratch2, scratch3)?;
            } else {
                emit_text_assign_value(
                    body, *dest, *src, scratch0, scratch1, scratch2, scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::TextRefAssign { dest, src } => {
            if gc_objects_enabled() {
                emit_text_ref_assign_gc(body, *dest, *src)?;
            } else {
                emit_text_share_assign(body, *dest, *src, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::TextCopy { dest, src } => {
            if gc_objects_enabled() {
                emit_text_copy_gc(body, *dest, *src, scratch0, scratch1, gch0)?;
            } else {
                emit_text_copy(body, *dest, *src, scratch0, scratch1, scratch2, scratch3);
            }
            Ok(false)
        }
        Op::ArrayCopy { dest, src } => {
            let is_text = function.local(*dest).ty == MirType::ArrayText;
            if gc_objects_enabled() {
                emit_array_copy_nd_gc(
                    body, function, *dest, *src, gab0, gae0, gafe0, gatxe0, gaoe0, gtf1, scratch0,
                    scratch1, scratch2, scratch3, gch0,
                )?;
            } else {
                emit_array_copy_nd(
                    body, *dest, *src, is_text, scratch0, scratch1, scratch2, scratch3, scratch4,
                    scratch5, scratch6, scratch7,
                );
            }
            Ok(false)
        }
        Op::TextConcat { dest, left, right } => {
            if gc_objects_enabled() {
                emit_text_concat_gc(
                    body, *dest, *left, *right, scratch0, scratch1, scratch2, gch0,
                )?;
            } else {
                emit_text_concat(
                    body, *dest, *left, *right, scratch0, scratch1, scratch2, scratch3,
                );
            }
            Ok(false)
        }
        Op::TextContentEq { dest, left, right } => {
            if gc_objects_enabled() {
                emit_text_content_eq_gc(
                    body, *dest, *left, *right, scratch0, scratch1, scratch2, scratch3, scratch4,
                    scratch5, scratch6, scratch7, gch0,
                )?;
            } else {
                emit_text_content_eq(
                    body, *dest, *left, *right, scratch0, scratch1, scratch2, scratch3,
                );
            }
            Ok(false)
        }
        Op::TextContentCmp { dest, left, right } => {
            if gc_objects_enabled() {
                emit_text_content_cmp_gc(
                    body, *dest, *left, *right, scratch0, scratch1, scratch2, scratch3, scratch4,
                    scratch5, scratch6, scratch7, gch0,
                )?;
            } else {
                emit_text_content_cmp(
                    body, *dest, *left, *right, scratch0, scratch1, scratch2, scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::TextLength { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_field_i32_gc(
                    body,
                    *dest,
                    *frame,
                    crate::codegen::wasm_gc::TEXT_FRAME_FIELD_LENGTH,
                )?;
            } else {
                emit_text_length(body, *dest, *frame, scratch0);
            }
            Ok(false)
        }
        Op::TextConstant { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_field_i32_gc(
                    body,
                    *dest,
                    *frame,
                    crate::codegen::wasm_gc::TEXT_FRAME_FIELD_CONSTANT,
                )?;
            } else {
                emit_text_constant(body, *dest, *frame, scratch0);
            }
            Ok(false)
        }
        Op::TextStart { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_field_i32_gc(
                    body,
                    *dest,
                    *frame,
                    crate::codegen::wasm_gc::TEXT_FRAME_FIELD_START,
                )?;
            } else {
                emit_text_start(body, *dest, *frame, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::TextMain { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_main_gc(body, *dest, *frame, gch0, scratch0)?;
            } else {
                emit_text_main(body, *dest, *frame, scratch0, scratch1, scratch2, scratch3);
            }
            Ok(false)
        }
        Op::TextPos { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_field_i32_gc(
                    body,
                    *dest,
                    *frame,
                    crate::codegen::wasm_gc::TEXT_FRAME_FIELD_POS,
                )?;
            } else {
                emit_text_pos(body, *dest, *frame, scratch0);
            }
            Ok(false)
        }
        Op::TextMore { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_more_gc(body, *dest, *frame, scratch0)?;
            } else {
                emit_text_more(body, *dest, *frame, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::TextSetpos { frame, index } => {
            if gc_objects_enabled() {
                emit_text_setpos_gc(body, *frame, *index, scratch0, scratch1)?;
            } else {
                emit_text_setpos(body, *frame, *index, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::TextGetchar { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_getchar_gc(body, *dest, *frame, scratch0, scratch1)?;
            } else {
                emit_text_getchar(body, *dest, *frame, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::TextPutchar { frame, ch } => {
            if gc_objects_enabled() {
                emit_text_putchar_gc(body, *frame, *ch, scratch0, scratch1)?;
            } else {
                emit_text_putchar(body, *frame, *ch, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::TextBlanks { dest, n } => {
            if gc_objects_enabled() {
                emit_text_blanks_gc(body, *dest, *n, scratch0, scratch1, gch0)?;
            } else {
                emit_text_blanks(body, *dest, *n, scratch0, scratch1, scratch2, scratch3);
            }
            Ok(false)
        }
        Op::TextRefEq { dest, left, right } => {
            if gc_objects_enabled() {
                emit_text_ref_eq_gc(body, *dest, *left, *right, scratch0)?;
            } else {
                emit_text_ref_eq(body, *dest, *left, *right, scratch0, scratch1);
            }
            Ok(false)
        }
        Op::TextSub { dest, frame, i, n } => {
            if gc_objects_enabled() {
                emit_text_sub_gc(
                    body, *dest, *frame, *i, *n, scratch0, scratch1, scratch2, scratch3, gtf0,
                )?;
            } else {
                emit_text_sub(
                    body, *dest, *frame, *i, *n, scratch0, scratch1, scratch2, scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::TextStrip { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_strip_gc(body, *dest, *frame, scratch0, scratch1)?;
            } else {
                emit_text_strip(body, *dest, *frame, scratch0, scratch1, scratch2, scratch3);
            }
            Ok(false)
        }
        Op::TextUpcase { frame } => {
            if gc_objects_enabled() {
                emit_text_case_fold_gc(body, *frame, true, scratch0, scratch1, scratch2)?;
            } else {
                emit_text_case_fold(body, *frame, true, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::TextLowcase { frame } => {
            if gc_objects_enabled() {
                emit_text_case_fold_gc(body, *frame, false, scratch0, scratch1, scratch2)?;
            } else {
                emit_text_case_fold(body, *frame, false, scratch0, scratch1, scratch2);
            }
            Ok(false)
        }
        Op::TextGetint { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_prepare_host_frame_gc(
                    body, *frame, scratch0, scratch1, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
                body.instruction(&Instruction::Call(text_getint));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
                emit_text_finish_host_frame_gc(
                    body, *frame, scratch0, scratch2, scratch1, scratch3, scratch4, gch0,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::Call(text_getint));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            Ok(false)
        }
        Op::TextPutint { frame, value } => {
            if gc_objects_enabled() {
                emit_text_prepare_host_frame_gc(
                    body, *frame, scratch0, scratch1, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::Call(text_putint));
                emit_text_finish_host_frame_gc(
                    body, *frame, scratch0, scratch2, scratch1, scratch3, scratch4, gch0,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::Call(text_putint));
            }
            Ok(false)
        }
        Op::TextGetfrac { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_prepare_host_frame_gc(
                    body, *frame, scratch0, scratch1, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
                body.instruction(&Instruction::Call(text_getfrac));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
                emit_text_finish_host_frame_gc(
                    body, *frame, scratch0, scratch2, scratch1, scratch3, scratch4, gch0,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::Call(text_getfrac));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            Ok(false)
        }
        Op::TextPutfrac {
            frame,
            value,
            places,
        } => {
            if gc_objects_enabled() {
                emit_text_prepare_host_frame_gc(
                    body, *frame, scratch0, scratch1, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::LocalGet(local_index(*places)));
                body.instruction(&Instruction::Call(text_putfrac));
                emit_text_finish_host_frame_gc(
                    body, *frame, scratch0, scratch2, scratch1, scratch3, scratch4, gch0,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::LocalGet(local_index(*places)));
                body.instruction(&Instruction::Call(text_putfrac));
            }
            Ok(false)
        }
        Op::TextGetreal { dest, frame } => {
            if gc_objects_enabled() {
                emit_text_prepare_host_frame_gc(
                    body, *frame, scratch0, scratch1, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
                body.instruction(&Instruction::Call(text_getreal));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
                emit_text_finish_host_frame_gc(
                    body, *frame, scratch0, scratch2, scratch1, scratch3, scratch4, gch0,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::Call(text_getreal));
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            Ok(false)
        }
        Op::TextPutfix {
            frame,
            value,
            places,
        } => {
            if gc_objects_enabled() {
                emit_text_prepare_host_frame_gc(
                    body, *frame, scratch0, scratch1, scratch2, scratch3, scratch4, scratch5, gch0,
                )?;
                body.instruction(&Instruction::LocalGet(scratch4));
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::LocalGet(local_index(*places)));
                body.instruction(&Instruction::Call(text_putfix));
                emit_text_finish_host_frame_gc(
                    body, *frame, scratch0, scratch2, scratch1, scratch3, scratch4, gch0,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::LocalGet(local_index(*places)));
                body.instruction(&Instruction::Call(text_putfix));
            }
            Ok(false)
        }
        Op::TextPutreal {
            frame,
            value,
            places,
            exp_digits,
        } => {
            if gc_objects_enabled() {
                emit_text_putreal_gc(
                    body,
                    *frame,
                    *value,
                    *places,
                    *exp_digits,
                    scratch0,
                    scratch1,
                    scratch2,
                    scratch3,
                    scratch4,
                    scratch5,
                    gch0,
                    text_putreal,
                )?;
            } else {
                body.instruction(&Instruction::LocalGet(local_index(*frame)));
                body.instruction(&Instruction::I32WrapI64);
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::LocalGet(local_index(*places)));
                body.instruction(&Instruction::I64Const(*exp_digits));
                body.instruction(&Instruction::Call(text_putreal));
            }
            Ok(false)
        }
        Op::AllocArray { dest, bounds } => {
            if bounds.is_empty() {
                return Err(CompileError::codegen(
                    "MIR wasm: AllocArray requires at least one dimension",
                ));
            }
            if gc_objects_enabled() {
                emit_array_alloc_nd_gc(
                    body, function, *dest, bounds, scratch0, scratch1, scratch2, gab0, gae0, gafe0,
                    gatxe0, gaoe0,
                )?;
            } else {
                let is_text = function.local(*dest).ty == MirType::ArrayText;
                emit_array_alloc_nd(
                    body, *dest, bounds, is_text, scratch0, scratch1, scratch2, scratch3, scratch4,
                    scratch5,
                );
            }
            Ok(false)
        }
        Op::ArrayLoad {
            dest,
            array,
            indices,
        } => {
            if indices.is_empty() {
                return Err(CompileError::codegen(
                    "MIR wasm: ArrayLoad requires at least one index",
                ));
            }
            if gc_objects_enabled() {
                emit_array_load_nd_gc(
                    body, function, *dest, *array, indices, gab0, gae0, gafe0, gatxe0, gaoe0,
                    scratch0, scratch1, scratch2,
                )?;
            } else {
                emit_array_load_nd(
                    body, function, *dest, *array, indices, scratch0, scratch1, scratch2, scratch3,
                    scratch4,
                );
            }
            Ok(false)
        }
        Op::ArrayStore {
            array,
            indices,
            value,
        } => {
            if indices.is_empty() {
                return Err(CompileError::codegen(
                    "MIR wasm: ArrayStore requires at least one index",
                ));
            }
            if gc_objects_enabled() {
                emit_array_store_nd_gc(
                    body, function, *array, indices, *value, gab0, gae0, gafe0, gatxe0, gaoe0,
                    scratch0, scratch1, scratch2,
                )?;
            } else {
                emit_array_store_nd(
                    body, function, *array, indices, *value, scratch0, scratch1, scratch2,
                    scratch3, scratch4,
                );
            }
            Ok(false)
        }
        Op::ConstNone { dest } => {
            if gc_objects_enabled() {
                body.instruction(&Instruction::RefNull(
                    crate::codegen::wasm_gc::object_ref_heap(),
                ));
            } else {
                body.instruction(&Instruction::I64Const(0));
            }
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::NewObject {
            dest,
            class_id,
            size,
        } => {
            if *size < 8 || *size > i32::MAX as i64 {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: NewObject size {size} is out of range"
                )));
            }
            // Sequencing records (`FunctionBuilder::alloc`) also use NewObject into
            // PTR locals — those must stay on the bump heap. Only ObjectRef dens
            // become WasmGC structs.
            if gc_objects_enabled() && function.local(*dest).ty == MirType::ObjectRef {
                emit_new_object_gc(body, function, *dest, *class_id)?;
            } else {
                emit_new_object(
                    body,
                    *dest,
                    *class_id,
                    *size as i32,
                    scratch0,
                    scratch1,
                    scratch2,
                );
            }
            Ok(false)
        }
        Op::FieldLoadI64 {
            dest,
            object,
            offset,
            class_qual,
        } => {
            if gc_objects_enabled() && function.local(*object).ty == MirType::ObjectRef {
                emit_field_load_gc(
                    body,
                    function,
                    *dest,
                    *object,
                    *offset,
                    function.local(*dest).ty,
                    class_qual.as_deref(),
                    scratch0,
                    scratch1,
                )?;
            } else if gc_objects_enabled() && gc_ref_home_ty(function.local(*dest).ty) {
                return Err(CompileError::codegen(
                    "MIR wasm: cannot load a GC ref through a non-object pointer \
                     (a reference has no integer encoding under WasmGC)",
                ));
            } else {
                emit_field_load_i64(
                    body,
                    *dest,
                    *object,
                    *offset,
                    scratch0,
                    function.local(*dest).ty,
                );
            }
            Ok(false)
        }
        Op::FieldStoreI64 {
            object,
            offset,
            value,
            class_qual,
        } => {
            if gc_objects_enabled() && function.local(*object).ty == MirType::ObjectRef {
                emit_field_store_gc(
                    body,
                    function,
                    *object,
                    *offset,
                    *value,
                    function.local(*value).ty,
                    class_qual.as_deref(),
                    scratch0,
                    scratch1,
                    scratch2,
                )?;
            } else if gc_objects_enabled() && gc_ref_home_ty(function.local(*value).ty) {
                return Err(CompileError::codegen(
                    "MIR wasm: cannot store a GC ref through a non-object pointer \
                     (a reference has no integer encoding under WasmGC)",
                ));
            } else {
                emit_field_store_i64(
                    body,
                    *object,
                    *offset,
                    *value,
                    scratch0,
                    function.local(*value).ty,
                );
            }
            Ok(false)
        }
        Op::ObjectIsNone { dest, object } => {
            body.instruction(&Instruction::LocalGet(local_index(*object)));
            if gc_objects_enabled() {
                body.instruction(&Instruction::RefIsNull);
            } else {
                body.instruction(&Instruction::I64Eqz);
            }
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::ObjectClassIdSafe { dest, object } => {
            if gc_objects_enabled() {
                emit_object_class_id_safe_gc(body, function, *dest, *object)?;
            } else {
                emit_object_class_id_safe(body, *dest, *object, scratch0);
            }
            Ok(false)
        }
        Op::Call { dest, name, args } => {
            let index = func_index.get(name).copied().ok_or_else(|| {
                CompileError::codegen(format!("MIR wasm: call to unknown function '{name}'"))
            })?;
            let callee = mir_by_name.get(name.as_str());
            for (arg_index, arg) in args.iter().enumerate() {
                body.instruction(&Instruction::LocalGet(local_index(*arg)));
                if gc_objects_enabled() {
                    let arg_ty = function.local(*arg).ty;
                    let param_ty = callee.and_then(|f| f.params.get(arg_index)).map(|p| p.ty);
                    if let Some(param_ty) = param_ty {
                        gc_reject_ref_word_mix(
                            &format!("Call arg{arg_index} of '{name}'"),
                            function,
                            param_ty,
                            arg_ty,
                        )?;
                    }
                }
            }
            body.instruction(&Instruction::Call(index));
            if let Some(dest) = dest {
                if gc_objects_enabled() {
                    let dest_ty = function.local(*dest).ty;
                    let ret_ty = callee.and_then(|f| f.result);
                    if let Some(ret_ty) = ret_ty {
                        gc_reject_ref_word_mix(
                            &format!("Call result of '{name}'"),
                            function,
                            dest_ty,
                            ret_ty,
                        )?;
                    }
                    if ret_ty == Some(MirType::ObjectRef) {
                        // e.g. `seq_runtime::SPILL_LOAD_REF` always returns a
                        // generic `eq`ref (Workstream 2c: Text/Array\* also
                        // spill through the ref region); narrow it back down
                        // to the destination's own concrete WasmGC type.
                        if let Some(heap) = gc_heap_for(dest_ty) {
                            body.instruction(&Instruction::RefCastNullable(heap));
                        }
                    }
                }
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            reload_addr_homes(body, function, homes, home_base, scratch0);
            Ok(false)
        }
        Op::Abort { message } => {
            let _ = message;
            // No portable abort import in this MVP; trap via unreachable.
            body.instruction(&Instruction::Unreachable);
            Ok(true)
        }
        Op::Return { value } => {
            if let Some(value) = value {
                let val_ty = function.local(*value).ty;
                if gc_objects_enabled()
                    && let Some(result) = result_ty
                {
                    gc_reject_ref_word_mix("Return", function, result, val_ty)?;
                }
                body.instruction(&Instruction::LocalGet(local_index(*value)));
                body.instruction(&Instruction::Return);
            } else if let Some(result) = result_ty {
                emit_zero_const(body, result);
                body.instruction(&Instruction::Return);
            } else {
                body.instruction(&Instruction::Return);
            }
            Ok(true)
        }
        Op::LocalAddr { dest, local } => {
            if gc_objects_enabled() && gc_ref_home_ty(function.local(*local).ty) {
                // Phase 4-R3: a home is a linear-memory word, so it cannot hold
                // a WasmGC reference. `mir::asyncify` already rehouses the
                // address-taken locals of resumable functions in heap cells;
                // what is left here is an outlined call-by-name actual naming a
                // remote field (`dec(r.x)`), whose `env` is the address of the
                // `ref(C)` local itself.
                return Err(CompileError::codegen(format!(
                    "MIR wasm: cannot take the address of the {} local '{}' under \
                     WasmGC (a Simula reference has no linear-memory home); rewrite \
                     the call-by-name actual so the procedure is inlined, or pass \
                     the attribute by value",
                    function.local(*local).ty,
                    function.local(*local).name
                )));
            }
            let Some(offset) = homes.offset_of(*local) else {
                return Err(CompileError::codegen(
                    "MIR wasm: LocalAddr on a local without a memory home",
                ));
            };
            // Keep the home cell in sync before exposing its address.
            sync_addr_home(body, function, homes, home_base, *local, scratch0);
            body.instruction(&Instruction::LocalGet(home_base));
            body.instruction(&Instruction::I32Const(offset as i32));
            body.instruction(&Instruction::I32Add);
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::FieldAddr {
            dest,
            object,
            offset,
        } => {
            if gc_objects_enabled() {
                return Err(CompileError::codegen(
                    "MIR wasm: FieldAddr is not supported under WasmGC yet \
                     (interior pointers are not WasmGC values)",
                ));
            }
            emit_object_ptr_or_trap(body, *object, scratch0);
            body.instruction(&Instruction::LocalGet(scratch0));
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::I64Const(*offset));
            body.instruction(&Instruction::I64Add);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::LoadRefI64 { dest, ptr, offset } => {
            if gc_objects_enabled() && gc_ref_home_ty(function.local(*dest).ty) {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: cannot read a {} through a linear-memory cell \
                     under WasmGC (ObjectRef homes are ref_cell structs)",
                    function.local(*dest).ty
                )));
            }
            body.instruction(&Instruction::LocalGet(local_index(*ptr)));
            body.instruction(&Instruction::I32WrapI64);
            body.instruction(&Instruction::I64Load(wasm_encoder::MemArg {
                offset: *offset as u64,
                align: 3,
                memory_index: 0,
            }));
            if function.local(*dest).ty.is_float() {
                body.instruction(&Instruction::F64ReinterpretI64);
            }
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::StoreRefI64 { ptr, src, offset } => {
            if gc_objects_enabled() && gc_ref_home_ty(function.local(*src).ty) {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: cannot write a {} through a linear-memory cell \
                     under WasmGC (ObjectRef homes are ref_cell structs)",
                    function.local(*src).ty
                )));
            }
            body.instruction(&Instruction::LocalGet(local_index(*ptr)));
            body.instruction(&Instruction::I32WrapI64);
            body.instruction(&Instruction::LocalGet(local_index(*src)));
            if function.local(*src).ty.is_float() {
                body.instruction(&Instruction::I64ReinterpretF64);
            }
            body.instruction(&Instruction::I64Store(wasm_encoder::MemArg {
                offset: *offset as u64,
                align: 3,
                memory_index: 0,
            }));
            Ok(false)
        }
        Op::StackAlloc { dest, bytes } => {
            emit_bump_alloc(body, *bytes as i32, scratch0);
            body.instruction(&Instruction::LocalGet(scratch0));
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::HeapAlloc { dest, bytes } => {
            // Dynamic bump: size lives in an i64 local; wasm linear memory is i32.
            body.instruction(&Instruction::LocalGet(local_index(*bytes)));
            body.instruction(&Instruction::I32WrapI64);
            body.instruction(&Instruction::LocalSet(scratch1));
            body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
            body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            body.instruction(&Instruction::LocalSet(scratch0));
            emit_heap_grow_if_needed(body, scratch0, BumpSize::Dynamic(scratch1));
            body.instruction(&Instruction::I32Const(HEAP_CURSOR as i32));
            body.instruction(&Instruction::LocalGet(scratch0));
            body.instruction(&Instruction::LocalGet(scratch1));
            body.instruction(&Instruction::I32Add);
            body.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            // Zero-fill: `refs_grow` / other dynamic buffers are read as
            // root-handle slots under WasmGC; garbage words TableGet OOBs.
            emit_zero_fill_dynamic(body, scratch0, scratch1, scratch2, scratch3);
            body.instruction(&Instruction::LocalGet(scratch0));
            body.instruction(&Instruction::I64ExtendI32U);
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::FuncAddr { dest, name } => {
            let slot = funcref_slot.get(name).copied().ok_or_else(|| {
                CompileError::codegen(format!("MIR wasm: func_addr of unknown function '{name}'"))
            })?;
            body.instruction(&Instruction::I64Const(slot as i64));
            body.instruction(&Instruction::LocalSet(local_index(*dest)));
            Ok(false)
        }
        Op::CallIndirect {
            dest,
            callee,
            args,
            sig,
        } => {
            let ty = *indirect_types.get(&CallSigKey::from(sig)).ok_or_else(|| {
                CompileError::codegen("MIR wasm: missing type index for call_indirect signature")
            })?;
            for arg in args {
                body.instruction(&Instruction::LocalGet(local_index(*arg)));
            }
            body.instruction(&Instruction::LocalGet(local_index(*callee)));
            body.instruction(&Instruction::I32WrapI64);
            body.instruction(&Instruction::CallIndirect {
                type_index: ty,
                table_index: FUNCREF_TABLE,
            });
            if let Some(dest) = dest {
                body.instruction(&Instruction::LocalSet(local_index(*dest)));
            }
            reload_addr_homes(body, function, homes, home_base, scratch0);
            Ok(false)
        }
    }
}

/// Mirror `local` into its linear home word. [`AddrHomes`] only gives homes to
/// scalars (phase 4-R3), so this is always a plain `i64`/`f64` store.
pub(in crate::codegen::wasm) fn sync_addr_home(
    body: &mut Function,
    function: &MirFunction,
    homes: &AddrHomes,
    home_base: u32,
    local: LocalId,
    addr: u32,
) {
    let Some(offset) = homes.offset_of(local) else {
        return;
    };
    body.instruction(&Instruction::LocalGet(home_base));
    body.instruction(&Instruction::I32Const(offset as i32));
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::LocalSet(addr));
    body.instruction(&Instruction::LocalGet(addr));
    body.instruction(&Instruction::LocalGet(local_index(local)));
    if function.local(local).ty.is_float() {
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

/// Re-read every homed local from its linear home word after a call that could
/// have written through the address. Scalars only — see [`AddrHomes`].
pub(in crate::codegen::wasm) fn reload_addr_homes(
    body: &mut Function,
    function: &MirFunction,
    homes: &AddrHomes,
    home_base: u32,
    addr: u32,
) {
    for (index, offset) in homes.offsets.iter().enumerate() {
        let Some(offset) = offset else {
            continue;
        };
        let local = function.local(LocalId(index));
        if !matches!(
            local.ty,
            MirType::I64 | MirType::Bool | MirType::F64 | MirType::LongF64
        ) {
            continue;
        }
        body.instruction(&Instruction::LocalGet(home_base));
        body.instruction(&Instruction::I32Const(*offset as i32));
        body.instruction(&Instruction::I32Add);
        body.instruction(&Instruction::LocalSet(addr));
        body.instruction(&Instruction::LocalGet(addr));
        if local.ty.is_float() {
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
        body.instruction(&Instruction::LocalSet(local_index(LocalId(index))));
    }
}

pub(in crate::codegen::wasm) fn local_index(id: LocalId) -> u32 {
    id.0 as u32
}

/// WasmGC heap type for a concrete-ref MIR type (Text/Array\*), if any. `None`
/// for types that stay `i64` (or, for `ObjectRef`, the generic `eq` heap
/// handled separately by callers).
///
/// `Text`/`ArrayI64`/`ArrayF64`/`ArrayText` share one flag with `ObjectRef`
/// (`gc_objects_enabled`); this only decides *which* concrete WasmGC type a
/// given MIR type maps onto once that flag is on.
pub(in crate::codegen::wasm) fn gc_heap_for(ty: MirType) -> Option<HeapType> {
    match ty {
        MirType::Text => gc_ctx(|ctx| ctx.text_frame_heap()),
        MirType::ArrayI64 => gc_ctx(|ctx| crate::codegen::wasm_gc::concrete_heap(ctx.array_i64_ty)),
        MirType::ArrayF64 => gc_ctx(|ctx| crate::codegen::wasm_gc::concrete_heap(ctx.array_f64_ty)),
        MirType::ArrayText => {
            gc_ctx(|ctx| crate::codegen::wasm_gc::concrete_heap(ctx.array_text_ty))
        }
        _ => None,
    }
}

/// Whether `ty` is *some* WasmGC ref once `gc_objects_enabled()` (generic
/// `anyref` for `ObjectRef`, concrete for `Text`/`ArrayI64`/`ArrayF64`/
/// `ArrayText`) — i.e. a value with no linear-memory representation, so a
/// "home"/captured-field slot for it must be a typed WasmGC field (or a
/// `ref_cell` object) rather than a raw `i64`/`f64` word.
pub(in crate::codegen::wasm) fn gc_ref_home_ty(ty: MirType) -> bool {
    ty == MirType::ObjectRef || gc_heap_for(ty).is_some()
}

pub(in crate::codegen::wasm) fn wasm_val_type(ty: MirType) -> ValType {
    match ty {
        MirType::F64 | MirType::LongF64 => ValType::F64,
        MirType::ObjectRef if gc_objects_enabled() => crate::codegen::wasm_gc::anyref_val(),
        _ if gc_objects_enabled() => match gc_heap_for(ty) {
            Some(heap) => ValType::Ref(RefType {
                nullable: true,
                heap_type: heap,
            }),
            None => ValType::I64,
        },
        MirType::I64
        | MirType::Bool
        | MirType::Text
        | MirType::ArrayI64
        | MirType::ArrayF64
        | MirType::ArrayText
        | MirType::ObjectRef
        | MirType::RefI64
        | MirType::FuncRef => ValType::I64,
    }
}

pub(in crate::codegen::wasm) fn emit_zero_const(body: &mut Function, ty: MirType) {
    match ty {
        MirType::F64 | MirType::LongF64 => {
            body.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
        }
        MirType::ObjectRef if gc_objects_enabled() => {
            body.instruction(&Instruction::RefNull(
                crate::codegen::wasm_gc::object_ref_heap(),
            ));
        }
        _ if gc_objects_enabled() && gc_heap_for(ty).is_some() => {
            body.instruction(&Instruction::RefNull(
                gc_heap_for(ty).expect("checked above"),
            ));
        }
        MirType::I64
        | MirType::Bool
        | MirType::Text
        | MirType::ArrayI64
        | MirType::ArrayF64
        | MirType::ArrayText
        | MirType::ObjectRef
        | MirType::RefI64
        | MirType::FuncRef => {
            body.instruction(&Instruction::I64Const(0));
        }
    }
}
