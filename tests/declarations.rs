mod common;

use outimage::compile_str;
use outimage::lex::tokenize;
use outimage::parse::parse;
use outimage::semantic::analyze;
use outimage::source::SourceFile;

fn parse_and_analyze(source: &str) -> Result<(), outimage::CompileError> {
    let stream = tokenize(&SourceFile::anonymous(source))?;
    let program = parse(&stream)?;
    analyze(&program)
}

#[test]
fn parses_integer_constant_declaration() {
    let stream = tokenize(&SourceFile::anonymous(
        "begin integer max = 100, limit = max + 50; end;",
    ))
    .unwrap();
    let program = parse(&stream).unwrap();
    let decl = &program.blocks[0].declarations[0];
    assert!(decl.items[0].is_constant);
    assert_eq!(decl.items[0].name, "max");
    assert!(decl.items[1].is_constant);
    assert!(!decl.items[0].initializer.is_none());
}

#[test]
fn semantic_accepts_constant_chain_in_block_head() {
    parse_and_analyze("begin integer a = 1, b = a + 2; end;").unwrap();
}

#[test]
fn semantic_rejects_constant_referencing_variable() {
    let err = parse_and_analyze("begin integer x := 1; integer c = x; end;").unwrap_err();
    assert!(err.to_string().contains("constant initializer"));
}

#[test]
fn semantic_rejects_assignment_to_constant() {
    let err = parse_and_analyze("begin integer c = 5; c := 10; end;").unwrap_err();
    assert!(err.to_string().contains("constant"));
}

