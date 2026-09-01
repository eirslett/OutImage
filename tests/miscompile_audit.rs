//! Interp ≡ native differential audit for a curated fixture allowlist
//! (silent-miscompile guard).

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-audit-{tag}-{id}"))
}

fn run_interpreted(source: &str) -> String {
    outimage::compile_str(source).unwrap_or_else(|error| panic!("interpreter failed: {error}"))
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
        _ => panic!("expected native artifact"),
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

fn assert_no_silent_miscompile(path: &str) {
    let source = common::fixture(path);
    let interpreted = run_interpreted(&source);
    let native = run_native(&source);
    assert_eq!(
        native, interpreted,
        "silent miscompile for fixture {path}\n--- native ---\n{native}\n--- interp ---\n{interpreted}"
    );
}

#[test]
fn environment_and_control_fixtures_match_across_backends() {
    for path in [
        "environment/basic_ops.sim",
        "environment/random.sim",
        "control_flow/goto_labelled_if_branch.sim",
        "control_flow/if_positive.sim",
        "expressions/arithmetic_precedence.sim",
        "expressions/boolean_operators.sim",
        "hello_world.sim",
        "basicio/outchar_break.sim",
    ] {
        assert_no_silent_miscompile(path);
    }
}

#[test]
fn driver_check_backend_lowers_random_fixture() {
    let source = common::fixture("environment/random.sim");
    let result = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(&source),
        &outimage::CompileOptions::for_check(),
    )
    .expect("check should succeed");
    assert!(matches!(result, outimage::CompileResult::Checked));
}
