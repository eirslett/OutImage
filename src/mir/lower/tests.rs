use super::*;
use crate::ast::StatementKind;
use crate::parse::test_support::parse_program;

/// Flattens every op across every block of `module`'s first (only)
/// function, in block order, for easy sequence assertions.
fn ops(module: &Module) -> Vec<&Op> {
    module.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.ops.iter().map(|spanned| &spanned.op))
        .collect()
}

fn lower(source: &str) -> Module {
    let program = parse_program(source);
    lower_program(&program).unwrap_or_else(|error| panic!("expected lowering to succeed: {error}"))
}

fn lower_err(source: &str) -> CompileError {
    let program = parse_program(source);
    lower_program(&program).expect_err("expected lowering to fail")
}

/// Nested if-expressions must emit Copy/Jump on the *arm end* blocks
/// (inner merge), never re-enter the arm entry after it already branched.
#[test]
fn nested_if_expression_has_no_ops_after_terminator() {
    let module = lower(
        r#"begin boolean b;
           b := if true then (if true then true else false)
                else (if false then true else false);
           end;"#,
    );
    let main = module
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main");
    for block in &main.blocks {
        let mut seen_terminator = false;
        for spanned in &block.ops {
            let is_term = matches!(
                spanned.op,
                Op::Jump { .. } | Op::Branch { .. } | Op::Return { .. }
            );
            assert!(
                !seen_terminator,
                "ops after terminator in {}: {:?}",
                block.id.0,
                block.ops.iter().map(|s| &s.op).collect::<Vec<_>>()
            );
            if is_term {
                seen_terminator = true;
            }
        }
    }
    // Every block that is reachable should end with a terminator (no empty
    // merges left behind by nested if-exprs).
    for block in &main.blocks {
        let has_term = block.ops.iter().any(|s| {
            matches!(
                s.op,
                Op::Jump { .. } | Op::Branch { .. } | Op::Return { .. }
            )
        });
        assert!(
            has_term,
            "block bb{} has no terminator: {:?}",
            block.id.0,
            block.ops.iter().map(|s| &s.op).collect::<Vec<_>>()
        );
    }
}

// 1. OutText + OutImage: one pooled string, two calls.
#[test]
fn lowers_out_text_and_out_image() {
    let module = lower(r#"begin OutText("hi"); OutImage; end;"#);
    assert_eq!(module.strings, vec!["hi".to_string()]);

    let ops = ops(&module);
    let out_text_count = ops
        .iter()
        .filter(|op| matches!(op, Op::CallOutText { string_id: 0 }))
        .count();
    let out_image_count = ops
        .iter()
        .filter(|op| matches!(op, Op::CallOutImage))
        .count();
    assert_eq!(
        out_text_count, 1,
        "expected exactly one CallOutText: {ops:?}"
    );
    assert_eq!(
        out_image_count, 1,
        "expected exactly one CallOutImage: {ops:?}"
    );
}

// 2b. Constants: assignment to a constant is a hard error (§5.8).
#[test]
fn rejects_assignment_to_constant() {
    let error = lower_err("begin integer c = 1; c := 2; end;");
    assert!(
        error.to_string().contains("cannot assign to constant"),
        "unexpected error: {error}"
    );
}

#[test]
fn hold_inside_process_procedure_attribute_emits_sim_hold() {
    // Nested `hold` / `wait` in Process method bodies (not only top-level
    // `__init` statements) lower in place — required by Simulation corpus
    // units such as simtst87 (`hold` / `wait` under `inspect` / `while`).
    let module = lower(
        r#"Simulation begin
            Process class Worker;
            begin
                procedure nap; begin hold(1.0); end;
                nap;
            end;
            ref(Worker) w; w :- new Worker; activate w;
        end;"#,
    );
    let nap = module
        .functions
        .iter()
        .find(|function| function.name.contains("nap"))
        .expect("Worker$nap");
    assert!(
        nap.blocks
            .iter()
            .flat_map(|block| block.ops.iter())
            .any(|spanned| matches!(spanned.op, Op::SimHold { .. })),
        "expected SimHold in nap: {nap:?}"
    );
}

#[test]
fn hold_inside_outlined_simulation_procedure_emits_sim_hold() {
    // A Simulation-block procedure with no enclosing captures is outlined
    // as a free function, not a method — it still has to lower `hold`.
    let module = lower(
        r#"Simulation begin
            procedure nap; begin hold(1.0); end;
            nap;
        end;"#,
    );
    let nap = module
        .functions
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case("nap"))
        .expect("nap");
    assert!(
        nap.blocks
            .iter()
            .flat_map(|block| block.ops.iter())
            .any(|spanned| matches!(spanned.op, Op::SimHold { .. })),
        "expected SimHold in outlined nap: {nap:?}"
    );
}

#[test]
fn process_inlines_sibling_boolean_array_reads() {
    // simtst97: Process body calls sibling `outstate` which indexes a
    // boolean array declared in the enclosing Simulation block.
    let module = lower(
        r#"Simulation begin
            process class p;
            begin
                outstate;
            end;
            procedure outstate;
            begin
                if active(1) then OutText("a");
            end;
            boolean array active(1:2);
            activate new p;
        end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name.contains("p$__init")),
        "expected p$__init in {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

#[test]
fn inspect_when_sees_enclosing_class_attributes() {
    // simtst87: `station` body does `inspect … when kund do … nr …`
    // — `nr` is a constructor param of station, not of kund.
    let module = lower(
        r#"Simulation begin
            process class kund; begin end;
            process class station(nr); integer nr;
            begin
                inspect none when kund do OutInt(nr, 2);
            end;
            activate new station(3);
        end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.contains("station$__init")),
        "expected station$__init"
    );
}

#[test]
fn nested_wait_in_process_inspect_lowers() {
    let module = lower(
        r#"Simulation begin
            ref(head) q;
            process class station;
            begin
                inspect none when head do ;
                otherwise begin wait(q); end;
            end;
            q :- new head;
            activate new station;
        end;"#,
    );
    let entry = module
        .functions
        .iter()
        .find(|f| f.name == "station$__coro")
        .expect("station$__coro");
    assert!(
        entry
            .blocks
            .iter()
            .flat_map(|b| b.ops.iter())
            .any(|s| { matches!(s.op, Op::SimsetInto { .. }) || matches!(s.op, Op::SimPassivate) }),
        "expected wait → into+passivate: {entry:?}"
    );
}

#[test]
fn normal_stream_on_enclosing_capture_lowers() {
    let module = lower(
        r#"Simulation begin
            integer U2;
            U2 := 12345;
            process class kund;
            begin
                real x;
                x := normal(0.0, 1.0, U2);
            end;
            activate new kund;
        end;"#,
    );
    let entry = module
        .functions
        .iter()
        .find(|f| f.name == "kund$__coro")
        .expect("kund$__coro");
    assert!(
        entry
            .blocks
            .iter()
            .flat_map(|b| b.ops.iter())
            .any(|s| matches!(&s.op, Op::CallEnv { name, .. } if name == "normal")),
        "expected CallEnv normal: {entry:?}"
    );
}

#[test]
fn class_method_with_formal_procedure_param_inlines() {
    // simtst62 shape: method B(E) with formal procedure E; nested local
    // class C closes over E across detach/resume.
    let module = lower(
        r#"begin
            class X; begin
                procedure B(E); procedure E; begin
                    ref(C) cc;
                    class C; begin
                        detach;
                        E;
                    end;
                    cc :- new C;
                    resume(cc);
                end;
                procedure E; begin end;
                B(E);
            end;
            ref(X) xx;
            xx :- new X;
        end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name.contains("X$__init")),
        "expected X$__init, got {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        !module
            .functions
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("X$B")),
        "B should be call-site inlined, not outlined: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let c = module
        .class_layouts
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case("C") || l.declared_name.eq_ignore_ascii_case("C"))
        .expect("C layout");
    assert!(
        c.enclosing_captures
            .iter()
            .any(|(n, _)| n.starts_with("__simrt_fp_")),
        "local class C should snapshot formal-proc E: {:?}",
        c.enclosing_captures
    );
}

#[test]
fn methods_collected_from_inspect_do_clause() {
    // simtst96: `inspect … do Simulation begin … link class town; … find …`
    let module = lower(
        r#"begin
            inspect new InFile(blanks(1)) do
            Simulation begin
                link class town;
                begin
                    ref(town) procedure find(code); text code;
                    begin find :- this town; end;
                end;
                ref(town) t;
                t :- new town;
                t :- t.find("x");
            end;
        end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("town$find")),
        "expected town$find signature/function, got {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

#[test]
fn for_while_is_narrows_control_qualification() {
    let module = lower(
        r#"Simulation begin
            link class town; begin boolean gone; end;
            ref(head) towns;
            ref(town) r;
            towns :- new head;
            r :- towns.first qua town;
            for r :- r.suc while r is town do r.gone := false;
        end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name == "main"),
        "expected main"
    );
}

