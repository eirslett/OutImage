//! Memory and GC behavior across all three
//! backends.
//!
//! It starts with the Phase 0 baseline — behavior a collector must not break
//! (terminated-but-referenced objects, detach/resume reactivation chains, many
//! small allocations, wasm heap growth) — then covers the MIR interpreter's
//! collector (Phases 1–2) through `run_mir_with_gc`, and the native collector
//! (Phase 3) through compiled binaries driven by `SIM_GC_EVERY` /
//! `SIM_GC_STATS`. Wasm reclamation is host WasmGC (Phase 4 / 4-R*):
//! objects, texts, and class Text/ObjectRef attributes are typed refs. Baseline
//! cases plus Phase 4e survival checks run under `wasm-node`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use outimage::{GcOptions, GcStats};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-memory-gc-{tag}-{id}"))
}

/// Runs `source` through the MIR interpreter (the semantics oracle).
fn run_interp(source: &str) -> String {
    outimage::compile_str(source)
        .unwrap_or_else(|error| panic!("MIR interpreter failed for {source:?}: {error}"))
}

/// Runs `source` through the MIR interpreter with the collector under test
/// control, returning stdout and the run's cumulative [`GcStats`].
fn run_interp_with_gc(source: &str, options: GcOptions) -> (String, GcStats) {
    outimage::run_mir_with_gc(source, options)
        .unwrap_or_else(|error| panic!("MIR interpreter (GC) failed for {source:?}: {error}"))
}

/// Collect on *every* object/array allocation, plus once more after the
/// program ends. The harshest schedule a correct collector must survive: any
/// root the tracer misses becomes a wrong answer almost immediately.
fn stress_gc() -> GcOptions {
    GcOptions {
        collect_every: Some(1),
        force_collect_at_end: true,
    }
}

/// Asserts stress-mode collection does not change what a program prints.
fn assert_stress_gc_preserves_output(source: &str) -> GcStats {
    let expected = run_interp(source);
    let (collected, stats) = run_interp_with_gc(source, stress_gc());
    assert_eq!(
        collected, expected,
        "collecting on every allocation changed the output of {source:?}"
    );
    assert!(
        stats.collections > 0,
        "expected stress mode to actually collect, got {stats:?}"
    );
    stats
}

/// Compiles `source` to a native executable and runs it with `env` applied,
/// returning stdout and stderr. Used to drive the native collector, whose
/// controls are environment variables (`SIM_GC_EVERY` / `SIM_GC_STATS`,
/// documented as implementation extensions in `docs/RUNTIME.md`).
fn run_native_with_env(source: &str, env: &[(&str, &str)]) -> (String, String) {
    let output_path = temp_path("native");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::Native,
        ),
    )
    .unwrap_or_else(|error| panic!("native compile failed for {source:?}: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected a native artifact")
        }
    };
    let mut command = std::process::Command::new(&artifact);
    for (key, value) in env {
        command.env(key, value);
    }
    let result = command
        .output()
        .unwrap_or_else(|error| panic!("native run failed for {source:?}: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        result.status.success(),
        "native exited {:?} for {source:?} with env {env:?}; stderr={}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    (
        String::from_utf8_lossy(&result.stdout).into_owned(),
        String::from_utf8_lossy(&result.stderr).into_owned(),
    )
}

/// Compiles `source` to a native executable and runs it, returning stdout.
fn run_native(source: &str) -> String {
    run_native_with_env(source, &[]).0
}

/// Asserts native output matches the interpreter and returns it.
fn assert_native_matches_interp(source: &str) -> String {
    let interpreted = run_interp(source);
    let native = run_native(source);
    assert_eq!(
        native, interpreted,
        "native diverged from MIR interpreter for {source:?}"
    );
    interpreted
}

/// Compiles `source` to wasm-node (WASI) and runs it under `tests/fixtures/run_wasi.mjs`
/// via `node`, matching the pattern in `tests/mir_wasm.rs`. Returns `None` (rather than
/// failing) when node, the wasm runner, or WASI support are unavailable in this
/// environment, so this test suite stays green on hosts without a wasm toolchain.
fn run_wasm_node(source: &str) -> Option<String> {
    let output_path = temp_path("wasm").with_extension("wasm");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::WasmNode,
        ),
    ) {
        Ok(outimage::CompileResult::Artifact(path)) => path,
        Ok(outimage::CompileResult::Interpreted(_)) | Ok(outimage::CompileResult::Checked) => {
            return None;
        }
        Err(_) => return None,
    };
    let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("run_wasi.mjs");
    if !runner.exists() {
        let _ = std::fs::remove_file(&artifact);
        return None;
    }
    let output = Command::new("node").arg(&runner).arg(&artifact).output();
    let _ = std::fs::remove_file(&artifact);
    match output {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                || stderr.contains("Cannot find module")
                || stderr.contains("WASI")
                || stderr.contains("WebAssembly.instantiate")
            {
                None
            } else {
                panic!(
                    "node wasm runner failed for {source:?}: status={:?} stderr={stderr}",
                    output.status
                );
            }
        }
        Err(_) => None,
    }
}

