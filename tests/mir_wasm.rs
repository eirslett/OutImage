//! Phase 6: MIR → wasm-node (WASI) and wasm-browser (`env.write`).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_wasm_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-mir-wasm-{tag}-{id}.wasm"))
}

fn compile_wasm(source: &str, target: outimage::CompileTarget) -> PathBuf {
    let tag = match target {
        outimage::CompileTarget::WasmNode => "node",
        outimage::CompileTarget::WasmBrowser => "browser",
        _ => "other",
    };
    let output_path = temp_wasm_path(tag);
    match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path.clone(), target),
    )
    .unwrap_or_else(|error| panic!("{target} compile failed for {source:?}: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected a wasm artifact")
        }
    }
}

fn runner_for(target: outimage::CompileTarget) -> PathBuf {
    let name = match target {
        outimage::CompileTarget::WasmNode => "run_wasi.mjs",
        outimage::CompileTarget::WasmBrowser => "run_browser.mjs",
        _ => panic!("not a wasm target: {target}"),
    };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run_wasm(path: &std::path::Path, target: outimage::CompileTarget) -> Option<String> {
    let runner = runner_for(target);
    if !runner.exists() {
        return None;
    }
    let output = Command::new("node").arg(&runner).arg(path).output().ok()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
            || stderr.contains("Cannot find module")
            || stderr.contains("WASI")
            || stderr.contains("WebAssembly.instantiate")
        {
            return None;
        }
        panic!(
            "node runner failed for {} ({target}): status={:?} stderr={stderr}",
            path.display(),
            output.status
        );
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn assert_wasm_target_matches(source: &str, target: outimage::CompileTarget) {
    let path = compile_wasm(source, target);
    let bytes = std::fs::read(&path).expect("wasm artifact should exist");
    assert!(bytes.starts_with(b"\0asm"), "expected wasm magic");

    let Some(stdout) = run_wasm(&path, target) else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    let interpreted = outimage::compile_str(source)
        .unwrap_or_else(|error| panic!("interpreter failed for {source:?}: {error}"));
    assert_eq!(
        stdout, interpreted,
        "{target} and interpreted output diverged for {source:?}"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("mjs"));
    let _ = std::fs::remove_file(path.with_extension("js"));
    let _ = std::fs::remove_file(path.with_extension("html"));
}

fn assert_wasm_matches_interpreter(source: &str) {
    assert_wasm_target_matches(source, outimage::CompileTarget::WasmNode);
    assert_wasm_target_matches(source, outimage::CompileTarget::WasmBrowser);
}

#[test]
fn wasm_node_generated_mjs_runs_hello_world() {
    let source = r#"begin OutText("hello world"); OutImage; end;"#;
    let path = compile_wasm(source, outimage::CompileTarget::WasmNode);
    let runner = path.with_extension("mjs");
    let output = Command::new("node")
        .arg(&runner)
        .output()
        .expect("spawn node for generated wasm-node runner");
    assert!(
        output.status.success(),
        "generated runner failed: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        outimage::compile_str(source).expect("interpreter")
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&runner);
}

#[test]
fn mir_wasm_out_int_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer n;
            n := 40 + 2;
            OutInt(n, 0);
            OutImage;
            OutInt(-3, 0);
            OutImage;
            OutInt(0, 0);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_environment_abs_mod_sign_sqrt() {
    assert_wasm_matches_interpreter(
        r#"begin
            OutInt(abs(-9), 0); OutImage;
            OutInt(mod(11, 4), 0); OutImage;
            OutInt(sign(-1.0), 0); OutImage;
            if sqrt(9.0) = 3.0 then begin OutText("ok"); OutImage; end;
           end;"#,
    );
}

#[test]
fn mir_wasm_mod_rem_negatives_match_interpreter() {
    assert_wasm_matches_interpreter(
        r#"begin
            OutInt(mod(-7, 3), 0); OutImage;
            OutInt(rem(-7, 3), 0); OutImage;
            OutInt(mod(7, -3), 0); OutImage;
            OutInt(rem(7, -3), 0); OutImage;
           end;"#,
    );
}

#[test]
fn mir_wasm_inline_reads_stdin_line() {
    let source = r#"begin
        OutText(InLine);
        OutImage;
    end;"#;
    for target in [
        outimage::CompileTarget::WasmNode,
        outimage::CompileTarget::WasmBrowser,
    ] {
        let path = compile_wasm(source, target);
        let runner = runner_for(target);
        if !runner.exists() {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let mut child = Command::new("node")
            .arg(&runner)
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn node for {target}: {error}"));
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().expect("stdin");
            stdin
                .write_all(b"hello\n")
                .unwrap_or_else(|error| panic!("write stdin: {error}"));
        }
        let output = child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait node: {error}"));
        let _ = std::fs::remove_file(&path);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("ERR_UNKNOWN_BUILTIN_MODULE")
                || stderr.contains("Cannot find module")
                || stderr.contains("WASI")
                || stderr.contains("WebAssembly.instantiate")
            {
                continue;
            }
            panic!(
                "InLine runner failed for {target}: status={:?} stderr={stderr}",
                output.status
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout, "hello\n", "{target} InLine stdout was {stdout:?}");
    }
}

fn assert_wasm_target_aborts(source: &str, target: outimage::CompileTarget) {
    let path = compile_wasm(source, target);
    let runner = runner_for(target);
    if !runner.exists() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let output = Command::new("node").arg(&runner).arg(&path).output().ok();
    let _ = std::fs::remove_file(&path);
    let Some(output) = output else {
        return;
    };
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!("expected {target} abort for {source:?}, got success stdout={stdout:?}");
    }
    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to also reject {source:?}, got {interpreted:?}"
    );
}

