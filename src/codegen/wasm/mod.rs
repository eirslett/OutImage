//! WebAssembly code generation via [wasm-encoder] (pure Rust, no clang).
//!
//! Phase 6: lower MIR for wasm-node (`wasi_snapshot_preview1.fd_write`) and
//! wasm-browser (`env.fd_write` polyfill) into a live `_start` that executes
//! scalar control flow, local procedures, text (incl. edit/deedit via host
//! imports), N-D arrays, objects, and `f64`/`**`.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataCountSection, DataSection,
    ElementSection, Elements, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    GlobalSection, GlobalType, HeapType, Ieee64, ImportSection, Instruction, MemorySection,
    MemoryType, Module, RefType, StartSection, TableSection, TableType, TypeSection, ValType,
};

use crate::ast::Program;
use crate::codegen::sourcemap::{Mapping, SourceMap, span_to_line_col};
use crate::codegen::wasm_dwarf;
use crate::error::CompileError;
use crate::layout::{FieldType as LayoutFieldType, SIMSET_PRED_OFFSET, SIMSET_SUC_OFFSET};
use crate::mir::{
    self, BinOp, CallSig, CmpOp, Function as MirFunction, LocalId, MirType, Module as MirModule,
    Op, UnOp, asyncify, seq_runtime, sim_runtime,
};
use crate::source::SourceFile;
use crate::target::{Charset, CompileTarget};

/// Funcref table for `CallIndirect` (MIR function ordinals).
const FUNCREF_TABLE: u32 = 0;
/// BASICIO-only pinning table (Phase 4-R4): a disk file's host identity is the
/// index of the slot pinning its receiver here. Slots 0..[`BASICIO_HOST_ID_FIRST_DISK`]
/// are never used: `0` mirrors `none` and `1` / `2` are the terminals' fixed
/// host ids, which need no pin because the terminals are rooted in
/// [`GLOBAL_SYSIN`] / [`GLOBAL_SYSOUT`]. There is no shared root-handle table.
const BASICIO_HANDLE_TABLE: u32 = 1;
/// Fixed BASICIO host identities for the SysIn / SysOut terminal singletons.
const BASICIO_HOST_ID_SYSIN: i32 = 1;
const BASICIO_HOST_ID_SYSOUT: i32 = 2;
const BASICIO_HOST_ID_FIRST_DISK: i32 = 3;
/// Mutable `(ref null eq)` globals rooting the SysIn / SysOut singletons under
/// WasmGC. `sysin` / `sysout` and every terminal test read them directly, so
/// the terminals no longer occupy reserved handle-table slots.
const GLOBAL_SYSIN: u32 = 0;
const GLOBAL_SYSOUT: u32 = 1;
/// Phase 4-R4 sequencing registry: a mutable `(ref null $seq_gc_registry)`
/// holding every coroutine's / component's GC side record, and the count of
/// slots handed out so far. A linear sequencing record names its own record
/// by index (`CORO_GC_SLOT` / `COMP_GC_SLOT`), so `CORO_ARG`, `COMP_OBJECT`
/// and the parked-frame ref spine are host-traced fields rather than
/// integer handles. The registry array is replaced (allocate,
/// `array.copy`, `global.set`) when it fills; indices stay stable.
const GLOBAL_SEQ_GC_REGISTRY: u32 = 2;
const GLOBAL_SEQ_GC_COUNT: u32 = 3;
/// Slots the sequencing registry starts with, and the floor every growth
/// step rounds up to.
const SEQ_GC_REGISTRY_INITIAL_SLOTS: i32 = 16;
/// Phase 4-R4 Simulation state: chapter 12's three process references are
/// mutable `(ref null eq)` globals rather than words in the linear
/// `sim_runtime` state block, and the SQS's `process` column is a WasmGC
/// array beside the linear notice records — see
/// [`crate::mir::sim_runtime::SIM_CURRENT_STORE`]. Nothing about the
/// Simulation uses a shared root-handle table any more.
const GLOBAL_SIM_CURRENT: u32 = 4;
const GLOBAL_SIM_RUNNING: u32 = 5;
const GLOBAL_SIM_MAIN: u32 = 6;
const GLOBAL_SIM_NOTICE_PROCS: u32 = 7;
/// Notices the SQS process column starts with, and the floor every growth
/// step rounds up to. The linear set is allocated for the hard `SQS_MAX_LEN`
/// limit up front; the reference column grows with the live length instead,
/// so an idle Simulation does not pin 65 536 array slots.
const SIM_NOTICE_PROCS_INITIAL_SLOTS: i32 = 16;

