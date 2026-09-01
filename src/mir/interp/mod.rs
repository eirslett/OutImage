//! MIR bytecode interpreter (Phases 1–6: core, text/arrays, objects, BASICIO,
//! SIMSET / sequencing / Simulation).
//!
//! Executes an already-lowered [`Module`] without codegen. Lowering stays in
//! [`crate::mir::lower`]; this module only interprets the resulting MIR.
//!
//! **Embedding from Rust:** [`Interpreter::from_module`], [`Interpreter::define_host`],
//! and [`Interpreter::call`] are the in-process analogue of `simrt_instantiate`
//! / a Host import table / `simrt_call`. A typical Simulation driver is:
//!
//! ```ignore
//! let mut vm = Interpreter::from_module(&mir);
//! vm.define_host("onTick", |_ctx, args| {
//!     eprintln!("{}", args[0].as_f64()?);
//!     Ok(Value::None)
//! });
//! vm.call("tick", &[])?;
//! ```
//!
//! **Phase 3 limits:** free `InImage`/`InLine` read stdin when present; an empty
//! stdin yields an empty image / notext line without blocking in unit tests.
//!
//! **Phase 6 (SIMSET / seq / Simulation):** chapter 7 quasi-parallel sequencing
//! has no OS-level coroutines here — each component is instead a parked
//! `Vec<CallFrame>` call stack (see [`seq_ops`]), and "switching" a coroutine is
//! just swapping which stack [`Vm::frames`] currently holds. This mirrors
//! `runtime/sequencing.c` / `runtime/runtime.c`'s Ch.12 SQS-over-Ch.7 model
//! exactly, one `simrt_coro*` pointer at a time replaced by a [`seq_ops::SeqTarget`].

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};

mod basicio_ops;
#[cfg(feature = "dap")]
mod debug_snap;
mod gc;
mod seq_ops;

use crate::basicio::{self, BasicioState};
use crate::diagnostics;
use crate::error::{CompileError, Span};
use crate::mir::{BinOp, BlockId, CmpOp, Function, LocalId, MirType, Module, Op, UnOp};
use crate::runtime::environment::EnvironmentRuntimeState;
use crate::runtime::host::{CapturingHost, IoHost, ReadLine, StdinRecord};
use crate::runtime::io::{Input, Output};
use crate::runtime::text::TextFrame;

use gc::SlotTag;
pub use gc::{GcOptions, GcStats};

use seq_ops::{SeqComponent, SeqFrame, SeqSystemState, SeqTarget, SimState};

/// Runtime value carried in interpreter locals.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    I64(i64),
    Bool(bool),
    F64(f64),
    /// Null object reference (`none`).
    None,
    /// Opaque object pointer placeholder (heap index).
    ObjectRef(usize),
    /// Simula `text` frame (shared buffer semantics via [`TextFrame`]).
    Text(TextFrame),
    /// Opaque array descriptor placeholder (heap index).
    ///
    /// [`usize::MAX`] is the null descriptor (encoded as i64 0 in object
    /// fields / ref cells) — matching native's null pointer. Capture-reload
    /// shims often load/store/compare these without ever indexing.
    Array(usize),
    /// Pointer into [`Vm::refs`] (local slot or stack-allocated cell).
    RefI64(usize),
    /// MIR function name (direct calls use [`Op::Call`] instead).
    FuncRef(String),
    /// Opaque handle to a chapter 7 quasi-parallel system (`Op::SeqSystemEnter`).
    SeqSystem(usize),
    /// Opaque handle to a not-yet-started component (`Op::SeqObjectCreate`).
    SeqComponentHandle(usize),
}

impl Value {
    pub fn as_i64(&self) -> Result<i64, CompileError> {
        match self {
            Self::I64(v) => Ok(*v),
            other => Err(type_error("value", "i64", other)),
        }
    }

    pub fn as_f64(&self) -> Result<f64, CompileError> {
        match self {
            Self::F64(v) => Ok(*v),
            other => Err(type_error("value", "f64", other)),
        }
    }

    pub fn as_bool(&self) -> Result<bool, CompileError> {
        match self {
            Self::Bool(v) => Ok(*v),
            other => Err(type_error("value", "bool", other)),
        }
    }

    pub fn as_char_rank(&self) -> Result<u32, CompileError> {
        let rank = self.as_i64()?;
        u32::try_from(rank)
            .map_err(|_| CompileError::codegen(format!("character rank {rank} is out of range")))
    }

    /// Latin-1/Unicode characters of a Simula `text` argument (cloned frame).
    pub fn as_text(&self) -> Result<String, CompileError> {
        match self {
            Self::Text(frame) => Ok(frame.content()),
            other => Err(type_error("value", "text", other)),
        }
    }

    /// Opaque object handle (`none` or a live instance). Hosts that retain the
    /// value across a later call must [`HostCtx::root`] it.
    pub fn as_object_ref(&self) -> Result<Option<usize>, CompileError> {
        match self {
            Self::None => Ok(None),
            Self::ObjectRef(index) => Ok(Some(*index)),
            other => Err(type_error("value", "object", other)),
        }
    }

    /// A constant Simula `text` for host results and tests.
    pub fn text(content: impl AsRef<str>) -> Self {
        Self::Text(TextFrame::from_literal(content.as_ref(), true))
    }
}

/// Scratch roots a host closure can pin so GC does not collect retained
/// `Value::Text` / `Value::ObjectRef` handles.
pub struct HostCtx<'a> {
    roots: &'a mut Vec<Value>,
}

impl HostCtx<'_> {
    /// Keep `value` alive until the interpreter is dropped or [`Self::unroot_all`].
    pub fn root(&mut self, value: Value) -> Value {
        self.roots.push(value.clone());
        value
    }

    pub fn unroot_all(&mut self) {
        self.roots.clear();
    }
}

/// What a [`Value::RefI64`] index resolves to.
#[derive(Debug, Clone, Copy)]
enum RefTarget {
    /// Local slot on a sequencing stack. `frame` is an index into that
    /// stack's `Vec<CallFrame>` (either [`Vm::frames`] when `stack` is the
    /// active target, or a parked stack). Must be stack-relative — absolute
    /// indices into `Vm::frames` go stale across chapter 7 stack switches.
    Local {
        stack: SeqTarget,
        frame: usize,
        local: LocalId,
    },
    Cell {
        base: usize,
        size_bytes: i64,
    },
    /// Byte offset into a heap object (for [`Op::FieldAddr`] captures).
    ObjectField {
        object: usize,
        offset: i64,
    },
}

/// One object instance (`simrt_object_alloc` layout).
#[derive(Debug, Clone)]
pub(super) struct HeapObject {
    bytes: Vec<u8>,
    /// One [`SlotTag`] per 8-byte word, so the collector can trace object
    /// fields precisely instead of guessing which words are heap indices.
    tags: Vec<SlotTag>,
    /// Set by the sweep when the slot goes on [`Vm::free_objects`]; the bytes
    /// are dropped too, so a stale index fails the offset check loudly rather
    /// than silently aliasing whatever is allocated into the slot next.
    dead: bool,
}

/// Interpreter-side N-D array storage (sparse, like the AST interpreter).
#[derive(Debug, Clone)]
enum ArrayStorage {
    I64 {
        bounds: Vec<(i64, i64)>,
        cells: HashMap<Vec<i64>, i64>,
        /// Tags for the non-scalar entries of `cells` (object / text / array
        /// handles stored through the universal i64 element ABI). Absent key
        /// means [`SlotTag::Scalar`].
        cell_tags: HashMap<Vec<i64>, SlotTag>,
    },
    F64 {
        bounds: Vec<(i64, i64)>,
        cells: HashMap<Vec<i64>, f64>,
    },
    Text {
        bounds: Vec<(i64, i64)>,
        cells: HashMap<Vec<i64>, TextFrame>,
    },
    /// A swept slot sitting on [`Vm::free_arrays`]. Every access errors out,
    /// so a stale descriptor is loud instead of silently aliasing.
    Free,
}

/// One activation record on the call stack.
#[derive(Debug, Clone)]
pub(super) struct CallFrame {
    function_index: usize,
    locals: Vec<Value>,
    block_id: BlockId,
    pc: usize,
    /// `(caller_frame_index, dest_local)` for the callee's return value.
    return_to: Option<(usize, LocalId)>,
    /// Local slot -> [`Vm::text_heap`] slot the slot's text *descriptor* lives
    /// in. Native MIR keeps `text` locals as `simrt_text*` pointers, so ops
    /// that mutate a descriptor in place (`:-`, `setpos`, `getchar`, `putint`,
    /// …) are visible through the object field / ref cell the pointer came
    /// from. The interpreter stores descriptors by value, so it records the
    /// origin here and mirrors the write back (see [`Vm::update_text_local`]).
    text_homes: HashMap<usize, usize>,
}

impl CallFrame {
    fn new(
        function: &Function,
        args: Vec<Value>,
        return_to: Option<(usize, LocalId)>,
    ) -> Result<Self, CompileError> {
        let slot_count = function.params.len() + function.locals.len();
        if args.len() != function.params.len() {
            return Err(CompileError::codegen(format!(
                "MIR interp: expected {} arguments for {}, got {}",
                function.params.len(),
                function.name,
                args.len()
            )));
        }
        let mut locals = vec![Value::I64(0); slot_count];
        for (index, arg) in args.into_iter().enumerate() {
            locals[index] = arg;
        }
        Ok(Self {
            function_index: 0, // filled in by caller
            locals,
            block_id: function.entry,
            pc: 0,
            return_to,
            text_homes: HashMap::new(),
        })
    }

    pub(super) fn set_local(&mut self, id: LocalId, value: Value) {
        self.locals[id.0] = value;
        self.text_homes.remove(&id.0);
    }

    /// Like [`Self::set_local`], but remembers where a text descriptor lives so
    /// later in-place mutations can be mirrored back.
    fn set_local_text_home(&mut self, id: LocalId, value: Value, home: Option<usize>) {
        self.locals[id.0] = value;
        match home {
            Some(home) => {
                self.text_homes.insert(id.0, home);
            }
            None => {
                self.text_homes.remove(&id.0);
            }
        }
    }

    fn text_home(&self, id: LocalId) -> Option<usize> {
        self.text_homes.get(&id.0).copied()
    }

    pub(super) fn get_local(&self, id: LocalId) -> Result<&Value, CompileError> {
        self.locals
            .get(id.0)
            .ok_or_else(|| CompileError::codegen(format!("MIR interp: local {} out of range", id)))
    }
}

/// How [`Vm::execute_op`] advances control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExecResult {
    Continue,
    Jump(BlockId),
    Return,
    Call,
    /// `self.frames` has already been fully replaced by a different component's
    /// stack (chapter 7 detach/call/resume/terminate); the run loop must not
    /// touch the old `frame_index`/`pc`, just restart from the new top frame.
    Switch,
    /// Host has no SYSIN record yet; do not advance `pc`.
    NeedStdin,
}

enum RunStop {
    Finished,
    NeedStdin,
}

/// Interpreter state for one module run.
struct Vm<'a> {
    module: &'a Module,
    host: Box<dyn IoHost>,
    stdin_pending: VecDeque<StdinRecord>,
    pub(super) sysout: Output,
    pub(super) sysin: Input,
    pub(super) basicio: BasicioState,
    pub(super) env_state: EnvironmentRuntimeState,
    pub(super) object_identities: HashMap<usize, u64>,
    pub(super) sysin_object: Option<usize>,
    pub(super) sysout_object: Option<usize>,
    pub(super) next_file_identity: u64,
    functions: HashMap<String, usize>,
    host_fns: BTreeMap<
        String,
        Box<dyn FnMut(&mut HostCtx, &[Value]) -> Result<Value, CompileError> + 'a>,
    >,
    /// Values a host closure pinned via [`HostCtx::root`].
    host_roots: Vec<Value>,
    last_return: Option<Value>,
    pub(super) frames: Vec<CallFrame>,
    refs: Vec<RefTarget>,
    cells: Vec<i64>,
    /// Tag per [`Vm::cells`] word, for the same reason as [`HeapObject::tags`].
    cell_tags: Vec<SlotTag>,
    pub(super) objects: Vec<HeapObject>,
    /// Text frames stored in object fields / ref cells (index 0 = first slot;
    /// 0 word = notext). `None` is a swept slot on [`Vm::free_texts`].
    text_heap: Vec<Option<TextFrame>>,
    /// Function names stored in ref cells / i64 carriers (index 0 = first; 0 word = unset).
    func_heap: Vec<String>,
    arrays: Vec<ArrayStorage>,
    terminated: bool,

    // --- Phase 1–2: mark-sweep collector (see `gc`) ---
    /// Swept slots, reused by the next allocation. Reuse only — live indices
    /// are never renumbered, since `ObjectRef` / array / text handles *are*
    /// indices into these vectors.
    free_objects: Vec<usize>,
    free_texts: Vec<usize>,
    free_arrays: Vec<usize>,
    allocs_since_gc: u64,
    /// Object/array allocations between collections; 0 disables automatic
    /// collection entirely (explicit [`Vm::collect`] only).
    gc_threshold: u64,
    /// Set by an allocation that crossed the threshold; the run loop collects
    /// at the next safepoint rather than mid-op with a half-built object.
    pending_gc: bool,
    gc_stats: GcStats,
    gc_stats_enabled: bool,

    // --- Phase 6: SIMSET / chapter 7 sequencing / Simulation (see `seq_ops`) ---
    /// Class id registered by `Op::SimsetSetHeadClassId`, or `-1` if none yet.
    pub(super) simset_head_class_id: i64,
    /// Every component ever created (`new` of a class needing its own stack).
    pub(super) seq_components: Vec<SeqComponent>,
    /// Every quasi-parallel system entered (subblocks/prefixed blocks declaring
    /// a class); indices are stable for the life of the VM.
    pub(super) seq_systems: Vec<SeqSystemState>,
    /// Active `(block, system, owner)` frames, innermost last — mirrors
    /// `simrt_seq_frames` in `runtime/sequencing.c`.
    pub(super) seq_frames: Vec<SeqFrame>,
    /// Object heap index -> component id, for chapter 7's by-reference lookup.
    pub(super) seq_by_object: HashMap<usize, usize>,
    /// Lazily-created system for classes declared outside any instrumented
    /// block (mirrors `simrt_seq_outermost_system`).
    pub(super) seq_outermost_system_id: Option<usize>,
    /// Which stack currently lives in `Vm::frames`.
    pub(super) active_target: SeqTarget,
    /// Parked frames for [`SeqTarget::Outermost`] while some other component
    /// is active (`Vm::frames` holds them again once switched back).
    pub(super) parked_outermost: Option<Vec<CallFrame>>,
    /// Ch.12 sequencing set (SQS) state (`sim.active` is false outside a
    /// Simulation block).
    pub(super) sim: SimState,

    /// Last non-empty [`crate::mir::SpannedOp`] span polled for DAP (statement coalescing).
    #[cfg(feature = "dap")]
    last_debug_span: Option<Span>,
}