#[test]
fn for_control_closes_over_enclosing_character() {
    // simtst96 `procedure scan; begin for ch:=inchar …` — `ch` is not a
    // fresh local; it must force call-site inlining over enclosing `ch`.
    let module = lower(
        r#"begin
            character ch;
            procedure scan;
            begin
                for ch := inchar while ch = ' ' do ;
            end;
            scan;
        end;"#,
    );
    assert!(
        !module
            .functions
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("scan")),
        "scan should be inlined (enclosing ch), got {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

// 2. Arithmetic precedence: `1 + 2 * 3` must lower as `1 + (2 * 3)`.
#[test]
fn lowers_arithmetic_with_correct_precedence() {
    let module = lower("begin integer x; x := 1 + 2 * 3; end;");
    let ops = ops(&module);

    // Declaration default-initializes x, then the assignment lowers
    // 1, 2, 3, a Mul (2*3), an Add (1 + mul), and a StoreLocal into x.
    let mul_index = ops
        .iter()
        .position(|op| matches!(op, Op::Binary { op: BinOp::Mul, .. }))
        .expect("expected a Mul op");
    let add_index = ops
        .iter()
        .position(|op| matches!(op, Op::Binary { op: BinOp::Add, .. }))
        .expect("expected an Add op");
    assert!(
        mul_index < add_index,
        "2*3 must be evaluated before the add: {ops:?}"
    );

    let Op::Binary {
        op: BinOp::Add,
        left,
        right,
        ..
    } = ops[add_index]
    else {
        unreachable!()
    };
    let Op::Binary {
        op: BinOp::Mul,
        dest: mul_dest,
        ..
    } = ops[mul_index]
    else {
        unreachable!()
    };
    assert_eq!(
        *right, *mul_dest,
        "add's right operand must be the mul's result"
    );
    assert_ne!(
        *left, *right,
        "add's left operand (1) must differ from the mul result"
    );

    let store_count = ops
        .iter()
        .filter(|op| matches!(op, Op::StoreLocal { .. }))
        .count();
    assert_eq!(store_count, 1);
}

// 3. `if`/`else` with both branches taken creates 3 extra blocks and
// stores the right constant on each side.
#[test]
fn lowers_if_else_with_both_branches() {
    let module = lower("begin integer x; if x = 0 then x := 1 else x := 2; end;");
    let function = &module.functions[0];
    assert_eq!(function.blocks.len(), 4, "entry + then + else + merge");

    let has_store_of = |block: &BasicBlock, expected: i64| {
        block.ops.iter().any(|spanned| {
            if let Op::StoreLocal { src, .. } = &spanned.op {
                block_has_const(block, *src, expected)
            } else {
                false
            }
        })
    };
    // then_block = index 1, else_block = index 2 (allocation order in lower_if).
    assert!(
        has_store_of(&function.blocks[1], 1),
        "then branch should store 1"
    );
    assert!(
        has_store_of(&function.blocks[2], 2),
        "else branch should store 2"
    );

    fn block_has_const(block: &BasicBlock, id: LocalId, expected: i64) -> bool {
        block
            .ops
            .iter()
            .any(|spanned| matches!(&spanned.op, Op::ConstI64 { dest, value } if *dest == id && *value == expected))
    }

    assert!(matches!(
        function.blocks[0].ops.last().map(|s| &s.op),
        Some(Op::Branch { .. })
    ));
}

// 4. `while` with a body that mutates the loop variable produces a
// header/body/exit shape and the body contains the mutation.
#[test]
fn lowers_while_with_mutating_body() {
    let module = lower("begin integer i; i := 0; while i < 5 do i := i + 1; end;");
    let function = &module.functions[0];
    assert_eq!(function.blocks.len(), 4, "entry + header + body + exit");

    let header = &function.blocks[1];
    assert!(matches!(
        header.ops.last().map(|s| &s.op),
        Some(Op::Branch { .. })
    ));

    let body = &function.blocks[2];
    assert!(
        body.ops
            .iter()
            .any(|s| matches!(&s.op, Op::Binary { op: BinOp::Add, .. }))
    );
    assert!(
        body.ops
            .iter()
            .any(|s| matches!(&s.op, Op::StoreLocal { .. }))
    );
    assert!(
        matches!(body.ops.last().map(|s| &s.op), Some(Op::Jump { target }) if *target == function.blocks[1].id)
    );
}

// 5. Booleans: `not`, `and`, `or`, relations.
#[test]
fn lowers_boolean_not() {
    let module = lower("begin boolean a; a := not a; end;");
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Unary { op: UnOp::Not, .. }))
    );
}

#[test]
fn lowers_boolean_and_or() {
    let and_module = lower("begin boolean a, b, c; c := a and b; end;");
    assert!(
        ops(&and_module)
            .iter()
            .any(|op| matches!(op, Op::Binary { op: BinOp::And, .. }))
    );

    let or_module = lower("begin boolean a, b, c; c := a or b; end;");
    assert!(
        ops(&or_module)
            .iter()
            .any(|op| matches!(op, Op::Binary { op: BinOp::Or, .. }))
    );
}

#[test]
fn lowers_relations() {
    let module = lower("begin integer x; boolean r; r := x = 1; end;");
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::Compare { op: CmpOp::Eq, .. }))
    );
}

// 6. Unary minus.
#[test]
fn lowers_unary_minus() {
    let module = lower("begin integer x; x := -x; end;");
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::Unary { op: UnOp::Neg, .. }))
    );
}

// 7. Chained assignment `a := b := c;`: the inner assignment stores into
// `b`, and the outer assignment must then store `b`'s freshly-written
// value into `a` (left-to-right evaluation of `A := B := C`).
#[test]
fn lowers_chained_assignment() {
    let module = lower("begin integer a, b, c; a := b := c; end;");
    let ops = ops(&module);
    let stores: Vec<(LocalId, LocalId)> = ops
        .iter()
        .filter_map(|op| match op {
            Op::StoreLocal { local, src } => Some((*local, *src)),
            _ => None,
        })
        .collect();
    assert_eq!(stores.len(), 2, "expected two StoreLocal ops: {ops:?}");
    let (inner_target, _inner_src) = stores[0];
    let (_outer_target, outer_src) = stores[1];
    assert_eq!(
        outer_src, inner_target,
        "outer assignment must read back the value just stored into the inner target"
    );
}

// 8. Empty `begin end;` lowers to a single block with just the trailing return.
#[test]
fn lowers_empty_block() {
    let module = lower("begin end;");
    let function = &module.functions[0];
    assert_eq!(function.blocks.len(), 1);
    assert!(function.locals.is_empty());
    assert_eq!(function.blocks[0].ops.len(), 1);
    assert!(matches!(
        function.blocks[0].ops[0].op,
        Op::Return { value: None }
    ));
}

// 9. Unsupported `new Foo` (undefined class) errors.
#[test]
fn errors_on_object_generator() {
    let error = lower_err("begin new Foo; end;");
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.to_ascii_lowercase().contains("class")
            || error.message.to_ascii_lowercase().contains("foo"),
        "message was: {}",
        error.message
    );
}

#[test]
fn lowers_new_and_remote_integer_field() {
    let module = lower(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- new Point;
            p.x := 1;
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::NewObject { .. })),
        "expected NewObject: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::FieldStoreI64 { .. })),
        "expected FieldStoreI64: {ops:?}"
    );
    assert!(
        module.functions[0]
            .locals
            .iter()
            .any(|local| local.ty == MirType::ObjectRef),
        "expected an ObjectRef local"
    );
    let p = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "p")
        .expect("local p");
    assert_eq!(p.class_qual.as_deref(), Some("Point"));
    assert!(
        module
            .class_layouts
            .iter()
            .any(|layout| layout.name == "Point"),
        "expected Point layout on MIR module"
    );
}

#[test]
fn lowers_none_assignment() {
    let module = lower(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- none;
        end;"#,
    );
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::ConstNone { .. })),
        "expected ConstNone"
    );
}

#[test]
fn lowers_qua_same_class_as_copy() {
    let module = lower(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- new Point;
            q :- p qua Point;
        end;"#,
    );
    let module_ops = ops(&module);
    assert!(
        module_ops
            .windows(2)
            .any(|window| matches!(window[0], Op::Copy { .. })
                && matches!(window[1], Op::StoreLocal { .. })),
        "expected Copy from qua into store: {module_ops:?}"
    );
}

#[test]
fn lowers_qua_on_none_reference() {
    let module = lower(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p, q;
            p :- none;
            q :- p qua Point;
        end;"#,
    );
    let module_ops = ops(&module);
    assert!(
        module_ops.iter().any(|op| matches!(op, Op::Copy { .. })),
        "expected Copy for none qua Point: {module_ops:?}"
    );
}

#[test]
fn errors_on_mismatched_qua() {
    let error = lower_err(
        r#"begin
            class Point; begin integer x; end;
            class Other; begin integer y; end;
            ref(Point) p;
            ref(Other) q;
            p :- new Point;
            q :- p qua Other;
        end;"#,
    );
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("cannot be qualified"),
        "message was: {}",
        error.message
    );
}

#[test]
fn lowers_class_methods_and_remote_calls() {
    let module = lower(
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
            v := c.get();
        end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name == "Counter$increment"),
        "expected mangled increment: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module.functions.iter().any(|f| f.name == "Counter$get"),
        "expected mangled get"
    );
    let increment = find_function(&module, "Counter$increment");
    assert_eq!(increment.params.len(), 1);
    assert_eq!(increment.params[0].name, "__this");
    assert_eq!(increment.params[0].ty, MirType::ObjectRef);
    assert!(
        increment
            .blocks
            .iter()
            .flat_map(|b| b.ops.iter())
            .any(|s| matches!(s.op, Op::FieldLoadI64 { .. })),
        "method body should FieldLoad the bare field name"
    );
    assert!(
        increment
            .blocks
            .iter()
            .flat_map(|b| b.ops.iter())
            .any(|s| matches!(s.op, Op::FieldStoreI64 { .. })),
        "method body should FieldStore the bare field name"
    );
    let main_ops = ops(&module);
    assert!(
        main_ops.iter().any(|op| matches!(
            op,
            Op::Call { name, dest: None, .. } if name == "Counter$increment"
        )),
        "expected Call Counter$increment: {main_ops:?}"
    );
    assert!(
        main_ops.iter().any(|op| matches!(
            op,
            Op::Call { name, dest: Some(_), .. } if name == "Counter$get"
        )),
        "expected Call Counter$get with dest: {main_ops:?}"
    );
}

#[test]
fn lowers_method_with_value_parameter() {
    let module = lower(
        r#"begin
            class Counter; begin
                integer n;
                procedure add(k); value k; integer k; begin n := n + k; end;
            end;
            ref(Counter) c;
            c :- new Counter;
            c.add(3);
        end;"#,
    );
    let add = find_function(&module, "Counter$add");
    assert_eq!(add.params.len(), 2);
    assert_eq!(add.params[0].name, "__this");
    assert_eq!(add.params[1].name, "k");
    assert_eq!(add.params[1].ty, MirType::I64);
}

