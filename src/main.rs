use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand, ValueEnum};
use outimage::error::{ColorChoice, DiagnosticConfig, SourceCache};
use outimage::source::{CompositeSource, SourceFile};
use outimage::{
    CompileError, CompileOptions, CompileResult, CompileTarget, compile_sources, lower_merged,
    run_stdio, unused_diagnostics,
};

/// Sentinel passed to `--emit-mir` when no `[PATH]` was supplied, so we can
/// tell "flag given without a value" apart from "flag not given at all"
/// (both of which would otherwise collapse to the same `Option` variant).
const EMIT_MIR_DEFAULT_PATH_SENTINEL: &str = "\0sim-emit-mir-default\0";

const DEFAULT_COMPILE_TARGET: &str = "native";

#[derive(Parser)]
#[command(
    name = "sim",
    version,
    about = "OutImage — a compiler for the Simula programming language"
)]
struct Cli {
    /// When to use colour in diagnostics
    #[arg(long, value_enum, global = true, default_value_t = CliColor::Auto)]
    color: CliColor,

    /// Emit diagnostics as JSON objects (one per line) instead of ariadne text
    #[arg(long, global = true)]
    json: bool,

    /// One-line diagnostics: code, title, file:line:col, lead sentence
    #[arg(long, global = true)]
    compact: bool,