/// Bump-pointer for WASI/IO scratch, terminal images, and scalar spill (i32).
const HEAP_CURSOR: u32 = 4;
/// Absolute address of the SysIn image header (`len`, `pos`, `endfile`, pad)
/// followed by an [`IMAGE_BUF_SIZE`] line buffer — written into memory at init.
const SYSIN_BASE_PTR: u32 = 0;
/// `nread` out-param for the `CallInLine` `fd_read` (stdin) call.
const NREAD_PTR: u32 = 12;
/// Scratch iovec for `OutText` of a text local (ptr + len).
const SCRATCH_IOV: u32 = 16;
/// Scratch iovec for the `CallInLine` `fd_read` (stdin) call (ptr + len).
const READ_IOV: u32 = 24;
/// Absolute address of the SysOut image header (`len`, `pos`, `line`, pad)
/// followed by an [`IMAGE_BUF_SIZE`] buffer. Out* calls fill the image and
/// `OutImage` / `BreakOutImage` flush it, matching `runtime/runtime.c`.
const SYSOUT_BASE_PTR: u32 = 32;
/// Address holding the singleton `sysin` / `sysout` BASICIO object pointers.
const SYSIN_OBJ_PTR: u32 = 36;
const SYSOUT_OBJ_PTR: u32 = 40;
/// Class id of SIMSET's `Head` as an i64, so `Suc` / `Pred` can stop at the
/// ring's head (§12.2). `-1` until `SimsetSetHeadClassId` registers the real
/// one, which never matches a class id.
const SIMSET_HEAD_CLASS_ID_PTR: u32 = 48;
const IOV_BASE: u32 = 56;
/// Capacity of the SysIn / SysOut image buffers, matching native
/// `sysin_line[4096]` / `line[4096]` (`runtime/runtime.c`). Terminal
/// `linelength` is still 80 / 132; this is the backing store so a long
/// `InImage` record is not truncated at 512.
const IMAGE_BUF_SIZE: u32 = 4096;
/// Byte size of the singleton `sysin` / `sysout` objects (class id only).
const TERMINAL_OBJ_SIZE: u32 = 16;
/// `filename` (§10.1) for the terminal files, which have no constructor path.
const SYSIN_FILENAME: &str = "<SYSIN>";
const SYSOUT_FILENAME: &str = "<SYSOUT>";
/// Implementation-defined SYSIN / SYSOUT image lengths (§10 intro), matching
/// `runtime/runtime.c`.
const SYSIN_LINELENGTH: u32 = 80;
const SYSOUT_LINELENGTH: u32 = 132;
/// The iovec table runs up to the sequencing runtime's static state, which
/// sits at a fixed address because MIR can only reach memory through a pointer
/// and a constant is the only way to name one.
const IOV_LIMIT: u32 = seq_runtime::STATE_BASE as u32;
/// Exclusive end of the `outimage-wasm-rt` slab (1MiB stack-first + data/arena).
/// Must match `runtime/wasm-rt` `C_RT_END` and `build.rs` `--initial-memory`.
const TEXT_BASE: u32 = 2 * 1024 * 1024;
/// A text value is a view (`ptr`, `len`) onto a main character buffer, plus the
/// position and constant flag. `start` and `main_len` describe where the view
/// sits inside its main object, which is what `start` and `main` report; the
/// main buffer begins at `ptr - (start - 1)`.
const FRAME_SIZE: i32 = 24;
const FRAME_OFF_PTR: u64 = 0;
const FRAME_OFF_LEN: u64 = 4;
const FRAME_OFF_POS: u64 = 8;
const FRAME_OFF_PAD: u64 = 12;
const FRAME_OFF_START: u64 = 16;
const FRAME_OFF_MAIN_LEN: u64 = 20;
/// A SysIn / SysOut image *is* a text frame: `sysin.image` / `sysout.image`
/// hand out this very frame, so `image.setpos` and the file's own `pos` are
/// the same field (§10.3) and `image := notext` blanks the pending record.
/// The frame is followed by `endfile` (SysIn) / `line` (SysOut) and the
/// character buffer.
const IMAGE_OFF_LEN: u64 = FRAME_OFF_LEN;
const IMAGE_OFF_POS: u64 = FRAME_OFF_POS;
const IMAGE_OFF_MAIN_LEN: u64 = FRAME_OFF_MAIN_LEN;
const IMAGE_OFF_FLAG: u64 = 24;
const IMAGE_OFF_BUF: u64 = 32;
/// Stdin `fd_read` / `CallInLine` buffer, matching native `sysin_line[4096]`.
/// Longer input is still truncated to this many bytes.
const READ_BUF_SIZE: i32 = 4096;