#[test]
fn lowers_bare_parameterless_method_remote_access() {
    let module = lower(
        r#"begin
            class C; begin
                integer x;
                procedure p; begin end;
            end;
            ref(C) r;
            r :- new C;
            r.p;
        end;"#,
    );
    let main = find_function(&module, "main");
    let entry = &main.blocks[main.entry.0];
    assert!(
        entry.ops.iter().any(|spanned| matches!(
            &spanned.op,
            Op::Call { name, .. } if name == "C$p"
        )),
        "expected a call to C$p for bare r.p; ops were: {:?}",
        entry
            .ops
            .iter()
            .map(|s| format!("{:?}", s.op))
            .collect::<Vec<_>>()
    );
}

#[test]
fn errors_on_bare_method_with_required_args() {
    let error = lower_err(
        r#"begin
            class C; begin
                procedure add(integer k); begin end;
            end;
            ref(C) r;
            r :- new C;
            r.add;
        end;"#,
    );
    assert!(
        error.message.to_ascii_lowercase().contains("argument"),
        "message was: {}",
        error.message
    );
}

#[test]
fn lowers_this_in_method_body() {
    let module = lower(
        r#"begin
            class Point; begin
                integer x;
                procedure init; begin
                    ref(Point) self;
                    self :- this Point;
                    self.x := 1;
                end;
            end;
            ref(Point) p;
            p :- new Point;
            p.init();
        end;"#,
    );
    let init = find_function(&module, "Point$init");
    assert!(
        init.blocks
            .iter()
            .flat_map(|b| b.ops.iter())
            .any(|s| matches!(s.op, Op::Copy { .. })),
        "expected Copy for 'this Point'"
    );
    assert!(
        init.blocks
            .iter()
            .flat_map(|b| b.ops.iter())
            .any(|s| matches!(s.op, Op::FieldStoreI64 { .. })),
        "expected FieldStoreI64 via self.x"
    );
}

#[test]
fn errors_on_this_outside_method() {
    let error = lower_err(
        r#"begin
            class Point; begin integer x; end;
            ref(Point) p;
            p :- this Point;
        end;"#,
    );
    assert!(
        error.message.contains("outside object context"),
        "message was: {}",
        error.message
    );
}

// 10. `OutText` with a non-text argument errors.
#[test]
fn errors_on_out_text_with_non_text() {
    let error = lower_err("begin integer x; OutText(x); end;");
    assert!(
        error.message.contains("text"),
        "message was: {}",
        error.message
    );
}

// 11. `Module::dump` / `Display` mentions the function name and block labels.
#[test]
fn dump_contains_function_name_and_block_labels() {
    let module = lower("begin integer x; if x = 0 then x := 1; end;");
    let dump = module.dump();
    assert!(dump.contains("fn main("), "dump was:\n{dump}");
    assert!(dump.contains("bb0"), "dump was:\n{dump}");
    assert!(dump.contains("bb1"), "dump was:\n{dump}");
    assert_eq!(format!("{module}"), dump, "Display should match dump()");
}

// 12. Spans: the AST now carries real spans, so MIR ops derived directly
// from a statement/expression must copy them rather than falling back to
// `0..0`. (If a future refactor makes `Expr`/`Statement` spanless again,
// this test should be replaced with one asserting the `0..0` fallback.)
#[test]
fn spans_propagate_from_ast_statements() {
    let program = parse_program("begin integer x; x := 42; end;");
    let assign_span = match &program.blocks[0].statements[0].kind {
        StatementKind::Assignment(_) => program.blocks[0].statements[0].span.clone(),
        other => panic!("expected assignment statement, got {other:?}"),
    };
    assert_ne!(
        assign_span,
        0..0,
        "parser is expected to produce real spans"
    );

    let module = lower_program(&program).unwrap();
    let store = module.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.ops.iter())
        .find(|spanned| matches!(spanned.op, Op::StoreLocal { .. }))
        .expect("expected a StoreLocal op");
    assert_eq!(store.span, assign_span);
}

#[test]
fn lowers_real_declaration_to_f64_local() {
    let module = lower("begin real x; end;");
    let x = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "x")
        .expect("real local");
    assert_eq!(x.ty, MirType::F64);
}

#[test]
fn lowers_long_real_declaration_to_long_f64_local() {
    let module = lower("begin long real x; end;");
    let x = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "x")
        .expect("long real local");
    assert_eq!(x.ty, MirType::LongF64);
}

#[test]
fn lowers_long_real_arithmetic_with_real_promotion() {
    let module = lower(r#"begin long real a; real b; a := 1.5; b := 2.0; a := a + b; end;"#);
    let ops: Vec<_> = module.functions[0]
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(op, Op::Binary { .. })),
        "expected binary add: {ops:?}"
    );
    let a = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "a")
        .expect("a");
    assert_eq!(a.ty, MirType::LongF64);
}

// --- Local procedures -----------------------------------------------------

/// Finds the (only) non-`main` function named `name`.
fn find_function<'a>(module: &'a Module, name: &str) -> &'a Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| {
            panic!(
                "expected a function named '{name}', found: {:?}",
                module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
            )
        })
}

#[test]
fn lowers_a_function_procedure_alongside_main() {
    let module = lower(
        "begin integer procedure f(x); value x; integer x; begin f := x + 1; end;
         integer y; y := f(41); end;",
    );
    assert_eq!(module.functions[0].name, "main");
    assert!(
        module.functions.iter().any(|f| f.name == "f"),
        "expected procedure f alongside main (and BASICIO inits)"
    );

    let f = find_function(&module, "f");
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].name, "x");
    assert_eq!(f.params[0].ty, MirType::I64);
    assert_eq!(f.result, Some(MirType::I64));

    // The implicit result local is named after the procedure and is
    // zero-initialized before the body runs.
    let result_local = f
        .locals
        .iter()
        .position(|local| local.name == "f")
        .expect("expected an implicit 'f' result local");
    let result_id = LocalId(f.params.len() + result_local);
    let entry = &f.blocks[f.entry.0];
    assert!(
        entry.ops.iter().any(
            |spanned| matches!(&spanned.op, Op::ConstI64 { dest, value: 0 } if *dest == result_id)
        ),
        "expected f's entry block to zero-initialize its result local: {:?}",
        entry.ops
    );

    // The trailing op of every block path must eventually reach a
    // `Return` carrying the result local.
    let has_matching_return =
        f.blocks.iter().flat_map(|block| block.ops.iter()).any(
            |spanned| matches!(&spanned.op, Op::Return { value: Some(id) } if *id == result_id),
        );
    assert!(
        has_matching_return,
        "expected Return to read the result local: {f:?}"
    );

    // The call site in `main` lowers to a `Call` with a `dest` (used as
    // an expression) and one argument.
    let main = &module.functions[0];
    let call = main
        .blocks
        .iter()
        .flat_map(|block| block.ops.iter())
        .find_map(|spanned| match &spanned.op {
            Op::Call { dest, name, args } if name == "f" => Some((dest, args)),
            _ => None,
        })
        .expect("expected a Call to 'f' in main");
    assert!(
        call.0.is_some(),
        "expression-position call should have a dest"
    );
    assert_eq!(call.1.len(), 1);
}

#[test]
fn inlined_type_procedure_result_is_debug_scoped() {
    // `flag` closes over `n`, so it is inlined. Its result slot lives in
    // `main`'s frame but must not show in DAP while the PC is outside `flag`.
    let module = lower(
        "begin integer n;
         boolean procedure flag;
         begin flag := n > 0; end;
         n := 0;
         if flag then n := 1; end;",
    );
    assert!(
        !module
            .functions
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("flag")),
        "expected flag to be inlined into main, found {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let main = module
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main");
    let flag = main
        .locals
        .iter()
        .find(|local| local.name.eq_ignore_ascii_case("flag"))
        .expect("inlined result local named flag");
    assert_eq!(flag.ty, MirType::Bool);
    let scope = flag
        .debug_scope
        .as_ref()
        .expect("inlined type-procedure result should carry a debug_scope");
    assert!(
        scope.start < scope.end,
        "debug_scope should be a real source span, got {scope:?}"
    );
    let n = main
        .locals
        .iter()
        .find(|local| local.name.eq_ignore_ascii_case("n"))
        .expect("n");
    assert!(
        n.debug_scope.is_none(),
        "enclosing local n should stay visible for the whole frame"
    );
    assert!(
        main.debug_scopes.iter().any(|scope| {
            scope.kind == DebugScopeKind::Procedure && scope.name.eq_ignore_ascii_case("flag")
        }),
        "inlined flag should be recorded as a procedure debug scope, got {:?}",
        main.debug_scopes
    );
}

#[test]
fn nested_block_local_is_debug_scoped() {
    let module = lower(
        "begin integer n;
         n := 1;
         begin integer k;
               k := 2;
               n := k;
         end;
         n := 3; end;",
    );
    let main = module
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main");
    let k = main
        .locals
        .iter()
        .find(|local| local.name.eq_ignore_ascii_case("k"))
        .expect("nested local k");
    let scope = k
        .debug_scope
        .as_ref()
        .expect("nested-block local should carry a debug_scope");
    assert!(
        scope.start < scope.end,
        "nested-block debug_scope should be a real source span, got {scope:?}"
    );
    let n = main
        .locals
        .iter()
        .find(|local| local.name.eq_ignore_ascii_case("n"))
        .expect("n");
    assert!(
        n.debug_scope.is_none(),
        "enclosing local n should stay visible for the whole frame"
    );
    assert!(
        main.debug_scopes
            .iter()
            .any(|scope| scope.kind == DebugScopeKind::Block),
        "nested begin should record a block debug scope, got {:?}",
        main.debug_scopes
    );
}

#[test]
fn prefixed_block_local_is_debug_scoped() {
    let module = lower(
        "begin
           class Box;
           begin integer x; x := 0; end;
           integer n;
           n := 0;
           Box begin
             integer k;
             k := 1;
             n := k;
           end;
           n := 2;
         end;",
    );
    let main = module
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main");
    let k = main
        .locals
        .iter()
        .find(|local| local.name.eq_ignore_ascii_case("k"))
        .expect("prefixed-block local k");
    let scope = k
        .debug_scope
        .as_ref()
        .expect("prefixed-block local should carry a debug_scope");
    assert!(
        scope.start < scope.end,
        "prefixed-block debug_scope should be a real source span, got {scope:?}"
    );
}

