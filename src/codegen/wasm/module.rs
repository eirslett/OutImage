//! Submodule of [`crate::codegen::wasm`].

use super::*;
use std::collections::HashSet;

fn import_env_helper(
    imports: &mut ImportSection,
    used: &HashSet<String>,
    next: &mut u32,
    name: &str,
    ty: u32,
) -> u32 {
    if !used.contains(name) {
        return u32::MAX;
    }
    imports.import("env", name, EntityType::Function(ty));
    let index = *next;
    *next += 1;
    index
}

fn store_u32_at(buf: &mut Vec<u8>, at: usize, value: u32) {
    if buf.len() < at + 4 {
        buf.resize(at + 4, 0);
    }
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn store_i64_at(buf: &mut Vec<u8>, at: usize, value: i64) {
    if buf.len() < at + 8 {
        buf.resize(at + 8, 0);
    }
    buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Image header + the implementation-defined blank record. The rest of the
/// 4 kB backing store stays zero in wasm memory (not encoded in the module).
fn terminal_image_bytes(base: u32, width: u32, pos: u32, out_flag: bool) -> Vec<u8> {
    let buf_off = IMAGE_OFF_BUF as usize;
    let mut bytes = vec![0u8; buf_off + width as usize];
    store_u32_at(
        &mut bytes,
        FRAME_OFF_PTR as usize,
        base + IMAGE_OFF_BUF as u32,
    );
    store_u32_at(&mut bytes, FRAME_OFF_LEN as usize, width);
    store_u32_at(&mut bytes, FRAME_OFF_POS as usize, pos);
    store_u32_at(&mut bytes, FRAME_OFF_START as usize, 1);
    store_u32_at(&mut bytes, FRAME_OFF_MAIN_LEN as usize, width);
    if out_flag {
        store_u32_at(&mut bytes, IMAGE_OFF_FLAG as usize, 1);
    }
    bytes[buf_off..buf_off + width as usize].fill(b' ');
    bytes
}

fn emit_active_data(data: &mut DataSection, addr: u32, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    data.active(0, &ConstExpr::i32_const(addr as i32), bytes.iter().copied());
}

pub(in crate::codegen::wasm) fn emit_mir(
    mir: &MirModule,
    io: WasmIo,
    debug_info: bool,
    source: &SourceFile,
    reachable: &HashSet<String>,
    used_env: &HashSet<String>,
) -> Result<(Vec<u8>, Option<SourceMap>), CompileError> {
    let want_gc_types = crate::codegen::wasm_gc::env_enabled();
    // ObjectRef / Text / Array → WasmGC whenever types are present (always,
    // except `with_force_enabled(false)` unit tests). Linear memory is WASI
    // scratch, string literals, terminal images, and scalar spill — not a
    // shadow object heap.
    let gc_objects = want_gc_types;
    GC_OBJECTS.with(|cell| cell.set(gc_objects));
    FFI_CHARSET.with(|cell| cell.set(mir.charset));

    let result = emit_mir_inner(
        mir,
        io,
        debug_info,
        source,
        want_gc_types,
        gc_objects,
        reachable,
        used_env,
    );

    GC_OBJECTS.with(|cell| cell.set(false));
    FFI_CHARSET.with(|cell| cell.set(Charset::Latin1));
    GC_CTX.with(|slot| {
        *slot.borrow_mut() = None;
    });
    result
}

pub(in crate::codegen::wasm) fn emit_mir_inner(
    mir: &MirModule,
    io: WasmIo,
    debug_info: bool,
    source: &SourceFile,
    want_gc_types: bool,
    gc_objects: bool,
    reachable: &HashSet<String>,
    used_env: &HashSet<String>,
) -> Result<(Vec<u8>, Option<SourceMap>), CompileError> {
    let need_sysin = super::used_env::needs_sysin_image(used_env);
    let need_sysout = super::used_env::needs_sysout_image(used_env);
    let need_terminals = need_sysin || need_sysout;
    let string_count = mir.strings.len();
    let extra_iovecs = usize::from(need_terminals) * 3;
    let iovec_slots = string_count + extra_iovecs;
    if IOV_BASE + (iovec_slots as u32) * 8 > IOV_LIMIT {
        return Err(CompileError::codegen(
            "MIR wasm: too many string literals for the fixed data layout",
        ));
    }

    let mut payloads = Vec::new();
    let mut iovec_ptrs_lens = Vec::with_capacity(iovec_slots);
    for text in &mir.strings {
        let ptr = TEXT_BASE + payloads.len() as u32;
        let len = text.len() as u32;
        iovec_ptrs_lens.push((ptr, len));
        payloads.extend_from_slice(text.as_bytes());
    }
    if need_terminals {
        let newline_ptr = TEXT_BASE + payloads.len() as u32;
        iovec_ptrs_lens.push((newline_ptr, 1));
        payloads.push(b'\n');
        for name in [SYSIN_FILENAME, SYSOUT_FILENAME] {
            let ptr = TEXT_BASE + payloads.len() as u32;
            iovec_ptrs_lens.push((ptr, name.len() as u32));
            payloads.extend_from_slice(name.as_bytes());
        }
    }

    // Reserve SysIn / SysOut images only when reachable I/O uses them. Wasm
    // memory is zero-filled, so unused 4 kB backing-store tails are not encoded.
    let image_bytes = IMAGE_OFF_BUF as usize + IMAGE_BUF_SIZE as usize;
    let (sysin_base, sysout_base, sysin_obj, sysout_obj, heap_base) = if need_terminals {
        let sysin_base = ((TEXT_BASE as usize + payloads.len()) + 15) & !15;
        let sysout_base = sysin_base + image_bytes;
        let sysin_obj = sysout_base + image_bytes;
        let sysout_obj = sysin_obj + TERMINAL_OBJ_SIZE as usize;
        let heap_base = sysout_obj + TERMINAL_OBJ_SIZE as usize;
        (sysin_base, sysout_base, sysin_obj, sysout_obj, heap_base)
    } else {
        let heap_base = ((TEXT_BASE as usize + payloads.len()) + 15) & !15;
        (0, 0, 0, 0, heap_base)
    };
    let mut header_bytes = Vec::new();
    if need_terminals {
        store_u32_at(
            &mut header_bytes,
            SYSIN_BASE_PTR as usize,
            sysin_base as u32,
        );
        store_u32_at(
            &mut header_bytes,
            SYSOUT_BASE_PTR as usize,
            sysout_base as u32,
        );
        store_u32_at(&mut header_bytes, SYSIN_OBJ_PTR as usize, sysin_obj as u32);
        store_u32_at(
            &mut header_bytes,
            SYSOUT_OBJ_PTR as usize,
            sysout_obj as u32,
        );
    }
    store_u32_at(&mut header_bytes, HEAP_CURSOR as usize, heap_base as u32);
    store_i64_at(&mut header_bytes, SIMSET_HEAD_CLASS_ID_PTR as usize, -1);
    for (i, (ptr, len)) in iovec_ptrs_lens.iter().enumerate() {
        let base = IOV_BASE as usize + i * 8;
        store_u32_at(&mut header_bytes, base, *ptr);
        store_u32_at(&mut header_bytes, base + 4, *len);
    }
    let sysin_image = if need_terminals {
        terminal_image_bytes(
            sysin_base as u32,
            SYSIN_LINELENGTH,
            SYSIN_LINELENGTH + 1,
            false,
        )
    } else {
        Vec::new()
    };
    let sysout_image = if need_terminals {
        terminal_image_bytes(sysout_base as u32, SYSOUT_LINELENGTH, 1, true)
    } else {
        Vec::new()
    };
    let data_segment_count = 2 + u32::from(!payloads.is_empty()) + 2 * u32::from(need_terminals);

    // Leave bump space after the static image. Components each own a spill
    // buffer, so a program with them needs considerably more room.
    let entry_point = entry_point(mir);
    let bump_space = if entry_point == seq_runtime::START {
        4 * 1024 * 1024
    } else {
        65536
    };
    let memory_pages = (heap_base + bump_space).div_ceil(65536) as u64;

    // func 0 = host write; 1 = host read (stdin, `CallInLine`); then only the
    // `env` helpers reachable MIR actually calls; then optional `sim`
    // JS-string helpers; then user `host`/`js`/`c` imports; then MIR functions.
    let env_import_count = super::used_env::env_import_count(used_env);
    let foreign_plan = collect_foreign_imports(mir, reachable)?;
    let js_text_helper_count = foreign_plan.js_text_helper_count();
    let mir_func_base = env_import_count + js_text_helper_count + foreign_plan.imports.len() as u32;
    let mut func_index: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (i, function) in mir.functions.iter().enumerate() {
        func_index.insert(function.name.clone(), mir_func_base + i as u32);
    }
    let start_index = *func_index.get(entry_point).ok_or_else(|| {
        CompileError::codegen(format!("MIR wasm: missing entry point '{entry_point}'"))
    })?;

    let mut module = Module::new();

    let mut types = TypeSection::new();
    // WasmGC composite types (text/array/class) go first so `GC_CTX` is ready
    // before any `wasm_val_type` call below — including the MIR function
    // signature loop, which needs concrete Text/Array ref types once WasmGC
    // is on (Workstream 2). Order within the type section does not matter to
    // the engine; every other index here is still computed via `types.len()`.
    let gc_types_opt = if want_gc_types {
        let mut gc_types = crate::codegen::wasm_gc::GcTypeRegistry::new();
        gc_types.populate_from_module(mir);
        let base = types.len();
        gc_types.append_to(&mut types, base);
        Some((gc_types, base))
    } else {
        None
    };
    if let Some((registry, base)) = gc_types_opt.as_ref() {
        let ctx = crate::codegen::wasm_gc::GcEmitCtx::from_registry(registry, *base, mir);
        GC_CTX.with(|slot| {
            *slot.borrow_mut() = Some(ctx);
        });
    }
    let write_ty = types.len();
    types.ty().function(
        [ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        [ValType::I32],
    );
    let f64_pow_ty = types.len();
    types
        .ty()
        .function([ValType::F64, ValType::F64], [ValType::F64]);
    let text_getint_ty = types.len();
    types.ty().function([ValType::I32], [ValType::I64]);
    let text_putint_ty = types.len();
    types.ty().function([ValType::I32, ValType::I64], []);
    let text_getfrac_ty = types.len();
    types.ty().function([ValType::I32], [ValType::I64]);
    let text_putfrac_ty = types.len();
    types
        .ty()
        .function([ValType::I32, ValType::I64, ValType::I64], []);
    let text_getreal_ty = types.len();
    types.ty().function([ValType::I32], [ValType::F64]);
    let text_putfix_ty = types.len();
    types
        .ty()
        .function([ValType::I32, ValType::F64, ValType::I64], []);
    let text_putreal_ty = types.len();
    types
        .ty()
        .function([ValType::I32, ValType::F64, ValType::I64, ValType::I64], []);
    let out_real_ty = types.len();
    types
        .ty()
        .function([ValType::F64, ValType::I64, ValType::I64, ValType::I64], []);
    let out_fix_ty = types.len();
    types
        .ty()
        .function([ValType::F64, ValType::I64, ValType::I64], []);
    let out_frac_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I64, ValType::I64], []);
    let f64_unary_ty = types.len();
    types.ty().function([ValType::F64], [ValType::F64]);
    let randint_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I64, ValType::I64], [ValType::I64]);
    let uniform_ty = types.len();
    types
        .ty()
        .function([ValType::F64, ValType::F64, ValType::I64], [ValType::F64]);
    let sysout_write_ty = types.len();
    types.ty().function([ValType::I32, ValType::I32], []);
    let sysout_flush_ty = types.len();
    types.ty().function([ValType::I32], []);
    let basicio_obj_ty = types.len();
    types.ty().function([ValType::I64], [ValType::I64]);
    let basicio_obj_void_ty = types.len();
    types.ty().function([ValType::I64], []);
    let basicio_register_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I32, ValType::I64], []);
    let basicio_open_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I64], [ValType::I64]);
    let basicio_out_text_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I32, ValType::I32], []);
    let basicio_out_char_ty = types.len();
    types.ty().function([ValType::I64, ValType::I64], []);
    let basicio_set_image_ty = types.len();
    types.ty().function([ValType::I64, ValType::I64], []);
    let basicio_setpos_ty = types.len();
    types.ty().function([ValType::I64, ValType::I64], []);
    let basicio_image_ty = types.len();
    types.ty().function([ValType::I64, ValType::I32], []);
    let basicio_inreal_ty = types.len();
    types.ty().function([ValType::I64], [ValType::F64]);
    let basicio_intext_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I64], [ValType::I64]);
    let basicio_out_real_ty = types.len();
    types.ty().function(
        [
            ValType::I64,
            ValType::F64,
            ValType::I64,
            ValType::I64,
            ValType::I64,
        ],
        [],
    );
    let basicio_out_fix_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::F64, ValType::I64, ValType::I64], []);
    let basicio_out_frac_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I64, ValType::I64, ValType::I64], []);
    let basicio_out_int_ty = types.len();
    types
        .ty()
        .function([ValType::I64, ValType::I64, ValType::I64], []);
    let error_ty = types.len();
    types.ty().function([ValType::I32], []);
    let basicio_open_byte_ty = types.len();
    types.ty().function([ValType::I64], [ValType::I64]);
    let basicio_out_byte_ty = types.len();
    types.ty().function([ValType::I64, ValType::I64], []);
    let basicio_locate_ty = types.len();
    types.ty().function([ValType::I64, ValType::I64], []);
    let negexp_ty = types.len();
    types
        .ty()
        .function([ValType::F64, ValType::I64], [ValType::F64]);
    let draw_ty = types.len();
    types
        .ty()
        .function([ValType::F64, ValType::I64], [ValType::I64]);
    let mut type_indices = Vec::with_capacity(mir.functions.len());
    for function in &mir.functions {
        let ty = types.len();
        let params: Vec<ValType> = function
            .params
            .iter()
            .map(|p| wasm_val_type(p.ty))
            .collect();
        if let Some(result) = function.result {
            types.ty().function(params, [wasm_val_type(result)]);
        } else {
            types.ty().function(params, []);
        }
        type_indices.push(ty);
    }
    let mut foreign_type_indices = Vec::with_capacity(foreign_plan.imports.len());
    for import in &foreign_plan.imports {
        let ty = types.len();
        let params = foreign_wasm_params(&import.abi);
        types
            .ty()
            .function(params, foreign_wasm_results(&import.abi));
        foreign_type_indices.push(ty);
    }
    let js_text_helper_types = if js_text_helper_count > 0 {
        let text_from_bytes = types.len();
        types.ty().function(
            [ValType::I32, ValType::I32, ValType::I32],
            [ValType::EXTERNREF],
        );
        let bytes_from_text = types.len();
        types.ty().function(
            [ValType::EXTERNREF, ValType::I32],
            [ValType::I32, ValType::I32],
        );
        Some((text_from_bytes, bytes_from_text))
    } else {
        None
    };
    let gc_terminal_init_ty = if gc_objects {
        let ty = types.len();
        types.ty().function([], []);
        Some(ty)
    } else {
        None
    };
    let mut user_export_types = Vec::new();
    let mut user_exports: Vec<(String, u32)> = Vec::new();
    for (i, function) in mir.functions.iter().enumerate() {
        let Some(name) = function.wasm_export_name() else {
            continue;
        };
        if name == "_start" || name == "memory" {
            return Err(CompileError::codegen(format!(
                "cannot export '{name}': reserved wasm export name"
            )));
        }
        let ty = types.len();
        let mut params = Vec::new();
        for param in &function.params {
            params.push(export_wasm_val(param.ty)?);
        }
        if let Some(result) = function.result {
            types.ty().function(params, [export_wasm_val(result)?]);
        } else {
            types.ty().function(params, []);
        }
        user_export_types.push(ty);
        user_exports.push((name, mir_func_base + i as u32));
    }
    // Extra signatures used by `CallIndirect` (may duplicate function types; wasm allows that).
    let mut indirect_type_cache: std::collections::HashMap<CallSigKey, u32> =
        std::collections::HashMap::new();
    for function in &mir.functions {
        for block in &function.blocks {
            for spanned in &block.ops {
                if let Op::CallIndirect { sig, .. } = &spanned.op {
                    let key = CallSigKey::from(sig);
                    if indirect_type_cache.contains_key(&key) {
                        continue;
                    }
                    let ty = types.len();
                    let params: Vec<ValType> =
                        sig.params.iter().copied().map(wasm_val_type).collect();
                    if let Some(result) = sig.result {
                        types.ty().function(params, [wasm_val_type(result)]);
                    } else {
                        types.ty().function(params, []);
                    }
                    indirect_type_cache.insert(key, ty);
                }
            }
        }
    }
    module.section(&types);

    let mut imports = ImportSection::new();
    match io {
        WasmIo::Wasi => {
            imports.import(
                "wasi_snapshot_preview1",
                "fd_write",
                EntityType::Function(write_ty),
            );
            // Same `(fd, iovs_ptr, iovs_len, nread_ptr) -> errno` ABI as
            // `fd_write`; Node's built-in WASI already implements this
            // against the real stdin fd, so `run_wasi.mjs` needs no polyfill.
            imports.import(
                "wasi_snapshot_preview1",
                "fd_read",
                EntityType::Function(write_ty),
            );
        }
        WasmIo::Browser => {
            imports.import("env", "fd_write", EntityType::Function(write_ty));
            imports.import("env", "fd_read", EntityType::Function(write_ty));
        }
    }
    let mut next_env = 2u32;
    let host = HostImports {
        f64_pow: import_env_helper(&mut imports, used_env, &mut next_env, "f64_pow", f64_pow_ty),
        text_getint: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_getint",
            text_getint_ty,
        ),
        text_putint: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_putint",
            text_putint_ty,
        ),
        text_getfrac: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_getfrac",
            text_getfrac_ty,
        ),
        text_putfrac: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_putfrac",
            text_putfrac_ty,
        ),
        text_getreal: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_getreal",
            text_getreal_ty,
        ),
        text_putfix: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_putfix",
            text_putfix_ty,
        ),
        text_putreal: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "text_putreal",
            text_putreal_ty,
        ),
        out_real: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "out_real",
            out_real_ty,
        ),
        out_fix: import_env_helper(&mut imports, used_env, &mut next_env, "out_fix", out_fix_ty),
        out_frac: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "out_frac",
            out_frac_ty,
        ),
        ln: import_env_helper(&mut imports, used_env, &mut next_env, "ln", f64_unary_ty),
        exp: import_env_helper(&mut imports, used_env, &mut next_env, "exp", f64_unary_ty),
        sin: import_env_helper(&mut imports, used_env, &mut next_env, "sin", f64_unary_ty),
        cos: import_env_helper(&mut imports, used_env, &mut next_env, "cos", f64_unary_ty),
        arctan: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "arctan",
            f64_unary_ty,
        ),
        addepsilon: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "addepsilon",
            f64_unary_ty,
        ),
        subepsilon: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "subepsilon",
            f64_unary_ty,
        ),
        randint: import_env_helper(&mut imports, used_env, &mut next_env, "randint", randint_ty),
        uniform: import_env_helper(&mut imports, used_env, &mut next_env, "uniform", uniform_ty),
        sysout_write: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "sysout_write",
            sysout_write_ty,
        ),
        sysout_flush: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "sysout_flush",
            sysout_flush_ty,
        ),
        basicio_register: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_register",
            basicio_register_ty,
        ),
        basicio_open: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_open",
            basicio_open_ty,
        ),
        basicio_close: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_close",
            basicio_obj_ty,
        ),
        basicio_isopen: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_isopen",
            basicio_obj_ty,
        ),
        basicio_out_text: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_text",
            basicio_out_text_ty,
        ),
        basicio_out_char: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_char",
            basicio_out_char_ty,
        ),
        basicio_out_image: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_image",
            basicio_obj_void_ty,
        ),
        basicio_break_out_image: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_break_out_image",
            basicio_obj_void_ty,
        ),
        basicio_in_image: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_in_image",
            basicio_obj_void_ty,
        ),
        basicio_in_char: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_in_char",
            basicio_obj_ty,
        ),
        basicio_endfile: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_endfile",
            basicio_obj_ty,
        ),
        basicio_image: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_image",
            basicio_image_ty,
        ),
        basicio_set_image: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_set_image",
            basicio_set_image_ty,
        ),
        basicio_pos: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_pos",
            basicio_obj_ty,
        ),
        basicio_length: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_length",
            basicio_obj_ty,
        ),
        basicio_setpos: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_setpos",
            basicio_setpos_ty,
        ),
        basicio_line: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_line",
            basicio_obj_ty,
        ),
        basicio_filename: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_filename",
            basicio_image_ty,
        ),
        basicio_lastitem: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_lastitem",
            basicio_obj_ty,
        ),
        basicio_inint: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_inint",
            basicio_obj_ty,
        ),
        basicio_inreal: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_inreal",
            basicio_inreal_ty,
        ),
        basicio_infrac: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_infrac",
            basicio_obj_ty,
        ),
        basicio_intext: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_intext",
            basicio_intext_ty,
        ),
        basicio_out_real: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_real",
            basicio_out_real_ty,
        ),
        basicio_out_fix: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_fix",
            basicio_out_fix_ty,
        ),
        basicio_out_frac: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_frac",
            basicio_out_frac_ty,
        ),
        basicio_out_int: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_int",
            basicio_out_int_ty,
        ),
        error: import_env_helper(&mut imports, used_env, &mut next_env, "error", error_ty),
        basicio_open_byte: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_open_byte",
            basicio_open_byte_ty,
        ),
        basicio_in_byte: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_in_byte",
            basicio_obj_ty,
        ),
        basicio_out_byte: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_out_byte",
            basicio_out_byte_ty,
        ),
        basicio_locate: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_locate",
            basicio_locate_ty,
        ),
        basicio_location: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_location",
            basicio_obj_ty,
        ),
        basicio_lastloc: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_lastloc",
            basicio_obj_ty,
        ),
        negexp: import_env_helper(&mut imports, used_env, &mut next_env, "negexp", negexp_ty),
        normal: import_env_helper(&mut imports, used_env, &mut next_env, "normal", uniform_ty),
        draw: import_env_helper(&mut imports, used_env, &mut next_env, "draw", draw_ty),
        basicio_setaccess: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_setaccess",
            basicio_open_ty,
        ),
        basicio_eject: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_eject",
            basicio_setpos_ty,
        ),
        basicio_linesperpage: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_linesperpage",
            basicio_open_ty,
        ),
        basicio_inrecord: import_env_helper(
            &mut imports,
            used_env,
            &mut next_env,
            "basicio_inrecord",
            basicio_obj_ty,
        ),
    };
    debug_assert_eq!(next_env, env_import_count);
    let js_text_helpers =
        if let Some((text_from_bytes_ty, bytes_from_text_ty)) = js_text_helper_types {
            imports.import(
                "sim",
                "text_from_bytes",
                EntityType::Function(text_from_bytes_ty),
            );
            imports.import(
                "sim",
                "bytes_from_text",
                EntityType::Function(bytes_from_text_ty),
            );
            Some(JsTextHelpers {
                text_from_bytes: env_import_count,
                bytes_from_text: env_import_count + 1,
            })
        } else {
            None
        };
    let mut foreign_func_index: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for (ordinal, import) in foreign_plan.imports.iter().enumerate() {
        let func_idx = env_import_count + js_text_helper_count + ordinal as u32;
        imports.import(
            &import.module,
            &import.name,
            EntityType::Function(foreign_type_indices[ordinal]),
        );
        for (mir_name, &index) in &foreign_plan.thunk_of {
            if index == ordinal {
                foreign_func_index.insert(mir_name.clone(), func_idx);
            }
        }
    }
    module.section(&imports);

    let mut functions = FunctionSection::new();
    for ty in &type_indices {
        functions.function(*ty);
    }
    let gc_terminal_init_func = if let Some(ty) = gc_terminal_init_ty {
        let idx = mir_func_base + mir.functions.len() as u32;
        functions.function(ty);
        Some(idx)
    } else {
        None
    };
    let user_export_base =
        mir_func_base + mir.functions.len() as u32 + u32::from(gc_terminal_init_func.is_some());
    for ty in &user_export_types {
        functions.function(*ty);
    }
    module.section(&functions);

    // Funcref table: slot i holds MIR function i (absolute index mir_func_base+i).
    let mir_func_count = mir.functions.len() as u32;
    let mut tables = TableSection::new();
    tables.table(TableType {
        element_type: RefType::FUNCREF,
        minimum: u64::from(mir_func_count.max(1)),
        maximum: Some(u64::from(mir_func_count.max(1))),
        table64: false,
        shared: false,
    });
    if gc_objects {
        // BASICIO host identities: slots below `BASICIO_HOST_ID_FIRST_DISK`
        // stay empty (see [`BASICIO_HANDLE_TABLE`]).
        tables.table(TableType {
            element_type: RefType::EQREF,
            minimum: BASICIO_HOST_ID_FIRST_DISK as u64,
            maximum: None,
            table64: false,
            shared: false,
        });
    }
    module.section(&tables);

    let mut memory = MemorySection::new();
    memory.memory(MemoryType {
        minimum: memory_pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&memory);

    if gc_objects {
        // The terminal singletons' roots (see [`GLOBAL_SYSIN`]); the start
        // function fills them in before `_start` can observe them.
        let mut globals = GlobalSection::new();
        let terminal_global = GlobalType {
            val_type: crate::codegen::wasm_gc::anyref_val(),
            mutable: true,
            shared: false,
        };
        let null_ref = ConstExpr::ref_null(crate::codegen::wasm_gc::object_ref_heap());
        globals.global(terminal_global, &null_ref);
        globals.global(terminal_global, &null_ref);
        // The sequencing GC registry (see [`GLOBAL_SEQ_GC_REGISTRY`]); it is
        // allocated lazily by the first `SEQ_GC_SLOT_NEW`.
        let registry_ty =
            gc_ctx(|ctx| ctx.seq_gc_registry_ty).expect("GC_CTX set whenever gc_objects_enabled()");
        globals.global(
            GlobalType {
                val_type: crate::codegen::wasm_gc::concrete_ref_null(registry_ty),
                mutable: true,
                shared: false,
            },
            &ConstExpr::ref_null(crate::codegen::wasm_gc::concrete_heap(registry_ty)),
        );
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(0),
        );
        // Chapter 12's CURRENT / RUNNING / MAIN (see [`GLOBAL_SIM_CURRENT`]);
        // `__simrt_sim_begin` gives them their values.
        globals.global(terminal_global, &null_ref);
        globals.global(terminal_global, &null_ref);
        globals.global(terminal_global, &null_ref);
        let notice_procs_ty = gc_ctx(|ctx| ctx.sim_notice_procs_ty)
            .expect("GC_CTX set whenever gc_objects_enabled()");
        globals.global(
            GlobalType {
                val_type: crate::codegen::wasm_gc::concrete_ref_null(notice_procs_ty),
                mutable: true,
                shared: false,
            },
            &ConstExpr::ref_null(crate::codegen::wasm_gc::concrete_heap(notice_procs_ty)),
        );
        module.section(&globals);
    }

    let mut exports = ExportSection::new();
    // Wasm is always a hybrid: the program entry plus public procedure
    // exports. `--crate-type` is a native linker knob (exe vs dylib).
    let user_names: std::collections::HashSet<&str> =
        user_exports.iter().map(|(name, _)| name.as_str()).collect();
    exports.export("_start", ExportKind::Func, start_index);
    if !user_names.contains("step") {
        exports.export("step", ExportKind::Func, start_index);
    }
    if !user_names.contains("now")
        && let Some(&idx) = func_index.get(crate::mir::sim_runtime::SIM_TIME)
    {
        exports.export("now", ExportKind::Func, idx);
    }
    exports.export("memory", ExportKind::Memory, 0);
    for (ordinal, (name, _)) in user_exports.iter().enumerate() {
        exports.export(name, ExportKind::Func, user_export_base + ordinal as u32);
    }
    module.section(&exports);

    if let Some(init_idx) = gc_terminal_init_func {
        module.section(&StartSection {
            function_index: init_idx,
        });
    }

    let table_funcs: Vec<u32> = (0..mir_func_count).map(|i| mir_func_base + i).collect();
    let mut elements = ElementSection::new();
    elements.active(
        None,
        &ConstExpr::i32_const(0),
        Elements::Functions(Cow::Owned(table_funcs)),
    );
    module.section(&elements);

    // WasmGC `array.new_data`/`array.init_data` (used by `emit_text_from_literal_gc`
    // to build a text's `(array i8)` straight out of the module's literal data
    // segment) index into the *data* index space, and validators need to know
    // that index space's size while validating the Code section — hence the
    // DataCount section must precede Code in the binary even though the actual
    // Data section (with the bytes) is written after it.
    //
    // Index 0: compact header / iovecs (seq/sim state stays zero in memory).
    // Index 1: passive literal bytes for `array.new_data` (must stay 1).
    // Later indices: TEXT_BASE payloads and/or terminal image headers.
    module.section(&DataCountSection {
        count: data_segment_count,
    });

    // name -> table slot (ordinal among MIR functions)
    let mut funcref_slot: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (i, function) in mir.functions.iter().enumerate() {
        funcref_slot.insert(function.name.clone(), i as u32);
    }

    let mut code = CodeSection::new();
    let mir_by_name: std::collections::HashMap<&str, &MirFunction> =
        mir.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut body_markers: Vec<Vec<BodyMarker>> = Vec::with_capacity(mir.functions.len());
    for function in &mir.functions {
        let mut markers = Vec::new();
        // The ref-spine and Simulation-state helpers have no MIR-expressible
        // body on wasm: their storage is a WasmGC array / global, not linear
        // memory (Phases 4-R2 and 4-R4).
        let body = if !reachable.contains(&function.name) {
            emit_trap_stub()
        } else if function.foreign.is_some() {
            let import_index =
                foreign_func_index
                    .get(&function.name)
                    .copied()
                    .ok_or_else(|| {
                        CompileError::codegen(format!(
                            "internal error: missing wasm import for foreign '{}'",
                            function.name
                        ))
                    })?;
            emit_foreign_thunk(function, import_index, js_text_helpers)?
        } else {
            match emit_ref_spine_body(&function.name).or_else(|| emit_sim_ref_body(&function.name))
            {
                Some(body) => body,
                None => emit_function_body(
                    function,
                    string_count,
                    &iovec_ptrs_lens,
                    &func_index,
                    &funcref_slot,
                    &indirect_type_cache,
                    &mir_by_name,
                    host,
                    debug_info.then_some(&mut markers),
                )?,
            }
        };
        code.function(&body);
        body_markers.push(markers);
    }
    if gc_terminal_init_func.is_some() {
        code.function(&emit_gc_terminal_init_body(mir)?);
    }
    for (_, mir_index) in &user_exports {
        let function = &mir.functions[(mir_index - mir_func_base) as usize];
        code.function(&emit_export_thunk(function, *mir_index)?);
    }
    module.section(&code);

    let mut data = DataSection::new();
    emit_active_data(&mut data, 0, &header_bytes);
    // Segment 1: a *passive* copy of the literal-string payload bytes, purely so
    // `emit_text_from_literal_gc`'s `array.new_data` has something to read.
    // V8 traps with "data segment out of bounds" when `array.new_data` targets
    // an *active* segment — only passive segments work at runtime.
    data.passive(payloads.iter().copied());
    if !payloads.is_empty() {
        emit_active_data(&mut data, TEXT_BASE, &payloads);
    }
    if need_terminals {
        emit_active_data(&mut data, sysin_base as u32, &sysin_image);
        emit_active_data(&mut data, sysout_base as u32, &sysout_image);
    }
    module.section(&data);

    let mut bytes = module.finish();
    if !debug_info {
        return Ok((bytes, None));
    }

    // The GC terminal-init body is appended after the MIR functions and has no
    // MIR function (so no DWARF entry); it still occupies a Code section slot.
    let body_count =
        mir.functions.len() + usize::from(gc_terminal_init_func.is_some()) + user_exports.len();
    let layout = code_section_layout(&bytes, body_count)?;
    let mut mappings = Vec::new();
    let mut dwarf_functions = Vec::with_capacity(mir.functions.len());
    for (fn_index, (function, markers)) in mir.functions.iter().zip(body_markers).enumerate() {
        let (body_start, body_size) = layout.bodies[fn_index];
        // DW_AT_low_pc / .debug_line addresses are Code-section-relative, per
        // the WebAssembly DWARF tool convention (see `CodeSectionLayout`).
        let low_pc = body_start - layout.section_start;
        let high_pc = low_pc + body_size;
        let mut rows = Vec::with_capacity(markers.len());
        for marker in &markers {
            let (line, column) = span_to_line_col(&source.text, marker.span_start);
            mappings.push(Mapping {
                op: marker.op,
                function: function.name.clone(),
                block: marker.block,
                span: [marker.span_start, marker.span_end],
                line,
                column,
                wasm_offset: Some(body_start + marker.body_offset),
            });
            rows.push((low_pc + marker.body_offset, line as u64, column as u64));
        }
        let default_line = rows.first().map(|(_, line, _)| *line).unwrap_or(1);
        let mut dwarf_locals = Vec::new();
        for (index, local) in function.params.iter().enumerate() {
            if wasm_dwarf::is_user_local_name(&local.name) {
                dwarf_locals.push(wasm_dwarf::WasmLocalDebug {
                    name: local.name.clone(),
                    ty: local.ty,
                    class_qual: local.class_qual.clone(),
                    wasm_local: index as u32,
                    is_param: true,
                });
            }
        }
        for (index, local) in function.locals.iter().enumerate() {
            if wasm_dwarf::is_user_local_name(&local.name) {
                dwarf_locals.push(wasm_dwarf::WasmLocalDebug {
                    name: local.name.clone(),
                    ty: local.ty,
                    class_qual: local.class_qual.clone(),
                    wasm_local: (function.params.len() + index) as u32,
                    is_param: false,
                });
            }
        }
        dwarf_functions.push(wasm_dwarf::WasmFunctionDebug {
            name: function.name.clone(),
            low_pc,
            high_pc,
            default_line,
            rows,
            locals: dwarf_locals,
        });
    }
    let map = SourceMap {
        version: 1,
        file: source.name.clone(),
        mappings,
    };

    for (name, payload) in
        wasm_dwarf::build_debug_sections(source, &dwarf_functions, &mir.class_layouts)
    {
        append_custom_section(&mut bytes, &name, &payload);
    }

    Ok((bytes, Some(map)))
}

