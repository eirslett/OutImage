//! Lowers the per-component-stack sequencing ops onto targets that have no
//! stack switching primitive (WebAssembly).
//!
//! The native backend gives every Chapter 7 component a real call stack, so
//! `seq.detach` / `seq.call` / `seq.resume` are context switches. WebAssembly
//! cannot switch stacks, so a component's call stack has to live on the heap:
//! suspending unwinds the physical stack back to a trampoline, spilling each
//! active frame's locals, and resuming rewinds by re-entering those frames and
//! jumping back to the instruction after the transfer. This is the same shape
//! as Emscripten's Asyncify.
//!
//! Only functions that can be on a component's stack at a transfer need that
//! treatment. The C runtime never calls back into Simula, so the set is closed
//! and computable from the MIR call graph alone: the transfer ops themselves,
//! plus every function that can reach one.

use std::collections::{HashMap, HashSet};

use super::seq_runtime;
use super::sim_runtime;
use super::{
    BasicBlock, BlockId, CmpOp, Function, Local, LocalId, MirType, Module, Op, Span, SpannedOp,
};

/// Rewrites the module so a target with one call stack can still run chapter 7:
/// the sequencing ops become calls into the synthesized runtime, and every
/// function a transfer can unwind through learns to spill its locals on the way
/// out and pick them back up on the way in.
///
/// A no-op for a module that never sequences, so the runtime and its trampoline
/// only appear in programs that have components.
pub fn lower_to_spill_buffers(module: &mut Module) {
    let sequences = module_sequences(module);
    let simulates = module_simulates(module);
    if !sequences && !simulates {
        return;
    }
    let suspendable = suspendable_functions(module);
    for function in &mut module.functions {
        let spillable = suspendable.contains(&function.name);
        rewrite_function(function, &suspendable, spillable);
    }
    if sequences {
        module.functions.extend(seq_runtime::functions());
    }
    if simulates {
        module.functions.extend(sim_runtime::functions());
    }
}

/// Whether the trampoline drives this module, i.e. whether
/// [`lower_to_spill_buffers`] installed the runtime.
pub fn module_sequences(module: &Module) -> bool {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.ops)
        .any(|spanned| is_sequencing_op(&spanned.op))
}

fn module_simulates(module: &Module) -> bool {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.ops)
        .any(|spanned| is_simulation_op(&spanned.op))
}

fn is_simulation_op(op: &Op) -> bool {
    matches!(
        op,
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
    )
}

fn is_sequencing_op(op: &Op) -> bool {
    is_transfer_op(op)
        || matches!(
            op,
            Op::SeqSystemEnter { .. }
                | Op::SeqSystemExit { .. }
                | Op::SeqObjectCreate { .. }
                | Op::SeqBlockInstance { .. }
        )
}

/// A transfer of control between components: the physical stack must be able
/// to unwind through it, so any frame live across one has to be spillable.
///
/// `seq.object_start` counts because the new component runs attached (§7.1) and
/// may transfer elsewhere before it detaches, and `seq.terminate` because the
/// component it ends never returns to its caller.
pub fn is_transfer_op(op: &Op) -> bool {
    matches!(
        op,
        Op::SeqDetach { .. }
            | Op::SeqCall { .. }
            | Op::SeqResume { .. }
            | Op::SeqObjectStart { .. }
            | Op::SeqTerminate { .. }
            | Op::SimTransferToHead
            | Op::SimTerminateCurrent { .. }
    )
}

/// Names of the functions a transfer can unwind through: those containing a
/// transfer op, and the transitive callers of those.
///
/// Indirect calls are not edges here. The only funcrefs the lowerer builds are
/// component entries (handed to `seq.object_create`, and entered by the
/// trampoline rather than by their creator) and the call-by-name get/set
/// helpers, which cannot suspend.
pub fn suspendable_functions(module: &Module) -> HashSet<String> {
    let mut suspendable: HashSet<String> = module
        .functions
        .iter()
        .filter(|function| function_has_transfer(function))
        .map(|function| function.name.clone())
        .collect();

    let callers = direct_callers(module);
    let mut queue: Vec<String> = suspendable.iter().cloned().collect();
    while let Some(callee) = queue.pop() {
        let Some(callers) = callers.get(&callee) else {
            continue;
        };
        for caller in callers {
            if suspendable.insert(caller.clone()) {
                queue.push(caller.clone());
            }
        }
    }
    suspendable
}

fn function_has_transfer(function: &Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.ops)
        .any(|spanned| is_transfer_op(&spanned.op))
}

/// `callee name -> names that call it directly`.
fn direct_callers(module: &Module) -> HashMap<String, Vec<String>> {
    let defined: HashSet<&str> = module
        .functions
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    let mut callers: HashMap<String, Vec<String>> = HashMap::new();
    for function in &module.functions {
        for spanned in function.blocks.iter().flat_map(|block| &block.ops) {
            let Op::Call { name, .. } = &spanned.op else {
                continue;
            };
            if !defined.contains(name.as_str()) {
                continue;
            }
            let entry = callers.entry(name.clone()).or_default();
            if !entry.iter().any(|caller| caller == &function.name) {
                entry.push(function.name.clone());
            }
        }
    }
    callers
}

/// A spilled frame holds the block to re-enter, then the activation's locals
/// split across two buffers (Phase 4-R2):
///
/// ```text
/// CORO_FRAMES:     resume | scalar locals… | ref_bytes | scalar_bytes
/// CORO_REF_FRAMES: GC-ref locals…
/// ```
///
/// Both frame lengths ride the scalar frame so the ref region can be a
/// host-traced `(array (ref null eq))` on wasm, which has no room for an
/// `i64` word. The scalar buffer stays type-blind `i64`; only interp/native
/// keep the ref region in linear memory too.
const RESUME_SLOT: i64 = 0;
const FIRST_LOCAL_SLOT: i64 = 8;
const SLOT_BYTES: i64 = 8;