#[test]
fn lowers_a_void_procedure_with_out_text_side_effect() {
    let module = lower(r#"begin procedure greet; begin OutText("hi"); end; greet; end;"#);
    let greet = find_function(&module, "greet");
    assert_eq!(greet.result, None);
    assert!(greet.params.is_empty());
    assert!(
        greet
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(s.op, Op::CallOutText { .. })),
        "expected greet's body to lower its OutText call"
    );
    assert!(
        greet
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(s.op, Op::Return { value: None })),
        "void procedure should return without a value"
    );

    let main = &module.functions[0];
    assert!(
        main.blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(&s.op, Op::Call { dest: None, name, .. } if name == "greet")),
        "expected a statement-position call with no dest"
    );
}

#[test]
fn lowers_a_procedure_with_multiple_parameters() {
    let module = lower(
        "begin integer procedure add(a, b); value a, b; integer a, b;
            begin add := a + b; end;
         integer z; z := add(2, 3); end;",
    );
    let add = find_function(&module, "add");
    assert_eq!(add.params.len(), 2);
    assert_eq!(add.params[0].name, "a");
    assert_eq!(add.params[1].name, "b");
}

#[test]
fn lowers_a_nested_call_expression() {
    let module = lower(
        "begin integer procedure f(x); value x; integer x; begin f := x + 1; end;
         integer procedure g(x); value x; integer x; begin g := x * 2; end;
         integer z; z := f(g(1)); end;",
    );
    let main = &module.functions[0];
    let calls: Vec<&str> = main
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .filter_map(|s| match &s.op {
            Op::Call { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(calls.contains(&"f"), "expected a call to f: {calls:?}");
    assert!(calls.contains(&"g"), "expected a call to g: {calls:?}");

    // `g`'s call must lower (and thus appear) before `f`'s, since `g(1)`
    // is evaluated to produce `f`'s argument.
    let g_index = calls.iter().position(|&n| n == "g").unwrap();
    let f_index = calls.iter().position(|&n| n == "f").unwrap();
    assert!(g_index < f_index, "g must be called before f: {calls:?}");
}

#[test]
fn inlines_call_by_name_assignment() {
    let module = lower(
        r#"begin integer i;
           procedure set(n); name n; integer n;
           begin n := 7; end;
           set(i); end;"#,
    );
    assert!(
        !module.functions.iter().any(|f| f.name == "set"),
        "name-param procedures should be inlined, not outlined: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::ConstI64 { value: 7, .. })),
        "expected inlined assignment of 7: {ops:?}"
    );
}

#[test]
fn inlines_innerproduct_style_jensen() {
    let module = lower(
        r#"begin integer array a(1:3), b(1:3);
           integer k, i; real y;
           procedure innerproduct(a,b,k,p,y); name p,y,a,b;
             integer k,p; real y,a,b;
           begin real s; integer pp;
             s := 0;
             for pp := 1 step 1 until k do
               begin p := pp; s := s + a * b; end;
             y := s
           end innerproduct;
           for i := 1 step 1 until 3 do
             begin a(i) := i; b(i) := 10 * i; end;
           k := 3;
           innerproduct(a(i), b(i), k, i, y); end;"#,
    );
    assert!(
        !module.functions.iter().any(|f| f.name == "innerproduct"),
        "innerproduct should be inlined"
    );
    let main = &module.functions[0];
    assert!(
        main.blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| { matches!(s.op, Op::Binary { op: BinOp::Mul, .. }) }),
        "expected inlined a*b multiply in main"
    );
}

#[test]
fn outlines_recursive_integer_name_parameter() {
    let module = lower(
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
           y := dec(x); end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name == "dec"),
        "recursive integer name-param procedures should be outlined: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name == "__simrt_name_get_ref"),
        "expected the shared name-thunk get helper to be added: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name == "__simrt_name_set_ref"),
        "expected the shared name-thunk set helper to be added: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let ops: Vec<_> = module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.ops))
        .map(|s| &s.op)
        .collect();
    // Call site (`dec(x)` in main): build the thunk triple from the
    // shared helpers plus `&x`.
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::FuncAddr { name, .. } if name == "__simrt_name_get_ref")),
        "expected FuncAddr of the get helper for the name actual: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::FuncAddr { name, .. } if name == "__simrt_name_set_ref")),
        "expected FuncAddr of the set helper for the name actual: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::LocalAddr { .. })),
        "expected LocalAddr for name actual: {ops:?}"
    );
    // Reads/writes of the name formal inside `dec`'s body go through
    // `call_indirect` on the thunk's `get`/`set`.
    assert!(
        ops.iter().any(|op| matches!(op, Op::CallIndirect { .. })),
        "expected CallIndirect for name-thunk read/write in outlined body: {ops:?}"
    );
    // The shared helpers themselves still bottom out in
    // load.ref.i64/store.ref.i64 on the `env` cell.
    assert!(
        ops.iter().any(|op| matches!(op, Op::LoadRefI64 { .. })),
        "expected LoadRefI64 in the shared get helper: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::StoreRefI64 { .. })),
        "expected StoreRefI64 in the shared set helper: {ops:?}"
    );
}

#[test]
fn outlines_recursive_integer_array_element_name_parameter() {
    // The name actual is an assigned array element `a(1)`: the outlined
    // thunk must go through the shared arr1 get/set helpers (not the
    // plain scalar ones) so the array is aliased end-to-end.
    let module = lower(
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
           y := dec(a(1)); end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name == "dec"),
        "recursive integer name-param procedures should be outlined: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name == "__simrt_name_get_arr1"),
        "expected the shared arr1 get helper to be added: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name == "__simrt_name_set_arr1"),
        "expected the shared arr1 set helper to be added: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let ops: Vec<_> = module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.ops))
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::FuncAddr { name, .. } if name == "__simrt_name_get_arr1")),
        "expected FuncAddr of the arr1 get helper for the array-element name actual: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::FuncAddr { name, .. } if name == "__simrt_name_set_arr1")),
        "expected FuncAddr of the arr1 set helper for the array-element name actual: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::NewObject {
                class_id: crate::layout::NAME_ARR1_ENV_CLASS_ID,
                ..
            }
        )),
        "expected a name-arr1 env object for the array-element name actual: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::ArrayLoad { .. })),
        "expected ArrayLoad in the shared arr1 get helper: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::ArrayStore { .. })),
        "expected ArrayStore in the shared arr1 set helper: {ops:?}"
    );
}

#[test]
fn outlines_recursive_name_with_assigned_remote_field_actual() {
    // Assigned formal `n` bound to `r.x`: per-offset field get/set helpers
    // plus LocalAddr of the object ref local.
    let module = lower(
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
           y := dec(r.x); end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name == "dec"),
        "recursive integer name-param procedures should be outlined: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.starts_with("__simrt_name_get_field_")),
        "expected a per-offset field get helper: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.starts_with("__simrt_name_set_field_")),
        "expected a per-offset field set helper: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let ops: Vec<_> = module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.ops))
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(op, Op::FieldLoadI64 { .. })),
        "expected FieldLoadI64 in the field get helper: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::FieldStoreI64 { .. })),
        "expected FieldStoreI64 in the field set helper: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::NewObject {
                class_id: crate::layout::REF_CELL_CLASS_ID,
                ..
            }
        )),
        "expected a ref_cell home for the object-ref field env: {ops:?}"
    );
}

#[test]
fn outlines_recursive_name_with_readonly_expression_actual() {
    // Formal `n` is only read, so `n - 1` becomes a per-call-site re-eval
    // get helper that captures the enclosing name-thunk formal.
    let module = lower(
        r#"begin integer y;
           integer procedure fact(n); name n; integer n;
           begin
              if n <= 1 then fact := 1 else fact := n * fact(n - 1);
           end;
           y := fact(y); end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name == "fact"),
        "expected outlined fact: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.starts_with("__simrt_name_get_expr_")),
        "expected a per-call-site expression get helper: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name == "__simrt_name_set_readonly"),
        "expected the readonly set helper: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let fact = module
        .functions
        .iter()
        .find(|f| f.name == "fact")
        .expect("fact");
    let ops: Vec<_> = fact
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::FuncAddr { name, .. } if name.starts_with("__simrt_name_get_expr_")
        )),
        "expected FuncAddr of expression get helper: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::NewObject {
                class_id: crate::layout::NAME_PACK_ENV_CLASS_ID,
                ..
            }
        )),
        "expected NAME_PACK_ENV object for expression name actual: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::CallIndirect { .. })),
        "expected CallIndirect for name-thunk read of 'n': {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Call { name, .. } if name == "fact")),
        "expected recursive Call: {ops:?}"
    );
}

#[test]
fn outlines_recursive_name_with_if_expression_actual() {
    let module = lower(
        r#"begin integer i, r;
           integer procedure twice(n, k); name n, k; integer n, k;
           begin
              integer t;
              t := n;
              k := k + 1;
              if n = -999 then twice := twice(n, k) else twice := t + n;
           end;
           i := 0;
           r := twice(if i < 1 then 10 else 20, i); end;"#,
    );
    let helper = module
        .functions
        .iter()
        .find(|f| f.name.starts_with("__simrt_name_get_expr_"))
        .expect("expected if-expression get helper");
    assert!(
        helper.blocks.len() > 1,
        "if-expression helper should use multiple blocks, got {}",
        helper.blocks.len()
    );
    let ops: Vec<_> = helper
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(op, Op::Branch { .. })),
        "expected Branch in if-expression helper: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::Compare { .. })),
        "expected Compare for if condition: {ops:?}"
    );
}