/// Like [`run_wasm_node`], but never panics on a non-zero exit / trap: returns
/// `Some((success, stdout, stderr))` so callers can treat a clean failure as
/// an acceptable outcome instead of an unexpected one. Still returns `None`
/// when node/the wasm runner/WASI support are unavailable.
fn run_wasm_node_lenient(source: &str) -> Option<(bool, String, String)> {
    let output_path = temp_path("wasm-lenient").with_extension("wasm");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::WasmNode,
        ),
    ) {
        Ok(outimage::CompileResult::Artifact(path)) => path,
        Ok(outimage::CompileResult::Interpreted(_)) | Ok(outimage::CompileResult::Checked) => {
            return None;
        }
        Err(_) => return None,
    };
    let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("run_wasi.mjs");
    if !runner.exists() {
        let _ = std::fs::remove_file(&artifact);
        return None;
    }
    let output = Command::new("node").arg(&runner).arg(&artifact).output();
    let _ = std::fs::remove_file(&artifact);
    match output {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if !output.status.success()
                && (stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WASI")
                    || stderr.contains("WebAssembly.instantiate"))
            {
                return None;
            }
            Some((
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr,
            ))
        }
        Err(_) => None,
    }
}

// --- 1. Terminated objects stay readable through a live ref -----------------
//
// Root set table: "terminated objects still referenced" is one of the two
// easy-to-miss required survivors. A process that runs to completion is
// *terminated* (SS §7.1), but `w` is still a live, computable `ref`
// expression, so `w.id` must keep reading the object's last field values.

#[test]
fn terminated_object_still_readable() {
    let source = r#"Simulation begin
        process class Worker(id); integer id;
        begin
            OutText("run "); OutInt(id, 0); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker(7);
        activate w;
        hold(1.0);
        if w.terminated then begin
            OutText("terminated "); OutInt(w.id, 0); OutImage;
        end else begin
            OutText("not terminated"); OutImage;
        end;
    end;"#;

    let interpreted = assert_native_matches_interp(source);
    assert_eq!(
        interpreted, "run 7\nterminated 7\n",
        "unexpected output: {interpreted:?}"
    );

    if let Some(wasm) = run_wasm_node(source) {
        assert_eq!(
            wasm, interpreted,
            "wasm diverged from MIR interpreter for terminated_object_still_readable"
        );
    }
}

// --- 2. Many small allocations succeed without an artificial cap -----------
//
// Locks decision 6: `MAX_OBJECTS` and friends
// must never abort a program early. Each loop iteration allocates a new `C`
// and immediately overwrites the only ref to the previous one, so every prior
// object becomes garbage — exactly the bump-allocate-and-never-free workload
// this plan exists to eventually reclaim. Kept modest (a few thousand) so the
// test stays fast on interp + native.

const MANY_ALLOCATIONS_COUNT: u32 = 3000;

fn many_allocations_source(count: u32) -> String {
    format!(
        r#"begin
            class C; begin integer x; end;
            ref(C) r;
            integer i, n;
            n := {count};
            i := 1;
            while i <= n do begin
                r :- new C;
                r.x := i;
                i := i + 1;
            end;
            OutInt(n, 0); OutImage;
        end;"#
    )
}

#[test]
fn many_small_object_allocations_succeed() {
    let source = many_allocations_source(MANY_ALLOCATIONS_COUNT);
    let interpreted = assert_native_matches_interp(&source);
    assert_eq!(
        interpreted,
        format!("{MANY_ALLOCATIONS_COUNT}\n"),
        "expected the full allocation loop to finish, got {interpreted:?}"
    );
    if let Some(wasm) = run_wasm_node(&source) {
        assert_eq!(
            wasm, interpreted,
            "wasm diverged from MIR interpreter for many_small_object_allocations_succeed"
        );
    }
}

// --- 3. Wasm heap grows under allocation pressure ---------------------------
//
// Proves the wasm bump heap's `memory.grow` path (`emit_heap_grow_if_needed`,
// `src/codegen/wasm.rs`) actually copes with moderate allocation pressure
// rather than only ever fitting inside the initial reservation. Uses more
// objects than test 2 so the loop's live data comfortably exceeds the 64 KiB
// bump-space reservation a non-component program starts with, forcing at
// least one real `memory.grow`. Skips (does not fail) when node or the wasm
// runner are unavailable, matching `tests/mir_wasm.rs`.

const WASM_ALLOCATION_PRESSURE_COUNT: u32 = 8000;