impl<'a> Vm<'a> {
    fn new(module: &'a Module) -> Self {
        let functions = module
            .functions
            .iter()
            .enumerate()
            .map(|(index, function)| (function.name.clone(), index))
            .collect();
        let mut vm = Self {
            module,
            host: Box::new(CapturingHost::new()),
            stdin_pending: VecDeque::new(),
            sysout: Output::new(),
            sysin: Input::new(),
            basicio: BasicioState::new(),
            env_state: EnvironmentRuntimeState::new(),
            object_identities: HashMap::new(),
            sysin_object: None,
            sysout_object: None,
            next_file_identity: 1,
            functions,
            host_fns: BTreeMap::new(),
            host_roots: Vec::new(),
            last_return: None,
            frames: Vec::new(),
            refs: Vec::new(),
            cells: Vec::new(),
            cell_tags: Vec::new(),
            objects: Vec::new(),
            text_heap: Vec::new(),
            func_heap: Vec::new(),
            arrays: Vec::new(),
            terminated: false,
            free_objects: Vec::new(),
            free_texts: Vec::new(),
            free_arrays: Vec::new(),
            allocs_since_gc: 0,
            gc_threshold: gc::threshold_from_env(),
            pending_gc: false,
            gc_stats: GcStats::default(),
            gc_stats_enabled: std::env::var("SIM_GC_STATS")
                .map(|value| value == "1")
                .unwrap_or(false),
            simset_head_class_id: -1,
            seq_components: Vec::new(),
            seq_systems: Vec::new(),
            seq_frames: Vec::new(),
            seq_by_object: HashMap::new(),
            seq_outermost_system_id: None,
            active_target: SeqTarget::Outermost,
            parked_outermost: None,
            sim: SimState::default(),
            #[cfg(feature = "dap")]
            last_debug_span: None,
        };
        vm.init_basicio();
        vm
    }

    fn with_host(module: &'a Module, host: Box<dyn IoHost>) -> Self {
        let mut vm = Self::new(module);
        vm.host = host;
        vm
    }

    fn provide_stdin(&mut self, record: StdinRecord) {
        self.stdin_pending.push_back(record);
    }

    fn read_stdin_record(&mut self) -> Result<ReadLine, CompileError> {
        if let Some(record) = self.stdin_pending.pop_front() {
            return Ok(ReadLine::Ready(record));
        }
        self.host.read_line().map_err(CompileError::codegen)
    }

    fn apply_stdin(&mut self) -> Result<ExecResult, CompileError> {
        match self.read_stdin_record()? {
            ReadLine::NeedStdin => Ok(ExecResult::NeedStdin),
            ReadLine::Ready(record) => {
                self.sysin.apply_record(record);
                Ok(ExecResult::Continue)
            }
        }
    }

    pub(super) fn emit_out_image(&mut self) {
        let chunk = self.sysout.out_image();
        self.host.write_stdout(&chunk);
    }

    pub(super) fn emit_break_out_image(&mut self) {
        let chunk = self.sysout.break_out_image();
        self.host.write_stdout(&chunk);
    }

    fn flush_sysout_remainder(&mut self) {
        if self.sysout.has_pending() {
            self.emit_out_image();
        }
    }

    fn take_captured_stdout(&self) -> String {
        self.host.captured_stdout().unwrap_or("").to_string()
    }

