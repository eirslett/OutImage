//! Simula compiler library.

pub mod ast;
pub mod ast_dump;
pub mod basicio;
#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
pub mod bundled;
#[cfg(any(feature = "native-aot", feature = "wasm-aot"))]
pub mod codegen;
pub mod concatenate;
#[cfg(feature = "dap")]
pub mod debug;
pub mod diagnostics;
pub mod driver;
pub mod environment;
pub mod error;
pub mod grammar_test;
pub mod layout;
pub mod lex;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod mir;
pub mod parse;
pub mod runtime;
pub mod semantic;
pub mod simulation;
pub mod source;
pub mod stdlib;
pub mod target;
pub mod text;
pub mod types;

pub use diagnostics::DiagId;
pub use driver::{
    Backend, CompileOptions, CompileResult, compile as compile_with_options, compile_sources,
    lower_merged, run_stdio, unused_diagnostics,
};
pub use error::{
    ColorChoice, CompileError, CompileErrors, DiagnosticConfig, DiagnosticLabel, ExplainLevel,
    Phase, SourceCache, SourceId, Span,
};
pub use lex::LexOptions;
pub use mir::interp::{GcOptions, GcStats, HostCtx, InterpretPoll, Interpreter, Value};
pub use runtime::{CapturingHost, IoHost, ReadLine, StdinRecord, StdioHost};
pub use source::{CompositeSource, SourceFile};
pub use target::{Charset, CompileTarget, CrateType};

/// Front-end diagnostics as a JSON array (lex + recovering parse + semantic).
///
/// Used by the playground for live editor markers. Does not lower MIR or run.
pub fn diagnose_json(source: &str) -> String {
    let file = SourceFile::anonymous(source);
    let options = LexOptions {
        allow_square_bracket_subscripts: true,
        allow_double_dash_comments: true,
    };
    let (tokens, _, lex_errors) = lex::tokenize_recovering(&file, &options, false);
    let mut items = Vec::new();
    for error in lex_errors {
        items.extend(error.to_json_values());
    }
    let (program, parse_errors) = parse::parse_lenient(&tokens);
    for error in parse_errors {
        items.extend(error.to_json_values());
    }
    if let Err(errors) = semantic::analyze_all(&program) {
        items.extend(errors.into_bundled().to_json_values());
    }
    serde_json::Value::Array(items).to_string()
}

/// Compiles and runs a Simula source file with the interpreter backend.
pub fn compile(source: &SourceFile) -> Result<String, CompileError> {
    match compile_with_options(source, &CompileOptions::for_run())? {
        CompileResult::Interpreted(output) => Ok(output),
        CompileResult::Artifact(_) => Err(CompileError::codegen(
            "compile() expects interpreter output",
        )),
        CompileResult::Checked => Err(CompileError::codegen(
            "compile() expects interpreter output",
        )),
    }
}

/// Compiles and runs anonymous in-memory Simula source.
pub fn compile_str(source: &str) -> Result<String, CompileError> {
    compile(&SourceFile::anonymous(source))
}

/// Like [`compile_str`], but drives the MIR interpreter's collector directly
/// and reports what it reclaimed.
pub fn run_mir_with_gc(
    source: &str,
    options: GcOptions,
) -> Result<(String, GcStats), CompileError> {
    let source = SourceFile::anonymous(source);
    let tokens = lex::tokenize_with_options(
        &source,
        &LexOptions {
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
        },
    )?;
    let program = parse::parse(&tokens)?;
    if let Err(errors) = semantic::analyze_all(&program) {
        return Err(errors.into_bundled());
    }
    let module = mir::lower_program_with_source(&program, &source.text)?;
    mir::interp::interpret_module_with_gc(&module, options)
}