#[test]
fn wasm_heap_grows_under_allocation_pressure() {
    let source = many_allocations_source(WASM_ALLOCATION_PRESSURE_COUNT);
    let Some(wasm) = run_wasm_node(&source) else {
        eprintln!(
            "skipping wasm_heap_grows_under_allocation_pressure: node/wasm runner unavailable"
        );
        return;
    };
    let interpreted = run_interp(&source);
    assert_eq!(
        wasm, interpreted,
        "wasm diverged from the MIR interpreter under allocation pressure"
    );
    assert_eq!(
        wasm,
        format!("{WASM_ALLOCATION_PRESSURE_COUNT}\n"),
        "expected the full allocation loop to finish under wasm, got {wasm:?}"
    );
}

// --- 4. Detach/resume reactivation chains keep components alive ------------
//
// Root set table: "Detach / resume reactivation chains". `new Worker`
// executes up to `detach`, hands control back to the block that called
// `new`, and later `resume(w)` reactivates the parked component through its
// reactivation chain (`seq_ops.rs` `park` / `attached_to` / `origin` stacks).
// This already works today; the point of locking it here is that any future
// collector must keep treating that chain as a root, not just live `ref`s.

#[test]
fn detach_resume_chain_survives() {
    let source = r#"begin
        class Worker;
        begin
            OutText("A"); OutImage;
            detach;
            OutText("B"); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker;
        OutText("C"); OutImage;
        resume(w);
    end;"#;

    let interpreted = assert_native_matches_interp(source);
    assert_eq!(
        interpreted, "A\nC\nB\n",
        "unexpected output: {interpreted:?}"
    );

    if let Some(wasm) = run_wasm_node(source) {
        assert_eq!(
            wasm, interpreted,
            "wasm diverged from MIR interpreter for detach_resume_chain_survives"
        );
    }
}

// --- 5. Array extent overflow must be rejected cleanly ---------------------
//
// The MIR interpreter's `alloc_array`
// (`src/mir/interp/mod.rs`) now computes the dense element count with
// checked `i64` arithmetic and rejects extents above `i32::MAX` — the same
// ceiling wasm's `i32` product would wrap on, so the interpreter never
// silently "succeeds" at declaring an array wasm could not represent.
//
// Bounds chosen so the element count (70000 * 70000 ~= 4.9e9) overflows a
// 32-bit multiplication without the test itself trying to actually
// materialize tens of gigabytes if some layer allocates eagerly.
//
// Native uses 64-bit arithmetic, so 4.9e9 elements does not overflow
// `int64_t`/`size_t` there, and a huge-but-untouched `calloc` can succeed via
// OS overcommit — native is not guaranteed to reject *this particular*
// extent (that backend's own overflow hardening is tracked separately in
// extent. Wasm codegen does not yet check for
// the `i32` wrap either. So this test only *requires* the interpreter to
// reject the extent; if native or wasm also reject it (now or once their own
// Phase 0 hardening lands), it additionally requires that rejection to be
// clean (no partial output).
#[test]
fn array_extent_overflow_rejected() {
    let source = r#"begin
        integer array a(1:70000, 1:70000);
        OutText("unreachable");
        OutImage;
    end;"#;

    let result = outimage::compile_str(source);
    assert!(
        result.is_err(),
        "huge array bounds should be rejected cleanly by the MIR interpreter, got {result:?}"
    );
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("array extent overflow"),
        "expected an \"array extent overflow\" error, got: {message}"
    );

    // Native: best-effort. Only require a clean failure (no output) if the
    // compiled binary happens to reject it; do not require rejection itself
    // (see comment above).
    let native_output_path = temp_path("native-overflow");
    if let Ok(outimage::CompileResult::Artifact(artifact)) = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(native_output_path, outimage::CompileTarget::Native),
    ) {
        if let Ok(run) = std::process::Command::new(&artifact).output()
            && !run.status.success()
        {
            assert_eq!(
                String::from_utf8_lossy(&run.stdout),
                "",
                "no output should be printed if native rejects the huge array extent"
            );
        }
        let _ = std::fs::remove_file(&artifact);
    }

    // Wasm: same best-effort story as native, using a non-panicking runner
    // since a trap here is an *acceptable* outcome, not a required one.
    if let Some((success, stdout, _stderr)) = run_wasm_node_lenient(source)
        && !success
    {
        assert_eq!(
            stdout, "",
            "no output should be printed if wasm rejects/traps on the huge array extent"
        );
    }
}