#[test]
fn outlines_readonly_expression_actual_reevals_free_var() {
    // Name formal `k` aliases `i`; mutating `k` must be visible to the
    // re-eval get thunk for expression actual `i + 1` (bound to `n`).
    let module = lower(
        r#"begin integer i, r;
           integer procedure twice(n, k); name n, k; integer n, k;
           begin
              integer t;
              t := n;
              k := k + 10;
              if n = -999 then twice := twice(n, k) else twice := t + n;
           end;
           i := 1;
           r := twice(i + 1, i); end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.starts_with("__simrt_name_get_expr_")),
        "expected expression get helper: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let helper = module
        .functions
        .iter()
        .find(|f| f.name.starts_with("__simrt_name_get_expr_"))
        .expect("expr helper");
    let ops: Vec<_> = helper
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(op, Op::LoadRefI64 { .. })),
        "helper should reload free locals from env: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Binary { op: BinOp::Add, .. })),
        "helper should re-evaluate i+1: {ops:?}"
    );
}

#[test]
fn outlines_readonly_remote_field_expression_actual() {
    // Expression actual `r.x` (and `r.x - 1` via recursion on the formal)
    // must capture the object pointer and FieldLoad in the get helper.
    let module = lower(
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
        end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.starts_with("__simrt_name_get_expr_")),
        "expected expression get helper: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let helper = module
        .functions
        .iter()
        .find(|f| f.name.starts_with("__simrt_name_get_expr_"))
        .expect("expr helper");
    let ops: Vec<_> = helper
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| {
            matches!(op, Op::FieldLoadI64 { .. }) || matches!(op, Op::CallIndirect { .. })
        }),
        "helper should load the remote integer field (FieldLoad or name-thunk CallIndirect): {ops:?}"
    );
}

#[test]
fn outlines_expression_actual_with_object_and_name_formal_captures() {
    // `r.x - 1` captures the object; `n - 1` inside the body captures the
    // name formal as a thunk pair. Both must live in NAME_PACK_ENV slots
    // (not i64 words) so WasmGC does not see a leftover handle Copy.
    let module = lower(
        r#"begin
           class C; begin integer x; end;
           ref(C) r;
           integer y;
           integer procedure fact(n); name n; integer n;
           begin
              if n <= 1 then fact := 1 else fact := n * fact(n - 1);
           end;
           r :- new C;
           r.x := 4;
           y := fact(r.x - 1);
        end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .any(|f| f.name.starts_with("__simrt_name_get_expr_")),
        "expected expression get helpers: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let ops: Vec<_> = module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.ops))
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::NewObject {
                class_id: crate::layout::NAME_PACK_ENV_CLASS_ID,
                ..
            }
        )),
        "expected NAME_PACK_ENV for mixed object/thunk captures: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::NewObject {
                class_id: crate::layout::NAME_THUNK_PAIR_CLASS_ID,
                ..
            }
        )),
        "expected NAME_THUNK_PAIR for the nested name-formal capture: {ops:?}"
    );
}

#[test]
fn errors_on_assigned_name_with_expression_actual() {
    let error = lower_err(
        r#"begin integer y;
           integer procedure bump(x); name x; integer x;
           begin
              x := x + 1;
              if x < 3 then bump := bump(x) else bump := x;
           end;
           y := bump(y + 1); end;"#,
    );
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("simple")
            || error.message.contains("thunk")
            || error.message.contains("expression")
            || error.message.contains("assigned"),
        "message was: {}",
        error.message
    );
}

#[test]
fn errors_on_recursive_name_parameter_procedure() {
    // Non-outline-eligible recursion (would need expression thunks) still
    // hard-errors when forced through the inliner — use a text name formal
    // so the outline gate fails.
    let error = lower_err(
        r#"begin text t;
           procedure f(x); name x; text x;
           begin f(x); end;
           f(t); end;"#,
    );
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("recursive call-by-name") || error.message.contains("not supported"),
        "message was: {}",
        error.message
    );
}

#[test]
fn outlines_array_reference_parameter() {
    let module = lower(
        r#"begin integer array a(1:2);
           procedure set(x); integer array x; begin x(1) := 99; end;
           set(a); end;"#,
    );
    assert!(
        module.functions.iter().any(|f| f.name == "set"),
        "array-reference procedures should be outlined: {:?}",
        module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    let set = module
        .functions
        .iter()
        .find(|f| f.name == "set")
        .expect("set");
    assert_eq!(set.params.len(), 1);
    assert_eq!(set.params[0].ty, MirType::ArrayI64);
    assert!(
        module.functions[0]
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| { matches!(&s.op, Op::Call { name, .. } if name == "set") }),
        "expected Call to set in main"
    );
}

#[test]
fn text_reference_parameter_never_rebinds_the_actual() {
    // §4.6.3: a call-by-reference text formal is a *local variable* holding
    // a copy of the reference. The formal is bound at the call site (the
    // procedure inlines so the copy can be materialized in the caller's
    // frame), and `x :- copy("hi")` rebinds only that local — the actual
    // `t` is never the destination of a reference assignment.
    let module = lower(
        r#"begin text t;
           procedure set(x); text x; begin x :- copy("hi"); end;
           set(t); end;"#,
    );
    let main = &module.functions[0];
    assert_eq!(main.name, "main");
    let actual = LocalId(
        main.locals
            .iter()
            .position(|local| local.name == "t")
            .expect("caller text local 't'"),
    );
    let rebinds_actual = main
        .blocks
        .iter()
        .flat_map(|block| &block.ops)
        .any(|stmt| matches!(&stmt.op, Op::TextRefAssign { dest, .. } if *dest == actual));
    assert!(
        !rebinds_actual,
        "`x :- copy(...)` must rebind the formal's local copy, not the actual 't'"
    );
    // The copy still happens: the body's `copy("hi")` is materialized and
    // reference-assigned to some other (formal) local.
    assert!(
        main.blocks
            .iter()
            .flat_map(|block| &block.ops)
            .any(|stmt| matches!(&stmt.op, Op::TextRefAssign { dest, .. } if *dest != actual)),
        "expected the formal's local to be rebound by `x :- copy(...)`"
    );
}

#[test]
fn outlines_value_text_parameter_with_copy() {
    let module = lower(
        r#"begin text t;
           procedure show(x); value x; text x; begin OutText(x); OutImage; end;
           t :- copy("hi");
           show(t); end;"#,
    );
    let show = module
        .functions
        .iter()
        .find(|f| f.name == "show")
        .expect("value text procedures should be outlined");
    assert_eq!(show.params.len(), 1);
    assert_eq!(show.params[0].ty, MirType::Text);
    let ops: Vec<_> = module.functions[0]
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Call { name, .. } if name == "show")),
        "expected Call to show: {ops:?}"
    );
    let call_idx = ops
        .iter()
        .position(|op| matches!(op, Op::Call { name, .. } if name == "show"))
        .expect("Call to show");
    assert!(
        ops[..call_idx]
            .iter()
            .rev()
            .take_while(|op| !matches!(op, Op::TextRefAssign { .. }))
            .any(|op| matches!(op, Op::TextCopy { .. })),
        "§4.6.2: a value text actual is copied before the call: {ops:?}"
    );
}

#[test]
fn value_array_parameter_emits_array_copy() {
    let module = lower(
        r#"begin integer array a(1:2);
           procedure set(x); value x; integer array x; begin x(1) := 99; end;
           set(a); end;"#,
    );
    assert!(
        module
            .functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.ops)
            .any(|s| matches!(s.op, Op::ArrayCopy { .. })),
        "expected ArrayCopy for value array formal"
    );
}

#[test]
fn name_assignment_to_non_variable_actual_lowers_into_a_temp() {
    // Wave 3: a call-by-name formal whose actual is not itself a
    // variable (`a + 1` here) no longer rejects the whole compile.
    // `resolve_name_actual_place` evaluates the actual once into a
    // fresh temp and treats that as the (unaliased) assignment target,
    // so `x := 1` inside `f` lowers to a `StoreLocal` into that temp
    // rather than mutating `a`.
    let module = lower(
        r#"begin integer a;
           procedure f(x); name x; integer x; begin x := 1; end;
           f(a + 1); end;"#,
    );
    let main_local_id = |name: &str| -> usize {
        module.functions[0]
            .locals
            .iter()
            .position(|local| local.name == name)
            .expect("local should exist")
    };
    let a_id = main_local_id("a");
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Binary { op: BinOp::Add, .. })),
        "expected the `a + 1` actual to still be evaluated: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::StoreLocal { local, .. } if local.0 == a_id)),
        "assignment through the name formal must not write back to `a`: {ops:?}"
    );
}

#[test]
fn errors_on_unknown_procedure_call() {
    let error = lower_err("begin integer x; x := NoSuchProc(1); end;");
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("NoSuchProc"),
        "message was: {}",
        error.message
    );
}

#[test]
fn lowers_detach_call_roundtrip_to_a_component_entry() {
    let module = lower(
        r#"begin
           class C; begin
               OutText("A"); OutImage;
               detach;
               OutText("B"); OutImage;
           end;
           ref(C) x; x :- new C; call(x);
        end;"#,
    );
    assert_component_entry_detaches(&module, "C");
    let main_ops: Vec<_> = module.functions[0]
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        main_ops
            .iter()
            .any(|op| matches!(op, Op::SeqObjectCreate { .. })),
        "new C should create a component: {main_ops:?}"
    );
    assert!(
        main_ops.iter().any(|op| matches!(op, Op::SeqCall { .. })),
        "call(x) should be a transfer, not a re-entered initializer: {main_ops:?}"
    );
}

/// The class body is the component's entry: whatever control flow the
/// `detach` sits inside, it stays a transfer op in `C$__coro` rather than
/// being flattened into a resumable state machine.
fn assert_component_entry_detaches(module: &Module, class_name: &str) {
    let entry_name = format!("{class_name}$__coro");
    let entry = module
        .functions
        .iter()
        .find(|f| f.name == entry_name)
        .unwrap_or_else(|| {
            panic!(
                "{entry_name} not found; module has {:?}",
                module
                    .functions
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
            )
        });
    let ops: Vec<_> = entry
        .blocks
        .iter()
        .flat_map(|b| &b.ops)
        .map(|s| &s.op)
        .collect();
    assert!(
        ops.iter().any(|op| matches!(op, Op::SeqDetach { .. })),
        "{entry_name} should detach: {ops:?}"
    );
}

