//! Foreign import thunks for wasm.

use super::*;
use crate::mir::{ForeignAbi, ForeignKind, ForeignType};

/// `simula.text_from_bytes` + `simula.bytes_from_text`, imported only when a
/// JS/Host `text` crosses the wasm edge as a JS `String` (`externref`).
pub(in crate::codegen::wasm) const JS_TEXT_HELPER_COUNT: u32 = 2;

/// Compiler-owned imports that turn linear scratch into a JS string (and back).
/// Application `JS` / `Host` functions never see `(ptr, len)`.
#[derive(Clone, Copy)]
pub(in crate::codegen::wasm) struct JsTextHelpers {
    pub text_from_bytes: u32,
    pub bytes_from_text: u32,
}

#[derive(Clone, Debug)]
pub(in crate::codegen::wasm) struct ForeignWasmImport {
    pub module: String,
    pub name: String,
    pub abi: ForeignAbi,
}

pub(in crate::codegen::wasm) struct ForeignImportPlan {
    pub imports: Vec<ForeignWasmImport>,
    /// MIR function name → index into [`Self::imports`].
    pub thunk_of: std::collections::HashMap<String, usize>,
}

pub(in crate::codegen::wasm) fn collect_foreign_imports(
    mir: &MirModule,
    reachable: &std::collections::HashSet<String>,
) -> Result<ForeignImportPlan, CompileError> {
    let mut imports = Vec::new();
    let mut thunk_of = std::collections::HashMap::new();
    let mut by_key = std::collections::HashMap::new();
    for function in &mir.functions {
        if !reachable.contains(&function.name) {
            continue;
        }
        let Some(abi) = &function.foreign else {
            continue;
        };
        if (abi.params.iter().any(|ty| ty.is_handle())
            || abi.result.is_some_and(ForeignType::is_handle))
            && !gc_objects_enabled()
        {
            return Err(CompileError::codegen(
                "ref handles at a wasm boundary require WasmGC",
            ));
        }
        let (module, name) = abi.wasm_import();
        let key = (module.clone(), name.clone());
        let index = if let Some(&index) = by_key.get(&key) {
            index
        } else {
            let index = imports.len();
            by_key.insert(key, index);
            imports.push(ForeignWasmImport {
                module,
                name,
                abi: abi.clone(),
            });
            index
        };
        thunk_of.insert(function.name.clone(), index);
    }
    Ok(ForeignImportPlan { imports, thunk_of })
}

impl ForeignImportPlan {
    pub(in crate::codegen::wasm) fn js_text_helper_count(&self) -> u32 {
        if self
            .imports
            .iter()
            .any(|import| uses_js_string_text(&import.abi))
        {
            JS_TEXT_HELPER_COUNT
        } else {
            0
        }
    }
}

/// Wasm JS/Host `text` is a JS `String` (`externref`). C stays `(ptr, len)`.
fn wasm_text_is_js_string(kind: ForeignKind) -> bool {
    matches!(kind, ForeignKind::Js | ForeignKind::Host)
}

fn uses_js_string_text(abi: &ForeignAbi) -> bool {
    wasm_text_is_js_string(abi.kind)
        && (abi.params.iter().any(|ty| ty.is_text())
            || abi.result.is_some_and(ForeignType::is_text))
}

pub(in crate::codegen::wasm) fn foreign_wasm_val(ty: ForeignType) -> ValType {
    match ty {
        ForeignType::F64 => ValType::F64,
        ForeignType::I64 => ValType::I64,
        ForeignType::Bool | ForeignType::Char | ForeignType::TextCopy => ValType::I32,
        ForeignType::ObjectHandle => {
            if gc_objects_enabled() {
                crate::codegen::wasm_gc::anyref_val()
            } else {
                ValType::I64
            }
        }
    }
}

pub(in crate::codegen::wasm) fn foreign_wasm_results(abi: &ForeignAbi) -> Vec<ValType> {
    match abi.result {
        Some(ForeignType::TextCopy) if wasm_text_is_js_string(abi.kind) => {
            vec![ValType::EXTERNREF]
        }
        Some(ForeignType::TextCopy) => vec![ValType::I32, ValType::I32],
        Some(ty) => vec![foreign_wasm_val(ty)],
        None => Vec::new(),
    }
}

pub(in crate::codegen::wasm) fn foreign_wasm_params(abi: &ForeignAbi) -> Vec<ValType> {
    let mut params = Vec::new();
    for ty in &abi.params {
        match ty {
            ForeignType::TextCopy if wasm_text_is_js_string(abi.kind) => {
                params.push(ValType::EXTERNREF);
            }
            ForeignType::TextCopy => {
                params.push(ValType::I32);
                params.push(ValType::I32);
            }
            other => params.push(foreign_wasm_val(*other)),
        }
    }
    params
}

