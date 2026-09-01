//! Interpreter Ch.7 MVP: `detach` during `new` + `call` resume.

#[test]
fn detach_during_new_then_call_orders_output() {
    let source = include_str!("fixtures/simulation/detach_call_roundtrip.sim");
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nC\nB\n");
}

#[test]
fn double_detach_requires_two_calls() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "1\nx\n2\ny\n3\n");
}

#[test]
fn resume_from_main_matches_call_ordering() {
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
        resume(w);
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nC\nB\n");
}

#[test]
fn resume_switch_between_objects_returns_to_main() {
    let source = r#"begin
        class A; begin
            OutText("A1"); OutImage;
            detach;
            OutText("A2"); OutImage;
            resume(b);
            OutText("A3"); OutImage;
        end;
        class B; begin
            OutText("B1"); OutImage;
            detach;
            OutText("B2"); OutImage;
            detach;
        end;
        ref(A) a; ref(B) b;
        a :- new A;
        b :- new B;
        OutText("M1"); OutImage;
        resume(a);
        OutText("M2"); OutImage;
        resume(b);
        OutText("M3"); OutImage;
        resume(a);
        OutText("M4"); OutImage;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    // new A: A1, detach; new B: B1, detach; M1;
    // resume(a): A2, resume(b) switches — B2, detach → main M2
    // resume(b): b already ran B2 and detached at end... wait b after first resume from A
    // Let me recalculate.
    assert_eq!(output, "A1\nB1\nM1\nA2\nB2\nM2\nM3\nA3\nM4\n");
}

#[test]
fn activate_statement_resumes_detached_object() {
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
        activate w;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nC\nB\n");
}

#[test]
fn call_on_object_that_never_became_a_component_errors() {
    // No `detach` in the class, so `new` does not create a chapter-7 component
    // (`runs_on_own_stack` is detach-driven). `call` is then §7.3.2 on a
    // non-component, not on a terminated one.
    let source = r#"begin
        class Worker; begin OutText("A"); OutImage; end;
        ref(Worker) w;
        w :- new Worker;
        call(w);
    end;"#;
    let err = outimage::compile_str(source).unwrap_err();
    assert_eq!(err.phase, outimage::Phase::Runtime);
    let msg = err.to_string();
    assert!(
        msg.contains("call with respect to an object that never became a component"),
        "unexpected error: {msg}"
    );
}

#[test]
fn call_on_terminated_object_errors() {
    // Detach so `new` creates a component; the first `call` runs the body to
    // its final `end` (§7.3.4 → terminated); the second `call` is §7.3.2.
    let source = r#"begin
        class Worker;
        begin
            OutText("A"); OutImage;
            detach;
            OutText("B"); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker;
        call(w);
        call(w);
    end;"#;
    let err = outimage::compile_str(source).unwrap_err();
    assert_eq!(err.phase, outimage::Phase::Runtime);
    let msg = err.to_string();
    assert!(
        msg.contains("call with respect to an object that is terminated"),
        "unexpected error: {msg}"
    );
}

#[test]
fn class_without_detach_still_runs_body_before_new_returns() {
    let source = r#"begin
        class Worker;
        begin
            OutText("A"); OutImage;
            OutText("B"); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker;
        OutText("C"); OutImage;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nB\nC\n");
}

#[test]
fn call_then_resume_fixture() {
    let source = include_str!("fixtures/simulation/call_resume_roundtrip.sim");
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nB\nC\n");
}

#[test]
fn resume_detached_object_fixture() {
    let source = include_str!("fixtures/simulation/activate_statement.sim");
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nB\n");
}

#[test]
fn simulation_hold_and_activate_orders_by_time() {
    let source = include_str!("fixtures/simulation/hold_and_activate.sim");
    let output = outimage::compile_str(source).unwrap();
    // activate w (direct): A, hold(1) → MAIN C, hold(2) → Worker B at t=1 → MAIN D at t=2
    assert_eq!(output, "A\nC\nB\nD\n");
}

#[test]
fn simulation_time_advances_with_hold() {
    let source = r#"Simulation begin
        real t;
        OutText("0"); OutImage;
        hold(3.0);
        t := time;
        if t = 3.0 then begin OutText("ok"); OutImage; end
        else begin OutText("bad"); OutImage; end;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "0\nok\n");
}

#[test]
fn simulation_activate_delay_runs_after_hold() {
    let source = include_str!("fixtures/simulation/activate_delay.sim");
    let output = outimage::compile_str(source).unwrap();
    // activate delay 2: M1, hold(1)->M2 at t=1, hold(2)->W at t=2, then M3 at t=3
    assert_eq!(output, "M1\nM2\nW\nM3\n");
}

#[test]
fn simulation_wait_queue_reactivates_from_head() {
    let source = include_str!("fixtures/simulation/wait_queue.sim");
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "wait\nmain\ngo\ndone\n");
}

#[test]
fn detach_inside_if_then_resumes_after_if() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nC\nB\n");
}

#[test]
fn detach_inside_if_compound_resumes_mid_branch() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nM\nB\nC\n");
}

#[test]
fn detach_in_else_resumes_after_if() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nM\nB\n");
}

#[test]
fn detach_inside_while_resumes_and_continues_loop() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nM1\nB\nA\nM2\nB\nC\nM3\n");
}

#[test]
fn detach_inside_for_step_until_resumes_and_continues() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nM1\nB\nA\nM2\nB\nC\nM3\n");
}

#[test]
fn detach_inside_for_value_list_resumes_next_element() {
    let source = r#"begin
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
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "A\nM1\nB\nA\nM2\nB\nC\nM3\n");
}

#[test]
fn remote_detach_remains_noop() {
    let source = r#"begin class Worker; begin end;
                  ref(Worker) w; w :- new Worker;
                  integer v; v := w.detach();
                  OutInt(v, 0); OutImage; end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "0\n");
}

#[test]
fn simulation_schedule_is_deterministic_across_runs() {
    let source = include_str!("fixtures/simulation/hold_and_activate.sim");
    let first = outimage::compile_str(source).unwrap();
    let second = outimage::compile_str(source).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, "A\nC\nB\nD\n");
}

#[test]
fn mid_call_hold_from_process_method_suspends() {
    let source = r#"Simulation begin
        Process class Worker;
        begin
            procedure nap;
            begin
                hold(1.0);
            end;
            OutText("W1"); OutImage;
            nap;
            OutText("W2"); OutImage;
        end;
        ref(Worker) w;
        w :- new Worker;
        activate w;
        OutText("M1"); OutImage;
        hold(2.0);
        OutText("M2"); OutImage;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "W1\nM1\nW2\nM2\n");
}

#[test]
fn hold_from_outlined_simulation_procedure_suspends() {
    let source = r#"Simulation begin
        procedure nap;
        begin
            hold(1.0);
        end;
        OutText("before"); OutImage;
        nap;
        OutText("after"); OutImage;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "before\nafter\n");
}

#[test]
fn goto_within_process_body_without_suspend_works() {
    let source = r#"begin
        class Worker;
        begin
            integer i;
            i := 1;
        L:
            if i = 1 then begin
                i := 2;
                goto L;
            end;
        end;
        ref(Worker) w;
        integer x;
        w :- new Worker;
        x := 1;
        OutInt(x, 0); OutImage;
    end;"#;
    let output = outimage::compile_str(source).unwrap();
    assert_eq!(output, "1\n");
}