#[derive(Clone, Debug)]
pub(in crate::codegen::wasm) struct BodyMarker {
    pub(in crate::codegen::wasm) op: usize,
    pub(in crate::codegen::wasm) block: usize,
    pub(in crate::codegen::wasm) span_start: usize,
    pub(in crate::codegen::wasm) span_end: usize,
    /// Offset within the wasm-encoder `Function` body bytes (locals + ops).
    pub(in crate::codegen::wasm) body_offset: u32,
}

#[derive(Clone, Copy)]
pub(in crate::codegen::wasm) struct HostImports {
    pub(in crate::codegen::wasm) f64_pow: u32,
    pub(in crate::codegen::wasm) text_getint: u32,
    pub(in crate::codegen::wasm) text_putint: u32,
    pub(in crate::codegen::wasm) text_getfrac: u32,
    pub(in crate::codegen::wasm) text_putfrac: u32,
    pub(in crate::codegen::wasm) text_getreal: u32,
    pub(in crate::codegen::wasm) text_putfix: u32,
    pub(in crate::codegen::wasm) text_putreal: u32,
    pub(in crate::codegen::wasm) out_real: u32,
    pub(in crate::codegen::wasm) out_fix: u32,
    pub(in crate::codegen::wasm) out_frac: u32,
    pub(in crate::codegen::wasm) ln: u32,
    pub(in crate::codegen::wasm) exp: u32,
    pub(in crate::codegen::wasm) sin: u32,
    pub(in crate::codegen::wasm) cos: u32,
    pub(in crate::codegen::wasm) arctan: u32,
    pub(in crate::codegen::wasm) addepsilon: u32,
    pub(in crate::codegen::wasm) subepsilon: u32,
    pub(in crate::codegen::wasm) randint: u32,
    pub(in crate::codegen::wasm) uniform: u32,
    pub(in crate::codegen::wasm) sysout_write: u32,
    pub(in crate::codegen::wasm) sysout_flush: u32,
    pub(in crate::codegen::wasm) basicio_register: u32,
    pub(in crate::codegen::wasm) basicio_open: u32,
    pub(in crate::codegen::wasm) basicio_close: u32,
    pub(in crate::codegen::wasm) basicio_isopen: u32,
    pub(in crate::codegen::wasm) basicio_out_text: u32,
    pub(in crate::codegen::wasm) basicio_out_char: u32,
    pub(in crate::codegen::wasm) basicio_out_image: u32,
    pub(in crate::codegen::wasm) basicio_break_out_image: u32,
    pub(in crate::codegen::wasm) basicio_in_image: u32,
    pub(in crate::codegen::wasm) basicio_in_char: u32,
    pub(in crate::codegen::wasm) basicio_endfile: u32,
    pub(in crate::codegen::wasm) basicio_image: u32,
    pub(in crate::codegen::wasm) basicio_set_image: u32,
    pub(in crate::codegen::wasm) basicio_pos: u32,
    pub(in crate::codegen::wasm) basicio_length: u32,
    pub(in crate::codegen::wasm) basicio_setpos: u32,
    pub(in crate::codegen::wasm) basicio_line: u32,
    pub(in crate::codegen::wasm) basicio_filename: u32,
    pub(in crate::codegen::wasm) basicio_lastitem: u32,
    pub(in crate::codegen::wasm) basicio_inint: u32,
    pub(in crate::codegen::wasm) basicio_inreal: u32,
    pub(in crate::codegen::wasm) basicio_infrac: u32,
    pub(in crate::codegen::wasm) basicio_intext: u32,
    pub(in crate::codegen::wasm) basicio_out_real: u32,
    pub(in crate::codegen::wasm) basicio_out_fix: u32,
    pub(in crate::codegen::wasm) basicio_out_frac: u32,
    pub(in crate::codegen::wasm) basicio_out_int: u32,
    pub(in crate::codegen::wasm) error: u32,
    pub(in crate::codegen::wasm) basicio_open_byte: u32,
    pub(in crate::codegen::wasm) basicio_in_byte: u32,
    pub(in crate::codegen::wasm) basicio_out_byte: u32,
    pub(in crate::codegen::wasm) basicio_locate: u32,
    pub(in crate::codegen::wasm) basicio_location: u32,
    pub(in crate::codegen::wasm) basicio_lastloc: u32,
    pub(in crate::codegen::wasm) negexp: u32,
    pub(in crate::codegen::wasm) normal: u32,
    pub(in crate::codegen::wasm) draw: u32,
    pub(in crate::codegen::wasm) basicio_setaccess: u32,
    pub(in crate::codegen::wasm) basicio_eject: u32,
    pub(in crate::codegen::wasm) basicio_linesperpage: u32,
    pub(in crate::codegen::wasm) basicio_inrecord: u32,
}

