//! Translates a [`mir::Module`] into Cranelift IR and defines it inside an
//! [`ObjectModule`].
//!
//! Scope: the scalar subset only. Each MIR
//! [`mir::Local`] becomes a Cranelift [`Variable`] (SSA-backed storage via
//! the `cranelift_frontend` variable API, not stack slots); each MIR
//! [`mir::BasicBlock`] becomes one Cranelift [`Block`]. All Cranelift blocks
//! for a function are created up front so branch/jump targets always
//! resolve, instructions are appended in MIR block order, and every block is
//! sealed in one pass at the end (`seal_all_blocks`) once every predecessor
//! edge has been emitted — simpler and just as correct as sealing
//! block-by-block for this non-looping-forward-reference CFG shape.
//!
//! `main` is emitted exported as `sim_main` returning `i32 0`, matching
//! the pre-MIR hello-world path. Every other [`mir::Function`] (a local
//! procedure — see `mir::lower`) is declared with `Linkage::Local` under a
//! mangled symbol name and defined the same way; [`mir::Op::Call`] resolves
//! its target through the `name -> FuncId` table built in
//! [`emit_mir_module`], which declares *every* function up front (in one
//! pass) before defining any of their bodies (in a second pass) so forward
//! references and recursive/mutually-recursive calls resolve correctly.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use cranelift::prelude::{
    AbiParam, Block as ClifBlock, FloatCC, InstBuilder, IntCC, Type, Variable, types,
};
use cranelift_codegen::LabelValueLoc;
use cranelift_codegen::ir::{Function as ClifFunction, StackSlot, UserFuncName, ValueLabel};
use cranelift_codegen::print_errors;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module as ClifModuleTrait};
use cranelift_object::ObjectModule;

use crate::codegen::dwarf::{
    DebugLocal, DebugValueType, FunctionDebugInfo, LocalLocRange, LocalLocation,
    default_location_for_function, encode_srcloc,
};
use crate::error::CompileError;
use crate::mir::{self, BinOp, BlockId, CmpOp, LocalId, MirType, Op, UnOp};
use crate::source::SourceFile;

static STRING_DATA_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The runtime (`runtime/runtime.c`) functions every emitted `main` may call,
/// declared once per object module and threaded through [`emit_function`] /
/// [`emit_op`] to avoid an ever-growing positional parameter list.
struct RuntimeFuncs {
    out_text: FuncId,
    out_image: FuncId,
    array_alloc_i64: FuncId,
    array_load_i64: FuncId,
    array_store_i64: FuncId,
    array_alloc_f64: FuncId,
    array_load_f64: FuncId,
    array_store_f64: FuncId,
    array_alloc_text: FuncId,
    array_load_text: FuncId,
    array_store_text: FuncId,
    array_copy_i64: FuncId,
    array_copy_f64: FuncId,
    array_copy_text: FuncId,
    array_lowerbound: FuncId,
    array_upperbound: FuncId,
    text_notext: FuncId,
    text_from_literal: FuncId,
    text_copy: FuncId,
    text_blanks: FuncId,
    text_concat: FuncId,
    text_assign_value: FuncId,
    text_assign_ref: FuncId,
    text_content_eq: FuncId,
    text_content_cmp: FuncId,
    text_ref_eq: FuncId,
    text_content_ptr_len: FuncId,
    text_utf8_ptr_len: FuncId,
    text_from_utf8: FuncId,
    text_length: FuncId,
    text_constant: FuncId,
    text_start: FuncId,
    text_main: FuncId,
    text_pos: FuncId,
    text_setpos: FuncId,
    text_more: FuncId,
    text_getchar: FuncId,
    text_putchar: FuncId,
    text_getint: FuncId,
    text_putint: FuncId,
    text_getfrac: FuncId,
    text_putfrac: FuncId,
    text_getreal: FuncId,
    text_putfix: FuncId,
    text_putreal: FuncId,
    text_sub: FuncId,
    text_strip: FuncId,
    text_upcase: FuncId,
    text_lowcase: FuncId,
    object_alloc: FuncId,
    object_load_i64: FuncId,
    object_store_i64: FuncId,
    object_class_id_safe: FuncId,
    f64_pow: FuncId,
    in_line: FuncId,
    out_int: FuncId,
    out_real: FuncId,
    out_fix: FuncId,
    out_frac: FuncId,
    out_char: FuncId,
    break_out_image: FuncId,
    in_image: FuncId,
    in_char: FuncId,
    endfile: FuncId,
    sysin: FuncId,
    sysout: FuncId,
    basicio_register_file: FuncId,
    basicio_open: FuncId,
    basicio_open_byte: FuncId,
    basicio_close: FuncId,
    basicio_isopen: FuncId,
    basicio_outtext: FuncId,
    basicio_outchar: FuncId,
    basicio_outimage: FuncId,
    basicio_breakoutimage: FuncId,
    basicio_inimage: FuncId,
    basicio_inchar: FuncId,
    basicio_lastitem: FuncId,
    basicio_inint: FuncId,
    basicio_inreal: FuncId,
    basicio_infrac: FuncId,
    basicio_intext: FuncId,
    basicio_endfile: FuncId,
    basicio_inbyte: FuncId,
    basicio_outbyte: FuncId,
    basicio_locate: FuncId,
    basicio_location: FuncId,
    basicio_lastloc: FuncId,
    basicio_outreal: FuncId,
    basicio_outfix: FuncId,
    basicio_outfrac: FuncId,
    basicio_outint: FuncId,
    basicio_line: FuncId,
    basicio_image: FuncId,
    basicio_pos: FuncId,
    basicio_length: FuncId,
    basicio_set_image: FuncId,
    basicio_setpos: FuncId,
    basicio_filename: FuncId,
    basicio_setaccess: FuncId,
    basicio_eject: FuncId,
    basicio_linesperpage: FuncId,
    basicio_inrecord: FuncId,
    terminate_program: FuncId,
    decimalmark: FuncId,
    lowten: FuncId,
    sqrt: FuncId,
    sin: FuncId,
    cos: FuncId,
    tan: FuncId,
    ln: FuncId,
    exp: FuncId,
    arctan: FuncId,
    cotan: FuncId,
    arcsin: FuncId,
    arccos: FuncId,
    arctan2: FuncId,
    addepsilon: FuncId,
    subepsilon: FuncId,
    mod_i64: FuncId,
    rem_i64: FuncId,
    sign: FuncId,
    abs_int: FuncId,
    abs_real: FuncId,
    draw: FuncId,
    randint: FuncId,
    uniform: FuncId,
    normal: FuncId,
    negexp: FuncId,
    poisson: FuncId,
    erlang: FuncId,
    discrete: FuncId,
    histd: FuncId,
    linear: FuncId,
    histo: FuncId,
    datetime: FuncId,
    cputime: FuncId,
    clocktime: FuncId,
    sinh: FuncId,
    cosh: FuncId,
    tanh: FuncId,
    log10: FuncId,
    digit: FuncId,
    letter: FuncId,
    char_code: FuncId,
    isochar: FuncId,
    rank: FuncId,
    isorank: FuncId,
    max_int: FuncId,
    min_int: FuncId,
    max_real: FuncId,
    min_real: FuncId,
    error_text: FuncId,
    current_lowten: FuncId,
    current_decimalmark: FuncId,
    file_exists: FuncId,
    file_read: FuncId,
    file_write: FuncId,
    sim_begin: FuncId,
    sim_end: FuncId,
    sim_hold: FuncId,
    sim_activate_direct: FuncId,
    sim_activate_timed: FuncId,
    sim_activate_relative: FuncId,
    sim_passivate: FuncId,
    sim_transfer_to_head: FuncId,
    sim_terminate_current: FuncId,
    sim_cancel: FuncId,
    sim_finish_main: FuncId,
    sim_time: FuncId,
    sim_is_main_current: FuncId,
    sim_has_current: FuncId,
    sim_current: FuncId,
    sim_main: FuncId,
    sim_idle: FuncId,
    sim_terminated: FuncId,
    sim_evtime: FuncId,
    sim_nextev: FuncId,
    simset_set_head_class_id: FuncId,
    simset_init_head: FuncId,
    simset_out: FuncId,
    simset_precede: FuncId,
    simset_follow: FuncId,
    simset_into: FuncId,
    simset_suc: FuncId,
    simset_pred: FuncId,
    simset_empty: FuncId,
    simset_cardinal: FuncId,
    seq_system_enter: FuncId,
    seq_system_exit: FuncId,
    seq_object_create: FuncId,
    seq_object_start: FuncId,
    seq_block_instance: FuncId,
    seq_detach: FuncId,
    seq_call: FuncId,
    seq_resume: FuncId,
    seq_terminate: FuncId,
    gc_root_push: FuncId,
    gc_root_pop: FuncId,
    host_resolve: FuncId,
    register_export: FuncId,
}

/// Declares one `runtime/sequencing.c` entry point. Apart from a leading block
/// identifier on the two operations that name a system head, they take and
/// return opaque pointers, which keeps the declarations uniform.
fn declare_seq_func(
    module: &mut ObjectModule,
    pointer_type: types::Type,
    name: &str,
    params: usize,
    returns_pointer: bool,
) -> Result<FuncId, CompileError> {
    declare_seq_func_of(module, pointer_type, name, params, returns_pointer, false)
}