    /// How much tutorial text to include (`full` is the default Elm-style body)
    #[arg(long = "explain-errors", global = true, value_enum, default_value_t = CliExplain::Full)]
    explain_errors: CliExplain,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum CliColor {
    #[default]
    Auto,
    Always,
    Never,
}

impl From<CliColor> for ColorChoice {
    fn from(value: CliColor) -> Self {
        match value {
            CliColor::Auto => ColorChoice::Auto,
            CliColor::Always => ColorChoice::Always,
            CliColor::Never => ColorChoice::Never,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum CliExplain {
    #[default]
    Full,
    Short,
}

impl From<CliExplain> for outimage::ExplainLevel {
    fn from(value: CliExplain) -> Self {
        match value {
            CliExplain::Full => outimage::ExplainLevel::Full,
            CliExplain::Short => outimage::ExplainLevel::Short,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Compile Simula source to a native executable or WebAssembly module
    Compile {
        /// Simula source file(s); multiple files are concatenated in order
        #[arg(required = true, num_args = 1.., value_name = "SOURCE")]
        sources: Vec<PathBuf>,
        /// Output path (defaults based on the first source file name and target)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Compilation target preset
        #[arg(short, long, default_value = DEFAULT_COMPILE_TARGET)]
        target: String,
        /// Disallow `[` `]` as array subscript delimiters (parentheses only)
        #[arg(long)]
        no_square_bracket_subscripts: bool,
        /// Do not treat `--` as a line comment (parse as two minus operators)
        #[arg(long)]
        no_double_dash_comments: bool,
        /// Write a MIR dump. If PATH is omitted, writes `<output>.mir`.
        #[arg(
            long,
            num_args = 0..=1,
            value_name = "PATH",
            default_missing_value = EMIT_MIR_DEFAULT_PATH_SENTINEL
        )]
        emit_mir: Option<PathBuf>,
        /// Write a native object file (`.o`) and skip linking.
        #[arg(long = "emit-obj")]
        emit_obj: bool,
        /// Extra linker input: object, archive, dylib, or library name
        /// (`add.o`, `libfoo`, `-lm`). Repeatable.
        #[arg(long = "link", value_name = "ITEM", allow_hyphen_values = true)]
        extra_link: Vec<String>,
        /// Separately checked Simula module, merged after lowering. Repeatable.
        /// Identification `= "utils"` matches `--with utils.sim`. Not concatenation.
        #[arg(long = "with", value_name = "FILE")]
        with_modules: Vec<PathBuf>,
        /// Native packaging: `bin` (executable) or `lib` (shared library, no
        /// process entry). Ignored on wasm — the module always exports `_start`
        /// plus public procedures.
        #[arg(long = "crate-type", value_enum, default_value_t = outimage::CrateType::Bin)]
        crate_type: outimage::CrateType,
        /// FFI text copy encoding: `latin1` (default, one byte per rank) or `utf8`.
        #[arg(long = "charset", value_enum, default_value_t = outimage::Charset::Latin1)]
        charset: outimage::Charset,
        /// Write Cranelift machine-code disassembly. If PATH is omitted,
        /// writes `<output>.s`.
        #[arg(
            long = "emit-asm",
            num_args = 0..=1,
            value_name = "PATH",
            default_missing_value = EMIT_MIR_DEFAULT_PATH_SENTINEL
        )]
        emit_asm: Option<PathBuf>,
        /// Enable debug info: also writes a `<output>.sim-map` JSON
        /// source map from MIR instruction spans (native `-g` also emits DWARF).
        #[arg(short = 'g', long = "debug")]
        debug: bool,
        /// Do not write `wasm_host.mjs` next to a wasm module. Ignored on native.
        #[arg(long = "no-wasm-host")]
        no_wasm_host: bool,
    },
    /// Interpret Simula source and print output (development / tests)
    Run {
        /// Simula source file(s); multiple files are concatenated in order
        #[arg(required = true, num_args = 1.., value_name = "SOURCE")]
        sources: Vec<PathBuf>,
        /// Disallow `[` `]` as array subscript delimiters (parentheses only)
        #[arg(long)]
        no_square_bracket_subscripts: bool,
        /// Do not treat `--` as a line comment (parse as two minus operators)
        #[arg(long)]
        no_double_dash_comments: bool,
        /// Separately checked Simula module, merged after lowering. Repeatable.
        #[arg(long = "with", value_name = "FILE")]
        with_modules: Vec<PathBuf>,
        /// FFI text copy encoding: `latin1` (default) or `utf8`.
        #[arg(long = "charset", value_enum, default_value_t = outimage::Charset::Latin1)]
        charset: outimage::Charset,
    },
    /// Lower Simula source to MIR and print the dump (no codegen/linking)
    Mir {
        /// Simula source file(s); multiple files are concatenated in order
        #[arg(required = true, num_args = 1.., value_name = "SOURCE")]
        sources: Vec<PathBuf>,
        /// Disallow `[` `]` as array subscript delimiters (parentheses only)
        #[arg(long)]
        no_square_bracket_subscripts: bool,
        /// Do not treat `--` as a line comment (parse as two minus operators)
        #[arg(long)]
        no_double_dash_comments: bool,
        /// Separately checked Simula module, merged after lowering. Repeatable.
        #[arg(long = "with", value_name = "FILE")]
        with_modules: Vec<PathBuf>,
        /// FFI text copy encoding: `latin1` (default) or `utf8`.
        #[arg(long = "charset", value_enum, default_value_t = outimage::Charset::Latin1)]
        charset: outimage::Charset,
    },
    /// Print an annotated AST dump with source spans (compiler debugging)
    Ast {
        /// Simula source file(s); multiple files are concatenated in order
        #[arg(required = true, num_args = 1.., value_name = "SOURCE")]
        sources: Vec<PathBuf>,
        /// Disallow `[` `]` as array subscript delimiters (parentheses only)
        #[arg(long)]
        no_square_bracket_subscripts: bool,
        /// Do not treat `--` as a line comment (parse as two minus operators)
        #[arg(long)]
        no_double_dash_comments: bool,
    },
    /// Generate a vscode-tmgrammar-test unit file from Simula source
    GrammarTest {
        /// Simula source file; omit or pass `-` to read stdin
        #[arg(value_name = "SOURCE")]
        source: Option<PathBuf>,
        /// SYNTAX TEST description
        #[arg(short, long)]
        description: Option<String>,
    },
    /// Lex, parse, analyze, and lower to MIR without emitting an artifact
    Check {
        /// Simula source file(s); multiple files are concatenated in order
        #[arg(required = true, num_args = 1.., value_name = "SOURCE")]
        sources: Vec<PathBuf>,
        /// Disallow `[` `]` as array subscript delimiters (parentheses only)
        #[arg(long)]
        no_square_bracket_subscripts: bool,
        /// Do not treat `--` as a line comment (parse as two minus operators)
        #[arg(long)]
        no_double_dash_comments: bool,
        /// Separately checked Simula module, merged after lowering. Repeatable.
        #[arg(long = "with", value_name = "FILE")]
        with_modules: Vec<PathBuf>,
        /// FFI text copy encoding: `latin1` (default) or `utf8`.
        #[arg(long = "charset", value_enum, default_value_t = outimage::Charset::Latin1)]
        charset: outimage::Charset,
        /// Do not report unused locals, parameters, or labels (`W0001`)
        #[arg(long = "no-unused")]
        no_unused: bool,
    },
    /// Explain a diagnostic code (`E0201`, `E-lex`, …)
    Explain {
        /// Diagnostic code to explain
        code: String,
    },
    /// Run the Language Server Protocol server on stdin/stdout
    #[cfg(feature = "lsp")]
    Lsp {
        /// Use stdin/stdout for communication with the client
        #[arg(short, long)]
        stdio: bool,
    },
    /// Run the Debug Adapter Protocol server on stdin/stdout (interpreter)
    Dap,
    /// Debug Simula under the interpreter probe (CLI sugar over the DAP engine)
    Debug {
        /// Simula source file
        #[arg(value_name = "SOURCE")]
        source: PathBuf,
        /// Break at 1-based source line (repeatable)
        #[arg(short = 'b', long = "break", value_name = "LINE")]
        breakpoints: Vec<u32>,
        /// Stop before the first statement
        #[arg(long)]
        stop_on_entry: bool,
        /// Debugger command to run at each pause (repeatable; otherwise read stdin)
        #[arg(short = 'c', long = "command", value_name = "CMD")]
        commands: Vec<String>,
        /// Log each stop to stderr (`reason` / line / frame count)
        #[arg(long)]
        trace: bool,
        /// Disallow `[` `]` as array subscript delimiters (parentheses only)
        #[arg(long)]
        no_square_bracket_subscripts: bool,
        /// Do not treat `--` as a line comment (parse as two minus operators)
        #[arg(long)]
        no_double_dash_comments: bool,
    },
    /// List supported cross-compilation targets
    Targets,
}

enum CliError {
    Message(String),
    Compile {
        cache: SourceCache,
        primary: SourceFile,
        error: CompileError,
        config: DiagnosticConfig,
        json: bool,
    },
}

impl CliError {
    fn message(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }

    fn io(path: &std::path::Path, error: std::io::Error) -> Self {
        Self::Message(format!("failed to read {}: {error}", path.display()))
    }

    fn compile_composite(
        composite: &CompositeSource,
        error: CompileError,
        config: DiagnosticConfig,
        json: bool,
    ) -> Self {
        Self::Compile {
            cache: composite.to_cache(),
            primary: composite.as_source_file().clone(),
            error,
            config,
            json,
        }
    }

    fn report(self) {
        match self {
            Self::Message(message) => eprintln!("error: {message}"),
            Self::Compile {
                cache,
                primary,
                error,
                config,
                json,
            } => {
                if json {
                    print_json_diagnostics(&error);
                    return;
                }
                // Print the primary error, then any related siblings (multi-error
                // semantic analysis packs extras onto `error.related`).
                let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
                if let Err(io_error) =
                    error.write_cached(&cache, &primary, std::io::stderr(), &config, tty)
                {
                    eprintln!("error: {error}");
                    eprintln!("(also failed to render diagnostic: {io_error})");
                } else {
                    print_related_errors(&error.related, &cache, &primary, &config, tty);
                }
            }
        }
    }
}

fn print_json_diagnostics(error: &CompileError) {
    println!("{}", error.to_json_value());
    for related in &error.related {
        print_json_diagnostics(related);
    }
}

fn print_related_errors(
    related: &[CompileError],
    cache: &SourceCache,
    primary: &SourceFile,
    config: &DiagnosticConfig,
    tty: bool,
) {
    for error in related {
        let _ = error.write_cached(cache, primary, std::io::stderr(), config, tty);
        print_related_errors(&error.related, cache, primary, config, tty);
    }
}

fn diagnostic_config(color: CliColor, compact: bool, explain: CliExplain) -> DiagnosticConfig {
    let mut config = DiagnosticConfig::for_stderr()
        .with_color(color.into())
        .with_compact(compact)
        .with_explain(explain.into());
    match color {
        CliColor::Always => config = config.with_unicode(true),
        CliColor::Never => config = config.with_unicode(false),
        CliColor::Auto => {}
    }
    config
}

fn main() {
    if let Err(error) = run() {
        error.report();
        process::exit(1);
    }
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    let diag = diagnostic_config(cli.color, cli.compact, cli.explain_errors);
    let json = cli.json;
    match cli.command {
        Command::Compile {
            sources,
            output,
            target,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            emit_mir,
            emit_obj,
            extra_link,
            with_modules,
            crate_type,
            charset,
            emit_asm,
            debug,
            no_wasm_host,
        } => compile_command(
            sources,
            output,
            target,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            emit_mir,
            emit_obj,
            extra_link,
            with_modules,
            crate_type,
            charset,
            emit_asm,
            debug,
            no_wasm_host,
            diag,
            json,
        ),
        Command::Run {
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            with_modules,
            charset,
        } => run_command(
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            with_modules,
            charset,
            diag,
            json,
        ),
        Command::Mir {
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            with_modules,
            charset,
        } => mir_command(
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            with_modules,
            charset,
            diag,
            json,
        ),
        Command::Ast {
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
        } => ast_command(
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            diag,
            json,
        ),
        Command::GrammarTest {
            source,
            description,
        } => grammar_test_command(source, description, diag, json),
        Command::Check {
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            with_modules,
            charset,
            no_unused,
        } => check_command(
            sources,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            with_modules,
            charset,
            no_unused,
            diag,
            json,
        ),
        Command::Explain { code } => explain_command(&code),
        #[cfg(feature = "lsp")]
        Command::Lsp { stdio } => lsp_command(stdio),
        Command::Dap => dap_command(),
        Command::Debug {
            source,
            breakpoints,
            stop_on_entry,
            commands,
            trace,
            no_square_bracket_subscripts,
            no_double_dash_comments,
        } => debug_command(
            source,
            breakpoints,
            stop_on_entry,
            commands,
            trace,
            no_square_bracket_subscripts,
            no_double_dash_comments,
            diag,
            json,
        ),
        Command::Targets => {
            list_targets();
            Ok(())
        }
    }
}

#[cfg(feature = "lsp")]
fn lsp_command(stdio: bool) -> Result<(), CliError> {
    // `--stdio` is a presence flag. Other transports are not implemented, so
    // `sim lsp` and `sim lsp --stdio` both use stdin/stdout.
    let _ = stdio;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::message(format!("failed to start async runtime: {error}")))?;
    runtime.block_on(outimage::lsp::run_stdio());
    Ok(())
}

fn dap_command() -> Result<(), CliError> {
    outimage::debug::run_stdio();
    Ok(())
}

fn debug_command(
    source: PathBuf,
    breakpoints: Vec<u32>,
    stop_on_entry: bool,
    commands: Vec<String>,
    trace: bool,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let composite = load_sources(std::slice::from_ref(&source))?;
    let opts = outimage::debug::CliDebugOptions {
        program: source,
        breakpoints,
        stop_on_entry,
        allow_square_bracket_subscripts: !no_square_bracket_subscripts,
        allow_double_dash_comments: !no_double_dash_comments,
        commands,
        trace,
    };
    match outimage::debug::run_cli_debug(opts) {
        Ok(output) => {
            print!("{output}");
            Ok(())
        }
        Err(error) => Err(CliError::compile_composite(&composite, error, diag, json)),
    }
}

fn load_sources(paths: &[PathBuf]) -> Result<CompositeSource, CliError> {
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        files.push(SourceFile::from_path(path).map_err(|error| CliError::io(path, error))?);
    }
    Ok(CompositeSource::concat(files))
}