pub(in crate::codegen::wasm) fn append_source_mapping_url(module_bytes: &mut Vec<u8>, url: &str) {
    append_custom_section(module_bytes, "sourceMappingURL", url.as_bytes());
}

/// Appends a wasm custom section (id 0) with the given `name`/`data` to an
/// already-finished module's byte stream. Used for `sourceMappingURL` and for
/// the `.debug_*` DWARF sections emitted under `-g`.
pub(in crate::codegen::wasm) fn append_custom_section(
    module_bytes: &mut Vec<u8>,
    name: &str,
    data: &[u8],
) {
    let mut section = Module::new();
    section.section(&CustomSection {
        name: std::borrow::Cow::Borrowed(name),
        data: std::borrow::Cow::Borrowed(data),
    });
    // `Module::new().section(...).finish()` still writes the wasm magic/version.
    // Strip the 8-byte header and append only the custom section bytes.
    let encoded = section.finish();
    module_bytes.extend_from_slice(&encoded[8..]);
}

pub(in crate::codegen::wasm) fn read_leb128_u32(
    data: &[u8],
    pos: &mut usize,
) -> Result<u32, CompileError> {
    let mut result = 0u32;
    let mut shift = 0u32;
    loop {
        if *pos >= data.len() {
            return Err(CompileError::codegen(
                "MIR wasm: truncated leb128 while locating Code section",
            ));
        }
        let byte = data[*pos];
        *pos += 1;
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return Err(CompileError::codegen(
                "MIR wasm: invalid leb128 while locating Code section",
            ));
        }
    }
    Ok(result)
}

