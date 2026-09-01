//! Language Server Protocol support for Simula (`sim lsp`).

mod actions;
mod analysis;
mod capabilities;
mod config;
mod diagnostics;
mod document;
mod features;
mod format;
mod hierarchy;
mod hints;
mod lint;
mod nav;
mod position;
mod server;
mod symbols;
mod workspace;

pub use analysis::{AnalysisOptions, AnalysisSnapshot, analyze_document};
pub use config::{CheckOn, LspConfig};
pub use diagnostics::compile_errors_to_diagnostics;
pub use document::{Document, DocumentStore};
pub(crate) use lint::unused_compile_errors;
pub use position::{Encoding, PositionIndex, byte_span_to_range, position_to_byte};
pub use server::{Backend, run_stdio};
pub use symbols::SymbolIndex;

/// Crate / server display name advertised in `InitializeResult`.
pub const SERVER_NAME: &str = "sim";

/// Language id clients should use for `.sim` buffers.
pub const LANGUAGE_ID: &str = "simula";