/// Whether a local's spilled bits are scalar state or a Simula reference that
/// must eventually live in a host-traced WasmGC slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillKind {
    Scalar,
    GcRef,
}

/// Classify a MIR local type for the Phase 4c spill map.
///
/// [`MirType::ObjectRef`] and, since Workstream 2 (Text/Array\* → WasmGC),
/// `Text`/`ArrayI64`/`ArrayF64`/`ArrayText` all live in the typed ref region:
/// under `gc_objects_enabled()` these are concrete `(ref null $T)` locals, so
/// their spilled bits are a GC reference the collector must be able to trace
/// across a suspension, not a bare `i64` bump handle. They spill/restore
/// through the same [`seq_runtime::SPILL_STORE_REF`]/`SPILL_LOAD_REF` path as
/// `ObjectRef` — wasm's generic `Op::Call` lowering already narrows
/// `SPILL_LOAD_REF`'s `anyref` result back to the destination's own concrete
/// heap type (see `gc_heap_for` in `codegen::wasm`), and passing a concrete
/// ref as a generic `eq`/`any`-typed argument needs no cast (implicit
/// upcast), so no further codegen changes were needed for this flip.
pub fn spill_kind(ty: MirType) -> SpillKind {
    match ty {
        MirType::ObjectRef
        | MirType::Text
        | MirType::ArrayI64
        | MirType::ArrayF64
        | MirType::ArrayText => SpillKind::GcRef,
        _ => SpillKind::Scalar,
    }
}

/// Per-activation spill map: dense scalar slots, then dense GC-ref slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpillLayout {
    pub scalars: Vec<LocalId>,
    pub refs: Vec<LocalId>,
}

impl SpillLayout {
    /// Build the map for every param and local present **before** the asyncify
    /// pass appends unwind temps (those sit above the restored region).
    pub fn from_function(function: &Function) -> Self {
        let mut layout = Self::default();
        let count = function.params.len() + function.locals.len();
        for index in 0..count {
            let id = LocalId(index);
            match spill_kind(function.local(id).ty) {
                SpillKind::Scalar => layout.scalars.push(id),
                SpillKind::GcRef => layout.refs.push(id),
            }
        }
        layout
    }

    pub fn total_slots(&self) -> i64 {
        (self.scalars.len() + self.refs.len()) as i64
    }

    pub fn scalar_offset(index: usize) -> i64 {
        FIRST_LOCAL_SLOT + index as i64 * SLOT_BYTES
    }

    /// Byte offset of ref slot `index` within the **ref** spill buffer
    /// (`CORO_REF_FRAMES`), not the scalar frame.
    pub fn ref_offset(index: usize) -> i64 {
        index as i64 * SLOT_BYTES
    }
}

/// Rebuilds a function's blocks in place. Original block ids are kept, so jumps
/// and branches already in the MIR stay valid and only new blocks are appended.
struct Rewriter {
    blocks: Vec<BasicBlock>,
    locals: Vec<Local>,
    param_count: usize,
    current: usize,
}

impl Rewriter {
    fn temp(&mut self, name: &str, ty: MirType) -> LocalId {
        self.locals.push(Local {
            name: name.to_string(),
            ty,
            class_qual: None,
            debug_scope: None,
        });
        LocalId(self.param_count + self.locals.len() - 1)
    }

    fn constant(&mut self, value: i64, span: &Span) -> LocalId {
        let dest = self.temp("k", MirType::I64);
        self.push(Op::ConstI64 { dest, value }, span);
        dest
    }

    fn block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len());
        self.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            ops: Vec::new(),
        });
        id
    }

    fn at(&mut self, block: BlockId) {
        self.current = block.0;
    }

    fn push(&mut self, op: Op, span: &Span) {
        self.blocks[self.current].ops.push(SpannedOp {
            op,
            span: span.clone(),
        });
    }

    fn current_is_empty(&self) -> bool {
        self.blocks[self.current].ops.is_empty()
    }

    fn current_is_terminated(&self) -> bool {
        self.blocks[self.current]
            .ops
            .last()
            .is_some_and(|spanned| terminates(&spanned.op))
    }

    /// Branches to `target` when `value` is non-zero, and continues in the
    /// fallthrough block.
    fn branch_nonzero(&mut self, value: LocalId, target: BlockId, span: &Span) {
        let zero = self.constant(0, span);
        let cond = self.temp("c", MirType::Bool);
        self.push(
            Op::Compare {
                dest: cond,
                op: CmpOp::Ne,
                left: value,
                right: zero,
            },
            span,
        );
        let otherwise = self.block();
        self.push(
            Op::Branch {
                cond,
                then_block: target,
                else_block: otherwise,
            },
            span,
        );
        self.at(otherwise);
    }
}

fn terminates(op: &Op) -> bool {
    matches!(
        op,
        Op::Jump { .. }
            | Op::GotoEscape { .. }
            | Op::Branch { .. }
            | Op::Return { .. }
            | Op::Abort { .. }
    )
}

/// Whether an op can unwind the physical stack: a transfer, or a call to a
/// function that can reach one.
fn suspends(op: &Op, suspendable: &HashSet<String>) -> bool {
    match op {
        Op::Call { name, .. } => suspendable.contains(name),
        other => is_transfer_op(other),
    }
}

fn homeable(ty: MirType) -> bool {
    matches!(
        ty,
        MirType::I64 | MirType::Bool | MirType::F64 | MirType::LongF64
    )
}

