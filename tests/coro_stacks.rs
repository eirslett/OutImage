//! Coverage for Chapter 7 sequencing, where a class object that suspends gets
//! its own call stack.
//!
//! Several of these use programs that only a real stack can express: chapter 7
//! puts a reactivation point at an arbitrary program point, including inside the
//! object's own procedure activations and inside nested loops.

use std::process::Command;

fn compile_and_run(source: &str, tag: &str) -> Result<String, String> {
    let dir = std::env::temp_dir();
    let sim = dir.join(format!("sim-coro-{tag}.sim"));
    let binary = dir.join(format!("sim-coro-{tag}"));
    std::fs::write(&sim, source).expect("write source");

    let mut compile = Command::new(env!("CARGO_BIN_EXE_sim"));
    compile.arg("compile").arg(&sim).arg("-o").arg(&binary);
    let compiled = compile.output().expect("compiler ran");
    if !compiled.status.success() {
        return Err(String::from_utf8_lossy(&compiled.stdout).into_owned()
            + &String::from_utf8_lossy(&compiled.stderr));
    }

    let run = Command::new(&binary).output().expect("program ran");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_file(&sim);
    let _ = std::fs::remove_file(&binary);
    if !run.status.success() {
        return Err(format!(
            "exited with {:?}: {}{}",
            run.status.code(),
            stdout,
            String::from_utf8_lossy(&run.stderr)
        ));
    }
    Ok(stdout)
}

/// `detach` three loops deep. A real stack does not care how deep the frame is.
const DETACH_DEEP_IN_LOOPS: &str = r#"begin
    class C;
    begin
        integer i, j;
        for i := 1 step 1 until 2 do
        begin
            j := 0;
            while j < 2 do
            begin
                j := j + 1;
                if j = 2 then
                begin
                    outint(i, 2); outint(j, 2); outimage;
                    detach;
                end;
            end;
        end;
        outtext("done"); outimage;
    end;
    ref(C) c;
    c :- new C;
    outtext("m1"); outimage;
    call(c);
    outtext("m2"); outimage;
    call(c);
    outtext("m3"); outimage;
end;"#;

/// `detach` inside one of the object's own procedures. 7.4's worked example
/// does exactly this, and the reactivation point lands inside the procedure
/// activation, which only a per-component stack can preserve.
const DETACH_INSIDE_PROCEDURE: &str = r#"begin
    class D;
    begin
        procedure advance;
        begin
            outtext("in-proc"); outimage;
            detach;
            outtext("after-proc"); outimage;
        end;
        outtext("d-start"); outimage;
        advance;
        outtext("d-end"); outimage;
    end;
    ref(D) d;
    d :- new D;
    outtext("m1"); outimage;
    call(d);
    outtext("m2"); outimage;
end;"#;

#[test]
fn detach_nested_in_loops_resumes_at_the_right_program_point() {
    let stdout = compile_and_run(DETACH_DEEP_IN_LOOPS, "deep").expect("compiles and runs");
    assert_eq!(
        stdout, " 1 2\nm1\n 2 2\nm2\ndone\nm3\n",
        "each call must continue inside the if, inside the while, inside the for"
    );
}

#[test]
fn detach_inside_a_procedure_keeps_the_activation() {
    let stdout = compile_and_run(DETACH_INSIDE_PROCEDURE, "proc").expect("compiles and runs");
    assert_eq!(
        stdout, "d-start\nin-proc\nm1\nafter-proc\nd-end\nm2\n",
        "the call must resume inside `advance`, not at the top of the body"
    );
}

#[test]
fn a_simple_detach_call_roundtrip_returns_to_the_caller() {
    let source = r#"begin
        class C;
        begin
            outtext("A"); outimage;
            detach;
            outtext("B"); outimage;
        end;
        ref(C) c;
        c :- new C;
        outtext("M"); outimage;
        call(c);
        outtext("Z"); outimage;
    end;"#;
    let stdout = compile_and_run(source, "simple").expect("compiles and runs");
    assert_eq!(stdout, "A\nM\nB\nZ\n");
}

#[test]
fn a_scheduled_process_still_reaches_its_activation_points() {
    // The SIMULATION scheduler reactivates a process by re-entering its body at
    // a stored statement index rather than driving the component runtime, so a
    // Process subclass keeps that lowering; its observable behaviour must not
    // depend on which one it uses.
    let source = r#"Simulation begin
        process class Ticker(mark); text mark;
        begin
            outtext(mark); outimage;
            hold(1);
            outtext(mark); outimage;
        end;
        activate new Ticker("a");
        activate new Ticker("b");
        hold(3);
        outtext("done"); outimage;
    end;"#;
    let stdout = compile_and_run(source, "scheduled-process").expect("compiles and runs");
    assert_eq!(stdout, "a\nb\na\nb\ndone\n");
}