    fn run(&mut self) -> Result<RunStop, CompileError> {
        if self.frames.is_empty() && !self.terminated {
            self.push_call("main", vec![], None)?;
        }

        'run: while !self.frames.is_empty() && !self.terminated {
            'block: loop {
                let frame_index = self.frames.len() - 1;
                let function_index = self.frames[frame_index].function_index;
                let block_id = self.frames[frame_index].block_id;

                loop {
                    // Safepoint: between ops every live value is in a local,
                    // a heap slot, or a sequencing root — never half-built.
                    if self.pending_gc {
                        self.collect();
                    }
                    let pc = self.frames[frame_index].pc;
                    let (span, op) = {
                        let function = &self.module.functions[function_index];
                        let block = function.block(block_id);
                        if pc >= block.ops.len() {
                            break;
                        }
                        let spanned = &block.ops[pc];
                        (spanned.span.clone(), spanned.op.clone())
                    };
                    self.poll_debug_span(&span);
                    let function = &self.module.functions[function_index];
                    match self.execute_op(frame_index, function, &op) {
                        Ok(ExecResult::Continue) => {
                            self.frames[frame_index].pc += 1;
                        }
                        Ok(ExecResult::Jump(target)) => {
                            let fi = self.frames.len() - 1;
                            self.frames[fi].block_id = target;
                            self.frames[fi].pc = 0;
                            continue 'block;
                        }
                        Ok(ExecResult::Return) => {
                            self.pop_return()?;
                            continue 'run;
                        }
                        Ok(ExecResult::Call) => {
                            self.frames[frame_index].pc += 1;
                            continue 'run;
                        }
                        Ok(ExecResult::Switch) => {
                            continue 'run;
                        }
                        Ok(ExecResult::NeedStdin) => {
                            return Ok(RunStop::NeedStdin);
                        }
                        Err(error) => return Err(error.with_span(span)),
                    }
                }

                return Err(CompileError::codegen(format!(
                    "MIR interp: block {block_id} fell through without terminator"
                )));
            }
        }

        Ok(RunStop::Finished)
    }

    fn push_call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        return_to: Option<(usize, LocalId)>,
    ) -> Result<(), CompileError> {
        let index = self.functions.get(name).copied().ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: undefined function '{name}'"))
        })?;
        let function = &self.module.functions[index];
        let mut frame = CallFrame::new(function, args, return_to)?;
        frame.function_index = index;
        // Nested calls only — probe already seeds a synthetic `main` frame.
        // Void `Call`s use `return_to: None` but still need a DAP frame.
        let nested = !self.frames.is_empty();
        if nested {
            #[cfg(feature = "dap")]
            crate::debug::push_frame(debug_snap::debug_frame_name(name));
        }
        self.frames.push(frame);
        Ok(())
    }

    fn try_invoke_foreign(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, CompileError> {
        let Some(function) = self.functions.get(name).copied() else {
            return Ok(None);
        };
        let Some(abi) = self.module.functions[function].foreign.clone() else {
            return Ok(None);
        };
        Ok(Some(self.invoke_foreign(&abi, args)?))
    }

    fn invoke_foreign(
        &mut self,
        abi: &crate::mir::ForeignAbi,
        args: &[Value],
    ) -> Result<Value, CompileError> {
        use crate::mir::{ForeignKind, ForeignType};
        match abi.kind {
            ForeignKind::Host => {}
            ForeignKind::Js => {
                return Err(CompileError::codegen(
                    "JS externals require a wasm target (wasm-browser or wasm-node)",
                ));
            }
            ForeignKind::C => {
                return Err(CompileError::codegen(
                    "C externals are not available in the interpreter; compile for native or use kind Host",
                ));
            }
        }
        if args.len() != abi.params.len() {
            return Err(CompileError::codegen(format!(
                "Host procedure '{}' expects {} arguments, got {}",
                abi.ident,
                abi.params.len(),
                args.len()
            )));
        }
        for (index, (ty, arg)) in abi.params.iter().zip(args.iter()).enumerate() {
            match (ty, arg) {
                (ForeignType::I64 | ForeignType::Char, Value::I64(_)) => {}
                (ForeignType::F64, Value::F64(_)) => {}
                (ForeignType::Bool, Value::Bool(_)) => {}
                (ForeignType::TextCopy, Value::Text(_)) => {}
                (ForeignType::ObjectHandle, Value::None | Value::ObjectRef(_)) => {}
                _ => {
                    return Err(CompileError::codegen(format!(
                        "Host procedure '{}' argument {} has the wrong type",
                        abi.ident,
                        index + 1
                    )));
                }
            }
        }
        let ident = abi.ident.clone();
        let Some(mut host_fn) = self.host_fns.remove(&ident) else {
            return Err(CompileError::codegen(format!(
                "unresolved Host procedure '{ident}'"
            )));
        };
        let result = {
            let mut ctx = HostCtx {
                roots: &mut self.host_roots,
            };
            host_fn(&mut ctx, args)
        };
        self.host_fns.insert(ident.clone(), host_fn);
        let result = result?;
        match (abi.result, &result) {
            (None, Value::None) => Ok(result),
            (Some(ForeignType::I64 | ForeignType::Char), Value::I64(_)) => Ok(result),
            (Some(ForeignType::F64), Value::F64(_)) => Ok(result),
            (Some(ForeignType::Bool), Value::Bool(_)) => Ok(result),
            (Some(ForeignType::TextCopy), Value::Text(_)) => Ok(result),
            (Some(ForeignType::ObjectHandle), Value::None | Value::ObjectRef(_)) => Ok(result),
            (None, _) => Ok(Value::None),
            (Some(expected), _) => Err(CompileError::codegen(format!(
                "Host procedure '{}' returned {result:?}, expected {expected:?}",
                abi.ident
            ))),
        }
    }

    fn pop_return(&mut self) -> Result<(), CompileError> {
        self.frames
            .pop()
            .ok_or_else(|| CompileError::codegen("MIR interp: return with empty call stack"))?;
        if !self.frames.is_empty() {
            #[cfg(feature = "dap")]
            crate::debug::pop_frame();
        }
        Ok(())
    }

    /// Pop the innermost frame without copying a return value (§5.4.18).
    fn pop_abandon(&mut self) -> Result<(), CompileError> {
        self.frames.pop().ok_or_else(|| {
            CompileError::codegen("MIR interp: goto escape with empty call stack")
        })?;
        if !self.frames.is_empty() {
            #[cfg(feature = "dap")]
            crate::debug::pop_frame();
        }
        Ok(())
    }

    /// Abandon nested calls until a frame owns `label`, then jump there.
    fn execute_goto_escape(&mut self, label: &str) -> Result<ExecResult, CompileError> {
        let key = label.to_ascii_lowercase();
        loop {
            if self.frames.is_empty() {
                return Err(diagnostics::undefined_label_runtime(label));
            }
            let frame_index = self.frames.len() - 1;
            let function = &self.module.functions[self.frames[frame_index].function_index];
            if let Some(&target) = function.labels.get(&key) {
                self.frames[frame_index].block_id = target;
                self.frames[frame_index].pc = 0;
                return Ok(ExecResult::Jump(target));
            }
            if self.frames.len() == 1 {
                return Err(diagnostics::undefined_label_runtime(label));
            }
            self.pop_abandon()?;
        }
    }

    /// Statement-grouped DAP poll: skip empty / synthetic spans and coalesce
    /// consecutive ops that share the same source span.
    fn poll_debug_span(&mut self, span: &Span) {
        #[cfg(not(feature = "dap"))]
        {
            let _ = span;
        }
        #[cfg(feature = "dap")]
        {
            if span.start >= span.end {
                return;
            }
            // Hot path: `sim run` has no probe — never build snapshots.
            if crate::debug::active_probe().is_none() {
                return;
            }
            if self.last_debug_span.as_ref() == Some(span) {
                return;
            }
            self.last_debug_span = Some(span.clone());
            let snap = self.debug_snapshot();
            crate::debug::poll_mir_span(
                span.clone(),
                snap,
                |name, variables_reference, value_text| {
                    self.debug_set_variable(name, variables_reference, value_text)
                },
            );
        }
    }

    fn execute_op(
        &mut self,
        frame_index: usize,
        function: &Function,
        op: &Op,
    ) -> Result<ExecResult, CompileError> {
        match op {
            Op::Nop => Ok(ExecResult::Continue),

            Op::ConstI64 { dest, value } => {
                self.frames[frame_index].set_local(*dest, Value::I64(*value));
                Ok(ExecResult::Continue)
            }

            Op::ConstF64 { dest, value } => {
                self.frames[frame_index].set_local(*dest, Value::F64(*value));
                Ok(ExecResult::Continue)
            }

            Op::ConstBool { dest, value } => {
                self.frames[frame_index].set_local(*dest, Value::Bool(*value));
                Ok(ExecResult::Continue)
            }

            Op::ConstNone { dest } => {
                self.frames[frame_index].set_local(*dest, Value::None);
                Ok(ExecResult::Continue)
            }

            Op::Copy { dest, src } => {
                self.copy_local(frame_index, *dest, *src)?;
                Ok(ExecResult::Continue)
            }

            Op::I64ToF64 { dest, src } => {
                let value = match self.frames[frame_index].get_local(*src)? {
                    Value::I64(value) => *value,
                    other => return Err(type_error("I64ToF64", "i64", other)),
                };
                self.frames[frame_index].set_local(*dest, Value::F64(value as f64));
                Ok(ExecResult::Continue)
            }

            Op::F64ToI64 { dest, src } => {
                let value = match self.frames[frame_index].get_local(*src)? {
                    Value::F64(value) => *value,
                    other => return Err(type_error("F64ToI64", "f64", other)),
                };
                // Simula `entier`: floor toward −∞.
                self.frames[frame_index].set_local(*dest, Value::I64(value.floor() as i64));
                Ok(ExecResult::Continue)
            }

            Op::LoadLocal { dest, local } => {
                self.copy_local(frame_index, *dest, *local)?;
                Ok(ExecResult::Continue)
            }

            Op::StoreLocal { local, src } => {
                self.copy_local(frame_index, *local, *src)?;
                Ok(ExecResult::Continue)
            }

            Op::Binary {
                dest,
                op,
                left,
                right,
            } => {
                let left_ty = function.local(*left).ty;
                let left_val = self.frames[frame_index].get_local(*left)?;
                let right_val = self.frames[frame_index].get_local(*right)?;
                let result = eval_binary(*op, left_ty, left_val, right_val)?;
                self.frames[frame_index].set_local(*dest, result);
                Ok(ExecResult::Continue)
            }

            Op::Unary { dest, op, src } => {
                let src_ty = function.local(*src).ty;
                let src_val = self.frames[frame_index].get_local(*src)?;
                let result = eval_unary(*op, src_ty, src_val)?;
                self.frames[frame_index].set_local(*dest, result);
                Ok(ExecResult::Continue)
            }

            Op::Compare {
                dest,
                op,
                left,
                right,
            } => {
                let left_ty = function.local(*left).ty;
                let left_val = self.frames[frame_index].get_local(*left)?;
                let right_val = self.frames[frame_index].get_local(*right)?;
                let result = eval_compare(*op, left_ty, left_val, right_val)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(result));
                Ok(ExecResult::Continue)
            }

            Op::LocalAddr { dest, local } => {
                let ref_id = self.refs.len();
                self.refs.push(RefTarget::Local {
                    stack: self.active_target,
                    frame: frame_index,
                    local: *local,
                });
                self.frames[frame_index].set_local(*dest, Value::RefI64(ref_id));
                Ok(ExecResult::Continue)
            }

            Op::StackAlloc { dest, bytes } => {
                let base = self.cells.len();
                let words = ((*bytes + 7) / 8) as usize;
                self.cells.resize(base + words, 0);
                self.cell_tags.resize(base + words, SlotTag::Scalar);
                let ref_id = self.refs.len();
                self.refs.push(RefTarget::Cell {
                    base,
                    size_bytes: *bytes,
                });
                self.frames[frame_index].set_local(*dest, Value::RefI64(ref_id));
                Ok(ExecResult::Continue)
            }

            Op::HeapAlloc { dest, bytes } => {
                let size = match self.frames[frame_index].get_local(*bytes)? {
                    Value::I64(v) => *v,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: HeapAlloc size expected i64, got {other:?}"
                        )));
                    }
                };
                let index = self.alloc_object(size, 0)?;
                self.frames[frame_index].set_local(*dest, Value::ObjectRef(index));
                Ok(ExecResult::Continue)
            }

            Op::LoadRefI64 { dest, ptr, offset } => {
                let dest_ty = function.local(*dest).ty;
                let ptr_val = self.frames[frame_index].get_local(*ptr)?.clone();
                let raw = self.load_ref_i64(&ptr_val, *offset)?;
                let value = self.i64_to_value(dest_ty, raw)?;
                let home = text_home_for(dest_ty, raw);
                self.frames[frame_index].set_local_text_home(*dest, value, home);
                Ok(ExecResult::Continue)
            }

            Op::StoreRefI64 { ptr, src, offset } => {
                let src_ty = function.local(*src).ty;
                let src_val = self.frames[frame_index].get_local(*src)?.clone();
                let (raw, tag) = self.value_to_i64(src_ty, &src_val)?;
                let ptr_val = self.frames[frame_index].get_local(*ptr)?.clone();
                self.store_ref_i64(&ptr_val, *offset, raw, tag)?;
                Ok(ExecResult::Continue)
            }

            Op::FuncAddr { dest, name } => {
                self.frames[frame_index].set_local(*dest, Value::FuncRef(name.clone()));
                Ok(ExecResult::Continue)
            }

            Op::CallIndirect {
                dest,
                callee,
                args,
                sig: _,
            } => {
                let name = match self.frames[frame_index].get_local(*callee)? {
                    Value::FuncRef(name) => name.clone(),
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: CallIndirect expected funcref, got {other:?}"
                        )));
                    }
                };
                let arg_values = collect_locals(&self.frames[frame_index], args)?;
                let return_to = dest.map(|d| (frame_index, d));
                self.push_call(&name, arg_values, return_to)?;
                Ok(ExecResult::Call)
            }

            Op::Jump { target } => Ok(ExecResult::Jump(*target)),

            Op::GotoEscape { label } => self.execute_goto_escape(label),

            Op::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let taken = match self.frames[frame_index].get_local(*cond)? {
                    Value::Bool(value) => *value,
                    Value::I64(value) => *value != 0,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: Branch expected bool, got {other:?}"
                        )));
                    }
                };
                Ok(ExecResult::Jump(if taken {
                    *then_block
                } else {
                    *else_block
                }))
            }

            Op::Return { value } => {
                let return_to = self.frames[frame_index].return_to;
                if let (Some((caller_index, dest)), Some(src)) = (return_to, value) {
                    let returned = self.frames[frame_index].get_local(*src)?.clone();
                    self.frames[caller_index].set_local(dest, returned);
                } else if let Some(src) = value {
                    self.last_return = Some(self.frames[frame_index].get_local(*src)?.clone());
                } else {
                    self.last_return = Some(Value::None);
                }
                Ok(ExecResult::Return)
            }

            Op::Call { dest, name, args } => {
                let arg_values = collect_locals(&self.frames[frame_index], args)?;
                if let Some(result) = self.try_invoke_foreign(name, &arg_values)? {
                    if let Some(dest) = dest {
                        self.frames[frame_index].set_local(*dest, result);
                    }
                    return Ok(ExecResult::Continue);
                }
                let return_to = dest.map(|d| (frame_index, d));
                self.push_call(name, arg_values, return_to)?;
                Ok(ExecResult::Call)
            }

            Op::CallOutInt { value, width } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallOutInt value",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallOutInt width",
                )?;
                let text =
                    basicio::format_outint_field(value, width).map_err(CompileError::codegen)?;
                self.sysout.out_text(&text);
                Ok(ExecResult::Continue)
            }

            Op::CallOutReal {
                value,
                digits,
                width,
            } => {
                // LONG REAL prints a 3-digit exponent (mirrors cranelift's
                // `simrt_out_real_ex` exp argument).
                let exp_digits = if function.local(*value).ty == MirType::LongF64 {
                    3
                } else {
                    2
                };
                let value = expect_f64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallOutReal value",
                )?;
                let digits = expect_i64(
                    self.frames[frame_index].get_local(*digits)?,
                    "CallOutReal digits",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallOutReal width",
                )?;
                let text = basicio_ops::format_basicio_outreal(value, digits, width, exp_digits)?;
                self.sysout.out_text(&text);
                Ok(ExecResult::Continue)
            }

            Op::CallOutFix {
                value,
                digits,
                width,
            } => {
                let value = expect_f64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallOutFix value",
                )?;
                let digits = expect_i64(
                    self.frames[frame_index].get_local(*digits)?,
                    "CallOutFix digits",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallOutFix width",
                )?;
                let text = basicio::format_outfix_field(value, digits, width)
                    .map_err(CompileError::codegen)?;
                self.sysout.out_text(&text);
                Ok(ExecResult::Continue)
            }

            Op::CallOutFrac {
                value,
                digits,
                width,
            } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallOutFrac value",
                )?;
                let digits = expect_i64(
                    self.frames[frame_index].get_local(*digits)?,
                    "CallOutFrac digits",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallOutFrac width",
                )?;
                let text = format_out_frac(value, digits, width)?;
                self.sysout.out_text(&text);
                Ok(ExecResult::Continue)
            }

            Op::CallOutChar { ch } => {
                let ch = i64_to_char(expect_i64(
                    self.frames[frame_index].get_local(*ch)?,
                    "CallOutChar ch",
                )?)?;
                self.sysout.out_char(ch);
                Ok(ExecResult::Continue)
            }

            Op::CallBreakOutImage => {
                self.emit_break_out_image();
                Ok(ExecResult::Continue)
            }

            Op::CallInLine { dest } => {
                match self.read_stdin_record()? {
                    ReadLine::NeedStdin => return Ok(ExecResult::NeedStdin),
                    ReadLine::Ready(StdinRecord::Eof) => {
                        self.frames[frame_index]
                            .set_local(*dest, Value::Text(TextFrame::from_literal("", true)));
                    }
                    ReadLine::Ready(StdinRecord::Line(line)) => {
                        self.frames[frame_index]
                            .set_local(*dest, Value::Text(TextFrame::from_literal(&line, true)));
                    }
                }
                Ok(ExecResult::Continue)
            }

            Op::CallInImage => self.apply_stdin(),

            Op::CallInChar { dest } => {
                // Match BASICIO `file_in_char` / SysIn: refill when exhausted.
                if self.sysin.image().pos() > self.sysin.image().length() {
                    match self.apply_stdin()? {
                        ExecResult::NeedStdin => return Ok(ExecResult::NeedStdin),
                        other => {
                            if other != ExecResult::Continue {
                                return Ok(other);
                            }
                        }
                    }
                }
                let ch = self.sysin.in_char().map_err(CompileError::codegen)?;
                self.frames[frame_index].set_local(*dest, Value::I64(ch as i64));
                Ok(ExecResult::Continue)
            }

            Op::CallEndfile { dest } => {
                self.frames[frame_index].set_local(*dest, Value::Bool(self.sysin.endfile()));
                Ok(ExecResult::Continue)
            }

            Op::CallTerminateProgram => {
                // Clear the call stack and leave via `Switch` (re-enter `run`),
                // not `Return` — that would `pop_return` on an empty stack.
                self.terminated = true;
                self.frames.clear();
                Ok(ExecResult::Switch)
            }

            Op::TextFromLiteral { dest, string_id } => {
                let text = self.string_literal(*string_id)?;
                self.frames[frame_index]
                    .set_local(*dest, Value::Text(TextFrame::from_literal(&text, true)));
                Ok(ExecResult::Continue)
            }

            Op::CallOutText { string_id } => {
                let text = self.string_literal(*string_id)?;
                self.sysout.out_text(&text);
                Ok(ExecResult::Continue)
            }

            Op::CallOutTextLocal { src } => {
                match self.frames[frame_index].get_local(*src)? {
                    Value::Text(text) => self.sysout.out_text(&text.content()),
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: CallOutTextLocal expected text, got {other:?}"
                        )));
                    }
                }
                Ok(ExecResult::Continue)
            }

            Op::CallOutImage => {
                self.emit_out_image();
                Ok(ExecResult::Continue)
            }

            Op::NewObject {
                dest,
                class_id,
                size,
            } => {
                let index = self.alloc_object(*size, *class_id)?;
                self.frames[frame_index].set_local(*dest, Value::ObjectRef(index));
                Ok(ExecResult::Continue)
            }

            Op::FieldLoadI64 {
                dest,
                object,
                offset,
                ..
            } => {
                let index = self.object_index(
                    frame_index,
                    *object,
                    "remote access through none reference",
                )?;
                let dest_ty = function.local(*dest).ty;
                let raw = self.load_object_i64(index, *offset)?;
                let value = self.i64_to_value(dest_ty, raw)?;
                let home = text_home_for(dest_ty, raw);
                self.frames[frame_index].set_local_text_home(*dest, value, home);
                Ok(ExecResult::Continue)
            }

            Op::FieldStoreI64 {
                object,
                offset,
                value,
                ..
            } => {
                let index = self.object_index(
                    frame_index,
                    *object,
                    "remote assignment through none reference",
                )?;
                let src_ty = function.local(*value).ty;
                let src_val = self.frames[frame_index].get_local(*value)?.clone();
                let (raw, tag) = self.value_to_i64(src_ty, &src_val)?;
                self.store_object_i64(index, *offset, raw, tag)?;
                Ok(ExecResult::Continue)
            }

            Op::FieldAddr {
                dest,
                object,
                offset,
            } => {
                let index = self.object_index(
                    frame_index,
                    *object,
                    "remote access through none reference",
                )?;
                let ref_id = self.refs.len();
                self.refs.push(RefTarget::ObjectField {
                    object: index,
                    offset: *offset,
                });
                self.frames[frame_index].set_local(*dest, Value::RefI64(ref_id));
                Ok(ExecResult::Continue)
            }

            Op::ObjectIsNone { dest, object } => {
                let is_none = matches!(self.frames[frame_index].get_local(*object)?, Value::None);
                self.frames[frame_index].set_local(*dest, Value::Bool(is_none));
                Ok(ExecResult::Continue)
            }

            Op::ObjectClassIdSafe { dest, object } => {
                let class_id = match self.frames[frame_index].get_local(*object)? {
                    Value::None => -1,
                    Value::ObjectRef(index) => self.load_object_i64(*index, 0)?,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: ObjectClassIdSafe expected object ref, got {other:?}"
                        )));
                    }
                };
                self.frames[frame_index].set_local(*dest, Value::I64(class_id));
                Ok(ExecResult::Continue)
            }

            Op::Abort { message } => Err(CompileError::codegen(message.clone())),

            Op::TextNotext { dest } => {
                self.frames[frame_index].set_local(*dest, Value::Text(TextFrame::notext()));
                Ok(ExecResult::Continue)
            }

            Op::TextCopy { dest, src } => {
                let src =
                    expect_text(self.frames[frame_index].get_local(*src)?, "TextCopy src")?.clone();
                self.frames[frame_index].set_local(*dest, Value::Text(TextFrame::copy(&src)));
                Ok(ExecResult::Continue)
            }

            Op::TextBlanks { dest, n } => {
                let n = expect_i64(self.frames[frame_index].get_local(*n)?, "TextBlanks n")?;
                let frame = TextFrame::blanks(n).map_err(CompileError::codegen)?;
                self.frames[frame_index].set_local(*dest, Value::Text(frame));
                Ok(ExecResult::Continue)
            }

            Op::TextConcat { dest, left, right } => {
                let left = expect_text(
                    self.frames[frame_index].get_local(*left)?,
                    "TextConcat left",
                )?
                .clone();
                let right = expect_text(
                    self.frames[frame_index].get_local(*right)?,
                    "TextConcat right",
                )?
                .clone();
                self.frames[frame_index].set_local(*dest, Value::Text(left.concat(&right)));
                Ok(ExecResult::Continue)
            }

            Op::TextAssign { dest, src } => {
                let src = expect_text(self.frames[frame_index].get_local(*src)?, "TextAssign src")?
                    .clone();
                self.update_text_local(frame_index, *dest, |dest_frame| {
                    dest_frame
                        .assign_value_from(&src)
                        .map_err(CompileError::codegen)
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextRefAssign { dest, src } => {
                let src = expect_text(
                    self.frames[frame_index].get_local(*src)?,
                    "TextRefAssign src",
                )?
                .clone();
                self.update_text_local(frame_index, *dest, |dest_frame| {
                    text_ref_assign(dest_frame, &src);
                    Ok(())
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextContentEq { dest, left, right } => {
                let left = expect_text(
                    self.frames[frame_index].get_local(*left)?,
                    "TextContentEq left",
                )?
                .clone();
                let right = expect_text(
                    self.frames[frame_index].get_local(*right)?,
                    "TextContentEq right",
                )?
                .clone();
                self.frames[frame_index].set_local(*dest, Value::Bool(left == right));
                Ok(ExecResult::Continue)
            }

            Op::TextContentCmp { dest, left, right } => {
                let left = expect_text(
                    self.frames[frame_index].get_local(*left)?,
                    "TextContentCmp left",
                )?
                .clone();
                let right = expect_text(
                    self.frames[frame_index].get_local(*right)?,
                    "TextContentCmp right",
                )?
                .clone();
                self.frames[frame_index]
                    .set_local(*dest, Value::I64(text_content_cmp(&left, &right)));
                Ok(ExecResult::Continue)
            }

            Op::TextRefEq { dest, left, right } => {
                let left =
                    expect_text(self.frames[frame_index].get_local(*left)?, "TextRefEq left")?
                        .clone();
                let right = expect_text(
                    self.frames[frame_index].get_local(*right)?,
                    "TextRefEq right",
                )?
                .clone();
                self.frames[frame_index]
                    .set_local(*dest, Value::Bool(left.references_same_frame(&right)));
                Ok(ExecResult::Continue)
            }

            Op::TextLength { dest, frame } => {
                let length = expect_text(
                    self.frames[frame_index].get_local(*frame)?,
                    "TextLength frame",
                )?
                .length;
                self.frames[frame_index].set_local(*dest, Value::I64(length));
                Ok(ExecResult::Continue)
            }

            Op::TextConstant { dest, frame } => {
                let constant = expect_text(
                    self.frames[frame_index].get_local(*frame)?,
                    "TextConstant frame",
                )?
                .constant();
                self.frames[frame_index].set_local(*dest, Value::Bool(constant));
                Ok(ExecResult::Continue)
            }

            Op::TextStart { dest, frame } => {
                let start = expect_text(
                    self.frames[frame_index].get_local(*frame)?,
                    "TextStart frame",
                )?
                .start;
                self.frames[frame_index].set_local(*dest, Value::I64(start));
                Ok(ExecResult::Continue)
            }

            Op::TextMain { dest, frame } => {
                let main = expect_text(
                    self.frames[frame_index].get_local(*frame)?,
                    "TextMain frame",
                )?
                .main_frame();
                self.frames[frame_index].set_local(*dest, Value::Text(main));
                Ok(ExecResult::Continue)
            }

            Op::TextPos { dest, frame } => {
                let pos =
                    expect_text(self.frames[frame_index].get_local(*frame)?, "TextPos frame")?.pos;
                self.frames[frame_index].set_local(*dest, Value::I64(pos));
                Ok(ExecResult::Continue)
            }

            Op::TextMore { dest, frame } => {
                let more = expect_text(
                    self.frames[frame_index].get_local(*frame)?,
                    "TextMore frame",
                )?
                .more();
                self.frames[frame_index].set_local(*dest, Value::Bool(more));
                Ok(ExecResult::Continue)
            }

            Op::TextSetpos { frame, index } => {
                let index = expect_i64(
                    self.frames[frame_index].get_local(*index)?,
                    "TextSetpos index",
                )?;
                self.update_text_local(frame_index, *frame, |text| {
                    text.setpos(index);
                    Ok(())
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextGetchar { dest, frame } => {
                let ch = {
                    let mut text = expect_text(
                        self.frames[frame_index].get_local(*frame)?,
                        "TextGetchar frame",
                    )?
                    .clone();
                    let ch = text.getchar().map_err(CompileError::codegen)?;
                    self.frames[frame_index].set_local(*frame, Value::Text(text));
                    ch
                };
                self.frames[frame_index].set_local(*dest, Value::I64(ch as i64));
                Ok(ExecResult::Continue)
            }

            Op::TextPutchar { frame, ch } => {
                let ch = i64_to_char(expect_i64(
                    self.frames[frame_index].get_local(*ch)?,
                    "TextPutchar ch",
                )?)?;
                self.update_text_local(frame_index, *frame, |text| {
                    text.putchar(ch).map_err(CompileError::codegen)
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextGetint { dest, frame } => {
                let value = {
                    let mut text = expect_text(
                        self.frames[frame_index].get_local(*frame)?,
                        "TextGetint frame",
                    )?
                    .clone();
                    let value = text.deedit_getint().map_err(CompileError::codegen)?;
                    self.frames[frame_index].set_local(*frame, Value::Text(text));
                    value
                };
                self.frames[frame_index].set_local(*dest, Value::I64(value));
                Ok(ExecResult::Continue)
            }

            Op::TextPutint { frame, value } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "TextPutint value",
                )?;
                self.update_text_local(frame_index, *frame, |text| {
                    text.edit_putint(value).map_err(CompileError::codegen)
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextGetfrac { dest, frame } => {
                let value = {
                    let mut text = expect_text(
                        self.frames[frame_index].get_local(*frame)?,
                        "TextGetfrac frame",
                    )?
                    .clone();
                    let value = text.deedit_getfrac().map_err(CompileError::codegen)?;
                    self.frames[frame_index].set_local(*frame, Value::Text(text));
                    value
                };
                self.frames[frame_index].set_local(*dest, Value::I64(value));
                Ok(ExecResult::Continue)
            }

            Op::TextPutfrac {
                frame,
                value,
                places,
            } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "TextPutfrac value",
                )?;
                let places = expect_i64(
                    self.frames[frame_index].get_local(*places)?,
                    "TextPutfrac places",
                )?;
                self.update_text_local(frame_index, *frame, |text| {
                    text.edit_putfrac(value, places)
                        .map_err(CompileError::codegen)
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextGetreal { dest, frame } => {
                let value = {
                    let mut text = expect_text(
                        self.frames[frame_index].get_local(*frame)?,
                        "TextGetreal frame",
                    )?
                    .clone();
                    let value = text.deedit_getreal().map_err(CompileError::codegen)?;
                    self.frames[frame_index].set_local(*frame, Value::Text(text));
                    value
                };
                self.frames[frame_index].set_local(*dest, Value::F64(value));
                Ok(ExecResult::Continue)
            }

            Op::TextPutfix {
                frame,
                value,
                places,
            } => {
                let value = expect_f64(
                    self.frames[frame_index].get_local(*value)?,
                    "TextPutfix value",
                )?;
                let places = expect_i64(
                    self.frames[frame_index].get_local(*places)?,
                    "TextPutfix places",
                )?;
                self.update_text_local(frame_index, *frame, |text| {
                    text.edit_putfix(value, places)
                        .map_err(CompileError::codegen)
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextPutreal {
                frame,
                value,
                places,
                exp_digits,
            } => {
                let value = expect_f64(
                    self.frames[frame_index].get_local(*value)?,
                    "TextPutreal value",
                )?;
                let places = expect_i64(
                    self.frames[frame_index].get_local(*places)?,
                    "TextPutreal places",
                )?;
                self.update_text_local(frame_index, *frame, |text| {
                    if *exp_digits == 3 {
                        text.edit_putreal_long_with(value, places, '.', '&')
                    } else {
                        text.edit_putreal(value, places)
                    }
                    .map_err(CompileError::codegen)
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextSub { dest, frame, i, n } => {
                let frame =
                    expect_text(self.frames[frame_index].get_local(*frame)?, "TextSub frame")?
                        .clone();
                let i = expect_i64(self.frames[frame_index].get_local(*i)?, "TextSub i")?;
                let n = expect_i64(self.frames[frame_index].get_local(*n)?, "TextSub n")?;
                let sub = frame.subframe(i, n).map_err(CompileError::codegen)?;
                self.frames[frame_index].set_local(*dest, Value::Text(sub));
                Ok(ExecResult::Continue)
            }

            Op::TextStrip { dest, frame } => {
                let stripped = expect_text(
                    self.frames[frame_index].get_local(*frame)?,
                    "TextStrip frame",
                )?
                .strip();
                self.frames[frame_index].set_local(*dest, Value::Text(stripped));
                Ok(ExecResult::Continue)
            }

            Op::TextUpcase { frame } => {
                self.update_text_local(frame_index, *frame, |text| {
                    text.upcase_in_place().map_err(CompileError::codegen)?;
                    text.setpos(1);
                    Ok(())
                })?;
                Ok(ExecResult::Continue)
            }

            Op::TextLowcase { frame } => {
                self.update_text_local(frame_index, *frame, |text| {
                    text.lowcase_in_place().map_err(CompileError::codegen)?;
                    text.setpos(1);
                    Ok(())
                })?;
                Ok(ExecResult::Continue)
            }

            Op::AllocArray { dest, bounds } => {
                let array_ty = function.local(*dest).ty;
                let bound_pairs = bounds
                    .iter()
                    .map(|(low, high)| {
                        Ok((
                            expect_i64(
                                self.frames[frame_index].get_local(*low)?,
                                "AllocArray low",
                            )?,
                            expect_i64(
                                self.frames[frame_index].get_local(*high)?,
                                "AllocArray high",
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?;
                let index = self.alloc_array(array_ty, bound_pairs)?;
                self.frames[frame_index].set_local(*dest, Value::Array(index));
                Ok(ExecResult::Continue)
            }

            Op::ArrayLoad {
                dest,
                array,
                indices,
            } => {
                let array_index = expect_array(
                    self.frames[frame_index].get_local(*array)?,
                    "ArrayLoad array",
                )?;
                let index_vec = collect_i64_indices(&self.frames[frame_index], indices)?;
                let dest_ty = function.local(*dest).ty;
                let value = self.array_load(array_index, &index_vec, dest_ty)?;
                self.frames[frame_index].set_local(*dest, value);
                Ok(ExecResult::Continue)
            }

            Op::ArrayStore {
                array,
                indices,
                value,
            } => {
                let array_index = expect_array(
                    self.frames[frame_index].get_local(*array)?,
                    "ArrayStore array",
                )?;
                let index_vec = collect_i64_indices(&self.frames[frame_index], indices)?;
                let value_ty = function.local(*value).ty;
                let stored = self.frames[frame_index].get_local(*value)?.clone();
                self.array_store(array_index, &index_vec, value_ty, stored)?;
                Ok(ExecResult::Continue)
            }

            Op::ArrayCopy { dest, src } => {
                let src_index =
                    expect_array(self.frames[frame_index].get_local(*src)?, "ArrayCopy src")?;
                let copied = self.arrays[src_index].deep_copy();
                let index = self.install_array(copied);
                self.frames[frame_index].set_local(*dest, Value::Array(index));
                Ok(ExecResult::Continue)
            }

            Op::CallSysIn { .. }
            | Op::CallSysOut { .. }
            | Op::CallBasicioRegisterFile { .. }
            | Op::CallBasicioOpen { .. }
            | Op::CallBasicioOpenByte { .. }
            | Op::CallBasicioClose { .. }
            | Op::CallBasicioIsOpen { .. }
            | Op::CallBasicioOutText { .. }
            | Op::CallBasicioOutChar { .. }
            | Op::CallBasicioOutImage { .. }
            | Op::CallBasicioBreakOutImage { .. }
            | Op::CallBasicioInImage { .. }
            | Op::CallBasicioInChar { .. }
            | Op::CallBasicioLastItem { .. }
            | Op::CallBasicioInInt { .. }
            | Op::CallBasicioInReal { .. }
            | Op::CallBasicioInFrac { .. }
            | Op::CallBasicioInText { .. }
            | Op::CallBasicioEndfile { .. }
            | Op::CallBasicioInByte { .. }
            | Op::CallBasicioOutByte { .. }
            | Op::CallBasicioLocate { .. }
            | Op::CallBasicioLocation { .. }
            | Op::CallBasicioLastloc { .. }
            | Op::CallBasicioOutReal { .. }
            | Op::CallBasicioOutFix { .. }
            | Op::CallBasicioOutFrac { .. }
            | Op::CallBasicioOutInt { .. }
            | Op::CallBasicioLine { .. }
            | Op::CallBasicioImage { .. }
            | Op::CallBasicioPos { .. }
            | Op::CallBasicioLength { .. }
            | Op::CallBasicioSetImage { .. }
            | Op::CallBasicioSetpos { .. }
            | Op::CallBasicioFilename { .. }
            | Op::CallBasicioSetAccess { .. }
            | Op::CallBasicioEject { .. }
            | Op::CallBasicioLinesPerPage { .. }
            | Op::CallBasicioInRecord { .. }
            | Op::CallEnv { .. }
            | Op::CallFileExists { .. }
            | Op::CallFileRead { .. }
            | Op::CallFileWrite { .. } => self.execute_basicio_or_env(frame_index, function, op),

            Op::SimBegin
            | Op::SimEnd
            | Op::SimHold { .. }
            | Op::SimActivateDirect { .. }
            | Op::SimActivateTimed { .. }
            | Op::SimActivateRelative { .. }
            | Op::SimPassivate
            | Op::SimTransferToHead
            | Op::SimTerminateCurrent { .. }
            | Op::SimCancel { .. }
            | Op::SimFinishMain
            | Op::SimTime { .. }
            | Op::SimIsMainCurrent { .. }
            | Op::SimHasCurrent { .. }
            | Op::SimCurrent { .. }
            | Op::SimMain { .. }
            | Op::SimIdle { .. }
            | Op::SimTerminated { .. }
            | Op::SimEvtime { .. }
            | Op::SimNextev { .. }
            | Op::SimsetSetHeadClassId { .. }
            | Op::SimsetInitHead { .. }
            | Op::SimsetOut { .. }
            | Op::SimsetPrecede { .. }
            | Op::SimsetFollow { .. }
            | Op::SimsetInto { .. }
            | Op::SimsetSuc { .. }
            | Op::SimsetPred { .. }
            | Op::SimsetEmpty { .. }
            | Op::SimsetCardinal { .. }
            | Op::SeqSystemEnter { .. }
            | Op::SeqSystemExit { .. }
            | Op::SeqObjectCreate { .. }
            | Op::SeqObjectStart { .. }
            | Op::SeqBlockInstance { .. }
            | Op::SeqDetach { .. }
            | Op::SeqCall { .. }
            | Op::SeqResume { .. }
            | Op::SeqTerminate { .. } => self.execute_seq_sim_or_simset(frame_index, op),
        }
    }

    pub(super) fn load_ref_i64(&mut self, ptr: &Value, offset: i64) -> Result<i64, CompileError> {
        let ref_id = match ptr {
            Value::RefI64(ref_id) if *ref_id != usize::MAX => *ref_id,
            // Universal i64 cell ABI may leave pointer bits in an `i64` local
            // after Copy; treat encoded index+1 as a RefI64.
            Value::I64(raw) if *raw > 0 => (*raw as usize) - 1,
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR interp: LoadRefI64 expected ref.i64, got {other:?}"
                )));
            }
        };
        let target = self.refs.get(ref_id).copied().ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid ref.i64 index {ref_id}"))
        })?;
        match target {
            RefTarget::Local {
                stack,
                frame: target_frame,
                local,
            } => {
                if offset != 0 {
                    return Err(CompileError::codegen(
                        "MIR interp: non-zero offset on LocalAddr ref",
                    ));
                }
                let (ty, value) = {
                    let frame = self.stack_frame(stack, target_frame)?;
                    let ty = self.module.functions[frame.function_index].local(local).ty;
                    let value = frame.get_local(local)?.clone();
                    (ty, value)
                };
                self.value_to_i64(ty, &value).map(|(raw, _tag)| raw)
            }
            RefTarget::Cell { base, size_bytes } => {
                if offset < 0 || offset + 8 > size_bytes {
                    return Err(CompileError::codegen(format!(
                        "MIR interp: LoadRefI64 offset {offset} out of range for {size_bytes}-byte cell"
                    )));
                }
                let index = base + (offset as usize / 8);
                Ok(self.cells[index])
            }
            RefTarget::ObjectField {
                object,
                offset: field_offset,
            } => {
                let byte_offset = field_offset + offset;
                self.load_object_i64(object, byte_offset)
            }
        }
    }

    pub(super) fn store_ref_i64(
        &mut self,
        ptr: &Value,
        offset: i64,
        raw: i64,
        tag: SlotTag,
    ) -> Result<(), CompileError> {
        let ref_id = match ptr {
            Value::RefI64(ref_id) if *ref_id != usize::MAX => *ref_id,
            Value::I64(encoded) if *encoded > 0 => (*encoded as usize) - 1,
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR interp: StoreRefI64 expected ref.i64, got {other:?}"
                )));
            }
        };
        let target = self.refs.get(ref_id).copied().ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid ref.i64 index {ref_id}"))
        })?;
        match target {
            RefTarget::Local {
                stack,
                frame: target_frame,
                local,
            } => {
                if offset != 0 {
                    return Err(CompileError::codegen(
                        "MIR interp: non-zero offset on LocalAddr ref",
                    ));
                }
                let ty = {
                    let frame = self.stack_frame(stack, target_frame)?;
                    self.module.functions[frame.function_index].local(local).ty
                };
                let value = self.i64_to_value(ty, raw)?;
                self.stack_frame_mut(stack, target_frame)?
                    .set_local(local, value);
            }
            RefTarget::Cell { base, size_bytes } => {
                if offset < 0 || offset + 8 > size_bytes {
                    return Err(CompileError::codegen(format!(
                        "MIR interp: StoreRefI64 offset {offset} out of range for {size_bytes}-byte cell"
                    )));
                }
                let index = base + (offset as usize / 8);
                self.cells[index] = raw;
                self.cell_tags[index] = tag;
            }
            RefTarget::ObjectField {
                object,
                offset: field_offset,
            } => {
                let byte_offset = field_offset + offset;
                self.store_object_i64(object, byte_offset, raw, tag)?;
            }
        }
        Ok(())
    }

    /// Resolve a [`RefTarget::Local`] to a frame on the named sequencing stack.
    fn stack_frame(&self, stack: SeqTarget, frame: usize) -> Result<&CallFrame, CompileError> {
        let frames = self.stack_frames(stack)?;
        frames.get(frame).ok_or_else(|| {
            CompileError::codegen(format!(
                "MIR interp: LocalAddr frame {frame} out of range on parked/active stack"
            ))
        })
    }

    fn stack_frame_mut(
        &mut self,
        stack: SeqTarget,
        frame: usize,
    ) -> Result<&mut CallFrame, CompileError> {
        let frames = self.stack_frames_mut(stack)?;
        let len = frames.len();
        frames.get_mut(frame).ok_or_else(|| {
            CompileError::codegen(format!(
                "MIR interp: LocalAddr frame {frame} out of range on parked/active stack (len {len})"
            ))
        })
    }

    fn stack_frames(&self, stack: SeqTarget) -> Result<&[CallFrame], CompileError> {
        if stack == self.active_target {
            return Ok(&self.frames);
        }
        match stack {
            SeqTarget::Outermost => self.parked_outermost.as_deref().ok_or_else(|| {
                CompileError::codegen(
                    "MIR interp: LocalAddr refers to outermost stack that is not parked",
                )
            }),
            SeqTarget::Component(id) => {
                let component = self.seq_components.get(id).ok_or_else(|| {
                    CompileError::codegen(format!(
                        "MIR interp: LocalAddr refers to unknown component {id}"
                    ))
                })?;
                if component.frames.is_empty() {
                    return Err(CompileError::codegen(format!(
                        "MIR interp: LocalAddr refers to component {id} with no parked frames"
                    )));
                }
                Ok(&component.frames)
            }
        }
    }

    fn stack_frames_mut(&mut self, stack: SeqTarget) -> Result<&mut Vec<CallFrame>, CompileError> {
        if stack == self.active_target {
            return Ok(&mut self.frames);
        }
        match stack {
            SeqTarget::Outermost => self.parked_outermost.as_mut().ok_or_else(|| {
                CompileError::codegen(
                    "MIR interp: LocalAddr refers to outermost stack that is not parked",
                )
            }),
            SeqTarget::Component(id) => {
                let component = self.seq_components.get_mut(id).ok_or_else(|| {
                    CompileError::codegen(format!(
                        "MIR interp: LocalAddr refers to unknown component {id}"
                    ))
                })?;
                if component.frames.is_empty() {
                    return Err(CompileError::codegen(format!(
                        "MIR interp: LocalAddr refers to component {id} with no parked frames"
                    )));
                }
                Ok(&mut component.frames)
            }
        }
    }

    pub(super) fn alloc_object(&mut self, size: i64, class_id: i64) -> Result<usize, CompileError> {
        if size < 8 {
            return Err(CompileError::codegen(format!(
                "MIR interp: invalid object size {size}"
            )));
        }
        let size_usize = size as usize;
        let mut bytes = vec![0u8; size_usize];
        bytes[0..8].copy_from_slice(&class_id.to_le_bytes());
        let object = HeapObject {
            tags: vec![SlotTag::Scalar; size_usize.div_ceil(8)],
            bytes,
            dead: false,
        };
        let index = match self.free_objects.pop() {
            Some(index) => {
                self.objects[index] = object;
                self.gc_stats.slots_reused += 1;
                index
            }
            None => {
                self.objects.push(object);
                self.objects.len() - 1
            }
        };
        self.note_allocation();
        Ok(index)
    }

    pub(super) fn object_index(
        &self,
        frame_index: usize,
        object: LocalId,
        none_message: &str,
    ) -> Result<usize, CompileError> {
        match self.frames[frame_index].get_local(object)? {
            Value::None => Err(diagnostics::none_dereference(none_message)),
            Value::ObjectRef(index) => Ok(*index),
            other => Err(CompileError::codegen(format!(
                "MIR interp: expected object ref, got {other:?}"
            ))),
        }
    }

    pub(super) fn load_object_i64(&self, index: usize, offset: i64) -> Result<i64, CompileError> {
        let object = self.objects.get(index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid object index {index}"))
        })?;
        debug_assert!(
            !object.dead,
            "MIR interp: load from collected object {index}"
        );
        if offset < 0 || offset + 8 > object.bytes.len() as i64 {
            return Err(CompileError::codegen(format!(
                "MIR interp: object field offset {offset} out of range for {}-byte object",
                object.bytes.len()
            )));
        }
        let start = offset as usize;
        Ok(i64::from_le_bytes(
            object.bytes[start..start + 8]
                .try_into()
                .expect("8-byte slice"),
        ))
    }

    pub(super) fn store_object_i64(
        &mut self,
        index: usize,
        offset: i64,
        raw: i64,
        tag: SlotTag,
    ) -> Result<(), CompileError> {
        let object = self.objects.get_mut(index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid object index {index}"))
        })?;
        debug_assert!(
            !object.dead,
            "MIR interp: store into collected object {index}"
        );
        if offset < 0 || offset + 8 > object.bytes.len() as i64 {
            return Err(CompileError::codegen(format!(
                "MIR interp: object field offset {offset} out of range for {}-byte object",
                object.bytes.len()
            )));
        }
        let start = offset as usize;
        object.bytes[start..start + 8].copy_from_slice(&raw.to_le_bytes());
        object.tags[start / 8] = tag;
        Ok(())
    }

    fn string_literal(&self, string_id: usize) -> Result<String, CompileError> {
        self.module.strings.get(string_id).cloned().ok_or_else(|| {
            CompileError::codegen(format!(
                "MIR interp: string pool index {string_id} out of range"
            ))
        })
    }

    /// `dest := src`, keeping any text-descriptor home so the copy still
    /// aliases the same descriptor (native copies the `simrt_text*` pointer).
    fn copy_local(
        &mut self,
        frame_index: usize,
        dest: LocalId,
        src: LocalId,
    ) -> Result<(), CompileError> {
        let value = self.frames[frame_index].get_local(src)?.clone();
        let home = self.frames[frame_index].text_home(src);
        self.frames[frame_index].set_local_text_home(dest, value, home);
        Ok(())
    }

    fn update_text_local(
        &mut self,
        frame_index: usize,
        local: LocalId,
        update: impl FnOnce(&mut TextFrame) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        let mut text = match self.frames[frame_index].get_local(local)? {
            Value::Text(frame) => frame.clone(),
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR interp: expected text local, got {other:?}"
                )));
            }
        };
        update(&mut text)?;
        let home = self.frames[frame_index].text_home(local);
        if let Some(slot) = home.and_then(|home| self.text_heap.get_mut(home)) {
            *slot = Some(text.clone());
        }
        self.frames[frame_index].set_local_text_home(local, Value::Text(text), home);
        Ok(())
    }

    fn alloc_array(
        &mut self,
        array_ty: MirType,
        bounds: Vec<(i64, i64)>,
    ) -> Result<usize, CompileError> {
        let count = dense_array_element_count(&bounds)?;
        if count > i32::MAX as i64 {
            return Err(diagnostics::array_extent_overflow());
        }
        let storage = match array_ty {
            MirType::ArrayI64 => ArrayStorage::I64 {
                bounds,
                cells: HashMap::new(),
                cell_tags: HashMap::new(),
            },
            MirType::ArrayF64 => ArrayStorage::F64 {
                bounds,
                cells: HashMap::new(),
            },
            MirType::ArrayText => ArrayStorage::Text {
                bounds,
                cells: HashMap::new(),
            },
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR interp: AllocArray dest has non-array type {other}"
                )));
            }
        };
        Ok(self.install_array(storage))
    }

    /// Places `storage` in a free slot when one is available, else appends.
    fn install_array(&mut self, storage: ArrayStorage) -> usize {
        let index = match self.free_arrays.pop() {
            Some(index) => {
                self.arrays[index] = storage;
                self.gc_stats.slots_reused += 1;
                index
            }
            None => {
                self.arrays.push(storage);
                self.arrays.len() - 1
            }
        };
        self.note_allocation();
        index
    }

    fn array_load(
        &self,
        index: usize,
        indices: &[i64],
        dest_ty: MirType,
    ) -> Result<Value, CompileError> {
        let array = self.arrays.get(index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid array index {index}"))
        })?;
        check_array_bounds(array, indices)?;
        match array {
            ArrayStorage::I64 { cells, .. } => {
                let raw = *cells.get(indices).unwrap_or(&0);
                if dest_ty == MirType::Bool {
                    Ok(Value::Bool(raw != 0))
                } else if dest_ty == MirType::ObjectRef {
                    if raw == 0 {
                        Ok(Value::None)
                    } else {
                        Ok(Value::ObjectRef(raw as usize - 1))
                    }
                } else {
                    Ok(Value::I64(raw))
                }
            }
            ArrayStorage::F64 { cells, .. } => Ok(Value::F64(*cells.get(indices).unwrap_or(&0.0))),
            ArrayStorage::Text { cells, .. } => Ok(Value::Text(
                cells
                    .get(indices)
                    .cloned()
                    .unwrap_or_else(TextFrame::notext),
            )),
            ArrayStorage::Free => Err(collected_array_error(index)),
        }
    }

    fn array_store(
        &mut self,
        index: usize,
        indices: &[i64],
        value_ty: MirType,
        value: Value,
    ) -> Result<(), CompileError> {
        let array = self.arrays.get_mut(index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid array index {index}"))
        })?;
        check_array_bounds(array, indices)?;
        match array {
            ArrayStorage::I64 {
                cells, cell_tags, ..
            } => {
                let (raw, tag) = match (value_ty, value) {
                    (MirType::Bool, Value::Bool(v)) => (i64::from(v), SlotTag::Scalar),
                    (MirType::ObjectRef, Value::None) => (0, SlotTag::Scalar),
                    (MirType::ObjectRef, Value::ObjectRef(index)) => {
                        (index as i64 + 1, SlotTag::Object)
                    }
                    (_, Value::I64(v)) => (v, SlotTag::Scalar),
                    (_, Value::None) => (0, SlotTag::Scalar),
                    (_, Value::ObjectRef(index)) => (index as i64 + 1, SlotTag::Object),
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: ArrayStore expected i64/bool/object, got {other:?}"
                        )));
                    }
                };
                cells.insert(indices.to_vec(), raw);
                if tag == SlotTag::Scalar {
                    cell_tags.remove(indices);
                } else {
                    cell_tags.insert(indices.to_vec(), tag);
                }
            }
            ArrayStorage::F64 { cells, .. } => {
                let raw = match value {
                    Value::F64(v) => v,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: ArrayStore expected f64, got {other:?}"
                        )));
                    }
                };
                cells.insert(indices.to_vec(), raw);
            }
            ArrayStorage::Text { cells, .. } => {
                let frame = match value {
                    Value::Text(frame) => frame,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: ArrayStore expected text, got {other:?}"
                        )));
                    }
                };
                cells.insert(indices.to_vec(), frame);
            }
            ArrayStorage::Free => return Err(collected_array_error(index)),
        }
        Ok(())
    }

    fn i64_to_value(&self, ty: MirType, raw: i64) -> Result<Value, CompileError> {
        match ty {
            MirType::I64 => Ok(Value::I64(raw)),
            MirType::Bool => Ok(Value::Bool(raw != 0)),
            MirType::F64 | MirType::LongF64 => Ok(Value::F64(f64::from_bits(raw as u64))),
            MirType::ObjectRef => {
                if raw == 0 {
                    Ok(Value::None)
                } else {
                    Ok(Value::ObjectRef(raw as usize - 1))
                }
            }
            MirType::Text => {
                if raw == 0 {
                    Ok(Value::Text(TextFrame::notext()))
                } else {
                    let index = raw as usize - 1;
                    Ok(Value::Text(
                        self.text_heap
                            .get(index)
                            .and_then(|slot| slot.clone())
                            .unwrap_or_else(TextFrame::notext),
                    ))
                }
            }
            MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText => {
                if raw == 0 {
                    Ok(Value::Array(usize::MAX))
                } else {
                    Ok(Value::Array(raw as usize - 1))
                }
            }
            // Enclosing-capture ("$env") slots and name-thunk env pointers.
            // Raw 0 is the null pointer (unwritten capture slot). Capture-reload
            // shims often load/store these without dereferencing — same as native
            // keeping a null i64 in the field (simtst96).
            MirType::RefI64 => {
                if raw == 0 {
                    Ok(Value::RefI64(usize::MAX))
                } else {
                    Ok(Value::RefI64(raw as usize - 1))
                }
            }
            MirType::FuncRef => {
                if raw == 0 {
                    Err(CompileError::codegen(
                        "MIR interp: funcref cell read before initialization",
                    ))
                } else {
                    let index = raw as usize - 1;
                    let name = self.func_heap.get(index).cloned().ok_or_else(|| {
                        CompileError::codegen(format!(
                            "MIR interp: invalid funcref heap index {index}"
                        ))
                    })?;
                    Ok(Value::FuncRef(name))
                }
            }
        }
    }

    /// Encodes `value` as the raw word native would store, together with the
    /// [`SlotTag`] the destination slot must remember so the collector can
    /// trace the word back to its heap slot.
    fn value_to_i64(&mut self, ty: MirType, value: &Value) -> Result<(i64, SlotTag), CompileError> {
        match (ty, value) {
            (MirType::I64, Value::I64(v)) => Ok((*v, SlotTag::Scalar)),
            (MirType::Bool, Value::Bool(v)) => Ok((i64::from(*v), SlotTag::Scalar)),
            // Name-thunk / cell ABI often carries booleans as i64 0/1.
            (MirType::Bool, Value::I64(v)) => Ok((*v, SlotTag::Scalar)),
            (MirType::F64 | MirType::LongF64, Value::F64(v)) => {
                Ok((v.to_bits() as i64, SlotTag::Scalar))
            }
            (MirType::ObjectRef, Value::None) => Ok((0, SlotTag::Scalar)),
            (MirType::ObjectRef, Value::ObjectRef(index)) => {
                Ok((*index as i64 + 1, SlotTag::Object))
            }
            // Lowering sometimes materializes `none` as an i64 0 temporary.
            (MirType::ObjectRef, Value::I64(0)) => Ok((0, SlotTag::Scalar)),
            (MirType::Text, Value::Text(frame)) => Ok((self.intern_text(frame), SlotTag::Text)),
            (MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText, Value::Array(index)) => {
                Ok(encode_array_handle(*index))
            }
            (MirType::RefI64, Value::RefI64(ref_id)) => Ok(encode_ref_handle(*ref_id)),
            // After Copy into an i64 temp and StoreRefI64 into a cell, reloads
            // may present the pointer bits as a plain I64 value.
            (MirType::RefI64, Value::I64(v)) if *v > 0 => Ok((*v, SlotTag::Ref)),
            (MirType::FuncRef, Value::FuncRef(name)) => Ok((self.intern_func(name), SlotTag::Func)),
            (MirType::FuncRef, Value::I64(v)) if *v > 0 => Ok((*v, SlotTag::Func)),
            // Universal i64 cell ABI: Copy may place pointer-sized values into
            // an `i64`-typed temporary before `StoreRefI64` into a stack cell
            // (see name-thunk / formal-procedure env packing).
            (MirType::I64, Value::RefI64(ref_id)) => Ok(encode_ref_handle(*ref_id)),
            (MirType::I64, Value::ObjectRef(index)) => Ok((*index as i64 + 1, SlotTag::Object)),
            (MirType::I64, Value::None) => Ok((0, SlotTag::Scalar)),
            (MirType::I64, Value::Array(index)) => Ok(encode_array_handle(*index)),
            (MirType::I64, Value::FuncRef(name)) => Ok((self.intern_func(name), SlotTag::Func)),
            (MirType::I64, Value::Text(frame)) => Ok((self.intern_text(frame), SlotTag::Text)),
            (MirType::I64, Value::Bool(v)) => Ok((i64::from(*v), SlotTag::Scalar)),
            _ => Err(CompileError::codegen(format!(
                "MIR interp: cannot encode {value:?} as i64 for {ty}"
            ))),
        }
    }

    /// Stores `frame` in a [`Vm::text_heap`] slot (reusing a swept one when
    /// available) and returns the raw word that denotes it.
    fn intern_text(&mut self, frame: &TextFrame) -> i64 {
        let index = match self.free_texts.pop() {
            Some(index) => {
                self.text_heap[index] = Some(frame.clone());
                self.gc_stats.slots_reused += 1;
                index
            }
            None => {
                self.text_heap.push(Some(frame.clone()));
                self.text_heap.len() - 1
            }
        };
        index as i64 + 1
    }

    /// `func_heap` entries are plain names and are never swept (nothing else
    /// points at them, and they cost a `String` each).
    fn intern_func(&mut self, name: &str) -> i64 {
        let index = self.func_heap.len();
        self.func_heap.push(name.to_string());
        index as i64 + 1
    }
}