pub(in crate::codegen::wasm) fn emit_foreign_thunk(
    function: &MirFunction,
    import_index: u32,
    js_text: Option<JsTextHelpers>,
) -> Result<Function, CompileError> {
    let abi = function.foreign.as_ref().ok_or_else(|| {
        CompileError::codegen("internal error: emit_foreign_thunk without ForeignAbi")
    })?;
    let utf8 = ffi_charset() == Charset::Utf8;
    let text_count = abi.params.iter().filter(|ty| ty.is_text()).count();
    let text_result = abi.result.is_some_and(ForeignType::is_text);
    if utf8 && (text_count > 0 || text_result) && !gc_objects_enabled() {
        return Err(CompileError::codegen(
            "--charset utf8 text FFI requires WasmGC",
        ));
    }
    let js_string = uses_js_string_text(abi);
    if js_string && js_text.is_none() {
        return Err(CompileError::codegen(
            "internal error: JS/Host text FFI missing sim string helpers",
        ));
    }
    // Module-wide helpers must not fire on a C `(ptr, len)` thunk in the same
    // module as a JS/Host `String` import.
    let helpers = if js_string { js_text } else { None };
    let nparams = abi.params.len() as u32;
    let mut next = nparams;
    let (s_idx, s_start, text_slots) = if text_count == 0 {
        (0, 0, Vec::new())
    } else {
        let s_idx = next;
        next += 1;
        let s_start = next;
        next += 1;
        let mut slots = Vec::with_capacity(text_count);
        for _ in 0..text_count {
            let addr = next;
            next += 1;
            let len = next;
            next += 1;
            slots.push((addr, len));
        }
        (s_idx, s_start, slots)
    };
    let (s_dst, s_ch) = if utf8 && text_count > 0 {
        let s_dst = next;
        next += 1;
        let s_ch = next;
        next += 1;
        (s_dst, s_ch)
    } else {
        (0, 0)
    };
    let (r_ptr, r_len, r_idx) = if text_result {
        let r_ptr = next;
        next += 1;
        let r_len = next;
        next += 1;
        let r_idx = if text_count == 0 {
            let idx = next;
            next += 1;
            idx
        } else {
            s_idx
        };
        (r_ptr, r_len, r_idx)
    } else {
        (0, 0, 0)
    };
    let (r_dst, r_nchars, r_tmp) = if utf8 && text_result {
        let r_dst = next;
        next += 1;
        let r_nchars = next;
        next += 1;
        let r_tmp = next;
        next += 1;
        (r_dst, r_nchars, r_tmp)
    } else {
        (0, 0, 0)
    };
    let mut js_str_slots = Vec::new();
    if js_string && text_count > 0 {
        js_str_slots.reserve(text_count);
        for _ in 0..text_count {
            js_str_slots.push(next);
            next += 1;
        }
    }
    let chars = next; // WasmGC chars ref after i32 scratch (and JS string locals)
    let r_chars = chars;

    let extra = extra_locals(text_count, text_result, utf8, js_str_slots.len())?;
    let mut body = Function::new(extra);
    let mut text_k = 0usize;
    for (index, ty) in abi.params.iter().enumerate() {
        if ty.is_text() {
            let (addr, len) = text_slots[text_k];
            emit_text_arg_to_scratch(
                &mut body,
                index as u32,
                addr,
                len,
                s_idx,
                s_start,
                chars,
                utf8.then_some((s_dst, s_ch)),
            )?;
            if let Some(helpers) = helpers {
                body.instruction(&Instruction::LocalGet(addr));
                body.instruction(&Instruction::LocalGet(len));
                body.instruction(&Instruction::I32Const(i32::from(utf8)));
                body.instruction(&Instruction::Call(helpers.text_from_bytes));
                body.instruction(&Instruction::LocalSet(js_str_slots[text_k]));
            }
            text_k += 1;
        }
    }
    text_k = 0;
    for (index, ty) in abi.params.iter().enumerate() {
        if ty.is_text() {
            if js_string {
                body.instruction(&Instruction::LocalGet(js_str_slots[text_k]));
            } else {
                let (addr, len) = text_slots[text_k];
                body.instruction(&Instruction::LocalGet(addr));
                body.instruction(&Instruction::LocalGet(len));
            }
            text_k += 1;
        } else {
            body.instruction(&Instruction::LocalGet(index as u32));
            if ty.is_i32_abi() {
                body.instruction(&Instruction::I32WrapI64);
            }
        }
    }
    body.instruction(&Instruction::Call(import_index));
    if text_result {
        if let Some(helpers) = helpers {
            body.instruction(&Instruction::I32Const(i32::from(utf8)));
            body.instruction(&Instruction::Call(helpers.bytes_from_text));
        }
        body.instruction(&Instruction::LocalSet(r_len));
        body.instruction(&Instruction::LocalSet(r_ptr));
        if gc_objects_enabled() {
            if utf8 {
                emit_push_text_frame_from_utf8_bytes(
                    &mut body, r_ptr, r_len, r_idx, r_chars, r_dst, r_nchars, r_tmp,
                )?;
            } else {
                emit_push_text_frame_from_linear_bytes(&mut body, r_ptr, r_len, r_idx, r_chars)?;
            }
        } else {
            return Err(CompileError::codegen(
                "text results at a wasm boundary require WasmGC",
            ));
        }
    } else if let Some(ty) = abi.result
        && ty.is_i32_abi()
    {
        body.instruction(&Instruction::I64ExtendI32U);
    }
    body.instruction(&Instruction::End);
    Ok(body)
}

