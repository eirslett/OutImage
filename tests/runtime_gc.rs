//! Exercises the native collector (`runtime/gc.c`) below any compiler
//! involvement, the way `tests/runtime_coro.rs` does for stack switching.
//! `tests/memory_gc.rs` covers the same rules through compiled Simula
//! programs; this file pins the C-level behavior — what is reclaimed and what
//! is rooted — so a rooting bug reports itself as a named assertion instead
//! of a rare wrong answer.

mod common;

use common::{NATIVE_RUNTIME_SOURCES, run_c_fixture};

#[test]
fn mark_sweep_reclaims_and_roots_the_right_blocks() {
    let stdout = run_c_fixture("gc_check.c", NATIVE_RUNTIME_SOURCES);

    if stdout.contains("SKIP collector disabled") {
        eprintln!("skipping: collector disabled on this host");
        return;
    }

    // Each case prints one PASS/FAIL line; a FAIL also exits non-zero, so the
    // assertions above have already fired. These name the cases so a dropped
    // or renamed test in the fixture cannot silently stop being run.
    for expected in [
        // Unreachable garbage really is freed, not just accounted for.
        "PASS unreachable objects are reclaimed",
        // The precise root frame sees a reference stored in an explicit slot.
        "PASS a reference held across a collection survives",
        // An integer bit-pattern equal to a heap address is not a root.
        "PASS an integer that looks like a pointer does not keep an object alive",
        // A cycle no reference count could break: two objects pointing at each
        // other through the word a SIMSET `SUC` link occupies.
        "PASS a ring reached through one live member survives whole",
        "PASS a ring with no external reference is collected",
        // Step 2: texts are swept once character storage lives in the managed block.
        "PASS unreachable text frames and text objects are reclaimed",
        "PASS an interior text content pointer keeps its text object alive",
        // Text array slots are traced precisely; int64/ref arrays and object
        // fields conservatively.
        "PASS a rooted text array survives with its elements",
        "PASS an array reached only through an object field survives",
        // An explicit C-runtime root with no user-visible reference.
        "PASS SYSOUT survives collection without a user reference",
        // Phase 3 step 3: the sweep hands blocks to a free list, not back to
        // the host, so same-size churn stops growing the heap.
        "PASS swept blocks are reused by later same-size allocations",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in gc_check output:\n{stdout}"
        );
    }

    assert!(
        stdout.contains("DONE collections="),
        "gc_check should have run to completion:\n{stdout}"
    );
}

#[test]
fn overflow_predicates_reject_wrapped_extents() {
    let stdout = run_c_fixture("bounds_proof.c", &["safety.c"]);
    for expected in [
        "PASS a one-dimensional extent is the inclusive length",
        "PASS empty dimension is a zero-length array",
        "PASS product of extents is rejected on int64 overflow",
        "PASS a single dimension that does not fit in int64 is rejected",
        "PASS header plus payload is rejected on size_t overflow",
        "PASS a bounds memcpy that would wrap size_t is rejected",
        "PASS object field offsets are rejected outside the payload",
        "DONE",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in bounds_proof output:\n{stdout}"
        );
    }
}

#[test]
fn overflow_predicates_cbmc_if_present() {
    use std::process::Command;

    let version = Command::new("cbmc").arg("--version").output();
    let Ok(version) = version else {
        return;
    };
    if !version.status.success() {
        return;
    }

    for function in [
        "simrt_cbmc_array_count",
        "simrt_cbmc_array_total",
        "simrt_cbmc_array_header",
        "simrt_cbmc_object_offset",
    ] {
        let output = Command::new("cbmc")
            .current_dir(common::repo_root())
            .args([
                "runtime/safety.c",
                "tests/fixtures/runtime/bounds_proof.c",
                "--function",
                function,
                "--unwind",
                "5",
                "--unwinding-assertions",
                "-I",
                "runtime",
            ])
            .output()
            .expect("cbmc should run once --version succeeded");
        assert!(
            output.status.success(),
            "cbmc {function} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
