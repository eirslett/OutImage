//! Integration coverage for the MIR → Cranelift native backend:
//! compiles small programs to real executables, runs them, and
//! checks their stdout against the interpreter (the semantics oracle).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-mir-native-{tag}-{id}"))
}

/// Compiles `source` to a native executable and runs it, returning stdout.
fn run_native(source: &str) -> String {
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

    assert!(
        result.status.success(),
        "compiled binary for {source:?} exited with {:?}; stderr: {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

/// Runs `source` through the interpreter (the oracle for expected output).
fn run_interpreted(source: &str) -> String {
    outimage::compile_str(source)
        .unwrap_or_else(|error| panic!("interpreter failed for {source:?}: {error}"))
}

/// Asserts the native binary and the interpreter agree on stdout.
fn assert_matches_interpreter(source: &str) {
    let native = run_native(source);
    let interpreted = run_interpreted(source);
    assert_eq!(
        native, interpreted,
        "native and interpreted output diverged for {source:?}"
    );
}

/// Asserts native stdout against an expectation derived by hand from the
/// Standard. Used where the interpreter cannot serve as the oracle because it
/// is known to deviate; each caller cites the rule it was derived from.
fn assert_native_output(source: &str, expected: &str) {
    assert_eq!(
        run_native(source),
        expected,
        "native output diverged from the Standard-derived expectation for {source:?}"
    );
}

#[test]
fn hello_world_out_text_and_out_image() {
    assert_matches_interpreter(r#"begin OutText("hello world"); OutImage; end;"#);
}

#[test]
fn arithmetic_executes_before_printing() {
    // Arithmetic runs, then a fixed
    // literal is printed regardless of the computed value.
    assert_matches_interpreter(r#"begin integer x; x := 40 + 2; OutText("ok"); OutImage; end;"#);
}

#[test]
fn arithmetic_result_is_correct() {
    // Stronger check than the above: the *value* of `40 + 2` drives which
    // branch executes, so a wrong add/compare would flip the branch.
    assert_matches_interpreter(
        r#"begin integer x; x := 40 + 2; if x = 42 then OutText("ok") else OutText("bad"); OutImage; end;"#,
    );
}

#[test]
fn if_else_picks_then_branch() {
    assert_matches_interpreter(
        r#"begin integer x; x := 5; if x > 3 then OutText("big") else OutText("small"); OutImage; end;"#,
    );
}

#[test]
fn if_else_picks_else_branch() {
    assert_matches_interpreter(
        r#"begin integer x; x := 1; if x > 3 then OutText("big") else OutText("small"); OutImage; end;"#,
    );
}

#[test]
fn if_without_else_when_false_does_nothing() {
    assert_matches_interpreter(
        r#"begin integer x; x := 1; if x > 3 then OutText("unreachable"); OutText("done"); OutImage; end;"#,
    );
}

#[test]
fn while_loop_prints_multiple_lines() {
    assert_matches_interpreter(
        r#"begin integer i; i := 0;
            while i < 4 do begin OutText("line"); OutImage; i := i + 1; end;
        end;"#,
    );
}

#[test]
fn while_loop_accumulates_then_prints_once() {
    assert_matches_interpreter(
        r#"begin integer i, total; i := 0; total := 0;
            while i < 5 do begin total := total + i; i := i + 1; end;
            if total = 10 then OutText("sum-ok") else OutText("sum-bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn while_loop_that_never_enters() {
    assert_matches_interpreter(
        r#"begin integer i; i := 10;
            while i < 5 do begin OutText("nope"); i := i + 1; end;
            OutText("skipped");
            OutImage;
        end;"#,
    );
}

#[test]
fn boolean_and_or_not_conditions() {
    assert_matches_interpreter(
        r#"begin boolean a, b;
            a := true; b := false;
            if a and not b then OutText("t1") else OutText("f1"); OutImage;
            if a or b then OutText("t2") else OutText("f2"); OutImage;
            if a and b then OutText("t3") else OutText("f3"); OutImage;
        end;"#,
    );
}

#[test]
fn boolean_from_relation() {
    assert_matches_interpreter(
        r#"begin integer x; boolean r;
            x := 7;
            r := x = 7;
            if r then OutText("eq") else OutText("ne"); OutImage;
        end;"#,
    );
}

#[test]
fn nested_if_statements() {
    assert_matches_interpreter(
        r#"begin integer x;
            x := 5;
            if x > 0 then begin
                if x > 10 then OutText("big") else OutText("small");
            end else OutText("nonpositive");
            OutImage;
        end;"#,
    );
}

#[test]
fn empty_begin_end_runs_and_exits_cleanly() {
    let native = run_native("begin end;");
    assert_eq!(native, "");
    assert_eq!(native, run_interpreted("begin end;"));
}

#[test]
fn integer_division_truncates_toward_zero() {
    assert_matches_interpreter(
        r#"begin integer x;
            x := 7 // 2;
            if x = 3 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn negative_integer_division() {
    assert_matches_interpreter(
        r#"begin integer x;
            x := (0 - 7) // 2;
            if x = -3 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn unary_minus_round_trip() {
    assert_matches_interpreter(
        r#"begin integer x;
            x := 5;
            x := -x;
            if x = -5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn unary_minus_on_negative_is_positive() {
    assert_matches_interpreter(
        r#"begin integer x;
            x := -5;
            x := -x;
            if x = 5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

// --- Local procedures -----------------------------------------------------

#[test]
fn function_procedure_result_selects_out_text_via_if() {
    // Exercises `integer procedure f(x); value x; integer x; begin f := x +
    // 1; end;` used as an expression, whose value then drives an `if`.
    assert_matches_interpreter(
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
fn void_procedure_out_text_side_effect() {
    assert_matches_interpreter(
        r#"begin
            procedure greet;
            begin
                OutText("hello from greet");
                OutImage;
            end;
            greet;
            OutText("done");
            OutImage;
        end;"#,
    );
}

#[test]
fn function_procedure_with_multiple_parameters() {
    assert_matches_interpreter(
        r#"begin
            integer procedure add(a, b); value a, b; integer a, b;
            begin
                add := a + b;
            end;
            integer z;
            z := add(19, 23);
            if z = 42 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn nested_call_f_of_g() {
    assert_matches_interpreter(
        r#"begin
            integer procedure f(x); value x; integer x;
            begin
                f := x + 1;
            end;
            integer procedure g(x); value x; integer x;
            begin
                g := x * 2;
            end;
            integer z;
            z := f(g(20));
            if z = 41 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn boolean_function_procedure_result() {
    assert_matches_interpreter(
        r#"begin
            boolean procedure isPositive(x); value x; integer x;
            begin
                isPositive := x > 0;
            end;
            if isPositive(5) then OutText("pos") else OutText("nonpos");
            OutImage;
            if isPositive(-5) then OutText("pos") else OutText("nonpos");
            OutImage;
        end;"#,
    );
}

#[test]
fn recursive_function_procedure() {
    // Not required for Phase 2, but the two-pass declare-then-define
    // emission in `emit.rs` supports it "for free", so
    // check it actually computes the right answer at runtime too.
    assert_matches_interpreter(
        r#"begin
            integer procedure fact(n); value n; integer n;
            begin
                if n <= 1 then fact := 1 else fact := n * fact(n - 1);
            end;
            integer r;
            r := fact(5);
            if r = 120 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn recursive_integer_name_parameter_matches_interpreter() {
    assert_matches_interpreter(
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
fn recursive_integer_array_element_name_parameter_matches_interpreter() {
    // The name actual is an assigned array element `a(1)`: the outlined
    // thunk must alias `a(1)` end-to-end through the whole recursive chain
    // (the interpreter is the oracle for `y = 3`, `a(1) = 0`).
    assert_matches_interpreter(
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
fn recursive_integer_array_element_name_parameter_simple_var_index_matches_interpreter() {
    // Same as above, but the index is a simple variable (`a(i)`) rather
    // than a constant literal; `i` itself is never mutated by `dec`, so the
    // index stays fixed at 1 for the whole recursive chain.
    assert_matches_interpreter(
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
fn recursive_readonly_name_expression_actual_matches_interpreter() {
    assert_matches_interpreter(
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
fn recursive_name_expression_actual_reevals_mutated_free_var() {
    // Jensen: `n` is bound to `i + 1`; mutating aliased `k` (also `i`) must
    // change the second read of `n`. Self-call forces the outlined thunk path.
    assert_matches_interpreter(
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
fn recursive_name_if_expression_actual_reevals_after_mutation() {
    // Name actual is `if i < 1 then 10 else 20`; mutating `i` between two
    // reads of formal `n` must change the second evaluation.
    assert_matches_interpreter(
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
fn recursive_integer_remote_field_name_parameter_matches_interpreter() {
    // Assigned name actual `r.x`: outlined field get/set thunks must alias
    // the live attribute through the whole recursive chain.
    assert_matches_interpreter(
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
fn recursive_name_remote_field_expression_actual_reevals() {
    // Outlined: name actual is the remote field `r.x` (field get/set helpers).
    assert_matches_interpreter(
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
fn inlined_name_remote_field_expression_reevals_after_mutation() {
    // Non-recursive (inlined) Jensen: body can see enclosing `r` and mutate
    // `r.x` between two reads of formal `n` bound to `r.x + 1`.
    assert_matches_interpreter(
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
fn goto_label_matches_interpreter() {
    assert_matches_interpreter(
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
fn goto_into_labelled_if_branch_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin integer x;
           x := 0;
           goto target;
           x := 9;
           if true then
               target: x := 1
           else
               x := 2;
           if x = 1 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn switch_class_attribute_matches_interpreter() {
    assert_matches_interpreter(include_str!(
        "fixtures/control_flow/switch_class_attribute.sim"
    ));
}

#[test]
fn name_parameter_errors_clearly_at_compile_time() {
    // Assigned name formals still reject non-L-value expression actuals.
    let error = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(
            r#"begin integer y;
               integer procedure bump(x); name x; integer x;
               begin
                  x := x + 1;
                  if x < 3 then bump := bump(x) else bump := x;
               end;
               y := bump(y + 1); end;"#,
        ),
        &outimage::CompileOptions::for_compile(
            temp_output_path("name-param"),
            outimage::CompileTarget::Native,
        ),
    )
    .expect_err("expected assigned name + expression actual to be rejected");

    assert_eq!(error.phase, outimage::error::Phase::Codegen);
    assert!(
        error.message.contains("simple")
            || error.message.contains("thunk")
            || error.message.contains("expression")
            || error.message.contains("assigned")
            || error.message.contains("call-by-name"),
        "message was: {}",
        error.message
    );
}

#[test]
fn for_step_until_prints_each_iteration() {
    assert_matches_interpreter(
        r#"begin
            integer i;
            for i := 1 step 1 until 3 do begin
                OutText("x");
                OutImage;
            end;
        end;"#,
    );
}

#[test]
fn real_arithmetic_add_mul_div_match_interpreter() {
    assert_matches_interpreter(
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
fn long_real_arithmetic_matches_interpreter() {
    assert_matches_interpreter(
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
fn real_division_of_integers_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            real r;
            r := 7 / 2;
            if r = 3.5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn real_compare_and_negation_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            real x;
            x := -1.25;
            if x < 0.0 and -x = 1.25 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn integer_assigned_to_real_promotes() {
    assert_matches_interpreter(
        r#"begin
            real r;
            integer i;
            i := 4;
            r := i;
            if r = 4.0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn real_pow_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            real r;
            r := 2.0 ** 3;
            if r = 8.0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn for_step_until_accumulates_sum() {
    assert_matches_interpreter(
        r#"begin
            integer i, s;
            s := 0;
            for i := 1 step 1 until 3 do s := s + i;
            if s = 6 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn for_value_element_single_assignment() {
    assert_matches_interpreter(
        r#"begin
            integer i, x;
            for i := 42 do x := i;
            if x = 42 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn for_while_element_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer i, n, x;
            n := 0; x := 0;
            for i := 1 while n < 3 do begin
                x := x + i;
                n := n + 1;
            end;
            if x = 3 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn for_reference_element_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class C; begin integer v; end;
            ref(C) r, s;
            s :- new C; s.v := 9;
            for r :- s do OutInt(r.v, 0);
            OutImage;
        end;"#,
    );
}

#[test]
fn enclosing_integer_and_text_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer n;
            text t;
            class Worker;
            begin
                OutInt(n, 0); OutImage;
                OutText(t); OutImage;
            end;
            n := 7;
            t :- copy("hi");
            ref(Worker) w;
            w :- new Worker;
        end;"#,
    );
}

#[test]
fn enclosing_mutation_at_new_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer n;
            n := 0;
            class W;
            begin
                n := n + 1;
            end;
            ref(W) w;
            w :- new W;
            OutInt(n, 0); OutImage;
        end;"#,
    );
}

#[test]
fn call_by_name_assignment_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin integer i;
           procedure set(n); name n; integer n;
           begin n := 7; end;
           set(i);
           if i = 7 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn jensen_innerproduct_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin integer array a(1:3), b(1:3);
           integer k, i; real y;
           procedure innerproduct(a,b,k,p,y); name p,y,a,b;
             integer k,p; real y,a,b;
           begin real s; integer pp;
             s := 0.0;
             for pp := 1 step 1 until k do
               begin p := pp; s := s + a * b; end;
             y := s
           end innerproduct;
           for i := 1 step 1 until 3 do
             begin a(i) := i; b(i) := 10 * i; end;
           k := 3;
           innerproduct(a(i), b(i), k, i, y);
           if y = 140.0 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn call_by_name_subscript_accum_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin integer array a(1:3);
           integer i, s;
           procedure accum(x, sum); name x, sum; integer x, sum;
           begin sum := sum + x; end;
           a(1) := 10; a(2) := 20; a(3) := 30;
           s := 0;
           for i := 1 step 1 until 3 do accum(a(i), s);
           if s = 60 then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn array_reference_parameter_aliases_caller() {
    assert_matches_interpreter(
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
fn text_array_reference_parameter_aliases_caller() {
    assert_matches_interpreter(
        r#"begin text array t(1:1);
           procedure put(x); text array x;
           begin x(1) :- copy("hi"); end;
           put(t);
           OutText(t(1)); OutImage;
        end;"#,
    );
}

#[test]
fn mixed_name_and_text_reference_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin text t; integer i;
           procedure mix(n, s); name n; integer n; text s;
           begin
             n := 7;
             s :- copy("hi");
           end;
           mix(i, t);
           if i = 7 then OutText(t) else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn mixed_name_and_object_reference_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
           class Box; begin integer v; end;
           ref(Box) p; integer i;
           procedure mix(n, x); name n; integer n; ref(Box) x;
           begin
             n := 3;
             x :- new Box;
           end;
           mix(i, p);
           if i = 3 and p =/= none then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn text_reference_parameter_ref_assign_does_not_update_caller() {
    // §4.6.3: `FP :- AP` copies the reference; rebinding the formal is local.
    assert_matches_interpreter(
        r#"begin text t;
           procedure set(x); text x;
           begin x :- copy("hi"); end;
           set(t);
           if t == notext then OutText("ok") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn text_reference_parameter_content_assign_shares_frame() {
    // §4.6.3: content assignment through the formal mutates the shared frame.
    assert_matches_interpreter(
        r#"begin text t;
           procedure fill(x); text x;
           begin x := "ab"; end;
           t :- copy("xy");
           fill(t);
           OutText(t); OutImage;
        end;"#,
    );
}

#[test]
fn text_value_parameter_copy_isolates_caller() {
    assert_matches_interpreter(
        r#"begin text t;
           procedure mutate(x); value x; text x;
           begin upcase(x); end;
           t :- copy("hi");
           mutate(t);
           OutText(t); OutImage;
        end;"#,
    );
}

#[test]
fn text_value_parameter_outtext_matches_interpreter() {
    assert_matches_interpreter(include_str!("fixtures/procedures/text_value_copy.sim"));
}

#[test]
fn object_reference_parameter_ref_assign_does_not_update_caller() {
    // §4.6.3: rebinding a ref formal is local; the caller's variable is unchanged.
    assert_matches_interpreter(
        r#"begin
           class Box; begin integer v; end;
           ref(Box) p;
           procedure bind(x); ref(Box) x;
           begin x :- new Box; end;
           procedure setv(x); ref(Box) x;
           begin x.v := 7; end;
           p :- new Box;
           p.v := 1;
           bind(p);
           if p =/= none and p.v = 1 then OutText("ok") else OutText("bad");
           OutImage;
           setv(p);
           if p.v = 7 then OutText("mut") else OutText("bad");
           OutImage;
        end;"#,
    );
}

#[test]
fn out_int_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer n;
            n := 40 + 2;
            OutInt(n, 0);
            OutImage;
            OutInt(-3, 0);
            OutImage;
        end;"#,
    );
}

#[test]
fn inline_reads_stdin_line() {
    let source = r#"begin
        OutText(InLine);
        OutImage;
    end;"#;
    let output_path = temp_output_path("inline");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .unwrap_or_else(|error| panic!("native InLine compile failed: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected a native artifact")
        }
    };

    let mut child = std::process::Command::new(&artifact)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn native InLine binary: {error}"));
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"hello\n")
            .unwrap_or_else(|error| panic!("write stdin: {error}"));
    }
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("wait native InLine: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        output.status.success(),
        "native InLine exited {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "hello\n", "native InLine stdout was {stdout:?}");
}

/// 10.4.3 defines `inchar` as `if not more then inimage; image.getchar`, with no
/// exception for an image the program supplied itself: `sysin.image :- …sub(1,5)`
/// gives SysIn a five-character image, so the first five characters come from the
/// blank image it starts with and the sixth triggers the transfer of the external
/// record *into* that image, which keeps its length (blank-padded, 10.4.2).
#[test]
fn inchar_refills_a_program_supplied_image_from_the_file() {
    let source = r#"begin character c; integer k;
        sysin.image :- sysin.image.sub(1,5);
        for k := 1 step 1 until 8 do
        begin c := inchar;
              outint(rank(c), 4);
        end;
        outimage;
    end;"#;
    let output_path = temp_output_path("inchar_refill");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .unwrap_or_else(|error| panic!("native inchar compile failed: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected a native artifact")
        }
    };

    let mut child = std::process::Command::new(&artifact)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn native inchar binary: {error}"));
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"!\n")
            .unwrap_or_else(|error| panic!("write stdin: {error}"));
    }
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("wait native inchar: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        output.status.success(),
        "native inchar exited {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "  32  32  32  32  32  33  32  32",
        "native inchar stdout was {stdout:?}"
    );
}

// --- Ch.7 coroutine MVP (detach + call) --------------------------------------

#[test]
fn detach_call_roundtrip_matches_interpreter() {
    assert_matches_interpreter(include_str!(
        "fixtures/simulation/detach_call_roundtrip.sim"
    ));
}

#[test]
fn detach_inside_if_then_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                OutText("A"); OutImage;
                if true then detach;
                OutText("B"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("C"); OutImage;
            call(w);
        end;"#,
    );
}

#[test]
fn detach_inside_if_compound_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                if true then begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end;
                OutText("C"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M"); OutImage;
            call(w);
        end;"#,
    );
}

#[test]
fn detach_inside_if_compound_else_skips_resume_prefix() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                if false then begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end else begin
                    OutText("E"); OutImage;
                end;
                OutText("C"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
        end;"#,
    );
}

#[test]
fn detach_in_else_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                OutText("A"); OutImage;
                if false then OutText("T") else detach;
                OutText("B"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M"); OutImage;
            call(w);
        end;"#,
    );
}

#[test]
fn detach_in_else_compound_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                if false then begin
                    OutText("T"); OutImage;
                end else begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end;
                OutText("C"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M"); OutImage;
            call(w);
        end;"#,
    );
}

#[test]
fn detach_inside_while_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                integer i;
                i := 0;
                while i < 2 do begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                    i := i + 1;
                end;
                OutText("C"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M1"); OutImage;
            call(w);
            OutText("M2"); OutImage;
            call(w);
            OutText("M3"); OutImage;
        end;"#,
    );
}

#[test]
fn mutual_resume_inside_while_native_output() {
    // Interpreter coroutine mutual-resume is incomplete; assert native sequencing.
    let source = r#"begin
            class Peer;
            begin
                ref(Peer) other;
                text tag;
                integer n;
                detach;
                while n < 2 do begin
                    OutText(tag); OutImage;
                    n := n + 1;
                    resume(other);
                end;
                OutText("end"); OutImage;
            end;
            ref(Peer) a, b;
            a :- new Peer;
            b :- new Peer;
            a.tag :- copy("P");
            b.tag :- copy("Q");
            a.other :- b;
            b.other :- a;
            resume(a);
            OutText("M"); OutImage;
        end;"#;
    assert_eq!(run_native(source), "P\nQ\nP\nQ\nend\nM\n");
}

#[test]
fn class_integer_array_after_detach_resume() {
    let out = run_native(
        r#"begin
            class Worker;
            begin
                integer array buf(1:3);
                integer n;
                buf(1) := 10;
                buf(2) := 20;
                detach;
                n := buf(1) + buf(2);
                outint(n, 0); outimage;
            end;
            ref(Worker) w;
            w :- new Worker;
            resume(w);
        end;"#,
    );
    assert_eq!(out, "30\n");
}

#[test]
fn class_text_array_after_detach_resume() {
    // simtst66 pattern: class attribute text array must survive detach/resume
    // via object field, not a stale inline local (SIGSEGV in array_store_text).
    let out = run_native(
        r#"begin
            class Reader;
            begin
                text array lines(1:2);
                lines(1) :- copy("alpha");
                lines(2) :- copy("beta");
                detach;
                if lines(1) = "alpha" and lines(2) = "beta" then
                    outtext("ok")
                else
                    outtext("bad");
                outimage;
            end;
            ref(Reader) r;
            r :- new Reader;
            resume(r);
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn simset_detach_resume_no_simulation_runtime() {
    // SIMSET + Link must not emit sim.cancel (requires active Simulation).
    //
    // The inner `begin ref(C) y; class C; ... end` is a subblock containing a
    // local class declaration, so by 7.2 it is a *system head* and `y` is a
    // component of that system rather than of the outer program. Hence:
    // `detach` in C returns to the generator (D); `resume(y)` (7.3.3) parks the
    // inner subblock with its reactivation point immediately after the resume
    // and moves into C (E); C's final end is a detach-with-termination (7.3.4),
    // which for a resumed object moves the PSC "to the current reactivation
    // point of the main component of S" -- the inner subblock -- giving F, G.
    // The interpreter stops after D here, so it cannot be the oracle.
    assert_native_output(
        r#"SIMSET begin
            ref(A) x;
            Link class A;
            begin
                OutText("A"); OutImage;
                begin
                    ref(C) y;
                    class C;
                    begin
                        OutText("C"); OutImage;
                        detach;
                        OutText("E"); OutImage;
                    end;
                    OutText("B"); OutImage;
                    y :- new C;
                    OutText("D"); OutImage;
                    resume(y);
                    OutText("F"); OutImage;
                end;
                OutText("G"); OutImage;
            end;
            OutText("AA"); OutImage;
            x :- new A;
            OutText("AB"); OutImage;
        end;"#,
        "AA\nA\nB\nC\nD\nE\nF\nG\nAB\n",
    );
}

#[test]
fn detach_inside_for_step_until_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                integer i;
                for i := 1 step 1 until 2 do begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end;
                OutText("C"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M1"); OutImage;
            call(w);
            OutText("M2"); OutImage;
            call(w);
            OutText("M3"); OutImage;
        end;"#,
    );
}

#[test]
fn detach_inside_for_value_list_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                integer i;
                for i := 1, 2 do begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end;
                OutText("C"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M1"); OutImage;
            call(w);
            OutText("M2"); OutImage;
            call(w);
            OutText("M3"); OutImage;
        end;"#,
    );
}

#[test]
fn multiple_detaches_in_if_branch_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class Worker;
            begin
                if true then begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                    detach;
                    OutText("C"); OutImage;
                end;
                OutText("D"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            OutText("M1"); OutImage;
            call(w);
            OutText("M2"); OutImage;
            call(w);
        end;"#,
    );
}

#[test]
fn resume_from_main_matches_call_ordering() {
    assert_matches_interpreter(
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
fn nested_resume_switch_matches_interpreter() {
    // `b` must exist before `new A` so the enclosing ObjectRef snapshot
    // matches interpreter `enclosing_locals` (copied at object creation).
    assert_matches_interpreter(
        r#"begin
            class B;
            begin
                OutText("B1"); OutImage;
                detach;
                OutText("B2"); OutImage;
                detach;
            end;
            class A;
            begin
                OutText("A1"); OutImage;
                detach;
                OutText("A2"); OutImage;
                resume(b);
                OutText("A3"); OutImage;
            end;
            ref(A) a; ref(B) b;
            b :- new B;
            a :- new A;
            OutText("M1"); OutImage;
            resume(a);
            OutText("M2"); OutImage;
            resume(b);
            OutText("M3"); OutImage;
            resume(a);
            OutText("M4"); OutImage;
        end;"#,
    );
}

#[test]
fn double_detach_requires_two_calls() {
    assert_matches_interpreter(
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

// --- Ch.12 Simulation / SQS MVP (hold + activate + time) ---------------------

#[test]
fn hold_and_activate_orders_by_time() {
    assert_matches_interpreter(include_str!("fixtures/simulation/hold_and_activate.sim"));
}

#[test]
fn hold_from_outlined_simulation_procedure_matches_interpreter() {
    // Outlined free procedures used to reject `hold` as "not supported in
    // native/wasm yet" because they were lowered without Simulation context.
    assert_matches_interpreter(
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
fn process_hold_via_enclosing_procedure_matches_interpreter() {
    assert_matches_interpreter(
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

#[test]
fn activate_delay_runs_after_hold() {
    assert_matches_interpreter(include_str!("fixtures/simulation/activate_delay.sim"));
}

#[test]
fn activate_at_schedules_absolute_time() {
    assert_matches_interpreter(
        r#"Simulation begin
            process class Worker;
            begin OutText("W"); OutImage; end;
            ref(Worker) w;
            w :- new Worker;
            activate w at 2.0;
            OutText("M1"); OutImage;
            hold(1.0);
            OutText("M2"); OutImage;
            hold(2.0);
            OutText("M3"); OutImage;
        end;"#,
    );
}

#[test]
fn cancel_removes_scheduled_process() {
    assert_matches_interpreter(
        r#"Simulation begin
            process class Worker;
            begin OutText("W"); OutImage; end;
            ref(Worker) w;
            w :- new Worker;
            activate w delay 1.0;
            cancel(w);
            OutText("M"); OutImage;
            hold(2.0);
            OutText("done"); OutImage;
        end;"#,
    );
}

#[test]
fn passivate_and_reactivate_direct() {
    assert_matches_interpreter(
        r#"Simulation begin
            process class Worker;
            begin
                OutText("A"); OutImage;
                passivate;
                OutText("B"); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            activate w;
            OutText("M1"); OutImage;
            reactivate w;
            OutText("M2"); OutImage;
            hold(0);
            OutText("done"); OutImage;
        end;"#,
    );
}

#[test]
fn mir_rejects_wait_in_simulation_main() {
    let err = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(
            r#"Simulation begin
                ref(head) q; q :- new head;
                wait(q);
            end;"#,
        ),
        &outimage::CompileOptions::for_compile(
            temp_output_path("wait-main-reject"),
            outimage::CompileTarget::Native,
        ),
    )
    .unwrap_err();
    let message = err.to_string().to_ascii_lowercase();
    assert!(message.contains("wait"), "unexpected error: {err}");
}

#[test]
fn wait_queue_matches_interpreter() {
    assert_matches_interpreter(include_str!("fixtures/simulation/wait_queue.sim"));
}

#[test]
fn process_class_array_survives_activate() {
    // Array attributes on Process bodies are allocated on first `__init` entry
    // (before detach) and must resolve via object fields after activate.
    let stdout = run_native(
        r#"Simulation
        begin
            process class kund;
            begin
                integer array servtid(1:2);
                servtid(1) := 3;
                OutInt(servtid(1), 0); OutImage;
            end;
            activate new kund;
            hold(0);
        end;"#,
    );
    assert_eq!(stdout, "3\n");
}

#[test]
fn process_enclosing_captures_survive_hold_and_for_loop() {
    // Regression: writeback after run_current must not snapshot unused block
    // locals (e.g. for-index `i`) onto every Process — that reset `i` and
    // allocated customers forever (simtst87). Shared counters must still
    // accumulate across activate/hold/passivate.
    let stdout = run_native(
        r#"BEGIN
          EXTERNAL CLASS SIMULATION;
          INTEGER kunder;
          kunder := 3;
          SIMULATION
          BEGIN
            INTEGER i, klara;
            REF (Head) ARRAY kundq(1:2);
            PROCESS CLASS kund;
            BEGIN
              activate kundq(2).first DELAY 0;
              wait(kundq(1));
              klara := klara + 1;
              IF klara = kunder THEN activate main;
            END;
            PROCESS CLASS station;
            WHILE TRUE DO INSPECT kundq(1).first WHEN kund DO
            BEGIN
              Out;
              hold(1.0);
              activate THIS kund DELAY 0;
            END
            OTHERWISE BEGIN wait(kundq(2)); Out END;
            kundq(1) :- NEW Head;
            kundq(2) :- NEW Head;
            activate NEW station DELAY 0;
            FOR i := 1 STEP 1 UNTIL kunder DO
            BEGIN
              hold(0.5);
              activate NEW kund DELAY 0;
            END;
            passivate;
            OutInt(klara, 0); OutImage;
          END;
        END;"#,
    );
    assert_eq!(stdout.trim(), "3");
}

#[test]
fn link_suc_equals_none_at_head_sentinel() {
    // Bare `suc` in a Link body must use simset.suc (Head → none), not a raw
    // SUC pointer load — otherwise `suc==none` never holds and find recurses
    // forever (simtst96).
    let stdout = run_native(
        r#"BEGIN
          EXTERNAL CLASS SIMSET;
          REF (Head) towns;
          towns :- NEW Head;
          LINK CLASS town(nam_); VALUE nam_; TEXT nam_;
          BEGIN
            REF (town) PROCEDURE find(code); TEXT code;
            IF code = nam_ THEN find :- THIS town
            ELSE IF suc == NONE THEN find :- NEW town(code)
            ELSE find :- suc QUA town.find(code);
            INTO(towns);
          END;
          REF (town) r;
          r :- NEW town("A");
          r :- r.find("B");
          OutText(r.nam_); OutImage;
        END;"#,
    );
    assert_eq!(stdout.trim(), "B");
}

#[test]
fn activate_none_is_noop() {
    let stdout = run_native(
        r#"Simulation begin
            ref(process) p;
            p :- none;
            activate p delay 0;
            OutText("ok"); OutImage;
        end;"#,
    );
    assert_eq!(stdout.trim(), "ok");
}

#[test]
fn simset_only_suc_stops_at_head() {
    // Pure SIMSET (no Simulation) must register Head class id so suc/pred
    // treat Head as a sentinel (simtst93/94).
    assert_matches_interpreter(
        r#"SIMSET
        begin
            Link Class Bead(i); Integer i;;
            Ref(Head) chain;
            Ref(Bead) b;
            chain :- New Head;
            b :- New Bead(1);
            b.Into(chain);
            if chain.Suc =/= none then begin OutText("s"); OutImage; end;
            if chain.Suc.Suc == none then begin OutText("e"); OutImage; end;
        end;"#,
    );
}

#[test]
fn simset_into_out_suc_empty_cardinal_match_interpreter() {
    assert_matches_interpreter(
        r#"Simulation
        begin
            ref(head) q;
            ref(link) a;
            ref(link) b;
            q :- new head;
            a :- new link;
            b :- new link;
            a.into(q);
            b.into(q);
            if q.empty then begin OutText("bad_empty"); OutImage; end
            else begin OutText("ne"); OutImage; end;
            OutInt(q.cardinal, 0); OutImage;
            if q.first =/= none then begin OutText("f"); OutImage; end;
            if q.last =/= none then begin OutText("l"); OutImage; end;
            a.out;
            OutInt(q.cardinal, 0); OutImage;
            b.out;
            if q.empty then begin OutText("e"); OutImage; end;
        end;"#,
    );
}

#[test]
fn simulation_time_advances_with_hold() {
    assert_matches_interpreter(
        r#"Simulation begin
            real t;
            OutText("0"); OutImage;
            hold(3.0);
            t := time;
            if t = 3.0 then begin OutText("ok"); OutImage; end
            else begin OutText("bad"); OutImage; end;
        end;"#,
    );
}

#[test]
fn formal_procedure_parameter_matches_interpreter() {
    assert_matches_interpreter(include_str!(
        "fixtures/procedures/procedure_formal_restriction.sim"
    ));
}

#[test]
fn formal_procedure_void_and_value_formals() {
    assert_matches_interpreter(
        r#"begin
            procedure ping; begin OutText("p"); OutImage; end;
            procedure run(f); procedure f; begin f; end;
            integer procedure twice(x); integer x; begin twice := 2 * x; end;
            integer procedure apply(f, n); integer procedure f; integer n;
            begin apply := f(n); end;
            run(ping);
            OutInt(apply(twice, 7), 0); OutImage;
           end;"#,
    );
}

#[test]
fn environment_math_and_decimalmark_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            character c;
            integer n;
            real r;
            n := abs(-5);
            OutInt(n, 0); OutImage;
            n := sign(-2.5);
            OutInt(n, 0); OutImage;
            n := mod(10, 3);
            OutInt(n, 0); OutImage;
            r := sqrt(9.0);
            if r = 3.0 then begin OutText("sqrt"); OutImage; end;
            c := decimalmark(',');
            if c = '.' then begin OutText("dm"); OutImage; end;
            c := lowten('&');
            if c = '&' then begin OutText("lt"); OutImage; end;
           end;"#,
    );
}

#[test]
fn environment_random_draw_randint_uniform_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer U, r;
            boolean d;
            real u;
            text t;
            U := 1;
            d := draw(0.5, U);
            r := randint(1, 6, U);
            u := uniform(0.0, 10.0, U);
            if d then OutText("T") else OutText("F");
            OutText(" ");
            t :- blanks(4);
            t.putint(r);
            OutText(t.strip);
            OutText(" ");
            t :- blanks(8);
            t.putfix(u, 3);
            OutText(t.strip);
            OutImage;
           end;"#,
    );
}

#[test]
fn environment_random_normal_negexp_poisson_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer U, p;
            real n, e;
            text t;
            U := 7;
            n := normal(0.0, 1.0, U);
            e := negexp(1.0, U);
            p := poisson(2.0, U);
            t :- blanks(10);
            t.putfix(n, 4);
            OutText(t.strip); OutText(" ");
            t :- blanks(10);
            t.putfix(e, 4);
            OutText(t.strip); OutText(" ");
            OutInt(p, 0); OutImage;
           end;"#,
    );
}

#[test]
fn environment_basic_ops_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/basic_ops.sim"));
}

#[test]
fn environment_constants_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/constants.sim"));
}

#[test]
fn environment_array_bounds_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/array_bounds.sim"));
}

#[test]
fn environment_distributions_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/distributions.sim"));
}

#[test]
fn environment_histo_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/histo.sim"));
}

#[test]
fn environment_current_attrs_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/current_attrs.sim"));
}

#[test]
fn environment_sourceline_fixture_matches_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/sourceline.sim"));
}

#[test]
fn environment_text_utils_digit_letter_match_interpreter() {
    assert_matches_interpreter(&common_fixture("environment/text_utils.sim"));
}

#[test]
fn environment_math_extended_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            text t;
            real a, b, c;
            a := arcsin(0.0);
            b := arccos(1.0);
            c := arctan2(0.0, 1.0);
            t :- blanks(8);
            t.putfix(a, 3); OutText(t.strip); OutText(" ");
            t :- blanks(8);
            t.putfix(b, 3); OutText(t.strip); OutText(" ");
            t :- blanks(8);
            t.putfix(c, 3); OutText(t.strip); OutText(" ");
            t :- blanks(8);
            t.putfix(cotan(0.785398163), 3); OutText(t.strip);
            OutImage;
            a := sinh(0.0);
            b := cosh(0.0);
            c := log10(100.0);
            t :- blanks(8);
            t.putfix(a, 3); OutText(t.strip); OutText(" ");
            t :- blanks(8);
            t.putfix(b, 3); OutText(t.strip); OutText(" ");
            t :- blanks(8);
            t.putfix(c, 3); OutText(t.strip); OutImage;
           end;"#,
    );
}

#[test]
fn environment_max_min_integers_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer a, b;
            a := max(3, 7);
            b := min(3, 7);
            OutInt(a, 0); OutText(" ");
            OutInt(b, 0); OutImage;
           end;"#,
    );
}

#[test]
fn environment_error_exits_nonzero() {
    let source = r#"begin error("boom"); end;"#;
    let output_path = temp_output_path("env-error");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .expect("should compile")
    {
        outimage::CompileResult::Artifact(path) => path,
        _ => panic!("expected native artifact"),
    };
    let result = std::process::Command::new(&artifact)
        .output()
        .expect("run native");
    let _ = std::fs::remove_file(&artifact);
    assert!(!result.status.success(), "error should exit non-zero");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("boom"),
        "stderr should include message: {stderr}"
    );
}

#[test]
fn environment_antithetic_uniform_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            integer U, V;
            real a, b;
            U := 17;
            V := -17;
            a := uniform(0.0, 1.0, U);
            b := uniform(0.0, 1.0, V);
            if abs((a + b) - 1.0) < 1.0&-12 then OutText("ok") else OutText("fail");
            OutImage;
           end;"#,
    );
}

#[test]
fn boolean_and_then_or_else_imp_eqv_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            boolean a, b, c;
            integer n;
            a := false;
            b := true;
            c := a and then b;
            if c then n := 1 else n := 0;
            OutInt(n, 0); OutText(" ");
            c := a or else b;
            if c then n := 1 else n := 0;
            OutInt(n, 0); OutText(" ");
            c := a imp b;
            if c then n := 1 else n := 0;
            OutInt(n, 0); OutText(" ");
            c := a eqv false;
            if c then n := 1 else n := 0;
            OutInt(n, 0); OutImage;
           end;"#,
    );
}

#[test]
fn and_then_or_else_short_circuit_match_interpreter() {
    // If the right side were evaluated when short-circuiting, integer
    // division by zero would abort the native binary.
    assert_matches_interpreter(
        r#"begin
            boolean b;
            integer z;
            z := 0;
            b := false and then (1 // z = 0);
            if b then OutText("T") else OutText("F");
            OutText(" ");
            b := true or else (1 // z = 0);
            if b then OutText("T") else OutText("F");
            OutImage;
           end;"#,
    );
}

fn common_fixture(name: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    })
}

#[test]
fn environment_randint_invalid_range_exits_nonzero() {
    let source = r#"begin
        integer U, r;
        U := 1;
        r := randint(5, 1, U);
       end;"#;
    let output_path = temp_output_path("randint-bad");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .expect("should compile")
    {
        outimage::CompileResult::Artifact(path) => path,
        _ => panic!("expected native artifact"),
    };
    let result = std::process::Command::new(&artifact)
        .output()
        .expect("run native");
    let _ = std::fs::remove_file(&artifact);
    assert!(
        !result.status.success(),
        "randint with b < a should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("randint"),
        "stderr should mention randint: {stderr}"
    );
}

#[test]
fn nested_if_expression_selects_correct_branch() {
    // Regression: lower_expr_if used to re-enter arm entry blocks after nested
    // if-exprs, producing MIR with ops after Branch (Cranelift panic).
    assert_matches_interpreter(
        r#"begin
            boolean ok;
            ok := if true then (if true then true else false)
                  else (if false then true else false);
            if ok then OutText("ok") else OutText("bad");
            OutImage;
           end;"#,
    );
}

#[test]
fn nested_if_expression_in_statement_condition() {
    assert_matches_interpreter(
        r#"begin
            if if true then (if false then true else false)
               else true
            then OutText("bad") else OutText("ok");
            OutImage;
           end;"#,
    );
}

#[test]
fn integer_power_allows_negative_base() {
    // Simula §3.5.4: negative base is defined when the exponent is an integer.
    assert_matches_interpreter(
        r#"begin
            integer x;
            x := (-2)**3;
            if x = -8 then OutText("ok") else OutText("bad");
            OutImage;
           end;"#,
    );
}

#[test]
fn real_power_rejects_negative_base_with_fractional_exponent() {
    let source = r#"begin OutFix((-2.0)**1.5, 2, 8); OutImage; end;"#;
    let output_path = temp_output_path("neg-frac-pow");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .expect("should compile")
    {
        outimage::CompileResult::Artifact(path) => path,
        _ => panic!("expected native artifact"),
    };
    let result = std::process::Command::new(&artifact)
        .output()
        .expect("run native");
    let _ = std::fs::remove_file(&artifact);
    assert!(
        !result.status.success(),
        "negative**non-integer should fail"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("exponentiation undefined"),
        "stderr: {stderr}"
    );
}

#[test]
fn class_local_error_procedure_shadows_environment_builtin() {
    // DosTestBatch simtst06 defines `procedure error` inside a class; it must
    // print a diagnostic instead of aborting via ENVIRONMENT `error`.
    let stdout = run_native(
        r#"begin
            class C;
            begin
                procedure error(t); value t; text t;
                begin
                    OutText("local:");
                    OutText(t);
                    OutImage;
                end;
                error("rpower");
            end;
            ref (C) x;
            x :- new C;
           end;"#,
    );
    assert!(
        stdout.contains("local:rpower"),
        "expected local error procedure; got {stdout:?}"
    );
}

#[test]
fn real_pow_matches_exp_ln_identity() {
    // DosTestBatch simtst06 compares `b**e` with `exp(e*ln(b))`.
    let stdout = run_native(
        r#"begin
            long real b, e, x, y, diff;
            b := 2.0; e := 0.5;
            x := exp(e * ln(b));
            y := b ** e;
            diff := abs(if x = 0 then x - y else (x - y) / x);
            if diff > 7.0&&-14 then OutText("bad") else OutText("ok");
            OutImage;
           end;"#,
    );
    assert!(
        stdout.contains("ok"),
        "expected ** to match exp(e*ln(b)); got {stdout:?}"
    );
}

#[test]
fn prefixed_block_virtual_label_skips_to_match() {
    // §4.10.1 / simtst59: prefix `goto L` must transfer into the prefixed
    // block's matching label (skipping `i:=3`), not trap on an empty `$__init` BB.
    let source = r#"begin
            boolean bad;
            class C;
            virtual: label L;
            begin
                integer i;
                i := 1;
                goto L;
            end;
            C begin
                i := 3;
            L:
                if i <> 1 then bad := true;
            end;
            if bad then OutText("bad") else OutText("ok");
            OutImage;
           end;"#;
    let stdout = run_native(source);
    assert!(
        stdout.contains("ok"),
        "expected ok (i remains 1 after virtual goto); got {stdout:?}"
    );
    assert!(
        !stdout.contains("bad"),
        "virtual goto must skip i:=3; got {stdout:?}"
    );
}

#[test]
fn subclass_virtual_label_shadows_prefix_in_init() {
    // DosTestBatch simtst54: concatenated `$__init` must bind gotos/switches to
    // the innermost label occurrence (last wins), not merge BBs.
    let stdout = run_native(
        r#"BEGIN
          Class A (L, S, I);
             Boolean L, S; Integer I;
             Virtual: Label L1, L2; Switch S1, S2;
          BEGIN
             Switch S1 := L1, L2;
             If L then Goto L1;
             If S then Goto S1 (I);
             Goto OutA;
             L1: Outtext (" A.L1 "); Goto OutA;
             L2: Outtext (" A.L2 "); Goto OutA;
             L3: Outtext (" A.L3 "); Goto OutA;
            OutA:
          END;
          A Class B (L, S, I);
             Boolean L, S; Integer I;
             Virtual: Label L3;
          BEGIN
             Switch S2 := L3, L4;
             If L then Goto L2;
             If S then Goto S2 (I);
             Goto OutB;
             L2: Outtext (" B.L2 "); Goto OutB;
             L3: Outtext (" B.L3 "); Goto OutB;
             L4: Outtext (" B.L4 "); Goto OutB;
            OutB:
          END;
          Ref (B) RB;
          RB :- New B (true, false, 0, false, true, 1);
          RB :- New B (false, true, 2, false, true, 2);
          Outimage;
        END;"#,
    );
    assert!(
        stdout.contains("A.L1") && stdout.contains("B.L3") && stdout.contains("B.L2"),
        "expected A.L1 B.L3 B.L2; got {stdout:?}"
    );
    assert!(
        !stdout.contains("A.L3") && !stdout.contains("B.L4"),
        "must not hit prefix L3 or wrong switch arm; got {stdout:?}"
    );
}

#[test]
fn switch_designator_out_of_range_is_noop() {
    // Simula §4.5 / simtst54: illegal switch index does not transfer control.
    let stdout = run_native(
        r#"BEGIN
          Switch S := L1, L2;
          Goto S(3);
          Outtext("oor"); Outimage;
          Goto Done;
          L1: Outtext("L1"); Outimage; Goto Done;
          L2: Outtext("L2"); Outimage;
        Done:
        END;"#,
    );
    assert_eq!(stdout.trim(), "oor");
}

#[test]
fn inlined_procedure_labels_do_not_collide_across_calls() {
    // DosTestBatch simtst00: enclosing-capture procedures are inlined; each
    // call site needs a fresh LOOP/PRINT label scope or the second call loops.
    let stdout = run_native(
        r#"begin
          integer n;
          integer iterationcount;
          iterationcount := 3;
          procedure P(up); boolean up;
          begin integer stepi;
            while stepi < iterationcount do begin
              stepi := stepi + 1;
              if up then goto LOOP;
            LOOP:
            end;
            n := n + 1;
          end;
          P(true);
          P(false);
          OutInt(n, 0); OutImage;
        end;"#,
    );
    assert_eq!(stdout.trim(), "2");
}

#[test]
fn class_attribute_constant_initializer_runs_in_init() {
    // DosTestBatch simtst98: `integer i=12` must store into the object field.
    let stdout = run_native(
        r#"begin
          class a;
          begin integer i=12; integer ai;
            ai := i;
            OutInt(ai, 0); OutImage;
          end;
          new a;
        end;"#,
    );
    assert_eq!(stdout.trim(), "12");
}

#[test]
fn void_virtual_override_compiles_native() {
    // Regression for simtst55: void override in a virtual dispatch table must
    // not be lowered as `%t = call Class$voidProc(...)`.
    assert_matches_interpreter(
        r#"begin
            class A;
                virtual: procedure P;
            begin end;
            A class B;
            begin
                text procedure P; OutText("B");
            end;
            A class D;
            begin
                procedure P; OutText("D");
            end;
            ref(A) r;
            r :- new D;
            r qua A.P;
            OutImage;
           end;"#,
    );
}

#[test]
fn virtual_matching_procedures_may_differ_in_arity() {
    // DosTestBatch simtst57: unmatched virtual Dump matched by AA (1 arg) and
    // AB (0 args); call sites use the arity of the concrete matching procedure.
    assert_matches_interpreter(
        r#"begin
            class A;
                virtual: procedure Emit;
                         procedure Dump;
            begin
                OutText("A");
            end;
            A class AA;
            begin
                procedure Emit; OutText("AAEmit");
                procedure Dump(rf); ref(A) rf; OutText("AADump");
                OutText("AA");
            end;
            A class AB;
            begin
                procedure Emit; OutText("ABEmit");
                procedure Dump; OutText("ABDump");
                OutText("AB");
            end;
            ref(A) rA;
            rA :- new AA;
            rA.Emit;
            rA.Dump(rA);
            rA :- new AB;
            rA.Emit;
            rA.Dump;
            OutImage;
        end;"#,
    );
}

#[test]
fn shadowed_fields_respect_ref_and_qua_qualification() {
    // §5.5.6: access level is the reference qualification / qua, not creation class.
    assert_matches_interpreter(
        r#"begin
            class A; begin integer i; i := 1; end;
            A class B; begin integer i; i := 2; end;
            ref(A) ra; ref(B) rb;
            ra :- rb :- new B;
            if ra.i = 1 and ra qua B.i = 2 and rb.i = 2 and rb qua A.i = 1 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn inspect_when_matches_prefix_class() {
    // §4.8: when-clause matches if the object's class is the when-class or a subclass.
    assert_matches_interpreter(
        r#"begin
            class A; begin end;
            A class B; begin end;
            ref(B) rb;
            rb :- new B;
            inspect rb when A do OutText("A")
               when B do OutText("B")
               otherwise OutText("?");
            OutImage;
        end;"#,
    );
}

#[test]
fn inspect_when_connection_binds_when_class_attributes() {
    assert_matches_interpreter(
        r#"begin
            class A; begin integer i; end;
            A class B; begin integer i; end;
            ref(A) ra;
            ra :- new B;
            inspect ra when B do begin i := 2; end
               when A do begin i := 1; end;
            if ra.i = 0 and ra qua B.i = 2 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn nonvirtual_method_uses_static_qualification() {
    assert_matches_interpreter(
        r#"begin
            class A;
            begin text procedure Tp; Tp :- Copy("A"); end;
            A class B;
            begin text procedure Tp; Tp :- Copy("B"); end;
            ref(A) ra;
            ra :- new B;
            if ra.Tp = "A" and ra qua B.Tp = "B" then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn nested_begin_runs_in_source_order_and_restores_scope() {
    // DosTestBatch simtst08: nested begin/end must not run after later
    // statements, and inner locals must not leak past `end`.
    assert_matches_interpreter(
        r#"begin
            integer isum, int;
            int := 1;
            begin
                integer int;
                int := 1000;
                isum := isum + int;
            end;
            isum := isum + int;
            OutInt(isum, 5);
            OutImage;
        end;"#,
    );
}

#[test]
fn outreal_uses_scientific_format_not_outfix() {
    // DosTestBatch simtst28: `Outreal` was mis-lowered as `outfix` because
    // the builtin name kept source casing (`Outreal` != `outreal`).
    let out = run_native(
        r#"begin
            long real l;
            l := 200.2&-5;
            Outreal(100.1, 5, 12);
            Outreal(l, 5, 12);
            OutImage;
        end;"#,
    );
    assert!(
        out.contains("1.0010&+02") && out.contains("2.0020&-003"),
        "unexpected Outreal formatting: {out:?}"
    );
}

#[test]
fn name_param_assignment_chain_keeps_formal_type_value() {
    // DosTestBatch simtst37: `x := r := s := 3.14` with real name formals and
    // integer actuals must leave x ≈ 3.14 (not the truncated integer 3).
    let out = run_native(
        r#"begin
            integer i, j;
            real x;
            procedure P(r, s); name r, s; real r, s;
                x := r := s := 3.14;
            P(i, j);
            if i = 3 and j = 3 and x > 3.13 and x < 3.15 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn local_in_procedure_body_shadows_name_formal() {
    // DosTestBatch simtst39: `integer i` inside the body hides name formal `i`.
    let out = run_native(
        r#"begin
            integer i;
            procedure P(i); name i; integer i;
            begin
                integer i;
                i := 5;
            end;
            P(i);
            if i = 0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn name_ref_formal_shadows_enclosing_array() {
    // DosTestBatch simtst63: name formal `x` must hide outer `ref(A) array x`
    // so `x.aa` re-evaluates the object-reference actual, not the array itself.
    let out = run_native(
        r#"begin
            class A; begin integer aa; end;
            A class B; begin integer bb; end;
            B class C; begin integer cc; end;
            ref(A) array x(0:2);
            integer i;
            procedure P(Q, x, i);
                name x, i;
                procedure Q; ref(C) x; integer i;
            begin
                Q(x, i);
            end;
            procedure Q(y, i);
                name y, i;
                ref(A) y; integer i;
            begin
                i := 0; y.aa := 1;
                i := 1; y qua B.bb := x(0).aa;
                i := 2; y qua C.cc := x(0).aa;
            end;
            x(0) :- new A;
            x(1) :- new B;
            x(2) :- new C;
            P(Q, x(i), i);
            if x(0).aa = 1 and x(1) qua B.bb = 1 and x(2) qua C.cc = 1 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn qua_remote_assignment_lhs() {
    // `y qua B.bb :=` must keep the qua qualification on the assignment LHS.
    let out = run_native(
        r#"begin
            class A; begin integer aa; end;
            A class B; begin integer bb; end;
            ref(A) y;
            y :- new B;
            y qua B.bb := 7;
            if y qua B.bb = 7 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn inspect_bare_attr_write_in_name_procedure() {
    // DosTestBatch simtst63 parts 2–3: inside an inlined procedure, bare
    // attribute writes in `inspect` must hit the connected object's fields,
    // not sibling-capture `__simrt_encl_*` slots that share the attribute
    // name (e.g. free `cc` mentioned in procedure R).
    let out = run_native(
        r#"begin
            class A; begin integer aa; end;
            A class B; begin integer bb; end;
            B class C; begin integer cc; end;
            ref(A) array x(0:2);
            integer i;
            procedure P(Q, x, i);
                name x, i;
                procedure Q; ref(C) x; integer i;
            begin
                Q(x, i);
            end;
            procedure R(y, i);
                name y, i;
                ref(A) y; integer i;
            begin
                integer j;
                for j := 0, 1, 2 do
                begin
                    i := j;
                    inspect y
                        when C do cc := 2
                        when B do bb := 2
                        when A do aa := 2;
                end;
            end;
            x(0) :- new A;
            x(1) :- new B;
            x(2) :- new C;
            P(R, x(i), i);
            if x(0).aa = 2 and x(1) qua B.bb = 2 and x(2) qua C.cc = 2 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn nested_switch_designational_flow_uses_standard_if_eqv() {
    // DosTestBatch simtst26: mutual switches + nested Boolean if/eqv in a
    // designational element. CBL86 wrote `not b` in q(4); Standard eval of
    // `if j=i then not b else b eqv b` inverts the documented t1→l2 / t2→l3
    // flow. Override uses `b` (same shape, Standard-correct path).
    let out = run_native(
        r#"begin
            boolean found_error;
            integer i, j, t;
            boolean b;
            i := 5;
            begin
               switch st := t1, t2;
               switch s := l1, l2, q(i), if b imp j > i then q(2) else l1;
               switch q := s(1), q(4), q(2),
                           if if j=i then b else b eqv b then l3 else s(2),
                           q(1);
            t0: if t <> 0 or i <> 5 or j <> 0 or b then found_error := true;
                t := 1; go to q(i);
            t1: if t <> 1 then found_error := true;
                t := 2; j := 5; goto s(4);
            t2: if t <> 2 or i <> 5 or j <> 5 or not b then found_error := true;
                t := j := i := 3; goto s(3);
            l1: if t <> 1 or i <> 5 or j <> 0 or b then found_error := true;
                begin switch r := s(1), q(4), s(3);
                   begin character c; goto st(t) end;
                end;
            l2: if t <> 2 or i <> 5 or j <> 5 or b then found_error := true;
                b := true; goto st(t);
            l3: if t <> 3 or i <> 3 or j <> 3 or not b then found_error := true;
            end;
            if found_error then OutText("ERRORS") else OutText("NO ERRORS");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "NO ERRORS\n");
}

#[test]
fn value_label_param_freezes_designational_if() {
    // DosTestBatch simtst31: LABEL (by value) evaluates `IF` at call time.
    let out = run_native(
        r#"begin
            boolean b;
            text path;
            procedure P(LFD); label LFD;
            begin
                b := not b;
                goto LFD;
            end;
            goto start;
            L1: path.putchar('1'); goto done;
            L2: path.putchar('2'); goto done;
            start:
            path :- blanks(8);
            b := true;
            P(IF b THEN L2 ELSE L1);
            done:
            OutText(path.strip);
            OutImage;
        end;"#,
    );
    assert_eq!(out, "2\n");
}

#[test]
fn detach_inside_method_saves_continuation_pc() {
    // simtst69 P2: detach inside a parameterless method/procedure called from
    // a class body must suspend with a PC after the detach (not fall through
    // or restart the whole segment).
    let out = run_native(
        r#"begin
            procedure tr(t); value t; text t; begin outtext(t); outimage; end;
            class C;
            begin
                procedure P;
                begin
                    tr("P1"); detach; tr("P2");
                end;
                tr("C1"); detach;
                tr("C2"); P;
                tr("C3"); detach;
                tr("C4");
            end;
            ref(C) r;
            r :- new C;
            tr("M1");
            call(r);
            tr("M2");
            call(r);
            tr("M3");
            call(r);
            tr("M4");
        end;"#,
    );
    assert_eq!(out, "C1\nM1\nC2\nP1\nM2\nP2\nC3\nM3\nC4\nM4\n");
}

// A `detach` in a procedure of a prefixed block instance used to be asserted
// here as suspending whichever object was running. 7.3.1 says such a detach has
// no effect, and simtst69 agrees: it requires `Sjekk(7); Detach; Sjekk(8)` to
// record 7 and 8 consecutively. The program now lives in `tests/coro_stacks.rs`.

#[test]
fn resume_subclass_via_base_ref_qualification() {
    // simtst68: `ref(Coroutine) c` must resume `Changer$__init`, not `Coroutine$__init`.
    let out = run_native(
        r#"begin
            class Coroutine; detach;
            Coroutine class Worker;
            begin
                outtext("A"); outimage;
                detach;
                outtext("B"); outimage;
            end;
            ref(Coroutine) w;
            w :- new Worker;
            call(w);
            resume(w);
        end;"#,
    );
    assert_eq!(out, "A\nB\n");
}

#[test]
fn nested_class_detach_proc_keeps_ref_declarations() {
    // simtst69 P1: detach inside nested `Class C4` must split C1's continuation
    // without losing `Ref (C4) rC4` / `Ref (C3) rC3` bindings.
    //
    // Standard trace: C4 is declared in a procedure body, so by 7.2 its objects
    // are *independent* components -- `call(rC4)` reattaches it and its final
    // end returns after the call statement (7.3.4 on an attached object). C3 is
    // declared in the prefixed block, which is a system head, so `resume(rC3)`
    // parks that system's main component with its reactivation point inside P1;
    // C3's final end then returns there (7.3.4 on a resumed object).
    //
    // There is no second `call(r1)`: r1 has terminated by then, and 7.3.2 makes
    // a call on a terminated object an error rather than a no-op.
    assert_native_output(
        r#"begin
            procedure tr(t); value t; text t; begin outtext(t); outimage; end;
            class A;;
            A begin
                class C1;
                begin
                    procedure P1;
                    begin
                        ref(C3) rC3;
                        class C4;
                        begin
                            tr("A"); detach; tr("B");
                        end;
                        ref(C4) rC4;
                        rC4:- new C4; rC3:- new C3;
                        inspect rC4 do
                        begin
                            resume(rC3);
                            tr("C");
                            call(rC4);
                            tr("D");
                        end;
                    end;
                    tr("1"); detach;
                    tr("2"); P1;
                    tr("3");
                end;
                class C3;
                begin
                    tr("E"); detach;
                    tr("F");
                end;
                ref(C1) r1;
                r1 :- new C1;
                tr("M1");
                call(r1);
                tr("M2");
                tr("M3");
            end;
        end;"#,
        "1\nM1\n2\nA\nE\nF\nC\nB\nD\n3\nM2\nM3\n",
    );
}

#[test]
fn name_switch_param_uses_toggled_b_for_designational_element() {
    // DosTestBatch simtst31 C9: after an odd number of `b := not b` toggles,
    // `if b then S(S1) else S(S3)` takes S(S1) → '1' (not the original CBL86
    // expectation of '3'). Portable Simula corrected the same strings.
    let out = run_native(
        r#"begin
            boolean b;
            integer i;
            text path;
            switch S1 := L1, L2, L3;
            switch S3 := IF b THEN L2 ELSE L3;
            procedure S(SFN); name SFN; switch SFN;
            begin
                b := not b;
                i := i + 1;
                goto SFN(i);
            end;
            goto start;
            L1: path.putchar('1'); goto done;
            L2: path.putchar('2'); goto done;
            L3: path.putchar('3'); goto done;
            start:
            path :- blanks(8);
            b := true;
            i := 0;
            if b then S(S1) else S(S3);
            done:
            OutText(path.strip);
            OutImage;
        end;"#,
    );
    assert_eq!(out, "1\n");
}

#[test]
fn text_array_ref_assign_does_not_alias_source_frame() {
    // simtst28 subscripted text actuals: `ta(i) :- txt` must not share the
    // mutable frame descriptor of `txt` across later `txt :- copy(...)`.
    let out = run_native(
        r#"begin
            text array ta(0:2);
            text txt;
            integer i;
            for i := 0 step 1 until 2 do begin
                txt :- copy(" .");
                txt.sub(1, 1).putint(i);
                ta(i) :- txt;
            end;
            OutText(ta(0)); OutText(ta(2)); OutImage;
        end;"#,
    );
    assert_eq!(out, "0.2.\n");
}

#[test]
fn basicio_filename_matches_constructor_path() {
    let out = run_native(
        r#"begin
            ref (infile) xf; text filnavn;
            filnavn :- copy("demo78.dat");
            xf :- new infile(filnavn);
            if xf.filename = filnavn then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn free_setpos_inside_class_targets_sysout() {
    // Bare setpos inside a non-file class must use SYSIN/SYSOUT, not `this`
    // (DosTestBatch simtst98).
    let out = run_native(
        r#"begin
            class a;
            begin
                outtext("hi");
                setpos(1);
                outtext("XY");
                outimage;
            end;
            new a;
        end;"#,
    );
    assert_eq!(out, "XY\n");
}

#[test]
fn maxreal_is_ieee_single_max() {
    let out = run_native(
        r#"begin
            text t;
            t :- blanks(30);
            t.putreal(maxreal, 7);
            OutText(t.strip);
            OutImage;
        end;"#,
    );
    assert_eq!(out.trim(), "3.402823&+38");
}

#[test]
fn recursive_name_actual_parameterless_type_procedure() {
    // DosTestBatch simtst35: `P(sqri)` must re-eval `sqri` (= i*i), not snapshot.
    let out = run_native(
        r#"begin
            integer i, j;
            integer procedure sqri; sqri := i * i;
            integer procedure P(f); name f; integer f;
            begin
                i := i + 1;
                if i = 3 then P := f else P := f + P(f);
            end;
            i := 0;
            j := P(sqri);
            if j = 14 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn name_remote_attr_of_type_procedure_eval_once_per_use() {
    // simtst72: `Q(y.i)` with `y` name-bound to `x.Z` must not double-eval `x.Z`
    // on a single read of `y.i` (Variable::Remote fallthrough bug).
    let out = run_native(
        r#"begin
            ref(A) x, v;
            class A;
            begin
                ref(A) procedure Z;
                begin P(x); Z:- v:- new A; end;
                integer i; i := 5;
            end;
            procedure P(y); name y; ref(A) y;
            begin Q(y.i) end;
            procedure Q(ii); name ii; integer ii;
            begin ii := ii + 2 end;
            x :- new A;
            P(x.Z);
            if x.i = 9 and v.i = 5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn detach_call_resume_enclosing_local_sequencing() {
    // simtst67: enclosing `i` must sync across New/Detach/Call/Resume.
    let out = run_native(
        r#"begin
            integer array ia(1:10);
            integer i, nri, j;
            procedure savei(k); integer k;
            begin nri := nri + 1; ia(nri) := k end;
            class A;
            begin integer j; ref(B) rb;
                j := i := i+i; savei(i);
                Detach;
                j := i := j+i; savei(i);
                Call(rb);
                j := i := j+i; savei(i);
            end;
            class B;
            begin integer j;
                j := i := i+1; savei(i);
                Detach;
                j := i := j+2*i; savei(i);
                Detach;
                j := i := j+2*i; savei(i);
            end;
            ref(A) ua;
            i := 1;
            ua :- new A;
            ua.rb :- new B;
            Resume(ua);
            Resume(ua.rb);
            if ia(1)=2 and ia(2)=3 and ia(3)=5 and ia(4)=13 and ia(5)=18 and ia(6)=49
            then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn outreal_normalizes_negative_zero() {
    // simtst65: `-r` when r=0 must print as 0.0000&+00, not -0.0000&+00.
    let out = run_native(
        r#"begin
            real r;
            r := 0.0;
            r := -r;
            Outreal(r, 5, 12);
            Outimage;
        end;"#,
    );
    assert_eq!(out.trim(), "0.0000&+00");
}

#[test]
fn inspect_when_method_shadows_free_procedure() {
    // DosTestBatch simtst71: `inspect … when B do P2` sees B.P2, not global P2.
    let out = run_native(
        r#"begin
            integer i;
            class A; virtual: procedure P1;
            begin procedure P1; i := 1; end;
            A class B;
            begin procedure P1; i := 2; procedure P2; i := 3; end;
            ref(A) rA;
            procedure P2; i := 4;
            rA :- new B;
            inspect rA do begin P2 end;
            if i <> 4 then begin OutText("do"); OutImage; end;
            rA :- new B;
            inspect rA when B do begin P2 end;
            if i <> 3 then begin OutText("when"); OutImage; end
            else begin OutText("ok"); OutImage; end;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn inspect_connection_local_declaration_shadows_attribute() {
    // DosTestBatch simtst50: a block local declared inside the connection
    // shadows the connected object's same-named attribute, so neither the
    // prefix nor the subclass `i` is written.
    let out = run_native(
        r#"begin
            integer j;
            class A; begin integer i; end;
            A class B; begin integer i; end;
            ref(A) ra;
            ra :- new B;
            inspect ra when B do begin integer i; i := 6; j := 2 end;
            if j = 2 and ra.i = 0 and ra qua B.i = 0 then OutText("ok")
            else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn prefixed_block_procedure_overrides_virtual_in_prefix_body() {
    // DosTestBatch simtst92: the prefixed block is the class's inner part, so
    // its `P2` matches the prefix's virtual `P2`. A connected-attribute
    // preference must not steal the call for the prefix's own default body.
    let out = run_native(
        r#"begin
            boolean bad;
            class C;
            virtual: procedure P2;
            begin
                procedure P2; bad := true;
                P2;
            end;
            C begin
                procedure P2; OutText("ok");
            end;
            if bad then OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn inspect_inside_inlined_name_procedure_writes_connected_attribute() {
    // DosTestBatch simtst63: inside an inlined call-by-name procedure, the
    // `inspect` connection binds `aa`/`bb`/`cc` to the connected object's real
    // attributes — never to `__simrt_encl_*` sibling-capture slots.
    let out = run_native(
        r#"begin
            class A; begin integer aa; end;
            A class B; begin integer bb; end;
            B class C; begin integer cc; end;
            ref(A) array x(0:2);
            integer i;
            procedure P(Q, y, i); name y, i;
                procedure Q; ref(C) y; integer i;
            begin
                Q(y, i)
            end;
            procedure R(y, i); name y, i; ref(A) y; integer i;
            begin
                integer j;
                for j := 0, 1, 2 do
                begin
                    i := j;
                    inspect y
                        when C do cc := 7
                        when B do bb := 7
                        when A do aa := 7;
                end;
            end;
            x(0) :- new A; x(1) :- new B; x(2) :- new C;
            P(R, x(i), i);
            if x(0).aa = 7 and x(1) qua B.bb = 7 and x(2) qua C.cc = 7 then
                OutText("ok")
            else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn formal_procedure_method_name_param_and_inspect() {
    // DosTestBatch simtst73: class-body / inspect / remote formal-proc + name.
    let out = run_native(
        r#"begin
            procedure P(Q); procedure Q; Q(i);
            integer i;
            class A;
            begin
                procedure R(k); name k; integer k; k := k + k;
                integer i;
                P(R);
            end;
            ref(A) x;
            i := 1;
            x :- new A;
            inspect x do P(R);
            P(x.R);
            if i = 8 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn simset_only_program_registers_head_sentinel() {
    // simtst93 / simtst94: a SIMSET program without a Simulation block still
    // needs the Head class id registered, otherwise `Suc` walks off the end of
    // the ring instead of stopping at the head.
    let out = run_native(
        r#"SIMSET begin
            Link Class Bead(i); Integer i;;
            Ref(Head) chain;
            Ref(Bead) b;
            chain :- New Head;
            b :- New Bead(1);
            b.Into(chain);
            if chain.Suc =/= none then OutText("s");
            if chain.Suc.Suc == none then OutText("e");
            if chain.Last.Pred == none then OutText("p");
            OutImage;
        end;"#,
    );
    assert_eq!(out, "sep\n");
}

#[test]
fn bare_suc_in_link_body_is_the_simset_procedure() {
    // simtst96: `SUC` / `PRED` are the raw ring slots; a bare `suc` in a Link
    // body means the SIMSET procedure, which yields none at the Head. Reading
    // the slot instead makes `suc == none` unreachable and the walk recurse
    // until the stack runs out.
    let out = run_native(
        r#"BEGIN
          EXTERNAL CLASS SIMSET;
          REF (Head) towns;
          towns :- NEW Head;
          LINK CLASS town(nam_); VALUE nam_; TEXT nam_;
          BEGIN
            REF (town) PROCEDURE find(code); TEXT code;
            IF code = nam_ THEN find :- THIS town
            ELSE IF suc == NONE THEN find :- NEW town(code)
            ELSE find :- suc QUA town.find(code);
            INTO(towns);
          END;
          REF (town) r;
          r :- NEW town("A");
          r :- r.find("B");
          OutText(r.nam_); OutImage;
        END;"#,
    );
    assert_eq!(out.trim(), "B");
}

#[test]
fn resumable_class_array_bound_procedure_reads_outer_shadowed_ref() {
    // DosTestBatch simtst74: `real array X(Q:1)` makes `X` an attribute of the
    // resumable class, and Simula is case-insensitive — but `Q`'s own `x` is
    // still the outer `ref(A) x` it captured, not that attribute. The second
    // `new A` therefore sees `x` already bound.
    let out = run_native(
        r#"begin
            integer r;
            ref(A) x;
            class A;
            begin
                real array X(Q:1);
                Detach;
            end;
            integer procedure Q;
            begin
                if x =/= none then r := 1 else r := 2;
                Q := 1
            end;
            x :- new A;
            OutInt(r, 2);
            x :- new A;
            OutInt(r, 2);
            OutImage;
        end;"#,
    );
    assert_eq!(out, " 2 1\n");
}

#[test]
fn resume_detach_sequencing_driven_by_array_bound_procedure() {
    // DosTestBatch simtst74 end to end: creating an object evaluates the array
    // bound first, so `P` resumes the coroutines created earlier before the new
    // object's own body runs.
    let out = run_native(
        r#"begin
            procedure Sjekk(c); character c; OutChar(c);
            ref(A) x;
            ref(B) y;
            class A;
            begin real array X(P:1); Sjekk('A');
                  Detach; Sjekk('B');
                  Detach; Sjekk('F');
            end;
            class B;
            begin real array X(P:1); Sjekk('E');
                  Detach; Sjekk('G');
                  Detach; Sjekk('H');
            end;
            real procedure P;
            begin if x =/= none then Resume(x); Sjekk('C');
                  if y =/= none then Resume(y); Sjekk('D');
            end;
            x :- new A;
            y :- new B;
            y :- new B;
            x :- none; x :- new A;
            Resume(y);
            OutImage;
        end;"#,
    );
    assert_eq!(out.trim(), "CDABCDEFCGDECGDAH");
}

#[test]
fn nested_block_local_class_detach_resume_sequencing() {
    // simtst76 pattern: local class inside a nested begin; Resume must not
    // clobber the block-local ref via enclosing-capture writeback.
    let out = run_native(
        r#"begin
            class A;
            begin
                begin ref(C) y;
                    class C;
                    begin outtext("C"); detach; outtext("E"); end;
                    outtext("B");
                    y :- new C;
                    outtext("D");
                    resume(y);
                    outtext("F");
                end;
                outtext("G");
            end;
            outtext("A");
            ref(A) x; x :- new A;
            outtext("H");
            outimage;
        end;"#,
    );
    assert_eq!(out.trim(), "ABCDEFGH", "got {out:?}");
}

#[test]
fn this_outer_detach_attachment_chain_resume() {
    // simtst76 part2: `This A.Detach` from a nested local class must mark A so
    // the next Resume(A) continues the inner component before A's own PC.
    let out = run_native(
        r#"begin
            class A;
            begin
                outtext("A"); detach;
                begin ref(C) y;
                    class C;
                    begin
                        outtext("D"); detach;
                        outtext("F"); this A.detach;
                        outtext("H"); detach;
                        outtext("J");
                    end;
                    outtext("C"); y :- new C;
                    outtext("E"); resume(y);
                    outtext("I"); resume(y);
                    detach;
                end;
                outtext("L");
            end;
            ref(A) x; x :- new A;
            outtext("B"); resume(x);
            outtext("G"); resume(x);
            outtext("K"); resume(x);
            outtext("M");
            outimage;
        end;"#,
    );
    assert_eq!(out.trim(), "ABCDEFGHIJKLM", "got {out:?}");
}

#[test]
fn duplicate_simset_block_class_names_detach_resume() {
    // simtst76 part 1: two SIMSET blocks each declare Link Class A with nested
    // Class C, but only the first block runs here. Resume(Y) must re-enter the
    // first block's C, not the second block's span-qualified homonym (C@…).
    let out = run_native(
        r#"begin
            procedure print(t); value t; text t; outtext(t);
            simset begin
                ref(A) x;
                link class A;
                begin print("A");
                    begin ref(C) y;
                        class C;
                        begin print("C"); detach; print("E"); end;
                        print("B");
                        y :- new C;
                        print("D");
                        resume(y);
                        print("F");
                    end;
                    print("G");
                end;
                print("AA");
                x :- new A;
                print("AB");
            end;
            simset begin
                link class A;
                begin
                    class C;
                    begin detach; end;
                end;
            end;
            outimage;
        end;"#,
    );
    assert_eq!(out.trim(), "AAABCDEFGAB", "got {out:?}");
}

#[test]
fn duplicate_simset_blocks_full_simtst76_detach_resume() {
    // Full simtst76: two SIMSET blocks each with Link Class A + nested Class C,
    // homonym span qualification (`A@…`, `C@…`), and block 2 `This A.Detach`.
    let out = run_native(
        r#"begin
            boolean found_error;
            text t;
            procedure print(t); value t; text t; outtext(t);
            simset begin
                ref(A) x;
                link class A;
                begin print("A");
                    begin ref(C) y;
                        class C;
                        begin print("C"); detach; print("E"); end;
                        print("B");
                        y :- new C;
                        print("D");
                        resume(y);
                        print("F");
                    end;
                    print("G");
                end;
                print("AA");
                x :- new A;
                print("AB");
                t :- copy(sysout.image.strip);
                sysout.setpos(1);
                sysout.image := notext;
                if t = "AAABCDEFGAB" then else found_error := true;
            end;
            simset begin
                ref(A) x;
                link class A;
                begin print("A");
                    detach;
                    begin ref(C) y;
                        class C;
                        begin
                            print("D"); detach;
                            print("F"); this A.detach;
                            print("H"); detach;
                            print("J");
                        end;
                        print("C"); y :- new C;
                        print("E"); resume(y);
                        print("I"); resume(y);
                        detach;
                    end;
                    print("L");
                end;
                x :- new A;
                print("B"); resume(x);
                print("G"); resume(x);
                print("K"); resume(x);
                print("M");
                t :- copy(sysout.image.strip);
                sysout.setpos(1);
                sysout.image := notext;
                if t = "ABCDEFGHIJKLM" then else found_error := true;
            end;
            if not found_error then outtext("OK");
            outimage;
        end;"#,
    );
    assert_eq!(out.trim(), "OK", "got {out:?}");
}

#[test]
fn empty_nested_class_does_not_steal_enclosing_detach_body() {
    // simtst62: `class A;;` inside X must not swallow X's detach/resume body.
    let out = run_native(
        r#"begin
            class X; begin
                class A;;
                outtext("new X"); outimage;
                detach;
                outtext("resume X"); outimage;
            end;
            ref(X) xx; xx :- new X;
            outtext("main"); outimage;
            resume(xx);
            outtext("done"); outimage;
        end;"#,
    );
    assert_eq!(out, "new X\nmain\nresume X\ndone\n");
}

#[test]
fn formal_proc_resume_through_nested_begin_local_class() {
    // simtst62 core: formal procedure E called from local class C after
    // Resume; C and F sit in nested begin blocks whose ref locals must not
    // become enclosing captures (writeback would set them to none).
    let out = run_native(
        r#"begin
            text array seq(1:20); integer seqi;
            procedure trace(t); value t; text t;
            begin seqi := seqi + 1; seq(seqi) :- t; end;
            class X; begin
                procedure B(E); procedure E; begin
                    real pi;
                    trace("enter B");
                    begin
                        ref(C) cc;
                        class C; begin
                            trace("new C"); detach;
                            trace("resume C"); E;
                            trace("terminate C");
                        end;
                        pi := 3.14;
                        cc :- new C;
                        resume(cc);
                        pi := 2.71;
                    end;
                    trace("exit B");
                end;
                procedure E; begin
                    trace("enter E");
                    begin
                        ref(F) ff;
                        class F; begin
                            trace("new F"); detach;
                            trace("resume and exit F");
                        end;
                        ff :- new F;
                        resume(ff);
                    end;
                    trace("exit E");
                end;
                detach;
                B(E);
            end;
            ref(X) xx; xx :- new X;
            resume(xx);
            if seq(1)="enter B" and seq(2)="new C" and seq(3)="resume C"
               and seq(4)="enter E" and seq(5)="new F"
               and seq(6)="resume and exit F" and seq(7)="exit E"
               and seq(8)="terminate C" and seq(9)="exit B"
            then outtext("ok") else outtext("bad");
            outimage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn mutual_resume_forward_ref_and_nested_prefixed_resume() {
    // simtst62: Y created before xx is bound; X resumes Y which resumes xx.
    // Nested A/D prefixed compounds must split Resume as a suspend boundary
    // so PC advances; terminated resume targets must not fall through.
    let out = run_native(
        r#"begin
            text array seq(1:30); integer seqi;
            procedure trace(t); value t; text t;
            begin seqi := seqi + 1; seq(seqi) :- t; end;
            ref(Y) yy; ref(X) xx;
            class Y; begin
                trace("new Y"); detach;
                trace("resume Y"); resume(xx);
                trace("terminate Y");
            end;
            class X; begin
                class A;;
                trace("new X"); detach;
                trace("resume X");
                A begin
                    class D;;
                    D begin
                        trace("enter D");
                        resume(yy);
                        trace("terminate D");
                    end;
                end;
                trace("terminate X");
            end;
            yy :- new Y;
            xx :- new X;
            trace("resume xx");
            resume(xx);
            if seq(1)="new Y" and seq(2)="new X" and seq(3)="resume xx"
               and seq(4)="resume X" and seq(5)="enter D" and seq(6)="resume Y"
               and seq(7)="terminate D" and seq(8)="terminate X"
               and seqi=8
            then outtext("ok") else outtext("bad");
            outimage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn while_resume_mutual_does_not_recreate_infile_prologue() {
    // simtst77: alternating Resume inside while must save continuation PC so
    // `fil :- new infile` is not re-executed on each re-entry.
    let out = run_native(
        r#"begin
            integer n;
            class Co(peer); ref(Co) peer;
            begin
                integer k;
                detach;
                k := 0;
                while k < 3 do
                begin
                    k := k + 1;
                    n := n + 1;
                    resume(peer);
                end;
            end;
            ref(Co) a, b;
            a :- new Co(none);
            b :- new Co(a);
            a.peer :- b;
            resume(a);
            if n = 6 then outtext("ok") else outtext("bad");
            outimage;
        end;"#,
    );
    assert_eq!(out, "ok\n");
}

#[test]
fn disjoint_simset_same_class_name_new_uses_local_decl() {
    // Two SIMSET blocks each declare Link Class A; `new A` in each block must
    // bind to that block's A (second has detach / coro).
    let out = run_native(
        r#"begin
            SIMSET begin
                ref(A) x;
                link class A;
                begin outtext("1"); end;
                x :- new A;
            end;
            SIMSET begin
                ref(A) x;
                link class A;
                begin outtext("2"); detach; outtext("3"); end;
                x :- new A;
                outtext("4");
                resume(x);
            end;
            outimage;
        end;"#,
    );
    assert_eq!(out.trim(), "1243", "got {out:?}");
}

#[test]
fn prefixed_block_class_body_labels_get_their_own_scope() {
    // DosTestBatch simtst98: the prefix's body is inlined into the block, so a
    // label `L` declared by the class text must not collide with the enclosing
    // program's own `L` — otherwise `goto L` inside the class body jumps back
    // into the main program and loops forever.
    let out = run_native(
        r#"begin
            integer n;
            class C;
            begin
                n := n + 1;
                if n < 3 then goto L;
                OutText("c");
            L:
            end;
            C begin
                OutText("b");
            end;
            OutText("m");
            OutImage;
            goto L;
        L:
        end;"#,
    );
    assert_eq!(out, "bm\n", "got {out:?}");
}

#[test]
fn prefixed_block_runs_prefix_attribute_initializers() {
    // DosTestBatch simtst98: `integer i = 12` style attribute initializers in
    // the prefix's body must run when that body is inlined as the head of a
    // prefixed block, exactly as they do in `C$__init`.
    let out = run_native(
        r#"begin
            class C;
            begin
                integer i;
                i := 12;
            end;
            C begin
                if i = 12 then OutText("ok") else OutText("bad");
            end;
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn prefixed_block_local_procedure_hides_same_named_prefix_attribute() {
    // DosTestBatch simtst92: the prefixed block is the class's inner part, so a
    // procedure it declares hides the prefix's same-named attribute for calls
    // written in the block, while the prefix's own body keeps calling its own.
    let out = run_native(
        r#"begin
            class C;
            begin
                procedure P2; OutText("prefix ");
                P2;
            end;
            C begin
                procedure P2; OutText("block");
                P2;
            end;
            OutImage;
        end;"#,
    );
    assert_eq!(out, "prefix block\n", "got {out:?}");
}

#[test]
fn prefixed_block_nested_procedure_is_callable_with_ref_argument() {
    // DosTestBatch simtst62: `B` is a procedure of a nested block inside a
    // prefixed block rather than a §5.5 attribute of the prefix. Attribute
    // lookup must fall through to it instead of reporting an unknown
    // procedure.
    let out = run_native(
        r#"begin
            class A; begin integer i; end;
            A begin
                ref(A) E;
                procedure B(r); ref(A) r;
                begin
                    r.i := r.i + 1;
                end;
                E :- new A;
                B(E);
                B(E);
                if E.i = 2 then OutText("ok") else OutText("bad");
            end;
            OutImage;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn enclosing_ref_capture_shared_across_process_coroutine() {
    // DosTestBatch simtst96: `h :- been` inside a Process must update the
    // enclosing block's `ref(Head) h`. ObjectRef enclosing captures are held by
    // pointer (like scalars), and Cranelift reloads the addr-taken cell after
    // the transfer returns.
    let out = run_native(
        r#"begin
            simulation begin
                ref(Head) h;
                integer seen;

                process class Worker;
                begin
                    ref(Head) local;
                    local :- new Head;
                    h :- local;
                    seen := 1;
                    passivate;
                end;

                ref(Worker) w;
                w :- new Worker;
                activate w;
                if seen = 1 and h =/= none then OutText("ok") else OutText("bad");
                OutImage;
            end;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn enclosing_ref_capture_under_inspect_infile_after_wait() {
    // simtst96 shape: `inspect InFile do Simulation` plus a Process that
    // `wait`s, is resumed, then writes an enclosing `ref`. Windows fibers
    // used to leave MAIN's `h` as none after the transfer.
    let data = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/dostestbatch_data/data96");
    assert!(data.is_file(), "missing {}", data.display());
    let path = data.to_string_lossy().replace('\\', "/");
    let out = run_native(&format!(
        r#"begin
            inspect new infile("{path}") do
            simulation begin
                ref(Head) h, q;
                q :- new Head;
                process class Worker;
                begin
                    wait(q);
                    h :- new Head;
                    passivate;
                end;
                if not open(blanks(80)) then begin
                    sysout.outtext("no-open"); sysout.outimage;
                    goto done;
                end;
                activate new Worker;
                activate q.first;
                passivate;
                if h =/= none then sysout.outtext("ok") else sysout.outtext("bad");
                sysout.outimage;
                close;
            done:
            end;
        end;"#
    ));
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn enclosing_ref_capture_visible_to_peer_after_assignment() {
    // Two processes share the same enclosing `ref`; a write in one is visible
    // to the other after control returns to the block.
    let out = run_native(
        r#"begin
            simulation begin
                ref(Head) shared;

                process class Writer;
                begin
                    shared :- new Head;
                    passivate;
                end;

                process class Reader;
                begin
                    if shared =/= none then OutText("ok") else OutText("bad");
                    passivate;
                end;

                ref(Writer) w;
                ref(Reader) r;
                w :- new Writer;
                r :- new Reader;
                activate w;
                activate r;
                OutImage;
            end;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn current_nextev_sees_scheduled_process_after_hold() {
    // simtst96: `if current.nextev=/=none then passivate` must drain cars still
    // in the SQS. `current.nextev` used to lower to none, so the drain never ran.
    let out = run_native(
        r#"begin
            simulation begin
                process class Worker;
                begin
                    hold(10);
                    passivate;
                end;
                activate new Worker;
                if current.nextev =/= none then OutText("ok") else OutText("bad");
                OutImage;
            end;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn main_town_names_survive_gc_while_process_holds() {
    // simtst96 on Windows: GC while a Process fiber is running skipped MAIN's
    // parked roots, so `town.find` missed existing names and built a duplicate
    // with an empty `cars` queue.
    let out = run_native(
        r#"begin
            simulation begin
                ref (head) towns;
                link class town(nam_); value nam_; text nam_;
                begin
                    ref (town) procedure find(code); text code;
                    if code = nam_ then find :- this town
                    else if suc == none then find :- new town(code)
                    else find :- suc qua town.find(code);
                    into(towns);
                end;
                process class Worker;
                begin
                    text t; integer i;
                    hold(1);
                    for i := 1 step 1 until 1200 do t :- copy("xxxxxxxxxxxxxxxx");
                    passivate;
                end;
                ref (town) r;
                towns :- new head;
                r :- new town("VESTBY");
                r :- new town("SAND");
                activate new Worker;
                hold(2);
                r :- towns.first qua town.find("VESTBY");
                if towns.cardinal = 2 and r.nam_ = "VESTBY" then OutText("ok") else OutText("bad");
                OutImage;
            end;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn process_ref_field_survives_hold() {
    // simtst96 Windows: after `hold`, `into(been)` saw none while the car
    // object's been slot was still a Head — the local was in a register the
    // fiber C call clobbered.
    let out = run_native(
        r#"begin
            simulation begin
                process class Worker;
                begin
                    ref (head) been;
                    been :- new head;
                    hold(1);
                    if been =/= none then OutText("ok") else OutText("bad");
                    OutImage;
                    passivate;
                end;
                activate new Worker;
                hold(2);
            end;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}

#[test]
fn main_text_survives_hold() {
    // simtst96 Windows: MAIN's second InImage name became `TBY` (VESTBY
    // minus the first three letters) after a passivate/hold drain.
    let out = run_native(
        r#"begin
            simulation begin
                text t;
                t :- Copy("VESTBY");
                hold(1);
                if t = "VESTBY" then OutText("ok") else OutText("bad");
                OutImage;
            end;
        end;"#,
    );
    assert_eq!(out, "ok\n", "got {out:?}");
}