fn compile_command(
    source_paths: Vec<PathBuf>,
    output: Option<PathBuf>,
    target_name: String,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    emit_mir: Option<PathBuf>,
    emit_obj: bool,
    extra_link: Vec<String>,
    with_modules: Vec<PathBuf>,
    crate_type: outimage::CrateType,
    charset: outimage::Charset,
    emit_asm: Option<PathBuf>,
    debug: bool,
    no_wasm_host: bool,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let target: CompileTarget = target_name
        .parse()
        .map_err(|error: CompileError| CliError::message(error.to_string()))?;

    if target.is_wasm() && crate_type == outimage::CrateType::Lib {
        eprintln!(
            "note: --crate-type is a native linker option (executable vs shared library); \
             on wasm it makes no difference (the module always exports _start plus public procedures)"
        );
    }

    let composite = load_sources(&source_paths)?;
    let first_path = &source_paths[0];

    let output_path = output.unwrap_or_else(|| {
        if emit_obj {
            first_path.with_extension("o")
        } else {
            default_output_path(first_path, target, crate_type)
        }
    });

    let mut options = CompileOptions::for_compile(output_path, target);
    options.allow_square_bracket_subscripts = !no_square_bracket_subscripts;
    options.allow_double_dash_comments = !no_double_dash_comments;
    // `--emit-mir` alone (no PATH) is parsed as `Some("")` via
    // `default_missing_value`; an empty path means "use the driver's default".
    if let Some(path) = emit_mir {
        options.emit_mir = true;
        if path.as_os_str() != EMIT_MIR_DEFAULT_PATH_SENTINEL {
            options.mir_output = Some(path);
        }
    }
    if let Some(path) = emit_asm {
        options.emit_asm = true;
        if path.as_os_str() != EMIT_MIR_DEFAULT_PATH_SENTINEL {
            options.asm_output = Some(path);
        }
    }
    options.emit_obj = emit_obj;
    options.debug_info = debug;
    options.crate_type = crate_type;
    options.extra_link = extra_link;
    options.with_modules = with_modules;
    options.charset = charset;
    options.write_wasm_host = !no_wasm_host;

    match compile_sources(composite.origins(), &options) {
        Ok(CompileResult::Artifact(path)) => {
            println!("{}", path.display());
            if let Some(hint) = target.run_hint(&path) {
                eprintln!("{hint}");
            }
            Ok(())
        }
        Ok(CompileResult::Interpreted(_)) => Err(CliError::message(
            "unexpected interpreter result from compile",
        )),
        Ok(CompileResult::Checked) => {
            Err(CliError::message("unexpected check result from compile"))
        }
        Err(error) => Err(CliError::compile_composite(&composite, error, diag, json)),
    }
}