/// How `OutText` / `OutImage` reach the host.
///
/// Both targets use the WASI `fd_write(fd, iovs, iovs_len, nwritten) -> errno`
/// calling convention so MIR emission stays identical; only the import path
/// differs (`wasi_snapshot_preview1.fd_write` vs `env.fd_write`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WasmIo {
    Wasi,
    Browser,
}

thread_local! {
    /// When true, `ObjectRef` lowers to `(ref null any)` and object ops use WasmGC.
    static GC_OBJECTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static GC_CTX: std::cell::RefCell<Option<crate::codegen::wasm_gc::GcEmitCtx>> =
        const { std::cell::RefCell::new(None) };
    static FFI_CHARSET: std::cell::Cell<Charset> = const { std::cell::Cell::new(Charset::Latin1) };
}

fn gc_objects_enabled() -> bool {
    GC_OBJECTS.with(|c| c.get())
}

fn ffi_charset() -> Charset {
    FFI_CHARSET.with(|c| c.get())
}

fn gc_ctx<R>(f: impl FnOnce(&crate::codegen::wasm_gc::GcEmitCtx) -> R) -> Option<R> {
    GC_CTX.with(|slot| slot.borrow().as_ref().map(f))
}

pub fn compile(
    program: &Program,
    target: CompileTarget,
    output_path: &Path,
    debug_info: bool,
    source: &SourceFile,
    write_wasm_host: bool,
) -> Result<PathBuf, CompileError> {
    let io = match target {
        CompileTarget::WasmNode => WasmIo::Wasi,
        CompileTarget::WasmBrowser => WasmIo::Browser,
        _ => {
            return Err(CompileError::codegen(format!(
                "internal error: {target} is not a wasm target"
            )));
        }
    };
    let (mut bytes, debug_map, keep) = try_emit_mir(program, io, debug_info, source)?;
    append_shaken_runtime(&mut bytes, &keep)?;

    if let Some(map) = debug_map {
        let map_name = format!(
            "{}.map",
            output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out.wasm")
        );
        let v3 = map.to_wasm_pc_source_map_v3(
            output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out.wasm"),
            Some(&source.text),
        );
        let v3_json = v3.to_json().map_err(|error| {
            CompileError::codegen(format!("failed to serialize wasm Source Map v3: {error}"))
        })?;
        let map_path = PathBuf::from(format!("{}.map", output_path.display()));
        std::fs::write(&map_path, v3_json).map_err(|error| {
            CompileError::codegen(format!("failed to write {}: {error}", map_path.display()))
        })?;

        let side_json = map.to_json().map_err(|error| {
            CompileError::codegen(format!("failed to serialize wasm side-map: {error}"))
        })?;
        let side_path = PathBuf::from(format!("{}.sim-map", output_path.display()));
        std::fs::write(&side_path, side_json).map_err(|error| {
            CompileError::codegen(format!("failed to write {}: {error}", side_path.display()))
        })?;

        append_source_mapping_url(&mut bytes, &map_name);
    }

    std::fs::write(output_path, &bytes).map_err(|error| {
        CompileError::codegen(format!(
            "failed to write wasm module {}: {error}",
            output_path.display()
        ))
    })?;
    runner::write_run_wrappers(output_path, target, write_wasm_host)?;

    Ok(output_path.to_path_buf())
}

