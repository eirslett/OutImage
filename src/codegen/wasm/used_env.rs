//! Which `env.*` helpers reachable MIR actually calls.

use std::collections::HashSet;

use crate::mir::{BinOp, Function as MirFunction, Module as MirModule, Op};

use super::seq_runtime;

/// `env` names implemented by `simrt` (`simrt_<name>`), matching
/// `RT_ENV_EXPORTS` in `tests/fixtures/wasm_host.mjs`.
pub(in crate::codegen::wasm) const RT_ENV_EXPORTS: &[&str] = &[
    "f64_pow",
    "text_getint",
    "text_putint",
    "text_getfrac",
    "text_putfrac",
    "text_getreal",
    "text_putfix",
    "text_putreal",
    "out_real",
    "out_fix",
    "out_frac",
    "ln",
    "exp",
    "sin",
    "cos",
    "arctan",
    "addepsilon",
    "subepsilon",
    "randint",
    "uniform",
    "negexp",
    "normal",
    "draw",
];

/// Catalog of `env.*` helpers after `fd_write` / `fd_read` (import order).
pub(in crate::codegen::wasm) const ENV_HELPER_NAMES: &[&str] = &[
    "f64_pow",
    "text_getint",
    "text_putint",
    "text_getfrac",
    "text_putfrac",
    "text_getreal",
    "text_putfix",
    "text_putreal",
    "out_real",
    "out_fix",
    "out_frac",
    "ln",
    "exp",
    "sin",
    "cos",
    "arctan",
    "addepsilon",
    "subepsilon",
    "randint",
    "uniform",
    "sysout_write",
    "sysout_flush",
    "basicio_register",
    "basicio_open",
    "basicio_close",
    "basicio_isopen",
    "basicio_out_text",
    "basicio_out_char",
    "basicio_out_image",
    "basicio_break_out_image",
    "basicio_in_image",
    "basicio_in_char",
    "basicio_endfile",
    "basicio_image",
    "basicio_set_image",
    "basicio_pos",
    "basicio_length",
    "basicio_setpos",
    "basicio_line",
    "basicio_filename",
    "basicio_lastitem",
    "basicio_inint",
    "basicio_inreal",
    "basicio_infrac",
    "basicio_intext",
    "basicio_out_real",
    "basicio_out_fix",
    "basicio_out_frac",
    "basicio_out_int",
    "error",
    "basicio_open_byte",
    "basicio_in_byte",
    "basicio_out_byte",
    "basicio_locate",
    "basicio_location",
    "basicio_lastloc",
    "negexp",
    "normal",
    "draw",
    "basicio_setaccess",
    "basicio_eject",
    "basicio_linesperpage",
    "basicio_inrecord",
];

/// `fd_write`/`fd_read` plus one slot per used catalog helper.
pub(in crate::codegen::wasm) fn env_import_count(used: &HashSet<String>) -> u32 {
    2 + ENV_HELPER_NAMES
        .iter()
        .filter(|name| used.contains(**name))
        .count() as u32
}

/// `simrt` exports implemented by the `no_std` math cdylib.
pub(in crate::codegen::wasm) const MATH_RT_EXPORTS: &[&str] = &[
    "simrt_f64_pow",
    "simrt_ln",
    "simrt_exp",
    "simrt_sin",
    "simrt_cos",
    "simrt_arctan",
    "simrt_addepsilon",
    "simrt_subepsilon",
];

/// Keep-set is empty or only math helpers — shake the math blob, not full `std`.
pub(in crate::codegen::wasm) fn keep_fits_math_rt(keep: &HashSet<String>) -> bool {
    keep.iter()
        .all(|name| MATH_RT_EXPORTS.contains(&name.as_str()))
}

/// Linear SysOut image (JS `sysout_write` / `sysout_flush` / OutReal).
pub(in crate::codegen::wasm) fn needs_sysout_image(used: &HashSet<String>) -> bool {
    used.contains("sysout_write")
        || used.contains("sysout_flush")
        || used.contains("out_real")
        || used.contains("out_fix")
        || used.contains("out_frac")
        || used.iter().any(|name| name.starts_with("basicio_"))
}

/// Linear SysIn image (`InImage` / `InChar` / BASICIO input).
pub(in crate::codegen::wasm) fn needs_sysin_image(used: &HashSet<String>) -> bool {
    used.contains("__sysin_image")
        || used.iter().any(|name| {
            name.starts_with("basicio_in")
                || name == "basicio_endfile"
                || name == "basicio_lastitem"
        })
}

