//! Interpreter-backed Debug Adapter Protocol (`sim dap`).
//!
//! Simula-aware statement stepping, breakpoints, and Simulation SQS.

mod cli;
mod format;
mod literal;
mod probe;
mod protocol;
mod server;
mod session;

pub use cli::{CliDebugOptions, run_cli_debug};
pub use format::{
    InlineFrameSnap, REF_ARRAY_BASE, REF_FRAME_BASE, REF_LOCALS, REF_OBJECT_BASE, REF_SIMULATION,
    REF_SQS, ThreadInfo, VarEntry, VariableSnapshot, condition_holds, evaluate_expression,
    format_log_message,
};
pub use literal::{DebugLiteral, parse_debug_value};
pub use probe::{
    DebugProbe, FrameInfo, PauseInfo, RunMode, SourceBreakpoint, active_probe, install_probe,
    poll_mir_span, pop_frame, push_frame, uninstall_probe,
};
pub use server::run_stdio;
pub use session::{LaunchConfig, PreparedProgram, launch_interpreted, prepare, run_with_probe};

/// Display name advertised to DAP clients.
pub const ADAPTER_NAME: &str = "sim";