/// Locals whose address is taken. Their true value has to live in a heap cell
/// that survives re-entry: the wasm backend's per-call home slab is allocated
/// again on every rewind, which would invalidate every pointer a component
/// captured with [`Op::LocalAddr`].
fn addr_taken_locals(function: &Function) -> Vec<LocalId> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for spanned in function.blocks.iter().flat_map(|block| &block.ops) {
        let Op::LocalAddr { local, .. } = &spanned.op else {
            continue;
        };
        if !homeable(function.local(*local).ty) {
            continue;
        }
        if seen.insert(*local) {
            out.push(*local);
        }
    }
    out
}

fn written_local(op: &Op) -> Option<LocalId> {
    match op {
        Op::StoreLocal { local, .. } => Some(*local),
        Op::ConstI64 { dest, .. }
        | Op::ConstF64 { dest, .. }
        | Op::ConstBool { dest, .. }
        | Op::I64ToF64 { dest, .. }
        | Op::F64ToI64 { dest, .. }
        | Op::Copy { dest, .. }
        | Op::Binary { dest, .. }
        | Op::Unary { dest, .. }
        | Op::Compare { dest, .. }
        | Op::LocalAddr { dest, .. }
        | Op::FieldAddr { dest, .. }
        | Op::LoadRefI64 { dest, .. }
        | Op::FuncAddr { dest, .. }
        | Op::StackAlloc { dest, .. }
        | Op::HeapAlloc { dest, .. }
        | Op::AllocArray { dest, .. }
        | Op::ArrayLoad { dest, .. }
        | Op::NewObject { dest, .. }
        | Op::FieldLoadI64 { dest, .. }
        | Op::ObjectIsNone { dest, .. }
        | Op::ObjectClassIdSafe { dest, .. }
        | Op::TextNotext { dest, .. }
        | Op::TextFromLiteral { dest, .. }
        | Op::TextCopy { dest, .. }
        | Op::TextConcat { dest, .. }
        | Op::TextContentEq { dest, .. }
        | Op::TextContentCmp { dest, .. }
        | Op::TextLength { dest, .. }
        | Op::TextConstant { dest, .. }
        | Op::TextStart { dest, .. }
        | Op::TextMain { dest, .. }
        | Op::TextPos { dest, .. }
        | Op::TextMore { dest, .. }
        | Op::SeqSystemEnter { dest, .. }
        | Op::SeqObjectCreate { dest, .. } => Some(*dest),
        Op::Call { dest, .. } | Op::CallIndirect { dest, .. } => *dest,
        Op::TextAssign { dest, .. } | Op::TextRefAssign { dest, .. } => Some(*dest),
        _ => None,
    }
}

/// Give every address-taken local a heap cell whose pointer is itself a local.
/// The pointer is spilled and restored with the rest of the frame, so captures
/// keep working across suspension; the cell is allocated only on cold entry.
fn install_stable_homes(function: &mut Function) -> HashMap<LocalId, LocalId> {
    let taken = addr_taken_locals(function);
    if taken.is_empty() {
        return HashMap::new();
    }

    let mut homes = HashMap::new();
    for local in &taken {
        let ptr = LocalId(function.params.len() + function.locals.len());
        function.locals.push(Local {
            name: format!("__home_{}", local.0),
            ty: MirType::RefI64,
            class_qual: None,
            debug_scope: None,
        });
        homes.insert(*local, ptr);
    }

    let span: Span = 0..0;
    let mut preface = Vec::new();
    for local in &taken {
        let ptr = homes[local];
        preface.push(SpannedOp {
            op: Op::StackAlloc {
                dest: ptr,
                bytes: 8,
            },
            span: span.clone(),
        });
        preface.push(SpannedOp {
            op: Op::StoreRefI64 {
                ptr,
                src: *local,
                offset: 0,
            },
            span: span.clone(),
        });
    }
    function.blocks[function.entry.0].ops.splice(0..0, preface);

    for block in &mut function.blocks {
        let ops = std::mem::take(&mut block.ops);
        let mut rewritten = Vec::with_capacity(ops.len() * 2);
        for spanned in ops {
            let span = spanned.span.clone();
            match spanned.op {
                Op::LocalAddr { dest, local } if homes.contains_key(&local) => {
                    rewritten.push(SpannedOp {
                        op: Op::Copy {
                            dest,
                            src: homes[&local],
                        },
                        span,
                    });
                }
                op => {
                    let written = written_local(&op);
                    rewritten.push(SpannedOp {
                        op,
                        span: span.clone(),
                    });
                    if let Some(local) = written
                        && let Some(&ptr) = homes.get(&local)
                    {
                        rewritten.push(SpannedOp {
                            op: Op::StoreRefI64 {
                                ptr,
                                src: local,
                                offset: 0,
                            },
                            span,
                        });
                    }
                }
            }
        }
        block.ops = rewritten;
    }
    homes
}

fn emit_reload_homes(rewriter: &mut Rewriter, homes: &HashMap<LocalId, LocalId>, span: &Span) {
    let mut pairs: Vec<_> = homes.iter().map(|(local, ptr)| (*local, *ptr)).collect();
    pairs.sort_by_key(|(local, _)| local.0);
    for (local, ptr) in pairs {
        rewriter.push(
            Op::LoadRefI64 {
                dest: local,
                ptr,
                offset: 0,
            },
            span,
        );
    }
}