fn assert_wasm_aborts(source: &str) {
    assert_wasm_target_aborts(source, outimage::CompileTarget::WasmNode);
    assert_wasm_target_aborts(source, outimage::CompileTarget::WasmBrowser);
}

#[test]
fn mir_wasm_node_embeds_outtext_literal() {
    let path = compile_wasm(
        r#"begin OutText("hello world"); OutImage; end;"#,
        outimage::CompileTarget::WasmNode,
    );
    let bytes = std::fs::read(&path).expect("read wasm");
    assert!(bytes.starts_with(b"\0asm"));
    assert!(
        bytes
            .windows(b"hello world".len())
            .any(|w| w == b"hello world"),
        "expected string literal in wasm data (MIR path)"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn mir_wasm_browser_embeds_outtext_literal() {
    let path = compile_wasm(
        r#"begin OutText("hello world"); OutImage; end;"#,
        outimage::CompileTarget::WasmBrowser,
    );
    let bytes = std::fs::read(&path).expect("read wasm");
    assert!(bytes.starts_with(b"\0asm"));
    assert!(
        bytes
            .windows(b"hello world".len())
            .any(|w| w == b"hello world"),
        "expected string literal in wasm-browser MIR data"
    );
    // Frozen browser modules exported `outputLen`; live MIR uses `_start`.
    assert!(
        !bytes.windows(b"outputLen".len()).any(|w| w == b"outputLen"),
        "wasm-browser should not use the frozen outputLen export"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn mir_wasm_hello_world() {
    assert_wasm_matches_interpreter(r#"begin OutText("hello world"); OutImage; end;"#);
}

#[test]
fn mir_wasm_arithmetic_then_branch() {
    assert_wasm_matches_interpreter(
        r#"begin integer x; x := 40 + 2; if x = 42 then OutText("ok") else OutText("bad"); OutImage; end;"#,
    );
}

#[test]
fn mir_wasm_if_else_else_branch() {
    assert_wasm_matches_interpreter(
        r#"begin integer x; x := 1; if x = 0 then OutText("then") else OutText("else"); OutImage; end;"#,
    );
}

#[test]
fn mir_wasm_while_counts() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer i;
            i := 0;
            while i < 3 do begin
                OutText("x");
                i := i + 1;
            end;
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_goto_label() {
    assert_wasm_matches_interpreter(
        r#"begin integer x;
           x := 0;
           goto done;
           x := 99;
           done:
           if x = 0 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_goto_into_labelled_if_branch() {
    assert_wasm_matches_interpreter(
        r#"begin integer x;
           x := 0;
           goto target;
           if true then target: x := 1 else x := 2;
           if x = 1 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_recursive_integer_name_parameter() {
    assert_wasm_matches_interpreter(
        r#"begin integer x, y;
           integer procedure dec(n); name n; integer n;
           begin
              if n <= 0 then dec := 0
              else begin
                 n := n - 1;
                 dec := dec(n) + 1;
              end;
           end;
           x := 3;
           y := dec(x);
           if y = 3 then begin
              if x = 0 then OutText("ok") else OutText("badx");
           end else OutText("bady");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_recursive_integer_array_element_name_parameter() {
    // The name actual is an assigned array element `a(1)`: the outlined
    // thunk must alias `a(1)` end-to-end through the whole recursive chain
    // (the interpreter is the oracle for `y = 3`, `a(1) = 0`).
    assert_wasm_matches_interpreter(
        r#"begin integer array a(1:2); integer y;
           integer procedure dec(n); name n; integer n;
           begin
              if n <= 0 then dec := 0
              else begin
                 n := n - 1;
                 dec := dec(n) + 1;
              end;
           end;
           a(1) := 3;
           y := dec(a(1));
           if y = 3 then begin
              if a(1) = 0 then OutText("ok") else OutText("bada1");
           end else OutText("bady");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_recursive_integer_array_element_name_parameter_simple_var_index() {
    // Same as above, but the index is a simple variable (`a(i)`) rather
    // than a constant literal; `i` itself is never mutated by `dec`, so the
    // index stays fixed at 1 for the whole recursive chain.
    assert_wasm_matches_interpreter(
        r#"begin integer array a(1:2); integer y, i;
           integer procedure dec(n); name n; integer n;
           begin
              if n <= 0 then dec := 0
              else begin
                 n := n - 1;
                 dec := dec(n) + 1;
              end;
           end;
           i := 1;
           a(i) := 3;
           y := dec(a(i));
           if y = 3 then begin
              if a(1) = 0 then OutText("ok") else OutText("bada1");
           end else OutText("bady");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_recursive_readonly_name_expression_actual() {
    assert_wasm_matches_interpreter(
        r#"begin integer n, r;
           integer procedure fact(x); name x; integer x;
           begin
              if x <= 1 then fact := 1 else fact := x * fact(x - 1);
           end;
           n := 5;
           r := fact(n);
           if r = 120 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_name_expression_actual_reevals_mutated_free_var() {
    assert_wasm_matches_interpreter(
        r#"begin integer i, r;
           integer procedure twice(n, k); name n, k; integer n, k;
           begin
              integer t;
              t := n;
              k := k + 10;
              if n = -999 then twice := twice(n, k) else twice := t + n;
           end;
           i := 1;
           r := twice(i + 1, i);
           if r = 14 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_name_if_expression_actual_reevals_after_mutation() {
    assert_wasm_matches_interpreter(
        r#"begin integer i, r;
           integer procedure twice(n, k); name n, k; integer n, k;
           begin
              integer t;
              t := n;
              k := k + 1;
              if n = -999 then twice := twice(n, k) else twice := t + n;
           end;
           i := 0;
           r := twice(if i < 1 then 10 else 20, i);
           if r = 30 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_recursive_integer_remote_field_name_parameter_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"begin
           class C; begin integer x; end;
           ref(C) r; integer y;
           integer procedure dec(n); name n; integer n;
           begin
              if n <= 0 then dec := 0
              else begin
                 n := n - 1;
                 dec := dec(n) + 1;
              end;
           end;
           r :- new C;
           r.x := 3;
           y := dec(r.x);
           if y = 3 then begin
              if r.x = 0 then OutText("ok") else OutText("badx");
           end else OutText("bady");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_name_remote_field_expression_actual_reevals() {
    assert_wasm_matches_interpreter(
        r#"begin
           class C; begin integer x; end;
           ref(C) r;
           integer y;
           integer procedure fact(n); name n; integer n;
           begin
              if n <= 1 then fact := 1 else fact := n * fact(n - 1);
           end;
           r :- new C;
           r.x := 5;
           y := fact(r.x);
           if y = 120 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_inlined_name_remote_field_expression_reevals_after_mutation() {
    assert_wasm_matches_interpreter(
        r#"begin
           class C; begin integer x; end;
           ref(C) r;
           integer result;
           integer procedure twice(n); name n; integer n;
           begin
              integer t;
              t := n;
              r.x := r.x + 10;
              twice := t + n;
           end;
           r :- new C;
           r.x := 1;
           result := twice(r.x + 1);
           if result = 14 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_boolean_and() {
    assert_wasm_matches_interpreter(
        r#"begin
            boolean a, b;
            a := true;
            b := false;
            if a and b then OutText("yes") else OutText("no");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_integer_procedure() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer procedure f(x); value x; integer x;
            begin
                f := x + 1;
            end;
            integer y;
            y := f(41);
            if y = 42 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_void_procedure_side_effect() {
    assert_wasm_matches_interpreter(
        r#"begin
            procedure greet;
            begin
                OutText("hi");
                OutImage;
            end;
            greet;
            OutText("done");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_recursive_fact() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer procedure fact(n); value n; integer n;
            begin
                if n <= 1 then fact := 1 else fact := n * fact(n - 1);
            end;
            if fact(5) = 120 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_notext_outtext() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_literal_assign_outtext() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t := "hello";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_ref_assign_outtext() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t :- "world";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_declaration_initializer() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t := "hi there";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_alias_assign() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t, u;
            t := "ab";
            u := t;
            OutText(u);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_concat() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t := "a" & "b";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_copy() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t :- copy("Hi");
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_content_eq() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t := "ab";
            if t = "ab" then OutText("y") else OutText("n");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_content_ne() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t := "ab";
            if t = "xy" then OutText("y") else OutText("n");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_integer_array_write_read() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer array a(1:3);
            integer x;
            a(1) := 10;
            a(3) := 30;
            x := a(1) + a(2) + a(3);
            if x = 40 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_integer_array_loop_sum() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer array a(1:3);
            integer i, s;
            i := 1;
            while i <= 3 do begin
                a(i) := i;
                i := i + 1;
            end;
            s := a(1) + a(2) + a(3);
            if s = 6 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_length_and_pos() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t := "ab";
            if t.length = 2 then OutText("L") else OutText("?");
            if t.pos = 1 then OutText("P") else OutText("?");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_setpos_more_getchar() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            character c;
            t := "ab";
            t.setpos(1);
            if t.more then begin
                c := t.getchar;
                if c = 'a' then OutText("a") else OutText("?");
            end;
            if t.more then begin
                c := t.getchar;
                if c = 'b' then OutText("b") else OutText("?");
            end;
            if not t.more then OutText("done") else OutText("more");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_array_write_read() {
    assert_wasm_matches_interpreter(
        r#"begin
            text array a(1:3);
            text t;
            t :- "hi";
            a(1) := "x";
            a(2) :- t;
            OutText(a(1));
            OutText(a(2));
            OutText(a(3));
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_value_text_array_formal_isolates_frames() {
    assert_wasm_matches_interpreter(
        r#"begin
            text array a(1:2);
            a(1) :- "hi";
            a(2) :- "lo";
            procedure bump(x); value x; text array x;
            begin x(1) :- "zz"; end;
            bump(a);
            if a(1) = "hi" then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_two_dimensional_integer_array() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer array m(1:2, 1:2);
            integer x;
            m(1, 1) := 10;
            m(2, 2) := 20;
            x := m(1, 1) + m(2, 2) + m(1, 2);
            if x = 30 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_two_dimensional_text_array() {
    assert_wasm_matches_interpreter(
        r#"begin
            text array m(1:2, 1:2);
            m(1, 1) := "a";
            m(2, 2) := "z";
            OutText(m(1, 1));
            OutText(m(2, 2));
            OutImage;
        end;"#,
    );
}

// --- Objects (Phase 6) -------------------------------------------------------

#[test]
fn mir_wasm_new_then_field_store_and_load() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- new Point;
            p.x := 1;
            if p.x = 1 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_none_then_new() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- none;
            p :- new Point;
            p.x := 7;
            if p.x = 7 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_two_refs_alias_same_object() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- new Point;
            q :- p;
            p.x := 3;
            if q.x = 3 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_two_independent_new_objects() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- new Point;
            q :- new Point;
            p.x := 1;
            q.x := 2;
            if p.x = 1 and q.x = 2 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_default_field_is_zero() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- new Point;
            if p.x = 0 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_boolean_field() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C; begin boolean flag; end;
            ref(C) p;
            p :- new C;
            p.flag := true;
            if p.flag then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_prefix_class_fields() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            Point class Polar; begin integer r; end;
            ref(Polar) p;
            p :- new Polar;
            p.x := 1;
            p.r := 2;
            if p.x = 1 and p.r = 2 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_counter_methods() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure increment; begin n := n + 1; end;
                integer procedure get; begin get := n; end;
            end;
            ref(Counter) c;
            c :- new Counter;
            c.increment;
            c.increment;
            if c.get = 2 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_constructor_param() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C(n); integer n;
            begin end;
            ref(C) p;
            p :- new C(42);
            if p.n = 42 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_class_body_init() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C; begin
                integer x;
                x := 10;
            end;
            ref(C) p;
            p :- new C;
            if p.x = 10 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_reference_equality() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C; begin integer x; end;
            ref(C) p, q;
            p :- new C;
            q :- p;
            if p == q then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_inspect_when() {
    assert_wasm_matches_interpreter(
        r#"begin
            class A; begin end;
            class B; begin end;
            ref(A) p;
            p :- new A;
            inspect p when A do OutText("A") when B do OutText("B") otherwise OutText("?");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_inspect_none_otherwise() {
    assert_wasm_matches_interpreter(
        r#"begin
            class A; begin end;
            ref(A) p;
            p :- none;
            inspect p when A do OutText("A") otherwise OutText("none");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_virtual_method_dispatch() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Base; virtual: integer procedure f;
            begin integer procedure f; begin f := 1; end; end;
            Base class Derived;
            begin integer procedure f; begin f := 2; end; end;
            ref(Base) p;
            p :- new Derived;
            if p.f() = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_field_outtext() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C; begin text t; end;
            ref(C) p;
            p :- new C;
            p.t :- copy("hi");
            OutText(p.t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_qua_same_class() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C; begin integer x; end;
            ref(C) p, q;
            p :- new C;
            p.x := 5;
            q :- p qua C;
            if q.x = 5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_real_arithmetic() {
    assert_wasm_matches_interpreter(
        r#"begin
            real a, b, c;
            a := 1.5;
            b := 2.0;
            c := a + b * 2.0;
            if c = 5.5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_long_real_arithmetic() {
    assert_wasm_matches_interpreter(
        r#"begin
            long real a, b, c;
            real r;
            a := 1.5&&0;
            b := 2.0;
            c := a + b * 2.0;
            r := c;
            if r = 5.5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_real_division_of_integers() {
    assert_wasm_matches_interpreter(
        r#"begin
            real r;
            r := 7 / 2;
            if r = 3.5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_real_object_field() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C; begin real x; end;
            ref(C) p;
            p :- new C;
            p.x := 2.5;
            if p.x = 2.5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_real_constructor_param() {
    assert_wasm_matches_interpreter(
        r#"begin
            class C(r); real r;
            begin end;
            ref(C) p;
            p :- new C(1.5);
            if p.r = 1.5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_real_pow() {
    assert_wasm_matches_interpreter(
        r#"begin
            real r;
            r := 2.0 ** 3.0;
            if r = 8.0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_blanks_and_strip() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t :- blanks(3);
            t := "ab ";
            OutText(t.strip);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_sub() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t, u;
            t :- copy("abcd");
            u :- t.sub(2, 2);
            OutText(u);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_upcase_on_copy() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t :- copy("Ab");
            upcase(t);
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_ref_eq() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t, u;
            t :- copy("x");
            u :- t;
            if t == u then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_getint_and_putint() {
    assert_wasm_matches_interpreter(
        r#"begin
            text amount, payment;
            integer pay;
            amount :- " 1200";
            pay := amount.getint;
            payment :- blanks(8);
            payment.putint(pay);
            OutText(payment.strip);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_getfrac_and_putfrac() {
    assert_wasm_matches_interpreter(
        r#"begin
            text amount, price, payment;
            integer pay;
            amount :- " 1200";
            price :- "155.75";
            pay := amount.getint * price.getfrac;
            payment :- blanks(12);
            payment.putfrac(pay, 2);
            OutText(payment.strip);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_deedit_edit_fixture() {
    let source = include_str!("fixtures/text_attributes/deedit_edit.sim");
    assert_wasm_matches_interpreter(source);
}

#[test]
fn mir_wasm_getreal_and_putfix() {
    assert_wasm_matches_interpreter(
        r#"begin
            real r;
            text t, out;
            t :- " 3.14";
            r := t.getreal;
            out :- blanks(10);
            out.putfix(r, 2);
            OutText(out.strip);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_putreal_scientific() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t;
            t :- blanks(16);
            t.putreal(12.5, 2);
            OutText(t.strip);
            OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_putreal_denormal_and_maxreal_match_interpreter() {
    assert_wasm_matches_interpreter(
        r#"begin
            text t; long real r;
            t :- blanks(40);
            r := addepsilon(0.0&&0);
            t.putreal(r, 18);
            OutText(t.strip); OutImage;
            t :- blanks(30);
            t.putreal(maxreal, 7);
            OutText(t.strip); OutImage;
            t :- blanks(30);
            t.putreal(.88888888888888888&&0, 16);
            OutText(t.strip); OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_getint_on_notext_aborts() {
    assert_wasm_aborts(
        r#"begin
            text t;
            integer n;
            n := t.getint;
        end;"#,
    );
}

#[test]
fn mir_wasm_putint_on_literal_aborts() {
    assert_wasm_aborts(
        r#"begin
            text t;
            t :- "abc";
            t.putint(1);
        end;"#,
    );
}

#[test]
fn mir_wasm_array_reference_parameter_aliases_caller() {
    assert_wasm_matches_interpreter(
        r#"begin integer array a(1:2);
           procedure set(x); integer array x; begin x(1) := 99; end;
           a(1) := 1; a(2) := 2;
           set(a);
           if a(1) = 99 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_reference_parameter_ref_assign_is_local() {
    assert_wasm_matches_interpreter(
        r#"begin text t;
           procedure set(x); text x; begin x :- copy("hi"); end;
           set(t);
           if t == notext then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_text_value_parameter_copy_isolates_caller() {
    assert_wasm_matches_interpreter(
        r#"begin text t;
           procedure mutate(x); value x; text x;
           begin upcase(x); end;
           t :- copy("hi");
           mutate(t);
           OutText(t); OutImage;
        end;"#,
    );
}

// --- Ch.7 coroutines over spill buffers --------------------------------------
//
// Every component runs on a stack of its own, and wasm cannot switch stacks, so
// `mir::asyncify` moves those stacks to the heap: suspending unwinds to a
// trampoline, spilling each live frame, and reactivating rewinds them. These
// check the visible half of that — the order control actually comes back in.

/// For programs the interpreter has no answer for, so the expected output is
/// spelled out. Native runs these too; see `tests/coro_stacks.rs`.
fn assert_wasm_prints(source: &str, expected: &str) {
    for target in [
        outimage::CompileTarget::WasmNode,
        outimage::CompileTarget::WasmBrowser,
    ] {
        let path = compile_wasm(source, target);
        let Some(stdout) = run_wasm(&path, target) else {
            let _ = std::fs::remove_file(&path);
            return;
        };
        let _ = std::fs::remove_file(&path);
        assert_eq!(stdout, expected, "{target} diverged for {source:?}");
    }
}

#[test]
fn mir_wasm_runs_a_detach_call_roundtrip() {
    assert_wasm_matches_interpreter(include_str!(
        "fixtures/simulation/detach_call_roundtrip.sim"
    ));
}

#[test]
fn mir_wasm_resume_makes_a_component_operative() {
    assert_wasm_matches_interpreter(
        r#"begin
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
        end;"#,
    );
}

#[test]
fn mir_wasm_a_class_can_detach_more_than_once() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Worker;
            begin
                OutText("1"); OutImage;
                detach;
                OutText("2"); OutImage;
                detach;
                OutText("3"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("x"); OutImage;
            call(w);
            OutText("y"); OutImage;
            call(w);
        end;"#,
    );
}

/// Spilling has to carry the frame's values, not just its resume point.
#[test]
fn mir_wasm_locals_survive_a_suspension() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Worker;
            begin
                integer n;
                n := 10;
                detach;
                n := n + 5;
                OutInt(n, 4); OutImage;
                detach;
                OutInt(n * 2, 4); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            call(w);
            call(w);
        end;"#,
    );
}

/// Two components of one system take turns, each keeping a loop counter across
/// the suspension that hands control to the other.
#[test]
fn mir_wasm_components_interleave_under_resume() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Worker(id); integer id;
            begin
                integer i;
                detach;
                while i < 3 do
                begin
                    i := i + 1;
                    OutInt(id, 2); OutInt(i, 2); OutImage;
                    detach;
                end;
            end;
            ref(Worker) a, b;
            a :- new Worker(1);
            b :- new Worker(2);
            resume(a); resume(b); resume(a); resume(b);
        end;"#,
    );
}

/// 7.2's reactivation point need not sit in the component's own head: the
/// suspending statement is two frames down, so both have to spill on the way
/// out and come back in order.
#[test]
fn mir_wasm_a_detach_below_the_class_body_unwinds_every_frame() {
    assert_wasm_prints(
        r#"begin
            class Worker;
            begin
                procedure tick(k); integer k;
                begin
                    OutText("in "); OutInt(k, 2); OutImage;
                    detach;
                    OutText("out "); OutInt(k, 2); OutImage;
                end;
                tick(7);
                OutText("body end"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("main"); OutImage;
            call(w);
            OutText("done"); OutImage;
        end;"#,
        "in  7\nmain\nout  7\nbody end\ndone\n",
    );
}

#[test]
fn mir_wasm_inspect_call_keeps_enclosing_captures() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer n;
            class C1;
            begin
                n := n + 1;
                detach;
                n := n + 1;
                detach;
            end;
            class C2;
            begin
                detach;
                n := n + 10;
            end;
            ref(C1) a;
            ref(C2) b;
            a :- new C1;
            b :- new C2;
            inspect a do
            begin
                call(a);
                call(b);
            end;
            OutInt(n, 0); OutImage;
        end;"#,
    );
}

/// A class that never suspends is not a coroutine, so wasm still compiles it.
#[test]
fn mir_wasm_a_class_without_transfers_still_compiles() {
    assert_wasm_matches_interpreter(
        r#"begin
            class Point(x, y); integer x, y;
            begin
                OutInt(x + y, 0); OutImage;
            end;
            ref(Point) p;
            p :- new Point(2, 3);
        end;"#,
    );
}

#[test]
fn mir_wasm_simulation_hold_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"Simulation begin
            hold(1.0);
            OutText("done"); OutImage;
            OutFix(time, 3, 8); OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_hold_from_outlined_simulation_procedure_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"Simulation begin
            procedure nap;
            begin
                hold(1.0);
            end;
            OutText("before"); OutImage;
            nap;
            OutText("after"); OutImage;
            OutFix(time, 1, 4); OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_process_hold_via_enclosing_procedure_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"Simulation begin
            procedure nap;
            begin
                hold(1.0);
            end;
            process class Worker;
            begin
                OutText("W1"); OutImage;
                nap;
                OutText("W2"); OutImage;
            end;
            activate new Worker;
            OutText("M1"); OutImage;
            hold(2.0);
            OutText("M2"); OutImage;
        end;"#,
    );
}

/// Event times are binary64: a SQS that converted instead of reinterpreting
/// them would round every activation to a whole time unit.
#[test]
fn mir_wasm_simulation_keeps_fractional_event_times() {
    assert_wasm_matches_interpreter(
        r#"Simulation begin
            process class p(i); integer i;
            begin
                OutInt(i, 2); OutFix(time, 2, 8); OutImage;
            end;
            ref(p) array a(1:3);
            integer i;
            for i := 1 step 1 until 3 do a(i) :- new p(i);
            activate a(1) at 2.75;
            activate a(2) at 1.25;
            activate a(3) at 1.25 prior;
            hold(5.0);
            OutText("main"); OutFix(time, 2, 8); OutImage;
        end;"#,
    );
}

/// A process reaching its final end while MAIN heads the set takes the
/// terminate path; one with another process ahead of it takes the resuming
/// path. Both have to leave the component machinery consistent.
#[test]
fn mir_wasm_simulation_process_termination_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"Simulation begin
            process class p(i); integer i;
            begin
                OutText("enter"); OutInt(i, 2); OutImage;
                hold(1.0);
                OutText("leave"); OutInt(i, 2); OutImage;
            end;
            ref(p) x, y;
            x :- new p(1);
            y :- new p(2);
            activate x;
            activate y;
            hold(10.0);
            if x.terminated and y.terminated then
            begin OutText("both terminated"); OutImage; end;
        end;"#,
    );
}

/// `activate X before/after Y` files at Y's time rather than scanning it, so a
/// self-branching scan here shows up as a hang rather than wrong output.
#[test]
fn mir_wasm_simulation_relative_activation_matches_interpreter() {
    assert_wasm_matches_interpreter(
        r#"Simulation begin
            process class p(i); integer i;
            begin
                OutInt(i, 2); OutFix(time, 2, 8); OutImage;
            end;
            ref(p) array a(1:4);
            integer i;
            for i := 1 step 1 until 4 do a(i) :- new p(i);
            activate a(1) at 3.5;
            activate a(2) before a(1);
            activate a(3) after a(1);
            activate a(4) after a(3);
            hold(9.0);
            OutText("done"); OutImage;
        end;"#,
    );
}

#[test]
fn mir_wasm_environment_draw_normal_negexp() {
    assert_wasm_matches_interpreter(
        r#"begin
            integer U;
            boolean d;
            real n, e;
            text t;
            U := 1;
            d := draw(0.5, U);
            n := normal(0.0, 1.0, U);
            e := negexp(1.0, U);
            if d then OutText("T") else OutText("F");
            OutText(" ");
            t :- blanks(12); t.putfix(n, 3); OutText(t.strip);
            OutText(" ");
            t :- blanks(12); t.putfix(e, 3); OutText(t.strip);
            OutText(" ");
            OutInt(U, 0);
            OutImage;
        end;"#,
    );
}

#[test]
fn wasm_rejects_environment_random_helpers() {
    let source = r#"begin
        integer U, p;
        U := 1;
        p := poisson(1.5, U);
    end;"#;
    let error = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            temp_wasm_path("poisson-reject"),
            outimage::CompileTarget::WasmNode,
        ),
    )
    .expect_err("wasm should reject poisson");
    let message = error.to_string();
    assert!(
        message.contains("poisson")
            || message.contains("ENVIRONMENT")
            || message.contains("native only")
            || message.contains("not supported"),
        "unexpected error: {message}"
    );
}

fn read_leb128_u32(data: &[u8], pos: &mut usize) -> u32 {
    let mut result = 0u32;
    let mut shift = 0u32;
    loop {
        let byte = data[*pos];
        *pos += 1;
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return result;
        }
        shift += 7;
    }
}

fn wasm_custom_section<'a>(bytes: &'a [u8], want: &str) -> Option<&'a [u8]> {
    assert!(bytes.starts_with(b"\0asm"), "expected wasm magic");
    let mut rest = &bytes[8..];
    while !rest.is_empty() {
        let id = rest[0];
        rest = &rest[1..];
        let mut pos = 0;
        let size = read_leb128_u32(rest, &mut pos) as usize;
        let payload = &rest[pos..pos + size];
        rest = &rest[pos + size..];
        if id != 0 {
            continue;
        }
        let mut p = 0;
        let nlen = read_leb128_u32(payload, &mut p) as usize;
        let name = std::str::from_utf8(&payload[p..p + nlen]).ok()?;
        p += nlen;
        if name == want {
            return Some(&payload[p..]);
        }
    }
    None
}

fn wasm_section_payload(bytes: &[u8], want_id: u8) -> Option<&[u8]> {
    assert!(bytes.starts_with(b"\0asm"), "expected wasm magic");
    let mut rest = &bytes[8..];
    while !rest.is_empty() {
        let id = rest[0];
        rest = &rest[1..];
        let mut pos = 0;
        let size = read_leb128_u32(rest, &mut pos) as usize;
        let payload = &rest[pos..pos + size];
        rest = &rest[pos + size..];
        if id == want_id {
            return Some(payload);
        }
    }
    None
}

fn wasm_env_imports(bytes: &[u8]) -> Vec<String> {
    assert!(bytes.starts_with(b"\0asm"), "expected wasm magic");
    let mut rest = &bytes[8..];
    while !rest.is_empty() {
        let id = rest[0];
        rest = &rest[1..];
        let mut pos = 0;
        let size = read_leb128_u32(rest, &mut pos) as usize;
        let payload = &rest[pos..pos + size];
        rest = &rest[pos + size..];
        if id != 2 {
            continue;
        }
        let mut p = 0;
        let count = read_leb128_u32(payload, &mut p);
        let mut names = Vec::new();
        for _ in 0..count {
            let mod_len = read_leb128_u32(payload, &mut p) as usize;
            let module = std::str::from_utf8(&payload[p..p + mod_len]).expect("import module utf8");
            p += mod_len;
            let name_len = read_leb128_u32(payload, &mut p) as usize;
            let name = std::str::from_utf8(&payload[p..p + name_len]).expect("import name utf8");
            p += name_len;
            let kind = payload[p];
            p += 1;
            match kind {
                0 => {
                    let _ = read_leb128_u32(payload, &mut p);
                }
                other => panic!("unexpected import kind {other} for {module}.{name}"),
            }
            if module == "env" {
                names.push(name.to_string());
            }
        }
        return names;
    }
    Vec::new()
}

#[test]
fn wasm_sin_cos_does_not_import_text_getint() {
    let source = r#"begin
            real x;
            x := sin(1.0) + cos(0.5);
        end;"#;
    let path = compile_wasm(source, outimage::CompileTarget::WasmBrowser);
    let bytes = std::fs::read(&path).expect("wasm artifact");
    let env = wasm_env_imports(&bytes);
    assert!(
        env.iter().any(|n| n == "sin") && env.iter().any(|n| n == "cos"),
        "expected sin/cos imports, got {env:?}"
    );
    assert!(
        !env.iter().any(|n| n == "text_getint"),
        "sin/cos program should not import text_getint: {env:?}"
    );
    assert!(
        !env.iter().any(|n| n == "randint"),
        "sin/cos program should not import randint: {env:?}"
    );
    let rt = wasm_custom_section(&bytes, "simrt").expect("simrt section");
    // no_std math blob + DCE: libm bodies, no text/`fmt` rodata.
    const MAX_SIN_COS_RT: usize = 15_000;
    assert!(
        rt.len() < MAX_SIN_COS_RT,
        "simrt for sin/cos is {} bytes (bar {MAX_SIN_COS_RT})",
        rt.len()
    );
    assert!(
        rt.len() < outimage::bundled::WASM_RUNTIME.len(),
        "shaken rt {} >= full {}",
        rt.len(),
        outimage::bundled::WASM_RUNTIME.len()
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wasm_hello_outtext_still_imports_sysout_write() {
    let source = r#"begin OutText("Hello"); OutImage; end;"#;
    let path = compile_wasm(source, outimage::CompileTarget::WasmBrowser);
    let bytes = std::fs::read(&path).expect("wasm artifact");
    let env = wasm_env_imports(&bytes);
    assert!(
        env.iter().any(|n| n == "sysout_write"),
        "OutText should import sysout_write: {env:?}"
    );
    assert!(
        !env.iter().any(|n| n == "sin"),
        "hello should not import sin: {env:?}"
    );
    let data = wasm_section_payload(&bytes, 11).expect("data section");
    assert!(
        data.len() < 4_000,
        "hello data section should not embed 8 kB of unused image zeros, got {}",
        data.len()
    );
    let _ = std::fs::remove_file(&path);
}