pub fn compile_mir(
    mir: &MirModule,
    target: CompileTarget,
    output_path: &Path,
    debug_info: bool,
    source: &SourceFile,
    write_wasm_host: bool,
) -> Result<PathBuf, CompileError> {
    let io = match target {
        CompileTarget::WasmNode => WasmIo::Wasi,
        CompileTarget::WasmBrowser => WasmIo::Browser,
        _ => {
            return Err(CompileError::codegen(format!(
                "internal error: {target} is not a wasm target"
            )));
        }
    };
    let (mut bytes, debug_map, keep) = emit_prepared_mir(mir.clone(), io, debug_info, source)?;
    append_shaken_runtime(&mut bytes, &keep)?;

    if let Some(map) = debug_map {
        let map_name = format!(
            "{}.map",
            output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out.wasm")
        );
        let v3 = map.to_wasm_pc_source_map_v3(
            output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out.wasm"),
            Some(&source.text),
        );
        let v3_json = v3.to_json().map_err(|error| {
            CompileError::codegen(format!("failed to serialize wasm Source Map v3: {error}"))
        })?;
        let map_path = PathBuf::from(format!("{}.map", output_path.display()));
        std::fs::write(&map_path, v3_json).map_err(|error| {
            CompileError::codegen(format!("failed to write {}: {error}", map_path.display()))
        })?;

        let side_json = map.to_json().map_err(|error| {
            CompileError::codegen(format!("failed to serialize wasm side-map: {error}"))
        })?;
        let side_path = PathBuf::from(format!("{}.sim-map", output_path.display()));
        std::fs::write(&side_path, side_json).map_err(|error| {
            CompileError::codegen(format!("failed to write {}: {error}", side_path.display()))
        })?;

        append_source_mapping_url(&mut bytes, &map_name);
    }

    std::fs::write(output_path, &bytes).map_err(|error| {
        CompileError::codegen(format!(
            "failed to write wasm module {}: {error}",
            output_path.display()
        ))
    })?;
    runner::write_run_wrappers(output_path, target, write_wasm_host)?;

    Ok(output_path.to_path_buf())
}

fn try_emit_mir(
    program: &Program,
    io: WasmIo,
    debug_info: bool,
    source: &SourceFile,
) -> Result<(Vec<u8>, Option<SourceMap>, HashSet<String>), CompileError> {
    let mir = mir::lower_program_with_source(program, &source.text)?;
    emit_prepared_mir(mir, io, debug_info, source)
}

fn emit_prepared_mir(
    mut mir: MirModule,
    io: WasmIo,
    debug_info: bool,
    source: &SourceFile,
) -> Result<(Vec<u8>, Option<SourceMap>, HashSet<String>), CompileError> {
    mir.ensure_externals_resolved()?;
    if mir.functions.is_empty() {
        return Err(CompileError::codegen("MIR wasm: missing main function"));
    }
    // Wasm has no way to switch stacks, so chapter 7's per-component stacks
    // become heap buffers of spilled frames driven by a trampoline.
    asyncify::lower_to_spill_buffers(&mut mir);
    // The standard classes are lowered whether or not a program uses them, so a
    // SIMSET-only program still carries `Process$__coro`. Only hold *live* code
    // to the supported subset; dead bodies become traps.
    let reachable = used_env::reachable_functions(&mir);
    for function in &mir.functions {
        ensure_supported_subset(function, reachable.contains(&function.name))?;
    }
    if !mir.functions.iter().any(|function| function.name == "main") {
        return Err(CompileError::codegen("MIR wasm: missing main function"));
    }
    let used = used_env::used_env_imports(&mir, &reachable);
    let keep = used_env::rt_keep_exports(&used);
    let (bytes, map) = emit_mir(&mir, io, debug_info, source, &reachable, &used)?;
    Ok((bytes, map, keep))
}