// --- 5b. BASICIO file slots are reclaimed on close --------------------------
//
// Fixed tables must
// autogrow or reclaim rather than silently truncate. Native's BASICIO file
// registry (`g_basicio_files[SIMRT_BASICIO_MAX_FILES = 64]`,
// `runtime/runtime.c:2479`) previously only ever grew via a monotonic
// `g_basicio_file_count`, so a program opening and closing more than 64
// *distinct* files over its lifetime — never more than one at a time — would
// exhaust the table even though every earlier file was already closed.
// Opens+closes more files than that fixed count, one at a time, so this only
// passes once `close` returns its slot for reuse.
//
// The interpreter's file table is a growable map, so it always passes here;
// this test's real value is locking in the native reclaim behavior.
const BASICIO_FILE_SLOT_COUNT: u32 = 70;

fn basicio_file_slots_source(base: &str, count: u32) -> String {
    format!(
        r#"begin
            integer i;
            text base, suffix, name;
            ref (OutFile) outf;
            base :- "{base}-";
            for i := 1 step 1 until {count} do begin
                suffix :- blanks(6);
                suffix.putint(i);
                name :- base & suffix.strip;
                outf :- new OutFile(name);
                if outf.open(blanks(20)) then begin
                    outf.outtext("x");
                    outf.outimage;
                    outf.close;
                end else begin
                    OutText("open-failed "); OutInt(i, 0); OutImage;
                end;
            end;
            OutText("done"); OutImage;
        end;"#,
        base = base,
        count = count,
    )
}

#[test]
fn basicio_file_slots_reclaimed_after_close() {
    let base = temp_path("basicio-slots")
        .to_string_lossy()
        .replace('\\', "\\\\");
    let source = basicio_file_slots_source(&base, BASICIO_FILE_SLOT_COUNT);

    let interpreted = run_interp(&source);
    assert_eq!(
        interpreted, "done\n",
        "interpreter should open/close more distinct files than any fixed slot count without a cap"
    );

    let native = run_native(&source);
    assert_eq!(
        native, "done\n",
        "native should reclaim BASICIO file slots on close instead of exhausting a fixed table"
    );

    for i in 1..=BASICIO_FILE_SLOT_COUNT {
        let path = format!("{}-{i}", base.replace("\\\\", "\\"));
        let _ = std::fs::remove_file(&path);
    }
}

// --- 6. Forced GC reclaims unreachable objects ------------------------------
//
// Allocate N objects while
// dropping every ref, collect, and check heap slot usage does not grow
// unboundedly. `slots_reused` is the direct measure — with a working free list
// the loop's Nth object lands in the slot the (N-1)th vacated, so live slots
// stay flat no matter how large N gets.

const GC_LOOP_COUNT: u32 = 500;

#[test]
fn force_gc_reclaims_unreachable_objects() {
    let source = many_allocations_source(GC_LOOP_COUNT);
    let expected = format!("{GC_LOOP_COUNT}\n");

    let (output, stats) = run_interp_with_gc(&source, stress_gc());
    assert_eq!(output, expected, "stress GC changed the program's output");
    assert!(
        stats.objects_freed >= u64::from(GC_LOOP_COUNT) - 2,
        "every overwritten `C` but the last should be reclaimed, got {stats:?}"
    );
    assert!(
        stats.slots_reused >= stats.objects_freed - 5,
        "reclaimed slots should be reused instead of growing the heap, got {stats:?}"
    );
}

#[test]
fn allocation_threshold_collects_without_being_asked() {
    // No explicit trigger at all: the default allocation-count safepoint has
    // to fire on its own, or long-running Simulations never reclaim anything.
    let source = many_allocations_source(MANY_ALLOCATIONS_COUNT);
    let (output, stats) = run_interp_with_gc(&source, GcOptions::default());
    assert_eq!(output, format!("{MANY_ALLOCATIONS_COUNT}\n"));
    assert!(
        stats.collections > 0 && stats.objects_freed > 0,
        "the default threshold should have collected on its own, got {stats:?}"
    );
}

// --- 7. A SIMSET ring with no external reference is collectible -------------
//
// Root set table: SIMSET `SUC` / `PRED` links keep members alive. Each
// iteration builds a `Head` with two `Node`s linked into it and then drops the
// only ref to the head, so the whole ring becomes unreachable *as a cycle* —
// reference counting could not reclaim it, but tracing can. The surviving
// `Cardinal` sum proves the rings were intact while they were still rooted.

#[test]
fn simset_ring_without_external_ref_is_collectible() {
    let source = r#"begin
        simset begin
            Link class Node(n); integer n; begin end;
            ref(Head) h;
            integer i, total;
            for i := 1 step 1 until 50 do begin
                h :- new Head;
                new Node(i).Into(h);
                new Node(i + 1).Into(h);
                total := total + h.Cardinal;
                h :- none;
            end;
            OutInt(total, 0); OutImage;
        end;
    end;"#;

    let stats = assert_stress_gc_preserves_output(source);
    assert_eq!(run_interp(source), "100\n", "each ring should hold 2 links");
    assert!(
        stats.objects_freed >= 140,
        "50 rings of 3 objects each become unreachable, got {stats:?}"
    );
}