fn encode_array_handle(index: usize) -> (i64, SlotTag) {
    if index == usize::MAX {
        (0, SlotTag::Scalar)
    } else {
        (index as i64 + 1, SlotTag::Array)
    }
}

fn encode_ref_handle(ref_id: usize) -> (i64, SlotTag) {
    if ref_id == usize::MAX {
        (0, SlotTag::Scalar)
    } else {
        (ref_id as i64 + 1, SlotTag::Ref)
    }
}

fn collected_array_error(index: usize) -> CompileError {
    CompileError::codegen(format!(
        "MIR interp: array descriptor {index} refers to a collected array"
    ))
}

impl ArrayStorage {
    fn deep_copy(&self) -> Self {
        match self {
            Self::I64 {
                bounds,
                cells,
                cell_tags,
            } => Self::I64 {
                bounds: bounds.clone(),
                cells: cells.clone(),
                cell_tags: cell_tags.clone(),
            },
            Self::F64 { bounds, cells } => Self::F64 {
                bounds: bounds.clone(),
                cells: cells.clone(),
            },
            Self::Text { bounds, cells } => Self::Text {
                bounds: bounds.clone(),
                cells: cells
                    .iter()
                    .map(|(key, frame)| (key.clone(), TextFrame::copy(frame)))
                    .collect(),
            },
            Self::Free => Self::Free,
        }
    }
}