fn rewrite_function(function: &mut Function, suspendable: &HashSet<String>, spillable: bool) {
    let touches_sequencing = function
        .blocks
        .iter()
        .flat_map(|block| &block.ops)
        .any(|spanned| is_sequencing_op(&spanned.op));
    let touches_simulation = function
        .blocks
        .iter()
        .flat_map(|block| &block.ops)
        .any(|spanned| is_simulation_op(&spanned.op));
    if !spillable && !touches_sequencing && !touches_simulation {
        return;
    }

    // Stable homes must exist before we count spill slots: their pointers are
    // part of the frame that has to come back with the activation.
    let homes = if spillable {
        install_stable_homes(function)
    } else {
        HashMap::new()
    };

    // Classify params/locals into scalar vs GC-ref regions before the rewriter
    // appends unwind temps (those sit above the restored slots).
    let spill = SpillLayout::from_function(function);
    let mut rewriter = Rewriter {
        blocks: std::mem::take(&mut function.blocks),
        locals: std::mem::take(&mut function.locals),
        param_count: function.params.len(),
        current: 0,
    };
    let original_blocks = rewriter.blocks.len();
    let mut resume_points: Vec<BlockId> = Vec::new();

    for index in 0..original_blocks {
        let ops = std::mem::take(&mut rewriter.blocks[index].ops);
        rewriter.at(BlockId(index));
        for spanned in ops {
            let span = spanned.span.clone();
            if !(spillable && suspends(&spanned.op, suspendable)) {
                let op = sequencing_call(&mut rewriter, spanned.op, &span);
                rewriter.push(op, &span);
                continue;
            }

            // The suspend point has to start a block, so that rewinding can
            // land either on it or just past it without replaying anything.
            let at_op = if rewriter.current_is_empty() {
                BlockId(rewriter.current)
            } else {
                let head = rewriter.block();
                rewriter.push(Op::Jump { target: head }, &span);
                rewriter.at(head);
                head
            };

            // A transfer has happened by the time control comes back, so it
            // resumes past itself; a call has to be re-entered, because the
            // frames it is holding are the ones being restored.
            let replay = matches!(spanned.op, Op::Call { .. });
            let op = sequencing_call(&mut rewriter, spanned.op, &span);
            rewriter.push(op, &span);

            let unwinding = rewriter.temp("unwinding", MirType::I64);
            rewriter.push(
                Op::Call {
                    dest: Some(unwinding),
                    name: seq_runtime::PHASE_IS_UNWINDING.to_string(),
                    args: Vec::new(),
                },
                &span,
            );
            let spill_block = rewriter.block();
            rewriter.branch_nonzero(unwinding, spill_block, &span);
            let after = BlockId(rewriter.current);

            let resume = if replay { at_op } else { after };
            resume_points.push(resume);
            rewriter.at(spill_block);
            emit_spill(&mut rewriter, resume, &spill, &span);
            rewriter.at(after);
            // A component may have written through our LocalAddr captures while
            // we were suspended; the spilled local values are stale.
            emit_reload_homes(&mut rewriter, &homes, &span);
        }

        // The lowerer leaves a block that runs off its end falling through to
        // the next one. Splitting moves that end elsewhere, so say it outright.
        if !rewriter.current_is_terminated() {
            let span = 0..0;
            if index + 1 < original_blocks {
                rewriter.push(
                    Op::Jump {
                        target: BlockId(index + 1),
                    },
                    &span,
                );
            } else {
                rewriter.push(Op::Return { value: None }, &span);
            }
        }
    }

    if !resume_points.is_empty() {
        let entry = emit_rewind_prologue(
            &mut rewriter,
            function.entry,
            &resume_points,
            &spill,
            &homes,
        );
        function.entry = entry;
    }

    function.blocks = rewriter.blocks;
    function.locals = rewriter.locals;
}

/// Hands this activation's locals to the runtime and returns, letting the
/// unwind carry on into the caller.
fn emit_spill(rewriter: &mut Rewriter, resume: BlockId, spill: &SpillLayout, span: &Span) {
    // Address-taken cells are the source of truth across a suspension: other
    // components may have updated them through captures, so the local must not
    // be written back into the home on the way out.
    let resume_id = rewriter.constant(resume.0 as i64, span);
    let scalar_slots = rewriter.constant(spill.scalars.len() as i64, span);
    let ref_slots = rewriter.constant(spill.refs.len() as i64, span);
    let frame = rewriter.temp("frame", MirType::I64);
    rewriter.push(
        Op::Call {
            dest: Some(frame),
            name: seq_runtime::FRAME_PUSH.to_string(),
            args: vec![resume_id, scalar_slots, ref_slots],
        },
        span,
    );
    for (index, local) in spill.scalars.iter().enumerate() {
        rewriter.push(
            Op::FieldStoreI64 {
                object: frame,
                offset: SpillLayout::scalar_offset(index),
                value: *local,
                class_qual: None,
            },
            span,
        );
    }
    for (index, local) in spill.refs.iter().enumerate() {
        // Route GC-ref candidates through the seq_runtime helper so wasm can
        // later retarget this call to a WasmGC array.set (Phase 4b/4c).
        let index_local = rewriter.constant(index as i64, span);
        rewriter.push(
            Op::Call {
                dest: None,
                name: seq_runtime::SPILL_STORE_REF.to_string(),
                args: vec![frame, scalar_slots, index_local, *local],
            },
            span,
        );
    }
    rewriter.push(Op::Return { value: None }, span);
}

