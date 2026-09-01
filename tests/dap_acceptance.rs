//! Tier-3 debugger acceptance tests (library-level probe sessions).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use outimage::debug::{
    DebugProbe, LaunchConfig, PauseInfo, REF_ARRAY_BASE, REF_FRAME_BASE, REF_OBJECT_BASE,
    REF_SIMULATION, REF_SQS, SourceBreakpoint, evaluate_expression, prepare, run_with_probe,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/debug")
        .join(name)
}

fn wait_stop(rx: &mpsc::Receiver<PauseInfo>) -> PauseInfo {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("expected debugger stop")
}

/// Resume until the eval thread finishes (drains any extra stops on the way).
fn finish(probe: &DebugProbe, rx: &mpsc::Receiver<PauseInfo>, handle: thread::JoinHandle<()>) {
    probe.continue_execution();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if handle.is_finished() {
            let _ = handle.join();
            return;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(_) => probe.continue_execution(),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    probe.request_terminate();
    let _ = handle.join();
    panic!("eval thread did not finish within timeout");
}

fn run_until_first_stop(
    source: &Path,
    breakpoints: Vec<SourceBreakpoint>,
    stop_on_entry: bool,
) -> (
    std::sync::Arc<DebugProbe>,
    mpsc::Receiver<PauseInfo>,
    thread::JoinHandle<()>,
) {
    let config = LaunchConfig {
        program: source.to_path_buf(),
        stop_on_entry,
        allow_square_bracket_subscripts: true,
        allow_double_dash_comments: true,
    };
    let prepared = prepare(&config).expect("prepare");
    let probe = DebugProbe::new(
        source.to_path_buf(),
        prepared.source.text.clone(),
        stop_on_entry,
    );
    if !breakpoints.is_empty() {
        probe.set_breakpoints(source, breakpoints);
    }
    let (tx, rx) = mpsc::channel();
    probe.set_on_stopped(move |info| {
        let _ = tx.send(info);
    });
    let probe_run = probe.clone();
    let handle = thread::spawn(move || {
        let _ = run_with_probe(&prepared, probe_run);
    });
    (probe, rx, handle)
}

#[test]
fn acceptance_step_into_and_out_multi_frame() {
    let path = fixture("procedures.sim");
    // Break on `x := 2` inside `leaf` — both outer and leaf should be on the stack.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(10)], false);
    let info = wait_stop(&rx);
    assert_eq!(info.reason, "breakpoint");
    let names: Vec<_> = info
        .frames
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|n| n == "outer"),
        "expected outer frame, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "leaf"),
        "expected leaf frame, got {names:?}"
    );

    probe.step_out();
    let info = wait_stop(&rx);
    assert_eq!(info.reason, "step");
    let names: Vec<_> = info
        .frames
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|n| n == "outer"),
        "after step-out expected outer, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "leaf"),
        "after step-out leaf should be gone, got {names:?}"
    );

    finish(&probe, &rx, handle);
}

#[test]
fn acceptance_expand_point_attributes() {
    let path = fixture("point.sim");
    // Break on OutText after `p :- new Point`.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(11)], false);
    let info = wait_stop(&rx);
    let p = info
        .variables
        .locals
        .iter()
        .find(|e| e.name == "p")
        .expect("local p");
    assert!(
        p.variables_reference >= REF_OBJECT_BASE,
        "p should be expandable: {p:?}"
    );
    let fields = info
        .variables
        .children
        .get(&p.variables_reference)
        .expect("point fields");
    assert!(
        fields.iter().any(|f| f.name == "x" && f.value == "10"),
        "{fields:?}"
    );
    assert!(
        fields.iter().any(|f| f.name == "y" && f.value == "20"),
        "{fields:?}"
    );
    let watch = evaluate_expression(&info.variables, "p.x").expect("watch p.x");
    assert_eq!(watch.value, "10");
    finish(&probe, &rx, handle);
}

#[test]
fn acceptance_watch_updates_across_steps() {
    let path = fixture("procedures.sim");
    // Break on `x := 1` inside outer (before the assignment runs).
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(14)], false);
    let before = wait_stop(&rx);
    let x0 = evaluate_expression(&before.variables, "x").expect("x before");
    assert_eq!(x0.value, "0", "x should be default 0 before assignment");

    probe.step_over();
    let after = wait_stop(&rx);
    let x1 = evaluate_expression(&after.variables, "x").expect("x after");
    assert_eq!(x1.value, "1", "after assigning x := 1, watch should see 1");

    finish(&probe, &rx, handle);
}

