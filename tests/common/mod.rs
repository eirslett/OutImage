#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn fixture(name: &str) -> String {
    let path = repo_root().join("tests/fixtures").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read fixture {}: {error}", path.display());
    })
}

pub fn temp_path(name: &str) -> String {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!("sim-{id}-{name}"))
        .to_string_lossy()
        .into_owned()
}

/// Native C runtime sources linked into a full AOT-shaped fixture.
#[allow(dead_code)]
pub const NATIVE_RUNTIME_SOURCES: &[&str] = &[
    "gc.c",
    "safety.c",
    "runtime.c",
    "env.c",
    "array.c",
    "text.c",
    "object.c",
    "io.c",
    "sim.c",
    "coro.c",
    "sequencing.c",
];

fn host_c_compiler() -> String {
    if let Ok(cc) = std::env::var("CC") {
        if !cc.is_empty() {
            return cc;
        }
    }
    for name in ["cc", "clang", "gcc"] {
        if Command::new(name)
            .arg("-v")
            .output()
            .map(|output| output.status.success() || !output.stderr.is_empty())
            .unwrap_or(false)
        {
            return name.to_string();
        }
    }
    "cc".to_string()
}

/// Compiles a C fixture against the runtime sources and returns its stdout.
///
/// On Unix this always instruments with ASan+UBSan so a buffer overrun in the
/// C runtime fails the test instead of silently corrupting.
/// MSVC is left unsanitized. Leak detection is off: sequencing
/// components and SYSIN/SYSOUT are immortal by contract.
#[allow(dead_code)]
pub fn run_c_fixture(fixture: &str, runtime_sources: &[&str]) -> String {
    run_c_fixture_ex(fixture, runtime_sources, &[])
}

pub fn run_c_fixture_ex(
    fixture: &str,
    runtime_sources: &[&str],
    extra_fixtures: &[&str],
) -> String {
    let root = repo_root();
    let stem = Path::new(fixture)
        .file_stem()
        .expect("fixture name")
        .to_string_lossy()
        .into_owned();
    let binary = if cfg!(windows) {
        std::env::temp_dir().join(format!("sim-{stem}.exe"))
    } else {
        std::env::temp_dir().join(format!("sim-{stem}"))
    };

    let mut compile = Command::new(host_c_compiler());
    compile.arg("-O1").arg("-Wall").arg("-g");
    if !cfg!(windows) {
        compile.arg("-fsanitize=address,undefined");
        compile.arg("-fno-omit-frame-pointer");
    }
    compile.arg(root.join("tests/fixtures/runtime").join(fixture));
    for source in runtime_sources {
        compile.arg(root.join("runtime").join(source));
    }
    for extra in extra_fixtures {
        compile.arg(root.join("tests/fixtures/runtime").join(extra));
    }
    // libm is a separate archive on Unix; MSVC/clang-cl put math in the CRT.
    if !cfg!(windows)
        && (fixture.contains("gc")
            || runtime_sources.iter().any(|s| {
                matches!(
                    *s,
                    "runtime.c"
                        | "gc.c"
                        | "env.c"
                        | "array.c"
                        | "text.c"
                        | "object.c"
                        | "io.c"
                        | "sim.c"
                )
            }))
    {
        compile.arg("-lm");
    }
    compile.arg("-o").arg(&binary);

    let compiled = compile
        .output()
        .expect("a C compiler is needed to test the runtime");
    assert!(
        compiled.status.success(),
        "compiling {fixture} failed:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let mut run = Command::new(&binary);
    if !cfg!(windows) {
        run.env("ASAN_OPTIONS", "detect_leaks=0:abort_on_error=1");
        run.env("UBSAN_OPTIONS", "halt_on_error=1:print_stacktrace=1");
    }
    run.env("SIM_GC_POISON", "1");
    let run = run.output().expect("fixture ran");
    let _ = std::fs::remove_file(&binary);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "{fixture} exited with {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    stdout
}
