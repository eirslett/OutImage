//! Exercises the runtime's quasi-parallel sequencing (Standard chapter 7) below
//! any compiler involvement: `runtime/coro.c` switches stacks with hand-written
//! assembly, and `runtime/sequencing.c` implements the detach / call / resume
//! rules on top of it. Both are worth proving directly rather than only through
//! compiled Simula programs.

mod common;

use common::run_c_fixture_ex;

#[test]
fn stack_switching_preserves_nested_frames() {
    let stdout = run_c_fixture_ex(
        "coro_check.c",
        &["coro.c", "gc.c"],
        &["runtime_roots_stub.c", "seq_roots_stub.c"],
    );
    // 1 main; a starts and parks; 2 main; b starts and transfers straight to a
    // (symmetric, not via main); A resumes a; d enters a nested call that parks
    // mid-frame; 3 main; D returns into that same frame with its locals intact;
    // z ends a; 4 main.
    assert_eq!(stdout.trim(), "1a2bAd3Dz4");
}

#[test]
fn sequencing_matches_the_standards_worked_example() {
    let stdout = run_c_fixture_ex(
        "sequencing_check.c",
        &["sequencing.c", "coro.c", "gc.c"],
        &["runtime_roots_stub.c"],
    );
    let mut lines = stdout.lines();

    // The annotated example of 7.4, whose figures fix the answer. The
    // interesting step is `2` -> `q`: figure 7.7 leaves X2's reactivation point
    // inside P2, which is running on X3's stack, and `call(X2)` from the
    // outermost block must continue exactly there.
    assert_eq!(lines.next(), Some("c1p11c2s2c3gkp22qre23"));

    // The inner subblock declares a class, so by 7.2 it is the system head; the
    // resumed object's final end therefore returns after the resume (F, G)
    // rather than out to the program block.
    assert_eq!(lines.next(), Some("AAABCDEFGAB"));
}