// --- 8. Required survivors keep surviving under collection ------------------
//
// The two easy-to-miss roots (SYSIN/SYSOUT and reactivation chains), re-run
// with the collector at its most aggressive. Test 1 and test 4 already lock the
// *outputs*; these lock that collection does not change them.

#[test]
fn terminated_but_referenced_object_survives_collection() {
    let source = r#"Simulation begin
        process class Worker(id); integer id;
        begin
            OutText("run "); OutInt(id, 0); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker(7);
        activate w;
        hold(1.0);
        if w.terminated then begin
            OutText("terminated "); OutInt(w.id, 0); OutImage;
        end else begin
            OutText("not terminated"); OutImage;
        end;
    end;"#;

    assert_stress_gc_preserves_output(source);
    let (output, _) = run_interp_with_gc(source, stress_gc());
    assert_eq!(output, "run 7\nterminated 7\n");
}

#[test]
fn detach_resume_chain_survives_collection() {
    // Allocates garbage between the detach and the resume so stress mode
    // actually runs collections while the component is parked.
    let source = r#"begin
        class Junk; begin integer x; end;
        class Worker;
        begin
            OutText("A"); OutImage;
            detach;
            OutText("B"); OutImage;
        end;
        ref(Worker) w;
        ref(Junk) j;
        integer i;
        w :- new Worker;
        OutText("C"); OutImage;
        for i := 1 step 1 until 20 do j :- new Junk;
        j :- none;
        resume(w);
    end;"#;

    let stats = assert_stress_gc_preserves_output(source);
    let (output, _) = run_interp_with_gc(source, stress_gc());
    assert_eq!(output, "A\nC\nB\n");
    assert!(
        stats.objects_freed > 0,
        "the discarded `Junk` objects should be reclaimed, got {stats:?}"
    );
}

// --- 9. Texts and arrays outlive their declaring block ----------------------
//
// CBL §9.1: "arrays and text objects cannot in general be deleted together
// with their declaring block".

#[test]
fn text_sub_keeps_its_textobj_alive_after_the_parent_ref_is_dropped() {
    let source = r#"begin
        class Holder; begin text t; end;
        ref(Holder) h;
        text whole, piece;
        integer i;
        h :- new Holder;
        h.t :- Copy("abcdefgh");
        whole :- h.t;
        piece :- whole.Sub(3, 4);
        h :- none;
        whole :- notext;
        for i := 1 step 1 until 40 do begin
            inspect new Holder do t :- Copy("junkjunk");
        end;
        OutText(piece); OutImage;
    end;"#;

    let stats = assert_stress_gc_preserves_output(source);
    let (output, _) = run_interp_with_gc(source, stress_gc());
    assert_eq!(
        output, "cdef\n",
        "the subframe must still see its TEXTOBJ characters"
    );
    assert!(
        stats.texts_freed > 0,
        "the discarded holders' texts should be reclaimed, got {stats:?}"
    );
    if let Some(wasm) = run_wasm_node(source) {
        assert_eq!(
            wasm, output,
            "wasm diverged from MIR interpreter for text_sub_keeps_its_textobj_alive"
        );
    }
}

#[test]
fn an_array_outliving_its_declaring_block_survives() {
    let source = r#"begin
        class Box; begin integer array a(1:3);
            procedure put(i, v); integer i, v; begin a(i) := v; end;
            integer procedure get(i); integer i; begin get := a(i); end;
        end;
        ref(Box) b;
        integer i;
        begin
            b :- new Box;
            b.put(1, 42);
        end;
        for i := 1 step 1 until 40 do begin
            inspect new Box do put(1, i);
        end;
        OutInt(b.get(1), 0); OutImage;
    end;"#;

    let stats = assert_stress_gc_preserves_output(source);
    let (output, _) = run_interp_with_gc(source, stress_gc());
    assert_eq!(output, "42\n", "the surviving Box keeps its array contents");
    assert!(
        stats.arrays_freed > 0,
        "the discarded boxes' arrays should be reclaimed, got {stats:?}"
    );
    if let Some(wasm) = run_wasm_node(source) {
        assert_eq!(
            wasm, output,
            "wasm diverged from MIR interpreter for an_array_outliving_its_declaring_block_survives"
        );
    }
}

// --- 10. Collection has no observable side effects --------------------------
//
// Collecting a file object must never close the underlying file. The loop
// allocates enough garbage between the two
// writes that stress mode collects many times while the file is open; if a
// collection closed it, the second write would be lost or fail.

