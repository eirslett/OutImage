//! Strip unused `simrt` exports and GC the prebuilt helper module.

use std::collections::HashSet;

use crate::error::CompileError;

use super::used_env;

/// Drop `simrt_*` exports that are not in `keep`, then DCE.
///
/// `keep` is wasm export names (`simrt_sin`, …). An empty set is valid:
/// the helper still instantiates (imported `memory` / `abort_message`).
/// Math-only keep-sets shake the `no_std` blob so text/`fmt` rodata is gone.
pub(in crate::codegen::wasm) fn shake_runtime(
    keep: &HashSet<String>,
) -> Result<Vec<u8>, CompileError> {
    let bytes = if used_env::keep_fits_math_rt(keep) {
        crate::bundled::WASM_RUNTIME_MATH
    } else {
        crate::bundled::WASM_RUNTIME
    };
    shake_runtime_bytes(bytes, keep)
}

fn shake_runtime_bytes(bytes: &[u8], keep: &HashSet<String>) -> Result<Vec<u8>, CompileError> {
    let mut module = walrus::Module::from_buffer(bytes)
        .map_err(|error| CompileError::codegen(format!("simrt parse failed: {error}")))?;
    let drop: Vec<_> = module
        .exports
        .iter()
        .filter(|export| export.name.starts_with("simrt_") && !keep.contains(&export.name))
        .map(|export| export.id())
        .collect();
    for id in drop {
        module.exports.delete(id);
    }
    walrus::passes::gc::run(&mut module);
    Ok(module.emit_wasm())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn export_names(bytes: &[u8]) -> HashSet<String> {
        let module = walrus::Module::from_buffer(bytes).expect("parse shaken rt");
        module.exports.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn shake_keeps_sin_cos_drops_text_getint() {
        let keep = ["simrt_sin", "simrt_cos"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let shaken = shake_runtime(&keep).expect("shake");
        let names = export_names(&shaken);
        assert!(names.contains("simrt_sin"), "{names:?}");
        assert!(names.contains("simrt_cos"), "{names:?}");
        assert!(
            !names.contains("simrt_text_getint"),
            "text_getint should be stripped: {names:?}"
        );
        // no_std math blob: libm bodies, no text/`fmt` rodata (~5.5 kB).
        assert!(
            shaken.len() < 8_000,
            "sin/cos shaken {} should stay well under the old 26 kB std floor",
            shaken.len()
        );
        assert!(
            shaken.len() < crate::bundled::WASM_RUNTIME.len(),
            "shaken {} >= full {}",
            shaken.len(),
            crate::bundled::WASM_RUNTIME.len()
        );
    }

    #[test]
    fn shake_empty_keep_is_valid() {
        let shaken = shake_runtime(&HashSet::new()).expect("shake");
        walrus::Module::from_buffer(&shaken).expect("empty keep-set must still parse");
        let names = export_names(&shaken);
        assert!(
            !names.iter().any(|n| n == "simrt_sin"),
            "empty keep should drop sin: {names:?}"
        );
        // Recorded after no_std math blob: empty keep has no libm and no std rodata.
        assert!(
            shaken.len() < 4_000,
            "empty keep shaken {} should stay under 4 kB",
            shaken.len()
        );
        assert!(
            shaken.len() < crate::bundled::WASM_RUNTIME.len(),
            "empty shaken {} >= full {}",
            shaken.len(),
            crate::bundled::WASM_RUNTIME.len()
        );
    }
}
