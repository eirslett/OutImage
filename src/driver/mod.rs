//! Compiler driver: front-end pipeline plus backend selection.

use std::path::{Path, PathBuf};

#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
use crate::codegen::sourcemap::SourceMap;
#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
use crate::codegen::{self};
use crate::error::CompileError;
use crate::runtime::{IoHost, StdioHost};
use crate::source::{CompositeSource, SourceFile};
use crate::target::{Charset, CompileTarget, CrateType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Interpreter,
    Cranelift,
    /// Lex, parse, semantic analysis, and MIR lowering — no codegen/link.
    Check,
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub backend: Backend,
    pub target: CompileTarget,
    pub output: Option<PathBuf>,
    /// When true, `[` and `]` may be used for array subscripts.
    pub allow_square_bracket_subscripts: bool,
    /// When true, `--` starts a line comment through the end of the line.
    /// Default on; `--no-double-dash-comments` disables it so consecutive
    /// minuses are operators as in the Standard.
    pub allow_double_dash_comments: bool,
    /// Write a MIR dump (see [`crate::mir::Module::dump`]) alongside
    /// compilation. Defaults to `<output>.mir` (or `<source>.mir` if there's
    /// no output path) unless [`Self::mir_output`] overrides the path.
    pub emit_mir: bool,
    /// `-g`: write `<output>.sim-map` (MIR JSON side-map) and
    /// `<output>.map` (Source Map v3). Native builds also emit DWARF.
    /// Wasm builds relocate `.map` columns to module byte offsets and embed
    /// a `sourceMappingURL` custom section.
    pub debug_info: bool,
    /// Explicit path for the `emit_mir` dump; overrides the default.
    pub mir_output: Option<PathBuf>,
    /// Write a native object file (`.o`) and skip linking. Ignored for wasm.
    pub emit_obj: bool,
    /// Write Cranelift machine-code disassembly (`.s`). Native targets only.
    pub emit_asm: bool,
    /// Explicit path for `--emit-asm`; defaults to `<output>.s`.
    pub asm_output: Option<PathBuf>,
    /// `bin` (default) or `lib`.
    pub crate_type: CrateType,
    /// Extra linker inputs from `--link` (object files, archives, dylibs, or
    /// library names such as `m` / `libm` / `-lm`).
    pub extra_link: Vec<String>,
    /// Separately checked Simula modules (`--with utils.sim`) merged after
    /// lowering.
    pub with_modules: Vec<PathBuf>,
    /// FFI text copy encoding (`--charset`). Does not change internal ranks.
    pub charset: Charset,
    /// Write `wasm_host.mjs` next to a wasm artifact so application JS can
    /// `import { instantiateSimulaWasm }` without reaching into the compiler
    /// tree. Ignored on native. `--no-wasm-host` sets this to false.
    pub write_wasm_host: bool,
    /// Emit `W0001` unused-binding warnings after a clean semantic pass.
    /// Default on for [`Self::for_check`]; off for compile/run.
    pub enable_unused_lints: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            backend: Backend::Cranelift,
            target: CompileTarget::Native,
            output: None,
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
            emit_mir: false,
            debug_info: false,
            mir_output: None,
            emit_obj: false,
            emit_asm: false,
            asm_output: None,
            crate_type: CrateType::Bin,
            extra_link: Vec::new(),
            with_modules: Vec::new(),
            charset: Charset::Latin1,
            write_wasm_host: true,
            enable_unused_lints: false,
        }
    }
}

impl CompileOptions {
    pub fn for_run() -> Self {
        Self {
            backend: Backend::Interpreter,
            allow_square_bracket_subscripts: true,
            ..Self::default()
        }
    }

    pub fn for_check() -> Self {
        Self {
            backend: Backend::Check,
            allow_square_bracket_subscripts: true,
            enable_unused_lints: true,
            ..Self::default()
        }
    }

    pub fn for_compile(output: PathBuf, target: CompileTarget) -> Self {
        Self {
            backend: Backend::Cranelift,
            target,
            output: Some(output),
            allow_square_bracket_subscripts: true,
            ..Self::default()
        }
    }