#[test]
fn a_detach_under_an_if_makes_a_component() {
    let module = lower(
        r#"begin
           class C; begin
               OutText("A"); OutImage;
               if true then detach;
               OutText("B"); OutImage;
           end;
           ref(C) x; x :- new C; call(x);
        end;"#,
    );
    let layout = module
        .class_layouts
        .iter()
        .find(|l| l.name == "C")
        .expect("C layout");
    assert!(layout.runs_on_own_stack);
}

#[test]
fn lowers_if_then_compound_detach_to_a_component_entry() {
    let module = lower(
        r#"begin
           class C; begin
               if true then begin
                   OutText("A"); OutImage;
                   detach;
                   OutText("B"); OutImage;
               end;
               OutText("C"); OutImage;
           end;
           ref(C) x; x :- new C; call(x);
        end;"#,
    );
    assert_component_entry_detaches(&module, "C");
}

/// The splitter could not express this at all — a `detach` in a loop has no
/// single continuation index. On its own stack it is just a transfer.
#[test]
fn lowers_detach_inside_while_to_a_component_entry() {
    let module = lower(
        r#"begin
           class C; begin
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
           ref(C) x; x :- new C; call(x);
        end;"#,
    );
    assert_component_entry_detaches(&module, "C");
}

#[test]
fn lowers_simulation_hold_to_sim_ops() {
    let module = lower(
        r#"Simulation begin
           hold(1.0);
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::SimBegin)),
        "expected SimBegin: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::SimHold { .. })),
        "expected SimHold: {ops:?}"
    );
}

#[test]
fn errors_clearly_on_hold_builtin_outside_simulation() {
    let error = lower_err("begin hold(1.0); end;");
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("hold"),
        "message was: {}",
        error.message
    );
}

#[test]
fn errors_clearly_on_wait_in_simulation_main() {
    let error = lower_err(
        r#"Simulation begin
            ref(head) q; q :- new head;
            wait(q);
        end;"#,
    );
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.to_ascii_lowercase().contains("wait"),
        "message was: {}",
        error.message
    );
}

#[test]
fn lowers_wait_in_process_body_to_simset_into_and_passivate() {
    let module = lower(
        r#"Simulation
        begin
            ref(head) q; q :- new head;
            process class Worker;
            begin
                wait(q);
            end;
            ref(Worker) w; w :- new Worker;
        end;"#,
    );
    let all_ops: Vec<&Op> = module
        .functions
        .iter()
        .flat_map(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| block.ops.iter().map(|spanned| &spanned.op))
        })
        .collect();
    assert!(
        all_ops.iter().any(|op| matches!(op, Op::SimsetInto { .. })),
        "expected SimsetInto: {all_ops:?}"
    );
    assert!(
        all_ops.iter().any(|op| matches!(op, Op::SimPassivate)),
        "expected SimPassivate after wait: {all_ops:?}"
    );
    assert!(
        all_ops
            .iter()
            .any(|op| matches!(op, Op::SimsetInitHead { .. })),
        "expected SimsetInitHead for new head: {all_ops:?}"
    );
    assert!(
        all_ops
            .iter()
            .any(|op| matches!(op, Op::SimsetSetHeadClassId { .. })),
        "expected SimsetSetHeadClassId: {all_ops:?}"
    );
    // `q` is snapshotted onto Worker at `new`.
    let main = &module.functions[0];
    let main_ops: Vec<&Op> = main
        .blocks
        .iter()
        .flat_map(|block| block.ops.iter().map(|spanned| &spanned.op))
        .collect();
    assert!(
        main_ops
            .iter()
            .any(|op| matches!(op, Op::FieldStoreI64 { .. })),
        "expected FieldStore of enclosing capture at new: {main_ops:?}"
    );
}

#[test]
fn errors_using_void_procedure_result_in_expression() {
    // A bare `greet` (no parens) parses as a variable reference, not a
    // call, so use a one-argument void procedure to force `greet(1)` to
    // parse as `ExprKind::FunctionCall` and exercise the "void result
    // used as a value" check.
    let error = lower_err(
        r#"begin procedure greet(n); integer n; begin OutText("hi"); end;
           integer x; x := greet(1); end;"#,
    );
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("does not return a value"),
        "message was: {}",
        error.message
    );
}

#[test]
fn dump_shows_procedure_signature_and_result_type() {
    let module =
        lower("begin integer procedure f(x); value x; integer x; begin f := x + 1; end; end;");
    let dump = module.dump();
    assert!(dump.contains("fn f(x: i64) -> i64"), "dump was:\n{dump}");
}

// --- 1-D integer arrays -------------------------------------------------

#[test]
fn lowers_array_declaration_to_alloc_array() {
    let module = lower("begin integer array a(1:10); end;");
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AllocArray { .. })),
        "expected an AllocArray op: {ops:?}"
    );
    let array_local = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "a")
        .expect("expected a local named 'a'");
    assert_eq!(array_local.ty, MirType::ArrayI64);
}

#[test]
fn lowers_array_store_via_assignment_lhs() {
    let module = lower("begin integer array a(1:10); a(1) := 42; end;");
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::ArrayStore { .. })),
        "expected an ArrayStore op: {ops:?}"
    );
}

// `a(i)` on the RHS of an expression parses as `ExprKind::FunctionCall`
// (see `parse::variable`'s doc comment), so this also exercises the
// procedure-call/array-read disambiguation fallback.
#[test]
fn lowers_array_load_via_expression_read() {
    let module = lower("begin integer array a(1:10); integer x; a(1) := 42; x := a(1); end;");
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::ArrayLoad { .. })),
        "expected an ArrayLoad op: {ops:?}"
    );
}

#[test]
fn lowers_two_dimensional_array_declaration() {
    let module = lower("begin integer array m(1:2, 1:2); end;");
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::AllocArray { bounds, .. } if bounds.len() == 2)),
        "expected a 2-D AllocArray op: {ops:?}"
    );
}

#[test]
fn lowers_two_dimensional_array_store_and_load() {
    let module =
        lower("begin integer array m(1:2, 1:2); m(2, 1) := 7; integer x; x := m(2, 1); end;");
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::ArrayStore { indices, .. } if indices.len() == 2)),
        "expected a 2-D ArrayStore op: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::ArrayLoad { indices, .. } if indices.len() == 2)),
        "expected a 2-D ArrayLoad op: {ops:?}"
    );
}

#[test]
fn lowers_real_array_declaration() {
    let module = lower("begin real array a(1:5); end;");
    let array_local = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "a")
        .expect("expected local a");
    assert_eq!(array_local.ty, MirType::ArrayF64);
}

#[test]
fn lowers_text_array_declaration() {
    let module = lower("begin text array a(1:5); end;");
    let array_local = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "a")
        .expect("expected a local named 'a'");
    assert_eq!(array_local.ty, MirType::ArrayText);
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AllocArray { .. })),
        "expected AllocArray: {ops:?}"
    );
}

#[test]
fn errors_on_boolean_subscript() {
    let error = lower_err("begin integer array a(1:5); boolean b; integer x; x := a(b); end;");
    assert_eq!(error.phase, Phase::Codegen);
    assert!(
        error.message.contains("integer expression"),
        "message was: {}",
        error.message
    );
}

#[test]
fn errors_on_indexing_a_non_array_variable() {
    let error = lower_err("begin integer a; integer x; x := a(1); end;");
    assert_eq!(error.phase, Phase::Codegen);
    // `a(1)` on a non-array, non-procedure `a` falls through every
    // disambiguation branch and reports the (accurate, if slightly
    // indirect) "unknown procedure" diagnosis rather than panicking.
    let hay = format!("{} {}", error.message, error.notes.join(" "));
    assert!(
        hay.contains("unknown procedure") || hay.contains("not lowered"),
        "message was: {} notes={:?}",
        error.message,
        error.notes
    );
}

#[test]
fn dump_shows_array_locals_and_ops() {
    let module = lower("begin integer array a(1:10); a(1) := 42; end;");
    let dump = module.dump();
    assert!(dump.contains("array.i64"), "dump was:\n{dump}");
    assert!(dump.contains("alloc_array"), "dump was:\n{dump}");
    assert!(dump.contains("array_store"), "dump was:\n{dump}");
}

// --- text ---------------------------------------------------------------

#[test]
fn lowers_text_declaration_to_notext() {
    let module = lower("begin text t; end;");
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextNotext { .. })),
        "expected a TextNotext op: {ops:?}"
    );
    let text_local = module.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "t")
        .expect("expected a local named 't'");
    assert_eq!(text_local.ty, MirType::Text);
}

#[test]
fn lowers_string_literal_expression_to_text_from_literal() {
    let module = lower(r#"begin text t; t := "hi"; end;"#);
    assert_eq!(module.strings, vec!["hi".to_string()]);
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::TextFromLiteral { string_id: 0, .. })),
        "expected TextFromLiteral: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextAssign { .. })),
        "expected TextAssign: {ops:?}"
    );
}

#[test]
fn lowers_text_concat_expression() {
    let module = lower(r#"begin text t; t := "a" & "b"; end;"#);
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextConcat { .. })),
        "expected TextConcat: {ops:?}"
    );
}

#[test]
fn lowers_out_text_with_text_variable() {
    let module = lower(r#"begin text t; t := "hello"; OutText(t); OutImage; end;"#);
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::CallOutTextLocal { .. })),
        "expected CallOutTextLocal: {ops:?}"
    );
}

#[test]
fn dump_shows_text_locals_and_ops() {
    let module = lower(r#"begin text t; t := "x"; OutText(t); end;"#);
    let dump = module.dump();
    assert!(dump.contains("text"), "dump was:\n{dump}");
    assert!(dump.contains("text.notext"), "dump was:\n{dump}");
    assert!(dump.contains("text.literal"), "dump was:\n{dump}");
    assert!(dump.contains("text.assign"), "dump was:\n{dump}");
    assert!(dump.contains("call out_text %"), "dump was:\n{dump}");
}

#[test]
fn lowers_blanks_call() {
    let module = lower("begin text t; t :- blanks(3); end;");
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextBlanks { .. })),
        "expected TextBlanks: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextRefAssign { .. })),
        "expected TextRefAssign for ':-': {ops:?}"
    );
}