fn front_end_from_composite(
    composite: &CompositeSource,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    diag: &DiagnosticConfig,
    json: bool,
) -> Result<outimage::ast::Program, CliError> {
    let lex_options = outimage::LexOptions {
        allow_square_bracket_subscripts: !no_square_bracket_subscripts,
        allow_double_dash_comments: !no_double_dash_comments,
    };
    let source = composite.as_source_file();
    let tokens = match outimage::lex::tokenize_with_options(source, &lex_options) {
        Ok(tokens) => tokens,
        Err(error) => {
            return Err(CliError::compile_composite(
                composite,
                error.remap_to_origins(composite),
                *diag,
                json,
            ));
        }
    };
    let program = match outimage::parse::parse(&tokens) {
        Ok(program) => program,
        Err(error) => {
            return Err(CliError::compile_composite(
                composite,
                error.remap_to_origins(composite),
                *diag,
                json,
            ));
        }
    };
    if let Err(errors) = outimage::semantic::analyze_all(&program) {
        return Err(CliError::compile_composite(
            composite,
            errors.into_bundled().remap_to_origins(composite),
            *diag,
            json,
        ));
    }
    Ok(program)
}

fn mir_command(
    source_paths: Vec<PathBuf>,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    with_modules: Vec<PathBuf>,
    charset: outimage::Charset,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let composite = load_sources(&source_paths)?;
    let mut options = CompileOptions::for_check();
    options.allow_square_bracket_subscripts = !no_square_bracket_subscripts;
    options.allow_double_dash_comments = !no_double_dash_comments;
    options.with_modules = with_modules;
    options.charset = charset;
    let module = match lower_merged(composite.as_source_file(), &options) {
        Ok(module) => module,
        Err(error) => {
            return Err(CliError::compile_composite(
                &composite,
                error.remap_to_origins(&composite),
                diag,
                json,
            ));
        }
    };

    print!("{}", module.dump());
    Ok(())
}