fn append_shaken_runtime(bytes: &mut Vec<u8>, keep: &HashSet<String>) -> Result<(), CompileError> {
    let rt = shake::shake_runtime(keep)?;
    append_custom_section(bytes, "simrt", &rt);
    Ok(())
}

fn unsupported_whole_file_io() -> CompileError {
    CompileError::codegen(
        "whole-file I/O (fileExists/fileRead/fileWrite) is not supported on wasm yet \
         (native only; use the interpreter or Native target)",
    )
}

fn ensure_supported_subset(function: &MirFunction, reachable: bool) -> Result<(), CompileError> {
    // Dead bodies (standard-class leftovers) become traps; only live code
    // has to stay inside the wasm-supported subset.
    if !reachable {
        return Ok(());
    }
    for block in &function.blocks {
        for spanned in &block.ops {
            match &spanned.op {
                Op::CallFileExists { .. } | Op::CallFileRead { .. } | Op::CallFileWrite { .. } => {
                    return Err(unsupported_whole_file_io());
                }
                op if !op_is_supported(op) => {
                    return Err(CompileError::codegen(format!(
                        "MIR wasm: unsupported op in Phase 6 subset: {:?}",
                        spanned.op
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn op_is_supported(op: &Op) -> bool {
    match op {
        Op::ConstI64 { .. }
        | Op::ConstF64 { .. }
        | Op::ConstBool { .. }
        | Op::I64ToF64 { .. }
        | Op::F64ToI64 { .. }
        | Op::Copy { .. }
        | Op::Binary { .. }
        | Op::Unary { .. }
        | Op::LoadLocal { .. }
        | Op::StoreLocal { .. }
        | Op::Compare { .. }
        | Op::Jump { .. }
        | Op::Branch { .. }
        | Op::CallOutText { .. }
        | Op::CallOutTextLocal { .. }
        | Op::CallOutImage
        | Op::CallOutInt { .. }
        | Op::CallOutReal { .. }
        | Op::CallOutFix { .. }
        | Op::CallOutFrac { .. }
        | Op::CallOutChar { .. }
        | Op::CallBreakOutImage
        | Op::CallInImage
        | Op::CallInChar { .. }
        | Op::CallEndfile { .. }
        | Op::CallEnv { .. }
        | Op::CallInLine { .. }
        | Op::Call { .. }
        | Op::Abort { .. }
        | Op::Return { .. }
        | Op::TextNotext { .. }
        | Op::TextFromLiteral { .. }
        | Op::TextAssign { .. }
        | Op::TextRefAssign { .. }
        | Op::TextCopy { .. }
        | Op::ArrayCopy { .. }
        | Op::TextConcat { .. }
        | Op::TextContentEq { .. }
        | Op::TextContentCmp { .. }
        | Op::TextLength { .. }
        | Op::TextConstant { .. }
        | Op::TextStart { .. }
        | Op::TextMain { .. }
        | Op::TextPos { .. }
        | Op::TextMore { .. }
        | Op::TextSetpos { .. }
        | Op::TextGetchar { .. }
        | Op::TextPutchar { .. }
        | Op::TextBlanks { .. }
        | Op::TextRefEq { .. }
        | Op::TextSub { .. }
        | Op::TextStrip { .. }
        | Op::TextUpcase { .. }
        | Op::TextLowcase { .. }
        | Op::TextGetint { .. }
        | Op::TextPutint { .. }
        | Op::TextGetfrac { .. }
        | Op::TextPutfrac { .. }
        | Op::TextGetreal { .. }
        | Op::TextPutfix { .. }
        | Op::TextPutreal { .. }
        | Op::ConstNone { .. }
        | Op::NewObject { .. }
        | Op::FieldLoadI64 { .. }
        | Op::FieldStoreI64 { .. }
        | Op::ObjectIsNone { .. }
        | Op::ObjectClassIdSafe { .. }
        | Op::SimBegin
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
        | Op::SimNextev { .. }
        | Op::SimsetSetHeadClassId { .. }
        | Op::SimsetInitHead { .. }
        | Op::SimsetOut { .. }
        | Op::SimsetPrecede { .. }
        | Op::SimsetFollow { .. }
        | Op::SimsetInto { .. }
        | Op::SimsetSuc { .. }
        | Op::SimsetPred { .. }
        | Op::SimsetEmpty { .. }
        | Op::SimsetCardinal { .. }
        | Op::SeqSystemEnter { .. }
        | Op::SeqSystemExit { .. }
        | Op::SeqObjectCreate { .. }
        | Op::SeqObjectStart { .. }
        | Op::SeqBlockInstance { .. }
        | Op::SeqDetach { .. }
        | Op::SeqCall { .. }
        | Op::SeqResume { .. }
        | Op::SeqTerminate { .. }
        | Op::GotoEscape { .. }
        | Op::Nop
        | Op::LocalAddr { .. }
        | Op::FieldAddr { .. }
        | Op::LoadRefI64 { .. }
        | Op::StoreRefI64 { .. }
        | Op::StackAlloc { .. }
        | Op::HeapAlloc { .. }
        | Op::FuncAddr { .. }
        | Op::CallIndirect { .. } => true,
        Op::AllocArray { bounds, .. } => !bounds.is_empty(),
        Op::ArrayLoad { indices, .. } | Op::ArrayStore { indices, .. } => !indices.is_empty(),
        Op::CallFileExists { .. } | Op::CallFileRead { .. } | Op::CallFileWrite { .. } => false,
        // Terminal-only BASICIO: the `sysin` / `sysout` singletons are backed
        // by the same static image buffers as the free Out*/In* wrappers.
        Op::CallSysIn { .. }
        | Op::CallSysOut { .. }
        | Op::CallBasicioOpen { .. }
        | Op::CallBasicioClose { .. }
        | Op::CallBasicioIsOpen { .. }
        | Op::CallBasicioOutText { .. }
        | Op::CallBasicioOutChar { .. }
        | Op::CallBasicioOutImage { .. }
        | Op::CallBasicioBreakOutImage { .. }
        | Op::CallBasicioInImage { .. }
        | Op::CallBasicioInChar { .. }
        | Op::CallBasicioLastItem { .. }
        | Op::CallBasicioInInt { .. }
        | Op::CallBasicioInReal { .. }
        | Op::CallBasicioInFrac { .. }
        | Op::CallBasicioInText { .. }
        | Op::CallBasicioEndfile { .. }
        | Op::CallBasicioOutReal { .. }
        | Op::CallBasicioOutFix { .. }
        | Op::CallBasicioOutFrac { .. }
        | Op::CallBasicioOutInt { .. }
        | Op::CallBasicioLine { .. }
        | Op::CallBasicioImage { .. }
        | Op::CallBasicioPos { .. }
        | Op::CallBasicioLength { .. }
        | Op::CallBasicioSetImage { .. }
        | Op::CallBasicioSetpos { .. }
        | Op::CallBasicioFilename { .. }
        | Op::CallBasicioSetAccess { .. }
        | Op::CallBasicioEject { .. }
        | Op::CallBasicioLinesPerPage { .. }
        | Op::CallBasicioInRecord { .. }
        | Op::CallBasicioRegisterFile { .. }
        | Op::CallTerminateProgram => true,
        Op::CallBasicioOpenByte { .. }
        | Op::CallBasicioInByte { .. }
        | Op::CallBasicioOutByte { .. }
        | Op::CallBasicioLocate { .. }
        | Op::CallBasicioLocation { .. }
        | Op::CallBasicioLastloc { .. } => true,
    }
}

mod array;
mod basicio;
mod emit;
mod env;
mod foreign;
mod heap;
mod module;
mod object;
mod runner;
mod seq;
mod shake;
mod simset;
mod text;
mod used_env;

use array::*;
use basicio::*;
use emit::*;
use env::*;
use foreign::*;
use heap::*;
use module::*;
use object::*;
use seq::*;
use simset::*;
use text::*;
pub(in crate::codegen::wasm) use used_env::entry_point;
