//! Integration coverage for `text` through the MIR → Cranelift native backend:
//! compiles small programs to real executables, runs
//! them, and checks their stdout against the interpreter (the semantics
//! oracle) — mirroring `tests/mir_arrays.rs` and `tests/mir_native.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-mir-text-{tag}-{id}"))
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

#[test]
fn string_literal_out_text_still_works() {
    assert_matches_interpreter(r#"begin OutText("hello"); OutImage; end;"#);
}

#[test]
fn declared_text_variable_defaults_to_notext() {
    assert_matches_interpreter(
        r#"begin
            text t;
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_variable_holds_string_literal_assignment() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t := "hello";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_declaration_with_initializer() {
    assert_matches_interpreter(
        r#"begin
            text t := "hi there";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn outtext_accepts_notext_keyword() {
    assert_matches_interpreter(r#"begin OutText(notext); OutImage; end;"#);
}

#[test]
fn text_concat_two_literals() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t := "hel" & "lo";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_concat_with_variable_operands() {
    assert_matches_interpreter(
        r#"begin
            text left, right, t;
            left := "foo";
            right := "bar";
            t := left & right;
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_concat_with_notext_operand() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t := notext & "only-right";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn text_concat_both_notext_is_empty() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t := notext & notext;
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn empty_string_literal_is_notext() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t := "";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn assignment_between_text_variables() {
    assert_matches_interpreter(
        r#"begin
            text t, s;
            t := "copied";
            s := t;
            OutText(s);
            OutImage;
        end;"#,
    );
}

#[test]
fn outtext_with_concat_expression_directly() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a := "x";
            b := "y";
            OutText(a & b);
            OutImage;
        end;"#,
    );
}

#[test]
fn while_loop_prints_concat_each_iteration() {
    // `:=` into an already-filled text frame cannot grow past its length
    // (interpreter oracle rejects that too), so exercise concat + OutText
    // inside a loop without reassigning a growing string into a fixed frame.
    assert_matches_interpreter(
        r#"begin
            text piece;
            integer i;
            piece := ".";
            i := 0;
            while i < 3 do begin
                OutText(piece & piece);
                OutImage;
                i := i + 1;
            end;
        end;"#,
    );
}

#[test]
fn nested_block_text_variable() {
    assert_matches_interpreter(
        r#"begin
            begin
                text t;
                t := "inner";
                OutText(t);
                OutImage;
            end;
        end;"#,
    );
}

#[test]
fn two_text_variables_do_not_alias() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a := "aaa";
            b := "bb";
            OutText(a);
            OutImage;
            OutText(b);
            OutImage;
        end;"#,
    );
}

#[test]
fn blanks_zero_is_notext_out_text() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- blanks(0);
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn blanks_three_spaces_out_text() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- blanks(3);
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn copy_of_notext() {
    assert_matches_interpreter(
        r#"begin
            text t, u;
            t :- notext;
            u :- copy(t);
            OutText(u);
            OutImage;
        end;"#,
    );
}

#[test]
fn copy_of_literal() {
    assert_matches_interpreter(
        r#"begin
            text t, u;
            t :- "hello";
            u :- copy(t);
            OutText(u);
            OutImage;
        end;"#,
    );
}

#[test]
fn content_equality_same_chars_different_frames() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a :- "xy";
            b :- copy(a);
            if a = b then OutText("eq") else OutText("ne");
            OutImage;
        end;"#,
    );
}

#[test]
fn text_ranking_relations_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            text a, b, c;
            a :- "a";
            b :- "b";
            c :- "aa";
            if a < b then OutText("lt") else OutText("bad");
            OutImage;
            if b > a then OutText("gt") else OutText("bad");
            OutImage;
            if a <= a and a >= a then OutText("eq") else OutText("bad");
            OutImage;
            if a < c then OutText("prefix") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn reference_assignment_shares_mutable_frame() {
    // `u :- t` shares the blanks buffer; assigning into `u` mutates `t`.
    assert_matches_interpreter(
        r#"begin
            text t, u;
            t :- blanks(5);
            u :- t;
            u := "x";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn copy_creates_independent_frame() {
    assert_matches_interpreter(
        r#"begin
            text t, u;
            t :- blanks(5);
            u :- copy(t);
            u := "x";
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn notext_equals_notext() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a :- notext;
            b :- notext;
            if a = b then OutText("eq") else OutText("ne");
            OutImage;
        end;"#,
    );
}

#[test]
fn unequal_lengths_are_not_equal() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a :- "ab";
            b :- "a";
            if a = b then OutText("eq") else OutText("ne");
            OutImage;
            if a <> b then OutText("diff") else OutText("same");
            OutImage;
        end;"#,
    );
}

#[test]
fn blanks_via_integer_expression() {
    assert_matches_interpreter(
        r#"begin
            text t;
            integer n;
            n := 2 + 2;
            t :- blanks(n);
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn nested_concat_with_blanks() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t := "a" & blanks(2) & "b";
            OutText(t);
            OutImage;
        end;"#,
    );
}

// --- Text attributes: length / pos / setpos / more / getchar ---------------

fn assert_aborts_on_getchar_past_end(source: &str) {
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );

    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to also reject getchar past end in {source:?}, got {interpreted:?}"
    );
}