/// A procedure declared in a prefixed block keeps belonging to that block
/// instance even while it runs on the stack of an object that called it, so by
/// 7.3.1 its detach has no effect. Corpus test simtst69 turns on exactly this:
/// its `P2` is `Sjekk(7); Detach; Sjekk(8)`, and the test only passes if 7 and 8
/// are recorded with nothing in between.
#[test]
fn detach_in_a_block_procedure_called_from_an_object_has_no_effect() {
    let source = r#"begin
        class A;;
        A begin
            class C;
            begin
                outtext("c1"); outimage; detach;
                outtext("c2"); outimage; P;
                outtext("c3"); outimage;
            end;
            procedure P;
            begin outtext("p1"); outimage; detach; outtext("p2"); outimage; end;
            ref(C) r;
            r :- new C;
            outtext("m1"); outimage;
            call(r);
            outtext("m2"); outimage;
        end;
    end;"#;
    let stdout = compile_and_run(source, "block-proc").expect("compiles and runs");
    assert_eq!(stdout, "c1\nm1\nc2\np1\np2\nc3\nm2\n");
}

/// 7.3.1 opens with "if X is an instance of a prefixed block the detach
/// statement has no effect". Here `P2` is an attribute of the `C1 begin end`
/// block instance, so the detach inside it does nothing even though `P2` is
/// running on the stack of the `C3` object that called it.
#[test]
fn detach_of_a_prefixed_block_instance_has_no_effect() {
    let source = r#"begin
        procedure tr(t); value t; text t; begin outtext(t); outimage; end;
        class C1;
        begin
            class C3;
            begin
                tr("C1"); detach;
                tr("C2"); P2;
                tr("C3"); detach;
                tr("C4");
            end;
            procedure P2; begin tr("P1"); detach; tr("P2"); end;
            ref(C3) r3;
            r3 :- new C3;
            tr("M1"); call(r3);
            tr("M2"); call(r3);
            tr("M3");
        end;
        C1 begin end;
    end;"#;
    let stdout = compile_and_run(source, "prefix-detach").expect("compiles and runs");
    assert_eq!(stdout, "C1\nM1\nC2\nP1\nP2\nC3\nM2\nC4\nM3\n");
}

/// 7.2 makes every subblock with a local class declaration its own system, and
/// the block instance its main component. Here `resume(y)` happens inside `A`'s
/// own subblock, so it must not displace `A` as the operative component of the
/// outer system: the later `this A.detach` is legal precisely because `A` is
/// still operative out there.
#[test]
fn nested_system_heads_keep_their_own_main_component() {
    let source = r#"begin
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
        outtext("M"); outimage;
    end;"#;
    let stdout = compile_and_run(source, "nested-systems").expect("compiles and runs");
    assert_eq!(stdout, "ABCDEFGHIJKLM\n");
}

/// `Inspect X do Detach` names X's detach attribute, so 7.3.1 case 1 applies to
/// X: control returns to the block instance X is attached to, and X keeps its
/// reactivation point where it was. Reduced from simtst69, whose `Sjekk` order
/// only admits this reading.
#[test]
fn detach_through_an_inspect_connection_detaches_the_connected_object() {
    let source = r#"begin
        ref(C2) rC2;
        class C2;
        begin ref(C5) rC5;
            detach;
            outtext("16");
            rC5 :- new C5;
            call(rC5);
            outtext("23");
        end;
        class C5;
        begin
            outtext("17"); detach;
            outtext("18");
            inspect rC2 do detach;
            outtext("22");
        end;
        rC2 :- new C2;
        call(rC2);
        outtext("19"); outimage;
    end;"#;
    let stdout = compile_and_run(source, "inspect-detach").expect("compiles and runs");
    assert_eq!(
        stdout, "16171819\n",
        "detaching rC2 must return to MAIN, which called it, leaving C5 suspended"
    );
}

/// A `ref(Coroutine)` variable holds objects of any subclass, and each subclass
/// lays its captured enclosing names out after its own attributes, so both the
/// names to carry and the slots to carry them in follow the runtime class rather
/// than the qualification (simtst88).
#[test]
fn transfers_through_a_prefix_qualified_reference_carry_the_subclasses_captures() {
    let source = r#"begin character c;
        ref(Coroutine) r, w;
        class Coroutine; detach;
        Coroutine class Reader;
        begin integer k;
            for k := 1 step 1 until 3 do
            begin c := char(rank('a') + k - 1); resume(w) end;
        end;
        Coroutine class Writer;
        begin integer k;
            for k := 1 step 1 until 3 do
            begin outchar(c); resume(r) end;
        end;
        r :- new Reader; w :- new Writer;
        resume(r);
        outimage;
    end;"#;
    let stdout = compile_and_run(source, "prefix-ring").expect("compiles and runs");
    assert_eq!(
        stdout, "abc\n",
        "the writer must see each character the reader assigned"
    );
}
