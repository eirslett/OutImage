//! Runtime support for compiled Simula programs.
//!
//! Standard library `external` procedures are implemented here in Rust.

pub mod environment;
pub mod error;
pub mod fs;
pub mod host;
pub mod io;
pub mod text;

pub use environment::EnvironmentRuntimeState;
pub use host::{CapturingHost, IoHost, ReadLine, StdinRecord, StdioHost};
pub use io::{Input, Output};