/// Extra `simrt` exports the JS BASICIO polyfill calls by name.
pub(in crate::codegen::wasm) const JS_FORMAT_EXPORTS: &[&str] = &[
    "simrt_format_scratch",
    "simrt_format_scratch_cap",
    "simrt_format_out_real",
    "simrt_format_out_fix",
    "simrt_format_out_frac",
];

/// Function names reachable from `_start`/`main` and from public wasm exports.
pub(in crate::codegen::wasm) fn reachable_functions(mir: &MirModule) -> HashSet<String> {
    let by_name: std::collections::HashMap<&str, &MirFunction> = mir
        .functions
        .iter()
        .map(|function| (function.name.as_str(), function))
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue = vec![entry_point(mir).to_string(), "main".to_string()];
    for function in &mir.functions {
        if function.export.is_some() || function.wasm_export_name().is_some() {
            queue.push(function.name.clone());
        }
    }
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(function) = by_name.get(name.as_str()) else {
            continue;
        };
        for block in &function.blocks {
            for spanned in &block.ops {
                match &spanned.op {
                    Op::Call { name, .. } | Op::FuncAddr { name, .. } => queue.push(name.clone()),
                    _ => {}
                }
            }
        }
    }
    seen
}

pub(in crate::codegen::wasm) fn entry_point(mir: &MirModule) -> &'static str {
    if mir
        .functions
        .iter()
        .any(|function| function.name == seq_runtime::START)
    {
        seq_runtime::START
    } else {
        "main"
    }
}

/// `env` helper names (not including `fd_write` / `fd_read`) used by live code.
pub(in crate::codegen::wasm) fn used_env_imports(
    mir: &MirModule,
    reachable: &HashSet<String>,
) -> HashSet<String> {
    let mut used = HashSet::new();
    for function in &mir.functions {
        if !reachable.contains(&function.name) {
            continue;
        }
        for block in &function.blocks {
            for spanned in &block.ops {
                collect_op(&spanned.op, &mut used);
            }
        }
    }
    used
}