fn ast_command(
    source_paths: Vec<PathBuf>,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let composite = load_sources(&source_paths)?;
    let program = front_end_from_composite(
        &composite,
        no_square_bracket_subscripts,
        no_double_dash_comments,
        &diag,
        json,
    )?;
    print!("{}", outimage::ast_dump::dump_program(&program));
    Ok(())
}

fn grammar_test_command(
    source_path: Option<PathBuf>,
    description: Option<String>,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let from_stdin = source_path
        .as_ref()
        .is_none_or(|path| path.as_os_str() == "-");
    let (input, default_description) = if from_stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .map_err(|error| CliError::message(format!("failed to read stdin: {error}")))?;
        (buf, "generated".to_string())
    } else {
        let path = source_path.as_ref().expect("path checked above");
        let text = std::fs::read_to_string(path).map_err(|error| CliError::io(path, error))?;
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("generated")
            .to_string();
        (text, name)
    };
    let description = description.unwrap_or(default_description);
    match outimage::grammar_test::render_syntax_test(&input, Some(&description)) {
        Ok(output) => {
            print!("{output}");
            Ok(())
        }
        Err(error) => {
            let primary = SourceFile::anonymous(input.as_str());
            Err(CliError::Compile {
                cache: SourceCache::from_file(&primary),
                primary,
                error,
                config: diag,
                json,
            })
        }
    }
}

fn check_command(
    source_paths: Vec<PathBuf>,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    with_modules: Vec<PathBuf>,
    charset: outimage::Charset,
    no_unused: bool,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let composite = load_sources(&source_paths)?;
    let mut options = CompileOptions::for_check();
    options.allow_square_bracket_subscripts = !no_square_bracket_subscripts;
    options.allow_double_dash_comments = !no_double_dash_comments;
    options.with_modules = with_modules;
    options.charset = charset;
    options.enable_unused_lints = !no_unused;
    match compile_sources(composite.origins(), &options) {
        Ok(CompileResult::Checked) => {
            emit_unused_warnings(&composite, &options, diag, json)?;
            Ok(())
        }
        Ok(CompileResult::Interpreted(_)) | Ok(CompileResult::Artifact(_)) => {
            Err(CliError::message("unexpected backend result from check"))
        }
        Err(error) => Err(CliError::compile_composite(&composite, error, diag, json)),
    }
}

fn emit_unused_warnings(
    composite: &CompositeSource,
    options: &CompileOptions,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let warnings = unused_diagnostics(composite.as_source_file(), options);
    if warnings.is_empty() {
        return Ok(());
    }
    let cache = composite.to_cache();
    let primary = composite.as_source_file();
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    for warning in warnings {
        let warning = warning.remap_to_origins(composite);
        if json {
            println!("{}", warning.to_json_value());
        } else if let Err(io_error) =
            warning.write_cached(&cache, primary, std::io::stderr(), &diag, tty)
        {
            eprintln!("warning: {warning}");
            eprintln!("(also failed to render diagnostic: {io_error})");
        }
    }
    Ok(())
}

fn explain_command(code: &str) -> Result<(), CliError> {
    match outimage::diagnostics::explain(code) {
        Ok(text) => {
            print!("{text}");
            Ok(())
        }
        Err(message) => Err(CliError::message(message)),
    }
}

fn run_command(
    source_paths: Vec<PathBuf>,
    no_square_bracket_subscripts: bool,
    no_double_dash_comments: bool,
    with_modules: Vec<PathBuf>,
    charset: outimage::Charset,
    diag: DiagnosticConfig,
    json: bool,
) -> Result<(), CliError> {
    let composite = load_sources(&source_paths)?;
    let mut options = CompileOptions::for_run();
    options.allow_square_bracket_subscripts = !no_square_bracket_subscripts;
    options.allow_double_dash_comments = !no_double_dash_comments;
    options.with_modules = with_modules;
    options.charset = charset;
    match run_stdio(composite.as_source_file(), &options) {
        Ok(()) => Ok(()),
        Err(error) => Err(CliError::compile_composite(&composite, error, diag, json)),
    }
}