#[test]
fn collecting_does_not_close_an_open_file() {
    let path = temp_path("gc-open-file");
    let path_literal = path.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"begin
            class Junk; begin integer x; end;
            ref (OutFile) outf;
            ref(Junk) j;
            integer i;
            outf :- new OutFile("{path_literal}");
            if outf.open(blanks(20)) then begin
                outf.outtext("first"); outf.outimage;
                for i := 1 step 1 until 40 do j :- new Junk;
                j :- none;
                outf.outtext("second"); outf.outimage;
                outf.close;
                OutText("wrote"); OutImage;
            end else begin
                OutText("open-failed"); OutImage;
            end;
        end;"#
    );

    let (output, stats) = run_interp_with_gc(&source, stress_gc());
    assert_eq!(output, "wrote\n", "the file work should complete");
    assert!(
        stats.objects_freed > 0,
        "the discarded `Junk` objects should be reclaimed, got {stats:?}"
    );
    // OutFile pads each image out to the width it was opened with.
    let contents = std::fs::read_to_string(&path).expect("the output file should exist");
    let lines: Vec<&str> = contents.lines().map(str::trim_end).collect();
    assert_eq!(
        lines,
        vec!["first", "second"],
        "a collection between the two writes must not have closed the file"
    );
    let _ = std::fs::remove_file(&path);
}

// --- 11. Native mark-sweep (Phase 3, step 1) --------------------------------
//
// Native tracing of frames is *precise*: Cranelift emits a linked list of
// root frames whose slots hold every GC-typed MIR local (`runtime/gc.c`).
// Heap object payloads are still scanned conservatively. These tests assert
// the two properties that actually matter: collecting never changes what a
// program prints, and a program that drops everything it allocates runs in
// bounded memory.
//
// Reclamation is on by default (a collection every 1024 allocations, matching
// the interpreter); the tests below that set `SIM_GC_EVERY=1` are asking
// for the stress schedule, and `SIM_GC_EVERY=0` turns collection off.

/// Collect on every single allocation: the harshest schedule, and the one that
/// turns a missed root into a wrong answer almost immediately.
const GC_STRESS: [(&str, &str); 1] = [("SIM_GC_EVERY", "1")];

fn gc_stat(stderr: &str, key: &str) -> u64 {
    let needle = format!("{key}=");
    let start = stderr
        .find(&needle)
        .unwrap_or_else(|| panic!("missing {needle:?} in SIM_GC_STATS output: {stderr:?}"))
        + needle.len();
    let digits: String = stderr[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("unparsable {key} in: {stderr:?}"))
}

#[test]
fn native_gc_stress_preserves_output() {
    // One program per root category the native tracer has to get right:
    // plain object churn, a SIMSET ring, texts and subframes outliving their
    // holder, an array outliving its declaring block, and SQS process
    // scheduling (whose event notices live in C globals, not on any stack).
    let programs: [&str; 5] = [
        &many_allocations_source(500),
        r#"begin
            simset begin
                Link class Node(n); integer n; begin end;
                ref(Head) h;
                integer i, total;
                for i := 1 step 1 until 50 do begin
                    h :- new Head;
                    new Node(i).Into(h);
                    new Node(i + 1).Into(h);
                    total := total + h.Cardinal;
                    h :- none;
                end;
                OutInt(total, 0); OutImage;
            end;
        end;"#,
        r#"begin
            class Holder; begin text t; end;
            ref(Holder) h;
            text whole, piece;
            integer i;
            h :- new Holder;
            h.t :- Copy("abcdefgh");
            whole :- h.t;
            piece :- whole.Sub(3, 4);
            h :- none;
            whole :- notext;
            for i := 1 step 1 until 40 do begin
                inspect new Holder do t :- Copy("junkjunk");
            end;
            OutText(piece); OutImage;
        end;"#,
        r#"begin
            class Box; begin integer array a(1:3);
                procedure put(i, v); integer i, v; begin a(i) := v; end;
                integer procedure get(i); integer i; begin get := a(i); end;
            end;
            ref(Box) b;
            integer i;
            begin
                b :- new Box;
                b.put(1, 42);
            end;
            for i := 1 step 1 until 40 do begin
                inspect new Box do put(1, i);
            end;
            OutInt(b.get(1), 0); OutImage;
        end;"#,
        r#"Simulation begin
            process class Worker(id); integer id;
            begin
                OutText("run "); OutInt(id, 0); OutImage;
                hold(1.0);
                OutText("again "); OutInt(id, 0); OutImage;
            end;
            ref(Worker) w;
            integer i;
            w :- new Worker(7);
            activate w;
            for i := 1 step 1 until 20 do begin
                inspect new Worker(i) do id := i;
            end;
            hold(5.0);
            if w.terminated then begin
                OutText("terminated "); OutInt(w.id, 0); OutImage;
            end;
        end;"#,
    ];

    for source in programs {
        let expected = run_native(source);
        let (collected, _) = run_native_with_env(source, &GC_STRESS);
        assert_eq!(
            collected, expected,
            "collecting on every allocation changed native output for {source:?}"
        );
    }
}

