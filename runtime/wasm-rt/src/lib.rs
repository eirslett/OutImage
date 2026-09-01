//! Wasm32 helpers for generated Simula modules.
//!
//! The generated artifact is refs-only WasmGC; these functions see the
//! linear-memory text-frame ABI that codegen already builds before a host
//! call. Instantiated against the program's exported `memory`.
//!
//! `math-only` is a second cdylib: `no_std` ENVIRONMENT math (`sin`/`cos`/…)
//! without the text/random/`std` rodata that survives DCE in the full blob.

#![cfg_attr(all(feature = "math-only", target_arch = "wasm32"), no_std)]
#![cfg_attr(target_arch = "wasm32", deny(unsafe_op_in_unsafe_fn))]

#[cfg(all(feature = "math-only", target_arch = "wasm32"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

#[cfg(feature = "math-only")]
#[allow(dead_code)]
mod math;

#[cfg(not(feature = "math-only"))]
mod abi;
#[cfg(not(feature = "math-only"))]
mod arena;
#[cfg(not(feature = "math-only"))]
mod layout;
#[cfg(not(feature = "math-only"))]
mod runtime;
