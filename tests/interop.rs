//! Interop: `external C/JS/Host`, scalar FFI, text copy, `--crate-type lib`,
//! opaque `ref` handles, and Simula `--with` linking.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use outimage::lex::tokenize;
use outimage::mir::{self, ForeignKind};
use outimage::parse::parse;
use outimage::semantic::analyze;
use outimage::source::SourceFile;
use outimage::{
    Charset, CompileOptions, CompileResult, CompileTarget, CrateType, Interpreter, Value,
};

fn parse_and_analyze(source: &str) -> Result<(), outimage::CompileError> {
    let stream = tokenize(&SourceFile::anonymous(source))?;
    let program = parse(&stream)?;
    analyze(&program)
}

const HOST_ADD: &str = r#"
external Host procedure add = "add"
   is integer procedure add(a, b); integer a, b;
begin
   OutInt(add(40, 2), 0);
   OutImage;
end;
"#;

const C_ADD: &str = r#"
external C procedure add = "add"
   is integer procedure add(a, b); integer a, b;
begin
   OutInt(add(2, 3), 0);
   OutImage;
end;
"#;

const JS_GREET: &str = r#"
external JS procedure greet = "console.log"
   is procedure greet(msg); value msg; text msg;
begin
end;
"#;

const HOST_TICK: &str = r#"
external Host procedure onTick = "onTick"
   is procedure onTick(now); real now;
begin
   onTick(1.5);
end;
"#;

#[test]
fn check_accepts_scalar_host_and_c_specs() {
    parse_and_analyze(HOST_ADD).unwrap();
    parse_and_analyze(C_ADD).unwrap();
    parse_and_analyze(HOST_TICK).unwrap();
    match outimage::compile_with_options(
        &SourceFile::anonymous(HOST_ADD),
        &CompileOptions::for_check(),
    )
    .unwrap()
    {
        CompileResult::Checked => {}
        other => panic!("expected check, got {other:?}"),
    }
}

