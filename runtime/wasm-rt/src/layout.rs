//! Linear-memory slab this module occupies when it shares the generated
//! program's memory. Must stay in lockstep with `src/codegen/wasm/mod.rs`
//! (`TEXT_BASE`) and `build.rs` (`--initial-memory`).
//!
//! rustc/wasm-ld uses `--stack-first` with a 1MiB stack at address 0. Generated
//! header words live in the unused tail of that stack (0..8320). Runtime data
//! and the bump arena occupy the rest of this 2MiB window.

/// Exclusive end of the runtime slab; generated `TEXT_BASE` starts here.
pub const C_RT_END: u32 = 2 * 1024 * 1024;
