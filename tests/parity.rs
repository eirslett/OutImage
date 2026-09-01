//! Backend parity smokes: MIR interpreter ≡ native (and wasm when supported).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-parity-{tag}-{id}"))
}

fn run_mir(source: &str) -> String {
    outimage::compile_str(source).unwrap_or_else(|error| panic!("MIR interpreter failed: {error}"))
}

fn run_interpreted(source: &str) -> String {
    run_mir(source)
}

fn run_native(source: &str) -> String {
    let output_path = temp_path("native");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .unwrap_or_else(|error| panic!("native compile failed: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected a native artifact")
        }
    };
    let result = std::process::Command::new(&artifact)
        .output()
        .unwrap_or_else(|error| panic!("native run failed: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        result.status.success(),
        "native exited {:?}; stderr={}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

fn run_wasm_node(source: &str) -> Option<String> {
    let output_path = temp_path("wasm").with_extension("wasm");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::WasmNode),
    ) {
        Ok(outimage::CompileResult::Artifact(path)) => path,
        Ok(outimage::CompileResult::Interpreted(_)) | Ok(outimage::CompileResult::Checked) => {
            return None;
        }
        Err(_) => return None,
    };
    let result = std::process::Command::new("wasmtime")
        .arg(&artifact)
        .output();
    let _ = std::fs::remove_file(&artifact);
    match result {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        _ => None,
    }
}

fn assert_triple(name: &str, source: &str, expect: &str) {
    let interpreted = run_interpreted(source);
    assert!(
        interpreted.contains(expect),
        "{name}/mir: expected {expect:?} in {interpreted:?}"
    );
    let native = run_native(source);
    assert_eq!(
        native, interpreted,
        "{name}: native diverged from MIR\nnative={native:?}\nmir={interpreted:?}"
    );
    if let Some(wasm) = run_wasm_node(source) {
        assert_eq!(
            wasm, interpreted,
            "{name}: wasm diverged from MIR\nwasm={wasm:?}\nmir={interpreted:?}"
        );
    }
}

#[test]
fn triple_run_oracle_smokes() {
    let cases = [
        (
            "arith",
            r#"begin integer n; n := 6 * 7; OutInt(n, 0); OutImage; end;"#,
            "42",
        ),
        ("text", r#"begin OutText("hi"); OutImage; end;"#, "hi"),
        (
            "class",
            r#"begin
                class C; begin integer x; x := 3; end;
                ref(C) r; r :- new C; OutInt(r.x, 0); OutImage;
               end;"#,
            "3",
        ),
        (
            "formal_proc",
            r#"begin
                integer procedure twice(x); integer x; begin twice := 2 * x; end;
                procedure apply(f, n); integer procedure f; integer n;
                begin OutInt(f(n), 0); OutImage; end;
                apply(twice, 5);
               end;"#,
            "10",
        ),
        (
            "env_abs_mod",
            r#"begin
                OutInt(abs(-9), 0); OutImage;
                OutInt(mod(11, 4), 0); OutImage;
                OutInt(sign(-1.0), 0); OutImage;
               end;"#,
            "-1",
        ),
        (
            "sim_delay",
            r#"Simulation begin
                process class Worker; begin OutText("W"); OutImage; end;
                ref(Worker) w; w :- new Worker;
                activate w delay 1.0;
                OutText("M"); OutImage;
                hold(2.0);
                OutText("D"); OutImage;
               end;"#,
            "W",
        ),
    ];
    for (name, source, expect) in cases {
        assert_triple(name, source, expect);
    }
}

const SHARED_FIXTURES: &[(&str, &str)] = &[
    ("hello", include_str!("fixtures/hello_world.sim")),
    (
        "hold_activate",
        include_str!("fixtures/simulation/hold_and_activate.sim"),
    ),
    (
        "activate_delay",
        include_str!("fixtures/simulation/activate_delay.sim"),
    ),
    (
        "wait_queue",
        include_str!("fixtures/simulation/wait_queue.sim"),
    ),
    (
        "detach_call",
        include_str!("fixtures/simulation/detach_call_roundtrip.sim"),
    ),
    (
        "call_resume",
        include_str!("fixtures/simulation/call_resume_roundtrip.sim"),
    ),
];

#[test]
fn differential_fixture_smokes() {
    for (name, source) in SHARED_FIXTURES {
        let interpreted = run_interpreted(source);
        let native = run_native(source);
        assert_eq!(
            native, interpreted,
            "{name}: native/MIR diverge\nnative={native:?}\nmir={interpreted:?}"
        );
    }
}

/// MIR matches native when MAIN ends while other processes remain scheduled.
#[test]
fn mir_matches_native_on_simulation_main_epilogue() {
    let source = r#"Simulation begin
        process class A; begin
            integer i;
            for i := 1 step 1 until 3 do begin OutText("a"); OutImage; hold(1.0); end;
        end;
        process class B; begin
            integer i;
            for i := 1 step 1 until 3 do begin OutText("b"); OutImage; hold(1.0); end;
        end;
        ref(A) a; ref(B) b;
        a :- new A; b :- new B;
        activate a; activate b;
       end;"#;
    let mir = run_mir(source);
    let native = run_native(source);
    assert_eq!(mir, "a\nb\na\nb\na\nb\n");
    assert_eq!(native, mir);
}