fn collect_op(op: &Op, used: &mut HashSet<String>) {
    match op {
        Op::Binary { op: BinOp::Pow, .. } => {
            used.insert("f64_pow".into());
        }
        Op::CallOutText { .. }
        | Op::CallOutTextLocal { .. }
        | Op::CallOutInt { .. }
        | Op::CallOutChar { .. } => {
            used.insert("sysout_write".into());
        }
        Op::CallOutImage | Op::CallBreakOutImage => {
            used.insert("sysout_flush".into());
        }
        Op::CallInImage | Op::CallInChar { .. } | Op::CallEndfile { .. } => {
            used.insert("__sysin_image".into());
        }
        Op::CallOutReal { .. } => {
            used.insert("out_real".into());
        }
        Op::CallOutFix { .. } => {
            used.insert("out_fix".into());
        }
        Op::CallOutFrac { .. } => {
            used.insert("out_frac".into());
        }
        Op::CallEnv { name, .. } => collect_call_env(name, used),
        Op::CallBasicioRegisterFile { .. } => {
            used.insert("basicio_register".into());
        }
        Op::CallBasicioOpen { .. } => {
            used.insert("basicio_open".into());
        }
        Op::CallBasicioClose { .. } => {
            used.insert("basicio_close".into());
        }
        Op::CallBasicioIsOpen { .. } => {
            used.insert("basicio_isopen".into());
        }
        Op::CallBasicioOutText { .. } => {
            used.insert("sysout_write".into());
            used.insert("basicio_out_text".into());
        }
        Op::CallBasicioOutChar { .. } => {
            used.insert("sysout_write".into());
            used.insert("basicio_out_char".into());
        }
        Op::CallBasicioOutImage { .. } => {
            used.insert("sysout_flush".into());
            used.insert("basicio_out_image".into());
        }
        Op::CallBasicioBreakOutImage { .. } => {
            used.insert("sysout_flush".into());
            used.insert("basicio_break_out_image".into());
        }
        Op::CallBasicioInImage { .. } => {
            used.insert("basicio_in_image".into());
        }
        Op::CallBasicioInChar { .. } => {
            used.insert("basicio_in_char".into());
        }
        Op::CallBasicioEndfile { .. } => {
            used.insert("basicio_endfile".into());
        }
        Op::CallBasicioImage { .. } => {
            used.insert("basicio_image".into());
        }
        Op::CallBasicioSetImage { .. } => {
            used.insert("basicio_set_image".into());
        }
        Op::CallBasicioPos { .. } => {
            used.insert("basicio_pos".into());
        }
        Op::CallBasicioLength { .. } => {
            used.insert("basicio_length".into());
        }
        Op::CallBasicioSetpos { .. } => {
            used.insert("basicio_setpos".into());
        }
        Op::CallBasicioLine { .. } => {
            used.insert("basicio_line".into());
        }
        Op::CallBasicioFilename { .. } => {
            used.insert("basicio_filename".into());
        }
        Op::CallBasicioLastItem { .. } => {
            used.insert("basicio_lastitem".into());
        }
        Op::CallBasicioInInt { .. } => {
            used.insert("text_getint".into());
            used.insert("basicio_inint".into());
        }
        Op::CallBasicioInReal { .. } => {
            used.insert("text_getreal".into());
            used.insert("basicio_inreal".into());
        }
        Op::CallBasicioInFrac { .. } => {
            used.insert("text_getfrac".into());
            used.insert("basicio_infrac".into());
        }
        Op::CallBasicioInText { .. } => {
            used.insert("basicio_intext".into());
        }
        Op::CallBasicioOutReal { .. } => {
            used.insert("out_real".into());
            used.insert("basicio_out_real".into());
        }
        Op::CallBasicioOutFix { .. } => {
            used.insert("out_fix".into());
            used.insert("basicio_out_fix".into());
        }
        Op::CallBasicioOutFrac { .. } => {
            used.insert("out_frac".into());
            used.insert("basicio_out_frac".into());
        }
        Op::CallBasicioOutInt { .. } => {
            used.insert("sysout_write".into());
            used.insert("basicio_out_int".into());
        }
        Op::CallBasicioOpenByte { .. } => {
            used.insert("basicio_open_byte".into());
        }
        Op::CallBasicioInByte { .. } => {
            used.insert("basicio_in_byte".into());
        }
        Op::CallBasicioOutByte { .. } => {
            used.insert("basicio_out_byte".into());
        }
        Op::CallBasicioLocate { .. } => {
            used.insert("basicio_locate".into());
        }
        Op::CallBasicioLocation { .. } => {
            used.insert("basicio_location".into());
        }
        Op::CallBasicioLastloc { .. } => {
            used.insert("basicio_lastloc".into());
        }
        Op::CallBasicioSetAccess { .. } => {
            used.insert("basicio_setaccess".into());
        }
        Op::CallBasicioEject { .. } => {
            used.insert("basicio_eject".into());
        }
        Op::CallBasicioLinesPerPage { .. } => {
            used.insert("basicio_linesperpage".into());
        }
        Op::CallBasicioInRecord { .. } => {
            used.insert("basicio_inrecord".into());
        }
        Op::TextGetint { .. } => {
            used.insert("text_getint".into());
        }
        Op::TextPutint { .. } => {
            used.insert("text_putint".into());
        }
        Op::TextGetfrac { .. } => {
            used.insert("text_getfrac".into());
        }
        Op::TextPutfrac { .. } => {
            used.insert("text_putfrac".into());
        }
        Op::TextGetreal { .. } => {
            used.insert("text_getreal".into());
        }
        Op::TextPutfix { .. } => {
            used.insert("text_putfix".into());
        }
        Op::TextPutreal { .. } => {
            used.insert("text_putreal".into());
        }
        _ => {}
    }
}

fn collect_call_env(name: &str, used: &mut HashSet<String>) {
    match name {
        "ln" | "exp" | "sin" | "cos" | "arctan" | "addepsilon" | "subepsilon" | "randint"
        | "uniform" | "normal" | "negexp" | "draw" | "error" => {
            used.insert(name.into());
        }
        _ => {}
    }
}

/// `simrt` export names to keep for this program.
pub(in crate::codegen::wasm) fn rt_keep_exports(used_env: &HashSet<String>) -> HashSet<String> {
    let mut keep = HashSet::new();
    for name in RT_ENV_EXPORTS {
        if used_env.contains(*name) {
            keep.insert(format!("simrt_{name}"));
        }
    }
    if used_env.contains("basicio_out_real")
        || used_env.contains("basicio_out_fix")
        || used_env.contains("basicio_out_frac")
    {
        for name in JS_FORMAT_EXPORTS {
            keep.insert((*name).into());
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_helper_names_are_unique_and_cover_rt_exports() {
        let mut seen = HashSet::new();
        for name in ENV_HELPER_NAMES {
            assert!(seen.insert(*name), "duplicate env helper {name}");
        }
        assert_eq!(ENV_HELPER_NAMES.len(), 63);
        for name in RT_ENV_EXPORTS {
            assert!(
                ENV_HELPER_NAMES.contains(name),
                "{name} missing from ENV_HELPER_NAMES"
            );
        }
    }
}