/// Layout of the Code section inside a finished wasm module: the reference
/// point WebAssembly DWARF code addresses are relative to
/// (<https://github.com/WebAssembly/tool-conventions/blob/main/Dwarf.md>),
/// plus each MIR function's absolute `(file_offset, size)` body range.
pub(in crate::codegen::wasm) struct CodeSectionLayout {
    /// File offset of the start of the Code section's *content* (i.e. right
    /// where the function-count `vec` length is encoded, immediately after
    /// the section's id + byte-length header). WebAssembly DWARF code
    /// addresses (`DW_AT_low_pc`, `.debug_line` instruction pointers, ...)
    /// are byte offsets relative to this point.
    pub(in crate::codegen::wasm) section_start: u32,
    /// `(file_offset, size)` of each MIR function's raw body (locals +
    /// instructions) inside the finished module, in module order.
    pub(in crate::codegen::wasm) bodies: Vec<(u32, u32)>,
}

/// Locates the Code section of a finished module and returns its layout.
/// Import functions are not present in the Code section. `body_count` counts
/// every emitted body, including any synthetic ones after the MIR functions.
pub(in crate::codegen::wasm) fn code_section_layout(
    module: &[u8],
    body_count: usize,
) -> Result<CodeSectionLayout, CompileError> {
    if module.len() < 8 || &module[..4] != b"\0asm" {
        return Err(CompileError::codegen(
            "MIR wasm: invalid module while locating Code section",
        ));
    }
    let mut pos = 8usize;
    while pos < module.len() {
        let section_id = module[pos];
        pos += 1;
        let section_len = read_leb128_u32(module, &mut pos)? as usize;
        let section_end = pos + section_len;
        if section_end > module.len() {
            return Err(CompileError::codegen(
                "MIR wasm: truncated section while locating Code section",
            ));
        }
        if section_id == 10 {
            // Code section payload (the DWARF-relative origin) starts at `pos`.
            let section_start = pos as u32;
            let mut cursor = pos;
            let count = read_leb128_u32(module, &mut cursor)? as usize;
            if count != body_count {
                return Err(CompileError::codegen(format!(
                    "MIR wasm: Code section has {count} functions, expected {body_count}"
                )));
            }
            let mut bodies = Vec::with_capacity(count);
            for _ in 0..count {
                let size = read_leb128_u32(module, &mut cursor)? as usize;
                bodies.push((cursor as u32, size as u32));
                cursor += size;
                if cursor > section_end {
                    return Err(CompileError::codegen(
                        "MIR wasm: function body overruns Code section",
                    ));
                }
            }
            return Ok(CodeSectionLayout {
                section_start,
                bodies,
            });
        }
        pos = section_end;
    }
    Err(CompileError::codegen(
        "MIR wasm: missing Code section for source map PCs",
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::codegen::wasm) struct CallSigKey {
    pub(in crate::codegen::wasm) params: Vec<MirType>,
    pub(in crate::codegen::wasm) result: Option<MirType>,
}

impl From<&CallSig> for CallSigKey {
    fn from(sig: &CallSig) -> Self {
        Self {
            params: sig.params.clone(),
            result: sig.result,
        }
    }
}
