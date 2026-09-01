//! Interpreter + native MIR coverage for whole-file BASICIO-ish MVP builtins
//! (`fileExists` / `fileRead` / `fileWrite`) and `OutInt` sugar.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NATIVE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = NATIVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-fs-native-{tag}-{id}"))
}

fn run_native(source: &str) -> String {
    let output_path = temp_output_path("bin");
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
        .unwrap_or_else(|error| panic!("native binary failed to run: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        result.status.success(),
        "native binary exited {:?}; stderr: {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

#[test]
fn file_write_read_exists_round_trip() {
    let path = common::temp_path("basicio_roundtrip.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            fileWrite("{path_lit}", "hello file");
            OutText(fileRead("{path_lit}"));
            OutImage;
            if fileExists("{path_lit}") then OutText("yes") else OutText("no");
            OutImage;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("filesystem builtins program");
    assert_eq!(output, "hello file\nyes\n");
    assert_eq!(
        std::fs::read_to_string(&path).expect("temp file should exist"),
        "hello file"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_exists_false_for_missing_path() {
    let path = common::temp_path("basicio_missing.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            if fileExists("{path_lit}") then OutText("yes") else OutText("no");
            OutImage;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("fileExists missing path");
    assert_eq!(output, "no\n");
}

#[test]
fn file_read_errors_when_missing() {
    let path = common::temp_path("basicio_read_missing.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            OutText(fileRead("{path_lit}"));
            OutImage;
        end;"#
    );
    let error = outimage::compile_str(&source).expect_err("fileRead missing path");
    assert!(
        error.to_string().contains("path not found"),
        "unexpected error: {error}"
    );
}

#[test]
fn out_int_formats_integer() {
    let output = outimage::compile_str(
        r#"begin
            integer n;
            n := -42;
            OutInt(n, 0);
            OutImage;
            OutInt(0, 0);
            OutImage;
            OutInt(7, 0);
            OutImage;
        end;"#,
    )
    .expect("OutInt interpreter");
    assert_eq!(output, "-42\n0\n7\n");
}

#[test]
fn native_file_write_read_exists_round_trip() {
    let path = common::temp_path("basicio_native_roundtrip.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            fileWrite("{path_lit}", "native file");
            OutText(fileRead("{path_lit}"));
            OutImage;
            if fileExists("{path_lit}") then OutText("yes") else OutText("no");
            OutImage;
        end;"#
    );
    let interpreted = outimage::compile_str(&source).expect("interpreter oracle");
    let native = run_native(&source);
    assert_eq!(native, interpreted);
    assert_eq!(native, "native file\nyes\n");
    assert_eq!(std::fs::read_to_string(&path).expect("file"), "native file");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn native_out_int_matches_interpreter() {
    let source = r#"begin
        integer n;
        n := 40 + 2;
        OutInt(n, 0);
        OutImage;
        OutInt(-7, 0);
        OutImage;
    end;"#;
    let interpreted = outimage::compile_str(source).expect("interpreter");
    let native = run_native(source);
    assert_eq!(native, interpreted);
    assert_eq!(native, "42\n-7\n");
}

#[test]
fn wasm_rejects_file_builtins_clearly() {
    let path = common::temp_path("basicio_wasm.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            if fileExists("{path_lit}") then OutText("yes") else OutText("no");
            OutImage;
        end;"#
    );
    let error = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(&source),
        &outimage::CompileOptions::for_compile(
            temp_output_path("wasm"),
            outimage::CompileTarget::WasmNode,
        ),
    )
    .expect_err("wasm should reject fileExists");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("file") || message.contains("whole-file"),
        "unexpected error: {error}"
    );
}