/// The new entry: on a rewind, take this activation's frame back and jump to
/// where it left off; otherwise start the function normally.
fn emit_rewind_prologue(
    rewriter: &mut Rewriter,
    entry: BlockId,
    resume_points: &[BlockId],
    spill: &SpillLayout,
    homes: &HashMap<LocalId, LocalId>,
) -> BlockId {
    let span = 0..0;
    let prologue = rewriter.block();
    let rewind = rewriter.block();

    rewriter.at(prologue);
    let rewinding = rewriter.temp("rewinding", MirType::I64);
    rewriter.push(
        Op::Call {
            dest: Some(rewinding),
            name: seq_runtime::PHASE_IS_REWINDING.to_string(),
            args: Vec::new(),
        },
        &span,
    );
    let zero = rewriter.constant(0, &span);
    let cond = rewriter.temp("c", MirType::Bool);
    rewriter.push(
        Op::Compare {
            dest: cond,
            op: CmpOp::Ne,
            left: rewinding,
            right: zero,
        },
        &span,
    );
    rewriter.push(
        Op::Branch {
            cond,
            then_block: rewind,
            else_block: entry,
        },
        &span,
    );

    rewriter.at(rewind);
    let frame = rewriter.temp("frame", MirType::I64);
    rewriter.push(
        Op::Call {
            dest: Some(frame),
            name: seq_runtime::FRAME_POP.to_string(),
            args: Vec::new(),
        },
        &span,
    );
    let resume = rewriter.temp("resume", MirType::I64);
    rewriter.push(
        Op::FieldLoadI64 {
            dest: resume,
            object: frame,
            offset: RESUME_SLOT,
            class_qual: None,
        },
        &span,
    );
    // Safe to write the locals back before dispatching: every temporary this
    // pass introduces sits above the slots being restored.
    for (index, local) in spill.scalars.iter().enumerate() {
        rewriter.push(
            Op::FieldLoadI64 {
                dest: *local,
                object: frame,
                offset: SpillLayout::scalar_offset(index),
                class_qual: None,
            },
            &span,
        );
    }
    let scalar_slots = rewriter.constant(spill.scalars.len() as i64, &span);
    for (index, local) in spill.refs.iter().enumerate() {
        let index_local = rewriter.constant(index as i64, &span);
        rewriter.push(
            Op::Call {
                dest: Some(*local),
                name: seq_runtime::SPILL_LOAD_REF.to_string(),
                args: vec![frame, scalar_slots, index_local],
            },
            &span,
        );
    }
    // Spilled values of address-taken locals are from before suspension; the
    // heap cells they point at may have been updated through captures.
    emit_reload_homes(rewriter, homes, &span);

    let mut seen = HashSet::new();
    for target in resume_points {
        if !seen.insert(*target) {
            continue;
        }
        let wanted = rewriter.constant(target.0 as i64, &span);
        let matches = rewriter.temp("c", MirType::Bool);
        rewriter.push(
            Op::Compare {
                dest: matches,
                op: CmpOp::Eq,
                left: resume,
                right: wanted,
            },
            &span,
        );
        let otherwise = rewriter.block();
        rewriter.push(
            Op::Branch {
                cond: matches,
                then_block: *target,
                else_block: otherwise,
            },
            &span,
        );
        rewriter.at(otherwise);
    }
    rewriter.push(
        Op::Abort {
            message: "sim: rewound a component into a suspension point it never had".to_string(),
        },
        &span,
    );
    prologue
}