fn extra_locals(
    text_count: usize,
    text_result: bool,
    utf8: bool,
    js_string_args: usize,
) -> Result<Vec<(u32, ValType)>, CompileError> {
    if text_count == 0 && !text_result {
        return Ok(Vec::new());
    }
    let mut i32_count = 0u32;
    if text_count > 0 {
        i32_count += 2 + text_count as u32 * 2;
        if utf8 {
            i32_count += 2; // s_dst, s_ch
        }
    }
    if text_result {
        i32_count += 2;
        if text_count == 0 {
            i32_count += 1; // loop index when we cannot reuse s_idx
        }
        if utf8 {
            i32_count += 3; // r_dst, r_nchars, r_tmp
        }
    }
    let mut extra = Vec::new();
    if i32_count > 0 {
        extra.push((i32_count, ValType::I32));
    }
    if js_string_args > 0 {
        extra.push((js_string_args as u32, ValType::EXTERNREF));
    }
    if gc_objects_enabled() && (text_count > 0 || text_result) {
        let chars_ref = gc_ctx(|ctx| crate::codegen::wasm_gc::concrete_ref_null(ctx.text_chars_ty))
            .ok_or_else(|| {
                CompileError::codegen("MIR wasm: WasmGC context missing for text FFI")
            })?;
        extra.push((1, chars_ref));
    }
    Ok(extra)
}

fn emit_text_arg_to_scratch(
    body: &mut Function,
    src: u32,
    addr: u32,
    len: u32,
    s_idx: u32,
    s_start: u32,
    chars: u32,
    utf8: Option<(u32, u32)>,
) -> Result<(), CompileError> {
    if gc_objects_enabled() {
        if let Some((s_dst, s_ch)) = utf8 {
            emit_text_to_linear_scratch_utf8_gc(
                body,
                LocalId(src as usize),
                addr,
                s_idx,
                len,
                s_start,
                chars,
                s_dst,
                s_ch,
            )
        } else {
            emit_text_to_linear_scratch_gc(
                body,
                LocalId(src as usize),
                addr,
                s_idx,
                len,
                s_start,
                chars,
            )
        }
    } else {
        body.instruction(&Instruction::LocalGet(src));
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::LocalTee(addr));
        body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
            offset: FRAME_OFF_PTR,
            align: 2,
            memory_index: 0,
        }));
        body.instruction(&Instruction::LocalSet(addr));
        body.instruction(&Instruction::LocalGet(src));
        body.instruction(&Instruction::I32WrapI64);
        body.instruction(&Instruction::I32Load(wasm_encoder::MemArg {
            offset: FRAME_OFF_LEN,
            align: 2,
            memory_index: 0,
        }));
        body.instruction(&Instruction::LocalSet(len));
        Ok(())
    }
}

fn mir_export_type(ty: crate::mir::MirType) -> Result<ForeignType, CompileError> {
    match ty {
        crate::mir::MirType::I64 => Ok(ForeignType::I64),
        crate::mir::MirType::F64 | crate::mir::MirType::LongF64 => Ok(ForeignType::F64),
        crate::mir::MirType::Bool => Ok(ForeignType::Bool),
        crate::mir::MirType::ObjectRef => Ok(ForeignType::ObjectHandle),
        other => Err(CompileError::codegen(format!(
            "cannot export Simula type {other} across a wasm boundary"
        ))),
    }
}

/// C/JS ABI wrapper around a Simula procedure: bool/char are i32 at the edge.
pub(in crate::codegen::wasm) fn emit_export_thunk(
    function: &MirFunction,
    mir_func_index: u32,
) -> Result<Function, CompileError> {
    let mut body = Function::new([]);
    for (index, param) in function.params.iter().enumerate() {
        body.instruction(&Instruction::LocalGet(index as u32));
        let ty = mir_export_type(param.ty)?;
        if ty.is_i32_abi() {
            body.instruction(&Instruction::I64ExtendI32U);
        }
    }
    body.instruction(&Instruction::Call(mir_func_index));
    if let Some(ty) = function.result {
        let foreign = mir_export_type(ty)?;
        if foreign.is_i32_abi() {
            body.instruction(&Instruction::I32WrapI64);
        }
    }
    body.instruction(&Instruction::End);
    Ok(body)
}

pub(in crate::codegen::wasm) fn export_wasm_val(
    ty: crate::mir::MirType,
) -> Result<ValType, CompileError> {
    Ok(foreign_wasm_val(mir_export_type(ty)?))
}
