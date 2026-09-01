//! Curated GC stress for CI.
//!
//! Cheaper than DosTestBatch and already run by the `cargo test` CI step:
//! allocation churn, SIMSET ring churn, and a short Simulation loop, on
//! interpreter + native + wasm. Host WasmGC has no pause-time metric; the
//! gate there is "finishes without OOM or trap".

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-gc-stress-{tag}-{id}"))
}

fn run_interp(source: &str) -> String {
    outimage::compile_str(source).unwrap_or_else(|error| panic!("interp failed: {error}"))
}

fn run_native_stress(source: &str) -> String {
    let output_path = temp_path("native");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::Native,
        ),
    )
    .unwrap_or_else(|error| panic!("native compile failed: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        _ => panic!("expected a native artifact"),
    };
    let result = Command::new(&artifact)
        .env("SIM_GC_EVERY", "1")
        .env("SIM_GC_STATS", "1")
        .output()
        .unwrap_or_else(|error| panic!("native run failed: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        result.status.success(),
        "native stress failed: status={:?} stderr={}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("pause_ns="),
        "native stress should report pause_ns, got {stderr:?}"
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

fn run_wasm(source: &str) -> Option<String> {
    let output_path = temp_path("wasm").with_extension("wasm");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::WasmNode,
        ),
    ) {
        Ok(outimage::CompileResult::Artifact(path)) => path,
        _ => return None,
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
        _ => None,
    }
}

fn assert_all_backends(source: &str, expected: &str) {
    let interp = run_interp(source);
    assert_eq!(interp, expected, "interpreter: {interp:?}");
    let native = run_native_stress(source);
    assert_eq!(native, expected, "native: {native:?}");
    if let Some(wasm) = run_wasm(source) {
        assert_eq!(wasm, expected, "wasm: {wasm:?}");
    }
}

#[test]
fn object_churn_stays_correct_under_collection() {
    let source = r#"begin
        class C; begin integer x; end;
        ref(C) r;
        integer i;
        for i := 1 step 1 until 400 do begin
            r :- new C;
            r.x := i;
        end;
        OutInt(r.x, 0); OutImage;
    end;"#;
    assert_all_backends(source, "400\n");
}

#[test]
fn simset_ring_churn_stays_correct_under_collection() {
    let source = r#"begin
        simset begin
            Link class Node(n); integer n; begin end;
            ref(Head) h;
            integer i, total;
            for i := 1 step 1 until 40 do begin
                h :- new Head;
                new Node(i).Into(h);
                new Node(i + 1).Into(h);
                total := total + h.Cardinal;
                h :- none;
            end;
            OutInt(total, 0); OutImage;
        end;
    end;"#;
    assert_all_backends(source, "80\n");
}

#[test]
fn short_simulation_loop_stays_correct_under_collection() {
    let source = r#"Simulation begin
        process class Worker;
        begin
            hold(0.1);
        end;
        integer i;
        for i := 1 step 1 until 20 do activate new Worker;
        hold(5.0);
        OutText("done"); OutImage;
    end;"#;
    assert_all_backends(source, "done\n");
}
