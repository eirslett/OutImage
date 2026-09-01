//! CLI smoke tests for `check`, `explain`, and `--json` diagnostics.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn sim_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sim"))
}

fn write_temp(tag: &str, source: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("sim-cli-{tag}-{id}.sim"));
    std::fs::write(&path, source).unwrap();
    path
}

#[test]
fn check_accepts_valid_source() {
    let path = write_temp("ok", "begin integer x; x := 1; end;");
    let output = Command::new(sim_bin())
        .arg("check")
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_rejects_syntax_error() {
    let path = write_temp("bad", "begin @@@");
    let output = Command::new(sim_bin())
        .arg("check")
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0001")
            || stderr.contains("E-lex")
            || stderr.contains("UNEXPECTED")
            || stderr.contains("lex"),
        "expected lex diagnostic, got: {stderr}"
    );
}

#[test]
fn json_diagnostics_emit_structured_object() {
    let path = write_temp("json", "begin @@@");
    let output = Command::new(sim_bin())
        .args(["--json", "check"])
        .arg(&path)
        .output()
        .expect("run sim --json check");
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("json line")).expect("parse json");
    assert_eq!(value["code"], "E0001");
    assert_eq!(value["title"], "UNEXPECTED CHARACTER");
    assert_eq!(value["phase"], "lex");
    assert!(
        value["message"].as_str().unwrap().contains('@')
            || value["message"].as_str().unwrap().contains("legal"),
        "{value}"
    );
    assert!(value["span"].is_object());
}

#[test]
fn explain_prints_known_codes() {
    for code in [
        "E-lex",
        "E-parse",
        "E-semantic",
        "E-codegen",
        "semantic",
        "E0201",
        "W0001",
        "E0901",
        "type-mismatch",
        "missing end",
    ] {
        let output = Command::new(sim_bin())
            .args(["explain", code])
            .output()
            .expect("run sim explain");
        assert!(
            output.status.success(),
            "explain {code} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let hay = stdout.to_ascii_lowercase();
        assert!(
            hay.contains("e-")
                || hay.contains("w0001")
                || hay.contains("failed")
                || hay.contains("type mismatch")
                || hay.contains(&code.to_ascii_lowercase())
                || hay.contains("e0")
                || hay.contains("e1"),
            "unexpected explain output for {code}: {stdout}"
        );
    }
}

#[test]
fn explain_rejects_unknown_code() {
    let output = Command::new(sim_bin())
        .args(["explain", "E-mystery"])
        .output()
        .expect("run sim explain");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown diagnostic code"));
}

#[test]
fn check_rejects_unknown_procedure() {
    let path = write_temp("mystery", "begin mystery; end;");
    let output = Command::new(sim_bin())
        .arg("check")
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mystery"),
        "expected rejection naming the procedure, got: {stderr}"
    );
}

#[test]
fn check_warns_on_unused_variable() {
    let path = write_temp("unused", "begin integer x; integer y; y := 1; end;");
    let output = Command::new(sim_bin())
        .arg("check")
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "unused is a warning, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0001") && stderr.contains("`x`"),
        "expected unused warning, got: {stderr}"
    );
}

#[test]
fn check_no_unused_suppresses_warning() {
    let path = write_temp("nounused", "begin integer x; integer y; y := 1; end;");
    let output = Command::new(sim_bin())
        .args(["check", "--no-unused"])
        .arg(&path)
        .output()
        .expect("run sim check --no-unused");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("W0001") && !stderr.contains("unused"),
        "expected no unused warning, got: {stderr}"
    );
}

#[test]
fn check_accepts_external_procedure_declaration() {
    // §6.3: an `external` procedure is supplied by a separately compiled
    // module, so declaring and calling one is well formed on its own. A
    // signature stub stands in for the body until the units are combined.
    let path = write_temp(
        "external",
        "begin procedure mystery; external; mystery; end;",
    );
    let output = Command::new(sim_bin())
        .arg("check")
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lsp_subcommand_is_documented() {
    let output = Command::new(sim_bin())
        .args(["lsp", "--help"])
        .output()
        .expect("run sim lsp --help");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_ascii_lowercase().contains("language server")
            || stdout.contains("stdin")
            || stdout.contains("stdout"),
        "unexpected help: {stdout}"
    );
}

#[test]
fn debug_cli_break_and_continue() {
    let path = write_temp(
        "dbg",
        "begin\ninteger x;\nx := 7;\nOutText(\"done\");\nOutImage;\nend\n",
    );
    let output = Command::new(sim_bin())
        .args([
            "debug",
            "--break",
            "3",
            "--command",
            "print x",
            "--command",
            "continue",
            "--trace",
        ])
        .arg(&path)
        .output()
        .expect("run sim debug");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("done"), "stdout={stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stop reason=") || stderr.contains("x ="),
        "stderr={stderr}"
    );
}

#[test]
fn debug_subcommand_is_documented() {
    let output = Command::new(sim_bin())
        .args(["debug", "--help"])
        .output()
        .expect("run sim debug --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--break") && stdout.contains("--command"),
        "unexpected help: {stdout}"
    );
}

#[test]
fn check_accepts_double_dash_comments() {
    let path = write_temp("ddash", "begin integer x; x := 1; -- trailing\nend;");
    let output = Command::new(sim_bin())
        .arg("check")
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn no_double_dash_comments_keeps_consecutive_minuses() {
    let path = write_temp(
        "nominus",
        "begin integer x; x := 1--2; OutInt(x, 2); OutImage; end;",
    );
    let output = Command::new(sim_bin())
        .args(["run", "--no-double-dash-comments"])
        .arg(&path)
        .output()
        .expect("run sim run");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"), "expected 1 - -2 = 3, got {stdout:?}");
}

#[test]
fn compact_flag_prints_one_line() {
    let path = write_temp("compact", "begin integer x; x := true; end;");
    let output = Command::new(sim_bin())
        .args(["--compact", "--color", "never", "check"])
        .arg(&path)
        .output()
        .expect("run sim --compact check");
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0201"), "{stderr}");
    assert!(
        !stderr.contains("╭") && !stderr.contains("---"),
        "compact leaked a box:\n{stderr}"
    );
}

#[test]
fn check_lenient_parse_does_not_hide_type_errors() {
    let path = write_temp("lenient", "begin integer x; x := true;");
    let output = Command::new(sim_bin())
        .args(["--color", "never", "check"])
        .arg(&path)
        .output()
        .expect("run sim check");
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0103") && stderr.contains("E0201"),
        "expected missing end and type mismatch, got: {stderr}"
    );
}