/// Computes the dense element count for `bounds`, matching the check native
/// and wasm perform before allocating array storage.
/// Any empty dimension makes the whole array empty (count 0).
fn dense_array_element_count(bounds: &[(i64, i64)]) -> Result<i64, CompileError> {
    let mut count: i64 = 1;
    for &(low, high) in bounds {
        let size = if high >= low {
            high.checked_sub(low)
                .and_then(|d| d.checked_add(1))
                .ok_or_else(diagnostics::array_extent_overflow)?
        } else {
            0
        };
        if size == 0 {
            return Ok(0);
        }
        count = count
            .checked_mul(size)
            .ok_or_else(diagnostics::array_extent_overflow)?;
    }
    Ok(count)
}

fn check_array_bounds(array: &ArrayStorage, indices: &[i64]) -> Result<(), CompileError> {
    let bounds = match array {
        ArrayStorage::I64 { bounds, .. }
        | ArrayStorage::F64 { bounds, .. }
        | ArrayStorage::Text { bounds, .. } => bounds,
        ArrayStorage::Free => {
            return Err(CompileError::codegen(
                "MIR interp: array access through a collected array descriptor",
            ));
        }
    };
    if indices.len() != bounds.len() {
        return Err(crate::diagnostics::array_subscript_count(
            bounds.len(),
            indices.len(),
        ));
    }
    for (index, &(lo, hi)) in indices.iter().zip(bounds.iter()) {
        if lo > hi {
            return Err(CompileError::runtime("array access on an empty dimension"));
        }
        if *index < lo || *index > hi {
            return Err(crate::diagnostics::array_subscript(*index, lo, hi));
        }
    }
    Ok(())
}