#[test]
fn lowers_copy_call() {
    let module = lower(r#"begin text t, u; t :- "hi"; u :- copy(t); end;"#);
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextCopy { .. })),
        "expected TextCopy: {ops:?}"
    );
}

#[test]
fn lowers_blanks_case_insensitive() {
    let module = lower("begin text t; t :- Blanks(0); end;");
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextBlanks { .. })),
        "expected TextBlanks for Blanks: {ops:?}"
    );
}

#[test]
fn lowers_text_content_equality() {
    let module = lower(r#"begin text a, b; a := "x"; b := "x"; if a = b then OutText("y"); end;"#);
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextContentEq { .. })),
        "expected TextContentEq: {ops:?}"
    );
}

#[test]
fn lowers_text_content_inequality_as_not_eq() {
    let module = lower(r#"begin text a, b; a := "x"; b := "y"; if a <> b then OutText("y"); end;"#);
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextContentEq { .. })),
        "expected TextContentEq: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Unary { op: UnOp::Not, .. })),
        "expected Not for '<>': {ops:?}"
    );
}

#[test]
fn dump_shows_blanks_and_ref_assign() {
    let module = lower("begin text t; t :- blanks(2); end;");
    let dump = module.dump();
    assert!(dump.contains("text.blanks"), "dump was:\n{dump}");
    assert!(dump.contains("text.ref_assign"), "dump was:\n{dump}");
}

#[test]
fn lowers_text_length_pos_more() {
    let module = lower(
        r#"begin
            text t;
            integer n;
            boolean b;
            t :- "ab";
            n := t.length;
            n := t.pos;
            b := t.more;
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextLength { .. })),
        "expected TextLength: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextPos { .. })),
        "expected TextPos: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextMore { .. })),
        "expected TextMore: {ops:?}"
    );
}

#[test]
fn lowers_text_setpos_and_getchar() {
    let module = lower(
        r#"begin
            text t;
            character c;
            t :- "ab";
            t.setpos(1);
            c := t.getchar;
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextSetpos { .. })),
        "expected TextSetpos: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextGetchar { .. })),
        "expected TextGetchar: {ops:?}"
    );
}

#[test]
fn lowers_text_getint_and_putint() {
    let module = lower(
        r#"begin
            text amount, payment;
            integer pay;
            amount :- " 1200";
            pay := amount.getint;
            payment :- blanks(8);
            payment.putint(pay);
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextGetint { .. })),
        "expected TextGetint: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextPutint { .. })),
        "expected TextPutint: {ops:?}"
    );
}

#[test]
fn lowers_character_literal_to_const_i64() {
    let module = lower(
        r#"begin
            character c;
            c := 'A';
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::ConstI64 { value: 65, .. })),
        "expected ConstI64(65) for 'A': {ops:?}"
    );
}

#[test]
fn lowers_text_constant_start_main() {
    let module = lower(
        r#"begin
            text t, u;
            boolean c;
            integer s;
            t :- "ab";
            c := t.constant;
            s := t.start;
            u :- t.main;
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextConstant { .. })),
        "expected TextConstant: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextStart { .. })),
        "expected TextStart: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextMain { .. })),
        "expected TextMain: {ops:?}"
    );
}

#[test]
fn lowers_text_putchar() {
    let module = lower(
        r#"begin
            text t;
            t :- blanks(2);
            t.setpos(1);
            t.putchar('X');
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextPutchar { .. })),
        "expected TextPutchar: {ops:?}"
    );
}

#[test]
fn paren_text_putchar_uses_descriptor_copy() {
    // `(t).putchar` must not mutate `t.pos`: lower a notext+ref_assign
    // temp before TextPutchar (expression receiver, Standard text values).
    let module = lower(
        r#"begin
            text t;
            t :- blanks(2);
            t.setpos(1);
            (t).putchar('X');
        end;"#,
    );
    let paren_ops = ops(&module);
    let put_idx = paren_ops
        .iter()
        .position(|op| matches!(op, Op::TextPutchar { .. }))
        .expect("expected TextPutchar");
    assert!(
        paren_ops[..put_idx]
            .iter()
            .any(|op| matches!(op, Op::TextRefAssign { .. })),
        "expected TextRefAssign before TextPutchar for (t).putchar: {paren_ops:?}"
    );
    // Bare `t.putchar` must keep mutating the variable local directly.
    let direct = lower(
        r#"begin
            text t;
            t :- blanks(2);
            t.setpos(1);
            t.putchar('X');
        end;"#,
    );
    let direct_ops = ops(&direct);
    let direct_put = direct_ops
        .iter()
        .position(|op| matches!(op, Op::TextPutchar { .. }))
        .expect("expected TextPutchar");
    assert!(
        !direct_ops[..direct_put]
            .iter()
            .rev()
            .take(3)
            .any(|op| matches!(op, Op::TextRefAssign { .. })),
        "bare t.putchar should not insert a fresh descriptor copy: {direct_ops:?}"
    );
}

#[test]
fn lowers_text_sub_and_strip() {
    let module = lower(
        r#"begin
            text t, sub, stripped;
            t :- "abc   ";
            sub :- t.sub(2, 2);
            stripped :- t.strip;
        end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextSub { .. })),
        "expected TextSub: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| matches!(op, Op::TextStrip { .. })),
        "expected TextStrip: {ops:?}"
    );
}

#[test]
fn dump_shows_text_attribute_ops() {
    let module = lower(r#"begin text t; integer n; t :- "x"; n := t.length; t.setpos(1); end;"#);
    let dump = module.dump();
    assert!(dump.contains("text.length"), "dump was:\n{dump}");
    assert!(dump.contains("text.setpos"), "dump was:\n{dump}");
}

#[test]
fn lowers_flat_goto_to_jump() {
    let module = lower(r#"begin integer x; x := 0; goto done; x := 99; done: x := 42; end;"#);
    let function = &module.functions[0];
    let jumps: Vec<_> = function
        .blocks
        .iter()
        .flat_map(|b| b.ops.iter())
        .filter_map(|s| match &s.op {
            Op::Jump { target } => Some(*target),
            _ => None,
        })
        .collect();
    assert!(
        jumps.len() >= 2,
        "expected goto + label fallthrough jumps, got {jumps:?} in:\n{}",
        module.dump()
    );
    // The `done` label block should contain a store of 42.
    let has_done_store = function.blocks.iter().any(|block| {
        block.ops.iter().any(|s| {
            matches!(
                &s.op,
                Op::ConstI64 { value: 42, .. } | Op::StoreLocal { .. }
            )
        })
    });
    assert!(has_done_store, "dump:\n{}", module.dump());
}

#[test]
fn lowers_goto_into_labelled_if_branch() {
    let module = lower(
        r#"begin integer x; x := 0; goto target; if true then target: x := 1 else x := 2; end;"#,
    );
    assert!(
        module.functions[0]
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(s.op, Op::Jump { .. })),
        "dump:\n{}",
        module.dump()
    );
}

#[test]
fn lowers_goto_via_switch_designator() {
    let module = lower(r#"begin switch s := L; goto s(1); L: ; end;"#);
    assert!(
        module.functions[0]
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(s.op, Op::Branch { .. })),
        "expected switch dispatch branches, dump:\n{}",
        module.dump()
    );
}

#[test]
fn inlines_label_formal_goto() {
    let module = lower(
        r#"begin
            procedure P(L); label L; begin goto L end;
            P(done);
            done: ;
         end;"#,
    );
    assert!(
        module.functions[0]
            .blocks
            .iter()
            .flat_map(|b| &b.ops)
            .any(|s| matches!(s.op, Op::Jump { .. })),
        "expected jump for formal label goto, dump:\n{}",
        module.dump()
    );
}

#[test]
fn lowers_parameterless_method_remote_attribute() {
    let module = lower(
        r#"begin
            class Person;
            begin
                text pname;
                ref(Person) procedure Mother; begin Mother :- this Person end;
                text t;
                t := Mother.pname;
            end;
            ref(Person) p;
            p :- new Person;
         end;"#,
    );
    assert!(
        !module.functions.is_empty(),
        "expected lowered module for Mother.pname"
    );
}

#[test]
fn inspect_directfile_outchar_uses_basicio_not_sysout() {
    let module = lower(
        r#"begin
            inspect new DirectFile("f") do begin
               outchar('X'); outimage;
            end;
         end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::CallBasicioOutChar { .. })),
        "expected CallBasicioOutChar: {ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::CallOutChar { .. })),
        "free CallOutChar must not appear: {ops:?}"
    );
}

#[test]
fn inspect_directfile_for_loop_outint_uses_basicio() {
    let module = lower(
        r#"begin
            inspect new DirectFile("f") do begin
               integer i;
               for i := 1 step 1 until 2 do begin
                  outint(i, 4); outimage;
               end;
            end;
         end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter()
            .filter(|op| matches!(op, Op::CallBasicioOutInt { .. }))
            .count()
            >= 1,
        "expected CallBasicioOutInt in for loop: {ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::CallOutInt { .. })),
        "free CallOutInt must not appear: {ops:?}"
    );
}

#[test]
fn inspect_directfile_outint_uses_basicio_not_sysout() {
    let module = lower(
        r#"begin
            inspect new DirectFile("f") do begin
               outint(1, 0); outimage;
            end;
         end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::CallBasicioOutInt { .. })),
        "expected CallBasicioOutInt: {ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::CallOutInt { .. })),
        "free CallOutInt must not appear in inspect DirectFile: {ops:?}"
    );
}

#[test]
fn lowers_inspect_new_directfile_locate() {
    let module = lower(
        r#"begin
            inspect new DirectFile("f") do begin
               locate(1); outint(1, 2); outimage;
            end;
         end;"#,
    );
    let ops = ops(&module);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::CallBasicioLocate { .. })),
        "expected CallBasicioLocate: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| { matches!(op, Op::CallBasicioOutInt { .. } | Op::CallOutInt { .. }) }),
        "expected outint call: {ops:?}"
    );
}

