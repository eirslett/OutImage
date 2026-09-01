//! Code generation backends for Simula programs.

#[cfg(feature = "native-aot")]
pub mod cranelift;
#[cfg(feature = "native-aot")]
pub mod dwarf;
#[cfg(feature = "native-aot")]
pub mod link;
pub mod sourcemap;
pub mod wasm;
pub mod wasm_dwarf;
pub mod wasm_gc;

use crate::ast::Program;
use crate::error::CompileError;
use crate::mir;
use crate::source::SourceFile;
use crate::target::{CompileTarget, CrateType};

use std::path::{Path, PathBuf};

/// Compile a program to a native executable, object file, or wasm module.
///
/// When `debug_info` is true, `source` is used to emit DWARF line tables into
/// native binaries and wasm PC→span Source Map v3 (plus `sourceMappingURL`).
/// When `emit_obj` is true on a native target, compilation stops after writing
/// the object file. When `asm_path` is `Some`, Cranelift machine-code
/// disassembly is written there (native targets only).
pub fn compile(
    program: &Program,
    target: CompileTarget,
    output_path: &Path,
    debug_info: bool,
    emit_obj: bool,
    asm_path: Option<&Path>,
    source: &SourceFile,
    crate_type: CrateType,
    extra_link: &[String],
    write_wasm_host: bool,
) -> Result<PathBuf, CompileError> {
    let mir = mir::lower_program_with_source(program, &source.text)?;
    compile_from_mir(
        &mir,
        target,
        output_path,
        debug_info,
        emit_obj,
        asm_path,
        source,
        crate_type,
        extra_link,
        write_wasm_host,
    )
}

/// Compile an already-lowered (and possibly `--with`-merged) MIR module.
pub fn compile_from_mir(
    mir_module: &mir::Module,
    target: CompileTarget,
    output_path: &Path,
    debug_info: bool,
    emit_obj: bool,
    asm_path: Option<&Path>,
    source: &SourceFile,
    crate_type: CrateType,
    extra_link: &[String],
    write_wasm_host: bool,
) -> Result<PathBuf, CompileError> {
    if target.is_wasm() {
        if emit_obj {
            return Err(CompileError::codegen(
                "--emit-obj is only supported for native targets",
            ));
        }
        if asm_path.is_some() {
            return Err(CompileError::codegen(
                "--emit-asm is only supported for native targets",
            ));
        }
        if !extra_link.is_empty() {
            return Err(CompileError::codegen(
                "--link is only supported for native targets",
            ));
        }
        wasm::compile_mir(
            mir_module,
            target,
            output_path,
            debug_info,
            source,
            write_wasm_host,
        )
    } else {
        compile_native_from_mir(
            mir_module,
            target,
            output_path,
            debug_info,
            emit_obj,
            asm_path,
            source,
            crate_type,
            extra_link,
        )
    }
}

#[cfg(feature = "native-aot")]
fn compile_native_from_mir(
    mir_module: &mir::Module,
    target: CompileTarget,
    output_path: &Path,
    debug_info: bool,
    emit_obj: bool,
    asm_path: Option<&Path>,
    source: &SourceFile,
    crate_type: CrateType,
    extra_link: &[String],
) -> Result<PathBuf, CompileError> {
    if emit_obj {
        cranelift::object_from_mir(
            mir_module,
            target,
            output_path,
            debug_info,
            source,
            asm_path,
            crate_type == CrateType::Lib,
        )?;
        Ok(output_path.to_path_buf())
    } else {
        let object_path = temporary_object_path(output_path);
        let _remove_object = RemoveFileOnDrop(&object_path);
        let (debug_functions, foreign_libs) = cranelift::object_from_mir(
            mir_module,
            target,
            &object_path,
            debug_info,
            source,
            asm_path,
            crate_type == CrateType::Lib,
        )?;
        let mut extra = link::classify_link_items(extra_link);
        for lib in foreign_libs {
            if !extra.libs.iter().any(|existing| existing == &lib) {
                extra.libs.push(lib);
            }
        }
        let pic = crate_type == CrateType::Lib;
        link::link_native(
            target,
            &object_path,
            output_path,
            debug_info,
            crate_type,
            &extra,
        )?;
        if let Some(debug) = debug_functions {
            let isa = cranelift::create_isa(target, !debug_info, pic)?;
            dwarf::write_dsym_bundle(output_path, &debug, source, target, isa.as_ref())?;
        }
        Ok(output_path.to_path_buf())
    }
}

#[cfg(not(feature = "native-aot"))]
fn compile_native_from_mir(
    _mir_module: &mir::Module,
    _target: CompileTarget,
    _output_path: &Path,
    _debug_info: bool,
    _emit_obj: bool,
    _asm_path: Option<&Path>,
    _source: &SourceFile,
    _crate_type: CrateType,
    _extra_link: &[String],
) -> Result<PathBuf, CompileError> {
    Err(CompileError::codegen(
        "native compilation is not included in this sim build; \
         use --target wasm-node or a native sim binary",
    ))
}

/// Scratch `.o` next to `-o`, not under the process-wide temp directory.
///
/// Parallel `sim compile -o …/unit` jobs (the corpus runner) would otherwise
/// share `{temp_dir}/unit.o`. The corpus already uses a `TemporaryDirectory` per
/// unit; putting the object beside the binary keeps it in that dir. `Drop`
/// deletes it after link (or on error); the tmpdir is the backstop.
#[cfg(feature = "native-aot")]
fn temporary_object_path(output_path: &Path) -> PathBuf {
    let candidate = output_path.with_extension("o");
    if candidate == output_path {
        output_path.with_extension("tmp.o")
    } else {
        candidate
    }
}

#[cfg(feature = "native-aot")]
struct RemoveFileOnDrop<'a>(&'a Path);

#[cfg(feature = "native-aot")]
impl Drop for RemoveFileOnDrop<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

#[cfg(all(test, feature = "native-aot"))]
mod tests {
    use super::temporary_object_path;
    use std::path::PathBuf;

    #[test]
    fn object_file_sits_beside_the_output_binary() {
        let out = PathBuf::from("scratch").join("outimage-unit").join("unit");
        assert_eq!(temporary_object_path(&out), out.with_extension("o"));
    }

    #[test]
    fn object_file_does_not_clobber_dot_o_output() {
        let out = PathBuf::from("scratch").join("unit.o");
        let object = temporary_object_path(&out);
        assert_ne!(object, out);
        assert_eq!(object.extension().and_then(|ext| ext.to_str()), Some("o"));
    }
}