fn collect_i64_indices(frame: &CallFrame, indices: &[LocalId]) -> Result<Vec<i64>, CompileError> {
    indices
        .iter()
        .map(|local| expect_i64(frame.get_local(*local)?, "array subscript"))
        .collect()
}

/// The [`Vm::text_heap`] slot a loaded raw word denotes, when the loaded local
/// is a `text` (i.e. a descriptor pointer in native's ABI).
fn text_home_for(ty: MirType, raw: i64) -> Option<usize> {
    if ty == MirType::Text && raw > 0 {
        Some(raw as usize - 1)
    } else {
        None
    }
}

pub(super) fn expect_text<'a>(
    value: &'a Value,
    context: &str,
) -> Result<&'a TextFrame, CompileError> {
    match value {
        Value::Text(frame) => Ok(frame),
        other => Err(CompileError::codegen(format!(
            "MIR interp: {context} expected text, got {other:?}"
        ))),
    }
}

pub(super) fn expect_array(value: &Value, context: &str) -> Result<usize, CompileError> {
    match value {
        Value::Array(index) if *index != usize::MAX => Ok(*index),
        Value::Array(_) => Err(CompileError::codegen(format!(
            "MIR interp: {context} through null array descriptor"
        ))),
        other => Err(CompileError::codegen(format!(
            "MIR interp: {context} expected array, got {other:?}"
        ))),
    }
}

pub(super) fn expect_f64(value: &Value, context: &str) -> Result<f64, CompileError> {
    match value {
        Value::F64(v) => Ok(*v),
        other => Err(CompileError::codegen(format!(
            "MIR interp: {context} expected f64, got {other:?}"
        ))),
    }
}

pub(super) fn i64_to_char(value: i64) -> Result<char, CompileError> {
    char::from_u32(value as u32)
        .ok_or_else(|| CompileError::codegen(format!("MIR interp: invalid character code {value}")))
}

fn text_ref_assign(dest: &mut TextFrame, src: &TextFrame) {
    if src.is_notext() {
        *dest = TextFrame::notext();
        return;
    }
    dest.obj = src.obj.clone();
    dest.start = src.start;
    dest.length = src.length;
    dest.pos = src.pos;
}

