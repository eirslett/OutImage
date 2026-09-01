//! Integration coverage for integer arrays (1-D and N-D) through the MIR →
//! Cranelift native backend: compiles small programs to
//! real executables, runs them, and checks their stdout against the
//! interpreter (the semantics oracle) — mirroring `tests/mir_native.rs`'s
//! approach for the scalar subset.
//!
//! `OutText` accepts compile-time string literals and runtime text
//! expressions (variables, concat, `notext`); see `tests/mir_text.rs` for
//! full text coverage. Array tests that need to observe a *value* computed
//! from array contents still do so by branching on it and printing one of
//! two fixed literals, exactly like `tests/mir_native.rs`'s arithmetic
//! tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-mir-arrays-{tag}-{id}"))
}

/// Compiles `source` to a native executable and runs it, returning
/// `(stdout, exit_success)`.
fn run_native(source: &str) -> (String, bool) {
    let output_path = temp_output_path("bin");
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

    let result = std::process::Command::new(&artifact)
        .output()
        .unwrap_or_else(|error| panic!("compiled binary failed to run: {error}"));
    let _ = std::fs::remove_file(&artifact);

    (
        String::from_utf8_lossy(&result.stdout).into_owned(),
        result.status.success(),
    )
}

/// Runs `source` through the interpreter (the oracle for expected output).
fn run_interpreted(source: &str) -> String {
    outimage::compile_str(source)
        .unwrap_or_else(|error| panic!("interpreter failed for {source:?}: {error}"))
}

/// Asserts the native binary ran successfully and its stdout matches the
/// interpreter's.
fn assert_matches_interpreter(source: &str) {
    let (native, success) = run_native(source);
    assert!(
        success,
        "native binary for {source:?} exited unsuccessfully"
    );
    let interpreted = run_interpreted(source);
    assert_eq!(
        native, interpreted,
        "native and interpreted output diverged for {source:?}"
    );
}

/// Asserts the native binary aborts (non-zero exit / signal) before printing
/// anything past the array access, and that the interpreter also rejects the
/// same out-of-bounds access (rather than silently disagreeing on whether
/// the access is valid).
fn assert_aborts_on_bad_access(source: &str) {
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );
    assert_eq!(
        stdout, "",
        "no output should be printed after an out-of-bounds access: {source:?}"
    );

    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to also reject the out-of-bounds access in {source:?}, got {interpreted:?}"
    );
}

/// Asserts native compilation itself fails with a clear error mentioning
/// `needle` (used for constructs still out of scope: non-integer element
/// types, wrong subscript counts). Some of these are already caught earlier
/// by semantic analysis rather than MIR lowering; either phase erroring
/// clearly is an acceptable outcome here.
fn assert_compile_error_contains(source: &str, needle: &str) {
    let error = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            temp_output_path("err"),
            outimage::CompileTarget::Native,
        ),
    )
    .expect_err(&format!("expected {source:?} to fail to compile"));
    assert!(
        matches!(
            error.phase,
            outimage::error::Phase::Codegen | outimage::error::Phase::Semantic
        ),
        "expected a semantic or codegen error, got {:?}: {}",
        error.phase,
        error.message
    );
    assert!(
        error
            .message
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase()),
        "message was: {}",
        error.message
    );
}

#[test]
fn write_then_read_a_single_element() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:10);
            integer x;
            a(3) := 42;
            x := a(3);
            if x = 42 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn write_and_read_are_independent_per_index() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:5);
            a(1) := 10;
            a(2) := 20;
            if a(1) = 10 and a(2) = 20 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn overwriting_an_element_keeps_the_latest_value() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:5);
            a(1) := 10;
            a(1) := 20;
            if a(1) = 20 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn unwritten_elements_default_to_zero() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:5);
            integer x;
            x := a(3);
            if x = 0 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn loop_fills_array_and_reads_back_sum_via_branching() {
    // Fills a(1:5) with i*i, then sums the elements back out one at a time
    // (no aggregate arithmetic across a loop-computed accumulator that also
    // depends on array reads would be a stronger test, but this already
    // exercises write-in-a-loop + read-many-elements).
    assert_matches_interpreter(
        r#"begin
            integer array a(1:5);
            integer i, total;
            i := 1;
            while i <= 5 do begin
                a(i) := i * i;
                i := i + 1;
            end;
            total := a(1) + a(2) + a(3) + a(4) + a(5);
            if total = 55 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn index_expression_can_be_a_variable() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:5);
            integer i;
            i := 4;
            a(i) := 99;
            if a(4) = 99 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn index_expression_can_be_computed() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:10);
            integer i;
            i := 2;
            a(i + 3) := 7;
            if a(5) = 7 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn negative_lower_bound() {
    assert_matches_interpreter(
        r#"begin
            integer array a(-3:3);
            integer i;
            i := -3;
            while i <= 3 do begin
                a(i) := i * 2;
                i := i + 1;
            end;
            if a(-3) = -6 and a(0) = 0 and a(3) = 6 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn single_element_array() {
    assert_matches_interpreter(
        r#"begin
            integer array a(7:7);
            a(7) := 123;
            if a(7) = 123 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn access_at_the_low_bound() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:10);
            a(1) := 5;
            if a(1) = 5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn access_at_the_high_bound() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:10);
            a(10) := 5;
            if a(10) = 5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn empty_range_declaration_alone_is_legal() {
    // `low > high` is a legal (empty) declaration; it's only an *access*
    // to it that's out of bounds (checked below).
    assert_matches_interpreter(
        r#"begin
            integer array a(5:2);
            OutText("declared-ok");
            OutImage;
        end;"#,
    );
}

#[test]
fn nested_begin_block_can_use_an_outer_array() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:3);
            begin
                integer i;
                a(2) := 77;
                i := a(2);
                if i = 77 then OutText("ok") else OutText("bad");
                OutImage;
            end;
        end;"#,
    );
}

