//! Integration coverage for Phase 5 MVP objects through MIR → Cranelift:
//! flat classes with integer attributes, `ref`/`none`/`new`/`:-`,
//! remote integer field load/store, and simple method calls (`obj.m(...)`).
//! Native stdout is checked against the interpreter oracle, matching
//! `tests/mir_arrays.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-mir-objects-{tag}-{id}"))
}

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

fn run_interpreted(source: &str) -> String {
    outimage::compile_str(source)
        .unwrap_or_else(|error| panic!("interpreter failed for {source:?}: {error}"))
}

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

/// Native-only success check for cases the interpreter cannot lower yet
/// (e.g. `ref` actuals that are expressions, formal procedure parameters).
fn assert_native_ok(source: &str) {
    let (native, success) = run_native(source);
    assert!(
        success,
        "native binary for {source:?} exited unsuccessfully; stdout={native}"
    );
    let upper = native.to_ascii_uppercase();
    assert!(
        upper.contains("OK") && !upper.contains("FAIL") && !upper.contains("*** ERROR"),
        "unexpected native stdout for {source:?}: {native}"
    );
}

fn assert_aborts_on_none_access(source: &str) {
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );
    assert_eq!(
        stdout, "",
        "no output should be printed after none-deref: {source:?}"
    );

    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to also reject none remote access in {source:?}, got {interpreted:?}"
    );
}

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
fn new_then_field_store_and_load() {
    assert_matches_interpreter(
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
fn none_assignment_then_new() {
    assert_matches_interpreter(
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
fn two_refs_alias_same_object() {
    assert_matches_interpreter(
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
fn two_independent_new_objects() {
    assert_matches_interpreter(
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
fn default_field_is_zero() {
    assert_matches_interpreter(
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
fn nested_begin_with_class() {
    assert_matches_interpreter(
        r#"begin
            begin
                class Point; begin integer x, y; end;
                ref(Point) p;
                p :- new Point;
                p.x := 4;
                p.y := 5;
                if p.x = 4 and p.y = 5 then OutText("ok") else OutText("bad");
                OutImage;
            end;
        end;"#,
    );
}

#[test]
fn overwrite_field_keeps_latest_value() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- new Point;
            p.x := 1;
            p.x := 9;
            if p.x = 9 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn field_in_while_condition() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin integer n; end;
            ref(Counter) c;
            c :- new Counter;
            c.n := 3;
            while c.n > 0 do begin
                OutText(".");
                OutImage;
                c.n := c.n - 1;
            end;
            OutText("done");
            OutImage;
        end;"#,
    );
}

#[test]
fn access_through_none_aborts() {
    assert_aborts_on_none_access(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- none;
            p.x := 1;
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn load_through_none_aborts() {
    assert_aborts_on_none_access(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            integer v;
            p :- none;
            v := p.x;
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn counter_increment_and_get_methods() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure increment; begin n := n + 1; end;
                integer procedure get; begin get := n; end;
            end;
            ref(Counter) c;
            integer v;
            c :- new Counter;
            c.increment();
            c.increment();
            v := c.get();
            if v = 2 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_with_value_parameter() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure add(k); value k; integer k; begin n := n + k; end;
                integer procedure get; begin get := n; end;
            end;
            ref(Counter) c;
            integer v;
            c :- new Counter;
            c.add(5);
            c.add(7);
            v := c.get();
            if v = 12 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_call_in_if_condition() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure increment; begin n := n + 1; end;
                integer procedure get; begin get := n; end;
            end;
            ref(Counter) c;
            c :- new Counter;
            c.increment();
            if c.get() = 1 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_call_through_none_aborts() {
    assert_aborts_on_none_access(
        r#"begin
            class Counter; begin
                integer n;
                procedure increment; begin n := n + 1; end;
            end;
            ref(Counter) c;
            c :- none;
            c.increment();
            OutText("unreachable");
            OutImage;
        end;"#,
    );
}

#[test]
fn bare_parameterless_method_call_matches_interpreter() {
    assert_matches_interpreter(
        r#"begin
            class C; begin
                integer x;
                procedure p; begin x := 1; end;
                integer procedure get; begin get := x; end;
            end;
            ref(C) r;
            integer v;
            r :- new C;
            r.p;
            v := r.get;
            if v = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn bare_method_with_params_still_requires_call() {
    assert_compile_error_contains(
        r#"begin
            class C; begin
                procedure add(integer k); begin end;
            end;
            ref(C) r;
            r :- new C;
            r.add;
        end;"#,
        "requires arguments",
    );
}

#[test]
fn unqualified_sibling_method_call_in_method_body() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure bump; begin n := n + 1; end;
                procedure twice; begin
                    bump();
                    bump();
                end;
            end;
            ref(Counter) c;
            c :- new Counter;
            c.twice();
            if c.n = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn boolean_field_store_and_load() {
    assert_matches_interpreter(
        r#"begin
            class Flags; begin boolean on; end;
            ref(Flags) f;
            f :- new Flags;
            f.on := true;
            if f.on then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn prefix_class_inherits_integer_fields() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x, y; end;
            Point class Polar; begin integer r; end;
            ref(Polar) p;
            p :- new Polar;
            p.x := 3;
            p.y := 4;
            p.r := 5;
            if p.x = 3 and p.y = 4 and p.r = 5 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn text_field_store_load_outtext() {
    assert_matches_interpreter(
        r#"begin
            class Box; begin text t; end;
            ref(Box) p;
            p :- new Box;
            p.t :- copy("hi");
            OutText(p.t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_field_value_assign() {
    assert_matches_interpreter(
        r#"begin
            class Box; begin text t; end;
            ref(Box) p;
            p :- new Box;
            p.t :- blanks(2);
            p.t := "hi";
            OutText(p.t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_field_in_method_body() {
    assert_matches_interpreter(
        r#"begin
            class Box; begin
                text t;
                procedure set; begin t :- copy("ok"); end;
            end;
            ref(Box) p;
            p :- new Box;
            p.set;
            OutText(p.t);
            OutImage;
        end;"#,
    );
}

#[test]
fn split_body_prefix_initial_then_main_final() {
    assert_matches_interpreter(
        r#"begin
            class Prefix;
            begin integer a; a := 1; end;
            Prefix class Main;
            begin integer b; b := 10; inner; b := 100; end;
            ref(Main) m;
            m :- new Main;
            if m.a = 1 and m.b = 100 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn real_field_round_trip_match_interpreter() {
    assert_matches_interpreter(
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
fn real_constructor_param_match_interpreter() {
    assert_matches_interpreter(
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
fn virtual_method_dispatch_uses_runtime_class() {
    assert_matches_interpreter(
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
fn virtual_method_base_instance_keeps_base() {
    assert_matches_interpreter(
        r#"begin
            class Base; virtual: integer procedure f;
            begin integer procedure f; begin f := 1; end; end;
            Base class Derived;
            begin integer procedure f; begin f := 2; end; end;
            ref(Base) p;
            p :- new Base;
            if p.f() = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_uses_this_for_ref_assignment_and_bare_field_store() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin
                integer x;
                procedure init; begin
                    ref(Point) self;
                    self :- this Point;
                    x := 42;
                end;
            end;
            ref(Point) p;
            p :- new Point;
            p.init();
            if p.x = 42 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_this_ref_then_bare_field_increment() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure inc; begin
                    ref(Counter) c;
                    c :- this Counter;
                    n := n + 1;
                end;
                integer procedure get; begin get := n; end;
            end;
            ref(Counter) c;
            c :- new Counter;
            c.n := 5;
            c.inc();
            if c.get() = 6 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_reads_field_through_this_remote_access() {
    assert_matches_interpreter(
        r#"begin
            class Counter; begin
                integer n;
                procedure bump; begin
                    integer v;
                    v := this Counter.n;
                    n := v + 1;
                end;
                integer procedure get; begin get := n; end;
            end;
            ref(Counter) c;
            c :- new Counter;
            c.n := 5;
            c.bump();
            if c.get() = 6 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn this_outside_method_errors_clearly() {
    assert_compile_error_contains(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- this Point;
        end;"#,
        "outside object context",
    );
}

#[test]
fn this_wrong_class_errors_clearly() {
    assert_compile_error_contains(
        r#"begin
            class Point; begin
                integer x;
                procedure bad; begin
                    ref(Other) q;
                    q :- this Other;
                end;
            end;
            class Other; begin integer y; end;
        end;"#,
        "prefix",
    );
}

#[test]
fn qua_same_class_is_identity() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- new Point;
            p.x := 7;
            q :- p qua Point;
            if q.x = 7 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn qua_none_stays_none() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- none;
            q :- p qua Point;
            if q = none then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn qua_prefix_upcast() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            Point class Polar; begin integer r; end;
            ref(Polar) p;
            ref(Point) q;
            p :- new Polar;
            p.x := 9;
            q :- p qua Point;
            if q.x = 9 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn qua_mismatch_errors_clearly() {
    assert_compile_error_contains(
        r#"begin
            class Point; begin integer x; end;
            class Other; begin integer y; end;
            ref(Point) p;
            ref(Other) q;
            p :- new Point;
            q :- p qua Other;
        end;"#,
        "cannot be qualified",
    );
}

#[test]
fn class_body_init_assigns_fields() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin
                integer x;
                x := 42;
            end;
            ref(Point) p;
            p :- new Point;
            if p.x = 42 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn class_body_init_with_prefix_runs_both() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin
                integer x;
                x := 1;
            end;
            Point class Polar; begin
                integer r;
                r := 2;
            end;
            ref(Polar) p;
            p :- new Polar;
            if p.x = 1 and p.r = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn reference_equality_same_object() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- new Point;
            q :- p;
            if p == q then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn reference_inequality_distinct_objects() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- new Point;
            q :- new Point;
            if p =/= q then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn none_reference_equality() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q, r;
            p :- none;
            q :- none;
            r :- new Point;
            if p == q and p =/= r then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn is_relation_exact_class() {
    assert_matches_interpreter(include_str!("fixtures/expressions/is_relation.sim"));
}

#[test]
fn in_relation_prefix_chain() {
    assert_matches_interpreter(include_str!("fixtures/expressions/in_relation.sim"));
}

#[test]
fn is_relation_ignores_qua_qualification() {
    assert_matches_interpreter(
        r#"begin
            class Point; begin integer x; end;
            Point class Polar; begin integer r; end;
            ref(Polar) p;
            ref(Point) q;
            p :- new Polar;
            q :- p qua Point;
            if q is Polar and not (q is Point) and q in Point then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn inspect_when_matches_class() {
    assert_matches_interpreter(
        r#"begin
            class Node;
            begin
            end Node;
            integer picked;
            ref(Node) n;
            n :- new Node;
            picked := 0;
            inspect n when Node do picked := 1;
            if picked = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn inspect_otherwise_on_none() {
    assert_matches_interpreter(
        r#"begin
            integer picked;
            picked := 99;
            inspect none when Node do picked := 1 otherwise picked := 0;
            if picked = 0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn inspect_do_clause_on_non_none() {
    assert_matches_interpreter(
        r#"begin
            class Node; begin end;
            integer flag;
            ref(Node) n;
            n :- new Node;
            flag := 0;
            inspect n do flag := 1;
            if flag = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn inspect_do_writes_connected_attribute() {
    assert_matches_interpreter(
        r#"begin
            class Node;
            integer x;
            begin
                x := 1;
            end;
            ref(Node) n;
            n :- new Node;
            inspect n do x := 2;
            if n.x = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn nested_inspect_restores_outer_connection() {
    assert_matches_interpreter(
        r#"begin
            class OuterCls;
            integer x;
            begin
                x := 1;
            end;
            class InnerCls;
            integer y;
            begin
                y := 2;
            end;
            ref(OuterCls) o;
            ref(InnerCls) i;
            integer seen;
            o :- new OuterCls;
            i :- new InnerCls;
            inspect o do
            begin
                inspect i do seen := y;
                seen := x;
            end;
            if seen = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn constructor_param_binds_attribute() {
    assert_matches_interpreter(
        r#"begin
            class Point(x); integer x; begin end;
            ref(Point) p;
            p :- new Point(7);
            if p.x = 7 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn constructor_multiple_params() {
    assert_matches_interpreter(
        r#"begin
            class Point(x, y); integer x, y; begin end;
            ref(Point) p;
            p :- new Point(3, 4);
            if p.x = 3 and p.y = 4 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn constructor_body_overwrites_param() {
    assert_matches_interpreter(
        r#"begin
            class Point(x); integer x; begin
                x := x + 1;
            end;
            ref(Point) p;
            p :- new Point(7);
            if p.x = 8 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn this_in_type_procedure_writes_enclosing_refs() {
    // simtst47: `This C` inside a method must update enclosing ref locals
    // via capture refresh/writeback, and the function result must be the object.
    assert_matches_interpreter(
        r#"begin
            ref(A) ra1, ra2, ra3;
            class A;
            begin
               ref(A) procedure Z;
               begin
                  ra2 :- This A;
                  begin integer i; ra3 :- This A end;
                  Z :- This A
               end;
            end;
            ra1 :- new A;
            ra1 :- ra1.Z;
            if ra1 == ra2 and ra2 == ra3 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn function_result_ref_qual_is_static() {
    // simtst46: `rP` returns `ref(A)`; `.iP` must use A's procedure, not B's.
    assert_native_ok(
        r#"begin
            class A; begin integer procedure iP; iP := 65; end;
            A class B; begin integer procedure iP; iP := 66; end;
            ref(B) rb;
            integer ia, ib;
            ref(A) procedure rP(ra); ref(A) ra; rP :- ra;
            rb :- new B;
            ia := rP(new A).iP;
            ib := rP(rb).iP;
            if ia = ib and ia = 65 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn long_real_enclosing_capture_roundtrips_through_inner() {
    // simtst52: LONG REAL enclosing locals must snapshot/writeback like REAL.
    assert_matches_interpreter(
        r#"begin
            long real a;
            real b;
            class C;
            begin
               long real d;
               d := a + b;
               inner;
               b := a + b + d
            end;
            a := b := 5.45;
            begin ref(C) g; g :- new C; end;
            if b > 21.7 and b < 21.9 and a > 5.44 and a < 5.46 then
               OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn protected_subclass_attr_falls_through_to_prefix() {
    // simtst60: remote `xb.i` skips protected B.i and writes A's i.
    assert_matches_interpreter(
        r#"begin
            class A;
            begin
               integer i;
               integer procedure vai; vai := i;
            end;
            A class B;
               protected i;
            begin
               integer i;
               integer procedure vbi; vbi := i;
            end;
            ref(B) xb;
            xb :- new B;
            xb.i := 5;
            if xb.vai = 5 and xb.vbi = 0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn protected_subclass_attr_visible_inside_class() {
    // simtst61: from inside B, remote `x.i` is B's protected i (not A's).
    assert_native_ok(
        r#"begin
            class A;
            begin
               integer i;
               integer procedure vai; vai := i;
            end;
            A class B;
               protected i;
            begin
               integer i;
               integer procedure vbi; vbi := i;
               procedure p; x.i := 1;
            end;
            ref(B) x;
            x :- new B;
            x.p;
            x.i := 2;
            if x.vai = 2 and x.vbi = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn virtual_name_param_matching_procedure() {
    // simtst56/75: name-parameter formal procedure + virtual Q.
    assert_native_ok(
        r#"begin
            ref(A) x;
            real ar, br;
            class A;
               virtual: real procedure Q;
            begin
               real procedure Q; Q := 2.5;
               procedure T(R); name R; real R;
               begin ar := R * Q end;
            end;
            procedure S(P, B); name P, B; procedure P; real B;
            begin P(x.Q); br := B * x.Q end;
            A class B;
            begin real procedure Q; Q := 2; end;
            x :- new B;
            S(x.T, x.Q);
            if ar > 3.9 and ar < 4.1 and br > 3.9 and br < 4.1 then
               OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn constructor_wrong_arity_errors_clearly() {
    assert_compile_error_contains(
        r#"begin
            class Point(x); integer x; begin end;
            ref(Point) p;
            p :- new Point(1, 2);
        end;"#,
        "expects 1 parameters",
    );
    assert_compile_error_contains(
        r#"begin
            class Point(x, y); integer x, y; begin end;
            ref(Point) p;
            p :- new Point(1);
        end;"#,
        "expects 2 parameters",
    );
}

#[test]
fn constructor_prefix_params_both_levels() {
    assert_matches_interpreter(
        r#"begin
            class Point(x); integer x; begin end;
            Point class Polar(r); integer r; begin end;
            ref(Polar) p;
            p :- new Polar(3, 5);
            if p.x = 3 and p.r = 5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn hidden_attribute_peels_one_binding_for_subclasses() {
    // simtst98 (§5.5.6): `hidden i` in `b` removes `a`'s protected `i` from the
    // attributes its subclasses see, so `c`'s text binds the *next* outer `i` —
    // here the enclosing block's. `a`'s own text keeps seeing `a.i`.
    assert_native_ok(
        r#"begin
            integer i;
            integer fromA, fromC;

            class a;
               protected i;
            begin
               integer i;
               procedure reada; fromA := i;
               i := 12;
            end;

            a class b;
               hidden i;
            begin end;

            b class c;
            begin
               procedure readc; fromC := i;
            end;

            ref(c) x;
            i := 7;
            x :- new c;
            x.reada;
            x.readc;
            if fromA = 12 and fromC = 7 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn hidden_virtual_stops_further_matching() {
    // simtst98 (§5.5.6): `hidden virtproc` in `b` freezes virtual matching, so
    // `c`'s same-named procedure is an ordinary attribute and must not become
    // the match. Calls through `a`'s virtual keep the last legal match.
    assert_native_ok(
        r#"begin
            text seen;

            class a;
               protected virtproc;
               virtual: procedure virtproc;
            begin
               procedure virtproc; seen :- Copy("a");
               procedure callvirt; virtproc;
            end;

            a class b;
               hidden virtproc;
            begin end;

            b class c;
            begin
               procedure virtproc; seen :- Copy("c");
            end;

            ref(c) x;
            x :- new c;
            x.callvirt;
            if seen = "a" then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn virtual_unmatched_at_declaring_class_dispatches_to_subclass() {
    // simtst55: `A` declares `virtual: procedure P` but never matches it, so
    // the static qualification has no match level. The call must still reach
    // the subclass that does declare `P` instead of being rejected as a field.
    assert_native_ok(
        r#"begin
            integer hits;
            class A;
               virtual: procedure P;
            begin end;
            A class B;
            begin
               procedure P; hits := hits + 1;
            end;
            ref(A) rA;
            ref(B) rB;
            rB :- new B;
            rA :- rB;
            rA.P;
            rB qua A.P;
            if hits = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn virtual_unmatched_at_declaring_class_picks_runtime_class() {
    // simtst57: two sibling subclasses match the same unmatched virtual with
    // different signatures; dispatch follows the runtime class of the object.
    assert_native_ok(
        r#"begin
            text seen;
            class A;
               virtual: procedure Emit;
            begin end;
            A class AA;
            begin
               procedure Emit; seen :- Copy("AA");
            end;
            A class AB;
            begin
               procedure Emit; seen :- Copy("AB");
            end;
            ref(A) rA;
            rA :- new AA;
            rA.Emit;
            if seen = "AA" then
            begin
               rA :- new AB;
               rA.Emit;
               if seen = "AB" then OutText("ok") else OutText("fail");
            end
            else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_formal_shadows_same_named_enclosing_capture() {
    // simtst96: the block declares `ref(Head) h` and class `Node` therefore
    // carries an `h` enclosing capture. Method `put(h)`'s own formal is a
    // different variable, so creating a `Node` inside `put` must not write the
    // instance's (stale) capture slot back over the argument.
    assert_native_ok(
        r#"begin
            class Head; begin integer count; end;
            ref(Head) h;

            class Node(n); integer n;
            begin
               procedure put(h); ref(Head) h;
               begin
                  ref(Node) fresh;
                  fresh :- new Node(n);
                  h.count := h.count + 1;
               end;
            end;

            ref(Head) target;
            ref(Node) node;
            target :- new Head;
            node :- new Node(1);
            node.put(target);
            node.put(target);
            if target.count = 2 and h == none then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn method_formal_shadowing_capture_survives_recursion() {
    // simtst96 shape: `put` recurses through `inspect suc when Node do put(h)`;
    // the formal must keep pointing at the caller's list across the recursion.
    assert_native_ok(
        r#"begin
            simset begin
               ref(Head) h;
               integer copied;

               Link class Node(n); integer n;
               begin
                  procedure put(h); ref(Head) h;
                  begin
                     new Node(n).Into(h);
                     inspect Suc when Node do put(h);
                  end;
               end;

               ref(Head) source, target;
               ref(Link) walk;
               source :- new Head;
               target :- new Head;
               new Node(1).Into(source);
               new Node(2).Into(source);
               new Node(3).Into(source);

               inspect source.First when Node do put(target);

               walk :- target.First;
               while walk =/= none do
               begin
                  copied := copied + 1;
                  walk :- walk.Suc;
               end;
               if copied = 3 and h == none then OutText("ok") else OutText("fail");
               OutImage;
            end;
        end;"#,
    );
}