fn text_content_cmp(left: &TextFrame, right: &TextFrame) -> i64 {
    match left.content().cmp(&right.content()) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

pub(super) fn format_out_frac(value: i64, digits: i64, width: i64) -> Result<String, CompileError> {
    let width_needed = {
        let mut tmp = TextFrame::blanks(64).map_err(CompileError::codegen)?;
        tmp.edit_putfrac(value, digits)
            .map_err(CompileError::codegen)?;
        tmp.content().trim().chars().count() as i64
    };
    let field_width = if width == 0 {
        width_needed.max(1)
    } else {
        width.abs()
    };
    let mut field = TextFrame::blanks(field_width).map_err(CompileError::codegen)?;
    field
        .edit_putfrac(value, digits)
        .map_err(CompileError::codegen)?;
    Ok(field.content().to_string())
}

fn collect_locals(frame: &CallFrame, args: &[LocalId]) -> Result<Vec<Value>, CompileError> {
    args.iter()
        .map(|arg| frame.get_local(*arg).cloned())
        .collect()
}

fn type_error(op: &str, expected: &str, found: &Value) -> CompileError {
    CompileError::codegen(format!(
        "MIR interp: {op} expected {expected}, got {found:?}"
    ))
}

pub(super) fn expect_i64(value: &Value, context: &str) -> Result<i64, CompileError> {
    match value {
        Value::I64(v) => Ok(*v),
        other => Err(CompileError::codegen(format!(
            "MIR interp: {context} expected i64, got {other:?}"
        ))),
    }
}

fn eval_binary(op: BinOp, ty: MirType, left: &Value, right: &Value) -> Result<Value, CompileError> {
    if ty.is_float() {
        let left = match left {
            Value::F64(v) => *v,
            other => return Err(type_error("binary float op", "f64", other)),
        };
        let right = match right {
            Value::F64(v) => *v,
            other => return Err(type_error("binary float op", "f64", other)),
        };
        let result = match op {
            BinOp::Add => left + right,
            BinOp::Sub => left - right,
            BinOp::Mul => left * right,
            BinOp::Div => left / right,
            BinOp::Pow => {
                // Match `simrt_f64_pow` / AST eval: non-integer exponents
                // on positive bases use exp(y*ln(x)) so simtst06's rpower
                // identity stays within tolerance.
                if left == 0.0 && right <= 0.0 {
                    return Err(crate::diagnostics::exponentiation_undefined());
                }
                if left < 0.0 {
                    if right != right.trunc() {
                        return Err(crate::diagnostics::exponentiation_undefined());
                    }
                    left.powf(right)
                } else if left > 0.0 && right != right.trunc() {
                    (right * left.ln()).exp()
                } else {
                    left.powf(right)
                }
            }
            BinOp::IntDiv | BinOp::And | BinOp::Or => {
                return Err(CompileError::codegen(format!(
                    "MIR interp: {op} invalid on f64 operands"
                )));
            }
        };
        return Ok(Value::F64(result));
    }

    if ty == MirType::Bool {
        let left = match left {
            Value::Bool(v) => *v,
            other => return Err(type_error("binary bool op", "bool", other)),
        };
        let right = match right {
            Value::Bool(v) => *v,
            other => return Err(type_error("binary bool op", "bool", other)),
        };
        let result = match op {
            BinOp::And => left && right,
            BinOp::Or => left || right,
            _ => {
                return Err(CompileError::codegen(format!(
                    "MIR interp: {op} invalid on bool operands"
                )));
            }
        };
        return Ok(Value::Bool(result));
    }

    let left = expect_i64(left, "binary int op left")?;
    let right = expect_i64(right, "binary int op right")?;
    let result = match op {
        BinOp::Add => left.wrapping_add(right),
        BinOp::Sub => left.wrapping_sub(right),
        BinOp::Mul => left.wrapping_mul(right),
        BinOp::Div | BinOp::IntDiv => {
            if right == 0 {
                return Err(crate::diagnostics::division_by_zero());
            }
            // Simula integer division truncates toward zero.
            left / right
        }
        BinOp::And => left & right,
        BinOp::Or => left | right,
        BinOp::Pow => {
            return Err(CompileError::codegen(
                "MIR interp: pow requires f64 operands",
            ));
        }
    };
    Ok(Value::I64(result))
}

fn eval_unary(op: UnOp, ty: MirType, src: &Value) -> Result<Value, CompileError> {
    match op {
        UnOp::Neg => {
            if ty.is_float() {
                let Value::F64(v) = src else {
                    return Err(type_error("unary neg", "f64", src));
                };
                Ok(Value::F64(-v))
            } else {
                let v = expect_i64(src, "unary neg")?;
                Ok(Value::I64(-v))
            }
        }
        UnOp::Not => {
            let Value::Bool(v) = src else {
                return Err(type_error("unary not", "bool", src));
            };
            Ok(Value::Bool(!v))
        }
    }
}

fn eval_compare(op: CmpOp, ty: MirType, left: &Value, right: &Value) -> Result<bool, CompileError> {
    if ty == MirType::ObjectRef {
        let left_id = match left {
            Value::None => None,
            Value::ObjectRef(index) => Some(*index),
            other => return Err(type_error("compare", "object ref", other)),
        };
        let right_id = match right {
            Value::None => None,
            Value::ObjectRef(index) => Some(*index),
            other => return Err(type_error("compare", "object ref", other)),
        };
        return Ok(match op {
            CmpOp::Eq => left_id == right_id,
            CmpOp::Ne => left_id != right_id,
            CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                return Err(CompileError::codegen(
                    "MIR interp: ordering compare invalid on object ref operands",
                ));
            }
        });
    }

    if matches!(
        ty,
        MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
    ) {
        let left_id = match left {
            Value::Array(index) => *index,
            other => return Err(type_error("compare", "array", other)),
        };
        let right_id = match right {
            Value::Array(index) => *index,
            other => return Err(type_error("compare", "array", other)),
        };
        return Ok(match op {
            CmpOp::Eq => left_id == right_id,
            CmpOp::Ne => left_id != right_id,
            CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                return Err(CompileError::codegen(
                    "MIR interp: ordering compare invalid on array operands",
                ));
            }
        });
    }

    if ty == MirType::Text {
        let left = expect_text(left, "compare text")?;
        let right = expect_text(right, "compare text")?;
        // `Compare` on text locals is used for capture-reload checks; match
        // TextFrame's reference-oriented PartialEq (content uses TextContentEq).
        return Ok(match op {
            CmpOp::Eq => left == right,
            CmpOp::Ne => left != right,
            CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                return Err(CompileError::codegen(
                    "MIR interp: ordering compare invalid on text operands",
                ));
            }
        });
    }

    if ty.is_float() {
        let left = match left {
            Value::F64(v) => *v,
            other => return Err(type_error("compare", "f64", other)),
        };
        let right = match right {
            Value::F64(v) => *v,
            other => return Err(type_error("compare", "f64", other)),
        };
        return Ok(match op {
            CmpOp::Eq => left == right,
            CmpOp::Ne => left != right,
            CmpOp::Lt => left < right,
            CmpOp::Le => left <= right,
            CmpOp::Gt => left > right,
            CmpOp::Ge => left >= right,
        });
    }

    let left = match left {
        Value::I64(v) => *v,
        Value::Bool(v) => i64::from(*v),
        other => return Err(type_error("compare", "i64/bool", other)),
    };
    let right = match right {
        Value::I64(v) => *v,
        Value::Bool(v) => i64::from(*v),
        other => return Err(type_error("compare", "i64/bool", other)),
    };
    Ok(match op {
        CmpOp::Eq => left == right,
        CmpOp::Ne => left != right,
        CmpOp::Lt => left < right,
        CmpOp::Le => left <= right,
        CmpOp::Gt => left > right,
        CmpOp::Ge => left >= right,
    })
}

/// Poll result from [`Interpreter::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpretPoll {
    NeedStdin,
    Exited,
}

/// Resumable MIR interpreter session (streaming stdio via [`IoHost`]).
///
/// This is the Rust embedding API: load a lowered
/// [`Module`], install Host closures, then [`Self::call`] named procedures
/// (including a Simulation `tick`) instead of running `main` to completion.
pub struct Interpreter<'a> {
    vm: Vm<'a>,
}

impl<'a> Interpreter<'a> {
    pub fn new(module: &'a Module, host: Box<dyn IoHost>) -> Self {
        Self {
            vm: Vm::with_host(module, host),
        }
    }

    pub fn from_module(module: &'a Module) -> Self {
        Self::new(module, Box::new(CapturingHost::new()))
    }

    pub fn define_host<F>(&mut self, name: impl Into<String>, f: F)
    where
        F: FnMut(&mut HostCtx, &[Value]) -> Result<Value, CompileError> + 'a,
    {
        self.vm.host_fns.insert(name.into(), Box::new(f));
    }