#[test]
fn two_arrays_do_not_alias_each_other() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:3);
            integer array b(1:3);
            a(1) := 1;
            b(1) := 2;
            if a(1) = 1 and b(1) = 2 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn array_element_can_feed_a_while_condition() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:5);
            integer i;
            a(1) := 3;
            i := 0;
            while i < a(1) do begin
                OutText(".");
                OutImage;
                i := i + 1;
            end;
            OutText("done");
            OutImage;
        end;"#,
    );
}

// --- Bounds checking (native abort) -------------------------------------

#[test]
fn store_past_the_high_bound_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            integer array a(1:10);
            a(11) := 1;
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn load_below_the_low_bound_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            integer array a(1:10);
            integer x;
            x := a(0);
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn any_access_to_an_empty_declared_range_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            integer array a(5:2);
            integer x;
            x := a(3);
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn negative_index_out_of_bounds_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            integer array a(-3:3);
            integer x;
            x := a(-4);
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

// --- Multi-dimensional arrays -------------------------------------------

#[test]
fn two_dimensional_write_then_read() {
    assert_matches_interpreter(
        r#"begin
            integer array m(1:2, 1:2);
            integer x;
            m(2, 1) := 7;
            x := m(2, 1);
            if x = 7 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn two_dimensional_independent_elements() {
    assert_matches_interpreter(
        r#"begin
            integer array m(1:2, 1:2);
            m(1, 1) := 10;
            m(2, 1) := 20;
            m(1, 2) := 30;
            m(2, 2) := 40;
            if m(1, 1) = 10 and m(2, 1) = 20 and m(1, 2) = 30 and m(2, 2) = 40 then
                OutText("ok")
            else
                OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn three_dimensional_array() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:2, 1:2, 1:2);
            integer x;
            a(2, 1, 2) := 99;
            x := a(2, 1, 2);
            if x = 99 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn two_dimensional_unwritten_elements_default_to_zero() {
    assert_matches_interpreter(
        r#"begin
            integer array m(1:2, 1:2);
            integer x;
            x := m(2, 2);
            if x = 0 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn two_dimensional_store_past_high_bound_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            integer array m(1:2, 1:2);
            m(3, 1) := 1;
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn two_dimensional_load_below_low_bound_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            integer array m(1:2, 1:2);
            integer x;
            x := m(0, 1);
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

// --- Explicitly out-of-scope constructs error clearly -------------------

#[test]
fn real_array_assign_and_read() {
    assert_matches_interpreter(
        r#"begin
            real array a(1:3);
            real r;
            a(1) := 1.5;
            a(2) := 2.5;
            r := a(1) + a(2);
            if r = 4.0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn real_array_bounds_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            real array a(0:2);
            integer lo, hi;
            lo := lowerbound(a, 1);
            hi := upperbound(a, 1);
            if lo = 0 and hi = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn text_array_defaults_to_notext() {
    assert_matches_interpreter(
        r#"begin
            text array a(1:3);
            OutText(a(1));
            OutText("|");
            OutText(a(2));
            OutImage;
        end;"#,
    );
}

#[test]
fn text_array_assign_and_outtext() {
    assert_matches_interpreter(
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
fn text_array_two_dimensional() {
    assert_matches_interpreter(
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

#[test]
fn text_array_store_out_of_bounds_aborts() {
    assert_aborts_on_bad_access(
        r#"begin
            text array a(1:2);
            a(3) := "x";
        end;"#,
    );
}

#[test]
fn array_formal_dimension_mismatch_is_compile_error() {
    // §4.6.6 rank is inferred from body uses (`x(1)` → 1-D); a 2-D actual is
    // rejected before codegen.
    assert_compile_error_contains(
        r#"begin
            integer array a(1:2, 1:2);
            procedure take(x); integer array x;
            begin integer v; v := x(1); end;
            take(a);
        end;"#,
        "array",
    );
}

#[test]
fn value_array_formal_does_not_mutate_caller() {
    assert_matches_interpreter(
        r#"begin
            integer array a(1:2);
            a(1) := 1; a(2) := 2;
            procedure bump(x); value x; integer array x;
            begin x(1) := 99; end;
            bump(a);
            if a(1) = 1 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn value_text_array_formal_does_not_mutate_caller() {
    assert_matches_interpreter(
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
fn boolean_array_declaration_compiles() {
    // Boolean arrays use the same `MirType::ArrayI64` backing as integer
    // arrays; element access converts Bool↔i64 in native emit.
    assert_matches_interpreter("begin boolean array a(1:5); end;");
}

#[test]
fn boolean_array_elements_roundtrip_native() {
    // Boolean arrays share the i64 cell ABI; emit must convert Bool↔i64 on
    // ArrayLoad/ArrayStore (same as FieldLoadI64/FieldStoreI64).
    assert_matches_interpreter(
        r#"begin
            boolean array a(1:3);
            a(1) := true;
            a(2) := false;
            a(3) := a(1);
            if a(1) and not a(2) and a(3) then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn indexing_with_two_subscripts_errors_clearly() {
    // Semantic analysis already rejects the dimension mismatch (a is
    // declared with 1 dimension but indexed with 2 subscripts) before MIR
    // lowering ever sees it, so the message talks about the mismatched
    // "array" type rather than "dimensional" explicitly.
    assert_compile_error_contains(
        "begin integer array a(1:5); integer x; x := a(1, 2); end;",
        "array",
    );
}