    fn lex_options(&self) -> crate::lex::LexOptions {
        crate::lex::LexOptions {
            allow_square_bracket_subscripts: self.allow_square_bracket_subscripts,
            allow_double_dash_comments: self.allow_double_dash_comments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileResult {
    /// Output from the interpreter backend (`sim run`).
    Interpreted(String),
    /// Path to a linked executable or wasm module (`sim compile`).
    Artifact(PathBuf),
    /// Front-end + MIR lowering succeeded (`sim check`).
    Checked,
}

pub fn compile(
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    let module = lower_merged(source, options)?;

    if options.emit_mir || options.debug_info {
        emit_debug_tooling(&module, source, options)?;
    }

    match options.backend {
        Backend::Interpreter => {
            let output = crate::mir::interp::interpret_module(&module)?;
            Ok(CompileResult::Interpreted(output))
        }
        Backend::Check => Ok(CompileResult::Checked),
        Backend::Cranelift => compile_cranelift(&module, source, options),
    }
}

fn front_end(
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<crate::ast::Program, CompileError> {
    let tokens = crate::lex::tokenize_with_options(source, &options.lex_options())?;
    if options.backend == Backend::Check {
        let (program, mut errors) = crate::parse::parse_lenient(&tokens);
        if let Err(semantic) = crate::semantic::analyze_all(&program) {
            errors.extend(semantic);
        }
        if !errors.is_empty() {
            return Err(crate::error::CompileErrors::new(errors).into_bundled());
        }
        return Ok(program);
    }
    let program = crate::parse::parse(&tokens)?;
    if let Err(errors) = crate::semantic::analyze_all(&program) {
        return Err(errors.into_bundled());
    }
    Ok(program)
}

/// Lower `source` and MIR-merge any `--with` modules. Used by `sim mir`.
pub fn lower_merged(
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<crate::mir::Module, CompileError> {
    let mut module = attach_libraries(lower_unit(source, options)?, options)?;
    module.charset = options.charset;
    Ok(module)
}

fn lower_unit(
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<crate::mir::Module, CompileError> {
    let program = front_end(source, options)?;
    crate::mir::lower_program_with_source(&program, &source.text)
}

/// Check and lower each `--with` file on its own, then MIR-merge by file stem.
fn attach_libraries(
    main: crate::mir::Module,
    options: &CompileOptions,
) -> Result<crate::mir::Module, CompileError> {
    if options.with_modules.is_empty() {
        return Ok(main);
    }
    let mut libs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in &options.with_modules {
        let source = SourceFile::from_path(path).map_err(|error| {
            CompileError::codegen(format!("failed to read {}: {error}", path.display()))
        })?;
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                CompileError::codegen(format!("--with {} has no file stem", path.display()))
            })?;
        let key = stem.to_ascii_lowercase();
        if !seen.insert(key) {
            return Err(CompileError::codegen(format!(
                "duplicate --with module '{stem}'"
            )));
        }
        let lib = lower_unit(&source, options)?;
        libs.push((stem.to_string(), lib));
    }
    crate::mir::merge_modules(main, libs)
}

/// Compiles one or more source files by concatenating them into a
/// [`CompositeSource`], then remapping any error spans back to origin files.
///
/// On failure, the returned [`CompileError`] has local spans and
/// [`CompileError::primary_source`] set so callers can render with:
/// `error.write_cached(&composite.to_cache(), composite.as_source_file(), …)`.
pub fn compile_sources(
    files: &[SourceFile],
    options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    let composite = CompositeSource::concat(files.iter().cloned());
    compile(composite.as_source_file(), options).map_err(|error| error.remap_to_origins(&composite))
}

/// Unused locals / parameters / labels after a clean semantic pass (`W0001`).
///
/// Returns an empty list when [`CompileOptions::enable_unused_lints`] is off,
/// when the LSP feature is disabled, or when the file does not analyze cleanly.
pub fn unused_diagnostics(source: &SourceFile, options: &CompileOptions) -> Vec<CompileError> {
    if !options.enable_unused_lints {
        return Vec::new();
    }
    unused_diagnostics_inner(source, options)
}

#[cfg(feature = "lsp")]
fn unused_diagnostics_inner(source: &SourceFile, options: &CompileOptions) -> Vec<CompileError> {
    let Ok(tokens) = crate::lex::tokenize_with_options(source, &options.lex_options()) else {
        return Vec::new();
    };
    let Ok(program) = crate::parse::parse(&tokens) else {
        return Vec::new();
    };
    if crate::semantic::analyze_all(&program).is_err() {
        return Vec::new();
    }
    let index = crate::lsp::SymbolIndex::build(&program, Some(&tokens));
    crate::lsp::unused_compile_errors(&index)
}

#[cfg(not(feature = "lsp"))]
fn unused_diagnostics_inner(_source: &SourceFile, _options: &CompileOptions) -> Vec<CompileError> {
    Vec::new()
}

#[cfg(test)]
mod compile_sources_tests {
    use super::*;

    #[test]
    fn compile_sources_remaps_error_to_second_file() {
        let files = vec![
            SourceFile {
                name: "ok.sim".into(),
                text: "begin ".into(),
            },
            SourceFile {
                name: "bad.sim".into(),
                text: "@@@".into(),
            },
        ];
        let err = compile_sources(&files, &CompileOptions::for_run()).expect_err("lex should fail");
        assert_eq!(err.primary_source.as_deref(), Some("bad.sim"));
        assert!(err.span.is_some());
    }
}

#[cfg(feature = "native-aot")]
fn asm_emit_path(options: &CompileOptions, output_path: &Path) -> Option<PathBuf> {
    if !options.emit_asm {
        return None;
    }
    Some(
        options
            .asm_output
            .clone()
            .unwrap_or_else(|| output_path.with_extension("s")),
    )
}

#[cfg(not(feature = "native-aot"))]
fn asm_emit_path(_options: &CompileOptions, _output_path: &Path) -> Option<PathBuf> {
    None
}

/// Writes `--emit-mir` / `-g` from an already-lowered (and possibly merged)
/// module so `--with` libraries show up in the dump.
fn emit_debug_tooling(
    module: &crate::mir::Module,
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<(), CompileError> {
    if options.emit_mir {
        let path = mir_dump_path(options, source);
        write_text_file(&path, &module.dump())?;
    }

    if options.debug_info {
        emit_source_maps(module, source, options)?;
    }

    Ok(())
}

#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
fn emit_source_maps(
    module: &crate::mir::Module,
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<(), CompileError> {
    let map = SourceMap::build(module, source);
    let json = map.to_json().map_err(|error| {
        CompileError::codegen(format!("failed to serialize source map: {error}"))
    })?;
    let path = source_map_path(options, source);
    write_text_file(&path, &json)?;

    // Wasm `-g` overwrites `.map` / `.sim-map` with PC-relocated
    // versions after codegen; skip the interim MIR-line map here.
    if !options.target.is_wasm() {
        let generated = options
            .output
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or(&source.name);
        let v3 = map.to_source_map_v3(generated, Some(&source.text));
        let v3_json = v3.to_json().map_err(|error| {
            CompileError::codegen(format!("failed to serialize Source Map v3: {error}"))
        })?;
        let v3_path = source_map_v3_path(options, source);
        write_text_file(&v3_path, &v3_json)?;
    }
    Ok(())
}

#[cfg(not(any(feature = "native-aot", feature = "wasm-aot")))]
fn emit_source_maps(
    _module: &crate::mir::Module,
    _source: &SourceFile,
    _options: &CompileOptions,
) -> Result<(), CompileError> {
    Err(CompileError::codegen(
        "-g source maps require an AOT build of sim",
    ))
}

#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
fn compile_cranelift(
    module: &crate::mir::Module,
    source: &SourceFile,
    options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    let output_path = options
        .output
        .clone()
        .ok_or_else(|| CompileError::codegen("output path is required for compilation"))?;
    let artifact = codegen::compile_from_mir(
        module,
        options.target,
        &output_path,
        options.debug_info,
        options.emit_obj,
        asm_emit_path(options, &output_path).as_deref(),
        source,
        options.crate_type,
        &options.extra_link,
        options.write_wasm_host,
    )?;
    Ok(CompileResult::Artifact(artifact))
}

#[cfg(not(any(feature = "native-aot", feature = "wasm-aot")))]
fn compile_cranelift(
    _module: &crate::mir::Module,
    _source: &SourceFile,
    _options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    Err(CompileError::codegen(
        "AOT compilation is not included in this sim build",
    ))
}

/// Lex / parse / analyze / lower / interpret with a caller-supplied stdio host.
pub fn run_with_host(
    source: &SourceFile,
    options: &CompileOptions,
    host: Box<dyn IoHost>,
) -> Result<(), CompileError> {
    let module = lower_merged(source, options)?;
    crate::mir::interp::interpret_module_with_host(&module, host)
}

/// Interpret `source` against the process stdio streams.
pub fn run_stdio(source: &SourceFile, options: &CompileOptions) -> Result<(), CompileError> {
    run_with_host(source, options, Box::new(StdioHost))
}

fn mir_dump_path(options: &CompileOptions, source: &SourceFile) -> PathBuf {
    if let Some(path) = &options.mir_output {
        return path.clone();
    }
    default_debug_artifact_path(options.output.as_deref(), source, "mir")
}

#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
fn source_map_path(options: &CompileOptions, source: &SourceFile) -> PathBuf {
    default_debug_artifact_path(options.output.as_deref(), source, "sim-map")
}

#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
fn source_map_v3_path(options: &CompileOptions, source: &SourceFile) -> PathBuf {
    default_debug_artifact_path(options.output.as_deref(), source, "map")
}

/// `<output>.<suffix>`, falling back to `<source>.<suffix>` when there's no
/// output path (e.g. the interpreter backend).
fn default_debug_artifact_path(
    output: Option<&Path>,
    source: &SourceFile,
    suffix: &str,
) -> PathBuf {
    match output {
        Some(path) => PathBuf::from(format!("{}.{suffix}", path.display())),
        None => PathBuf::from(format!("{}.{suffix}", source.name)),
    }
}

fn write_text_file(path: &Path, contents: &str) -> Result<(), CompileError> {
    std::fs::write(path, contents).map_err(|error| {
        CompileError::codegen(format!("failed to write {}: {error}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("sim-driver-test-{tag}-{}-{id}", std::process::id()))
    }

    struct TempFile(PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn emit_mir_writes_a_dump_next_to_the_output_path() {
        let output = temp_path("bin");
        let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::Native);
        options.emit_mir = true;
        let source = SourceFile::anonymous("begin integer x; x := 1; end;");

        let result = compile(&source, &options).expect("expected compilation to succeed");
        let CompileResult::Artifact(artifact) = &result else {
            panic!("expected an artifact result");
        };
        let _bin = TempFile(artifact.clone());

        let mir_path = PathBuf::from(format!("{}.mir", output.display()));
        let _mir_guard = TempFile(mir_path.clone());
        let dump = std::fs::read_to_string(&mir_path)
            .unwrap_or_else(|error| panic!("expected {} to exist: {error}", mir_path.display()));
        assert!(dump.contains("fn main("), "dump was:\n{dump}");
        assert!(dump.contains("store"), "dump was:\n{dump}");
    }

    #[test]
    fn emit_mir_honors_explicit_mir_output_path() {
        let output = temp_path("bin2");
        let explicit_mir_path = temp_path("explicit.mir");
        let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::Native);
        options.emit_mir = true;
        options.mir_output = Some(explicit_mir_path.clone());
        let source = SourceFile::anonymous("begin end;");

        let result = compile(&source, &options).expect("expected compilation to succeed");
        let CompileResult::Artifact(artifact) = &result else {
            panic!("expected an artifact result");
        };
        let _bin = TempFile(artifact.clone());
        let _mir_guard = TempFile(explicit_mir_path.clone());

        assert!(
            explicit_mir_path.exists(),
            "explicit MIR path should have been written"
        );
        let default_path = PathBuf::from(format!("{}.mir", output.display()));
        assert!(
            !default_path.exists(),
            "default MIR path should not have been written"
        );
    }

    #[test]
    fn debug_info_writes_a_parseable_json_source_map() {
        let output = temp_path("bin3");
        let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::Native);
        options.debug_info = true;
        let source = SourceFile::anonymous("begin integer x;\nx := 1;\nend;\n");

        let result = compile(&source, &options).expect("expected compilation to succeed");
        let CompileResult::Artifact(artifact) = &result else {
            panic!("expected an artifact result");
        };
        let _bin = TempFile(artifact.clone());

        let map_path = PathBuf::from(format!("{}.sim-map", output.display()));
        let _map_guard = TempFile(map_path.clone());
        let json = std::fs::read_to_string(&map_path)
            .unwrap_or_else(|error| panic!("expected {} to exist: {error}", map_path.display()));

        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["version"], 1);
        assert!(value["mappings"].as_array().is_some_and(|m| !m.is_empty()));

        let v3_path = PathBuf::from(format!("{}.map", output.display()));
        let _v3_guard = TempFile(v3_path.clone());
        let v3_json = std::fs::read_to_string(&v3_path)
            .unwrap_or_else(|error| panic!("expected {} to exist: {error}", v3_path.display()));
        let v3: serde_json::Value =
            serde_json::from_str(&v3_json).expect("valid Source Map v3 JSON");
        assert_eq!(v3["version"], 3);
        assert!(v3["mappings"].is_string());
        assert!(v3["sources"].as_array().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn neither_artifact_is_written_when_options_disable_them() {
        let output = temp_path("bin4");
        let options = CompileOptions::for_compile(output.clone(), CompileTarget::Native);
        let source = SourceFile::anonymous("begin end;");

        let result = compile(&source, &options).expect("expected compilation to succeed");
        let CompileResult::Artifact(artifact) = &result else {
            panic!("expected an artifact result");
        };
        let _bin = TempFile(artifact.clone());

        assert!(!PathBuf::from(format!("{}.mir", output.display())).exists());
        assert!(!PathBuf::from(format!("{}.sim-map", output.display())).exists());
        assert!(!PathBuf::from(format!("{}.map", output.display())).exists());
    }

    #[test]
    fn semantic_multi_error_bundles_related_into_compile_error() {
        let source = SourceFile::anonymous("begin integer i; boolean b; i := b; b := 1; end;");
        let options = CompileOptions::for_run();
        let error = compile(&source, &options).expect_err("expected type errors");
        assert!(error.message.contains("assignment"), "primary: {error}");
        assert_eq!(error.related.len(), 1, "expected one related sibling");
        assert!(
            error.notes.iter().any(|n| n.contains("1 more error")),
            "notes: {:?}",
            error.notes
        );
        let rendered = error.render(&source);
        assert!(
            rendered.matches("assignment needs").count() >= 2,
            "rendered: {rendered}"
        );
    }
}