fn default_output_path(
    source_path: &Path,
    target: CompileTarget,
    crate_type: outimage::CrateType,
) -> PathBuf {
    let stem = source_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("sim");

    let extension = target.default_output_extension_for(crate_type);
    if extension.is_empty() {
        PathBuf::from(stem)
    } else {
        PathBuf::from(format!("{stem}.{extension}"))
    }
}

fn list_targets() {
    println!("Supported targets:\n");
    for target in CompileTarget::all() {
        let note = match target {
            CompileTarget::WindowsX86_64 => " (Windows host; needs MSVC LIB)",
            CompileTarget::WasmNode | CompileTarget::WasmBrowser => " (portable AOT)",
            _ => "",
        };
        println!("  {:<16} {}{}", target, target.triple(), note);
    }
    println!("\nBackends:");
    println!("  native targets  Cranelift (object) + host linker + bundled runtime");
    println!("  wasm targets    wasm-encoder (pure Rust)");
    println!("\nNotes:");
    println!("  • --crate-type is native packaging (exe vs shared library); ignored on wasm.");
    println!(
        "  • Wasm compile writes wasm_host.mjs next to the module (skip with --no-wasm-host)."
    );
    println!("  • Native link uses the host-built C runtime — cross-OS link is not supported yet.");
    println!("  • Prefer wasm-node / wasm-browser for portable AOT.");
    println!("\nExamples:");
    println!("  sim compile hello.sim --target native -o hello");
    println!("  sim compile prog.sim --link add.o -o prog");
    println!("  sim compile api.sim --crate-type lib -o libapi");
    println!("  sim compile hello.sim --target wasm-node -o hello.wasm");
    println!("  node hello.mjs");
    println!("  sim compile hello.sim --target wasm-browser -o hello.wasm");
    println!("  # then open hello.html in a browser");
    println!("  sim run hello.sim");
    println!("  sim run a.sim b.sim   # concatenate sources in order");
    println!("  sim check hello.sim");
    println!("  sim debug --break 3 -c continue hello.sim");
    println!("  sim explain E-semantic");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn lsp_stdio_is_a_presence_flag() {
        let omitted = Cli::try_parse_from(["sim", "lsp"]).expect("parse lsp");
        match omitted.command {
            Command::Lsp { stdio } => assert!(!stdio, "omitting --stdio should be false"),
            _ => panic!("expected Lsp"),
        }

        let present = Cli::try_parse_from(["sim", "lsp", "--stdio"]).expect("parse lsp --stdio");
        match present.command {
            Command::Lsp { stdio } => assert!(stdio, "passing --stdio should be true"),
            _ => panic!("expected Lsp"),
        }

        let short = Cli::try_parse_from(["sim", "lsp", "-s"]).expect("parse lsp -s");
        match short.command {
            Command::Lsp { stdio } => assert!(stdio, "passing -s should be true"),
            _ => panic!("expected Lsp"),
        }

        assert!(
            Cli::try_parse_from(["sim", "lsp", "--stdio", "true"]).is_err(),
            "--stdio should not take a value"
        );
    }

    #[test]
    fn compile_accepts_link_and_crate_type() {
        let parsed = Cli::try_parse_from([
            "sim",
            "compile",
            "prog.sim",
            "--link",
            "add.o",
            "--link",
            "-lm",
            "--crate-type",
            "lib",
            "--with",
            "utils.sim",
            "--charset",
            "utf8",
            "--no-wasm-host",
        ])
        .expect("parse compile --link --crate-type");
        match parsed.command {
            Command::Compile {
                extra_link,
                crate_type,
                with_modules,
                charset,
                no_wasm_host,
                ..
            } => {
                assert_eq!(extra_link, ["add.o", "-lm"]);
                assert_eq!(crate_type, outimage::CrateType::Lib);
                assert_eq!(with_modules, [PathBuf::from("utils.sim")]);
                assert_eq!(charset, outimage::Charset::Utf8);
                assert!(no_wasm_host);
            }
            _ => panic!("expected Compile"),
        }
    }
}