#[test]
fn evaluates_constants_left_to_right() {
    let out = compile_str(
        "begin integer a = 3, b = a * 4; OutInt(a, 0); OutText(\" \"); OutInt(b, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "3 12\n");
}

#[test]
fn runtime_rejects_constant_assignment() {
    let result = compile_str("begin integer c = 1; c := 2; end;");
    assert!(result.is_err());
}

#[test]
fn character_default_is_nul() {
    let out = compile_str("begin character c; OutInt(rank(c), 0); OutImage; end;").unwrap();
    assert_eq!(out, "0\n");
}

#[test]
fn default_values_per_type() {
    let out = compile_str(
        "begin class Point; begin end;
         real r; integer i; boolean b; character c; text t; ref(Point) p;
         if r = 0.0 and i = 0 and not b and rank(c) = 0 and t = notext and p == none then
             OutText(\"ok\");
         OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "ok\n");
}

#[test]
fn fixture_default_values_prints_ok() {
    let source = common::fixture("declarations/default_values.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("ok"), "output was: {output:?}");
}

#[test]
fn parses_prefixed_class_and_concatenates_attributes() {
    let stream = tokenize(&SourceFile::anonymous(
        "begin class Point(x, y); integer x, y;
         begin end;
         Point class Polar(r); real r;
         begin end; end;",
    ))
    .unwrap();
    let program = parse(&stream).unwrap();
    let polar = &program.blocks[0].classes[1];
    assert_eq!(polar.prefix.as_deref(), Some("Point"));
}

#[test]
fn evaluates_class_object_with_attributes() {
    let out = compile_str(
        "begin class Point(x, y); integer x, y;
         begin end;
         ref(Point) p;
         p :- new Point(3, 4);
         integer s; s := p.x + p.y;
         OutInt(s, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "7\n");
}

#[test]
fn parses_switch_declaration() {
    let stream = tokenize(&SourceFile::anonymous(
        "begin switch s := L1, L2, if true then L3 else L4; end;",
    ))
    .unwrap();
    let program = parse(&stream).unwrap();
    let switches = &program.blocks[0].switches;
    assert_eq!(switches.len(), 1);
    assert_eq!(switches[0].name, "s");
    assert_eq!(switches[0].elements.len(), 3);
    assert!(matches!(
        switches[0].elements[0],
        outimage::ast::DesignationalExpr::Label(ref name) if name == "L1"
    ));
    assert!(matches!(
        switches[0].elements[2],
        outimage::ast::DesignationalExpr::If { .. }
    ));
}

#[test]
fn goto_via_switch_designator() {
    let out = compile_str(
        "begin switch s := L1, L2;
         integer i;
         i := 2;
         goto s(i);
         i := 99;
         L1: i := 10;
         L2: i := 20;
         OutInt(i, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "20\n");
}

#[test]
fn switch_conditional_designational_expr_reevaluates() {
    let out = compile_str(
        "begin switch s := L1, if i < 2 then L2 else L3;
         integer i;
         i := 1;
         goto s(2);
         i := 99;
         L1: i := 10;
         L3: i := 30;
         L2: i := 20;
         OutInt(i, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "20\n");

    let out = compile_str(
        "begin switch s := L1, if i < 2 then L2 else L3;
         integer i;
         i := 3;
         goto s(2);
         i := 99;
         L1: i := 10;
         L2: i := 20;
         L3: i := 30;
         OutInt(i, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "30\n");
}

#[test]
fn prefixed_class_inherits_prefix_attributes() {
    let out = compile_str(
        "begin class Point(x); integer x; begin end;
         Point class Polar(r); real r;
         begin r := x + 1; end;
         ref(Polar) p; p :- new Polar(10, 0.0);
         integer v; v := p.r;
         OutInt(v, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "11\n");
}

#[test]
fn parses_integer_array_declaration() {
    let stream = tokenize(&SourceFile::anonymous("begin integer array a(1:10); end;")).unwrap();
    let program = parse(&stream).unwrap();
    let arrays = &program.blocks[0].arrays;
    assert_eq!(arrays.len(), 1);
    assert_eq!(
        arrays[0].element_type,
        outimage::types::Type::Integer { short: false }
    );
    assert_eq!(arrays[0].segments[0].names, vec!["a"]);
}

#[test]
fn semantic_rejects_array_bound_in_same_block_head() {
    let err = parse_and_analyze("begin integer n; integer array a(1:n); end;").unwrap_err();
    assert!(err.to_string().contains("same block head"));
}

#[test]
fn evaluates_array_read_and_write() {
    let out = compile_str(
        "begin integer array a(1:3); a(2) := 42; integer x; x := a(2);
         OutInt(x, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "42\n");
}

#[test]
fn evaluates_multi_dimensional_array() {
    let out = compile_str(
        "begin integer array m(1:2, 1:2); m(2, 1) := 7; integer x; x := m(2, 1);
         OutInt(x, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "7\n");
}

#[test]
fn runtime_rejects_empty_array_access() {
    let result = compile_str("begin integer array a(2:1); integer x; x := a(2); end;");
    assert!(result.is_err());
}

#[test]
fn runtime_rejects_out_of_bounds_access() {
    let result = compile_str("begin integer array a(1:3); integer x; x := a(5); end;");
    assert!(result.is_err());
}

#[test]
fn array_reference_parameter_aliases_caller() {
    let out = compile_str(
        "begin integer array a(1:2);
         procedure set(x); integer array x; begin x(1) := 99; end;
         set(a);
         integer v; v := a(1);
         OutInt(v, 0); OutImage; end;",
    )
    .unwrap();
    assert_eq!(out, "99\n");
}

#[test]
fn array_value_parameter_copies_actual() {
    let source = r#"begin integer array a(1:2);
         procedure bump(x); value x; integer array x;
         begin x(1) := 99; end;
         a(1) := 1;
         bump(a);
         integer v; v := a(1);
         OutInt(v, 0); OutImage; end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "1\n", "value array formal must not alias caller");
}

#[test]
fn array_formal_dimension_mismatch_rejects_at_call() {
    // §4.6.6: formal array rank is fixed by body uses; a 2-D actual passed to a
    // formal indexed as 1-D is rejected at the call site.
    let source = r#"begin
         integer array a(1:2,1:2);
         procedure take(x); integer array x;
         begin integer v; v := x(1); end;
         take(a);
       end;"#;
    let err = parse_and_analyze(source).expect_err("expected dimension mismatch at call");
    let message = err.to_string().to_ascii_lowercase();
    assert!(
        message.contains("array") || message.contains("argument"),
        "unexpected error: {err}"
    );
}

#[test]
fn array_formal_matching_rank_is_accepted() {
    let source = r#"begin
         integer array a(1:2,1:2);
         procedure take(x); integer array x;
         begin integer v; v := x(1,2); end;
         take(a);
       end;"#;
    parse_and_analyze(source).unwrap();
}

#[test]
fn array_formal_conflicting_body_ranks_reject() {
    let source = r#"begin
         procedure take(x); integer array x;
         begin integer v; v := x(1); v := x(1,2); end;
       end;"#;
    let err = parse_and_analyze(source).expect_err("expected conflicting formal ranks");
    assert!(
        err.to_string().to_ascii_lowercase().contains("subscript"),
        "unexpected error: {err}"
    );
}

#[test]
fn text_reference_parameter_ref_assign_is_local() {
    let source = r#"begin text t;
         procedure set(x); text x; begin x :- copy("hi"); end;
         set(t);
         if t = notext then OutText("notext") else OutText(t);
         OutImage; end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(
        out, "notext\n",
        "§4.6.3: rebinding text formal must not update caller"
    );
}

#[test]
fn text_value_parameter_copies_actual() {
    let source = r#"begin text t;
         procedure mutate(x); value x; text x;
         begin upcase(x); end;
         t :- copy("hi");
         mutate(t);
         OutText(t); OutImage; end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "hi\n", "value text formal must not alias caller");
}

#[test]
fn protected_attribute_accessible_in_class_body() {
    let source = "begin class Point(x); integer x; protected x;
                  begin integer t; t := x; end; end;";
    parse_and_analyze(source).unwrap();
    compile_str(source).unwrap();
}

#[test]
fn semantic_rejects_protected_remote_access_from_outer_block() {
    let err = parse_and_analyze(
        "begin class Point(x); integer x; protected x;
         begin end;
         ref(Point) p; p :- new Point(5);
         integer v; v := p.x; end;",
    )
    .unwrap_err();
    assert!(err.to_string().contains("protected"));
}

#[test]
fn hidden_attribute_is_not_a_bare_name_candidate_in_subclass_body() {
    // §5.5.4 / §5.5.6.5: hiding removes the attribute from the visible
    // attributes of the inner classes rather than making a mention of the
    // identifier an error, so a bare `x` in `Polar`'s body binds to the
    // enclosing block's `x` (simtst98). Remote `p.x` is still rejected.
    parse_and_analyze(
        "begin integer x;
         class Point; protected x;
         begin integer x; end;
         Point class Polar; hidden x;
         begin integer t; t := x; end; end;",
    )
    .unwrap();

    let err = parse_and_analyze(
        "begin integer x;
         class Point; protected x;
         begin integer x; end;
         Point class Polar; hidden x;
         begin end;
         ref(Polar) p; p :- new Polar;
         integer v; v := p.x; end;",
    )
    .unwrap_err();
    let message = err.to_string().to_ascii_lowercase();
    assert!(
        message.contains("hidden") || message.contains("protected"),
        "unexpected error: {err}"
    );
}

#[test]
fn semantic_rejects_hidden_without_protected() {
    let err = parse_and_analyze("begin class C; hidden x; begin integer x; end; end;").unwrap_err();
    assert!(err.to_string().contains("protected"));
}

#[test]
fn virtual_integer_subclass_override_via_remote_access() {
    let source = "begin class Base; virtual: integer x;
                  begin integer x; x := 1; end;
                  Base class Derived; begin integer x; x := 2; end;
                  ref(Derived) p; p :- new Derived;
                  integer v; v := p.x;
                  OutInt(v, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "2\n");
}

#[test]
fn virtual_procedure_innermost_match_wins_in_class_body() {
    let source = r#"begin class hashing; virtual: integer procedure hash;
                  begin integer procedure hash(t); text t; begin hash := 100; end hash;
                        integer result; result := hash(""); end;
                  hashing class ALGOL_hash;
                  begin integer procedure hash(T); text T; begin hash := 200; end hash;
                        integer result; result := hash(""); end;
                  ref(ALGOL_hash) h; h :- new ALGOL_hash;
                  integer v; v := h.result;
                  OutInt(v, 0); OutImage; end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "200\n");
}

#[test]
fn virtual_procedure_remote_call_uses_subclass_match() {
    let source = r#"begin class hashing; virtual: integer procedure hash;
                  begin integer procedure hash(t); text t; begin hash := 100; end hash; end;
                  hashing class ALGOL_hash;
                  begin integer procedure hash(T); text T; begin hash := 200; end hash; end;
                  ref(ALGOL_hash) h; h :- new ALGOL_hash;
                  integer v; v := h.hash("");
                  OutInt(v, 0); OutImage; end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "200\n");
}

#[test]
fn split_body_executes_prefix_initial_then_main_final() {
    let source = "begin class Prefix;
                  begin integer a; a := 1; end;
                  Prefix class Main;
                  begin integer b; b := 10; inner; b := 100; end;
                  ref(Main) m; m :- new Main;
                  integer va, vb; va := m.a; vb := m.b;
                  OutInt(va, 0); OutText(\" \"); OutInt(vb, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "1 100\n");
}

#[test]
fn point_polar_concatenation_inherits_coordinates_and_local_attributes() {
    let source = "begin class point(x,y); real x,y;
                  begin end point;
                  point class polar;
                  begin real r; r := x + y; end polar;
                  ref(polar) p; p :- new polar(3.0, 4.0);
                  real rx, ry, rr;
                  rx := p.x; ry := p.y; rr := p.r;
                  OutFix(rx, 0, 0); OutText(\" \");
                  OutFix(ry, 0, 0); OutText(\" \");
                  OutFix(rr, 0, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "3 4 7\n");
}

#[test]
fn point_polar_plus_override_uses_subclass_procedure() {
    let source = common::fixture("classes/point_polar_plus.sim");
    parse_and_analyze(&source).unwrap();
    let trimmed = source.trim_end();
    let source = if trimmed.ends_with("end;") {
        format!(
            "{}    OutFix(rx, 0, 0); OutText(\" \"); OutFix(ry, 0, 0); OutText(\" \"); OutFix(rr, 0, 0); OutImage;\nend;",
            &trimmed[..trimmed.len() - 4]
        )
    } else if trimmed.ends_with("end") {
        format!(
            "{}    OutFix(rx, 0, 0); OutText(\" \"); OutFix(ry, 0, 0); OutText(\" \"); OutFix(rr, 0, 0); OutImage;\nend",
            &trimmed[..trimmed.len() - 3]
        )
    } else {
        panic!("fixture missing program end");
    };
    let out = compile_str(&source).unwrap();
    assert_eq!(out, "11 22 3\n");
}

// --- §5.6 Scope and visibility ---

#[test]
fn inner_block_variable_shadows_outer() {
    let source = "begin integer x; x := 1;
                  begin integer x; x := x + 1; end;
                  integer y; y := x;
                  OutInt(x, 0); OutText(\" \"); OutInt(y, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "1 1\n");
}

#[test]
fn nested_block_restores_outer_after_shadow() {
    let source = "begin integer x; x := 5; begin integer x; x := 99; end;
                  OutInt(x, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "5\n");
}

#[test]
fn class_body_mutates_enclosing_local() {
    let source = r#"begin
        integer n; n := 0;
        class W; begin n := n + 1; end;
        ref(W) w; w :- new W;
        OutInt(n, 0); OutImage;
    end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "1\n");
}

#[test]
fn prefix_attribute_shadowed_by_subclass_attribute() {
    let source = "begin class Point(x); integer x; begin end;
                  Point class Polar(r); integer x; real r;
                  begin x := 5; end;
                  ref(Polar) p; p :- new Polar(10, 0.0);
                  integer t; t := p.x;
                  OutInt(t, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "5\n");
}

#[test]
fn semantic_rejects_formal_parameter_outside_procedure_body() {
    let err = parse_and_analyze(
        "begin procedure p(x); integer x; begin end;
         integer y; y := x; end;",
    )
    .unwrap_err();
    assert!(err.to_string().contains("formal parameter"));
}

#[test]
fn semantic_rejects_class_formal_parameter_outside_class_body() {
    let err = parse_and_analyze(
        "begin class C(x); integer x; begin end;
         integer y; y := x; end;",
    )
    .unwrap_err();
    assert!(err.to_string().contains("formal parameter"));
}

#[test]
fn semantic_rejects_class_attribute_outside_class_body() {
    let err = parse_and_analyze(
        "begin class Point; begin integer x; end;
         integer y; y := x; end;",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not visible"),
        "unexpected error: {err}"
    );
}

#[test]
fn label_in_class_body_visible_to_goto() {
    parse_and_analyze("begin class C; begin goto L; L: begin end; end; end;").unwrap();
}

#[test]
fn goto_to_undefined_label_rejected_in_class() {
    let err = parse_and_analyze("begin class C; begin goto L; end; end;").unwrap_err();
    assert!(err.to_string().contains("label"), "{}", err);
}

#[test]
fn this_expression_accepted_in_class_body() {
    parse_and_analyze(
        "begin class Point(x); integer x; begin end;
         Point class Polar(r); real r;
         begin ref(Point) p; p :- this Point; end; end;",
    )
    .unwrap();
}

#[test]
fn qua_expression_accepted_in_class_body() {
    parse_and_analyze(
        "begin class Point(x); integer x; begin end;
         Point class Polar(r); real r;
         begin ref(Polar) q; q :- this Polar; ref(Point) p; p :- q qua Point; end; end;",
    )
    .unwrap();
}

#[test]
fn semantic_rejects_this_with_invalid_prefix_class() {
    let err = parse_and_analyze("begin class C; begin ref(C) r; r :- this Unknown; end; end;")
        .unwrap_err();
    assert!(err.to_string().contains("prefix"));
}

// --- §5.4 Procedure declaration semantics ---

#[test]
fn innerproduct_style_call_by_name() {
    let source = "begin integer array a(1:3), b(1:3);
                  integer k, i; real y;
                  procedure innerproduct(a,b,k,p,y); name p,y,a,b;
                    integer k,p; real y,a,b;
                  begin real s; integer pp;
                    s := 0;
                    for pp := 1 step 1 until k do
                      begin p := pp; s := s + a * b; end;
                    y := s
                  end innerproduct;
                  integer t;
                  for i := 1 step 1 until 3 do
                    begin a(i) := i; b(i) := 10 * i; end;
                  k := 3;
                  innerproduct(a(i), b(i), k, i, y);
                  t := y;
                  OutInt(t, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "140\n");
}

#[test]
fn call_by_name_assigns_back_to_caller_variable() {
    let source = "begin integer i;
                  procedure set(n); name n; integer n;
                  begin n := 7; end;
                  set(i);
                  integer v; v := i;
                  OutInt(v, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "7\n");
}

#[test]
fn call_by_name_expression_reevaluates_actual() {
    // Jensen: each read of formal `n` re-evaluates the actual `i`.
    let source = "begin integer i, r;
                  integer procedure sum2(n); name n; integer n;
                  begin sum2 := n + n; end;
                  i := 21;
                  r := sum2(i);
                  OutInt(r, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "42\n");
}

#[test]
fn call_by_name_remote_field_reevaluates_after_mutation() {
    // Jensen + objects: each read of `n` re-evaluates `r.x` against the live object
    // (call frames clone object maps, so name re-eval must sync mutations).
    let source = r#"begin
        class C; begin integer x; end;
        ref(C) r;
        integer result;
        integer procedure snap(n); name n; integer n;
        begin
           snap := n;
           r.x := 99;
           snap := snap + n;
        end;
        r :- new C;
        r.x := 1;
        result := snap(r.x);
        OutInt(result, 0); OutImage;
    end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "100\n");
}

#[test]
fn call_by_name_reads_subscripted_actual_with_mutating_index() {
    // Classic pattern: name formal bound to `a(i)` sees updates to `i`.
    let source = "begin integer array a(1:3);
                  integer i, s;
                  procedure accum(x, sum); name x, sum; integer x, sum;
                  begin sum := sum + x; end;
                  a(1) := 10; a(2) := 20; a(3) := 30;
                  s := 0;
                  for i := 1 step 1 until 3 do accum(a(i), s);
                  OutInt(s, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "60\n");
}

#[test]
fn semantic_rejects_duplicate_formal_parameters() {
    let err = parse_and_analyze("begin procedure p(a, a); integer a; begin end; end;").unwrap_err();
    assert!(err.to_string().contains("duplicate formal parameter"));
}

#[test]
fn semantic_rejects_procedure_name_in_formal_list() {
    let err = parse_and_analyze("begin procedure p(p); procedure p; begin end; end;").unwrap_err();
    assert!(err.to_string().contains("formal parameter list"));
}

#[test]
fn semantic_accepts_formal_procedure_redeclaration_in_body() {
    parse_and_analyze(
        "begin procedure caller(f); procedure f;
         begin procedure f; begin OutImage; end; end;
         begin end; end;",
    )
    .unwrap();
}

#[test]
fn semantic_rejects_non_procedure_formal_redeclared_as_procedure() {
    let err = parse_and_analyze(
        "begin procedure p(x); integer x;
         begin procedure x; begin end; end; end;",
    )
    .unwrap_err();
    assert!(err.to_string().contains("cannot be redeclared"));
}

#[test]
fn integer_class_param_defaults_to_value_copy() {
    let source = "begin class C(x); integer x;
                  begin x := 99; end;
                  integer a; a := 5;
                  ref(C) p; p :- new C(a);
                  integer v; v := a;
                  OutInt(a, 0); OutText(\" \"); OutInt(v, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "5 5\n");
}

#[test]
fn ref_class_param_defaults_to_reference_alias() {
    let source = "begin class Point(x); integer x; begin end;
                  class Box(p); ref(Point) p;
                  begin p.x := 42; end;
                  ref(Point) pt; pt :- new Point(0);
                  ref(Box) b; b :- new Box(pt);
                  integer v; v := pt.x;
                  OutInt(v, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "42\n");
}

#[test]
fn reference_is_a_legal_identifier() {
    // `reference` is not a reserved word (§5.4.2). Transmission by reference is
    // the default for text / ref / arrays, not a spelled mode identifier.
    let source = "begin integer reference; reference := 1; OutInt(reference, 1); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "1\n");
}

#[test]
fn semantic_rejects_class_name_parameter_mode() {
    let err =
        parse_and_analyze("begin class C(x); name x; integer x; begin end; end;").unwrap_err();
    assert!(
        err.to_string().to_ascii_lowercase().contains("name"),
        "unexpected error: {err}"
    );
}

#[test]
fn semantic_rejects_object_ref_class_param_by_value() {
    let err = parse_and_analyze(
        "begin class Point; begin end; class C(p); value p; ref(Point) p; begin end; end;",
    )
    .unwrap_err();
    assert!(
        err.to_string().to_ascii_lowercase().contains("value")
            || err.to_string().to_ascii_lowercase().contains("reference"),
        "unexpected error: {err}"
    );
}

#[test]
fn semantic_rejects_wrong_arity_at_call() {
    let err =
        parse_and_analyze("begin procedure p(x); integer x; begin end; p(1, 2); end;").unwrap_err();
    assert!(
        err.to_string().contains("expects") || err.to_string().contains("argument"),
        "unexpected error: {err}"
    );
}

#[test]
fn semantic_rejects_type_mismatch_at_call() {
    let err = parse_and_analyze(r#"begin procedure p(x); integer x; begin end; p("no"); end;"#)
        .unwrap_err();
    assert!(
        err.to_string().contains("integer") || err.to_string().contains("text"),
        "unexpected error: {err}"
    );
}

#[test]
fn concatenated_class_has_fictitious_detach_stub() {
    let source = "begin class Worker; begin end; end;";
    parse_and_analyze(source).unwrap();
}

#[test]
fn detach_statement_in_class_body_suspends_new() {
    let source = r#"begin
        class Worker;
        begin
            OutText("A"); OutImage;
            detach;
            OutText("B"); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker;
        OutText("C"); OutImage;
        call(w);
    end;"#;
    parse_and_analyze(source).unwrap();
    let output = compile_str(source).unwrap();
    assert_eq!(output, "A\nC\nB\n");
}

#[test]
fn remote_detach_call_on_object_is_noop() {
    let source = "begin class Worker; begin end;
                  ref(Worker) w; w :- new Worker;
                  integer v; v := w.detach();
                  OutInt(v, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "0\n");
}

#[test]
fn semantic_rejects_detach_outside_object_context() {
    let err = parse_and_analyze("begin detach; end;").unwrap_err();
    assert!(err.to_string().contains("detach"));
}

// --- §5.5.6 Remote accessing ---

#[test]
fn semantic_typechecks_remote_attribute_against_class_table() {
    parse_and_analyze(
        "begin class Point(x); integer x; begin end;
         ref(Point) p; p :- new Point(1);
         integer v; v := p.x; end;",
    )
    .unwrap();
}

#[test]
fn semantic_rejects_unknown_remote_attribute() {
    let err = parse_and_analyze(
        "begin class Point(x); integer x; begin end;
         ref(Point) p; p :- new Point(1);
         integer v; v := p.missing; end;",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("no attribute"),
        "unexpected error: {err}"
    );
}

#[test]
fn semantic_rejects_incompatible_remote_attribute_assignment() {
    let err = parse_and_analyze(
        "begin class Point(x); integer x; begin end;
         ref(Point) p; p :- new Point(1);
         p.x := \"hello\"; end;",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("cannot assign")
            || err.to_string().contains("assignment needs")
            || err.to_string().contains("text"),
        "unexpected error: {err}"
    );
}

#[test]
fn remote_access_at_subclass_level_uses_subclass_attribute_type() {
    let err = parse_and_analyze(
        "begin class Point(x); integer x; begin end;
         Point class Polar(r); boolean x; real r; begin end;
         ref(Polar) p; integer t;
         begin p :- new Polar(10, 0.0); t := p.x; end; end;",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("cannot assign")
            || err.to_string().contains("assignment needs")
            || err.to_string().contains("boolean"),
        "expected innermost subclass attribute type at outer access level, got: {err}"
    );
}

#[test]
fn class_procedure_remote_call_uses_object_environment() {
    let source = r#"begin class Counter;
                  begin integer count;
                        integer procedure get; begin get := count; end;
                        count := 42;
                  end;
                  ref(Counter) c; c :- new Counter;
                  integer v; v := c.get();
                  OutInt(v, 0); OutImage; end;"#;
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "42\n");
}

#[test]
fn remote_access_shadows_prefix_attribute_at_outer_level() {
    let source = "begin class Point(x); integer x; begin end;
                  Point class Polar(r); integer x; real r;
                  begin x := 5; end;
                  ref(Polar) p; p :- new Polar(10, 0.0);
                  integer t; t := p.x;
                  OutInt(t, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "5\n");
}

#[test]
fn remote_access_at_prefix_level_via_qua_uses_prefix_attribute() {
    // §5.5.6.5: `(p qua Point).x` resolves the Point-level attribute, not Polar's.
    let source = "begin class Point(x); integer x; begin end;
                  Point class Polar(r); integer x; real r;
                  begin x := 5; end;
                  ref(Polar) p; p :- new Polar(10, 0.0);
                  integer t; t := (p qua Point).x;
                  OutInt(t, 0); OutImage; end;";
    parse_and_analyze(source).unwrap();
    let out = compile_str(source).unwrap();
    assert_eq!(out, "10\n");
}

#[test]
fn fixture_rejects_virtual_procedure_heading_mismatch() {
    let source = common::fixture("classes/virtual_procedure_heading.sim");
    let err = parse_and_analyze(&source).unwrap_err();
    assert!(
        err.to_string().contains("does not match") || err.to_string().contains("heading"),
        "unexpected error: {err}"
    );
}

#[test]
fn fixture_rejects_prefix_locality_violation() {
    let source = common::fixture("classes/prefix_locality.sim");
    let err = parse_and_analyze(&source).unwrap_err();
    assert!(
        err.to_string().contains("not local"),
        "unexpected error: {err}"
    );
}

#[test]
fn fixture_rejects_this_in_block_prefix() {
    let source = common::fixture("classes/this_in_block_prefix.sim");
    let err = parse_and_analyze(&source).unwrap_err();
    assert!(err.to_string().contains("this"), "unexpected error: {err}");
}

#[test]
fn fixture_constant_arithmetic_conversion_uses_entier() {
    let source = common::fixture("declarations/constant_arithmetic_conversion.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("ok"), "output was: {output:?}");
}

#[test]
fn fixture_virtual_type_subordination_accepts_subclass_ref() {
    let source = common::fixture("classes/virtual_type_subordination.sim");
    parse_and_analyze(&source).unwrap();
    compile_str(&source).unwrap();
}

#[test]
fn fixture_rejects_virtual_type_not_subordinate() {
    let source = common::fixture("classes/virtual_type_subordination_reject.sim");
    let err = parse_and_analyze(&source).unwrap_err();
    assert!(
        err.to_string().contains("does not match") || err.to_string().contains("virtual"),
        "unexpected error: {err}"
    );
}

#[test]
fn fixture_unmatched_virtual_is_visible() {
    let source = common::fixture("classes/unmatched_virtual.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("0"), "output was: {output:?}");
}

#[test]
fn prefixed_block_can_access_protected_attribute() {
    let source = common::fixture("blocks/prefixed_block_protected.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("7"), "output was: {output:?}");
}

#[test]
fn goto_subclass_to_prefix_label() {
    let source = common::fixture("control_flow/goto_subclass_prefix_label.sim");
    parse_and_analyze(&source).unwrap();
    let out = compile_str(&source).unwrap();
    assert_eq!(out, "ok\n");
}

#[test]
fn fixture_remote_access_level_via_qua() {
    let source = common::fixture("declarations/remote_access_level.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("ok"), "output was: {output:?}");
}

#[test]
fn fixture_identifier_substitution_on_concatenation() {
    let source = common::fixture("declarations/identifier_substitution.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("ok"), "output was: {output:?}");
}

#[test]
fn fixture_connection_hidden_attribute() {
    let source = common::fixture("declarations/connection_hidden.sim");
    parse_and_analyze(&source).unwrap();
    let output = compile_str(&source).unwrap();
    assert!(output.contains("ok"), "output was: {output:?}");
}
