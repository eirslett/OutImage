//! Cranelift native code generation (object files for native targets).
//!
//! The pipeline is `ast::Program` → [`crate::mir::lower_program`] → Cranelift
//! IR (see [`emit`]) → object file. There is no AST→Cranelift path anymore;
//! everything the scalar subset supports goes through MIR first, so codegen
//! bugs and MIR bugs can't hide behind each other.

mod emit;

use std::path::Path;

use cranelift_codegen::isa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::{Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::Triple;

use crate::ast::Program;
use crate::codegen::dwarf::DebugContext;
use crate::error::CompileError;
use crate::mir;
use crate::source::SourceFile;
use crate::target::CompileTarget;

pub fn compile_to_object(
    program: &Program,
    target: CompileTarget,
    object_path: &Path,
    debug_info: bool,
    source: &SourceFile,
    asm_path: Option<&Path>,
    pic: bool,
) -> Result<(Option<crate::codegen::dwarf::NativeDebugInfo>, Vec<String>), CompileError> {
    let mir_module = mir::lower_program_with_source(program, &source.text)?;
    object_from_mir(
        &mir_module,
        target,
        object_path,
        debug_info,
        source,
        asm_path,
        pic,
    )
}

pub fn object_from_mir(
    mir_module: &mir::Module,
    target: CompileTarget,
    object_path: &Path,
    debug_info: bool,
    source: &SourceFile,
    asm_path: Option<&Path>,
    pic: bool,
) -> Result<(Option<crate::codegen::dwarf::NativeDebugInfo>, Vec<String>), CompileError> {
    mir_module.ensure_externals_resolved()?;

    let foreign_libs = foreign_c_libs(mir_module);
    let isa = create_isa(target, !debug_info, pic)?;
    let mut module = ObjectModule::new(
        ObjectBuilder::new(
            isa,
            object_path.to_string_lossy().as_ref(),
            default_libcall_names(),
        )
        .map_err(|error| CompileError::codegen(format!("object builder failed: {error}")))?,
    );

    let debug_source = debug_info.then_some(source);
    let (function_debug, asm) = emit::emit_mir_module(
        &mut module,
        mir_module,
        debug_source,
        asm_path.is_some(),
        pic,
    )?;

    if let Some(path) = asm_path {
        let text = asm.unwrap_or_default();
        std::fs::write(path, text).map_err(|error| {
            CompileError::codegen(format!(
                "failed to write assembly file {}: {error}",
                path.display()
            ))
        })?;
    }

    let debug_endian = match module.isa().endianness() {
        cranelift_codegen::ir::Endianness::Little => gimli::RunTimeEndian::Little,
        cranelift_codegen::ir::Endianness::Big => gimli::RunTimeEndian::Big,
    };
    let mut debug_context =
        debug_info.then(|| DebugContext::new(module.isa(), source, &mir_module.class_layouts));
    if let Some(context) = debug_context.as_mut() {
        for info in &function_debug {
            context.add_function(info);
        }
    }

    let mut product = module.finish();
    if let Some(context) = debug_context.as_mut() {
        context.write_into_product(&mut product, debug_endian)?;
    }

    let bytes = product
        .emit()
        .map_err(|error| CompileError::codegen(format!("failed to emit object file: {error}")))?;

    std::fs::write(object_path, bytes).map_err(|error| {
        CompileError::codegen(format!(
            "failed to write object file {}: {error}",
            object_path.display()
        ))
    })?;

    Ok((
        if debug_info {
            Some(crate::codegen::dwarf::NativeDebugInfo {
                functions: function_debug,
                class_layouts: mir_module.class_layouts.clone(),
            })
        } else {
            None
        },
        foreign_libs,
    ))
}

fn foreign_c_libs(module: &mir::Module) -> Vec<String> {
    let mut libs = Vec::new();
    for function in &module.functions {
        let Some(abi) = &function.foreign else {
            continue;
        };
        let Some(lib) = abi.native_link_lib() else {
            continue;
        };
        if !libs.iter().any(|existing| existing == lib) {
            libs.push(lib.to_string());
        }
    }
    libs
}

pub(crate) fn create_isa(
    target: CompileTarget,
    optimize: bool,
    pic: bool,
) -> Result<cranelift_codegen::isa::OwnedTargetIsa, CompileError> {
    let triple: Triple = target
        .triple()
        .parse()
        .map_err(|error| CompileError::codegen(format!("invalid target triple: {error}")))?;

    let mut flag_builder = settings::builder();
    let opt_level = if optimize { "speed" } else { "none" };
    flag_builder
        .set("opt_level", opt_level)
        .map_err(|error| CompileError::codegen(format!("invalid Cranelift flag: {error}")))?;
    // Keep FP so DWARF `DW_OP_fbreg` locations stay valid without `.eh_frame`
    // (Mach-O cranelift-object cannot emit `__eh_frame` yet).
    if !optimize {
        flag_builder
            .enable("preserve_frame_pointers")
            .map_err(|error| CompileError::codegen(format!("invalid Cranelift flag: {error}")))?;
    }
    if pic
        || matches!(
            target,
            CompileTarget::MacOsX86_64
                | CompileTarget::MacOsAarch64
                | CompileTarget::Native if cfg!(target_os = "macos")
        )
    {
        flag_builder
            .set("is_pic", "true")
            .map_err(|error| CompileError::codegen(format!("invalid Cranelift flag: {error}")))?;
    }

    let isa_builder = isa::lookup(triple)
        .map_err(|error| CompileError::codegen(format!("unsupported target triple: {error}")))?;

    isa_builder
        .finish(settings::Flags::new(flag_builder))
        .map_err(|error| CompileError::codegen(format!("failed to build ISA: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::test_support::parse_program;

    fn compile(source: &str) -> Result<(), CompileError> {
        let program = parse_program(source);
        let path = std::env::temp_dir().join(format!(
            "sim-cranelift-unit-{}.o",
            std::process::id() as u64 * 100_000 + line!() as u64
        ));
        let source_file = SourceFile::anonymous(source);
        let result = compile_to_object(
            &program,
            CompileTarget::Native,
            &path,
            false,
            &source_file,
            None,
            false,
        );
        let _ = std::fs::remove_file(&path);
        result.map(|_| ())
    }

    fn assert_compiles(source: &str) {
        compile(source).unwrap_or_else(|error| panic!("expected {source:?} to compile: {error}"));
    }

    #[test]
    fn compiles_hello_world() {
        assert_compiles(r#"begin OutText("hello world"); OutImage; end;"#);
    }

    #[test]
    fn compiles_arithmetic_and_assignment() {
        assert_compiles("begin integer x; x := 40 + 2; OutText(\"ok\"); OutImage; end;");
    }

    #[test]
    fn compiles_if_else() {
        assert_compiles(
            r#"begin integer x; x := 1; if x = 1 then OutText("yes") else OutText("no"); OutImage; end;"#,
        );
    }

    #[test]
    fn compiles_while_loop() {
        assert_compiles(
            r#"begin integer i; i := 0; while i < 3 do begin OutText("."); i := i + 1; end; OutImage; end;"#,
        );
    }

    #[test]
    fn compiles_booleans() {
        assert_compiles(
            "begin boolean a, b; a := true; b := not a; if a and not b then OutText(\"ok\"); OutImage; end;",
        );
    }

    #[test]
    fn compiles_empty_program() {
        assert_compiles("begin end;");
    }

    #[test]
    fn compiles_nested_if() {
        assert_compiles(
            r#"begin integer x; x := 5; if x > 0 then begin if x > 10 then OutText("big") else OutText("small"); end; OutImage; end;"#,
        );
    }

    #[test]
    fn compiles_unary_minus() {
        assert_compiles("begin integer x; x := -5; x := -x; OutText(\"ok\"); OutImage; end;");
    }

    #[test]
    fn compiles_integer_division() {
        assert_compiles("begin integer x; x := 7 // 2; OutText(\"ok\"); OutImage; end;");
    }

    #[test]
    fn errors_clearly_on_unsupported_mir_construct() {
        // hold outside Simulation remains rejected.
        let error = compile("begin hold(1.0); end;").expect_err("expected hold to be rejected");
        assert_eq!(error.phase, crate::error::Phase::Codegen);
        assert!(
            error.message.contains("hold"),
            "message was: {}",
            error.message
        );
    }

    #[test]
    fn errors_clearly_on_unknown_procedure_call() {
        let error = compile("begin SomeUnknownProcedure; end;")
            .expect_err("expected unknown call to be rejected");
        assert!(
            error.message.contains("SomeUnknownProcedure") || error.message.contains("call"),
            "message was: {}",
            error.message
        );
    }

    #[test]
    fn compiles_a_function_procedure_call() {
        assert_compiles(
            r#"begin
                integer procedure f(x); value x; integer x;
                begin
                    f := x + 1;
                end;
                integer y;
                y := f(41);
                OutText("ok");
                OutImage;
            end;"#,
        );
    }

    #[test]
    fn compiles_a_void_procedure_with_side_effect() {
        assert_compiles(
            r#"begin
                procedure greet;
                begin
                    OutText("hi");
                end;
                greet;
                OutImage;
            end;"#,
        );
    }

    #[test]
    fn compiles_a_recursive_function_procedure() {
        // Recursion isn't required for Phase 2, but the two-pass
        // declare-then-define emission (see `emit.rs`) supports it for
        // free, so make sure it doesn't regress.
        assert_compiles(
            r#"begin
                integer procedure fact(n); value n; integer n;
                begin
                    if n <= 1 then fact := 1 else fact := n * fact(n - 1);
                end;
                integer r;
                r := fact(5);
                OutText("ok");
                OutImage;
            end;"#,
        );
    }

    #[test]
    fn debug_object_contains_dwarf_sections() {
        use object::{Object, ObjectSection};

        let source_text = "begin integer x;\n  x := 1;\n  OutText(\"hi\");\n  OutImage;\nend;\n";
        let program = parse_program(source_text);
        let source = SourceFile::anonymous(source_text);
        let path = std::env::temp_dir().join(format!(
            "sim-cranelift-debug-{}.o",
            std::process::id() as u64 * 100_000 + line!() as u64
        ));
        compile_to_object(
            &program,
            CompileTarget::Native,
            &path,
            true,
            &source,
            None,
            false,
        )
        .expect("debug compile should succeed");
        let bytes = std::fs::read(&path).expect("read object");
        let _ = std::fs::remove_file(&path);

        let file = object::File::parse(&bytes[..]).expect("parse object");
        let has_debug = file.sections().any(|section| {
            section
                .name()
                .map(|name| {
                    name == ".debug_line"
                        || name == "__debug_line"
                        || name == ".debug_info"
                        || name == "__debug_info"
                })
                .unwrap_or(false)
        });
        assert!(has_debug, "object file should contain DWARF debug sections");
    }
}