#[test]
fn native_gc_reclaims_under_allocation_pressure() {
    // The overwrite loop from test 2: every iteration but the last leaves an
    // unreachable `C` behind. Without reclamation the heap grows to 3000
    // blocks; with it, the peak should stay near the live set (one object).
    let source = many_allocations_source(MANY_ALLOCATIONS_COUNT);
    let (stdout, stderr) =
        run_native_with_env(&source, &[("SIM_GC_EVERY", "1"), ("SIM_GC_STATS", "1")]);
    assert_eq!(
        stdout,
        format!("{MANY_ALLOCATIONS_COUNT}\n"),
        "the allocation loop should still finish under the collector"
    );

    let collections = gc_stat(&stderr, "collections");
    let blocks_freed = gc_stat(&stderr, "blocks_freed");
    let peak_blocks = gc_stat(&stderr, "peak_blocks");
    let slots_reused = gc_stat(&stderr, "slots_reused");
    assert!(
        collections > 0,
        "stress mode should have collected: {stderr:?}"
    );
    assert!(
        slots_reused > u64::from(MANY_ALLOCATIONS_COUNT) / 2,
        "every `C` is the same size, so the loop should keep landing in the \
         slot the previous one vacated: {stderr:?}"
    );
    assert!(
        blocks_freed > u64::from(MANY_ALLOCATIONS_COUNT) / 2,
        "most of the overwritten objects should be reclaimed: {stderr:?}"
    );
    assert!(
        peak_blocks < 200,
        "a loop that drops everything it allocates should run in bounded memory, \
         but the heap peaked at {peak_blocks} blocks for {MANY_ALLOCATIONS_COUNT} allocations"
    );
    assert!(
        !stderr.contains("sim:"),
        "the run should not have reported a runtime error: {stderr:?}"
    );
}

#[test]
fn native_gc_stats_never_pollute_stdout() {
    // `SIM_GC_STATS` is an implementation extension, so it must not be
    // able to corrupt a program's SYSOUT — the DosTestBatch harness compares
    // stdout, and a stats line landing there would be a silent wrong answer.
    let source = many_allocations_source(100);
    let (stdout, stderr) =
        run_native_with_env(&source, &[("SIM_GC_EVERY", "1"), ("SIM_GC_STATS", "1")]);
    assert_eq!(stdout, "100\n", "stats leaked into stdout: {stdout:?}");
    assert!(
        stderr.contains("sim gc: collections="),
        "expected one stats line on stderr, got {stderr:?}"
    );
    assert!(
        stderr.contains("pause_ns="),
        "stats line should include pause_ns, got {stderr:?}"
    );
}

#[test]
fn native_gc_default_collects() {
    // Phase 3 step 3: the default threshold is 1024 allocations, so a program
    // that never asks for anything still reclaims. Enough iterations to cross
    // that threshold several times over.
    let source = many_allocations_source(2000);
    let (stdout, stderr) = run_native_with_env(&source, &[("SIM_GC_STATS", "1")]);
    assert_eq!(stdout, "2000\n");
    assert!(
        gc_stat(&stderr, "collections") > 0,
        "the default threshold should collect on its own: {stderr:?}"
    );
    assert!(
        gc_stat(&stderr, "blocks_freed") > 0,
        "the overwritten objects should be reclaimed by default: {stderr:?}"
    );
    assert!(
        gc_stat(&stderr, "slots_reused") > 0,
        "reclaimed blocks should be reused rather than returned to the host: {stderr:?}"
    );
    assert!(
        gc_stat(&stderr, "pause_ns") > 0,
        "a collecting run should record pause time: {stderr:?}"
    );
}

#[test]
fn native_gc_every_zero_disables() {
    // The escape hatch: `SIM_GC_EVERY=0` keeps the managed heap and its
    // accounting but never collects, which is how a run opts out of tracing
    // entirely (and how the default-off behavior can still be reproduced).
    let source = many_allocations_source(2000);
    let (stdout, stderr) =
        run_native_with_env(&source, &[("SIM_GC_EVERY", "0"), ("SIM_GC_STATS", "1")]);
    assert_eq!(stdout, "2000\n");
    assert_eq!(
        gc_stat(&stderr, "collections"),
        0,
        "SIM_GC_EVERY=0 should suppress automatic collection: {stderr:?}"
    );
    assert_eq!(
        gc_stat(&stderr, "blocks_freed"),
        0,
        "nothing should be freed with collection disabled: {stderr:?}"
    );
    assert_eq!(
        gc_stat(&stderr, "pause_ns"),
        0,
        "disabled collection should not accumulate pause time: {stderr:?}"
    );
}