fn declare_seq_func_of(
    module: &mut ObjectModule,
    pointer_type: types::Type,
    name: &str,
    params: usize,
    returns_pointer: bool,
    leading_block: bool,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    if leading_block {
        sig.params.push(AbiParam::new(types::I64));
    }
    for _ in 0..params {
        sig.params.push(AbiParam::new(pointer_type));
    }
    if returns_pointer {
        sig.returns.push(AbiParam::new(pointer_type));
    }
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

/// Emits every function in `mir_module` (`main` plus every local procedure)
/// into `module`, defining `sim_main` as an exported `i32`-returning
/// function and every procedure as a locally-linked function Cranelift/the
/// linker can resolve calls to.
pub fn emit_mir_module(
    module: &mut ObjectModule,
    mir_module: &mir::Module,
    debug_source: Option<&SourceFile>,
    collect_asm: bool,
    is_lib: bool,
) -> Result<(Vec<FunctionDebugInfo>, Option<String>), CompileError> {
    let pointer_type = module.isa().pointer_type();
    let runtime = RuntimeFuncs {
        out_text: declare_out_text(module, pointer_type)?,
        out_image: declare_out_image(module)?,
        array_alloc_i64: declare_array_alloc(module, pointer_type)?,
        array_load_i64: declare_array_load(module, pointer_type)?,
        array_store_i64: declare_array_store(module, pointer_type)?,
        array_alloc_f64: declare_array_alloc_named(module, pointer_type, "simrt_array_alloc_f64")?,
        array_load_f64: declare_array_load_f64(module, pointer_type)?,
        array_store_f64: declare_array_store_f64(module, pointer_type)?,
        array_alloc_text: declare_array_alloc_text(module, pointer_type)?,
        array_load_text: declare_array_load_text(module, pointer_type)?,
        array_store_text: declare_array_store_text(module, pointer_type)?,
        array_copy_i64: declare_array_copy_i64(module, pointer_type)?,
        array_copy_f64: declare_array_copy_named(module, pointer_type, "simrt_array_copy_f64")?,
        array_copy_text: declare_array_copy_text(module, pointer_type)?,
        array_lowerbound: declare_array_bound(module, pointer_type, "simrt_array_lowerbound")?,
        array_upperbound: declare_array_bound(module, pointer_type, "simrt_array_upperbound")?,
        text_notext: declare_text_notext(module, pointer_type)?,
        text_from_literal: declare_text_from_literal(module, pointer_type)?,
        text_copy: declare_text_copy(module, pointer_type)?,
        text_blanks: declare_text_blanks(module, pointer_type)?,
        text_concat: declare_text_concat(module, pointer_type)?,
        text_assign_value: declare_text_assign_value(module, pointer_type)?,
        text_assign_ref: declare_text_assign_ref(module, pointer_type)?,
        text_content_eq: declare_text_content_eq(module, pointer_type)?,
        text_content_cmp: declare_text_content_cmp(module, pointer_type)?,
        text_ref_eq: declare_text_ref_eq(module, pointer_type)?,
        text_content_ptr_len: declare_text_content_ptr_len(module, pointer_type)?,
        text_utf8_ptr_len: declare_text_utf8_ptr_len(module, pointer_type)?,
        text_from_utf8: declare_text_from_utf8(module, pointer_type)?,
        text_length: declare_text_length(module, pointer_type)?,
        text_constant: declare_text_constant(module, pointer_type)?,
        text_start: declare_text_start(module, pointer_type)?,
        text_main: declare_text_main(module, pointer_type)?,
        text_pos: declare_text_pos(module, pointer_type)?,
        text_setpos: declare_text_setpos(module, pointer_type)?,
        text_more: declare_text_more(module, pointer_type)?,
        text_getchar: declare_text_getchar(module, pointer_type)?,
        text_putchar: declare_text_putchar(module, pointer_type)?,
        text_getint: declare_text_getint(module, pointer_type)?,
        text_putint: declare_text_putint(module, pointer_type)?,
        text_getfrac: declare_text_getfrac(module, pointer_type)?,
        text_putfrac: declare_text_putfrac(module, pointer_type)?,
        text_getreal: declare_text_getreal(module, pointer_type)?,
        text_putfix: declare_text_putfix(module, pointer_type)?,
        text_putreal: declare_text_putreal(module, pointer_type)?,
        text_sub: declare_text_sub(module, pointer_type)?,
        text_strip: declare_text_strip(module, pointer_type)?,
        text_upcase: declare_text_upcase(module, pointer_type)?,
        text_lowcase: declare_text_lowcase(module, pointer_type)?,
        object_alloc: declare_object_alloc(module, pointer_type)?,
        object_load_i64: declare_object_load_i64(module, pointer_type)?,
        object_store_i64: declare_object_store_i64(module, pointer_type)?,
        object_class_id_safe: declare_object_class_id_safe(module, pointer_type)?,
        f64_pow: declare_f64_pow(module)?,
        in_line: declare_in_line(module, pointer_type)?,
        out_int: declare_out_int(module)?,
        out_real: declare_out_f64_i64_i64_i64(module, "simrt_out_real_ex")?,
        out_fix: declare_out_f64_i64_i64(module, "simrt_out_fix")?,
        out_frac: declare_out_i64_i64_i64(module, "simrt_out_frac")?,
        out_char: declare_out_char(module)?,
        break_out_image: declare_void0(module, "simrt_break_out_image")?,
        in_image: declare_void0(module, "simrt_in_image")?,
        in_char: declare_i32_ret0(module, "simrt_in_char")?,
        endfile: declare_i32_ret0(module, "simrt_endfile")?,
        sysin: declare_ptr_ret0(module, pointer_type, "simrt_sysin")?,
        sysout: declare_ptr_ret0(module, pointer_type, "simrt_sysout")?,
        basicio_register_file: declare_basicio_register_file(module, pointer_type)?,
        basicio_open: declare_basicio_ptr_ptr_ret_i32(module, pointer_type, "simrt_basicio_open")?,
        basicio_open_byte: declare_basicio_ptr_ret_i32(
            module,
            pointer_type,
            "simrt_basicio_open_byte",
        )?,
        basicio_close: declare_basicio_ptr_ret_i32(module, pointer_type, "simrt_basicio_close")?,
        basicio_isopen: declare_basicio_ptr_ret_i32(module, pointer_type, "simrt_basicio_isopen")?,
        basicio_outtext: declare_basicio_ptr_ptr(module, pointer_type, "simrt_basicio_outtext")?,
        basicio_outchar: declare_basicio_ptr_i64(module, pointer_type, "simrt_basicio_outchar")?,
        basicio_outimage: declare_basicio_ptr(module, pointer_type, "simrt_basicio_outimage")?,
        basicio_breakoutimage: declare_basicio_ptr(
            module,
            pointer_type,
            "simrt_basicio_breakoutimage",
        )?,
        basicio_inimage: declare_basicio_ptr(module, pointer_type, "simrt_basicio_inimage")?,
        basicio_inchar: declare_basicio_ptr_ret_i32(module, pointer_type, "simrt_basicio_inchar")?,
        basicio_lastitem: declare_basicio_ptr_ret_i32(
            module,
            pointer_type,
            "simrt_basicio_lastitem",
        )?,
        basicio_inint: declare_basicio_ptr_ret_i64(module, pointer_type, "simrt_basicio_inint")?,
        basicio_inreal: declare_basicio_ptr_ret_f64(module, pointer_type, "simrt_basicio_inreal")?,
        basicio_infrac: declare_basicio_ptr_ret_i64(module, pointer_type, "simrt_basicio_infrac")?,
        basicio_intext: declare_basicio_ptr_i64_ret_ptr(
            module,
            pointer_type,
            "simrt_basicio_intext",
        )?,
        basicio_endfile: declare_basicio_ptr_ret_i32(
            module,
            pointer_type,
            "simrt_basicio_endfile",
        )?,
        basicio_inbyte: declare_basicio_ptr_ret_i32(module, pointer_type, "simrt_basicio_inbyte")?,
        basicio_outbyte: declare_basicio_ptr_i64(module, pointer_type, "simrt_basicio_outbyte")?,
        basicio_locate: declare_basicio_ptr_i64(module, pointer_type, "simrt_basicio_locate")?,
        basicio_location: declare_basicio_ptr_ret_i64(
            module,
            pointer_type,
            "simrt_basicio_location",
        )?,
        basicio_lastloc: declare_basicio_ptr_ret_i64(
            module,
            pointer_type,
            "simrt_basicio_lastloc",
        )?,
        basicio_outreal: declare_basicio_ptr_f64_i64_i64_i64(
            module,
            pointer_type,
            "simrt_basicio_outreal_ex",
        )?,
        basicio_outfix: declare_basicio_ptr_f64_i64_i64(
            module,
            pointer_type,
            "simrt_basicio_outfix",
        )?,
        basicio_outfrac: declare_basicio_ptr_i64_i64_i64(
            module,
            pointer_type,
            "simrt_basicio_outfrac",
        )?,
        basicio_outint: declare_basicio_ptr_i64_i64(module, pointer_type, "simrt_basicio_outint")?,
        basicio_line: declare_basicio_ptr_ret_i64(module, pointer_type, "simrt_basicio_line")?,
        basicio_image: declare_basicio_ptr_ret_ptr(module, pointer_type, "simrt_basicio_image")?,
        basicio_pos: declare_basicio_ptr_ret_i64(module, pointer_type, "simrt_basicio_pos")?,
        basicio_length: declare_basicio_ptr_ret_i64(module, pointer_type, "simrt_basicio_length")?,
        basicio_set_image: declare_basicio_ptr_ptr(
            module,
            pointer_type,
            "simrt_basicio_set_image",
        )?,
        basicio_setpos: declare_basicio_ptr_i64(module, pointer_type, "simrt_basicio_setpos")?,
        basicio_filename: declare_basicio_ptr_ret_ptr(
            module,
            pointer_type,
            "simrt_basicio_filename",
        )?,
        basicio_setaccess: declare_basicio_ptr_ptr_ret_i32(
            module,
            pointer_type,
            "simrt_basicio_setaccess",
        )?,
        basicio_eject: declare_basicio_ptr_i64(module, pointer_type, "simrt_basicio_eject")?,
        basicio_linesperpage: {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            module
                .declare_function("simrt_basicio_linesperpage", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        basicio_inrecord: declare_basicio_ptr_ret_i32(
            module,
            pointer_type,
            "simrt_basicio_inrecord",
        )?,
        terminate_program: {
            let sig = module.make_signature();
            module
                .declare_function("simrt_terminate_program", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        decimalmark: declare_env_i64_i64(module, "simrt_decimalmark")?,
        lowten: declare_env_i64_i64(module, "simrt_lowten")?,
        sqrt: declare_env_f64_f64(module, "simrt_sqrt")?,
        sin: declare_env_f64_f64(module, "simrt_sin")?,
        cos: declare_env_f64_f64(module, "simrt_cos")?,
        tan: declare_env_f64_f64(module, "simrt_tan")?,
        ln: declare_env_f64_f64(module, "simrt_ln")?,
        exp: declare_env_f64_f64(module, "simrt_exp")?,
        arctan: declare_env_f64_f64(module, "simrt_arctan")?,
        cotan: declare_env_f64_f64(module, "simrt_cotan")?,
        arcsin: declare_env_f64_f64(module, "simrt_arcsin")?,
        arccos: declare_env_f64_f64(module, "simrt_arccos")?,
        arctan2: declare_env_f64_f64_f64(module, "simrt_arctan2")?,
        addepsilon: declare_env_f64_f64(module, "simrt_addepsilon")?,
        subepsilon: declare_env_f64_f64(module, "simrt_subepsilon")?,
        mod_i64: declare_env_i64_i64_i64(module, "simrt_mod")?,
        rem_i64: declare_env_i64_i64_i64(module, "simrt_rem")?,
        sign: declare_env_f64_i64(module, "simrt_sign")?,
        abs_int: declare_env_i64_i64(module, "simrt_abs_int")?,
        abs_real: declare_env_f64_f64(module, "simrt_abs_real")?,
        draw: declare_env_draw(module, pointer_type)?,
        randint: declare_env_randint(module, pointer_type)?,
        uniform: declare_env_uniform(module, pointer_type)?,
        normal: declare_env_normal(module, pointer_type)?,
        negexp: declare_env_negexp(module, pointer_type)?,
        poisson: declare_env_poisson(module, pointer_type)?,
        erlang: {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::F64));
            sig.params.push(AbiParam::new(types::F64));
            sig.params.push(AbiParam::new(pointer_type));
            sig.returns.push(AbiParam::new(types::F64));
            module
                .declare_function("simrt_erlang", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        discrete: {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(pointer_type));
            sig.returns.push(AbiParam::new(types::I64));
            module
                .declare_function("simrt_discrete", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        histd: {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(pointer_type));
            sig.returns.push(AbiParam::new(types::I64));
            module
                .declare_function("simrt_histd", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        linear: {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(pointer_type));
            sig.returns.push(AbiParam::new(types::F64));
            module
                .declare_function("simrt_linear", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        histo: {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(pointer_type));
            sig.params.push(AbiParam::new(types::F64));
            sig.params.push(AbiParam::new(types::F64));
            sig.returns.push(AbiParam::new(types::I64));
            module
                .declare_function("simrt_histo", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        datetime: {
            let mut sig = module.make_signature();
            sig.returns.push(AbiParam::new(pointer_type));
            module
                .declare_function("simrt_datetime", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        cputime: {
            let mut sig = module.make_signature();
            sig.returns.push(AbiParam::new(types::F64));
            module
                .declare_function("simrt_cputime", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        clocktime: {
            let mut sig = module.make_signature();
            sig.returns.push(AbiParam::new(types::F64));
            module
                .declare_function("simrt_clocktime", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        sinh: declare_env_f64_f64(module, "simrt_sinh")?,
        cosh: declare_env_f64_f64(module, "simrt_cosh")?,
        tanh: declare_env_f64_f64(module, "simrt_tanh")?,
        log10: declare_env_f64_f64(module, "simrt_log10")?,
        digit: declare_env_i64_i64(module, "simrt_digit")?,
        letter: declare_env_i64_i64(module, "simrt_letter")?,
        char_code: declare_env_i64_i64(module, "simrt_char")?,
        isochar: declare_env_i64_i64(module, "simrt_isochar")?,
        rank: declare_env_i64_i64(module, "simrt_rank")?,
        isorank: declare_env_i64_i64(module, "simrt_isorank")?,
        max_int: declare_env_i64_i64_i64(module, "simrt_max_int")?,
        min_int: declare_env_i64_i64_i64(module, "simrt_min_int")?,
        max_real: declare_env_f64_f64_f64(module, "simrt_max_real")?,
        min_real: declare_env_f64_f64_f64(module, "simrt_min_real")?,
        error_text: declare_env_error_text(module, pointer_type)?,
        current_lowten: {
            let mut sig = module.make_signature();
            sig.returns.push(AbiParam::new(types::I64));
            module
                .declare_function("simrt_current_lowten", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        current_decimalmark: {
            let mut sig = module.make_signature();
            sig.returns.push(AbiParam::new(types::I64));
            module
                .declare_function("simrt_current_decimalmark", Linkage::Import, &sig)
                .map_err(map_module_error)?
        },
        file_exists: declare_file_exists(module, pointer_type)?,
        file_read: declare_file_read(module, pointer_type)?,
        file_write: declare_file_write(module, pointer_type)?,
        sim_begin: declare_sim_begin(module)?,
        sim_end: declare_sim_end(module)?,
        sim_hold: declare_sim_hold(module)?,
        sim_activate_direct: declare_sim_activate_direct(module, pointer_type)?,
        sim_activate_timed: declare_sim_activate_timed(module, pointer_type)?,
        sim_activate_relative: declare_sim_activate_relative(module, pointer_type)?,
        sim_passivate: declare_sim_passivate(module)?,
        sim_transfer_to_head: declare_sim_transfer_to_head(module)?,
        sim_terminate_current: declare_sim_terminate_current(module, pointer_type)?,
        sim_cancel: declare_sim_cancel(module, pointer_type)?,
        sim_finish_main: declare_sim_finish_main(module)?,
        sim_time: declare_sim_time(module)?,
        sim_is_main_current: declare_sim_is_main_current(module)?,
        sim_has_current: declare_sim_has_current(module)?,
        sim_current: declare_sim_current(module, pointer_type)?,
        sim_main: declare_sim_main(module, pointer_type)?,
        sim_idle: declare_sim_idle(module, pointer_type)?,
        sim_terminated: declare_sim_terminated(module, pointer_type)?,
        sim_evtime: declare_sim_evtime(module, pointer_type)?,
        sim_nextev: declare_sim_nextev(module, pointer_type)?,
        simset_set_head_class_id: declare_simset_i64(module, "simrt_simset_set_head_class_id")?,
        simset_init_head: declare_simset_ptr(module, pointer_type, "simrt_simset_init_head")?,
        simset_out: declare_simset_ptr(module, pointer_type, "simrt_simset_out")?,
        simset_precede: declare_simset_ptr_ptr(module, pointer_type, "simrt_simset_precede")?,
        simset_follow: declare_simset_ptr_ptr(module, pointer_type, "simrt_simset_follow")?,
        simset_into: declare_simset_ptr_ptr(module, pointer_type, "simrt_simset_into")?,
        simset_suc: declare_simset_ptr_ret_ptr(module, pointer_type, "simrt_simset_suc")?,
        simset_pred: declare_simset_ptr_ret_ptr(module, pointer_type, "simrt_simset_pred")?,
        simset_empty: declare_simset_ptr_ret_i64(module, pointer_type, "simrt_simset_empty")?,
        simset_cardinal: declare_simset_ptr_ret_i64(module, pointer_type, "simrt_simset_cardinal")?,
        seq_system_enter: declare_seq_func_of(
            module,
            pointer_type,
            "simrt_seq_system_enter",
            0,
            true,
            true,
        )?,
        seq_system_exit: declare_seq_func(module, pointer_type, "simrt_seq_system_exit", 1, false)?,
        seq_object_create: declare_seq_func_of(
            module,
            pointer_type,
            "simrt_seq_object_create",
            2,
            true,
            true,
        )?,
        seq_object_start: declare_seq_func(
            module,
            pointer_type,
            "simrt_seq_object_start",
            1,
            false,
        )?,
        seq_block_instance: declare_seq_func(
            module,
            pointer_type,
            "simrt_seq_block_instance",
            1,
            false,
        )?,
        seq_detach: declare_seq_func(module, pointer_type, "simrt_seq_detach", 1, false)?,
        seq_call: declare_seq_func(module, pointer_type, "simrt_seq_call", 1, false)?,
        seq_resume: declare_seq_func(module, pointer_type, "simrt_seq_resume", 1, false)?,
        seq_terminate: declare_seq_func(module, pointer_type, "simrt_seq_terminate", 1, false)?,
        gc_root_push: declare_gc_root_push(module, pointer_type)?,
        gc_root_pop: declare_gc_root_pop(module, pointer_type)?,
        host_resolve: declare_host_resolve(module, pointer_type)?,
        register_export: declare_register_export(module, pointer_type)?,
    };

    // Pass 1: declare every function's signature before defining any body,
    // so calls (including forward references and recursion) always resolve
    // to a known `FuncId`.
    let mut proc_ids: HashMap<String, FuncId> = HashMap::new();
    let mut foreign_imports: HashMap<String, FuncId> = HashMap::new();
    for function in &mir_module.functions {
        if let Some(abi) = &function.foreign {
            let use_host_table = is_lib && abi.kind == crate::mir::ForeignKind::Host;
            if !use_host_table {
                let import_id = declare_foreign_import(module, abi, pointer_type)?;
                foreign_imports.insert(function.name.clone(), import_id);
            }
        }
        let func_id = if function.name == "main" {
            declare_main(module)?
        } else {
            declare_procedure(module, function, pointer_type)?
        };
        proc_ids.insert(function.name.clone(), func_id);
    }

    // Pass 2: define each function's body.
    let mut string_data: HashMap<usize, DataId> = HashMap::new();
    let mut function_debug = Vec::new();
    let mut asm_text = collect_asm.then(String::new);
    for function in &mir_module.functions {
        let func_id = proc_ids[&function.name];
        let mut ctx = module.make_context();
        let sig = module
            .declarations()
            .get_function_decl(func_id)
            .signature
            .clone();
        ctx.func = ClifFunction::with_name_signature(UserFuncName::user(0, func_id.as_u32()), sig);
        if collect_asm {
            ctx.set_disasm(true);
        }

        let symbol_name = if function.name == "main" {
            "sim_main".to_string()
        } else {
            mangled_procedure_name(&function.name)
        };

        if function
            .foreign
            .as_ref()
            .is_some_and(|abi| is_lib && abi.kind == crate::mir::ForeignKind::Host)
        {
            let utf8 = mir_module.charset == crate::target::Charset::Utf8;
            emit_host_table_thunk(
                module,
                &mut ctx.func,
                function,
                pointer_type,
                if utf8 {
                    runtime.text_utf8_ptr_len
                } else {
                    runtime.text_content_ptr_len
                },
                if utf8 {
                    runtime.text_from_utf8
                } else {
                    runtime.text_from_literal
                },
                runtime.host_resolve,
                runtime.gc_root_push,
                runtime.gc_root_pop,
            )?;
        } else if let Some(&import_id) = foreign_imports.get(&function.name) {
            let utf8 = mir_module.charset == crate::target::Charset::Utf8;
            emit_foreign_thunk(
                module,
                &mut ctx.func,
                function,
                import_id,
                pointer_type,
                if utf8 {
                    runtime.text_utf8_ptr_len
                } else {
                    runtime.text_content_ptr_len
                },
                if utf8 {
                    runtime.text_from_utf8
                } else {
                    runtime.text_from_literal
                },
                runtime.gc_root_push,
                runtime.gc_root_pop,
            )?;
        } else {
            emit_function(
                module,
                &mut ctx.func,
                function,
                &mir_module.strings,
                pointer_type,
                &runtime,
                &proc_ids,
                &mut string_data,
                debug_source,
            )?;
        }

        module
            .define_function(func_id, &mut ctx)
            .map_err(|error| map_define_error(error, &ctx.func))?;

        if let Some(compiled) = ctx.compiled_code() {
            if let Some(asm) = asm_text.as_mut()
                && let Some(vcode) = &compiled.vcode
            {
                use std::fmt::Write;
                let _ = writeln!(asm, "# {symbol_name}");
                asm.push_str(vcode);
                if !vcode.ends_with('\n') {
                    asm.push('\n');
                }
                asm.push('\n');
            }
            if let Some(source) = debug_source {
                let spans = function
                    .blocks
                    .iter()
                    .flat_map(|block| block.ops.iter().map(|spanned| spanned.span.clone()));
                let (default_line, default_column) =
                    default_location_for_function(&source.text, spans);
                let locals = collect_debug_locals(function, compiled, module.isa());
                function_debug.push(FunctionDebugInfo {
                    func_id,
                    symbol_name: symbol_name.clone(),
                    srclocs: compiled
                        .buffer
                        .get_srclocs_sorted()
                        .iter()
                        .map(|srcloc| crate::codegen::dwarf::SrcLocRange {
                            start: srcloc.start,
                            end: srcloc.end,
                            loc: srcloc.loc,
                        })
                        .collect(),
                    default_line,
                    default_column,
                    locals,
                });
            }
        }

        module.clear_context(&mut ctx);
    }

    let mut registered = Vec::new();
    for function in &mir_module.functions {
        let Some(export_name) = function.native_export_name() else {
            continue;
        };
        let local_id = proc_ids[&function.name];
        let export_id = emit_c_export_wrapper(module, function, &export_name, local_id)?;
        registered.push((export_name, export_id, export_sig_code(function)?));
    }
    emit_module_init(module, pointer_type, runtime.register_export, &registered)?;

    Ok((function_debug, asm_text))
}

fn declare_out_text(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_out_text", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_out_image(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function("simrt_out_image", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_alloc(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    declare_array_alloc_named(module, pointer_type, "simrt_array_alloc_i64")
}

fn declare_array_alloc_named(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_load_f64(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_array_load_f64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_store_f64(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_array_store_f64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_copy_named(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_bound(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_load(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_array_load_i64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_store(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_array_store_i64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_alloc_text(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_array_alloc_text", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_load_text(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_array_load_text", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_store_text(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_array_store_text", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_copy_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_array_copy_i64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_array_copy_text(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_array_copy_text", Linkage::Import, &sig)
        .map_err(map_module_error)
}

/// Builds a stack-allocated `i64` array from MIR locals and returns its address.
fn emit_i64_stack_array(
    builder: &mut FunctionBuilder<'_>,
    vars: &[Variable],
    locals: &[LocalId],
    pointer_type: Type,
) -> cranelift_codegen::ir::Value {
    let count = locals.len();
    let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        (count as u32) * types::I64.bytes(),
        0,
    ));
    let base = builder.ins().stack_addr(pointer_type, slot, 0);
    for (index, local) in locals.iter().enumerate() {
        let value = builder.use_var(vars[local.0]);
        builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::trusted(),
            value,
            base,
            (index as i32) * types::I64.bytes() as i32,
        );
    }
    base
}

fn declare_text_notext(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_notext", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_from_literal(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_from_literal", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_copy(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_copy", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_blanks(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_blanks", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_concat(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_concat", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_assign_value(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_assign_value", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_assign_ref(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_assign_ref", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_content_eq(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function("simrt_text_content_eq", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_content_cmp(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_content_cmp", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_ref_eq(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function("simrt_text_ref_eq", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_content_ptr_len(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_content_ptr_len", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_utf8_ptr_len(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_utf8_ptr_len", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_from_utf8(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_from_utf8", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_length(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_length", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_constant(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_constant", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_start(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_start", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_main(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_main", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_pos(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_pos", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_setpos(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_setpos", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_more(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_more", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_getchar(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_getchar", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_putchar(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_putchar", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_getint(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_getint", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_putint(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_putint", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_getfrac(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_getfrac", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_putfrac(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_putfrac", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_getreal(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_text_getreal", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_putfix(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_putfix", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_putreal(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_text_putreal_ex", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_sub(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_sub", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_strip(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_strip", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_upcase(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_upcase", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_text_lowcase(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_text_lowcase", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_object_alloc(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_object_alloc", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_object_load_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_object_load_i64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_object_store_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_object_store_i64", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_object_class_id_safe(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_object_class_id_safe", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_f64_pow(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::F64));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_f64_pow", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_in_line(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_in_line", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_out_int(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_out_int", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_out_char(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_out_char", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_out_f64_i64_i64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_out_f64_i64_i64_i64(
    module: &mut ObjectModule,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_out_i64_i64_i64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_void0(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_gc_root_push(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_gc_root_push", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_gc_root_pop(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_gc_root_pop", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_host_resolve(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_host_resolve", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_register_export(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I32));
    module
        .declare_function("simrt_register_export", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_i32_ret0(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_ptr_ret0(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_register_file(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_basicio_register_file", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_ptr_ret_i32(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_ret_i32(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_ret_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_ret_f64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_i64_ret_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_ret_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_f64_i64_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_f64_i64_i64_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_i64_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_basicio_ptr_i64_i64_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_i64_i64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_i64_i64_i64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_f64_f64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_f64_f64_f64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::F64));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_f64_i64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_draw(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_draw", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_randint(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_randint", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_uniform(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_uniform", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_normal(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_normal", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_negexp(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_negexp", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_poisson(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_poisson", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_env_error_text(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_error_text", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_file_exists(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function("simrt_file_exists", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_file_read(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_file_read", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_file_write(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_file_write", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_begin(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function("simrt_sim_begin", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_end(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function("simrt_sim_end", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_hold(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_sim_hold", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_activate_direct(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_sim_activate_direct", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_activate_timed(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::F64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_sim_activate_timed", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_activate_relative(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_sim_activate_relative", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_passivate(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function("simrt_sim_passivate", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_transfer_to_head(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function("simrt_sim_transfer_to_head", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_terminate_current(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_sim_terminate_current", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_cancel(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_sim_cancel", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_finish_main(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let sig = module.make_signature();
    module
        .declare_function("simrt_sim_finish_main", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_time(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_sim_time", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_is_main_current(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_sim_is_main_current", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_has_current(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_sim_has_current", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_current(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_sim_current", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_main(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_sim_main", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_idle(module: &mut ObjectModule, pointer_type: Type) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_sim_idle", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_terminated(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function("simrt_sim_terminated", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_evtime(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::F64));
    module
        .declare_function("simrt_sim_evtime", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_sim_nextev(
    module: &mut ObjectModule,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function("simrt_sim_nextev", Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_simset_i64(module: &mut ObjectModule, name: &str) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_simset_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_simset_ptr_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.params.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_simset_ptr_ret_ptr(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(pointer_type));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_simset_ptr_ret_i64(
    module: &mut ObjectModule,
    pointer_type: Type,
    name: &str,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(pointer_type));
    sig.returns.push(AbiParam::new(types::I64));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn declare_main(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(types::I32));
    module
        .declare_function("sim_main", Linkage::Export, &sig)
        .map_err(map_module_error)
}

/// Declares a local procedure's Cranelift signature: one `AbiParam` per
/// value parameter (in declaration order) and, for a function procedure, a
/// single return matching [`mir::Function::result`]. The symbol is mangled
/// (and kept `Linkage::Local`, i.e. not exported) so a user procedure can
/// never collide with `sim_main` or a `simrt_*` runtime import.
fn declare_procedure(
    module: &mut ObjectModule,
    function: &mir::Function,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();
    for param in &function.params {
        sig.params
            .push(AbiParam::new(clif_type(param.ty, pointer_type)));
    }
    if let Some(result) = function.result {
        sig.returns
            .push(AbiParam::new(clif_type(result, pointer_type)));
    }
    module
        .declare_function(
            &mangled_procedure_name(&function.name),
            Linkage::Local,
            &sig,
        )
        .map_err(map_module_error)
}

fn mangled_procedure_name(name: &str) -> String {
    format!("simrt_proc_{name}")
}

fn declare_foreign_import(
    module: &mut ObjectModule,
    abi: &crate::mir::ForeignAbi,
    pointer_type: Type,
) -> Result<FuncId, CompileError> {
    let symbol = abi.native_symbol()?;
    let sig = foreign_clif_signature(module, abi, pointer_type);
    module
        .declare_function(&symbol, Linkage::Import, &sig)
        .map_err(map_module_error)
}

fn foreign_clif_type(ty: crate::mir::ForeignType, pointer_type: Type) -> Type {
    match ty {
        crate::mir::ForeignType::I64 => types::I64,
        crate::mir::ForeignType::F64 => types::F64,
        crate::mir::ForeignType::Bool | crate::mir::ForeignType::Char => types::I32,
        crate::mir::ForeignType::TextCopy | crate::mir::ForeignType::ObjectHandle => pointer_type,
    }
}

fn emit_push_handle_frame(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    pointer_type: Type,
    gc_root_push: FuncId,
    nslots: u32,
    handle_vals: &[cranelift_codegen::ir::Value],
) -> Option<StackSlot> {
    if nslots == 0 {
        return None;
    }
    let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        GC_ROOT_HEADER_BYTES + 8 * nslots,
        3,
    ));
    let addr = builder.ins().stack_addr(pointer_type, slot, 0);
    let n = builder.ins().iconst(types::I64, i64::from(nslots));
    let push = module.declare_func_in_func(gc_root_push, builder.func);
    builder.ins().call(push, &[addr, n]);
    for (index, &value) in handle_vals.iter().enumerate() {
        let offset = (GC_ROOT_HEADER_BYTES + 8 * index as u32) as i32;
        builder.ins().stack_store(value, slot, offset);
    }
    Some(slot)
}

fn emit_pop_handle_frame(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    pointer_type: Type,
    gc_root_pop: FuncId,
    frame: Option<StackSlot>,
) {
    let Some(slot) = frame else {
        return;
    };
    let addr = builder.ins().stack_addr(pointer_type, slot, 0);
    let pop = module.declare_func_in_func(gc_root_pop, builder.func);
    builder.ins().call(pop, &[addr]);
}

fn emit_foreign_thunk(
    module: &mut ObjectModule,
    clif_func: &mut ClifFunction,
    function: &mir::Function,
    import_id: FuncId,
    pointer_type: Type,
    content_ptr_len: FuncId,
    text_from_literal: FuncId,
    gc_root_push: FuncId,
    gc_root_pop: FuncId,
) -> Result<(), CompileError> {
    let abi = function.foreign.as_ref().ok_or_else(|| {
        CompileError::codegen("internal error: emit_foreign_thunk without ForeignAbi")
    })?;
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(clif_func, &mut fb_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let import_ref = module.declare_func_in_func(import_id, builder.func);
    let block_params = builder.block_params(entry).to_vec();
    let mut args = Vec::new();
    let mut handle_vals = Vec::new();
    for (ty, raw) in abi.params.iter().zip(block_params) {
        if ty.is_text() {
            let (ptr, len) = emit_text_content_ptr_len(
                module,
                &mut builder,
                pointer_type,
                content_ptr_len,
                raw,
            )?;
            args.push(ptr);
            args.push(len);
        } else {
            let converted = convert_to_foreign(&mut builder, raw, *ty);
            if ty.is_handle() {
                handle_vals.push(converted);
            }
            args.push(converted);
        }
    }
    let result_handle = abi.result.is_some_and(crate::mir::ForeignType::is_handle);
    let nslots = handle_vals.len() as u32 + u32::from(result_handle);
    let root_frame = emit_push_handle_frame(
        module,
        &mut builder,
        pointer_type,
        gc_root_push,
        nslots,
        &handle_vals,
    );
    let text_len_slot = if abi.result.is_some_and(crate::mir::ForeignType::is_text) {
        let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            types::I64.bytes(),
            0,
        ));
        args.push(builder.ins().stack_addr(pointer_type, slot, 0));
        Some(slot)
    } else {
        None
    };
    let call = builder.ins().call(import_ref, &args);
    if result_handle {
        let result = builder.inst_results(call)[0];
        if let Some(frame) = root_frame {
            let offset = (GC_ROOT_HEADER_BYTES + 8 * handle_vals.len() as u32) as i32;
            builder.ins().stack_store(result, frame, offset);
        }
    }
    emit_foreign_return(
        module,
        &mut builder,
        abi,
        function.result,
        call,
        text_len_slot,
        text_from_literal,
        pointer_type,
        root_frame,
        gc_root_pop,
    )?;
    builder.finalize();
    Ok(())
}

fn foreign_clif_signature(
    module: &ObjectModule,
    abi: &crate::mir::ForeignAbi,
    pointer_type: Type,
) -> cranelift_codegen::ir::Signature {
    let mut sig = module.make_signature();
    for ty in &abi.params {
        match ty {
            crate::mir::ForeignType::TextCopy => {
                sig.params.push(AbiParam::new(pointer_type));
                sig.params.push(AbiParam::new(types::I64));
            }
            other => sig
                .params
                .push(AbiParam::new(foreign_clif_type(*other, pointer_type))),
        }
    }
    match abi.result {
        Some(crate::mir::ForeignType::TextCopy) => {
            // `const uint8_t *fn(..., int64_t *out_len)` — copy, do not free.
            sig.params.push(AbiParam::new(pointer_type));
            sig.returns.push(AbiParam::new(pointer_type));
        }
        Some(result) => sig
            .returns
            .push(AbiParam::new(foreign_clif_type(result, pointer_type))),
        None => {}
    }
    sig
}

fn emit_foreign_return(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    abi: &crate::mir::ForeignAbi,
    mir_result: Option<MirType>,
    call: cranelift_codegen::ir::Inst,
    text_len_slot: Option<cranelift_codegen::ir::StackSlot>,
    text_from_literal: FuncId,
    pointer_type: Type,
    root_frame: Option<StackSlot>,
    gc_root_pop: FuncId,
) -> Result<(), CompileError> {
    match abi.result {
        Some(crate::mir::ForeignType::TextCopy) => {
            let ptr = builder.inst_results(call)[0];
            let slot = text_len_slot.ok_or_else(|| {
                CompileError::codegen("internal error: text result missing length slot")
            })?;
            let len_addr = builder.ins().stack_addr(pointer_type, slot, 0);
            let len = builder.ins().load(
                types::I64,
                cranelift_codegen::ir::MemFlagsData::trusted(),
                len_addr,
                0,
            );
            let helper = module.declare_func_in_func(text_from_literal, builder.func);
            let made = builder.ins().call(helper, &[ptr, len]);
            let frame = builder.inst_results(made)[0];
            emit_pop_handle_frame(module, builder, pointer_type, gc_root_pop, root_frame);
            builder.ins().return_(&[frame]);
        }
        Some(ty) => {
            let result = builder.inst_results(call)[0];
            let converted = convert_from_foreign(builder, result, ty, mir_result);
            emit_pop_handle_frame(module, builder, pointer_type, gc_root_pop, root_frame);
            builder.ins().return_(&[converted]);
        }
        None => {
            emit_pop_handle_frame(module, builder, pointer_type, gc_root_pop, root_frame);
            builder.ins().return_(&[]);
        }
    }
    Ok(())
}

fn intern_cstr(module: &mut ObjectModule, text: &str) -> Result<DataId, CompileError> {
    let counter = STRING_DATA_COUNTER.fetch_add(1, Ordering::Relaxed);
    let data_id = module
        .declare_data(&format!("mir_cstr_{counter}"), Linkage::Local, false, false)
        .map_err(map_module_error)?;
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let mut data = DataDescription::new();
    data.define(bytes.into_boxed_slice());
    module
        .define_data(data_id, &data)
        .map_err(map_module_error)?;
    Ok(data_id)
}

fn emit_host_table_thunk(
    module: &mut ObjectModule,
    clif_func: &mut ClifFunction,
    function: &mir::Function,
    pointer_type: Type,
    content_ptr_len: FuncId,
    text_from_literal: FuncId,
    host_resolve: FuncId,
    gc_root_push: FuncId,
    gc_root_pop: FuncId,
) -> Result<(), CompileError> {
    let abi = function.foreign.as_ref().ok_or_else(|| {
        CompileError::codegen("internal error: emit_host_table_thunk without ForeignAbi")
    })?;
    let ident_data = intern_cstr(module, &abi.ident)?;
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(clif_func, &mut fb_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let gv = module.declare_data_in_func(ident_data, builder.func);
    let name_ptr = builder.ins().global_value(pointer_type, gv);
    let resolve = module.declare_func_in_func(host_resolve, builder.func);
    let resolved = builder.ins().call(resolve, &[name_ptr]);
    let fnptr = builder.inst_results(resolved)[0];

    let block_params = builder.block_params(entry).to_vec();
    let mut args = Vec::new();
    let mut handle_vals = Vec::new();
    for (ty, raw) in abi.params.iter().zip(block_params) {
        if ty.is_text() {
            let (ptr, len) = emit_text_content_ptr_len(
                module,
                &mut builder,
                pointer_type,
                content_ptr_len,
                raw,
            )?;
            args.push(ptr);
            args.push(len);
        } else {
            let converted = convert_to_foreign(&mut builder, raw, *ty);
            if ty.is_handle() {
                handle_vals.push(converted);
            }
            args.push(converted);
        }
    }
    let result_handle = abi.result.is_some_and(crate::mir::ForeignType::is_handle);
    let nslots = handle_vals.len() as u32 + u32::from(result_handle);
    let root_frame = emit_push_handle_frame(
        module,
        &mut builder,
        pointer_type,
        gc_root_push,
        nslots,
        &handle_vals,
    );
    let text_len_slot = if abi.result.is_some_and(crate::mir::ForeignType::is_text) {
        let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            types::I64.bytes(),
            0,
        ));
        args.push(builder.ins().stack_addr(pointer_type, slot, 0));
        Some(slot)
    } else {
        None
    };
    let sigref = builder.import_signature(foreign_clif_signature(module, abi, pointer_type));
    let call = builder.ins().call_indirect(sigref, fnptr, &args);
    if result_handle {
        let result = builder.inst_results(call)[0];
        if let Some(frame) = root_frame {
            let offset = (GC_ROOT_HEADER_BYTES + 8 * handle_vals.len() as u32) as i32;
            builder.ins().stack_store(result, frame, offset);
        }
    }
    emit_foreign_return(
        module,
        &mut builder,
        abi,
        function.result,
        call,
        text_len_slot,
        text_from_literal,
        pointer_type,
        root_frame,
        gc_root_pop,
    )?;
    builder.finalize();
    Ok(())
}

fn export_sig_nibble(ty: Option<crate::mir::ForeignType>) -> i32 {
    match ty {
        None => 0,
        Some(crate::mir::ForeignType::I64) => 1,
        Some(crate::mir::ForeignType::F64) => 2,
        Some(crate::mir::ForeignType::Bool) => 3,
        Some(crate::mir::ForeignType::Char) => 4,
        Some(crate::mir::ForeignType::TextCopy) => 0,
        Some(crate::mir::ForeignType::ObjectHandle) => 5,
    }
}

fn export_sig_code(function: &mir::Function) -> Result<i32, CompileError> {
    let result = function.result.map(mir_scalar_foreign).transpose()?;
    let mut code = export_sig_nibble(result);
    for (index, param) in function.params.iter().take(4).enumerate() {
        let ty = mir_scalar_foreign(param.ty)?;
        code |= export_sig_nibble(Some(ty)) << (4 * (index + 1));
    }
    code |= (function.params.len() as i32) << 20;
    Ok(code)
}

fn emit_module_init(
    module: &mut ObjectModule,
    pointer_type: Type,
    register_export: FuncId,
    exports: &[(String, FuncId, i32)],
) -> Result<(), CompileError> {
    let names: Vec<DataId> = {
        let mut name_ids = Vec::with_capacity(exports.len());
        for (name, _, _) in exports {
            name_ids.push(intern_cstr(module, name)?);
        }
        name_ids
    };
    let sig = module.make_signature();
    let init_id = module
        .declare_function("simrt_module_init", Linkage::Export, &sig)
        .map_err(map_module_error)?;
    let mut ctx = module.make_context();
    ctx.func = ClifFunction::with_name_signature(UserFuncName::user(0, init_id.as_u32()), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let register = module.declare_func_in_func(register_export, builder.func);
    for ((_, func_id, sig_code), data_id) in exports.iter().zip(names) {
        let gv = module.declare_data_in_func(data_id, builder.func);
        let name_ptr = builder.ins().global_value(pointer_type, gv);
        let func_ref = module.declare_func_in_func(*func_id, builder.func);
        let addr = builder.ins().func_addr(pointer_type, func_ref);
        let code = builder.ins().iconst(types::I32, i64::from(*sig_code));
        builder.ins().call(register, &[name_ptr, addr, code]);
    }
    builder.ins().return_(&[]);
    builder.finalize();
    module
        .define_function(init_id, &mut ctx)
        .map_err(|error| map_define_error(error, &ctx.func))?;
    module.clear_context(&mut ctx);
    Ok(())
}

fn emit_text_content_ptr_len(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    pointer_type: Type,
    content_ptr_len: FuncId,
    frame: cranelift_codegen::ir::Value,
) -> Result<(cranelift_codegen::ir::Value, cranelift_codegen::ir::Value), CompileError> {
    let ptr_slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        pointer_type.bytes(),
        0,
    ));
    let len_slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        types::I64.bytes(),
        0,
    ));
    let ptr_addr = builder.ins().stack_addr(pointer_type, ptr_slot, 0);
    let len_addr = builder.ins().stack_addr(pointer_type, len_slot, 0);
    let helper = module.declare_func_in_func(content_ptr_len, builder.func);
    builder.ins().call(helper, &[frame, ptr_addr, len_addr]);
    let content_ptr = builder.ins().load(
        pointer_type,
        cranelift_codegen::ir::MemFlagsData::trusted(),
        ptr_addr,
        0,
    );
    let content_len = builder.ins().load(
        types::I64,
        cranelift_codegen::ir::MemFlagsData::trusted(),
        len_addr,
        0,
    );
    Ok((content_ptr, content_len))
}

fn convert_to_foreign(
    builder: &mut FunctionBuilder,
    value: cranelift_codegen::ir::Value,
    ty: crate::mir::ForeignType,
) -> cranelift_codegen::ir::Value {
    match ty {
        crate::mir::ForeignType::Bool => builder.ins().uextend(types::I32, value),
        crate::mir::ForeignType::Char => builder.ins().ireduce(types::I32, value),
        _ => value,
    }
}

fn convert_from_foreign(
    builder: &mut FunctionBuilder,
    value: cranelift_codegen::ir::Value,
    ty: crate::mir::ForeignType,
    result: Option<MirType>,
) -> cranelift_codegen::ir::Value {
    match ty {
        crate::mir::ForeignType::Bool => builder.ins().ireduce(types::I8, value),
        crate::mir::ForeignType::Char => builder.ins().uextend(types::I64, value),
        crate::mir::ForeignType::I64 if result == Some(MirType::Bool) => {
            builder.ins().ireduce(types::I8, value)
        }
        _ => value,
    }
}

fn mir_scalar_foreign(ty: MirType) -> Result<crate::mir::ForeignType, CompileError> {
    match ty {
        MirType::I64 => Ok(crate::mir::ForeignType::I64),
        MirType::F64 | MirType::LongF64 => Ok(crate::mir::ForeignType::F64),
        MirType::Bool => Ok(crate::mir::ForeignType::Bool),
        MirType::ObjectRef => Ok(crate::mir::ForeignType::ObjectHandle),
        other => Err(CompileError::codegen(format!(
            "cannot export Simula type {other} across a C boundary"
        ))),
    }
}

fn emit_c_export_wrapper(
    module: &mut ObjectModule,
    function: &mir::Function,
    export_name: &str,
    local_id: FuncId,
) -> Result<FuncId, CompileError> {
    let pointer_type = module.isa().pointer_type();
    let mut sig = module.make_signature();
    let mut foreign_params = Vec::with_capacity(function.params.len());
    for param in &function.params {
        let ty = mir_scalar_foreign(param.ty)?;
        foreign_params.push(ty);
        sig.params
            .push(AbiParam::new(foreign_clif_type(ty, pointer_type)));
    }
    let foreign_result = match function.result {
        Some(ty) => {
            let foreign = mir_scalar_foreign(ty)?;
            sig.returns
                .push(AbiParam::new(foreign_clif_type(foreign, pointer_type)));
            Some(foreign)
        }
        None => None,
    };
    let export_id = module
        .declare_function(export_name, Linkage::Export, &sig)
        .map_err(map_module_error)?;

    let mut ctx = module.make_context();
    ctx.func = ClifFunction::with_name_signature(UserFuncName::user(0, export_id.as_u32()), sig);

    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let local_ref = module.declare_func_in_func(local_id, builder.func);
    let block_params = builder.block_params(entry).to_vec();
    let mut args = Vec::with_capacity(foreign_params.len());
    for (ty, raw) in foreign_params.iter().zip(block_params) {
        args.push(convert_from_foreign(&mut builder, raw, *ty, None));
    }
    let call = builder.ins().call(local_ref, &args);
    match foreign_result {
        Some(ty) => {
            let result = builder.inst_results(call)[0];
            let converted = convert_to_foreign(&mut builder, result, ty);
            builder.ins().return_(&[converted]);
        }
        None => {
            builder.ins().return_(&[]);
        }
    }
    builder.finalize();

    module
        .define_function(export_id, &mut ctx)
        .map_err(|error| map_define_error(error, &ctx.func))?;
    module.clear_context(&mut ctx);
    Ok(export_id)
}

fn clif_type(ty: MirType, pointer_type: Type) -> Type {
    match ty {
        MirType::I64 => types::I64,
        // Booleans are plain 0/1 values in Cranelift (no dedicated `bN`
        // types since they were removed upstream); `I8` is the smallest
        // integer type and matches what `icmp` already produces.
        MirType::Bool => types::I8,
        MirType::F64 | MirType::LongF64 => types::F64,
        // Array locals hold an opaque descriptor pointer (see
        // `runtime/runtime.c`'s `SimrtArrayI64`), never the elements
        // themselves. Text locals hold a `SimrtTextFrame` pointer.
        // Object locals hold an opaque object pointer (or null for `none`).
        MirType::ArrayI64
        | MirType::ArrayF64
        | MirType::ArrayText
        | MirType::Text
        | MirType::ObjectRef
        | MirType::RefI64
        | MirType::FuncRef => pointer_type,
    }
}

/// Translates one MIR [`mir::Function`] into `clif_func`'s body.
///
/// `main`'s `Op::Return` is special-cased to always emit `iconst.i32 0` —
/// the MIR `main` function models a whole program's top-level statements,
/// not a value-returning procedure, so its `Op::Return { value }` is always
/// `None` (see [`mir::lower::lower_program`]). Every other function's
/// `Op::Return` returns `value` (or nothing, for a void procedure) as-is.
fn collect_debug_locals(
    function: &mir::Function,
    compiled: &cranelift_codegen::CompiledCode,
    isa: &dyn cranelift_codegen::isa::TargetIsa,
) -> Vec<DebugLocal> {
    let mut locals = Vec::new();
    let total = function.params.len() + function.locals.len();
    for index in 0..total {
        let local = function.local(LocalId(index));
        if !is_user_local_name(&local.name) {
            continue;
        }
        let label = ValueLabel::from_u32(index as u32);
        let locations = compiled
            .value_labels_ranges
            .get(&label)
            .map(|ranges| {
                ranges
                    .iter()
                    .filter_map(|range| {
                        let loc = match range.loc {
                            LabelValueLoc::Reg(reg) => {
                                let dwarf_reg = isa.map_regalloc_reg_to_dwarf(reg).ok()?;
                                LocalLocation::Reg(dwarf_reg)
                            }
                            LabelValueLoc::CFAOffset(offset) => LocalLocation::CfaOffset(offset),
                        };
                        Some(LocalLocRange {
                            start: range.start,
                            end: range.end,
                            loc,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        locals.push(DebugLocal {
            name: local.name.clone(),
            is_param: index < function.params.len(),
            ty: debug_value_type(local.ty),
            class_qual: local.class_qual.clone(),
            locations,
        });
    }
    locals
}

fn is_user_local_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('%')
}

/// Pointer-typed MIR locals the native collector must see. `FuncRef` is not
/// among them: the function heap is never swept. `RefI64` is, because
/// `FieldAddr` interiors and name-thunk cells live there.
fn is_gc_ptr_ty(ty: MirType) -> bool {
    matches!(
        ty,
        MirType::ObjectRef
            | MirType::Text
            | MirType::ArrayI64
            | MirType::ArrayF64
            | MirType::ArrayText
            | MirType::RefI64
    )
}

/// Stack home for a MIR local: either a dedicated slot (addr-taken scalars,
/// DWARF) or an offset into the function's GC root frame.
#[derive(Clone, Copy)]
struct StackHome {
    slot: StackSlot,
    offset: i32,
}

/// Header of `SimrtGcRootFrame` (`runtime/gc.h`): prev + nslots + pad.
const GC_ROOT_HEADER_BYTES: u32 = 16;

fn emit_gc_root_pop(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    runtime: &RuntimeFuncs,
    frame: Option<StackSlot>,
) {
    let Some(slot) = frame else {
        return;
    };
    let pointer_type = module.isa().pointer_type();
    let addr = builder.ins().stack_addr(pointer_type, slot, 0);
    let pop = module.declare_func_in_func(runtime.gc_root_pop, builder.func);
    builder.ins().call(pop, &[addr]);
}

fn reload_addr_taken_locals(
    builder: &mut FunctionBuilder<'_>,
    function: &mir::Function,
    vars: &[Variable],
    homes: &[Option<StackHome>],
    pointer_type: Type,
    track_debug: bool,
) {
    for (index, home) in homes.iter().enumerate() {
        let Some(home) = home else {
            continue;
        };
        let local = function.local(LocalId(index));
        // Skip homes that exist only for DWARF and were never address-taken.
        let addr_taken = function
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(&s.op, Op::LocalAddr { local, .. } if local.0 == index));
        if !addr_taken {
            continue;
        }
        let ty = clif_type(local.ty, pointer_type);
        let value = if local.ty == MirType::Bool {
            let wide = builder.ins().stack_load(types::I64, home.slot, home.offset);
            builder.ins().ireduce(types::I8, wide)
        } else {
            builder.ins().stack_load(ty, home.slot, home.offset)
        };
        def_local(
            builder,
            function,
            vars,
            homes,
            LocalId(index),
            value,
            track_debug,
        );
    }
}

fn debug_value_type(ty: MirType) -> DebugValueType {
    match ty {
        MirType::I64 => DebugValueType::I64,
        MirType::Bool => DebugValueType::Bool,
        MirType::F64 | MirType::LongF64 => DebugValueType::F64,
        MirType::Text => DebugValueType::Text,
        MirType::ArrayI64 => DebugValueType::ArrayI64,
        // Same descriptor header layout as i64 arrays; DWARF reuses that type.
        MirType::ArrayF64 => DebugValueType::ArrayI64,
        MirType::ArrayText => DebugValueType::ArrayText,
        MirType::ObjectRef | MirType::RefI64 | MirType::FuncRef => DebugValueType::Pointer,
    }
}

fn def_local(
    builder: &mut FunctionBuilder<'_>,
    function: &mir::Function,
    vars: &[Variable],
    homes: &[Option<StackHome>],
    id: LocalId,
    value: cranelift_codegen::ir::Value,
    track_debug: bool,
) {
    builder.def_var(vars[id.0], value);
    if let Some(Some(home)) = homes.get(id.0) {
        builder.ins().stack_store(value, home.slot, home.offset);
    }
    if track_debug && is_user_local_name(&function.local(id).name) {
        builder.set_val_label(value, ValueLabel::from_u32(id.0 as u32));
    }
}

fn zero_for_type(
    builder: &mut FunctionBuilder<'_>,
    ty: MirType,
    pointer_type: Type,
) -> cranelift_codegen::ir::Value {
    match ty {
        MirType::F64 | MirType::LongF64 => builder
            .ins()
            .f64const(cranelift_codegen::ir::immediates::Ieee64::with_bits(0)),
        MirType::Bool => builder.ins().iconst(types::I8, 0),
        MirType::I64 => builder.ins().iconst(types::I64, 0),
        MirType::ArrayI64
        | MirType::ArrayF64
        | MirType::ArrayText
        | MirType::Text
        | MirType::ObjectRef
        | MirType::RefI64
        | MirType::FuncRef => builder.ins().iconst(pointer_type, 0),
    }
}

/// Keeps named locals live until return so DWARF value labels cover unused vars.
fn keep_alive_named_locals(
    builder: &mut FunctionBuilder<'_>,
    _function: &mir::Function,
    vars: &[Variable],
    homes: &[Option<StackHome>],
) {
    for (index, home) in homes.iter().enumerate() {
        let Some(home) = home else {
            continue;
        };
        let id = LocalId(index);
        let value = builder.use_var(vars[id.0]);
        builder.set_val_label(value, ValueLabel::from_u32(id.0 as u32));
        builder.ins().stack_store(value, home.slot, home.offset);
    }
}

/// Parameters arrive as the entry block's Cranelift block parameters (via
/// `append_block_params_for_function_params`); they're copied into their
/// `Variable`s right after switching to the entry block, before any op runs.
#[allow(clippy::too_many_arguments)]
fn emit_function(
    module: &mut ObjectModule,
    clif_func: &mut ClifFunction,
    function: &mir::Function,
    strings: &[String],
    pointer_type: Type,
    runtime: &RuntimeFuncs,
    proc_ids: &HashMap<String, FuncId>,
    string_data: &mut HashMap<usize, DataId>,
    debug_source: Option<&SourceFile>,
) -> Result<(), CompileError> {
    let is_main = function.name == "main";
    let track_debug = debug_source.is_some();
    if track_debug {
        clif_func.dfg.collect_debug_info();
    }
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(clif_func, &mut fb_ctx);

    let total_locals = function.params.len() + function.locals.len();
    let vars: Vec<Variable> = (0..total_locals)
        .map(|index| {
            let local = function.local(LocalId(index));
            builder.declare_var(clif_type(local.ty, pointer_type))
        })
        .collect();

    // Stack homes for `-g` named locals (DWARF), for any local whose address
    // is taken (`LocalAddr` — recursive call-by-name MVP), and for every
    // GC-typed local (precise root-frame slots the collector walks).
    // Addr-taken booleans use an 8-byte home so name-thunk `LoadRefI64` /
    // `StoreRefI64` helpers see a full i64 cell (low byte holds 0/1).
    let addr_taken: std::collections::HashSet<usize> = function
        .blocks
        .iter()
        .flat_map(|block| &block.ops)
        .filter_map(|spanned| match &spanned.op {
            Op::LocalAddr { local, .. } => Some(local.0),
            _ => None,
        })
        .collect();
    let gc_indices: Vec<usize> = (0..total_locals)
        .filter(|&index| is_gc_ptr_ty(function.local(LocalId(index)).ty))
        .collect();
    let nslots = gc_indices.len() as u32;
    let gc_frame: Option<StackSlot> = if nslots > 0 {
        Some(
            builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                GC_ROOT_HEADER_BYTES + 8 * nslots,
                3,
            )),
        )
    } else {
        None
    };
    let homes: Vec<Option<StackHome>> = (0..total_locals)
        .map(|index| {
            if let Some(slot_index) = gc_indices.iter().position(|&i| i == index) {
                let offset = (GC_ROOT_HEADER_BYTES + 8 * slot_index as u32) as i32;
                return Some(StackHome {
                    slot: gc_frame.expect("GC local implies a root frame"),
                    offset,
                });
            }
            let local = function.local(LocalId(index));
            let needs_home =
                addr_taken.contains(&index) || (track_debug && is_user_local_name(&local.name));
            if needs_home {
                let ty = clif_type(local.ty, pointer_type);
                let bytes = if addr_taken.contains(&index) && local.ty == MirType::Bool {
                    8
                } else {
                    ty.bytes()
                };
                Some(StackHome {
                    slot: builder.create_sized_stack_slot(
                        cranelift_codegen::ir::StackSlotData::new(
                            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            bytes,
                            0,
                        ),
                    ),
                    offset: 0,
                })
            } else {
                None
            }
        })
        .collect();
    debug_assert_eq!(GC_ROOT_HEADER_BYTES, 16);
    debug_assert_eq!(nslots as usize, gc_indices.len());
    for (slot_index, &local_index) in gc_indices.iter().enumerate() {
        let home = homes[local_index]
            .as_ref()
            .expect("every GC-typed local has a root-frame home");
        debug_assert_eq!(
            home.offset,
            (GC_ROOT_HEADER_BYTES + 8 * slot_index as u32) as i32
        );
    }

    let clif_blocks: Vec<ClifBlock> = function
        .blocks
        .iter()
        .map(|_| builder.create_block())
        .collect();
    builder.append_block_params_for_function_params(clif_blocks[function.entry.0]);

    for (block, &clif_block) in function.blocks.iter().zip(&clif_blocks) {
        builder.switch_to_block(clif_block);
        if block.id == function.entry {
            if let Some(frame) = gc_frame {
                let addr = builder.ins().stack_addr(pointer_type, frame, 0);
                let n = builder.ins().iconst(types::I64, i64::from(nslots));
                let push = module.declare_func_in_func(runtime.gc_root_push, builder.func);
                builder.ins().call(push, &[addr, n]);
            }
            if !function.params.is_empty() {
                let block_params: Vec<_> = builder.block_params(clif_block).to_vec();
                for (index, value) in block_params.into_iter().enumerate() {
                    def_local(
                        &mut builder,
                        function,
                        &vars,
                        &homes,
                        LocalId(index),
                        value,
                        track_debug,
                    );
                }
            }
            if track_debug {
                for index in function.params.len()..total_locals {
                    let local = function.local(LocalId(index));
                    if !is_user_local_name(&local.name) {
                        continue;
                    }
                    let zero = zero_for_type(&mut builder, local.ty, pointer_type);
                    def_local(
                        &mut builder,
                        function,
                        &vars,
                        &homes,
                        LocalId(index),
                        zero,
                        track_debug,
                    );
                }
            }
        }
        let mut emitted_terminator = false;
        for spanned in &block.ops {
            if emitted_terminator {
                // Malformed MIR (ops after Jump/Branch/Return) — ignore the
                // dead tail rather than panicking inside Cranelift.
                break;
            }
            if let Some(source) = debug_source
                && (spanned.span.start != 0 || spanned.span.end != 0)
            {
                let (line, column) =
                    crate::codegen::sourcemap::span_to_line_col(&source.text, spanned.span.start);
                builder.set_srcloc(encode_srcloc(line, column));
            }
            emit_op(
                module,
                &mut builder,
                function,
                &vars,
                &homes,
                gc_frame,
                &clif_blocks,
                runtime,
                proc_ids,
                is_main,
                strings,
                string_data,
                &spanned.op,
                track_debug,
            )?;
            emitted_terminator = matches!(
                spanned.op,
                Op::Jump { .. }
                    | Op::GotoEscape { .. }
                    | Op::Branch { .. }
                    | Op::Return { .. }
                    | Op::Abort { .. }
            );
        }
        // Unresolved labels / incomplete gotos can leave terminator-less MIR
        // blocks; trap rather than emit invalid Cranelift CFGs.
        if !emitted_terminator {
            builder
                .ins()
                .trap(cranelift_codegen::ir::TrapCode::STACK_OVERFLOW);
        }
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_op(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    function: &mir::Function,
    vars: &[Variable],
    homes: &[Option<StackHome>],
    gc_frame: Option<StackSlot>,
    clif_blocks: &[ClifBlock],
    runtime: &RuntimeFuncs,
    proc_ids: &HashMap<String, FuncId>,
    is_main: bool,
    strings: &[String],
    string_data: &mut HashMap<usize, DataId>,
    op: &Op,
    track_debug: bool,
) -> Result<(), CompileError> {
    match op {
        Op::ConstI64 { dest, value } => {
            let value = builder.ins().iconst(types::I64, *value);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::ConstF64 { dest, value } => {
            let value =
                builder
                    .ins()
                    .f64const(cranelift_codegen::ir::immediates::Ieee64::with_bits(
                        value.to_bits(),
                    ));
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::I64ToF64 { dest, src } => {
            let src = builder.use_var(vars[src.0]);
            let value = builder.ins().fcvt_from_sint(types::F64, src);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::F64ToI64 { dest, src } => {
            // Simula real→integer uses `entier` (floor toward −∞).
            let src = builder.use_var(vars[src.0]);
            let floored = builder.ins().floor(src);
            let value = builder.ins().fcvt_to_sint(types::I64, floored);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::ConstBool { dest, value } => {
            let value = builder.ins().iconst(types::I8, i64::from(*value));
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::Copy { dest, src } => {
            let value = builder.use_var(vars[src.0]);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::ArrayCopy { dest, src } => {
            let src = builder.use_var(vars[src.0]);
            let copy = match function.local(*dest).ty {
                MirType::ArrayText => runtime.array_copy_text,
                MirType::ArrayF64 => runtime.array_copy_f64,
                _ => runtime.array_copy_i64,
            };
            let copy = module.declare_func_in_func(copy, builder.func);
            let call = builder.ins().call(copy, &[src]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::Binary {
            dest,
            op,
            left,
            right,
        } => {
            let left_id = *left;
            let left = builder.use_var(vars[left.0]);
            let right = builder.use_var(vars[right.0]);
            let is_f64 = function.local(left_id).ty.is_float();
            let value = if is_f64 {
                match op {
                    BinOp::Add => builder.ins().fadd(left, right),
                    BinOp::Sub => builder.ins().fsub(left, right),
                    BinOp::Mul => builder.ins().fmul(left, right),
                    BinOp::Div => builder.ins().fdiv(left, right),
                    BinOp::Pow => {
                        let f64_pow = module.declare_func_in_func(runtime.f64_pow, builder.func);
                        let call = builder.ins().call(f64_pow, &[left, right]);
                        builder.inst_results(call)[0]
                    }
                    BinOp::IntDiv | BinOp::And | BinOp::Or => {
                        return Err(CompileError::codegen(
                            "native codegen: integer/boolean binary op on f64 operands",
                        ));
                    }
                }
            } else {
                match op {
                    BinOp::Add => builder.ins().iadd(left, right),
                    BinOp::Sub => builder.ins().isub(left, right),
                    BinOp::Mul => builder.ins().imul(left, right),
                    // Simula integer division truncates toward zero, matching
                    // Cranelift's `sdiv` (signed division). `//` is IntDiv;
                    // `/` on integers is lowered to f64 Div above.
                    BinOp::Div | BinOp::IntDiv => builder.ins().sdiv(left, right),
                    BinOp::And => builder.ins().band(left, right),
                    BinOp::Or => builder.ins().bor(left, right),
                    BinOp::Pow => {
                        return Err(CompileError::codegen(
                            "native codegen: pow requires f64 operands",
                        ));
                    }
                }
            };
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::Unary { dest, op, src } => {
            let src_id = *src;
            let src = builder.use_var(vars[src.0]);
            let value = match op {
                UnOp::Neg => {
                    if function.local(src_id).ty.is_float() {
                        builder.ins().fneg(src)
                    } else {
                        builder.ins().ineg(src)
                    }
                }
                UnOp::Not => {
                    // `src` is always 0 or 1 (a MIR `Bool`); flip the low bit.
                    let one = builder.ins().iconst(types::I8, 1);
                    builder.ins().bxor(src, one)
                }
            };
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::LoadLocal { dest, local } => {
            let value = builder.use_var(vars[local.0]);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::StoreLocal { local, src } => {
            let value = builder.use_var(vars[src.0]);
            def_local(builder, function, vars, homes, *local, value, track_debug);
        }
        Op::LocalAddr { dest, local } => {
            let pointer_type = module.isa().pointer_type();
            let Some(Some(home)) = homes.get(local.0).copied() else {
                return Err(CompileError::codegen(format!(
                    "internal error: LocalAddr of %{} without a stack home",
                    local.0
                )));
            };
            // Ensure the home reflects the current SSA value before taking its address.
            let current = builder.use_var(vars[local.0]);
            let stored = if function.local(*local).ty == MirType::Bool {
                // Addr-taken bool homes are 8 bytes for name-thunk i64 ABI.
                builder.ins().uextend(types::I64, current)
            } else {
                current
            };
            builder.ins().stack_store(stored, home.slot, home.offset);
            let addr = builder
                .ins()
                .stack_addr(pointer_type, home.slot, home.offset);
            def_local(builder, function, vars, homes, *dest, addr, track_debug);
        }
        Op::FieldAddr {
            dest,
            object,
            offset,
        } => {
            let object = builder.use_var(vars[object.0]);
            let addr = builder.ins().iadd_imm(object, *offset);
            def_local(builder, function, vars, homes, *dest, addr, track_debug);
        }
        Op::LoadRefI64 { dest, ptr, offset } => {
            let ptr = builder.use_var(vars[ptr.0]);
            let value = builder.ins().load(
                types::I64,
                cranelift_codegen::ir::MemFlagsData::trusted(),
                ptr,
                *offset as i32,
            );
            // Cells are eight bytes wide whatever they hold, so a narrower or
            // differently-typed destination needs the value reinterpreted.
            let value = match function.local(*dest).ty {
                MirType::Bool => builder.ins().ireduce(types::I8, value),
                MirType::F64 | MirType::LongF64 => builder.ins().bitcast(
                    types::F64,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    value,
                ),
                _ => value,
            };
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::StoreRefI64 { ptr, src, offset } => {
            let ptr = builder.use_var(vars[ptr.0]);
            let value = builder.use_var(vars[src.0]);
            let value = match function.local(*src).ty {
                MirType::Bool => builder.ins().uextend(types::I64, value),
                MirType::F64 | MirType::LongF64 => builder.ins().bitcast(
                    types::I64,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    value,
                ),
                _ => value,
            };
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::trusted(),
                value,
                ptr,
                *offset as i32,
            );
        }
        Op::StackAlloc { dest, bytes } => {
            let pointer_type = module.isa().pointer_type();
            let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                *bytes as u32,
                0,
            ));
            let addr = builder.ins().stack_addr(pointer_type, slot, 0);
            def_local(builder, function, vars, homes, *dest, addr, track_debug);
        }
        Op::HeapAlloc { dest, bytes } => {
            let size = builder.use_var(vars[bytes.0]);
            let class_id = builder.ins().iconst(types::I64, 0);
            let object_alloc = module.declare_func_in_func(runtime.object_alloc, builder.func);
            let call = builder.ins().call(object_alloc, &[size, class_id]);
            let pointer = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, pointer, track_debug);
        }
        Op::FuncAddr { dest, name } => {
            let pointer_type = module.isa().pointer_type();
            let &func_id = proc_ids.get(name).ok_or_else(|| {
                CompileError::codegen(format!(
                    "native codegen: func_addr of unknown procedure '{name}'"
                ))
            })?;
            let func_ref = module.declare_func_in_func(func_id, builder.func);
            let addr = builder.ins().func_addr(pointer_type, func_ref);
            def_local(builder, function, vars, homes, *dest, addr, track_debug);
        }
        Op::CallIndirect {
            dest,
            callee,
            args,
            sig,
        } => {
            let pointer_type = module.isa().pointer_type();
            let mut signature = module.make_signature();
            for ty in &sig.params {
                signature
                    .params
                    .push(AbiParam::new(clif_type(*ty, pointer_type)));
            }
            if let Some(result) = sig.result {
                signature
                    .returns
                    .push(AbiParam::new(clif_type(result, pointer_type)));
            }
            let sigref = builder.import_signature(signature);
            let callee = builder.use_var(vars[callee.0]);
            let arg_values: Vec<_> = args
                .iter()
                .map(|arg| builder.use_var(vars[arg.0]))
                .collect();
            let call = builder.ins().call_indirect(sigref, callee, &arg_values);
            if let Some(dest) = dest {
                let result = builder.inst_results(call)[0];
                def_local(builder, function, vars, homes, *dest, result, track_debug);
            }
            reload_addr_taken_locals(builder, function, vars, homes, pointer_type, track_debug);
        }
        Op::Compare {
            dest,
            op,
            left,
            right,
        } => {
            let left_id = *left;
            let left = builder.use_var(vars[left.0]);
            let right = builder.use_var(vars[right.0]);
            let value = if function.local(left_id).ty.is_float() {
                let cc = match op {
                    CmpOp::Eq => FloatCC::Equal,
                    CmpOp::Ne => FloatCC::NotEqual,
                    CmpOp::Lt => FloatCC::LessThan,
                    CmpOp::Le => FloatCC::LessThanOrEqual,
                    CmpOp::Gt => FloatCC::GreaterThan,
                    CmpOp::Ge => FloatCC::GreaterThanOrEqual,
                };
                builder.ins().fcmp(cc, left, right)
            } else {
                let cc = match op {
                    CmpOp::Eq => IntCC::Equal,
                    CmpOp::Ne => IntCC::NotEqual,
                    CmpOp::Lt => IntCC::SignedLessThan,
                    CmpOp::Le => IntCC::SignedLessThanOrEqual,
                    CmpOp::Gt => IntCC::SignedGreaterThan,
                    CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                };
                builder.ins().icmp(cc, left, right)
            };
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::Jump { target } => {
            builder.ins().jump(target_block(clif_blocks, *target), &[]);
        }
        Op::GotoEscape { label } => {
            // Non-local goto (§5.4.18) is handled by the MIR interpreter via
            // stack unwind. AOT does not implement cross-activation transfer;
            // trap if the path is taken (corpus units that compile historically
            // only hit this on rare error paths).
            let _ = label;
            builder
                .ins()
                .trap(cranelift_codegen::ir::TrapCode::STACK_OVERFLOW);
        }
        Op::Branch {
            cond,
            then_block,
            else_block,
        } => {
            let cond = builder.use_var(vars[cond.0]);
            builder.ins().brif(
                cond,
                target_block(clif_blocks, *then_block),
                &[],
                target_block(clif_blocks, *else_block),
                &[],
            );
        }
        Op::CallOutText { string_id } => {
            emit_out_text(
                module,
                builder,
                runtime.out_text,
                strings,
                string_data,
                *string_id,
            )?;
        }
        Op::CallOutTextLocal { src } => {
            let frame = builder.use_var(vars[src.0]);
            emit_out_text_local(module, builder, runtime, frame)?;
        }
        Op::CallOutImage => {
            let out_image = module.declare_func_in_func(runtime.out_image, builder.func);
            builder.ins().call(out_image, &[]);
        }
        Op::CallInLine { dest } => {
            let in_line = module.declare_func_in_func(runtime.in_line, builder.func);
            let call = builder.ins().call(in_line, &[]);
            let frame = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::CallOutInt { value, width } => {
            let value = builder.use_var(vars[value.0]);
            let width = builder.use_var(vars[width.0]);
            let out_int = module.declare_func_in_func(runtime.out_int, builder.func);
            builder.ins().call(out_int, &[value, width]);
        }
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
            let value = builder.use_var(vars[value.0]);
            let digits = builder.use_var(vars[digits.0]);
            let width = builder.use_var(vars[width.0]);
            let exp = builder.ins().iconst(types::I64, exp_digits);
            let f = module.declare_func_in_func(runtime.out_real, builder.func);
            builder.ins().call(f, &[value, digits, width, exp]);
        }
        Op::CallOutFix {
            value,
            digits,
            width,
        } => {
            let value = builder.use_var(vars[value.0]);
            let digits = builder.use_var(vars[digits.0]);
            let width = builder.use_var(vars[width.0]);
            let f = module.declare_func_in_func(runtime.out_fix, builder.func);
            builder.ins().call(f, &[value, digits, width]);
        }
        Op::CallOutFrac {
            value,
            digits,
            width,
        } => {
            let value = builder.use_var(vars[value.0]);
            let digits = builder.use_var(vars[digits.0]);
            let width = builder.use_var(vars[width.0]);
            let f = module.declare_func_in_func(runtime.out_frac, builder.func);
            builder.ins().call(f, &[value, digits, width]);
        }
        Op::CallOutChar { ch } => {
            let ch = builder.use_var(vars[ch.0]);
            let out_char = module.declare_func_in_func(runtime.out_char, builder.func);
            builder.ins().call(out_char, &[ch]);
        }
        Op::CallBreakOutImage => {
            let f = module.declare_func_in_func(runtime.break_out_image, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::CallInImage => {
            let f = module.declare_func_in_func(runtime.in_image, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::CallInChar { dest } => {
            let f = module.declare_func_in_func(runtime.in_char, builder.func);
            let call = builder.ins().call(f, &[]);
            let raw = builder.inst_results(call)[0];
            let value = builder.ins().sextend(types::I64, raw);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallEndfile { dest } => {
            let f = module.declare_func_in_func(runtime.endfile, builder.func);
            let call = builder.ins().call(f, &[]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallSysIn { dest } => {
            let f = module.declare_func_in_func(runtime.sysin, builder.func);
            let call = builder.ins().call(f, &[]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallSysOut { dest } => {
            let f = module.declare_func_in_func(runtime.sysout, builder.func);
            let call = builder.ins().call(f, &[]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioRegisterFile { object, path, mode } => {
            let object = builder.use_var(vars[object.0]);
            let path = builder.use_var(vars[path.0]);
            let mode = builder.ins().iconst(types::I64, *mode);
            let f = module.declare_func_in_func(runtime.basicio_register_file, builder.func);
            builder.ins().call(f, &[object, path, mode]);
        }
        Op::CallBasicioOpen {
            dest,
            object,
            fileimage,
        } => {
            let object = builder.use_var(vars[object.0]);
            let fileimage = builder.use_var(vars[fileimage.0]);
            let f = module.declare_func_in_func(runtime.basicio_open, builder.func);
            let call = builder.ins().call(f, &[object, fileimage]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioClose { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_close, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioIsOpen { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_isopen, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioOutText { object, text } => {
            let object = builder.use_var(vars[object.0]);
            let text = builder.use_var(vars[text.0]);
            let f = module.declare_func_in_func(runtime.basicio_outtext, builder.func);
            builder.ins().call(f, &[object, text]);
        }
        Op::CallBasicioOutChar { object, ch } => {
            let object = builder.use_var(vars[object.0]);
            let ch = builder.use_var(vars[ch.0]);
            let f = module.declare_func_in_func(runtime.basicio_outchar, builder.func);
            builder.ins().call(f, &[object, ch]);
        }
        Op::CallBasicioOutImage { object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_outimage, builder.func);
            builder.ins().call(f, &[object]);
        }
        Op::CallBasicioBreakOutImage { object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_breakoutimage, builder.func);
            builder.ins().call(f, &[object]);
        }
        Op::CallBasicioInImage { object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_inimage, builder.func);
            builder.ins().call(f, &[object]);
        }
        Op::CallBasicioInChar { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_inchar, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let value = builder.ins().sextend(types::I64, raw);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioLastItem { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_lastitem, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioInInt { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_inint, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioInReal { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_inreal, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioInFrac { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_infrac, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioInText {
            dest,
            object,
            width,
        } => {
            let object = builder.use_var(vars[object.0]);
            let width = builder.use_var(vars[width.0]);
            let f = module.declare_func_in_func(runtime.basicio_intext, builder.func);
            let call = builder.ins().call(f, &[object, width]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioEndfile { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_endfile, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioOpenByte { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_open_byte, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioInByte { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_inbyte, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let value = builder.ins().sextend(types::I64, raw);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioOutByte { object, value } => {
            let object = builder.use_var(vars[object.0]);
            let value = builder.use_var(vars[value.0]);
            let f = module.declare_func_in_func(runtime.basicio_outbyte, builder.func);
            builder.ins().call(f, &[object, value]);
        }
        Op::CallBasicioLocate { object, loc } => {
            let object = builder.use_var(vars[object.0]);
            let loc = builder.use_var(vars[loc.0]);
            let f = module.declare_func_in_func(runtime.basicio_locate, builder.func);
            builder.ins().call(f, &[object, loc]);
        }
        Op::CallBasicioLocation { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_location, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioLastloc { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_lastloc, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioOutReal {
            object,
            value,
            digits,
            width,
            exp_digits,
        } => {
            let object = builder.use_var(vars[object.0]);
            let value = builder.use_var(vars[value.0]);
            let digits = builder.use_var(vars[digits.0]);
            let width = builder.use_var(vars[width.0]);
            let exp = builder.ins().iconst(types::I64, *exp_digits);
            let f = module.declare_func_in_func(runtime.basicio_outreal, builder.func);
            builder.ins().call(f, &[object, value, digits, width, exp]);
        }
        Op::CallBasicioOutFix {
            object,
            value,
            digits,
            width,
        } => {
            let object = builder.use_var(vars[object.0]);
            let value = builder.use_var(vars[value.0]);
            let digits = builder.use_var(vars[digits.0]);
            let width = builder.use_var(vars[width.0]);
            let f = module.declare_func_in_func(runtime.basicio_outfix, builder.func);
            builder.ins().call(f, &[object, value, digits, width]);
        }
        Op::CallBasicioOutFrac {
            object,
            value,
            digits,
            width,
        } => {
            let object = builder.use_var(vars[object.0]);
            let value = builder.use_var(vars[value.0]);
            let digits = builder.use_var(vars[digits.0]);
            let width = builder.use_var(vars[width.0]);
            let f = module.declare_func_in_func(runtime.basicio_outfrac, builder.func);
            builder.ins().call(f, &[object, value, digits, width]);
        }
        Op::CallBasicioOutInt {
            object,
            value,
            width,
        } => {
            let object = builder.use_var(vars[object.0]);
            let value = builder.use_var(vars[value.0]);
            let width = builder.use_var(vars[width.0]);
            let f = module.declare_func_in_func(runtime.basicio_outint, builder.func);
            builder.ins().call(f, &[object, value, width]);
        }
        Op::CallBasicioLine { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_line, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioImage { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_image, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioPos { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_pos, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioLength { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_length, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioSetImage { object, text } => {
            let object = builder.use_var(vars[object.0]);
            let text = builder.use_var(vars[text.0]);
            let f = module.declare_func_in_func(runtime.basicio_set_image, builder.func);
            builder.ins().call(f, &[object, text]);
        }
        Op::CallBasicioSetpos { object, index } => {
            let object = builder.use_var(vars[object.0]);
            let index = builder.use_var(vars[index.0]);
            let f = module.declare_func_in_func(runtime.basicio_setpos, builder.func);
            builder.ins().call(f, &[object, index]);
        }
        Op::CallBasicioFilename { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_filename, builder.func);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioSetAccess { dest, object, mode } => {
            let object = builder.use_var(vars[object.0]);
            let mode = builder.use_var(vars[mode.0]);
            let f = module.declare_func_in_func(runtime.basicio_setaccess, builder.func);
            let call = builder.ins().call(f, &[object, mode]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioEject { object, line } => {
            let object = builder.use_var(vars[object.0]);
            let line = builder.use_var(vars[line.0]);
            let f = module.declare_func_in_func(runtime.basicio_eject, builder.func);
            builder.ins().call(f, &[object, line]);
        }
        Op::CallBasicioLinesPerPage { dest, object, n } => {
            let object = builder.use_var(vars[object.0]);
            let n = builder.use_var(vars[n.0]);
            let f = module.declare_func_in_func(runtime.basicio_linesperpage, builder.func);
            let call = builder.ins().call(f, &[object, n]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallBasicioInRecord { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let f = module.declare_func_in_func(runtime.basicio_inrecord, builder.func);
            let call = builder.ins().call(f, &[object]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallTerminateProgram => {
            let f = module.declare_func_in_func(runtime.terminate_program, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::CallEnv { dest, name, args } => {
            let func_id = match name.as_str() {
                "decimalmark" => runtime.decimalmark,
                "lowten" => runtime.lowten,
                "sqrt" => runtime.sqrt,
                "sin" => runtime.sin,
                "cos" => runtime.cos,
                "tan" => runtime.tan,
                "ln" => runtime.ln,
                "exp" => runtime.exp,
                "arctan" => runtime.arctan,
                "cotan" => runtime.cotan,
                "arcsin" => runtime.arcsin,
                "arccos" => runtime.arccos,
                "arctan2" => runtime.arctan2,
                "addepsilon" => runtime.addepsilon,
                "subepsilon" => runtime.subepsilon,
                "mod" => runtime.mod_i64,
                "rem" => runtime.rem_i64,
                "sign" => runtime.sign,
                "abs_int" => runtime.abs_int,
                "abs_real" => runtime.abs_real,
                "draw" => runtime.draw,
                "randint" => runtime.randint,
                "uniform" => runtime.uniform,
                "normal" => runtime.normal,
                "negexp" => runtime.negexp,
                "poisson" => runtime.poisson,
                "erlang" => runtime.erlang,
                "discrete" => runtime.discrete,
                "histd" => runtime.histd,
                "linear" => runtime.linear,
                "histo" => runtime.histo,
                "datetime" => runtime.datetime,
                "cputime" => runtime.cputime,
                "clocktime" => runtime.clocktime,
                "sinh" => runtime.sinh,
                "cosh" => runtime.cosh,
                "tanh" => runtime.tanh,
                "log10" => runtime.log10,
                "digit" => runtime.digit,
                "letter" => runtime.letter,
                "char" => runtime.char_code,
                "isochar" => runtime.isochar,
                "rank" => runtime.rank,
                "isorank" => runtime.isorank,
                "max_int" => runtime.max_int,
                "min_int" => runtime.min_int,
                "max_real" => runtime.max_real,
                "min_real" => runtime.min_real,
                "error" => runtime.error_text,
                "current_lowten" => runtime.current_lowten,
                "current_decimalmark" => runtime.current_decimalmark,
                "lowerbound" => runtime.array_lowerbound,
                "upperbound" => runtime.array_upperbound,
                other => {
                    return Err(CompileError::codegen(format!(
                        "MIR cranelift: unknown ENVIRONMENT helper '{other}'"
                    )));
                }
            };
            let arg_vals: Vec<_> = args
                .iter()
                .map(|arg| builder.use_var(vars[arg.0]))
                .collect();
            let callee = module.declare_func_in_func(func_id, builder.func);
            let call = builder.ins().call(callee, &arg_vals);
            let mut value = builder.inst_results(call)[0];
            if name == "draw" || name == "digit" || name == "letter" {
                // Runtime returns i64 0/1; MIR bool locals are i8.
                let zero = builder.ins().iconst(types::I64, 0);
                value = builder.ins().icmp(IntCC::NotEqual, value, zero);
            }
            def_local(builder, function, vars, homes, *dest, value, track_debug);
            // Random helpers mutate the stream through a LocalAddr pointer.
            let pointer_type = module.isa().pointer_type();
            reload_addr_taken_locals(builder, function, vars, homes, pointer_type, track_debug);
        }
        Op::CallFileExists { dest, path } => {
            let path = builder.use_var(vars[path.0]);
            let file_exists = module.declare_func_in_func(runtime.file_exists, builder.func);
            let call = builder.ins().call(file_exists, &[path]);
            let result = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, result, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::CallFileRead { dest, path } => {
            let path = builder.use_var(vars[path.0]);
            let file_read = module.declare_func_in_func(runtime.file_read, builder.func);
            let call = builder.ins().call(file_read, &[path]);
            let frame = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::CallFileWrite { path, contents } => {
            let path = builder.use_var(vars[path.0]);
            let contents = builder.use_var(vars[contents.0]);
            let file_write = module.declare_func_in_func(runtime.file_write, builder.func);
            builder.ins().call(file_write, &[path, contents]);
        }
        Op::SimBegin => {
            let f = module.declare_func_in_func(runtime.sim_begin, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::SimEnd => {
            let f = module.declare_func_in_func(runtime.sim_end, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::SimHold { dt } => {
            let f = module.declare_func_in_func(runtime.sim_hold, builder.func);
            let dt = builder.use_var(vars[dt.0]);
            builder.ins().call(f, &[dt]);
        }
        Op::SimActivateDirect { process } => {
            let f = module.declare_func_in_func(runtime.sim_activate_direct, builder.func);
            let process = builder.use_var(vars[process.0]);
            builder.ins().call(f, &[process]);
        }
        Op::SimActivateTimed {
            process,
            t,
            mode,
            prior,
            reac,
        } => {
            let f = module.declare_func_in_func(runtime.sim_activate_timed, builder.func);
            let process = builder.use_var(vars[process.0]);
            let t = builder.use_var(vars[t.0]);
            let mode = builder.ins().iconst(types::I64, *mode);
            let prior = builder.ins().iconst(types::I64, i64::from(*prior));
            let reac = builder.ins().iconst(types::I64, i64::from(*reac));
            builder.ins().call(f, &[process, t, mode, prior, reac]);
        }
        Op::SimActivateRelative {
            process,
            other,
            before,
        } => {
            let f = module.declare_func_in_func(runtime.sim_activate_relative, builder.func);
            let process = builder.use_var(vars[process.0]);
            let other = builder.use_var(vars[other.0]);
            let before = builder.ins().iconst(types::I64, i64::from(*before));
            builder.ins().call(f, &[process, other, before]);
        }
        Op::SimPassivate => {
            let f = module.declare_func_in_func(runtime.sim_passivate, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::SimTransferToHead => {
            let f = module.declare_func_in_func(runtime.sim_transfer_to_head, builder.func);
            builder.ins().call(f, &[]);
            // Another process may have run while this one was parked and written
            // through a by-reference capture pointing into this frame.
            let pointer_type = module.isa().pointer_type();
            reload_addr_taken_locals(builder, function, vars, homes, pointer_type, track_debug);
        }
        Op::SimTerminateCurrent { process } => {
            let f = module.declare_func_in_func(runtime.sim_terminate_current, builder.func);
            let process = builder.use_var(vars[process.0]);
            builder.ins().call(f, &[process]);
        }
        Op::SimCancel { process } => {
            let f = module.declare_func_in_func(runtime.sim_cancel, builder.func);
            let process = builder.use_var(vars[process.0]);
            builder.ins().call(f, &[process]);
        }
        Op::SimFinishMain => {
            let f = module.declare_func_in_func(runtime.sim_finish_main, builder.func);
            builder.ins().call(f, &[]);
        }
        Op::SimTime { dest } => {
            let f = module.declare_func_in_func(runtime.sim_time, builder.func);
            let call = builder.ins().call(f, &[]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimIsMainCurrent { dest } => {
            let f = module.declare_func_in_func(runtime.sim_is_main_current, builder.func);
            let call = builder.ins().call(f, &[]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimHasCurrent { dest } => {
            let f = module.declare_func_in_func(runtime.sim_has_current, builder.func);
            let call = builder.ins().call(f, &[]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimCurrent { dest } => {
            let f = module.declare_func_in_func(runtime.sim_current, builder.func);
            let call = builder.ins().call(f, &[]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimMain { dest } => {
            let f = module.declare_func_in_func(runtime.sim_main, builder.func);
            let call = builder.ins().call(f, &[]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimIdle { dest, process } => {
            let f = module.declare_func_in_func(runtime.sim_idle, builder.func);
            let process = builder.use_var(vars[process.0]);
            let call = builder.ins().call(f, &[process]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimTerminated { dest, process } => {
            let f = module.declare_func_in_func(runtime.sim_terminated, builder.func);
            let process = builder.use_var(vars[process.0]);
            let call = builder.ins().call(f, &[process]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimEvtime { dest, process } => {
            let f = module.declare_func_in_func(runtime.sim_evtime, builder.func);
            let process = builder.use_var(vars[process.0]);
            let call = builder.ins().call(f, &[process]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimNextev { dest, process } => {
            let f = module.declare_func_in_func(runtime.sim_nextev, builder.func);
            let process = builder.use_var(vars[process.0]);
            let call = builder.ins().call(f, &[process]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimsetSetHeadClassId { class_id } => {
            let f = module.declare_func_in_func(runtime.simset_set_head_class_id, builder.func);
            let class_id = builder.ins().iconst(types::I64, *class_id);
            builder.ins().call(f, &[class_id]);
        }
        Op::SimsetInitHead { head } => {
            let f = module.declare_func_in_func(runtime.simset_init_head, builder.func);
            let head = builder.use_var(vars[head.0]);
            builder.ins().call(f, &[head]);
        }
        Op::SimsetOut { object } => {
            let f = module.declare_func_in_func(runtime.simset_out, builder.func);
            let object = builder.use_var(vars[object.0]);
            builder.ins().call(f, &[object]);
        }
        Op::SimsetPrecede { object, ptr } => {
            let f = module.declare_func_in_func(runtime.simset_precede, builder.func);
            let object = builder.use_var(vars[object.0]);
            let ptr = builder.use_var(vars[ptr.0]);
            builder.ins().call(f, &[object, ptr]);
        }
        Op::SimsetFollow { object, ptr } => {
            let f = module.declare_func_in_func(runtime.simset_follow, builder.func);
            let object = builder.use_var(vars[object.0]);
            let ptr = builder.use_var(vars[ptr.0]);
            builder.ins().call(f, &[object, ptr]);
        }
        Op::SimsetInto { object, head } => {
            let f = module.declare_func_in_func(runtime.simset_into, builder.func);
            let object = builder.use_var(vars[object.0]);
            let head = builder.use_var(vars[head.0]);
            builder.ins().call(f, &[object, head]);
        }
        Op::SimsetSuc { dest, object } => {
            let f = module.declare_func_in_func(runtime.simset_suc, builder.func);
            let object = builder.use_var(vars[object.0]);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimsetPred { dest, object } => {
            let f = module.declare_func_in_func(runtime.simset_pred, builder.func);
            let object = builder.use_var(vars[object.0]);
            let call = builder.ins().call(f, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimsetEmpty { dest, head } => {
            let f = module.declare_func_in_func(runtime.simset_empty, builder.func);
            let head = builder.use_var(vars[head.0]);
            let call = builder.ins().call(f, &[head]);
            let raw = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, raw, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SimsetCardinal { dest, head } => {
            let f = module.declare_func_in_func(runtime.simset_cardinal, builder.func);
            let head = builder.use_var(vars[head.0]);
            let call = builder.ins().call(f, &[head]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SeqSystemEnter { dest, block } => {
            let f = module.declare_func_in_func(runtime.seq_system_enter, builder.func);
            let block = builder.ins().iconst(types::I64, *block);
            let call = builder.ins().call(f, &[block]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SeqObjectCreate {
            dest,
            declaring_block,
            entry,
            object,
        } => {
            let f = module.declare_func_in_func(runtime.seq_object_create, builder.func);
            let declaring_block = builder.ins().iconst(types::I64, *declaring_block);
            let entry = builder.use_var(vars[entry.0]);
            let object = builder.use_var(vars[object.0]);
            let call = builder.ins().call(f, &[declaring_block, entry, object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::SeqSystemExit { system: operand }
        | Op::SeqObjectStart { component: operand }
        | Op::SeqBlockInstance { object: operand }
        | Op::SeqDetach { object: operand }
        | Op::SeqCall { object: operand }
        | Op::SeqResume { object: operand }
        | Op::SeqTerminate { object: operand } => {
            let func_id = match op {
                Op::SeqSystemExit { .. } => runtime.seq_system_exit,
                Op::SeqObjectStart { .. } => runtime.seq_object_start,
                Op::SeqBlockInstance { .. } => runtime.seq_block_instance,
                Op::SeqDetach { .. } => runtime.seq_detach,
                Op::SeqCall { .. } => runtime.seq_call,
                Op::SeqResume { .. } => runtime.seq_resume,
                _ => runtime.seq_terminate,
            };
            let f = module.declare_func_in_func(func_id, builder.func);
            let operand = builder.use_var(vars[operand.0]);
            builder.ins().call(f, &[operand]);
            // While this component is parked, another one may write an enclosing
            // variable through a by-reference capture pointer.
            let pointer_type = module.isa().pointer_type();
            reload_addr_taken_locals(builder, function, vars, homes, pointer_type, track_debug);
        }
        Op::Call { dest, name, args } => {
            let pointer_type = module.isa().pointer_type();
            let &func_id = proc_ids.get(name).ok_or_else(|| {
                CompileError::codegen(format!(
                    "native codegen: call to unknown procedure '{name}'"
                ))
            })?;
            let func_ref = module.declare_func_in_func(func_id, builder.func);
            let arg_values: Vec<_> = args
                .iter()
                .map(|arg| builder.use_var(vars[arg.0]))
                .collect();
            let call = builder.ins().call(func_ref, &arg_values);
            if let Some(dest) = dest {
                let results = builder.inst_results(call);
                if results.is_empty() {
                    return Err(CompileError::codegen(format!(
                        "native codegen: call to '{name}' has no return value"
                    )));
                }
                let result = results[0];
                def_local(builder, function, vars, homes, *dest, result, track_debug);
            }
            // Callees may have written through `LocalAddr` pointers into stack
            // homes; reload those locals into SSA vars.
            reload_addr_taken_locals(builder, function, vars, homes, pointer_type, track_debug);
        }
        Op::Abort { message } => {
            let _ = message;
            builder
                .ins()
                .trap(cranelift_codegen::ir::TrapCode::STACK_OVERFLOW);
        }
        Op::Return { value } => {
            if track_debug {
                keep_alive_named_locals(builder, function, vars, homes);
            }
            emit_gc_root_pop(module, builder, runtime, gc_frame);
            if is_main {
                // `main`'s MIR return never carries a value (see module
                // docs); the emitted native entry point always reports
                // success.
                let zero = builder.ins().iconst(types::I32, 0);
                builder.ins().return_(&[zero]);
            } else {
                match value {
                    Some(id) => {
                        let value = builder.use_var(vars[id.0]);
                        builder.ins().return_(&[value]);
                    }
                    None => {
                        builder.ins().return_(&[]);
                    }
                }
            }
        }
        Op::AllocArray { dest, bounds } => {
            let pointer_type = module.isa().pointer_type();
            let ndims = bounds.len() as i64;
            let flat_bounds: Vec<LocalId> = bounds
                .iter()
                .flat_map(|(low, high)| [*low, *high])
                .collect();
            let bounds_ptr = emit_i64_stack_array(builder, vars, &flat_bounds, pointer_type);
            let ndims_val = builder.ins().iconst(types::I64, ndims);
            let alloc_id = match function.local(*dest).ty {
                MirType::ArrayText => runtime.array_alloc_text,
                MirType::ArrayF64 => runtime.array_alloc_f64,
                _ => runtime.array_alloc_i64,
            };
            let array_alloc = module.declare_func_in_func(alloc_id, builder.func);
            let call = builder.ins().call(array_alloc, &[ndims_val, bounds_ptr]);
            let pointer = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, pointer, track_debug);
        }
        Op::ArrayLoad {
            dest,
            array,
            indices,
        } => {
            let pointer_type = module.isa().pointer_type();
            let array_ty = function.local(*array).ty;
            let array = builder.use_var(vars[array.0]);
            let ndims = indices.len() as i64;
            let indices_ptr = emit_i64_stack_array(builder, vars, indices, pointer_type);
            let ndims_val = builder.ins().iconst(types::I64, ndims);
            let load_id = match array_ty {
                MirType::ArrayText => runtime.array_load_text,
                MirType::ArrayF64 => runtime.array_load_f64,
                _ => runtime.array_load_i64,
            };
            let array_load = module.declare_func_in_func(load_id, builder.func);
            let call = builder
                .ins()
                .call(array_load, &[array, ndims_val, indices_ptr]);
            let value = builder.inst_results(call)[0];
            // Boolean arrays use the i64 cell ABI; Bool SSA locals are i8.
            let value = match function.local(*dest).ty {
                MirType::Bool => {
                    let zero = builder.ins().iconst(types::I64, 0);
                    builder.ins().icmp(IntCC::NotEqual, value, zero)
                }
                _ => value,
            };
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::ArrayStore {
            array,
            indices,
            value,
        } => {
            let pointer_type = module.isa().pointer_type();
            let array_ty = function.local(*array).ty;
            let array = builder.use_var(vars[array.0]);
            let ndims = indices.len() as i64;
            let indices_ptr = emit_i64_stack_array(builder, vars, indices, pointer_type);
            let ndims_val = builder.ins().iconst(types::I64, ndims);
            let value_local = *value;
            let value = builder.use_var(vars[value_local.0]);
            let value = match function.local(value_local).ty {
                MirType::Bool => builder.ins().uextend(types::I64, value),
                _ => value,
            };
            let store_id = match array_ty {
                MirType::ArrayText => runtime.array_store_text,
                MirType::ArrayF64 => runtime.array_store_f64,
                _ => runtime.array_store_i64,
            };
            let array_store = module.declare_func_in_func(store_id, builder.func);
            builder
                .ins()
                .call(array_store, &[array, ndims_val, indices_ptr, value]);
        }
        Op::TextNotext { dest } => {
            let text_notext = module.declare_func_in_func(runtime.text_notext, builder.func);
            let call = builder.ins().call(text_notext, &[]);
            let frame = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::TextFromLiteral { dest, string_id } => {
            let frame = emit_text_from_literal(
                module,
                builder,
                runtime.text_from_literal,
                strings,
                string_data,
                *string_id,
            )?;
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::TextCopy { dest, src } => {
            let src = builder.use_var(vars[src.0]);
            let text_copy = module.declare_func_in_func(runtime.text_copy, builder.func);
            let call = builder.ins().call(text_copy, &[src]);
            let frame = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::TextBlanks { dest, n } => {
            let n = builder.use_var(vars[n.0]);
            let text_blanks = module.declare_func_in_func(runtime.text_blanks, builder.func);
            let call = builder.ins().call(text_blanks, &[n]);
            let frame = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::TextConcat { dest, left, right } => {
            let left = builder.use_var(vars[left.0]);
            let right = builder.use_var(vars[right.0]);
            let text_concat = module.declare_func_in_func(runtime.text_concat, builder.func);
            let call = builder.ins().call(text_concat, &[left, right]);
            let frame = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, frame, track_debug);
        }
        Op::TextAssign { dest, src } => {
            let dest_frame = builder.use_var(vars[dest.0]);
            let src_frame = builder.use_var(vars[src.0]);
            let text_assign = module.declare_func_in_func(runtime.text_assign_value, builder.func);
            builder.ins().call(text_assign, &[dest_frame, src_frame]);
        }
        Op::TextRefAssign { dest, src } => {
            let dest_frame = builder.use_var(vars[dest.0]);
            let src_frame = builder.use_var(vars[src.0]);
            let text_assign_ref =
                module.declare_func_in_func(runtime.text_assign_ref, builder.func);
            builder
                .ins()
                .call(text_assign_ref, &[dest_frame, src_frame]);
        }
        Op::TextContentEq { dest, left, right } => {
            let left = builder.use_var(vars[left.0]);
            let right = builder.use_var(vars[right.0]);
            let text_content_eq =
                module.declare_func_in_func(runtime.text_content_eq, builder.func);
            let call = builder.ins().call(text_content_eq, &[left, right]);
            let result = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, result, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextContentCmp { dest, left, right } => {
            let left = builder.use_var(vars[left.0]);
            let right = builder.use_var(vars[right.0]);
            let text_content_cmp =
                module.declare_func_in_func(runtime.text_content_cmp, builder.func);
            let call = builder.ins().call(text_content_cmp, &[left, right]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextRefEq { dest, left, right } => {
            let left = builder.use_var(vars[left.0]);
            let right = builder.use_var(vars[right.0]);
            let text_ref_eq = module.declare_func_in_func(runtime.text_ref_eq, builder.func);
            let call = builder.ins().call(text_ref_eq, &[left, right]);
            let result = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I32, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, result, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextLength { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_length = module.declare_func_in_func(runtime.text_length, builder.func);
            let call = builder.ins().call(text_length, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextConstant { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_constant = module.declare_func_in_func(runtime.text_constant, builder.func);
            let call = builder.ins().call(text_constant, &[frame]);
            let result = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, result, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextStart { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_start = module.declare_func_in_func(runtime.text_start, builder.func);
            let call = builder.ins().call(text_start, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextMain { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_main = module.declare_func_in_func(runtime.text_main, builder.func);
            let call = builder.ins().call(text_main, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextPos { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_pos = module.declare_func_in_func(runtime.text_pos, builder.func);
            let call = builder.ins().call(text_pos, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextMore { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_more = module.declare_func_in_func(runtime.text_more, builder.func);
            let call = builder.ins().call(text_more, &[frame]);
            let result = builder.inst_results(call)[0];
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().icmp(IntCC::NotEqual, result, zero);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextSetpos { frame, index } => {
            let frame = builder.use_var(vars[frame.0]);
            let index = builder.use_var(vars[index.0]);
            let text_setpos = module.declare_func_in_func(runtime.text_setpos, builder.func);
            builder.ins().call(text_setpos, &[frame, index]);
        }
        Op::TextGetchar { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_getchar = module.declare_func_in_func(runtime.text_getchar, builder.func);
            let call = builder.ins().call(text_getchar, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextPutchar { frame, ch } => {
            let frame = builder.use_var(vars[frame.0]);
            let ch = builder.use_var(vars[ch.0]);
            let text_putchar = module.declare_func_in_func(runtime.text_putchar, builder.func);
            builder.ins().call(text_putchar, &[frame, ch]);
        }
        Op::TextGetint { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_getint = module.declare_func_in_func(runtime.text_getint, builder.func);
            let call = builder.ins().call(text_getint, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextPutint { frame, value } => {
            let frame = builder.use_var(vars[frame.0]);
            let value = builder.use_var(vars[value.0]);
            let text_putint = module.declare_func_in_func(runtime.text_putint, builder.func);
            builder.ins().call(text_putint, &[frame, value]);
        }
        Op::TextGetfrac { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_getfrac = module.declare_func_in_func(runtime.text_getfrac, builder.func);
            let call = builder.ins().call(text_getfrac, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextPutfrac {
            frame,
            value,
            places,
        } => {
            let frame = builder.use_var(vars[frame.0]);
            let value = builder.use_var(vars[value.0]);
            let places = builder.use_var(vars[places.0]);
            let text_putfrac = module.declare_func_in_func(runtime.text_putfrac, builder.func);
            builder.ins().call(text_putfrac, &[frame, value, places]);
        }
        Op::TextGetreal { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_getreal = module.declare_func_in_func(runtime.text_getreal, builder.func);
            let call = builder.ins().call(text_getreal, &[frame]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::TextPutfix {
            frame,
            value,
            places,
        } => {
            let frame = builder.use_var(vars[frame.0]);
            let value = builder.use_var(vars[value.0]);
            let places = builder.use_var(vars[places.0]);
            let text_putfix = module.declare_func_in_func(runtime.text_putfix, builder.func);
            builder.ins().call(text_putfix, &[frame, value, places]);
        }
        Op::TextPutreal {
            frame,
            value,
            places,
            exp_digits,
        } => {
            let frame = builder.use_var(vars[frame.0]);
            let value = builder.use_var(vars[value.0]);
            let places = builder.use_var(vars[places.0]);
            let exp = builder.ins().iconst(types::I64, *exp_digits);
            let text_putreal = module.declare_func_in_func(runtime.text_putreal, builder.func);
            builder
                .ins()
                .call(text_putreal, &[frame, value, places, exp]);
        }
        Op::TextSub { dest, frame, i, n } => {
            let frame = builder.use_var(vars[frame.0]);
            let i = builder.use_var(vars[i.0]);
            let n = builder.use_var(vars[n.0]);
            let text_sub = module.declare_func_in_func(runtime.text_sub, builder.func);
            let call = builder.ins().call(text_sub, &[frame, i, n]);
            let result = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, result, track_debug);
        }
        Op::TextStrip { dest, frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_strip = module.declare_func_in_func(runtime.text_strip, builder.func);
            let call = builder.ins().call(text_strip, &[frame]);
            let result = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, result, track_debug);
        }
        Op::TextUpcase { frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_upcase = module.declare_func_in_func(runtime.text_upcase, builder.func);
            builder.ins().call(text_upcase, &[frame]);
        }
        Op::TextLowcase { frame } => {
            let frame = builder.use_var(vars[frame.0]);
            let text_lowcase = module.declare_func_in_func(runtime.text_lowcase, builder.func);
            builder.ins().call(text_lowcase, &[frame]);
        }
        Op::ConstNone { dest } => {
            let pointer_type = module.isa().pointer_type();
            let null = builder.ins().iconst(pointer_type, 0);
            def_local(builder, function, vars, homes, *dest, null, track_debug);
        }
        Op::NewObject {
            dest,
            class_id,
            size,
        } => {
            let size = builder.ins().iconst(types::I64, *size);
            let class_id = builder.ins().iconst(types::I64, *class_id);
            let object_alloc = module.declare_func_in_func(runtime.object_alloc, builder.func);
            let call = builder.ins().call(object_alloc, &[size, class_id]);
            let pointer = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, pointer, track_debug);
        }
        Op::FieldLoadI64 {
            dest,
            object,
            offset,
            ..
        } => {
            let object = builder.use_var(vars[object.0]);
            let offset = builder.ins().iconst(types::I64, *offset);
            let object_load = module.declare_func_in_func(runtime.object_load_i64, builder.func);
            let call = builder.ins().call(object_load, &[object, offset]);
            let value = builder.inst_results(call)[0];
            let value = match function.local(*dest).ty {
                MirType::Bool => {
                    let zero = builder.ins().iconst(types::I64, 0);
                    builder.ins().icmp(IntCC::NotEqual, value, zero)
                }
                // Text/object slots are pointer-sized; native targets use
                // `i64` pointers, so the loaded bits are already the right type.
                MirType::Text
                | MirType::ObjectRef
                | MirType::ArrayI64
                | MirType::ArrayF64
                | MirType::ArrayText
                | MirType::RefI64
                | MirType::FuncRef
                | MirType::I64 => value,
                MirType::F64 | MirType::LongF64 => builder.ins().bitcast(
                    types::F64,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    value,
                ),
            };
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::FieldStoreI64 {
            object,
            offset,
            value,
            ..
        } => {
            let object = builder.use_var(vars[object.0]);
            let offset = builder.ins().iconst(types::I64, *offset);
            let value_local = *value;
            let value = builder.use_var(vars[value_local.0]);
            let value = match function.local(value_local).ty {
                MirType::Bool => builder.ins().uextend(types::I64, value),
                MirType::Text
                | MirType::ObjectRef
                | MirType::ArrayI64
                | MirType::ArrayF64
                | MirType::ArrayText
                | MirType::RefI64
                | MirType::FuncRef
                | MirType::I64 => value,
                MirType::F64 | MirType::LongF64 => builder.ins().bitcast(
                    types::I64,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    value,
                ),
            };
            let object_store = module.declare_func_in_func(runtime.object_store_i64, builder.func);
            builder.ins().call(object_store, &[object, offset, value]);
        }
        Op::ObjectIsNone { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let pointer_type = module.isa().pointer_type();
            let null = builder.ins().iconst(pointer_type, 0);
            let value = builder.ins().icmp(IntCC::Equal, object, null);
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::ObjectClassIdSafe { dest, object } => {
            let object = builder.use_var(vars[object.0]);
            let class_id_safe =
                module.declare_func_in_func(runtime.object_class_id_safe, builder.func);
            let call = builder.ins().call(class_id_safe, &[object]);
            let value = builder.inst_results(call)[0];
            def_local(builder, function, vars, homes, *dest, value, track_debug);
        }
        Op::Nop => {}
    }
    Ok(())
}

fn target_block(clif_blocks: &[ClifBlock], id: BlockId) -> ClifBlock {
    clif_blocks[id.0]
}

fn emit_out_text(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    out_text_id: FuncId,
    strings: &[String],
    string_data: &mut HashMap<usize, DataId>,
    string_id: usize,
) -> Result<(), CompileError> {
    let pointer_type = module.isa().pointer_type();
    let text = strings.get(string_id).ok_or_else(|| {
        CompileError::codegen(format!(
            "internal error: unknown string pool id {string_id}"
        ))
    })?;

    let data_id = match string_data.get(&string_id) {
        Some(&id) => id,
        None => {
            let counter = STRING_DATA_COUNTER.fetch_add(1, Ordering::Relaxed);
            let data_id = module
                .declare_data(&format!("mir_str_{counter}"), Linkage::Local, false, false)
                .map_err(map_module_error)?;
            let mut data = DataDescription::new();
            data.define(text.as_bytes().to_vec().into_boxed_slice());
            module
                .define_data(data_id, &data)
                .map_err(map_module_error)?;
            string_data.insert(string_id, data_id);
            data_id
        }
    };

    let gv = module.declare_data_in_func(data_id, builder.func);
    let ptr = builder.ins().global_value(pointer_type, gv);
    let len = builder.ins().iconst(types::I64, text.len() as i64);

    let out_text = module.declare_func_in_func(out_text_id, builder.func);
    builder.ins().call(out_text, &[ptr, len]);
    Ok(())
}

fn emit_out_text_local(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    runtime: &RuntimeFuncs,
    frame: cranelift_codegen::ir::Value,
) -> Result<(), CompileError> {
    let pointer_type = module.isa().pointer_type();
    let ptr_slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        pointer_type.bytes(),
        0,
    ));
    let len_slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        types::I64.bytes(),
        0,
    ));
    let ptr_addr = builder.ins().stack_addr(pointer_type, ptr_slot, 0);
    let len_addr = builder.ins().stack_addr(pointer_type, len_slot, 0);
    let content_ptr_len = module.declare_func_in_func(runtime.text_content_ptr_len, builder.func);
    builder
        .ins()
        .call(content_ptr_len, &[frame, ptr_addr, len_addr]);
    let content_ptr = builder.ins().load(
        pointer_type,
        cranelift_codegen::ir::MemFlagsData::trusted(),
        ptr_addr,
        0,
    );
    let content_len = builder.ins().load(
        types::I64,
        cranelift_codegen::ir::MemFlagsData::trusted(),
        len_addr,
        0,
    );
    let out_text = module.declare_func_in_func(runtime.out_text, builder.func);
    builder.ins().call(out_text, &[content_ptr, content_len]);
    Ok(())
}

fn emit_text_from_literal(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    from_literal_id: FuncId,
    strings: &[String],
    string_data: &mut HashMap<usize, DataId>,
    string_id: usize,
) -> Result<cranelift_codegen::ir::Value, CompileError> {
    let pointer_type = module.isa().pointer_type();
    let text = strings.get(string_id).ok_or_else(|| {
        CompileError::codegen(format!(
            "internal error: unknown string pool id {string_id}"
        ))
    })?;

    let data_id = match string_data.get(&string_id) {
        Some(&id) => id,
        None => {
            let counter = STRING_DATA_COUNTER.fetch_add(1, Ordering::Relaxed);
            let data_id = module
                .declare_data(&format!("mir_str_{counter}"), Linkage::Local, false, false)
                .map_err(map_module_error)?;
            let mut data = DataDescription::new();
            data.define(text.as_bytes().to_vec().into_boxed_slice());
            module
                .define_data(data_id, &data)
                .map_err(map_module_error)?;
            string_data.insert(string_id, data_id);
            data_id
        }
    };

    let gv = module.declare_data_in_func(data_id, builder.func);
    let ptr = builder.ins().global_value(pointer_type, gv);
    let len = builder.ins().iconst(types::I64, text.len() as i64);
    let from_literal = module.declare_func_in_func(from_literal_id, builder.func);
    let call = builder.ins().call(from_literal, &[ptr, len]);
    Ok(builder.inst_results(call)[0])
}

fn map_module_error(error: cranelift_module::ModuleError) -> CompileError {
    CompileError::codegen(format!("Cranelift module error: {error}"))
}

fn map_define_error(error: cranelift_module::ModuleError, function: &ClifFunction) -> CompileError {
    match error {
        cranelift_module::ModuleError::Compilation(codegen_error) => {
            let message = match codegen_error {
                cranelift_codegen::CodegenError::Verifier(verifier_errors) => {
                    print_errors::pretty_verifier_error(function, None, verifier_errors)
                }
                other => other.to_string(),
            };
            CompileError::codegen(format!("Cranelift verifier errors:\n{message}"))
        }
        other => map_module_error(other),
    }
}