#[test]
fn acceptance_array_expansion() {
    let path = fixture("array.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(7)], false);
    let info = wait_stop(&rx);
    let a = info
        .variables
        .locals
        .iter()
        .find(|e| e.name == "a")
        .expect("array a");
    assert!(
        a.variables_reference >= REF_ARRAY_BASE,
        "array should expand: {a:?}"
    );
    let elems = info
        .variables
        .children
        .get(&a.variables_reference)
        .expect("elements");
    assert!(
        elems.iter().any(|e| e.name == "[1]" && e.value == "11"),
        "{elems:?}"
    );
    assert!(
        elems.iter().any(|e| e.name == "[2]" && e.value == "22"),
        "{elems:?}"
    );
    finish(&probe, &rx, handle);
}

#[test]
fn acceptance_detached_thread_listed() {
    let path = fixture("detach.sim");
    // Break on OutText("C") in main after Worker detached.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(13)], false);
    let info = wait_stop(&rx);
    assert!(
        info.variables.threads.len() >= 2,
        "expected main + detached Worker, got {:?}",
        info.variables.threads
    );
    let det = info
        .variables
        .threads
        .iter()
        .find(|t| t.name.contains("detached"))
        .expect("detached thread");
    assert!(det.resume_summary.is_some(), "{det:?}");
    finish(&probe, &rx, handle);
}

#[test]
fn acceptance_simulation_sqs_scope() {
    let path = fixture("simulation.sim");
    // Break inside Worker body on first OutText (runs after activate).
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(6)], false);
    let info = wait_stop(&rx);
    assert!(
        info.variables.has_simulation,
        "Simulation scope should be present; locals={:?}",
        info.variables.locals
    );
    let sim = info
        .variables
        .children
        .get(&REF_SIMULATION)
        .expect("Simulation children");
    assert!(sim.iter().any(|e| e.name == "time"), "{sim:?}");
    assert!(sim.iter().any(|e| e.name == "sqs"), "{sim:?}");
    let sqs = info.variables.children.get(&REF_SQS).expect("sqs list");
    assert!(!sqs.is_empty(), "SQS should list events");
    finish(&probe, &rx, handle);
}

#[test]
fn inlined_type_procedure_hidden_outside_body() {
    let path = fixture("inline_type_proc.sim");
    // `n := 0` — caller body, after `ispos` is declared but before it is called.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(8)], false);
    let info = wait_stop(&rx);
    assert_eq!(info.reason, "breakpoint");
    let names: Vec<_> = info
        .variables
        .locals
        .iter()
        .map(|e| e.name.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|n| n == "n"),
        "expected enclosing local n, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "ispos"),
        "inlined boolean procedure ispos must not appear as a caller local, got {names:?}"
    );

    probe.continue_execution();
    finish(&probe, &rx, handle);
}

#[test]
fn inlined_type_procedure_visible_inside_body() {
    let path = fixture("inline_type_proc.sim");
    // `ispos := n > 0` inside the procedure.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(6)], false);
    let info = wait_stop(&rx);
    assert_eq!(info.reason, "breakpoint");
    let ispos = info
        .variables
        .locals
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("ispos"))
        .expect("ispos result variable inside the procedure");
    assert!(
        ispos.value == "true" || ispos.value == "false",
        "ispos should display as a boolean, got {}",
        ispos.value
    );
    let names: Vec<_> = info
        .frames
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|n| n == "ispos"),
        "expected synthetic ispos frame while inside the inlined body, got {names:?}"
    );
    finish(&probe, &rx, handle);
}

#[test]
fn inlined_procedure_per_frame_locals() {
    let path = fixture("inline_type_proc.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(6)], false);
    let info = wait_stop(&rx);
    let ispos_frame = info
        .frames
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("ispos"))
        .expect("ispos frame");
    let caller_frame = info
        .frames
        .iter()
        .find(|f| !f.name.eq_ignore_ascii_case("ispos"))
        .expect("caller frame");

    let ispos_locals = info
        .variables
        .children
        .get(&(REF_FRAME_BASE + ispos_frame.id))
        .expect("ispos frame locals");
    assert!(
        ispos_locals
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case("ispos")),
        "ispos frame should own the ispos result, got {ispos_locals:?}"
    );

    let caller_locals = info
        .variables
        .children
        .get(&(REF_FRAME_BASE + caller_frame.id))
        .expect("caller frame locals");
    assert!(
        caller_locals
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case("n")),
        "caller frame should own n, got {caller_locals:?}"
    );
    assert!(
        !caller_locals
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case("ispos")),
        "caller Locals must not list the inlined ispos result, got {caller_locals:?}"
    );
    finish(&probe, &rx, handle);
}