#[test]
fn native_gc_detach_resume_still_works() {
    // Root set table: a detached object with no user-visible `ref` is still
    // resumable through its reactivation chain. On native that object's frames
    // sit on a *parked coroutine stack* (`runtime/coro.c`), which the collector
    // has to scan as well as the running one — this is the case a stack scan
    // that only looks at the current stack gets wrong.
    let source = r#"begin
        class Junk; begin integer x; end;
        class Worker;
        begin
            integer local;
            ref(Junk) mine;
            mine :- new Junk;
            mine.x := 99;
            local := 5;
            OutText("A"); OutImage;
            detach;
            OutText("B"); OutInt(local, 0); OutInt(mine.x, 0); OutImage;
        end;
        ref(Worker) w;
        ref(Junk) j;
        integer i;
        w :- new Worker;
        OutText("C"); OutImage;
        for i := 1 step 1 until 100 do j :- new Junk;
        j :- none;
        resume(w);
    end;"#;

    let expected = run_native(source);
    assert_eq!(
        expected, "A\nC\nB599\n",
        "unexpected baseline output: {expected:?}"
    );
    let (collected, _) = run_native_with_env(source, &GC_STRESS);
    assert_eq!(
        collected, expected,
        "collecting while a component is parked lost its frame state"
    );
    assert_eq!(
        collected,
        run_interp(source),
        "native diverged from the oracle"
    );
}

#[test]
fn native_gc_does_not_close_an_open_file() {
    // A collection between two writes
    // must not close the file the program is still using. The BASICIO file
    // object is a root, and marking it does nothing observable.
    let path = temp_path("native-gc-open-file");
    let path_literal = path.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"begin
            class Junk; begin integer x; end;
            ref (OutFile) outf;
            ref(Junk) j;
            integer i;
            outf :- new OutFile("{path_literal}");
            if outf.open(blanks(20)) then begin
                outf.outtext("first"); outf.outimage;
                for i := 1 step 1 until 100 do j :- new Junk;
                j :- none;
                outf.outtext("second"); outf.outimage;
                outf.close;
                OutText("wrote"); OutImage;
            end else begin
                OutText("open-failed"); OutImage;
            end;
        end;"#
    );

    let (stdout, _) = run_native_with_env(&source, &GC_STRESS);
    assert_eq!(stdout, "wrote\n", "the file work should complete");
    let contents = std::fs::read_to_string(&path).expect("the output file should exist");
    let lines: Vec<&str> = contents.lines().map(str::trim_end).collect();
    assert_eq!(
        lines,
        vec!["first", "second"],
        "a native collection between the two writes must not have closed the file"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn native_gc_reclaims_text_churn() {
    // Phase 3 step 2: text frames/objects are no longer UNSWEPT. Overwriting a
    // text local each iteration must free the previous frame+object under
    // stress collection — this is the dominant allocation pattern on the
    // DosTestBatch corpus.
    let source = r#"begin
        text t;
        integer i;
        for i := 1 step 1 until 500 do begin
            t :- copy("churn-payload");
        end;
        OutText(t); OutImage;
    end;"#;
    let (stdout, stderr) =
        run_native_with_env(source, &[("SIM_GC_EVERY", "1"), ("SIM_GC_STATS", "1")]);
    assert_eq!(stdout, "churn-payload\n");
    let blocks_freed = gc_stat(&stderr, "blocks_freed");
    let peak_blocks = gc_stat(&stderr, "peak_blocks");
    assert!(
        blocks_freed > 200,
        "text churn should reclaim frames/objects, got {stderr:?}"
    );
    assert!(
        peak_blocks < 200,
        "text churn should run in bounded memory, peak_blocks={peak_blocks}: {stderr:?}"
    );
}

/// Phase 4e: a class `text` attribute and a class `ref` attribute are typed
/// WasmGC fields. Host GC must keep both alive across an allocation churn
/// that drops every other object.
#[test]
fn wasm_class_text_and_ref_attrs_survive_allocation_churn() {
    let source = r#"begin
        class Box;
        begin
            ref(Box) next;
            text t;
        end;
        class Junk; begin integer x; end;
        ref(Box) p, q;
        ref(Junk) j;
        integer i;
        p :- new Box;
        q :- new Box;
        p.next :- q;
        p.t :- "keep";
        for i := 1 step 1 until 200 do j :- new Junk;
        j :- none;
        OutText(p.t);
        if p.next == q then OutText("ok");
        OutImage;
    end;"#;

    let interpreted = run_interp(source);
    assert_eq!(interpreted, "keepok\n");
    let Some(wasm) = run_wasm_node(source) else {
        eprintln!(
            "skipping wasm_class_text_and_ref_attrs_survive_allocation_churn: \
             node/wasm runner unavailable"
        );
        return;
    };
    assert_eq!(
        wasm, interpreted,
        "wasm diverged from the MIR interpreter for typed class attrs under churn"
    );
}
