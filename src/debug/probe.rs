//! Cooperative debug probe installed into the interpreter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::codegen::sourcemap::span_to_line_col;
use crate::error::Span;
use std::sync::mpsc::{self, Receiver, Sender};

use super::format::{
    REF_FRAME_BASE, REF_LOCALS, VarEntry, VariableSnapshot, condition_holds, format_log_message,
};

/// How the interpreter should proceed after a pause decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Run until a breakpoint (or program end).
    Continue,
    /// Stop on the next statement.
    StepIn,
    /// Stop on the next statement at frame depth ≤ `depth`.
    StepOver { depth: usize },
    /// Stop when frame depth drops below `depth`.
    StepOut { depth: usize },
    /// Debugger requested termination.
    Terminate,
}

/// One source breakpoint (possibly conditional / logpoint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBreakpoint {
    pub line: u32,
    pub condition: Option<String>,
    pub log_message: Option<String>,
}

impl SourceBreakpoint {
    pub fn line(line: u32) -> Self {
        Self {
            line,
            condition: None,
            log_message: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct PauseInfo {
    pub reason: String,
    pub line: u32,
    pub column: u32,
    pub frames: Vec<FrameInfo>,
    pub variables: VariableSnapshot,
}

struct PendingSetVariable {
    name: String,
    variables_reference: i64,
    value_text: String,
    reply: Sender<Result<VarEntry, String>>,
}

struct ProbeInner {
    source_path: PathBuf,
    source_text: String,
    source_key: String,
    breakpoints: HashMap<String, Vec<SourceBreakpoint>>,
    mode: RunMode,
    stop_on_entry: bool,
    entry_seen: bool,
    paused: bool,
    pause_info: Option<PauseInfo>,
    frames: Vec<FrameInfo>,
    next_frame_id: i64,
    wake: bool,
    break_on_exceptions: bool,
    pending_sets: Vec<PendingSetVariable>,
    /// Last source line visited while running (for MIR multi-op coalescing).
    last_poll_line: Option<u32>,
    /// Line of the last breakpoint stop; cleared when execution leaves the line.
    last_breakpoint_line: Option<u32>,
    /// Innermost inlined procedure span at the start of step-over / step-out.
    step_scope: Option<Span>,
}

enum HitDecision {
    None,
    Stop(PauseInfo),
    Log(String),
}

/// Shared probe between the DAP server thread and the interpreter thread.
pub struct DebugProbe {
    inner: Mutex<ProbeInner>,
    cv: Condvar,
    on_stopped: Mutex<Option<Box<dyn Fn(PauseInfo) + Send>>>,
    on_output: Mutex<Option<Box<dyn Fn(String) + Send>>>,
    terminated: AtomicBool,
}

impl DebugProbe {
    pub fn new(source_path: PathBuf, source_text: String, stop_on_entry: bool) -> Arc<Self> {
        let source_key = normalize_path_key(&source_path);
        Arc::new(Self {
            inner: Mutex::new(ProbeInner {
                source_path,
                source_text,
                source_key,
                breakpoints: HashMap::new(),
                mode: if stop_on_entry {
                    RunMode::StepIn
                } else {
                    RunMode::Continue
                },
                stop_on_entry,
                entry_seen: false,
                paused: false,
                pause_info: None,
                frames: vec![FrameInfo {
                    id: 1,
                    name: "main".into(),
                }],
                next_frame_id: 2,
                wake: false,
                break_on_exceptions: true,
                pending_sets: Vec::new(),
                last_poll_line: None,
                last_breakpoint_line: None,
                step_scope: None,
            }),
            cv: Condvar::new(),
            on_stopped: Mutex::new(None),
            on_output: Mutex::new(None),
            terminated: AtomicBool::new(false),
        })
    }

    pub fn source_path(&self) -> PathBuf {
        self.inner.lock().expect("probe").source_path.clone()
    }

    pub fn set_on_stopped<F>(&self, f: F)
    where
        F: Fn(PauseInfo) + Send + 'static,
    {
        *self.on_stopped.lock().expect("on_stopped") = Some(Box::new(f));
    }

    pub fn set_on_output<F>(&self, f: F)
    where
        F: Fn(String) + Send + 'static,
    {
        *self.on_output.lock().expect("on_output") = Some(Box::new(f));
    }

    pub fn set_breakpoints(&self, path: &Path, breakpoints: Vec<SourceBreakpoint>) {
        let key = normalize_path_key(path);
        let mut inner = self.inner.lock().expect("probe");
        if breakpoints.is_empty() {
            inner.breakpoints.remove(&key);
        } else {
            inner.breakpoints.insert(key, breakpoints);
        }
    }

    pub fn continue_execution(&self) {
        let mut inner = self.inner.lock().expect("probe");
        inner.mode = RunMode::Continue;
        inner.step_scope = None;
        inner.paused = false;
        inner.wake = true;
        self.cv.notify_all();
    }

    pub fn step_in(&self) {
        let mut inner = self.inner.lock().expect("probe");
        inner.mode = RunMode::StepIn;
        inner.paused = false;
        inner.wake = true;
        self.cv.notify_all();
    }

    pub fn step_over(&self) {
        let mut inner = self.inner.lock().expect("probe");
        let depth = inner.frames.len();
        inner.step_scope = inner.pause_info.as_ref().and_then(|pause| {
            pause
                .variables
                .innermost_procedure
                .as_ref()
                .map(|(_, start, end)| *start..*end)
        });
        inner.mode = RunMode::StepOver { depth };
        inner.paused = false;
        inner.wake = true;
        self.cv.notify_all();
    }

    pub fn step_out(&self) {
        let mut inner = self.inner.lock().expect("probe");
        let depth = inner.frames.len();
        inner.step_scope = inner.pause_info.as_ref().and_then(|pause| {
            pause
                .variables
                .innermost_procedure
                .as_ref()
                .map(|(_, start, end)| *start..*end)
        });
        inner.mode = RunMode::StepOut { depth };
        inner.paused = false;
        inner.wake = true;
        self.cv.notify_all();
    }

    pub fn request_terminate(&self) {
        self.terminated.store(true, Ordering::SeqCst);
        let mut inner = self.inner.lock().expect("probe");
        inner.mode = RunMode::Terminate;
        inner.paused = false;
        inner.wake = true;
        self.cv.notify_all();
    }

    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }

    pub fn pause_info(&self) -> Option<PauseInfo> {
        self.inner.lock().expect("probe").pause_info.clone()
    }

    pub fn push_frame(&self, name: impl Into<String>) {
        let mut inner = self.inner.lock().expect("probe");
        let id = inner.next_frame_id;
        inner.next_frame_id += 1;
        inner.frames.push(FrameInfo {
            id,
            name: name.into(),
        });
    }

    pub fn pop_frame(&self) {
        let mut inner = self.inner.lock().expect("probe");
        if inner.frames.len() > 1 {
            inner.frames.pop();
        }
    }

    pub fn set_break_on_exceptions(&self, enabled: bool) {
        self.inner.lock().expect("probe").break_on_exceptions = enabled;
    }

    pub fn break_on_exceptions(&self) -> bool {
        self.inner.lock().expect("probe").break_on_exceptions
    }

    /// Queue a `setVariable` while the interpreter is paused; blocks until applied.
    pub fn request_set_variable(
        &self,
        name: String,
        variables_reference: i64,
        value_text: String,
    ) -> Result<VarEntry, String> {
        let (tx, rx): (Sender<Result<VarEntry, String>>, Receiver<_>) = mpsc::channel();
        {
            let mut inner = self.inner.lock().expect("probe");
            if !inner.paused {
                return Err("not paused".into());
            }
            inner.pending_sets.push(PendingSetVariable {
                name,
                variables_reference,
                value_text,
                reply: tx,
            });
            self.cv.notify_all();
        }
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "timed out waiting to set variable".to_string())?
    }

    pub fn frames(&self) -> Vec<FrameInfo> {
        self.inner.lock().expect("probe").frames.clone()
    }

    fn matching_breakpoints(inner: &ProbeInner, line: u32) -> Vec<SourceBreakpoint> {
        let mut hits = Vec::new();
        for (key, bps) in &inner.breakpoints {
            if key == &inner.source_key || paths_maybe_same(key, &inner.source_key) {
                for bp in bps {
                    if bp.line == line {
                        hits.push(bp.clone());
                    }
                }
            }
        }
        hits
    }

    /// MIR interpreter pause hook with optional `setVariable` application.
    pub fn poll_span_mir<F>(&self, span: Span, variables: VariableSnapshot, mut on_set: F)
    where
        F: FnMut(&str, i64, &str) -> Result<VarEntry, String>,
    {
        self.poll_span_with_set(span, variables, &mut on_set);
    }

    fn poll_span_with_set(
        &self,
        span: Span,
        mut variables: VariableSnapshot,
        on_set: &mut dyn FnMut(&str, i64, &str) -> Result<VarEntry, String>,
    ) {
        if self.terminated.load(Ordering::SeqCst) {
            return;
        }
        if span.start >= span.end {
            return;
        }

        let decision = {
            let mut inner = self.inner.lock().expect("probe");
            let (line, column) = span_to_line_col(&inner.source_text, span.start);
            let line = line as u32;
            let column = column as u32;
            let depth = inner.frames.len();

            if matches!(inner.mode, RunMode::Terminate) {
                return;
            }

            // MIR lowers one statement to many ops; treat a line change as the
            // statement boundary for stepping / breakpoint re-entry.
            let line_changed = inner.last_poll_line != Some(line);
            if line_changed {
                inner.last_breakpoint_line = None;
            }
            inner.last_poll_line = Some(line);

            let stop_entry = inner.stop_on_entry && !inner.entry_seen;
            if stop_entry {
                inner.entry_seen = true;
            }

            let now_inner = innermost_procedure_span(&variables);
            let skip_inline = nested_inline(inner.step_scope.as_ref(), now_inner.as_ref(), &span);
            let step_stop = match inner.mode {
                RunMode::StepIn if line_changed => true,
                RunMode::StepOver { depth: d } if depth <= d && line_changed && !skip_inline => {
                    true
                }
                RunMode::StepOut { depth: d }
                    if line_changed
                        && (depth < d
                            || inner
                                .step_scope
                                .as_ref()
                                .is_some_and(|started| !span_covers_pc(started, &span))) =>
                {
                    true
                }
                _ => false,
            };

            let frames = attach_logical_frames(&inner, &mut variables);

            let decision = if stop_entry {
                HitDecision::Stop(PauseInfo {
                    reason: "entry".into(),
                    line,
                    column,
                    frames: frames.clone(),
                    variables: variables.clone(),
                })
            } else if step_stop {
                HitDecision::Stop(PauseInfo {
                    reason: "step".into(),
                    line,
                    column,
                    frames: frames.clone(),
                    variables: variables.clone(),
                })
            } else {
                let matches = Self::matching_breakpoints(&inner, line);
                if matches.is_empty() {
                    HitDecision::None
                } else if inner.last_breakpoint_line == Some(line) {
                    HitDecision::None
                } else {
                    let mut log_msg: Option<String> = None;
                    let mut should_stop = false;
                    for bp in matches {
                        if let Some(cond) = bp.condition.as_deref()
                            && !condition_holds(&variables, cond)
                        {
                            continue;
                        }
                        if let Some(template) = bp.log_message.as_deref() {
                            let mut msg = format_log_message(&variables, template);
                            if !msg.ends_with('\n') {
                                msg.push('\n');
                            }
                            log_msg = Some(msg);
                            continue;
                        }
                        should_stop = true;
                        break;
                    }
                    if should_stop {
                        inner.last_breakpoint_line = Some(line);
                        HitDecision::Stop(PauseInfo {
                            reason: "breakpoint".into(),
                            line,
                            column,
                            frames,
                            variables: variables.clone(),
                        })
                    } else if let Some(msg) = log_msg {
                        HitDecision::Log(msg)
                    } else {
                        HitDecision::None
                    }
                }
            };

            if let HitDecision::Stop(info) = &decision {
                inner.pause_info = Some(info.clone());
                inner.paused = true;
                inner.wake = false;
                if matches!(
                    inner.mode,
                    RunMode::StepIn | RunMode::StepOver { .. } | RunMode::StepOut { .. }
                ) {
                    inner.mode = RunMode::Continue;
                }
            }

            decision
        };

        match decision {
            HitDecision::None => {}
            HitDecision::Log(msg) => {
                if let Some(cb) = self.on_output.lock().expect("on_output").as_ref() {
                    cb(msg);
                }
            }
            HitDecision::Stop(info) => {
                if let Some(cb) = self.on_stopped.lock().expect("on_stopped").as_ref() {
                    cb(info);
                }
                self.park_while_paused_with_set(on_set);
            }
        }
    }

    fn park_while_paused_with_set(
        &self,
        on_set: &mut dyn FnMut(&str, i64, &str) -> Result<VarEntry, String>,
    ) {
        loop {
            let pending = {
                let mut inner = self.inner.lock().expect("probe");
                std::mem::take(&mut inner.pending_sets)
            };
            for req in pending {
                let result = on_set(&req.name, req.variables_reference, &req.value_text);
                if let Ok(ref entry) = result {
                    let mut inner = self.inner.lock().expect("probe");
                    if let Some(pause) = inner.pause_info.as_mut() {
                        refresh_snapshot_entry(
                            &mut pause.variables,
                            req.variables_reference,
                            entry,
                        );
                    }
                }
                let _ = req.reply.send(result);
            }

            if self.wait_while_paused() {
                break;
            }
        }
    }

    /// Returns true when the pause has been cleared and the interpreter should resume.
    fn wait_while_paused(&self) -> bool {
        let mut inner = self.inner.lock().expect("probe");
        if !inner.paused || inner.wake || self.terminated.load(Ordering::SeqCst) {
            inner.paused = false;
            inner.wake = false;
            return true;
        }
        let (guard, timeout) = self
            .cv
            .wait_timeout(inner, Duration::from_millis(100))
            .expect("probe wait");
        drop(guard);
        let _ = timeout;
        false
    }
}

fn innermost_procedure_span(variables: &VariableSnapshot) -> Option<Span> {
    variables
        .innermost_procedure
        .as_ref()
        .map(|(_, start, end)| *start..*end)
}

fn span_covers_pc(scope: &Span, pc: &Span) -> bool {
    pc.start < pc.end && pc.start >= scope.start && pc.end <= scope.end
}

fn nested_inline(started: Option<&Span>, now: Option<&Span>, pc: &Span) -> bool {
    match started {
        None => now.is_some(),
        Some(started) => now.is_some_and(|now| {
            (now.start != started.start || now.end != started.end) && span_covers_pc(started, pc)
        }),
    }
}

const SYNTHETIC_FRAME_BASE: i64 = 8_000;

fn attach_logical_frames(inner: &ProbeInner, variables: &mut VariableSnapshot) -> Vec<FrameInfo> {
    let mut frames = inner.frames.clone();
    let mir_len = frames.len();
    for (index, inline) in variables.inline_frames.iter().enumerate() {
        frames.push(FrameInfo {
            id: SYNTHETIC_FRAME_BASE + index as i64,
            name: inline.name.clone(),
        });
    }
    for (index, frame) in frames.iter().enumerate() {
        let locals = if index + 1 == mir_len {
            variables.function_locals.clone()
        } else if index < mir_len {
            Vec::new()
        } else {
            variables.inline_frames[index - mir_len].locals.clone()
        };
        variables.children.insert(REF_FRAME_BASE + frame.id, locals);
    }
    frames
}

fn refresh_snapshot_entry(snap: &mut VariableSnapshot, reference: i64, entry: &VarEntry) {
    let list = if reference == REF_LOCALS || reference == 0 {
        &mut snap.locals
    } else {
        snap.children.entry(reference).or_default()
    };
    if let Some(existing) = list.iter_mut().find(|e| e.name == entry.name) {
        *existing = entry.clone();
    } else {
        list.push(entry.clone());
    }
}

fn normalize_path_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn paths_maybe_same(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let an = Path::new(a);
    let bn = Path::new(b);
    matches!(
        (an.file_name(), bn.file_name()),
        (Some(x), Some(y)) if x == y
    )
}

thread_local! {
    /// Per-eval-thread active probe. A process-global slot races when multiple
    /// debug sessions run in parallel (DAP acceptance tests).
    static ACTIVE: std::cell::RefCell<Option<Arc<DebugProbe>>> =
        const { std::cell::RefCell::new(None) };
}

pub fn install_probe(probe: Arc<DebugProbe>) {
    ACTIVE.with(|slot| {
        *slot.borrow_mut() = Some(probe);
    });
}

pub fn uninstall_probe() {
    ACTIVE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub fn active_probe() -> Option<Arc<DebugProbe>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}

/// MIR interpreter hook — no-op when no probe is installed.
pub fn poll_mir_span<F>(span: Span, variables: VariableSnapshot, on_set: F)
where
    F: FnMut(&str, i64, &str) -> Result<VarEntry, String>,
{
    if let Some(probe) = active_probe() {
        probe.poll_span_mir(span, variables, on_set);
    }
}

pub fn push_frame(name: impl Into<String>) {
    if let Some(probe) = active_probe() {
        probe.push_frame(name);
    }
}

pub fn pop_frame() {
    if let Some(probe) = active_probe() {
        probe.pop_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn snap_with_x(value: &str) -> VariableSnapshot {
        let mut snap = VariableSnapshot::default();
        snap.locals.push(VarEntry {
            name: "x".into(),
            value: value.into(),
            variables_reference: 0,
        });
        snap
    }

    #[test]
    fn pauses_on_breakpoint_line() {
        let source = "begin\ninteger x;\nx := 1;\nend";
        let probe = DebugProbe::new(PathBuf::from("/tmp/bp.sim"), source.into(), false);
        probe.set_breakpoints(Path::new("/tmp/bp.sim"), vec![SourceBreakpoint::line(3)]);

        let (tx, rx) = mpsc::channel();
        probe.set_on_stopped(move |info| {
            let _ = tx.send(info);
        });

        install_probe(probe.clone());
        let handle = std::thread::spawn({
            let probe = probe.clone();
            move || {
                probe.poll_span_mir(17..23, snap_with_x("0"), |_, _, _| {
                    Err("unexpected set".into())
                });
            }
        });

        let info = rx.recv_timeout(Duration::from_secs(2)).expect("stopped");
        assert_eq!(info.reason, "breakpoint");
        assert_eq!(info.line, 3);
        probe.continue_execution();
        handle.join().unwrap();
        uninstall_probe();
    }

    #[test]
    fn conditional_breakpoint_skips_when_false() {
        let source = "begin\ninteger x;\nx := 1;\nend";
        let probe = DebugProbe::new(PathBuf::from("/tmp/cond.sim"), source.into(), false);
        probe.set_breakpoints(
            Path::new("/tmp/cond.sim"),
            vec![SourceBreakpoint {
                line: 3,
                condition: Some("x > 5".into()),
                log_message: None,
            }],
        );
        let (tx, rx) = mpsc::channel();
        probe.set_on_stopped(move |info| {
            let _ = tx.send(info);
        });
        probe.poll_span_mir(17..23, snap_with_x("1"), |_, _, _| {
            Err("unexpected set".into())
        });
        assert!(
            rx.try_recv().is_err(),
            "should not stop when condition false"
        );
    }

    #[test]
    fn logpoint_emits_without_stopping() {
        let source = "begin\ninteger x;\nx := 1;\nend";
        let probe = DebugProbe::new(PathBuf::from("/tmp/log.sim"), source.into(), false);
        probe.set_breakpoints(
            Path::new("/tmp/log.sim"),
            vec![SourceBreakpoint {
                line: 3,
                condition: None,
                log_message: Some("x is {x}".into()),
            }],
        );
        let (tx, rx) = mpsc::channel();
        probe.set_on_output(move |msg| {
            let _ = tx.send(msg);
        });
        let (stx, srx) = mpsc::channel();
        probe.set_on_stopped(move |_| {
            let _ = stx.send(());
        });
        probe.poll_span_mir(17..23, snap_with_x("9"), |_, _, _| {
            Err("unexpected set".into())
        });
        let msg = rx.recv_timeout(Duration::from_secs(1)).expect("log");
        assert!(msg.contains("x is 9"), "{msg}");
        assert!(srx.try_recv().is_err(), "logpoint must not pause");
    }

    #[test]
    fn set_variable_while_paused() {
        let source = "begin\ninteger x;\nx := 1;\nend";
        let probe = DebugProbe::new(PathBuf::from("/tmp/set.sim"), source.into(), false);
        probe.set_breakpoints(Path::new("/tmp/set.sim"), vec![SourceBreakpoint::line(3)]);
        let (tx, rx) = mpsc::channel();
        probe.set_on_stopped(move |_| {
            let _ = tx.send(());
        });
        let probe2 = probe.clone();
        let stored = Arc::new(Mutex::new(String::from("1")));
        let stored_set = stored.clone();
        let handle = std::thread::spawn(move || {
            probe2.poll_span_mir(17..23, snap_with_x("1"), move |name, _ref, text| {
                assert_eq!(name, "x");
                *stored_set.lock().unwrap() = text.to_string();
                Ok(VarEntry {
                    name: name.into(),
                    value: text.into(),
                    variables_reference: 0,
                })
            });
        });
        rx.recv_timeout(Duration::from_secs(2)).expect("stopped");
        let entry = probe
            .request_set_variable("x".into(), REF_LOCALS, "99".into())
            .expect("set");
        assert_eq!(entry.value, "99");
        probe.continue_execution();
        handle.join().unwrap();
        assert_eq!(*stored.lock().unwrap(), "99");
    }

    #[test]
    fn mir_interp_stop_on_entry_then_continue() {
        let source = "begin\nOutText(\"hi\");\nOutImage;\nend;";
        let probe = DebugProbe::new(PathBuf::from("/tmp/mir-entry.sim"), source.into(), true);
        let (tx, rx) = mpsc::channel();
        probe.set_on_stopped(move |info| {
            let _ = tx.send(info);
        });
        let probe_run = probe.clone();
        let handle = std::thread::spawn(move || {
            // Probe is thread-local — install on the interpreter thread.
            install_probe(probe_run);
            let program = crate::parse::test_support::parse_program(source);
            let module = crate::mir::lower_program_with_source(&program, source).expect("lower");
            let output = crate::mir::interp::interpret_module(&module).expect("run");
            uninstall_probe();
            output
        });
        let info = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("entry pause");
        assert_eq!(info.reason, "entry");
        assert!(info.line >= 1, "line={}", info.line);
        probe.continue_execution();
        let output = handle.join().expect("join");
        assert_eq!(output, "hi\n");
    }

    #[test]
    fn mir_interp_set_variable_local() {
        let source = "begin\ninteger x;\nx := 1;\nOutInt(x, 0);\nOutImage;\nend;";
        let probe = DebugProbe::new(PathBuf::from("/tmp/mir-set.sim"), source.into(), false);
        // Break on OutInt — after x := 1, before print.
        probe.set_breakpoints(
            Path::new("/tmp/mir-set.sim"),
            vec![SourceBreakpoint::line(4)],
        );
        let (tx, rx) = mpsc::channel();
        probe.set_on_stopped(move |info| {
            let _ = tx.send(info);
        });
        let probe_run = probe.clone();
        let handle = std::thread::spawn(move || {
            install_probe(probe_run);
            let program = crate::parse::test_support::parse_program(source);
            let module = crate::mir::lower_program_with_source(&program, source).expect("lower");
            let output = crate::mir::interp::interpret_module(&module).expect("run");
            uninstall_probe();
            output
        });
        let info = rx.recv_timeout(Duration::from_secs(2)).expect("breakpoint");
        assert_eq!(info.reason, "breakpoint");
        assert!(
            info.variables
                .locals
                .iter()
                .any(|e| e.name == "x" && e.value == "1"),
            "locals={:?}",
            info.variables.locals
        );
        let entry = probe
            .request_set_variable("x".into(), REF_LOCALS, "42".into())
            .expect("set x");
        assert_eq!(entry.value, "42");
        probe.continue_execution();
        let output = handle.join().expect("join");
        assert_eq!(output, "42\n");
    }
}