    /// Call an exported / top-level MIR function. Foreign stubs dispatch to
    /// [`Self::define_host`]; Simula procedures run until they return.
    pub fn call(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, CompileError> {
        if let Some(result) = self.vm.try_invoke_foreign(name, args)? {
            return Ok(match result {
                Value::None => None,
                other => Some(other),
            });
        }
        self.vm.last_return = None;
        self.vm.push_call(name, args.to_vec(), None)?;
        match self.vm.run()? {
            RunStop::Finished => Ok(self.vm.last_return.take().filter(|v| *v != Value::None)),
            RunStop::NeedStdin => Err(CompileError::codegen("MIR interp: call blocked on stdin")),
        }
    }

    pub fn provide_stdin(&mut self, record: StdinRecord) {
        self.vm.provide_stdin(record);
    }

    pub fn poll(&mut self) -> Result<InterpretPoll, CompileError> {
        match self.vm.run()? {
            RunStop::NeedStdin => Ok(InterpretPoll::NeedStdin),
            RunStop::Finished => {
                self.vm.flush_sysout_remainder();
                self.vm.report_gc_stats();
                Ok(InterpretPoll::Exited)
            }
        }
    }

    pub fn gc_stats(&self) -> GcStats {
        self.vm.gc_stats.clone()
    }

    pub fn take_captured_stdout(&self) -> String {
        self.vm.take_captured_stdout()
    }
}

/// Runs `module`'s `main` function and returns captured stdout (OutText /
/// OutImage / …).
pub fn interpret_module(module: &Module) -> Result<String, CompileError> {
    interpret_module_with_gc(module, GcOptions::default()).map(|(output, _stats)| output)
}

/// Interpret with a caller-supplied host. Capturing hosts that run out of
/// queued stdin yield EOF (same as an empty process stdin). A host that
/// returns [`ReadLine::NeedStdin`] errors — use [`Interpreter::poll`] instead.
pub fn interpret_module_with_host(
    module: &Module,
    host: Box<dyn IoHost>,
) -> Result<(), CompileError> {
    let mut interp = Interpreter::new(module, host);
    match interp.poll()? {
        InterpretPoll::Exited => Ok(()),
        InterpretPoll::NeedStdin => Err(CompileError::codegen(
            "interpreter paused on stdin; use Interpreter::poll for interactive hosts",
        )),
    }
}

/// Like [`interpret_module`], but with the collector under test control and
/// the cumulative [`GcStats`] returned alongside stdout.
pub fn interpret_module_with_gc(
    module: &Module,
    options: GcOptions,
) -> Result<(String, GcStats), CompileError> {
    let mut vm = Vm::new(module);
    if let Some(every) = options.collect_every {
        vm.gc_threshold = every;
    }
    match vm.run()? {
        RunStop::Finished => {}
        RunStop::NeedStdin => {
            return Err(CompileError::codegen(
                "unexpected stdin wait in capturing interpreter",
            ));
        }
    }
    if options.force_collect_at_end {
        vm.collect();
    }
    vm.flush_sysout_remainder();
    vm.report_gc_stats();
    let stats = vm.gc_stats.clone();
    Ok((vm.take_captured_stdout(), stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::lower_program;
    use crate::parse::test_support::parse_program;

    fn interpret_source(source: &str) -> String {
        let program = parse_program(source);
        let module = lower_program(&program)
            .unwrap_or_else(|error| panic!("expected lowering to succeed: {error}"));
        interpret_module(&module)
            .unwrap_or_else(|error| panic!("expected interpretation to succeed: {error}"))
    }

    #[test]
    fn hello_world_out_text_and_out_image() {
        let output = interpret_source(r#"begin OutText("hello world"); OutImage; end;"#);
        assert_eq!(output, "hello world\n");
    }

    #[test]
    fn pauses_on_inimage_until_stdin_is_provided() {
        struct PauseHost {
            stdout: String,
        }
        impl IoHost for PauseHost {
            fn write_stdout(&mut self, text: &str) {
                self.stdout.push_str(text);
            }
            fn write_stderr(&mut self, _text: &str) {}
            fn read_line(&mut self) -> Result<ReadLine, String> {
                Ok(ReadLine::NeedStdin)
            }
            fn captured_stdout(&self) -> Option<&str> {
                Some(&self.stdout)
            }
        }

        let program = parse_program(
            r#"begin OutText("name?"); OutImage; InImage; OutText("done"); OutImage; end;"#,
        );
        let module = lower_program(&program).expect("lower");
        let mut interp = Interpreter::new(
            &module,
            Box::new(PauseHost {
                stdout: String::new(),
            }),
        );
        assert_eq!(interp.poll().expect("poll"), InterpretPoll::NeedStdin);
        assert_eq!(interp.take_captured_stdout(), "name?\n");
        interp.provide_stdin(StdinRecord::Line("eirik".into()));
        assert_eq!(interp.poll().expect("poll"), InterpretPoll::Exited);
        assert_eq!(interp.take_captured_stdout(), "name?\ndone\n");
    }

    #[test]
    fn integer_arithmetic_out_int() {
        let output = interpret_source(
            r#"begin
                integer x;
                x := 40 + 2;
                OutInt(x, 0);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "42\n");
    }

    #[test]
    fn if_else_then_branch() {
        let output = interpret_source(
            r#"begin
                integer x;
                x := 1;
                if x > 0 then OutText("yes") else OutText("no");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "yes\n");
    }

    #[test]
    fn if_else_else_branch() {
        let output = interpret_source(
            r#"begin
                integer x;
                x := 0;
                if x > 0 then OutText("yes") else OutText("no");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "no\n");
    }

    #[test]
    fn recursive_factorial() {
        let output = interpret_source(
            r#"begin
                integer procedure fact(n); value n; integer n;
                begin
                    if n <= 1 then fact := 1
                    else fact := n * fact(n - 1);
                end;
                OutInt(fact(5), 0);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "120\n");
    }

    #[test]
    fn object_new_field_store_and_load() {
        let output = interpret_source(
            r#"begin
                class Point; begin integer x; end;
                ref(Point) p;
                p :- new Point;
                p.x := 42;
                OutInt(p.x, 0);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "42\n");
    }

    #[test]
    fn object_method_reads_field() {
        let output = interpret_source(
            r#"begin
                class Counter; begin
                    integer n;
                    procedure increment; begin n := n + 1; end;
                    integer procedure get; begin get := n; end;
                end;
                ref(Counter) c;
                c :- new Counter;
                c.increment();
                c.increment();
                OutInt(c.get(), 0);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "2\n");
    }

    #[test]
    fn nested_class_captures_and_mutates_enclosing_integer() {
        // Regression test for the RefI64 object-field encoding gap (Phase 6
        // note / Phase 7 hardening item): a class declared inside a block
        // implicitly captures a free variable from the enclosing scope
        // (§5.5) as a *by-reference* capture slot — an object field whose
        // MIR type is `RefI64` (a pointer to the block-local's cell), not a
        // plain `I64` copy — because scalars/`ref`s are one shared variable,
        // not a snapshot per instance. Before this fix, `field_value_to_i64`
        // / `field_i64_to_value` only knew how to (de)serialize `I64` /
        // `Bool` / `F64` / `ObjectRef` / `Text` into an object's byte field
        // and rejected `Value::RefI64`, so simply creating `Inner` aborted
        // interpretation with "cannot encode RefI64(..) as i64 for ref.i64".
        let output = interpret_source(
            r#"begin
                integer outer;
                outer := 10;
                class Nested;
                begin
                    outer := outer + 5;
                end;
                ref(Nested) obj;
                obj :- new Nested;
                OutInt(outer, 0);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "15\n");
    }

    #[test]
    fn detach_resume_preserves_enclosing_ref_captures() {
        // LocalAddr of an outermost `ref` must remain valid after chapter 7
        // stack switches — frame indices into `Vm::frames` go stale when the
        // active Vec is swapped.
        let output = interpret_source(
            r#"begin
                ref(Y) yy; ref(X) xx;
                class Y; begin
                    OutText("Y1"); OutImage; detach;
                    OutText("Y2"); OutImage; resume(xx);
                    OutText("Y3"); OutImage;
                end;
                class X; begin
                    OutText("X1"); OutImage; detach;
                    OutText("X2"); OutImage;
                    resume(yy);
                    OutText("X3"); OutImage;
                end;
                yy :- new Y;
                xx :- new X;
                OutText("M1"); OutImage;
                resume(xx);
                OutText("M2"); OutImage;
            end;"#,
        );
        // After X finishes, control returns to main; Y may still be detached
        // at Y3 depending on whether X's terminate resumes the system main.
        assert!(
            output.starts_with("Y1\nX1\nM1\nX2\nY2\nX3\n"),
            "unexpected sequencing: {output:?}"
        );
        assert!(output.contains("M2\n"), "unexpected sequencing: {output:?}");
    }

    #[test]
    fn name_param_local_addr_survives_call_detach() {
        // Outlined call-by-name (`LocalAddr` of the actual in the thunk env)
        // must still write the caller's local after a chapter 7 stack switch
        // while the name binding is live (`detach` inside a class method, then
        // `call` resume). Without `RefTarget::Local { stack: SeqTarget, … }`,
        // the post-switch store would hit the wrong `Vm::frames` entry.
        let output = interpret_source(
            r#"begin
                integer n;
                ref(Worker) w;
                class Worker;
                begin
                    procedure mutate(x); name x; integer x;
                    begin
                        x := x + 10;
                        detach;
                        x := x + 100;
                        if x = -999999 then mutate(x);
                    end;
                    mutate(n);
                end;
                n := 1;
                w :- new Worker;
                OutInt(n, 0); OutImage;
                call(w);
                OutInt(n, 0); OutImage;
            end;"#,
        );
        assert_eq!(output, "11\n111\n");
    }

    #[test]
    fn object_array_field_stores_descriptor() {
        // Array descriptors in object fields use the same index+1 i64 encoding
        // as ObjectRef (see `Vm::value_to_i64`).
        let output = interpret_source(
            r#"begin
                class Box;
                begin
                    integer array a(1:2);
                    a(1) := 7;
                    a(2) := 9;
                    OutInt(a(1), 0); OutText(" ");
                    OutInt(a(2), 0); OutImage;
                end;
                ref(Box) b;
                b :- new Box;
            end;"#,
        );
        assert_eq!(output, "7 9\n");
    }

    #[test]
    fn text_ref_assign_through_object_field_updates_the_attribute() {
        // `x.t :- …` lowers to `field_load` + `text.ref_assign`, which mutates
        // the loaded descriptor in place (native: a `simrt_text*` pointer),
        // so the write must reach the attribute (simtst33).
        let output = interpret_source(
            r#"begin
                class D(t); value t; text t;;
                ref(D) rD;
                rD :- new D("1");
                rD.t :- copy("8");
                OutText(rD.t); OutImage;
            end;"#,
        );
        assert_eq!(output, "8\n");
    }

    #[test]
    fn text_ref_assign_through_ref_parameter_updates_the_caller_object() {
        // Same descriptor write-back, reached through a `ref(D)` parameter and
        // an inherited attribute (simtst33 P1/P2/P3).
        let output = interpret_source(
            r#"begin
                class D(t); value t; text t;;
                D class E;;
                ref(E) rE;
                procedure P(Ef); ref(E) Ef;
                begin
                    Ef.t :- copy("6");
                end;
                rE :- new E("2");
                P(rE);
                OutText(rE.t); OutImage;
            end;"#,
        );
        assert_eq!(output, "6\n");
    }

    #[test]
    fn text_attribute_keeps_its_own_descriptor_after_ref_assign() {
        // `:-` copies the *descriptor* into the attribute's own cell, so later
        // rebinding the source text must not follow into the attribute.
        let output = interpret_source(
            r#"begin
                class F; begin text u; end;
                ref(F) rF;
                text t;
                rF :- new F;
                t :- copy("ccc");
                rF.u :- t;
                t :- copy("ddd");
                OutText(rF.u); OutImage;
            end;"#,
        );
        assert_eq!(output, "ccc\n");
    }

    #[test]
    fn free_outreal_uses_three_exponent_digits_for_long_real() {
        // `Outreal` on a LONG REAL prints a 3-digit exponent, matching
        // cranelift's `simrt_out_real_ex` exp argument (simtst28).
        let output = interpret_source(
            r#"begin
                real r;
                long real l;
                r := 100.1;
                l := 200.2&-5;
                Outreal(r, 5, 12);
                Outreal(l, 5, 12);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "  1.0010&+02 2.0020&-003\n");
    }

    #[test]
    fn name_procedure_param_passed_through_formal_procedure() {
        // Packing formal-procedure env refs into stack cells uses Copy into
        // i64 temps + StoreRefI64 (universal i64 cell ABI).
        let output = interpret_source(
            r#"begin
                boolean bool;
                procedure P(F, a); name F, a; procedure F; boolean a;
                begin
                    a := not a;
                    if a then P2(F) else F;
                end;
                procedure P2(F); procedure F;
                begin boolean a;
                    a := true;
                    P(F, a);
                    bool := true;
                    if bool then P(Q1, bool) else P(Q2, bool);
                end;
                procedure Q1;
                begin OutText("Q1"); OutImage; end;
                integer procedure Q2;
                begin OutText("Q2"); OutImage; end;
                bool := false;
                if bool then P(Q1, bool) else P(Q2, bool);
            end;"#,
        );
        assert_eq!(output, "Q2\nQ1\n");
    }

    #[test]
    fn call_env_min_max_are_binary_not_constants() {
        let output = interpret_source(
            r#"begin
                OutFix(min(1.0, maxreal), 6, 20);
                OutImage;
                OutInt(max(3, 9), 0);
                OutImage;
            end;"#,
        );
        assert!(output.contains("1.000000"), "got {output:?}");
        assert!(output.contains("9\n"), "got {output:?}");
    }

    #[test]
    fn object_ref_array_store_and_load() {
        let output = interpret_source(
            r#"begin
                class Item; begin end;
                ref(Item) array slot(1:1);
                ref(Item) p;
                p :- new Item;
                slot(1) :- p;
                if slot(1) == p then OutText("ok") else OutText("bad");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "ok\n");
    }

    #[test]
    fn object_none_is_none() {
        let output = interpret_source(
            r#"begin
                class Point; begin integer x; end;
                ref(Point) p;
                p :- none;
                if p = none then OutText("none") else OutText("obj");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "none\n");
    }

    #[test]
    fn text_concat_and_out() {
        let output = interpret_source(
            r#"begin
                text t;
                t := "hel" & "lo";
                OutText(t);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "hello\n");
    }

    #[test]
    fn text_sub_out() {
        let output = interpret_source(
            r#"begin
                text t;
                t := "hello";
                OutText(t.sub(2, 3));
                OutImage;
            end;"#,
        );
        assert_eq!(output, "ell\n");
    }

    #[test]
    fn text_pos_reports_one_based() {
        let output = interpret_source(
            r#"begin
                text t;
                t := "abc";
                if t.pos = 1 then OutText("ok") else OutText("bad");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "ok\n");
    }

    #[test]
    fn integer_array_store_and_load() {
        let output = interpret_source(
            r#"begin
                integer array a(1:10);
                integer x;
                a(3) := 42;
                x := a(3);
                if x = 42 then OutText("ok") else OutText("bad");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "ok\n");
    }

    #[test]
    fn declared_text_defaults_to_notext() {
        let output = interpret_source(
            r#"begin
                text t;
                OutText(t);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "\n");
    }

    #[test]
    fn sysout_outtext_and_outimage() {
        let output = interpret_source(
            r#"begin
                sysout.outtext("x");
                sysout.outimage;
            end;"#,
        );
        assert_eq!(output, "x\n");
    }

    #[test]
    fn outfile_write_text_and_close() {
        let path = std::env::temp_dir().join("simc_mir_interp_outfile.sim");
        let path_str = path.to_string_lossy();
        let source = format!(
            r#"begin
                ref(OutFile) f;
                f :- new OutFile("{path_str}");
                f.open(blanks(80));
                f.outtext("fileok");
                f.outimage;
                f.close;
            end;"#
        );
        let _ = std::fs::remove_file(&path);
        interpret_source(&source);
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(contents.starts_with("fileok"));
        assert!(contents.ends_with('\n'));
        assert_eq!(contents.len(), 81); // blanks(80) image + newline
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn call_env_sqrt() {
        let output = interpret_source(
            r#"begin
                real r;
                r := sqrt(16.0);
                OutFix(r, 0, 0);
                OutImage;
            end;"#,
        );
        assert_eq!(output, "4\n");
    }

    // --------------------------------------------------------------- Phase 6

    #[test]
    fn simset_head_into_and_suc_walk() {
        let output = interpret_source(
            r#"begin
                ref(head) h;
                ref(link) a, b, c;
                h :- new head;
                a :- new link;
                b :- new link;
                c :- new link;
                a.into(h);
                b.into(h);
                c.into(h);
                if h.first == a then OutText("first-a") else OutText("first-?");
                OutImage;
                if h.last == c then OutText("last-c") else OutText("last-?");
                OutImage;
                if a.suc == b then OutText("a-suc-b") else OutText("a-suc-?");
                OutImage;
                if b.pred == a then OutText("b-pred-a") else OutText("b-pred-?");
                OutImage;
                if c.suc == none then OutText("c-suc-none") else OutText("c-suc-?");
                OutImage;
                OutInt(h.cardinal, 0); OutImage;
                b.out;
                OutInt(h.cardinal, 0); OutImage;
                if a.suc == c then OutText("a-suc-c-after-out") else OutText("a-suc-?");
                OutImage;
                if h.empty then OutText("empty") else OutText("nonempty");
                OutImage;
            end;"#,
        );
        assert_eq!(
            output,
            "first-a\nlast-c\na-suc-b\nb-pred-a\nc-suc-none\n3\n2\na-suc-c-after-out\nnonempty\n"
        );
    }

    #[test]
    fn simset_head_empty_after_clearing_all_members() {
        let output = interpret_source(
            r#"begin
                ref(head) h;
                ref(link) a;
                h :- new head;
                a :- new link;
                a.into(h);
                if h.empty then OutText("empty") else OutText("nonempty");
                OutImage;
                a.out;
                if h.empty then OutText("empty") else OutText("nonempty");
                OutImage;
            end;"#,
        );
        assert_eq!(output, "nonempty\nempty\n");
    }

    #[test]
    fn detach_call_roundtrip_returns_to_the_caller() {
        let output = interpret_source(
            r#"begin
                class C;
                begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end;
                ref(C) c;
                c :- new C;
                OutText("M"); OutImage;
                call(c);
                OutText("Z"); OutImage;
            end;"#,
        );
        assert_eq!(output, "A\nM\nB\nZ\n");
    }

    #[test]
    fn double_detach_requires_two_calls() {
        let output = interpret_source(
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
        assert_eq!(output, "1\nx\n2\ny\n3\n");
    }

    #[test]
    fn resume_from_main_matches_call_ordering() {
        let output = interpret_source(
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
        assert_eq!(output, "A\nC\nB\n");
    }

    #[test]
    fn detach_deep_in_loops_resumes_at_the_right_program_point() {
        let output = interpret_source(
            r#"begin
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
                                OutInt(i, 2); OutInt(j, 2); OutImage;
                                detach;
                            end;
                        end;
                    end;
                    OutText("done"); OutImage;
                end;
                ref(C) c;
                c :- new C;
                OutText("m1"); OutImage;
                call(c);
                OutText("m2"); OutImage;
                call(c);
                OutText("m3"); OutImage;
            end;"#,
        );
        assert_eq!(output, " 1 2\nm1\n 2 2\nm2\ndone\nm3\n");
    }

    #[test]
    fn simulation_activate_and_hold_orders_two_processes() {
        let output = interpret_source(
            r#"Simulation begin
                process class Ticker(mark); text mark;
                begin
                    OutText(mark); OutImage;
                    hold(1);
                    OutText(mark); OutImage;
                end;
                activate new Ticker("a");
                activate new Ticker("b");
                hold(3);
                OutText("done"); OutImage;
            end;"#,
        );
        assert_eq!(output, "a\nb\na\nb\ndone\n");
    }

    #[test]
    fn simulation_epilogue_lets_still_scheduled_processes_finish_after_main() {
        // Once MAIN's own block ends, §12's "hand off to the sequencing set"
        // epilogue (`SimFinishMain` + `SimTransferToHead`) keeps running any
        // still-scheduled process to completion before the whole program
        // exits — MAIN falling off the end is not itself the end of the run.
        let output = interpret_source(
            r#"Simulation begin
                process class Ticker;
                begin
                    integer i;
                    for i := 1 step 1 until 3 do
                    begin
                        OutText("tick"); OutImage;
                        hold(1);
                    end;
                end;
                activate new Ticker;
                hold(1);
                OutText("done"); OutImage;
            end;"#,
        );
        assert_eq!(output, "tick\ntick\ndone\ntick\n");
    }

    #[test]
    fn simulation_wait_queue_and_release_via_simset() {
        // §12.4's `wait(q)` (`into(q); passivate`) combined with SIMSET
        // traversal (`q.first`) to release the waiter — the two Phase 6
        // subsystems (SIMSET + Simulation) working together.
        let output = interpret_source(
            r#"Simulation begin
                ref(head) q;
                process class Waiter(rq); ref(head) rq;
                begin
                    OutText("wait-in"); OutImage;
                    wait(rq);
                    OutText("wait-out"); OutImage;
                end;
                ref(Waiter) w;
                q :- new head;
                w :- new Waiter(q);
                activate w;
                hold(1);
                OutInt(q.cardinal, 0); OutImage;
                activate q.first;
                hold(1);
                OutText("done"); OutImage;
            end;"#,
        );
        assert_eq!(output, "wait-in\n1\nwait-out\ndone\n");
    }

    #[test]
    fn simulation_passivate_and_time_track_scheduling() {
        let output = interpret_source(
            r#"Simulation begin
                process class P;
                begin
                    OutText("start"); OutImage;
                    passivate;
                    OutText("resumed"); OutImage;
                end;
                ref(P) p;
                p :- new P;
                activate p;
                hold(0);
                OutFix(time, 0, 0); OutImage;
                if p.idle then OutText("idle") else OutText("scheduled");
                OutImage;
                activate p;
                hold(0);
                OutText("done"); OutImage;
            end;"#,
        );
        assert_eq!(output, "start\n0\nidle\nresumed\ndone\n");
    }
}