/// Maps a sequencing op onto its runtime call, materializing the compile-time
/// block ids it names. Anything else passes through untouched.
fn sequencing_call(rewriter: &mut Rewriter, op: Op, span: &Span) -> Op {
    let (dest, name, args) = match op {
        Op::SeqSystemEnter { dest, block } => {
            let block = rewriter.constant(block, span);
            (Some(dest), seq_runtime::SEQ_SYSTEM_ENTER, vec![block])
        }
        Op::SeqSystemExit { system } => (None, seq_runtime::SEQ_SYSTEM_EXIT, vec![system]),
        Op::SeqObjectCreate {
            dest,
            declaring_block,
            entry,
            object,
        } => {
            let block = rewriter.constant(declaring_block, span);
            (
                Some(dest),
                seq_runtime::SEQ_OBJECT_CREATE,
                vec![block, entry, object],
            )
        }
        Op::SeqObjectStart { component } => (None, seq_runtime::SEQ_OBJECT_START, vec![component]),
        Op::SeqBlockInstance { object } => (None, seq_runtime::SEQ_BLOCK_INSTANCE, vec![object]),
        Op::SeqDetach { object } => (None, seq_runtime::SEQ_DETACH, vec![object]),
        Op::SeqCall { object } => (None, seq_runtime::SEQ_CALL, vec![object]),
        Op::SeqResume { object } => (None, seq_runtime::SEQ_RESUME, vec![object]),
        Op::SeqTerminate { object } => (None, seq_runtime::SEQ_TERMINATE, vec![object]),
        Op::SimBegin => (None, sim_runtime::SIM_BEGIN, vec![]),
        Op::SimEnd => (None, sim_runtime::SIM_END, vec![]),
        Op::SimHold { dt } => (None, sim_runtime::SIM_HOLD, vec![dt]),
        Op::SimActivateDirect { process } => {
            (None, sim_runtime::SIM_ACTIVATE_DIRECT, vec![process])
        }
        Op::SimActivateTimed {
            process,
            t,
            mode,
            prior,
            reac,
        } => {
            let mode = rewriter.constant(mode, span);
            let prior = rewriter.constant(i64::from(prior), span);
            let reac = rewriter.constant(i64::from(reac), span);
            (
                None,
                sim_runtime::SIM_ACTIVATE_TIMED,
                vec![process, t, mode, prior, reac],
            )
        }
        Op::SimActivateRelative {
            process,
            other,
            before,
        } => {
            let before = rewriter.constant(i64::from(before), span);
            (
                None,
                sim_runtime::SIM_ACTIVATE_RELATIVE,
                vec![process, other, before],
            )
        }
        Op::SimPassivate => (None, sim_runtime::SIM_PASSIVATE, vec![]),
        Op::SimTransferToHead => (None, sim_runtime::SIM_TRANSFER_TO_HEAD, vec![]),
        Op::SimTerminateCurrent { process } => {
            (None, sim_runtime::SIM_TERMINATE_CURRENT, vec![process])
        }
        Op::SimCancel { process } => (None, sim_runtime::SIM_CANCEL, vec![process]),
        Op::SimFinishMain => (None, sim_runtime::SIM_FINISH_MAIN, vec![]),
        Op::SimTime { dest } => (Some(dest), sim_runtime::SIM_TIME, vec![]),
        Op::SimIsMainCurrent { dest } => (Some(dest), sim_runtime::SIM_IS_MAIN_CURRENT, vec![]),
        Op::SimHasCurrent { dest } => (Some(dest), sim_runtime::SIM_HAS_CURRENT, vec![]),
        Op::SimCurrent { dest } => (Some(dest), sim_runtime::SIM_CURRENT, vec![]),
        Op::SimMain { dest } => (Some(dest), sim_runtime::SIM_MAIN, vec![]),
        Op::SimIdle { dest, process } => (Some(dest), sim_runtime::SIM_IDLE, vec![process]),
        Op::SimTerminated { dest, process } => {
            (Some(dest), sim_runtime::SIM_TERMINATED, vec![process])
        }
        Op::SimEvtime { dest, process } => (Some(dest), sim_runtime::SIM_EVTIME, vec![process]),
        Op::SimNextev { dest, process } => (Some(dest), sim_runtime::SIM_NEXTEV, vec![process]),
        other => return other,
    };
    Op::Call {
        dest,
        name: name.to_string(),
        args,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BasicBlock, BlockId, Function, LocalId, SpannedOp};

    fn func(name: &str, ops: Vec<Op>) -> Function {
        Function {
            name: name.to_string(),
            params: Vec::new(),
            locals: Vec::new(),
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                ops: ops
                    .into_iter()
                    .map(|op| SpannedOp { op, span: 0..0 })
                    .collect(),
            }],
            labels: Default::default(),
            result: None,
            array_elem_kinds: std::collections::HashMap::new(),
            foreign: None,
            export: None,
            debug_scopes: Vec::new(),
        }
    }

    fn call(name: &str) -> Op {
        Op::Call {
            dest: None,
            name: name.to_string(),
            args: Vec::new(),
        }
    }

    fn module(functions: Vec<Function>) -> Module {
        Module {
            functions,
            ..Module::default()
        }
    }

    #[test]
    fn a_function_that_detaches_can_suspend() {
        let module = module(vec![func(
            "C$__coro",
            vec![Op::SeqDetach { object: LocalId(0) }],
        )]);
        assert_eq!(
            suspendable_functions(&module),
            HashSet::from(["C$__coro".to_string()])
        );
    }

    #[test]
    fn a_function_with_no_transfer_stays_out_of_the_set() {
        let module = module(vec![func(
            "helper",
            vec![Op::ConstI64 {
                dest: LocalId(0),
                value: 1,
            }],
        )]);
        assert!(suspendable_functions(&module).is_empty());
    }

    /// A `detach` inside a procedure called from a class body suspends the body
    /// too, so both frames have to be spillable.
    #[test]
    fn suspendability_propagates_to_callers_transitively() {
        let module = module(vec![
            func("C$__coro", vec![call("outer")]),
            func("outer", vec![call("inner")]),
            func("inner", vec![Op::SeqResume { object: LocalId(0) }]),
            func("unrelated", vec![call("leaf")]),
            func(
                "leaf",
                vec![Op::ConstI64 {
                    dest: LocalId(0),
                    value: 1,
                }],
            ),
        ]);
        let suspendable = suspendable_functions(&module);
        assert_eq!(
            suspendable,
            HashSet::from([
                "C$__coro".to_string(),
                "outer".to_string(),
                "inner".to_string(),
            ])
        );
    }

    /// Mutual recursion must not spin the fixpoint forever.
    #[test]
    fn recursive_calls_reach_a_fixpoint() {
        let module = module(vec![
            func("ping", vec![call("pong")]),
            func(
                "pong",
                vec![call("ping"), Op::SeqDetach { object: LocalId(0) }],
            ),
        ]);
        assert_eq!(
            suspendable_functions(&module),
            HashSet::from(["ping".to_string(), "pong".to_string()])
        );
    }

    /// Calls to runtime helpers that no MIR function defines are not edges.
    #[test]
    fn calls_to_undefined_names_are_not_call_graph_edges() {
        let module = module(vec![func("main", vec![call("simrt_something")])]);
        assert!(suspendable_functions(&module).is_empty());
    }

    fn with_locals(name: &str, count: usize, ops: Vec<Op>) -> Function {
        let mut function = func(name, ops);
        function.locals = (0..count)
            .map(|index| Local {
                name: format!("l{index}"),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            })
            .collect();
        function
    }

    fn calls_to(function: &Function, name: &str) -> usize {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.ops)
            .filter(
                |spanned| matches!(&spanned.op, Op::Call { name: called, .. } if called == name),
            )
            .count()
    }

    fn find<'a>(module: &'a Module, name: &str) -> &'a Function {
        module
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap_or_else(|| panic!("{name} is missing"))
    }

    /// The block a spill records as its resume point.
    fn resume_point_of_spill(function: &Function) -> BlockId {
        for block in &function.blocks {
            let mut previous: Option<i64> = None;
            for spanned in &block.ops {
                match &spanned.op {
                    Op::ConstI64 { value, .. } => previous = Some(*value),
                    Op::Call { name, .. } if name == seq_runtime::FRAME_PUSH => {
                        // `frame_push(resume, slots)`: the resume constant is
                        // the first of the two materialized just above.
                        let ops: Vec<i64> = block
                            .ops
                            .iter()
                            .filter_map(|spanned| match &spanned.op {
                                Op::ConstI64 { value, .. } => Some(*value),
                                _ => None,
                            })
                            .collect();
                        let _ = previous;
                        return BlockId(ops[0] as usize);
                    }
                    _ => {}
                }
            }
        }
        panic!("no spill found");
    }

    #[test]
    fn a_module_without_sequencing_is_left_alone() {
        let mut module = module(vec![func(
            "main",
            vec![Op::ConstI64 {
                dest: LocalId(0),
                value: 1,
            }],
        )]);
        let before = module.functions.len();
        lower_to_spill_buffers(&mut module);
        assert_eq!(module.functions.len(), before);
        assert!(!module_sequences(&module));
    }

    #[test]
    fn sequencing_ops_become_runtime_calls() {
        let mut module = module(vec![with_locals(
            "C$__coro",
            1,
            vec![Op::SeqDetach { object: LocalId(0) }],
        )]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        assert_eq!(calls_to(coro, seq_runtime::SEQ_DETACH), 1);
        assert!(
            !coro
                .blocks
                .iter()
                .flat_map(|block| &block.ops)
                .any(|spanned| is_sequencing_op(&spanned.op))
        );
    }

    #[test]
    fn the_runtime_and_its_trampoline_come_with_the_transform() {
        let mut module = module(vec![with_locals(
            "main",
            1,
            vec![Op::SeqDetach { object: LocalId(0) }],
        )]);
        lower_to_spill_buffers(&mut module);
        for name in seq_runtime::function_names() {
            find(&module, &name);
        }
    }

    /// Rewinding has to re-enter the function, so the entry becomes the
    /// prologue that asks whether that is what is happening.
    #[test]
    fn a_spillable_function_gets_a_rewind_prologue() {
        let mut module = module(vec![with_locals(
            "C$__coro",
            1,
            vec![Op::SeqDetach { object: LocalId(0) }],
        )]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        assert_ne!(coro.entry, BlockId(0));
        let prologue = coro.block(coro.entry);
        assert!(matches!(
            &prologue.ops[0].op,
            Op::Call { name, .. } if name == seq_runtime::PHASE_IS_REWINDING
        ));
        assert_eq!(calls_to(coro, seq_runtime::FRAME_POP), 1);
    }

    /// Every local is handed to the runtime and taken back, so a value living
    /// across a transfer survives it. All-scalar frames keep LocalId order in
    /// the scalar region (offsets 8, 16, 24 for three I64 locals).
    #[test]
    fn all_of_the_activations_locals_are_spilled_and_restored() {
        let mut module = module(vec![with_locals(
            "C$__coro",
            3,
            vec![Op::SeqDetach { object: LocalId(0) }],
        )]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        let stored: Vec<i64> = coro
            .blocks
            .iter()
            .flat_map(|block| &block.ops)
            .filter_map(|spanned| match &spanned.op {
                Op::FieldStoreI64 { offset, .. } => Some(*offset),
                _ => None,
            })
            .collect();
        let loaded: Vec<i64> = coro
            .blocks
            .iter()
            .flat_map(|block| &block.ops)
            .filter_map(|spanned| match &spanned.op {
                Op::FieldLoadI64 { offset, .. } => Some(*offset),
                _ => None,
            })
            .collect();
        assert_eq!(stored, vec![8, 16, 24]);
        // The resume point at offset 0, then the same three slots.
        assert_eq!(loaded, vec![0, 8, 16, 24]);
    }

    /// Phase 4c: ObjectRef locals pack after scalars so the ref region can
    /// become a WasmGC spine without reshuffling scalar offsets. Since
    /// Workstream 2, Text/array handles also lower to WasmGC and pack into
    /// the same ref region as ObjectRef. Ref-typed locals store through
    /// [`seq_runtime::SPILL_STORE_REF`].
    #[test]
    fn gc_ref_locals_spill_after_scalars() {
        let mut function = func("C$__coro", vec![Op::SeqDetach { object: LocalId(1) }]);
        function.locals = vec![
            Local {
                name: "n".into(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "obj".into(),
                ty: MirType::ObjectRef,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "t".into(),
                ty: MirType::Text,
                class_qual: None,
                debug_scope: None,
            },
            Local {
                name: "m".into(),
                ty: MirType::I64,
                class_qual: None,
                debug_scope: None,
            },
        ];
        let layout = SpillLayout::from_function(&function);
        assert_eq!(layout.scalars, vec![LocalId(0), LocalId(3)]);
        assert_eq!(layout.refs, vec![LocalId(1), LocalId(2)]);
        assert_eq!(SpillLayout::scalar_offset(0), 8);
        assert_eq!(SpillLayout::scalar_offset(1), 16);
        assert_eq!(SpillLayout::ref_offset(0), 0);
        assert_eq!(SpillLayout::ref_offset(1), 8);

        let mut module = module(vec![function]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        let scalar_stores: Vec<(i64, LocalId)> = coro
            .blocks
            .iter()
            .flat_map(|block| &block.ops)
            .filter_map(|spanned| match &spanned.op {
                Op::FieldStoreI64 { offset, value, .. } => Some((*offset, *value)),
                _ => None,
            })
            .collect();
        assert_eq!(
            scalar_stores,
            vec![(8, LocalId(0)), (16, LocalId(3))],
            "plain scalars use FieldStoreI64"
        );
        let ref_stores: Vec<&[LocalId]> = coro
            .blocks
            .iter()
            .flat_map(|block| &block.ops)
            .filter_map(|spanned| match &spanned.op {
                Op::Call { name, args, .. } if name == seq_runtime::SPILL_STORE_REF => {
                    Some(args.as_slice())
                }
                _ => None,
            })
            .collect();
        assert_eq!(ref_stores.len(), 2, "obj and t both spill as GC refs");
        // args: frame, scalar_slots, index, value
        assert_eq!(ref_stores[0][3], LocalId(1));
        assert_eq!(ref_stores[1][3], LocalId(2));
        assert_eq!(
            calls_to(coro, seq_runtime::SPILL_LOAD_REF),
            2,
            "rewind restores obj/t via SPILL_LOAD_REF"
        );
    }

    #[test]
    fn spill_kind_classifies_simula_refs() {
        assert_eq!(spill_kind(MirType::I64), SpillKind::Scalar);
        assert_eq!(spill_kind(MirType::RefI64), SpillKind::Scalar);
        assert_eq!(spill_kind(MirType::ObjectRef), SpillKind::GcRef);
        // Workstream 2: Text/Array* now lower to WasmGC refs too, so they
        // spill through the same GC-ref region as ObjectRef.
        assert_eq!(spill_kind(MirType::Text), SpillKind::GcRef);
        assert_eq!(spill_kind(MirType::ArrayI64), SpillKind::GcRef);
        assert_eq!(spill_kind(MirType::ArrayF64), SpillKind::GcRef);
        assert_eq!(spill_kind(MirType::ArrayText), SpillKind::GcRef);
    }

    /// A transfer has already happened when control comes back, so replaying it
    /// would detach twice.
    #[test]
    fn a_transfer_resumes_past_itself() {
        let mut module = module(vec![with_locals(
            "C$__coro",
            1,
            vec![
                Op::SeqDetach { object: LocalId(0) },
                Op::ConstI64 {
                    dest: LocalId(1),
                    value: 7,
                },
            ],
        )]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        let resume = resume_point_of_spill(coro);
        let ops = &coro.block(resume).ops;
        assert!(matches!(&ops[0].op, Op::ConstI64 { value: 7, .. }));
    }

    /// A call is re-entered instead, because the frames being restored are the
    /// ones it is holding.
    #[test]
    fn a_call_that_can_suspend_resumes_at_the_call() {
        let mut module = module(vec![
            with_locals(
                "C$__coro",
                1,
                vec![
                    call("p"),
                    Op::ConstI64 {
                        dest: LocalId(1),
                        value: 7,
                    },
                ],
            ),
            with_locals("p", 1, vec![Op::SeqDetach { object: LocalId(0) }]),
        ]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        let resume = resume_point_of_spill(coro);
        assert!(matches!(
            &coro.block(resume).ops[0].op,
            Op::Call { name, .. } if name == "p"
        ));
    }

    /// Splitting moves a block's tail elsewhere, so the fallthrough the lowerer
    /// relied on has to be written down before it stops being true.
    #[test]
    fn splitting_makes_fallthrough_explicit() {
        let mut function = with_locals("C$__coro", 1, vec![Op::SeqDetach { object: LocalId(0) }]);
        function.blocks.push(BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            ops: vec![SpannedOp {
                op: Op::Return { value: None },
                span: 0..0,
            }],
        });
        let mut module = module(vec![function]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        assert!(
            coro.blocks
                .iter()
                .flat_map(|block| &block.ops)
                .any(|spanned| matches!(spanned.op, Op::Jump { target: BlockId(1) }))
        );
    }

    /// The runtime implements suspension rather than undergoing it: giving it a
    /// prologue would have the trampoline rewinding its own machinery.
    #[test]
    fn the_runtime_is_not_itself_transformed() {
        let mut module = module(vec![with_locals(
            "main",
            1,
            vec![Op::SeqDetach { object: LocalId(0) }],
        )]);
        lower_to_spill_buffers(&mut module);
        for name in seq_runtime::function_names() {
            let function = find(&module, &name);
            assert_eq!(
                calls_to(function, seq_runtime::PHASE_IS_REWINDING),
                0,
                "{name} was transformed"
            );
        }
    }

    /// A component may update an enclosing local through a by-reference capture
    /// while we are suspended. The home cell holds that update; syncing the stale
    /// local back into the home on the way out would wipe it.
    #[test]
    fn a_by_ref_capture_update_survives_the_callers_next_suspend() {
        let src = r#"begin
            integer n;
            class C;
            begin
                n := n + 1;
                detach;
            end;
            ref(C) a, b;
            a :- new C;
            b :- new C;
            call(a);
            call(b);
            OutInt(n, 0); OutImage;
        end;"#;
        let program = crate::parse::test_support::parse_program(src);
        let mut module = crate::mir::lower_program_with_source(&program, src).unwrap();
        lower_to_spill_buffers(&mut module);
        let main = find(&module, "main");
        assert!(
            main.blocks
                .iter()
                .flat_map(|block| &block.ops)
                .any(|spanned| matches!(&spanned.op, Op::LoadRefI64 { .. })),
            "expected reloads from stable homes"
        );
    }

    /// Every block the pass appends ends deliberately; a block that runs off
    /// its end would fall into whichever block happens to follow it.
    #[test]
    fn every_block_the_pass_writes_is_terminated() {
        let mut module = module(vec![with_locals(
            "C$__coro",
            2,
            vec![
                Op::SeqDetach { object: LocalId(0) },
                Op::SeqDetach { object: LocalId(0) },
            ],
        )]);
        lower_to_spill_buffers(&mut module);
        let coro = find(&module, "C$__coro");
        for block in &coro.blocks {
            let last = block.ops.last().expect("an empty block goes nowhere");
            assert!(
                terminates(&last.op),
                "{} runs off its end: {:?}",
                block.id,
                last.op
            );
        }
    }
}