#[test]
fn lowers_entier_call() {
    let module = lower(r#"begin integer n; n := entier(3.7); end;"#);
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::F64ToI64 { .. })),
        "expected F64ToI64 for entier: {:?}",
        ops(&module)
    );
}

#[test]
fn inlines_formal_procedure_parameter() {
    let module = lower(
        r#"begin
            integer procedure twice(x); integer x; begin twice := 2 * x; end;
            procedure apply(f, n); integer procedure f; integer n;
            begin OutInt(f(n), 0); OutImage; end;
            apply(twice, 5);
           end;"#,
    );
    let dump = module.dump();
    assert!(
        !dump.contains("function apply"),
        "formal-procedure procedures should be inlined, dump:\n{dump}"
    );
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::Call { name, .. } if name == "twice")),
        "expected inlined call to twice: {:?}",
        ops(&module)
    );
}

#[test]
fn inlines_name_mode_array_formal_element_store() {
    let module = lower(
        r#"begin
            boolean array bva1(0:0);
            procedure Q(bfa1); name bfa1; boolean array bfa1;
            begin bfa1(0) := false; end;
            bva1(0) := true;
            Q(bva1);
           end;"#,
    );
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::ArrayStore { .. })),
        "expected ArrayStore via name-array formal: {:?}",
        ops(&module)
    );
}

#[test]
fn inlines_name_array_formal_from_remote_class_attribute() {
    let module = lower(
        r#"begin
            class A; begin boolean array bva1(0:0); end;
            ref(A) rav;
            procedure Q(bfa1); name bfa1; boolean array bfa1;
            begin bfa1(0) := false; end;
            rav :- new A;
            Q(rav.bva1);
           end;"#,
    );
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::ArrayStore { .. })),
        "expected ArrayStore via remote array name actual: {:?}",
        ops(&module)
    );
}

#[test]
fn name_actual_remote_survives_formal_name_shadowing() {
    // Formal `rav` must not hide outer `rav` while re-evaluating name
    // actual `rav.tva1` (simtst30 test1arrayreference pattern).
    let module = lower(
        r#"begin
            class A; begin text array tva1(0:0); end;
            ref(A) rav;
            text array tfa1(0:0);
            procedure check(tt, tv); name tt; text array tt, tv;
            begin if tt(0) == tv(0) then; end;
            rav :- new A;
            check(rav.tva1, tfa1);
           end;"#,
    );
    let _ = module;
}

#[test]
fn inlines_formal_procedure_bound_to_remote_method() {
    let module = lower(
        r#"begin
            ref(A) x; real ar;
            class A;
            begin
                real procedure Q; Q := 2.5;
                procedure T(R); name R; real R; begin ar := R * Q; end;
            end;
            procedure S(P, B); name P, B; procedure P; real B;
            begin P(x.Q); end;
            x :- new A;
            S(x.T, x.Q);
           end;"#,
    );
    let dump = module.dump();
    assert!(
        !dump.contains("function S"),
        "S should be inlined, dump:\n{dump}"
    );
    assert!(
        dump.contains("A$T") || dump.contains("$T"),
        "expected method T call via formal proc: {dump}"
    );
}

#[test]
fn case_insensitive_call_to_name_param_procedure() {
    let module = lower(
        r#"begin
            integer i;
            procedure P(j); name j; integer j; begin j := j + 1; end;
            p(i);
           end;"#,
    );
    let _ = module;
}

#[test]
fn inlines_formal_procedure_bound_to_inspect_method() {
    let module = lower(
        r#"begin
            integer i;
            procedure P(Q); procedure Q; begin Q(i); end;
            class A;
            begin
                procedure R(k); name k; integer k; begin k := k + k; end;
                integer j;
            end;
            ref(A) x;
            i := 1;
            x :- new A;
            inspect x do P(R);
            P(x.R);
           end;"#,
    );
    let dump = module.dump();
    assert!(
        dump.contains("A$R") || dump.contains("$R"),
        "expected method R via formal proc: {dump}"
    );
}

#[test]
fn outlines_recursive_formal_procedure_mutual_recursion() {
    // simtst34 shape: P ↔ P2 with formal procedure F and boolean name a.
    let module = lower(
        r#"begin
            boolean found_error;
            boolean bool;
            procedure P(F, a); name F, a; procedure F; boolean a;
            begin
                a := not a;
                if a then P2(F) else F;
            end;
            procedure P2(F); procedure F;
            begin
                boolean a;
                a := true;
                P(F, a);
                bool := true;
                if bool then P(Q1, bool) else P(Q2, bool);
            end;
            procedure Q1;
            begin
                if bool then found_error := true;
            end;
            integer procedure Q2;
            begin
                if bool then Q2 := 1 else found_error := true;
            end;
            if bool then P(Q1, bool) else P(Q2, bool);
           end;"#,
    );
    let dump = module.dump();
    assert!(
        dump.contains("fn P("),
        "recursive formal-proc P should outline: {dump}"
    );
    assert!(
        dump.contains("fn P2("),
        "recursive formal-proc P2 should outline: {dump}"
    );
    assert!(
        dump.contains("funcref") || dump.contains("__simrt_fp_invoke_"),
        "expected formal-proc FuncRef / invoke shim: {dump}"
    );
}

#[test]
fn outlines_recursive_name_procedure_with_free_enclosing_integer() {
    let module = lower(
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
           end;"#,
    );
    let dump = module.dump();
    assert!(
        dump.contains("fn P("),
        "recursive name procedure with free i should outline: {dump}"
    );
}

#[test]
fn outlines_recursive_object_ref_alias_procedure() {
    let module = lower(
        r#"begin
            class C; begin integer n; end;
            boolean procedure Ok(f1, f2, l);
                ref(C) f1, f2; integer l;
            if l > f1.n then Ok := true
            else Ok := Ok(f1, f2, l + 1);
            ref(C) a, b;
            a :- new C; b :- new C;
            if Ok(a, b, 1) then;
           end;"#,
    );
    let dump = module.dump();
    assert!(
        dump.contains("fn Ok(") || dump.contains("fn OK("),
        "recursive ObjectRef alias procedure should outline: {dump}"
    );
}

#[test]
fn lowers_remote_array_attribute_element_access() {
    let module = lower(
        r#"begin
            class Innfil; begin text array linjer(1:2); integer lnr; end;
            ref(Innfil) f1;
            boolean b;
            f1 :- new Innfil;
            b := f1.linjer(1) = f1.linjer(1);
           end;"#,
    );
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::ArrayLoad { .. } | Op::FieldLoadI64 { .. })),
        "expected remote array element access: {:?}",
        ops(&module)
    );
}

#[test]
fn name_ref_actual_from_type_procedure_keeps_object_qual() {
    let module = lower(
        r#"begin
            ref(A) x, v;
            class A;
            begin
                ref(A) procedure Z;
                begin Z :- v :- new A; end;
                integer i;
                i := 5;
            end;
            procedure P(y); name y; ref(A) y;
            begin y.i := y.i + 2; end;
            x :- new A;
            P(x.Z);
           end;"#,
    );
    let _ = module;
}

#[test]
fn name_array_formal_shadows_same_named_procedure() {
    // Case-insensitive: formal `r` must not resolve as procedure `R`.
    let module = lower(
        r#"begin
            real array ra(0:0);
            procedure R(a); name a; real array a; begin a(0) := 1.0; end;
            procedure check(r); name r; real array r;
            begin if r(0) = 0.0 then; end;
            ra(0) := 0.0;
            check(ra);
            R(ra);
           end;"#,
    );
    let _ = module;
}

#[test]
fn qua_refines_unqualified_simset_pred_for_remote_field() {
    let module = lower(
        r#"begin
            SIMSET begin
                Link class Bead(i); integer i;;
                ref(Head) chain; integer k;
                chain :- new Head;
                new Bead(1).into(chain);
                k := chain.pred qua Bead.i;
            end;
           end;"#,
    );
    assert!(
        ops(&module)
            .iter()
            .any(|op| matches!(op, Op::SimsetPred { .. })),
        "expected SimsetPred: {:?}",
        ops(&module)
    );
}

#[test]
fn unmatched_virtual_method_dispatches_via_qua() {
    let module = lower(
        r#"begin
            class A;
                virtual: procedure P;
            begin
                real procedure rP; begin end;
            end;
            A class B;
            begin
                integer procedure P; begin end;
            end;
            ref(B) rB;
            rB :- new B;
            rB qua A.P;
           end;"#,
    );
    let dump = module.dump();
    assert!(
        dump.contains("B$P") || dump.contains("$P"),
        "expected virtual P dispatch to B$P: {dump}"
    );
}

#[test]
fn void_virtual_override_call_has_no_dest() {
    // simtst55 shape: unmatched void virtual with typed + void overrides.
    // Dispatch must never emit `%t = call D$P(...)` for the void body.
    let module = lower(
        r#"begin
            class A;
                virtual: procedure P;
            begin end;
            A class B;
            begin
                integer procedure P; begin end;
            end;
            A class D;
            begin
                procedure P; begin end;
            end;
            ref(A) rA;
            rA :- new D;
            rA qua A.P;
           end;"#,
    );
    let dump = module.dump();
    assert!(
        !dump.contains("= call D$P("),
        "void D$P must not have a call dest: {dump}"
    );
    assert!(
        dump.contains("call D$P("),
        "expected a void call to D$P: {dump}"
    );
}

#[test]
fn lowers_environment_helpers() {
    let module = lower(
        r#"begin
            integer n; real r; character c;
            n := abs(-3);
            n := mod(8, 3);
            n := sign(-1.0);
            r := sqrt(4.0);
            c := decimalmark(',');
           end;"#,
    );
    let env_names: Vec<_> = ops(&module)
        .iter()
        .filter_map(|op| match op {
            Op::CallEnv { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for expected in ["abs_int", "mod", "sign", "sqrt", "decimalmark"] {
        assert!(
            env_names.contains(&expected),
            "missing CallEnv {expected}: {env_names:?}"
        );
    }
}