#[test]
fn length_of_literal() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- "hello";
            if t.length = 5 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn length_of_notext_is_zero() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- notext;
            if t.length = 0 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn notext_attributes_match_standard() {
    assert_matches_interpreter(
        r#"begin
            text t;
            if t.constant and t.start = 1 and t.length = 0 and t.pos = 1
               and t = notext and t.main.start = 1 and t.main.length = 0 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn text_ref_assign_copies_source_pos() {
    assert_matches_interpreter(
        r#"begin
            text t1, t2;
            t1 :- copy("abcd");
            t1.setpos(3);
            t2 :- t1;
            if t1.pos = 3 and t2.pos = 3 and t1 == t2 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn text_value_assign_preserves_pos() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- copy("abcd");
            t := "123";
            if t = "123 " and t.pos = 1 and t.length = 4 and not t.constant then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn chained_text_sub_and_constant_on_result() {
    assert_matches_interpreter(
        r#"begin
            text t1, t2;
            t1 :- copy("abcdef");
            t2 :- t1.sub(3, 4);
            if t1.sub(2, 4).sub(2, 3) == t1.sub(3, 3)
               and t2.main.sub(t2.start, t2.length) == t2
               and t2.sub(2, 0).constant then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn blanks_zero_is_constant_notext() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- blanks(0);
            if t == notext and t.constant and t.start = 1 and t.pos = 1 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn class_value_text_param_is_mutable_copy() {
    assert_matches_interpreter(
        r#"begin
            class A(t); value t; text t;;
            ref(A) r;
            boolean ok;
            r :- new A("hi");
            r.t := "xy";
            ok := r.t = "xy";
            if ok then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn pos_defaults_to_one() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- "ab";
            if t.pos = 1 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn setpos_clamps_high_more_false() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- "ab";
            t.setpos(99);
            if t.pos = 3 and not t.more then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn setpos_one_on_ab_more_true() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- "ab";
            t.setpos(1);
            if t.more then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn getchar_advances_pos() {
    // Character is lowered as i64 codepoint; compare with a character literal
    // and check that `pos` advanced.
    assert_matches_interpreter(
        r#"begin
            text t;
            character c;
            t :- "ab";
            t.setpos(1);
            c := t.getchar;
            if c = 'a' and t.pos = 2 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn getchar_compare_character_literal() {
    assert_matches_interpreter(
        r#"begin
            text t;
            character c1, c2;
            t :- "ab";
            t.setpos(1);
            c1 := t.getchar;
            c2 := t.getchar;
            if c1 = 'a' and c2 = 'b' and not t.more then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn getchar_past_end_aborts() {
    assert_aborts_on_getchar_past_end(
        r#"begin
            text t;
            character c;
            t :- "a";
            t.setpos(1);
            c := t.getchar;
            c := t.getchar;
        end;"#,
    );
}

#[test]
fn attributes_combine_with_blanks_and_copy() {
    assert_matches_interpreter(
        r#"begin
            text t, u;
            t :- blanks(3);
            u :- copy(t);
            t.setpos(2);
            if t.length = 3 and u.length = 3 and t.more and u.pos = 1 then
                OutText("ok")
            else
                OutText("fail");
            OutImage;
        end;"#,
    );
}

// --- Text sub / strip --------------------------------------------------------

fn assert_aborts_on_sub_out_of_frame(source: &str) {
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );

    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to also reject sub out of frame in {source:?}, got {interpreted:?}"
    );
}

#[test]
fn sub_and_strip_fixture() {
    assert_matches_interpreter(
        r#"begin
            text t, sub, stripped;
            t :- "abc   ";
            sub :- t.sub(2, 2);
            stripped :- t.strip;
            if sub = "bc" and stripped = "abc" then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn sub_zero_length_yields_notext() {
    assert_matches_interpreter(
        r#"begin
            text t, sub;
            t :- "abc";
            sub :- t.sub(1, 0);
            if sub = notext then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn strip_on_notext_is_notext() {
    assert_matches_interpreter(
        r#"begin
            text t, stripped;
            stripped :- t.strip;
            if stripped = notext then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn strip_as_procedure_call() {
    assert_matches_interpreter(
        r#"begin
            text t, stripped;
            t :- "xy  ";
            stripped :- t.strip();
            if stripped = "xy" then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn sub_out_of_frame_aborts() {
    assert_aborts_on_sub_out_of_frame(
        r#"begin
            text t, sub;
            t :- "a";
            sub :- t.sub(2, 2);
        end;"#,
    );
}

#[test]
fn sub_on_nested_frame() {
    assert_matches_interpreter(
        r#"begin
            text t, mid, piece;
            t :- "abcdef";
            mid :- t.sub(2, 3);
            piece :- mid.sub(2, 1);
            if piece = "c" then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

// --- Text deedit/edit: getint / putint --------------------------------------

fn assert_aborts_on_getint_no_numeric_item(source: &str) {
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );

    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to also reject getint with no numeric item in {source:?}, got {interpreted:?}"
    );
}

#[test]
fn getint_parses_leading_integer_with_blanks() {
    assert_matches_interpreter(
        r#"begin
            text amount;
            integer pay;
            amount :- " 1200";
            pay := amount.getint;
            if pay = 1200 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn getint_as_procedure_call() {
    assert_matches_interpreter(
        r#"begin
            text amount;
            integer pay;
            amount :- " 42";
            pay := amount.getint();
            if pay = 42 then OutText("ok") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn putint_right_aligns_into_blanks_frame() {
    assert_matches_interpreter(
        r#"begin
            text payment;
            integer pay;
            pay := 186900;
            payment :- blanks(12);
            payment.putint(pay);
            OutText(payment);
            OutImage;
        end;"#,
    );
}

#[test]
fn getint_and_putint_round_trip() {
    // Subset of `deedit_edit.sim` without getfrac/putfrac.
    assert_matches_interpreter(
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
fn getfrac_and_putfrac_match_interpreter() {
    assert_matches_interpreter(
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
fn getfrac_stops_before_spaced_decimal_mark() {
    // Simula GROUPED-ITEM: blanks before DECIMAL-MARK end the integer groups
    // (DosTestBatch simtst17).
    assert_matches_interpreter(
        r#"begin
            text t1; integer i, j;
            t1 :- copy("12 3 45 . 67");
            i := t1.getfrac;
            t1.putchar('0');
            j := t1.getfrac;
            if i = 12345 and j = 123450 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn putfrac_omits_zero_whole_and_groups_fraction() {
    let (stdout, ok) = run_native(
        r#"begin
                text t;
                t :- blanks(20);
                t.putfrac(1234567, 7);
                OutText(t);
                OutImage;
            end;"#,
    );
    assert!(ok, "native putfrac failed: {stdout}");
    assert_eq!(stdout.trim(), ".123 456 7");
}

#[test]
fn putreal_signed_exponent_matches_standard_form() {
    let (stdout, ok) = run_native(
        r#"begin
                text t;
                t :- blanks(30);
                t.putreal(123456, 7);
                OutText(t);
                OutImage;
            end;"#,
    );
    assert!(ok, "native putreal failed: {stdout}");
    assert_eq!(stdout.trim(), "1.234560&+05");
}

#[test]
fn getint_sign_part_blanks_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            text t1; integer i;
            t1 :- blanks(8);
            t1 := notext;
            t1.setpos(2);
            t1.putchar('+');
            t1.setpos(4);
            t1.putchar('2');
            t1.putchar('4');
            t1.putchar(' ');
            t1.putchar('2');
            i := t1.getint;
            if i = 24 and t1.pos = 6 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn deedit_edit_fixture_matches_interpreter() {
    let source = include_str!("fixtures/text_attributes/deedit_edit.sim");
    assert_matches_interpreter(source);
}

#[test]
fn getint_on_notext_aborts() {
    assert_aborts_on_getint_no_numeric_item(
        r#"begin
            text t;
            integer n;
            n := t.getint;
        end;"#,
    );
}

#[test]
fn upcase_and_lowcase_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- copy("AbC");
            upcase(t);
            if t = "ABC" then OutText("up") else OutText("fail");
            OutImage;
            lowcase(t);
            if t = "abc" then OutText("low") else OutText("fail");
            OutImage;
        end;"#,
    );
}

#[test]
fn upcase_on_literal_aborts() {
    let source = r#"begin
        text t;
        t :- "abc";
        upcase(t);
    end;"#;
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );
}

#[test]
fn text_reference_equality_after_ref_assign() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a :- copy("hi");
            b :- a;
            if a == b then OutText("same") else OutText("diff");
            OutImage;
        end;"#,
    );
}

#[test]
fn text_reference_inequality_distinct_copies() {
    assert_matches_interpreter(
        r#"begin
            text a, b;
            a :- copy("hi");
            b :- copy("hi");
            if a =/= b then OutText("diff") else OutText("same");
            OutImage;
        end;"#,
    );
}

#[test]
fn putint_on_literal_aborts() {
    let source = r#"begin
        text t;
        t :- "abc";
        t.putint(1);
    end;"#;
    let (stdout, success) = run_native(source);
    assert!(
        !success,
        "expected the native binary to abort for {source:?}, stdout was {stdout:?}"
    );
    let interpreted = outimage::compile_str(source);
    assert!(
        interpreted.is_err(),
        "expected the interpreter to reject putint on constant text in {source:?}, got {interpreted:?}"
    );
}

#[test]
fn getreal_and_putfix_match_interpreter() {
    assert_matches_interpreter(
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
fn putreal_scientific_form_match_interpreter() {
    assert_matches_interpreter(
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
fn text_constant_start_main_match_interpreter() {
    assert_matches_interpreter(include_str!("fixtures/text_attributes/text_metadata.sim"));
}

#[test]
fn blanks_copy_main_identity_match_interpreter() {
    assert_matches_interpreter(include_str!("fixtures/text_attributes/blanks_copy.sim"));
}

#[test]
fn putchar_match_interpreter() {
    assert_matches_interpreter(
        r#"begin
            text t;
            t :- blanks(3);
            t.setpos(1);
            t.putchar('A');
            t.putchar('B');
            OutText(t.strip);
            OutImage;
        end;"#,
    );
}

#[test]
fn paren_putchar_does_not_update_variable_pos() {
    // Expression receivers share the character object but keep an independent
    // POS — `(t).putchar` must leave `t.pos` unchanged (DosTestBatch simtst19).
    assert_matches_interpreter(
        r#"begin
            text t1;
            integer p;
            t1 :- copy("abcde");
            t1.setpos(3);
            (t1).putchar('3');
            p := t1.pos;
            OutInt(p, 2);
            OutText(t1);
            OutImage;
        end;"#,
    );
}

#[test]
fn sysout_image_strip_captures_outint() {
    // Terminal `sysout.image` must reflect the free OutText/OutInt line buffer
    // (DosTestBatch simtst28 / simtst49). Avoid `image := notext` here: the
    // interpreter's free-image assign path still diverges from native.
    assert_matches_interpreter(
        r#"begin
            text t;
            OutInt(0, 3);
            OutInt(1, 3);
            OutInt(2, 3);
            t :- copy(sysout.image.strip);
            OutImage;
            OutText(IF t = "  0  1  2" THEN "yes" ELSE "no");
            OutImage;
            OutText(t);
            OutImage;
        end;"#,
    );
}

#[test]
fn overlapping_subtext_match_interpreter() {
    assert_matches_interpreter(include_str!(
        "fixtures/text_attributes/overlapping_subtext.sim"
    ));
}

#[test]
fn compact_match_interpreter() {
    assert_matches_interpreter(include_str!("fixtures/text_attributes/compact.sim"));
}

#[test]
fn getreal_sign_part_blanks_match_interpreter() {
    // SIGN-PART may insert blanks around the sign (DosTestBatch simtst18).
    assert_matches_interpreter(
        r#"begin
            text txt; long real lon;
            txt :- copy("   -  12.34&-10                   ");
            lon := txt.getreal;
            if abs(lon + 0.000000001234) < 1.0&-15 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}

#[test]
fn getreal_denormal_after_addepsilon_does_not_abort() {
    // addepsilon(0) yields a denormal; putreal/getreal must accept it (simtst00).
    assert_matches_interpreter(
        r#"begin
            text t; long real r, s;
            t :- blanks(40);
            r := addepsilon(0.0&&0);
            t.putreal(r, 18);
            s := t.getreal;
            if s > 0.0&&0 then OutText("ok") else OutText("bad");
            OutImage;
        end;"#,
    );
}
