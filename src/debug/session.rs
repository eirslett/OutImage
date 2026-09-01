//! Launch an interpreted debug session for a `.sim` file.

use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::Program;
use crate::error::CompileError;
use crate::lex::LexOptions;
use crate::source::SourceFile;

use super::probe::{DebugProbe, install_probe, uninstall_probe};

#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub program: PathBuf,
    pub stop_on_entry: bool,
    pub allow_square_bracket_subscripts: bool,
    pub allow_double_dash_comments: bool,
}

pub struct PreparedProgram {
    pub source: SourceFile,
    pub program: Program,
}

pub fn prepare(config: &LaunchConfig) -> Result<PreparedProgram, CompileError> {
    let source = SourceFile::from_path(&config.program).map_err(|error| {
        CompileError::codegen(format!(
            "failed to read {}: {error}",
            config.program.display()
        ))
    })?;
    let lex_options = LexOptions {
        allow_square_bracket_subscripts: config.allow_square_bracket_subscripts,
        allow_double_dash_comments: config.allow_double_dash_comments,
    };
    let tokens = crate::lex::tokenize_with_options(&source, &lex_options)?;
    let program = crate::parse::parse(&tokens)?;
    if let Err(errors) = crate::semantic::analyze_all(&program) {
        return Err(errors.into_bundled());
    }
    Ok(PreparedProgram { source, program })
}

/// Installs `probe`, runs the MIR interpreter, then uninstalls the probe.
pub fn run_with_probe(
    prepared: &PreparedProgram,
    probe: Arc<DebugProbe>,
) -> Result<String, CompileError> {
    install_probe(probe);
    let result = (|| {
        let module =
            crate::mir::lower_program_with_source(&prepared.program, &prepared.source.text)?;
        crate::mir::interp::interpret_module(&module)
    })();
    uninstall_probe();
    result
}

pub fn launch_interpreted(
    config: &LaunchConfig,
    probe: Arc<DebugProbe>,
) -> Result<String, CompileError> {
    let prepared = prepare(config)?;
    run_with_probe(&prepared, probe)
}