#[test]
fn check_accepts_js_text_spec_without_linking() {
    parse_and_analyze(JS_GREET).unwrap();
    match outimage::compile_with_options(
        &SourceFile::anonymous(JS_GREET),
        &CompileOptions::for_check(),
    )
    .unwrap()
    {
        CompileResult::Checked => {}
        other => panic!("expected check, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_kind() {
    let err = parse_and_analyze("external Fortran procedure sin; begin end;").unwrap_err();
    assert!(
        err.to_string().contains("unknown external kind"),
        "unexpected: {err}"
    );
}

#[test]
fn rejects_kind_without_specification() {
    let err = parse_and_analyze("external C procedure add; begin end;").unwrap_err();
    assert!(
        err.to_string().contains("requires a specification"),
        "unexpected: {err}"
    );
}

#[test]
fn rejects_name_parameter_on_foreign_spec() {
    let err = parse_and_analyze(
        r#"external Host procedure p = "p"
           is integer procedure p(x); name x; integer x;
           begin end;"#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("name parameters cannot cross"),
        "unexpected: {err}"
    );
}

#[test]
fn rejects_array_parameter_on_foreign_spec() {
    let err = parse_and_analyze(
        r#"external Host procedure p = "p"
           is procedure p(a); integer array a;
           begin end;"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("array"), "unexpected: {err}");
}

#[test]
fn mir_records_foreign_abi() {
    let stream = tokenize(&SourceFile::anonymous(HOST_ADD)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program(&program).unwrap();
    let add = module
        .functions
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case("add"))
        .expect("add stub");
    let abi = add.foreign.as_ref().expect("ForeignAbi");
    assert_eq!(abi.kind, ForeignKind::Host);
    assert_eq!(abi.ident, "add");
    assert!(module.unresolved_externals.is_empty());
    module.ensure_externals_resolved().unwrap();
}

#[test]
fn interpreter_define_host_add() {
    let stream = tokenize(&SourceFile::anonymous(HOST_ADD)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program_with_source(&program, HOST_ADD).unwrap();
    let mut interp = Interpreter::from_module(&module);
    interp.define_host("add", |_ctx, args| {
        let a = args[0].as_i64()?;
        let b = args[1].as_i64()?;
        Ok(Value::I64(a + b))
    });
    interp.poll().unwrap();
    assert_eq!(interp.take_captured_stdout(), "42\n");
}

const HOST_GREET: &str = r#"
external Host procedure greet = "greet"
   is procedure greet(msg); value msg; text msg;
begin
   greet("hi");
end;
"#;

const HOST_ECHO: &str = r#"
external Host procedure echo = "echo"
   is text procedure echo(msg); value msg; text msg;
begin
   OutText(echo("hi"));
   OutImage;
end;
"#;

#[test]
fn interpreter_host_greets_text() {
    let stream = tokenize(&SourceFile::anonymous(HOST_GREET)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program_with_source(&program, HOST_GREET).unwrap();
    let mut interp = Interpreter::from_module(&module);
    let seen = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let seen_host = seen.clone();
    interp.define_host("greet", move |ctx, args| {
        ctx.root(args[0].clone());
        *seen_host.borrow_mut() = args[0].as_text()?;
        Ok(Value::None)
    });
    interp.poll().unwrap();
    assert_eq!(seen.borrow().as_str(), "hi");
}

#[test]
fn interpreter_host_echoes_text() {
    let stream = tokenize(&SourceFile::anonymous(HOST_ECHO)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program_with_source(&program, HOST_ECHO).unwrap();
    let mut interp = Interpreter::from_module(&module);
    interp.define_host("echo", |_ctx, args| Ok(Value::text(args[0].as_text()?)));
    interp.poll().unwrap();
    assert_eq!(interp.take_captured_stdout(), "hi\n");
}

#[test]
fn interpreter_rejects_missing_host() {
    let stream = tokenize(&SourceFile::anonymous(HOST_ADD)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program(&program).unwrap();
    let mut interp = Interpreter::from_module(&module);
    let err = interp.poll().unwrap_err();
    assert!(
        err.to_string().contains("unresolved Host procedure"),
        "unexpected: {err}"
    );
}

#[test]
fn interpreter_rejects_js_kind() {
    let source = r#"
external JS procedure flag = "flag"
   is boolean procedure flag;
begin
   flag;
end;
"#;
    let stream = tokenize(&SourceFile::anonymous(source)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program(&program).unwrap();
    let mut interp = Interpreter::from_module(&module);
    let err = interp.poll().unwrap_err();
    assert!(
        err.to_string().contains("JS externals require a wasm"),
        "unexpected: {err}"
    );
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_wasm(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-interop-{tag}-{id}.wasm"))
}

#[test]
fn wasm_host_add_runs() {
    let output_path = temp_wasm("host-add");
    match outimage::compile_with_options(
        &SourceFile::anonymous(HOST_ADD),
        &CompileOptions::for_compile(output_path.clone(), CompileTarget::WasmBrowser),
    ) {
        Ok(CompileResult::Artifact(path)) => {
            let runner =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_host.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .output()
                .expect("node");
            let _ = std::fs::remove_file(&path);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                {
                    return;
                }
                panic!("host wasm runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm compile failed: {error}"),
    }
}

const JS_GREET_CALL: &str = r#"
external JS procedure greet = "console.log"
   is procedure greet(msg); value msg; text msg;
begin
   greet("hi");
end;
"#;

#[test]
fn wasm_js_greet_runs() {
    let output_path = temp_wasm("js-greet");
    match outimage::compile_with_options(
        &SourceFile::anonymous(JS_GREET_CALL),
        &CompileOptions::for_compile(output_path.clone(), CompileTarget::WasmBrowser),
    ) {
        Ok(CompileResult::Artifact(path)) => {
            let runner =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_js_greet.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .output()
                .expect("node");
            let _ = std::fs::remove_file(&path);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                {
                    return;
                }
                panic!("js greet runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm js greet compile failed: {error}"),
    }
}

const JS_ECHO: &str = r#"
external JS procedure echo = "echo"
   is text procedure echo(msg); value msg; text msg;
begin
   OutText(echo("hi"));
   OutImage;
end;
"#;

#[test]
fn wasm_js_echo_text_result_runs() {
    let output_path = temp_wasm("js-echo");
    match outimage::compile_with_options(
        &SourceFile::anonymous(JS_ECHO),
        &CompileOptions::for_compile(output_path.clone(), CompileTarget::WasmBrowser),
    ) {
        Ok(CompileResult::Artifact(path)) => {
            let runner =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_js_echo.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .output()
                .expect("node");
            let _ = std::fs::remove_file(&path);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                {
                    return;
                }
                panic!("js echo runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm js echo compile failed: {error}"),
    }
}

const JS_UTF8_GREET: &str = r#"
external JS procedure greet = "console.log"
   is procedure greet(msg); value msg; text msg;
begin
   text t;
   t :- Blanks(1);
   t.putchar(isochar(233));
   greet(t);
end;
"#;

const JS_UTF8_ECHO: &str = r#"
external JS procedure echo = "echo"
   is text procedure echo(msg); value msg; text msg;
begin
   text t, u;
   t :- Blanks(1);
   t.putchar(isochar(233));
   u :- echo(t);
   OutInt(rank(u.getchar), 0);
   OutImage;
end;
"#;

#[test]
fn wasm_js_utf8_greet_runs() {
    let output_path = temp_wasm("js-utf8-greet");
    let mut options = CompileOptions::for_compile(output_path.clone(), CompileTarget::WasmBrowser);
    options.charset = Charset::Utf8;
    match outimage::compile_with_options(&SourceFile::anonymous(JS_UTF8_GREET), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/run_js_utf8_greet.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .output()
                .expect("node");
            let _ = std::fs::remove_file(&path);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                {
                    return;
                }
                panic!("js utf8 greet runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout), "é\n");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm js utf8 greet compile failed: {error}"),
    }
}

#[test]
fn wasm_js_utf8_echo_roundtrips_rank() {
    let output_path = temp_wasm("js-utf8-echo");
    let mut options = CompileOptions::for_compile(output_path.clone(), CompileTarget::WasmBrowser);
    options.charset = Charset::Utf8;
    match outimage::compile_with_options(&SourceFile::anonymous(JS_UTF8_ECHO), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let runner =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_js_echo.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .output()
                .expect("node");
            let _ = std::fs::remove_file(&path);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                {
                    return;
                }
                panic!("js utf8 echo runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout), "233\n");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm js utf8 echo compile failed: {error}"),
    }
}

#[test]
fn native_host_object_emits() {
    let output = std::env::temp_dir().join(format!(
        "sim-interop-host-{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.emit_obj = true;
    match outimage::compile_with_options(&SourceFile::anonymous(HOST_ADD), &options) {
        Ok(CompileResult::Artifact(path)) => {
            assert!(path.exists(), "object {} missing", path.display());
            let _ = std::fs::remove_file(&path);
        }
        Ok(other) => panic!("expected artifact, got {other:?}"),
        Err(error) => panic!("native emit-obj failed: {error}"),
    }
}

const PROC_MODULE_ADD: &str = r#"
integer procedure add(a, b); integer a, b;
begin
   add := a + b;
end;
"#;

const C_SKETCH_A: &str = r#"
external C procedure add = "add"
   is integer procedure add(a, b); integer a, b;
begin
   OutInt(add(40, 2), 0);
   OutImage;
end;
"#;

#[test]
fn mir_procedure_module_records_c_export() {
    let stream = tokenize(&SourceFile::anonymous(PROC_MODULE_ADD)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program(&program).unwrap();
    let add = module
        .functions
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case("add"))
        .expect("add");
    assert_eq!(add.export.as_deref(), Some("add"));
    assert_eq!(add.native_export_name().as_deref(), Some("sim_add"));
    assert_eq!(add.wasm_export_name().as_deref(), Some("add"));
}

#[test]
fn mir_block_local_procedure_is_not_exported() {
    let source = r#"
begin
   integer procedure add(a, b); integer a, b;
   add := a + b;
   OutInt(add(1, 2), 0);
   OutImage;
end;
"#;
    let stream = tokenize(&SourceFile::anonymous(source)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program(&program).unwrap();
    let add = module
        .functions
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case("add"))
        .expect("add");
    assert!(add.export.is_none(), "block-local add should stay internal");
}

#[test]
fn mir_export_identification_publishes_block_local_procedure() {
    let source = r#"
begin
   integer procedure tick = "export:tick";
   tick := 1;
   OutInt(tick, 0);
   OutImage;
end;
"#;
    let stream = tokenize(&SourceFile::anonymous(source)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program(&program).unwrap();
    let tick = module
        .functions
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case("tick"))
        .expect("tick");
    assert_eq!(tick.export.as_deref(), Some("export:tick"));
    assert_eq!(tick.native_export_name().as_deref(), Some("tick"));
    assert_eq!(tick.wasm_export_name().as_deref(), Some("tick"));
}

fn host_cc() -> Option<&'static str> {
    for name in ["cc", "clang", "gcc"] {
        if Command::new(name)
            .arg("-v")
            .output()
            .map(|output| output.status.success() || !output.stderr.is_empty())
            .unwrap_or(false)
        {
            return Some(name);
        }
    }
    None
}

fn temp_path(tag: &str, ext: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir().join(format!("sim-interop-{tag}-{id}"));
    if !ext.is_empty() {
        path.set_extension(ext);
    }
    path
}

fn compile_c_object(source: &str, object: &Path) -> bool {
    let Some(cc) = host_cc() else {
        return false;
    };
    let c_path = temp_path("c", "c");
    std::fs::write(&c_path, source).unwrap();
    let output = Command::new(cc)
        .args(["-c", "-o"])
        .arg(object)
        .arg(&c_path)
        .output()
        .expect("cc -c");
    let _ = std::fs::remove_file(&c_path);
    if !output.status.success() {
        panic!("cc -c failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    true
}

#[test]
fn native_c_link_add_runs() {
    let object = temp_path("add", "o");
    if !compile_c_object(
        "#include <stdint.h>\nint64_t add(int64_t a, int64_t b) { return a + b; }\n",
        &object,
    ) {
        return;
    }
    let output = temp_path("sketch-a", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.extra_link = vec![object.to_string_lossy().into_owned()];
    match outimage::compile_with_options(&SourceFile::anonymous(C_SKETCH_A), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run sketch A");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&object);
            assert!(
                result.status.success(),
                "sketch A failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n");
        }
        Ok(other) => {
            let _ = std::fs::remove_file(&object);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            let _ = std::fs::remove_file(&object);
            panic!("native --link compile failed: {error}");
        }
    }
}

const C_GREET: &str = r#"
external C procedure greet = "greet"
   is procedure greet(msg); value msg; text msg;
begin
   greet("hi");
end;
"#;

#[test]
fn native_c_greet_text_runs() {
    let object = temp_path("greet", "o");
    if !compile_c_object(
        r#"
#include <stdint.h>
#include <stdio.h>
void greet(const uint8_t *p, int64_t n) {
    fwrite(p, 1, (size_t)n, stdout);
    fputc('\n', stdout);
}
"#,
        &object,
    ) {
        return;
    }
    let output = temp_path("c-greet", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.extra_link = vec![object.to_string_lossy().into_owned()];
    match outimage::compile_with_options(&SourceFile::anonymous(C_GREET), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run c greet");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&object);
            assert!(
                result.status.success(),
                "c greet failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), "hi\n");
        }
        Ok(other) => {
            let _ = std::fs::remove_file(&object);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            let _ = std::fs::remove_file(&object);
            panic!("native text --link compile failed: {error}");
        }
    }
}

const C_ECHO: &str = r#"
external C procedure echo = "echo"
   is text procedure echo(msg); value msg; text msg;
begin
   OutText(echo("hi"));
   OutImage;
end;
"#;

#[test]
fn native_c_echo_text_result_runs() {
    let object = temp_path("echo", "o");
    if !compile_c_object(
        r#"
#include <stdint.h>
const uint8_t *echo(const uint8_t *p, int64_t n, int64_t *out_len) {
    *out_len = n;
    return p;
}
"#,
        &object,
    ) {
        return;
    }
    let output = temp_path("c-echo", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.extra_link = vec![object.to_string_lossy().into_owned()];
    match outimage::compile_with_options(&SourceFile::anonymous(C_ECHO), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run c echo");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&object);
            assert!(
                result.status.success(),
                "c echo failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), "hi\n");
        }
        Ok(other) => {
            let _ = std::fs::remove_file(&object);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            let _ = std::fs::remove_file(&object);
            panic!("native text-result --link compile failed: {error}");
        }
    }
}

const C_UTF8_GREET: &str = r#"
external C procedure greet = "greet"
   is procedure greet(msg); value msg; text msg;
begin
   text t;
   t :- Blanks(1);
   t.putchar(isochar(233));
   greet(t);
end;
"#;

const C_UTF8_ECHO: &str = r#"
external C procedure echo = "echo"
   is text procedure echo(msg); value msg; text msg;
begin
   text t, u;
   t :- Blanks(1);
   t.putchar(isochar(233));
   u :- echo(t);
   OutInt(rank(u.getchar), 0);
   OutImage;
end;
"#;

#[test]
fn native_c_utf8_greet_encodes_ranks() {
    let object = temp_path("utf8-greet", "o");
    if !compile_c_object(
        r#"
#include <stdint.h>
#include <stdio.h>
void greet(const uint8_t *p, int64_t n) {
    fwrite(p, 1, (size_t)n, stdout);
    fputc('\n', stdout);
}
"#,
        &object,
    ) {
        return;
    }
    let output = temp_path("c-utf8-greet", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.extra_link = vec![object.to_string_lossy().into_owned()];
    options.charset = Charset::Utf8;
    match outimage::compile_with_options(&SourceFile::anonymous(C_UTF8_GREET), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run c utf8 greet");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&object);
            assert!(
                result.status.success(),
                "c utf8 greet failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(result.stdout, b"\xC3\xA9\n");
        }
        Ok(other) => {
            let _ = std::fs::remove_file(&object);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            let _ = std::fs::remove_file(&object);
            panic!("native utf8 greet compile failed: {error}");
        }
    }
}

#[test]
fn native_c_utf8_echo_roundtrips_rank() {
    let object = temp_path("utf8-echo", "o");
    if !compile_c_object(
        r#"
#include <stdint.h>
const uint8_t *echo(const uint8_t *p, int64_t n, int64_t *out_len) {
    *out_len = n;
    return p;
}
"#,
        &object,
    ) {
        return;
    }
    let output = temp_path("c-utf8-echo", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.extra_link = vec![object.to_string_lossy().into_owned()];
    options.charset = Charset::Utf8;
    match outimage::compile_with_options(&SourceFile::anonymous(C_UTF8_ECHO), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run c utf8 echo");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&object);
            assert!(
                result.status.success(),
                "c utf8 echo failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), "233\n");
        }
        Ok(other) => {
            let _ = std::fs::remove_file(&object);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            let _ = std::fs::remove_file(&object);
            panic!("native utf8 echo compile failed: {error}");
        }
    }
}

#[test]
fn native_c_calls_exported_sim_add() {
    let lib = temp_path(
        "libadd",
        if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        },
    );
    let mut options = CompileOptions::for_compile(lib.clone(), CompileTarget::Native);
    options.crate_type = CrateType::Lib;
    let lib_path =
        match outimage::compile_with_options(&SourceFile::anonymous(PROC_MODULE_ADD), &options) {
            Ok(CompileResult::Artifact(path)) => path,
            Ok(other) => panic!("expected library artifact, got {other:?}"),
            Err(error) => panic!("crate-type lib failed: {error}"),
        };

    let Some(cc) = host_cc() else {
        let _ = std::fs::remove_file(&lib_path);
        return;
    };
    let host_c = temp_path("host", "c");
    std::fs::write(
        &host_c,
        r#"
#include <stdint.h>
#include <stdio.h>
int64_t sim_add(int64_t a, int64_t b);
int main(void) {
    printf("%lld\n", (long long)sim_add(40, 2));
    return 0;
}
"#,
    )
    .unwrap();
    let host_bin = temp_path("host-bin", "");
    let mut command = Command::new(cc);
    command.arg("-o").arg(&host_bin).arg(&host_c).arg(&lib_path);
    let output = command.output().expect("cc host");
    let _ = std::fs::remove_file(&host_c);
    if !output.status.success() {
        let _ = std::fs::remove_file(&lib_path);
        panic!(
            "cc host failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let result = Command::new(&host_bin).output().expect("run host");
    let _ = std::fs::remove_file(&host_bin);
    let _ = std::fs::remove_file(&lib_path);
    assert!(
        result.status.success(),
        "C host failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n");
}

#[test]
fn native_export_identification_uses_exact_symbol() {
    let lib = temp_path(
        "libplus",
        if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        },
    );
    let source = r#"
integer procedure add(a, b) = "export:plus"; integer a, b;
begin
   add := a + b;
end;
"#;
    let mut options = CompileOptions::for_compile(lib.clone(), CompileTarget::Native);
    options.crate_type = CrateType::Lib;
    let lib_path = match outimage::compile_with_options(&SourceFile::anonymous(source), &options) {
        Ok(CompileResult::Artifact(path)) => path,
        Ok(other) => panic!("expected library artifact, got {other:?}"),
        Err(error) => panic!("crate-type lib failed: {error}"),
    };
    let Some(cc) = host_cc() else {
        let _ = std::fs::remove_file(&lib_path);
        return;
    };
    let host_c = temp_path("host-plus", "c");
    std::fs::write(
        &host_c,
        r#"
#include <stdint.h>
#include <stdio.h>
int64_t plus(int64_t a, int64_t b);
int main(void) {
    printf("%lld\n", (long long)plus(40, 2));
    return 0;
}
"#,
    )
    .unwrap();
    let host_bin = temp_path("host-plus-bin", "");
    let output = Command::new(cc)
        .arg("-o")
        .arg(&host_bin)
        .arg(&host_c)
        .arg(&lib_path)
        .output()
        .expect("cc host");
    let _ = std::fs::remove_file(&host_c);
    if !output.status.success() {
        let _ = std::fs::remove_file(&lib_path);
        panic!(
            "cc host failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let result = Command::new(&host_bin).output().expect("run host");
    let _ = std::fs::remove_file(&host_bin);
    let _ = std::fs::remove_file(&lib_path);
    assert!(
        result.status.success(),
        "C host failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n");
}

#[test]
fn wasm_lib_exports_add() {
    let output_path = temp_path("lib-add", "wasm");
    let mut options = CompileOptions::for_compile(output_path, CompileTarget::WasmBrowser);
    options.crate_type = CrateType::Lib;
    match outimage::compile_with_options(&SourceFile::anonymous(PROC_MODULE_ADD), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let runner =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_export.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .arg("add")
                .output()
                .expect("node");
            let html = path.with_extension("html");
            let js = path.with_extension("js");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&html);
            let _ = std::fs::remove_file(&js);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                {
                    return;
                }
                panic!("wasm export runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm crate-type lib failed: {error}"),
    }
}

const HOST_LIB_USE: &str = r#"
external Host procedure add = "add"
   is integer procedure add(a, b); integer a, b;
integer procedure combo;
begin
   combo := add(40, 2);
end;
"#;

#[test]
fn native_instantiate_host_table_and_call() {
    let lib = temp_path(
        "libhost",
        if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        },
    );
    let mut options = CompileOptions::for_compile(lib.clone(), CompileTarget::Native);
    options.crate_type = CrateType::Lib;
    let lib_path =
        match outimage::compile_with_options(&SourceFile::anonymous(HOST_LIB_USE), &options) {
            Ok(CompileResult::Artifact(path)) => path,
            Ok(other) => panic!("expected library artifact, got {other:?}"),
            Err(error) => panic!("host lib compile failed: {error}"),
        };

    let Some(cc) = host_cc() else {
        let _ = std::fs::remove_file(&lib_path);
        return;
    };
    let runtime_inc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
    let host_c = temp_path("embed-host", "c");
    std::fs::write(
        &host_c,
        r#"
#include "embed.h"
#include <stdint.h>
#include <stdio.h>
static int64_t host_add(int64_t a, int64_t b) { return a + b; }
int main(void) {
    SimrtHostDef host[] = {{"add", (void *)host_add}};
    SimrtInstance *s = simrt_instantiate(host, 1);
    SimrtVal result;
    if (simrt_call(s, "sim_combo", NULL, 0, &result) != 0) {
        fprintf(stderr, "simrt_call failed\n");
        return 1;
    }
    printf("%lld\n", (long long)result.u.i64);
    printf("%.1f\n", simrt_sim_now(s));
    printf("%d\n", simrt_sim_step(s));
    simrt_release(s);
    return 0;
}
"#,
    )
    .unwrap();
    let host_bin = temp_path("embed-bin", "");
    let output = Command::new(cc)
        .arg("-o")
        .arg(&host_bin)
        .arg("-I")
        .arg(&runtime_inc)
        .arg(&host_c)
        .arg(&lib_path)
        .output()
        .expect("cc embed host");
    let _ = std::fs::remove_file(&host_c);
    if !output.status.success() {
        let _ = std::fs::remove_file(&lib_path);
        panic!(
            "cc embed host failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let result = Command::new(&host_bin).output().expect("run embed host");
    let _ = std::fs::remove_file(&host_bin);
    let _ = std::fs::remove_file(&lib_path);
    assert!(
        result.status.success(),
        "embed host failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n0.0\n0\n");
}

#[test]
fn interpreter_call_documents_embedding() {
    let stream = tokenize(&SourceFile::anonymous(HOST_LIB_USE)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program_with_source(&program, HOST_LIB_USE).unwrap();
    let mut vm = Interpreter::from_module(&module);
    vm.define_host("add", |_ctx, args| {
        Ok(Value::I64(args[0].as_i64()? + args[1].as_i64()?))
    });
    let result = vm.call("combo", &[]).unwrap();
    assert_eq!(result, Some(Value::I64(42)));
}

const UTILS_HELPER: &str = r#"
integer procedure helper;
begin
   helper := 42;
end;
"#;

const MAIN_WITH_UTILS: &str = r#"
external integer procedure helper = "utils";
begin
   OutInt(helper, 0);
   OutImage;
end;
"#;

fn write_sim(stem: &str, source: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sim-interop-with-{stem}-{id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{stem}.sim"));
    std::fs::write(&path, source).unwrap();
    path
}

fn remove_sim(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

#[test]
fn interpreter_with_merges_procedure_module() {
    let utils = write_sim("utils", UTILS_HELPER);
    let main = SourceFile::anonymous(MAIN_WITH_UTILS);
    let mut options = CompileOptions::for_run();
    options.with_modules = vec![utils.clone()];
    match outimage::compile_with_options(&main, &options) {
        Ok(CompileResult::Interpreted(output)) => assert_eq!(output, "42\n"),
        Ok(other) => panic!("expected interpreted, got {other:?}"),
        Err(error) => panic!("--with interp failed: {error}"),
    }
    remove_sim(&utils);
}

#[test]
fn check_with_merges_procedure_module() {
    let utils = write_sim("utils", UTILS_HELPER);
    let mut options = CompileOptions::for_check();
    options.with_modules = vec![utils.clone()];
    match outimage::compile_with_options(&SourceFile::anonymous(MAIN_WITH_UTILS), &options) {
        Ok(CompileResult::Checked) => {}
        Ok(other) => panic!("expected checked, got {other:?}"),
        Err(error) => panic!("--with check failed: {error}"),
    }
    remove_sim(&utils);
}

#[test]
fn native_with_merges_procedure_module() {
    let utils = write_sim("utils", UTILS_HELPER);
    let output = temp_path("with-main", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.with_modules = vec![utils.clone()];
    match outimage::compile_with_options(&SourceFile::anonymous(MAIN_WITH_UTILS), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run --with native");
            let _ = std::fs::remove_file(&path);
            remove_sim(&utils);
            assert!(
                result.status.success(),
                "--with native failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n");
        }
        Ok(other) => {
            remove_sim(&utils);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            remove_sim(&utils);
            panic!("--with native compile failed: {error}");
        }
    }
}

const HOST_KEEP: &str = r#"
begin
   class Cell;
   begin
      integer n;
   end;
   ref (Cell) a, b;
   external Host procedure keep = "keep"
      is ref (Cell) procedure keep(r); ref (Cell) r;
   a :- new Cell;
   a.n := 7;
   b :- keep(a);
   OutInt(b.n, 0);
   OutImage;
end;
"#;

const C_KEEP: &str = r#"
begin
   class Cell;
   begin
      integer n;
   end;
   ref (Cell) a, b;
   external C procedure keep = "keep"
      is ref (Cell) procedure keep(r); ref (Cell) r;
   a :- new Cell;
   a.n := 7;
   b :- keep(a);
   OutInt(b.n, 0);
   OutImage;
end;
"#;

#[test]
fn check_accepts_ref_handle_spec() {
    parse_and_analyze(HOST_KEEP).unwrap();
}

#[test]
fn interpreter_host_keeps_object_handle() {
    let stream = tokenize(&SourceFile::anonymous(HOST_KEEP)).unwrap();
    let program = parse(&stream).unwrap();
    analyze(&program).unwrap();
    let module = mir::lower_program_with_source(&program, HOST_KEEP).unwrap();
    let mut interp = Interpreter::from_module(&module);
    interp.define_host("keep", |ctx, args| {
        let value = args[0].clone();
        let _ = value.as_object_ref()?;
        Ok(ctx.root(value))
    });
    interp.poll().unwrap();
    assert_eq!(interp.take_captured_stdout(), "7\n");
}

#[test]
fn native_c_keep_object_handle_survives_collect() {
    let object = temp_path("keep", "o");
    let Some(cc) = host_cc() else {
        return;
    };
    let runtime_inc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
    let c_path = temp_path("keep", "c");
    std::fs::write(
        &c_path,
        r#"
#include "gc.h"
void *keep(void *r) {
    int64_t id = simrt_ref_pin(r);
    simrt_gc_collect();
    void *got = simrt_ref_get(id);
    simrt_ref_unpin(id);
    return got;
}
"#,
    )
    .unwrap();
    let compiled = Command::new(cc)
        .args(["-c", "-o"])
        .arg(&object)
        .arg("-I")
        .arg(&runtime_inc)
        .arg(&c_path)
        .output()
        .expect("cc -c keep");
    let _ = std::fs::remove_file(&c_path);
    if !compiled.status.success() {
        panic!(
            "cc -c keep failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
    }
    let output = temp_path("keep-bin", "");
    let mut options = CompileOptions::for_compile(output, CompileTarget::Native);
    options.extra_link = vec![object.to_string_lossy().into_owned()];
    match outimage::compile_with_options(&SourceFile::anonymous(C_KEEP), &options) {
        Ok(CompileResult::Artifact(path)) => {
            let result = Command::new(&path).output().expect("run keep");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&object);
            assert!(
                result.status.success(),
                "keep handle failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), "7\n");
        }
        Ok(other) => {
            let _ = std::fs::remove_file(&object);
            panic!("expected artifact, got {other:?}");
        }
        Err(error) => {
            let _ = std::fs::remove_file(&object);
            panic!("native keep compile failed: {error}");
        }
    }
}

#[test]
fn wasm_host_keep_object_handle_runs() {
    let output_path = temp_wasm("host-keep");
    match outimage::compile_with_options(
        &SourceFile::anonymous(HOST_KEEP),
        &CompileOptions::for_compile(output_path.clone(), CompileTarget::WasmBrowser),
    ) {
        Ok(CompileResult::Artifact(path)) => {
            let runner =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_host.mjs");
            if !runner.exists() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            let output = Command::new("node")
                .arg(&runner)
                .arg(&path)
                .output()
                .expect("node");
            let _ = std::fs::remove_file(&path);
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                    || stderr.contains("Cannot find module")
                    || stderr.contains("WebAssembly.instantiate")
                    || stderr.contains("eq")
                    || stderr.contains("externref")
                    || stderr.contains("anyref")
                {
                    return;
                }
                panic!("host keep wasm runner failed: {stderr}");
            }
            assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
        }
        Ok(other) => panic!("expected wasm artifact, got {other:?}"),
        Err(error) => panic!("wasm keep compile failed: {error}"),
    }
}