#[test]
fn inlined_procedure_watch_outside_body_is_procedure() {
    let path = fixture("inline_type_proc.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(8)], false);
    let info = wait_stop(&rx);
    let names: Vec<_> = info
        .frames
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    assert!(
        !names.iter().any(|n| n == "ispos"),
        "ispos must not appear on the stack outside its body, got {names:?}"
    );
    let watch = evaluate_expression(&info.variables, "ispos").expect("watch ispos");
    assert_eq!(watch.value, "<procedure>");
    finish(&probe, &rx, handle);
}

#[test]
fn step_over_inlined_call_does_not_enter_body() {
    let path = fixture("inline_type_proc.sim");
    // `if ispos then n := 1` — stepping over must skip the inlined body.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(9)], false);
    let info = wait_stop(&rx);
    assert_eq!(info.reason, "breakpoint");
    probe.step_over();
    let after = wait_stop(&rx);
    assert_eq!(after.reason, "step");
    assert_ne!(
        after.line, 6,
        "step over should not land inside the inlined ispos body"
    );
    let names: Vec<_> = after
        .frames
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    assert!(
        !names.iter().any(|n| n == "ispos"),
        "step over should leave ispos off the stack, got {names:?}"
    );
    finish(&probe, &rx, handle);
}

#[test]
fn step_out_leaves_inlined_procedure() {
    let path = fixture("inline_type_proc.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(6)], false);
    let info = wait_stop(&rx);
    assert!(
        info.frames
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("ispos")),
        "expected ispos frame, got {:?}",
        info.frames.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    probe.step_out();
    let after = wait_stop(&rx);
    assert_eq!(after.reason, "step");
    let names: Vec<_> = after
        .frames
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    assert!(
        !names.iter().any(|n| n == "ispos"),
        "step out should leave the inlined ispos frame, got {names:?}"
    );
    finish(&probe, &rx, handle);
}

#[test]
fn nested_block_local_hidden_outside_and_visible_inside() {
    let path = fixture("nested_block.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(4)], false);
    let outside = wait_stop(&rx);
    let names: Vec<_> = outside
        .variables
        .locals
        .iter()
        .map(|e| e.name.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|n| n == "n"),
        "expected enclosing local n, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "k"),
        "nested-block local k must not appear outside the block, got {names:?}"
    );
    probe.continue_execution();
    finish(&probe, &rx, handle);

    // `n := k` — after `k := 2`, still inside the nested block.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(8)], false);
    let inside = wait_stop(&rx);
    let k = inside
        .variables
        .locals
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("k"))
        .expect("k should be visible inside the nested block");
    assert_eq!(k.value, "2");
    finish(&probe, &rx, handle);

    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(10)], false);
    let after = wait_stop(&rx);
    let names: Vec<_> = after
        .variables
        .locals
        .iter()
        .map(|e| e.name.to_ascii_lowercase())
        .collect();
    assert!(
        !names.iter().any(|n| n == "k"),
        "nested-block local k must not leak after the block, got {names:?}"
    );
    finish(&probe, &rx, handle);
}

#[test]
fn prefixed_block_local_hidden_outside_and_visible_inside() {
    let path = fixture("prefixed_block.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(9)], false);
    let outside = wait_stop(&rx);
    let names: Vec<_> = outside
        .variables
        .locals
        .iter()
        .map(|e| e.name.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|n| n == "n"),
        "expected enclosing local n, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "k"),
        "prefixed-block local k must not appear outside the block, got {names:?}"
    );
    finish(&probe, &rx, handle);

    // `n := k` — after `k := 1`, still inside the prefixed block.
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(13)], false);
    let inside = wait_stop(&rx);
    let k = inside
        .variables
        .locals
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("k"))
        .expect("k should be visible inside the prefixed block");
    assert_eq!(k.value, "1");
    finish(&probe, &rx, handle);
}

#[test]
fn acceptance_double_dash_comments_are_skipped() {
    let path = fixture("double_dash_comment.sim");
    let (probe, rx, handle) = run_until_first_stop(&path, vec![SourceBreakpoint::line(4)], false);
    let info = wait_stop(&rx);
    assert_eq!(info.reason, "breakpoint");
    finish(&probe, &rx, handle);
}
