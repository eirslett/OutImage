//! SIMSET / chapter 7 sequencing / Simulation ops for the MIR interpreter
//! (Phase 6).
//!
//! **SIMSET** (`Simset*` ops) is plain pointer surgery on the `SUC`/`PRED`
//! fields every `Head`/`Link`/`Process` object carries at fixed offsets
//! (`crate::layout::SIMSET_SUC_OFFSET` / `SIMSET_PRED_OFFSET`) — a direct
//! translation of `runtime/runtime.c`'s `simrt_simset_*` family.
//!
//! **Chapter 7 sequencing** (`Seq*` ops) is the interesting part: native code
//! gives every component (an object generated from a class that can suspend)
//! its own OS stack and switches between them with `runtime/coro.c`. This
//! interpreter has no such stacks to switch — instead, each component's
//! "stack" is just a `Vec<CallFrame>`, and a chapter 7 transfer is nothing
//! more than swapping which `Vec<CallFrame>` lives in [`Vm::frames`]. Because
//! the interpreter already represents a call stack as an explicit vector
//! rather than the host's own native stack, this is a *simpler* and exact
//! mechanical translation of `runtime/sequencing.c`'s
//! `simrt_component`/`simrt_system` model:
//!
//! | native (`sequencing.c`)        | interpreter (`seq_ops.rs`)              |
//! |---------------------------------|------------------------------------------|
//! | `simrt_coro *`                 | [`SeqTarget`] (`Outermost` or `Component(id)`) |
//! | coroutine's own machine stack    | `Vec<CallFrame>` parked in the target's storage slot |
//! | `simrt_component::park`        | `SeqComponent::park: Option<SeqTarget>` |
//! | `simrt_component::attached_to` | `SeqComponent::attached_to: Option<SeqTarget>` |
//! | `simrt_system::main_park`      | `SeqSystemState::main_park: SeqTarget`  |
//! | `simrt_coro_switch(cur, tgt)`| park `self.frames` at `self.active_target`, load `tgt`'s stack into `self.frames`, set `self.active_target = tgt` |
//!
//! **Simulation** (`Sim*` ops) is, per `runtime/runtime.c`, an event-notice
//! array (the SQS) that is *independent* of SIMSET — scheduling reorders a
//! plain `Vec<SimEventNotice>`, and only `SimTransferToHead` /
//! `SimTerminateCurrent` reach into the chapter 7 primitives above (every
//! Process is an ordinary component; `hold`/`activate` are chapter 12
//! bookkeeping around the very same `SeqDetach`/`SeqResume`/`SeqTerminate`
//! transfers §7.3 already defines).
//!
//! **LocalAddr across stack switches:** `RefTarget::Local` records the
//! [`SeqTarget`] that owned the frame when the address was taken, so
//! load/store through a by-reference (`name`) binding still hits the right
//! parked/`Vm::frames` vector after detach/call/resume (see
//! `name_param_local_addr_survives_call_detach`). Native codegen's
//! `reload_addr_taken_locals` is a separate SSA/stack-home concern and is
//! not needed here — interpreter locals live directly on `CallFrame`.

use crate::error::CompileError;
use crate::layout::{SIMSET_PRED_OFFSET, SIMSET_SUC_OFFSET};
use crate::mir::{LocalId, Op};
use crate::simulation::MAX_SQS_LENGTH;

use super::{CallFrame, ExecResult, SlotTag, Value, Vm, expect_f64};

/// Sentinel `class_id` for the Simulation `MAIN` pseudo-object (`Op::SimBegin`);
/// never equals a real compiler-assigned class id.
const SIM_MAIN_CLASS_ID: i64 = i64::MIN;

/// Which parked stack (or the still-running outermost program) a chapter 7
/// reactivation point currently names — the interpreter's stand-in for a
/// native `simrt_coro *`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeqTarget {
    /// The top-level `main` call stack (never itself a [`SeqComponent`]).
    Outermost,
    Component(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeqState {
    Attached,
    Detached,
    Resumed,
    Terminated,
}

fn seq_state_english(state: SeqState) -> &'static str {
    match state {
        SeqState::Attached => "attached",
        SeqState::Detached => "detached",
        SeqState::Resumed => "resumed",
        SeqState::Terminated => "terminated",
    }
}

/// Chapter 7 sequencing mistakes are user-facing runtime errors, not ICEs.
fn seq_runtime_error(message: impl Into<String>) -> CompileError {
    CompileError::runtime(message)
}

/// One component: an object generated from a class needing its own stack
/// (`runtime/sequencing.c`'s `simrt_component`).
#[derive(Debug)]
pub(super) struct SeqComponent {
    /// This component's own parked call stack; empty exactly while it is the
    /// active one (its frames then live in [`Vm::frames`] instead).
    pub(super) frames: Vec<CallFrame>,
    state: SeqState,
    /// 7.3.1 case 1 target: the block instance this object is attached to.
    attached_to: Option<SeqTarget>,
    /// Where this component's reactivation point currently lives, once it has
    /// left `Attached` at least once.
    park: Option<SeqTarget>,
    /// System this component is local to, or `None` for one that can only
    /// ever be an independent component (7.2).
    system: Option<usize>,
    /// Heap object index this component belongs to.
    pub(super) object: usize,
    /// The component that was active when this one was generated (used to
    /// walk outward when looking up a declaring block's system).
    origin: SeqTarget,
    /// A prefixed block instance: has a detach attribute but isn't an object
    /// (7.3.1's "if X is an instance of a prefixed block …").
    pub(super) block_instance: bool,
    /// Entry function name, taken (and the body started) by `SeqObjectStart`.
    entry_name: Option<String>,
}

impl SeqComponent {
    pub(super) fn is_detached(&self) -> bool {
        matches!(self.state, SeqState::Detached)
    }
}

/// A quasi-parallel system created by entering a subblock or prefixed block
/// that declares a class (7.2).
#[derive(Debug)]
pub(super) struct SeqSystemState {
    /// Reactivation point of the system's main component.
    main_park: SeqTarget,
    /// The system's operative component; `None` means the main component.
    operative: Option<usize>,
}

/// One active `(block, system, owner)` triple — mirrors `simrt_seq_frame`.
#[derive(Debug, Clone, Copy)]
pub(super) struct SeqFrame {
    block: i64,
    system: usize,
    owner: SeqTarget,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SimEventNotice {
    pub(super) evtime: f64,
    pub(super) process: usize,
    /// Insertion order, kept only for parity with native's tie-break field
    /// (this interpreter already inserts at the correctly ordered position).
    #[allow(dead_code)]
    seq: i64,
}

/// Ch.12 sequencing set (SQS) state — mirrors `runtime/runtime.c`'s `g_sim`.
#[derive(Debug, Default)]
pub(super) struct SimState {
    pub(super) active: bool,
    /// Head of the SQS: the process that *should* be active.
    pub(super) current: Option<usize>,
    /// The component physically executing (trails `current` mid-transfer).
    pub(super) running: Option<usize>,
    pub(super) sqs: Vec<SimEventNotice>,
    next_seq: i64,
    /// Heap object index of the `MAIN` sentinel, allocated by `SimBegin`.
    pub(super) main_object: Option<usize>,
}

impl<'a> Vm<'a> {
    /// Dispatches every `Simset*` / `Seq*` / `Sim*` op (Phase 6).
    pub(super) fn execute_seq_sim_or_simset(
        &mut self,
        frame_index: usize,
        op: &Op,
    ) -> Result<ExecResult, CompileError> {
        match op {
            // ---------------------------------------------------------- SIMSET
            Op::SimsetSetHeadClassId { class_id } => {
                self.simset_head_class_id = *class_id;
                Ok(ExecResult::Continue)
            }
            Op::SimsetInitHead { head } => {
                if let Some(idx) = self.object_or_none(frame_index, *head)? {
                    let raw = idx as i64 + 1;
                    self.store_object_i64(idx, SIMSET_SUC_OFFSET, raw, SlotTag::Object)?;
                    self.store_object_i64(idx, SIMSET_PRED_OFFSET, raw, SlotTag::Object)?;
                }
                Ok(ExecResult::Continue)
            }
            Op::SimsetOut { object } => {
                if let Some(idx) = self.object_or_none(frame_index, *object)? {
                    self.simset_out_object(idx)?;
                }
                Ok(ExecResult::Continue)
            }
            Op::SimsetPrecede { object, ptr } => {
                self.simset_precede(frame_index, *object, *ptr)?;
                Ok(ExecResult::Continue)
            }
            Op::SimsetFollow { object, ptr } => {
                self.simset_follow(frame_index, *object, *ptr)?;
                Ok(ExecResult::Continue)
            }
            Op::SimsetInto { object, head } => {
                self.simset_precede(frame_index, *object, *head)?;
                Ok(ExecResult::Continue)
            }
            Op::SimsetSuc { dest, object } => {
                let x = self.object_or_none(frame_index, *object)?;
                let suc = self.simset_suc_of(x)?;
                self.set_object_or_none(frame_index, *dest, suc);
                Ok(ExecResult::Continue)
            }
            Op::SimsetPred { dest, object } => {
                let x = self.object_or_none(frame_index, *object)?;
                let pred = self.simset_pred_of(x)?;
                self.set_object_or_none(frame_index, *dest, pred);
                Ok(ExecResult::Continue)
            }
            Op::SimsetEmpty { dest, head } => {
                let head = self.object_or_none(frame_index, *head)?;
                let empty = match head {
                    None => true,
                    Some(idx) => match self.simset_load(idx, SIMSET_SUC_OFFSET)? {
                        None => true,
                        Some(suc) => suc == idx,
                    },
                };
                self.frames[frame_index].set_local(*dest, Value::Bool(empty));
                Ok(ExecResult::Continue)
            }
            Op::SimsetCardinal { dest, head } => {
                let head = self.object_or_none(frame_index, *head)?;
                let mut count: i64 = 0;
                let mut ptr = self.simset_suc_of(head)?;
                let limit = self.objects.len() as i64 + 1;
                while let Some(p) = ptr {
                    count += 1;
                    if count > limit {
                        return Err(CompileError::codegen(
                            "MIR interp: SIMSET ring is not well-formed (cardinal did not terminate)",
                        ));
                    }
                    ptr = self.simset_suc_of(Some(p))?;
                }
                self.frames[frame_index].set_local(*dest, Value::I64(count));
                Ok(ExecResult::Continue)
            }

            // ------------------------------------------------- Chapter 7 sequencing
            Op::SeqSystemEnter { dest, block } => {
                let system = self.seq_system_enter(*block);
                self.frames[frame_index].set_local(*dest, Value::SeqSystem(system));
                Ok(ExecResult::Continue)
            }
            Op::SeqSystemExit { system } => {
                match self.frames[frame_index].get_local(*system)?.clone() {
                    Value::SeqSystem(id) => {
                        self.seq_system_exit(id);
                        Ok(ExecResult::Continue)
                    }
                    other => Err(CompileError::codegen(format!(
                        "MIR interp: SeqSystemExit expected a system handle, got {other:?}"
                    ))),
                }
            }
            Op::SeqObjectCreate {
                dest,
                declaring_block,
                entry,
                object,
            } => {
                let entry_name = match self.frames[frame_index].get_local(*entry)?.clone() {
                    Value::FuncRef(name) => name,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: SeqObjectCreate expected funcref, got {other:?}"
                        )));
                    }
                };
                let object_index =
                    self.object_index(frame_index, *object, "generating a component for none")?;
                let system = self.seq_system_for_block(*declaring_block);
                let component_id = self.seq_components.len();
                self.seq_components.push(SeqComponent {
                    frames: Vec::new(),
                    state: SeqState::Attached,
                    attached_to: None,
                    park: None,
                    system,
                    object: object_index,
                    origin: self.active_target,
                    block_instance: false,
                    entry_name: Some(entry_name),
                });
                self.seq_by_object.insert(object_index, component_id);
                self.frames[frame_index].set_local(*dest, Value::SeqComponentHandle(component_id));
                Ok(ExecResult::Continue)
            }
            Op::SeqObjectStart { component } => {
                let component_id = match self.frames[frame_index].get_local(*component)?.clone() {
                    Value::SeqComponentHandle(id) => id,
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: SeqObjectStart expected a component handle, got {other:?}"
                        )));
                    }
                };
                self.seq_object_start(frame_index, component_id)
            }
            Op::SeqBlockInstance { object } => {
                let object_index =
                    self.object_index(frame_index, *object, "prefixed block instance is none")?;
                let component_id = self.seq_components.len();
                self.seq_components.push(SeqComponent {
                    frames: Vec::new(),
                    state: SeqState::Attached,
                    attached_to: None,
                    park: None,
                    system: None,
                    object: object_index,
                    origin: self.active_target,
                    block_instance: true,
                    entry_name: None,
                });
                self.seq_by_object.insert(object_index, component_id);
                Ok(ExecResult::Continue)
            }
            Op::SeqDetach { object } => {
                let idx = self.object_index(frame_index, *object, "detach with respect to none")?;
                self.seq_op_detach(frame_index, idx)
            }
            Op::SeqCall { object } => {
                let idx = self.object_index(frame_index, *object, "call with respect to none")?;
                self.seq_op_call(frame_index, idx)
            }
            Op::SeqResume { object } => {
                let idx = self.object_index(frame_index, *object, "resume with respect to none")?;
                self.seq_op_resume(frame_index, idx)
            }
            Op::SeqTerminate { object } => {
                let idx =
                    self.object_index(frame_index, *object, "final end with respect to none")?;
                self.seq_op_terminate(idx)
            }

            // ------------------------------------------------------- Simulation
            Op::SimBegin => self.sim_begin(),
            Op::SimEnd => {
                self.sim = SimState::default();
                Ok(ExecResult::Continue)
            }
            Op::SimHold { dt } => self.sim_hold(frame_index, *dt),
            Op::SimActivateDirect { process } => self.sim_activate_direct(frame_index, *process),
            Op::SimActivateTimed {
                process,
                t,
                mode,
                prior,
                reac,
            } => self.sim_activate_timed(frame_index, *process, *t, *mode, *prior, *reac),
            Op::SimActivateRelative {
                process,
                other,
                before,
            } => self.sim_activate_relative(frame_index, *process, *other, *before),
            Op::SimPassivate => self.sim_passivate(),
            Op::SimTransferToHead => self.sim_transfer_to_head(frame_index),
            Op::SimTerminateCurrent { process } => {
                let idx = self.object_index(
                    frame_index,
                    *process,
                    "SimTerminateCurrent with respect to none",
                )?;
                self.sim_terminate_current(idx)
            }
            Op::SimCancel { process } => self.sim_cancel(frame_index, *process),
            Op::SimFinishMain => self.sim_finish_main(),
            Op::SimTime { dest } => {
                self.sim_ensure_active()?;
                let t = self.sim_time();
                self.frames[frame_index].set_local(*dest, Value::F64(t));
                Ok(ExecResult::Continue)
            }
            Op::SimIsMainCurrent { dest } => {
                self.sim_ensure_active()?;
                let is_main =
                    self.sim.running.is_none() || self.sim.running == self.sim.main_object;
                self.frames[frame_index].set_local(*dest, Value::Bool(is_main));
                Ok(ExecResult::Continue)
            }
            Op::SimHasCurrent { dest } => {
                self.sim_ensure_active()?;
                let has_current = !self.sim.sqs.is_empty();
                self.frames[frame_index].set_local(*dest, Value::Bool(has_current));
                Ok(ExecResult::Continue)
            }
            Op::SimCurrent { dest } => {
                self.sim_ensure_active()?;
                let idx = self.sim_running_or_main()?;
                self.frames[frame_index].set_local(*dest, Value::ObjectRef(idx));
                Ok(ExecResult::Continue)
            }
            Op::SimMain { dest } => {
                self.sim_ensure_active()?;
                let idx = self.sim.main_object.ok_or_else(sim_not_active)?;
                self.frames[frame_index].set_local(*dest, Value::ObjectRef(idx));
                Ok(ExecResult::Continue)
            }
            Op::SimIdle { dest, process } => self.sim_idle(frame_index, *dest, *process),
            Op::SimTerminated { dest, process } => {
                self.sim_terminated(frame_index, *dest, *process)
            }
            Op::SimEvtime { dest, process } => self.sim_evtime(frame_index, *dest, *process),
            Op::SimNextev { dest, process } => self.sim_nextev(frame_index, *dest, *process),

            other => Err(CompileError::codegen(format!(
                "MIR interp: execute_seq_sim_or_simset called with unexpected op {other:?}"
            ))),
        }
    }

    // ---------------------------------------------------------------- SIMSET

    fn object_or_none(
        &self,
        frame_index: usize,
        local: LocalId,
    ) -> Result<Option<usize>, CompileError> {
        match self.frames[frame_index].get_local(local)?.clone() {
            Value::None => Ok(None),
            Value::ObjectRef(idx) => Ok(Some(idx)),
            other => Err(CompileError::codegen(format!(
                "MIR interp: expected object ref, got {other:?}"
            ))),
        }
    }

    fn set_object_or_none(&mut self, frame_index: usize, local: LocalId, value: Option<usize>) {
        let value = match value {
            Some(idx) => Value::ObjectRef(idx),
            None => Value::None,
        };
        self.frames[frame_index].set_local(local, value);
    }

    fn simset_load(&self, object: usize, offset: i64) -> Result<Option<usize>, CompileError> {
        let raw = self.load_object_i64(object, offset)?;
        Ok(if raw == 0 {
            None
        } else {
            Some((raw - 1) as usize)
        })
    }

    fn simset_store(
        &mut self,
        object: usize,
        offset: i64,
        value: Option<usize>,
    ) -> Result<(), CompileError> {
        let raw = value.map(|idx| idx as i64 + 1).unwrap_or(0);
        // SUC / PRED always denote an object (or `none`), so the collector can
        // follow SIMSET rings out of any live member.
        self.store_object_i64(object, offset, raw, SlotTag::Object)
    }

    fn simset_is_head(&self, object: usize) -> Result<bool, CompileError> {
        if self.simset_head_class_id < 0 {
            return Ok(false);
        }
        Ok(self.load_object_i64(object, 0)? == self.simset_head_class_id)
    }

    fn simset_is_link(&self, object: usize) -> Result<bool, CompileError> {
        Ok(!self.simset_is_head(object)?)
    }

    fn simset_out_object(&mut self, x: usize) -> Result<(), CompileError> {
        let suc = self.simset_load(x, SIMSET_SUC_OFFSET)?;
        if let Some(suc_idx) = suc {
            let pred = self.simset_load(x, SIMSET_PRED_OFFSET)?;
            self.simset_store(suc_idx, SIMSET_PRED_OFFSET, pred)?;
            if let Some(pred_idx) = pred {
                self.simset_store(pred_idx, SIMSET_SUC_OFFSET, Some(suc_idx))?;
            }
            self.simset_store(x, SIMSET_SUC_OFFSET, None)?;
            self.simset_store(x, SIMSET_PRED_OFFSET, None)?;
        }
        Ok(())
    }

    /// `precede(x, ptr)` / `into(x, head)` — insert `x` immediately before
    /// `ptr` (`precede(none)` degrades to a plain `out`, matching §12.3).
    fn simset_precede(
        &mut self,
        frame_index: usize,
        object: LocalId,
        ptr: LocalId,
    ) -> Result<(), CompileError> {
        let Some(x) = self.object_or_none(frame_index, object)? else {
            return Ok(());
        };
        self.simset_out_object(x)?;
        let Some(ptr_idx) = self.object_or_none(frame_index, ptr)? else {
            return Ok(());
        };
        let ptr_suc = self.simset_load(ptr_idx, SIMSET_SUC_OFFSET)?;
        if ptr_suc.is_none() && !self.simset_is_head(ptr_idx)? {
            return Ok(());
        }
        let pred = self.simset_load(ptr_idx, SIMSET_PRED_OFFSET)?;
        self.simset_store(x, SIMSET_SUC_OFFSET, Some(ptr_idx))?;
        self.simset_store(x, SIMSET_PRED_OFFSET, pred)?;
        if let Some(pred_idx) = pred {
            self.simset_store(pred_idx, SIMSET_SUC_OFFSET, Some(x))?;
        }
        self.simset_store(ptr_idx, SIMSET_PRED_OFFSET, Some(x))?;
        Ok(())
    }

    fn simset_follow(
        &mut self,
        frame_index: usize,
        object: LocalId,
        ptr: LocalId,
    ) -> Result<(), CompileError> {
        let Some(x) = self.object_or_none(frame_index, object)? else {
            return Ok(());
        };
        self.simset_out_object(x)?;
        let Some(ptr_idx) = self.object_or_none(frame_index, ptr)? else {
            return Ok(());
        };
        let ptr_suc = self.simset_load(ptr_idx, SIMSET_SUC_OFFSET)?;
        if ptr_suc.is_none() && !self.simset_is_head(ptr_idx)? {
            return Ok(());
        }
        self.simset_store(x, SIMSET_PRED_OFFSET, Some(ptr_idx))?;
        self.simset_store(x, SIMSET_SUC_OFFSET, ptr_suc)?;
        if let Some(suc_idx) = ptr_suc {
            self.simset_store(suc_idx, SIMSET_PRED_OFFSET, Some(x))?;
        }
        self.simset_store(ptr_idx, SIMSET_SUC_OFFSET, Some(x))?;
        Ok(())
    }

    fn simset_suc_of(&self, x: Option<usize>) -> Result<Option<usize>, CompileError> {
        let Some(idx) = x else {
            return Ok(None);
        };
        let mut guard = 0;
        let mut cur = self.simset_load(idx, SIMSET_SUC_OFFSET)?;
        while let Some(s) = cur {
            if !self.simset_is_link(s)? {
                return Ok(None);
            }
            if self.sim.active && self.sim_is_scheduled(s) {
                guard += 1;
                if guard > 65536 {
                    return Ok(None);
                }
                cur = self.simset_load(s, SIMSET_SUC_OFFSET)?;
                continue;
            }
            return Ok(Some(s));
        }
        Ok(None)
    }

    fn simset_pred_of(&self, x: Option<usize>) -> Result<Option<usize>, CompileError> {
        let Some(idx) = x else {
            return Ok(None);
        };
        let mut guard = 0;
        let mut cur = self.simset_load(idx, SIMSET_PRED_OFFSET)?;
        while let Some(p) = cur {
            if !self.simset_is_link(p)? {
                return Ok(None);
            }
            if self.sim.active && self.sim_is_scheduled(p) {
                guard += 1;
                if guard > 65536 {
                    return Ok(None);
                }
                cur = self.simset_load(p, SIMSET_PRED_OFFSET)?;
                continue;
            }
            return Ok(Some(p));
        }
        Ok(None)
    }

    // ------------------------------------------------------ Chapter 7 sequencing

    fn seq_system_enter(&mut self, block: i64) -> usize {
        let system_id = self.seq_systems.len();
        self.seq_systems.push(SeqSystemState {
            main_park: self.active_target,
            operative: None,
        });
        self.seq_frames.push(SeqFrame {
            block,
            system: system_id,
            owner: self.active_target,
        });
        system_id
    }

    fn seq_system_exit(&mut self, system_id: usize) {
        if let Some(pos) = self.seq_frames.iter().rposition(|f| f.system == system_id) {
            self.seq_frames.remove(pos);
        }
    }

    /// Not instrumented (an injected system class, say): keep such an object
    /// a component of one lazily-created outermost system rather than failing.
    fn seq_outermost_system(&mut self) -> usize {
        if let Some(id) = self.seq_outermost_system_id {
            return id;
        }
        let id = self.seq_systems.len();
        self.seq_systems.push(SeqSystemState {
            main_park: SeqTarget::Outermost,
            operative: None,
        });
        self.seq_outermost_system_id = Some(id);
        id
    }

    /// The system of the instance of `block` the active component is
    /// executing inside — mirrors `simrt_seq_system_for_block`.
    fn seq_system_for_block(&mut self, block: i64) -> Option<usize> {
        if block == 0 {
            return None;
        }
        let mut target = Some(self.active_target);
        while let Some(t) = target {
            if let Some(frame) = self
                .seq_frames
                .iter()
                .rev()
                .find(|f| f.block == block && f.owner == t)
            {
                return Some(frame.system);
            }
            target = match t {
                SeqTarget::Component(id) => Some(self.seq_components[id].origin),
                SeqTarget::Outermost => None,
            };
        }
        Some(self.seq_outermost_system())
    }

    fn seq_require(&self, object: usize, op_name: &str) -> Result<usize, CompileError> {
        self.seq_by_object.get(&object).copied().ok_or_else(|| {
            seq_runtime_error(format!(
                "{op_name} with respect to an object that never became a component"
            ))
        })
    }

    fn seq_store(&mut self, target: SeqTarget, frames: Vec<CallFrame>) {
        match target {
            SeqTarget::Outermost => self.parked_outermost = Some(frames),
            SeqTarget::Component(id) => self.seq_components[id].frames = frames,
        }
    }

    fn seq_load(&mut self, target: SeqTarget) -> Result<Vec<CallFrame>, CompileError> {
        match target {
            SeqTarget::Outermost => self.parked_outermost.take().ok_or_else(|| {
                CompileError::codegen("MIR interp: outermost sequencing stack is not parked")
            }),
            SeqTarget::Component(id) => {
                let frames = std::mem::take(&mut self.seq_components[id].frames);
                if frames.is_empty() {
                    return Err(CompileError::codegen(
                        "MIR interp: component has no parked stack (not started yet, or already active)",
                    ));
                }
                Ok(frames)
            }
        }
    }

    fn seq_switch_to(&mut self, target: SeqTarget) -> Result<(), CompileError> {
        let frames = self.seq_load(target)?;
        self.frames = frames;
        self.active_target = target;
        Ok(())
    }

    /// Parks whatever is currently in [`Vm::frames`] under [`Vm::active_target`]
    /// and returns that target (the coroutine that was "current").
    fn seq_park_active(&mut self) -> SeqTarget {
        let current = self.active_target;
        let frames = std::mem::take(&mut self.frames);
        self.seq_store(current, frames);
        current
    }

    /// Shared by `detach` (7.3.1) and an object's final end (7.3.4) — mirrors
    /// `simrt_seq_leave`.
    fn seq_leave(&mut self, component: usize, next: SeqState) -> Result<ExecResult, CompileError> {
        let target = match self.seq_components[component].state {
            SeqState::Attached => self.seq_components[component].attached_to.ok_or_else(|| {
                CompileError::codegen(
                    "MIR interp: detach with respect to an attached object with no attachment point",
                )
            })?,
            SeqState::Resumed => {
                let system = self.seq_components[component].system.ok_or_else(|| {
                    CompileError::codegen("MIR interp: a resumed object must belong to a system")
                })?;
                let target = self.seq_systems[system].main_park;
                self.seq_systems[system].operative = None;
                target
            }
            SeqState::Detached => {
                return Err(seq_runtime_error(
                    "detach with respect to an object that is already detached",
                ));
            }
            SeqState::Terminated => {
                return Err(seq_runtime_error(
                    "detach with respect to a terminated object",
                ));
            }
        };
        let leaving_from = self.seq_park_active();
        self.seq_components[component].state = next;
        self.seq_components[component].park = if next == SeqState::Terminated {
            None
        } else {
            Some(leaving_from)
        };
        self.seq_switch_to(target)?;
        Ok(ExecResult::Switch)
    }

    fn seq_op_detach(
        &mut self,
        frame_index: usize,
        object: usize,
    ) -> Result<ExecResult, CompileError> {
        let component = self.seq_require(object, "detach")?;
        if self.seq_components[component].block_instance {
            // 7.3.1: "If X is an instance of a prefixed block the detach
            // statement has no effect."
            return Ok(ExecResult::Continue);
        }
        self.frames[frame_index].pc += 1;
        self.seq_leave(component, SeqState::Detached)
    }

    fn seq_op_terminate(&mut self, object: usize) -> Result<ExecResult, CompileError> {
        let component = self.seq_require(object, "final end")?;
        self.seq_leave(component, SeqState::Terminated)
    }

    fn seq_op_call(
        &mut self,
        frame_index: usize,
        object: usize,
    ) -> Result<ExecResult, CompileError> {
        let component = self.seq_require(object, "call")?;
        if self.seq_components[component].state != SeqState::Detached {
            return Err(seq_runtime_error(format!(
                "call with respect to an object that is {}; §7.3.2 requires a detached object",
                seq_state_english(self.seq_components[component].state),
            )));
        }
        let current = self.active_target;
        self.frames[frame_index].pc += 1;
        self.seq_park_active();
        self.seq_components[component].state = SeqState::Attached;
        self.seq_components[component].attached_to = Some(current);
        let park = self.seq_components[component].park.ok_or_else(|| {
            seq_runtime_error("call with respect to a component with no reactivation point")
        })?;
        self.seq_switch_to(park)?;
        Ok(ExecResult::Switch)
    }

    fn seq_op_resume(
        &mut self,
        frame_index: usize,
        object: usize,
    ) -> Result<ExecResult, CompileError> {
        let component = self.seq_require(object, "resume")?;
        let system = self.seq_components[component].system.ok_or_else(|| {
            seq_runtime_error("resume with respect to an object that is not local to a system head")
        })?;
        match self.seq_components[component].state {
            SeqState::Resumed => return Ok(ExecResult::Continue),
            SeqState::Detached => {}
            other => {
                return Err(seq_runtime_error(format!(
                    "resume with respect to an object that is {}; §7.3.3 requires a detached object",
                    seq_state_english(other),
                )));
            }
        }
        let current = self.active_target;
        match self.seq_systems[system].operative {
            None => self.seq_systems[system].main_park = current,
            Some(operative) => {
                self.seq_components[operative].state = SeqState::Detached;
                self.seq_components[operative].park = Some(current);
            }
        }
        self.seq_systems[system].operative = Some(component);
        self.seq_components[component].state = SeqState::Resumed;

        self.frames[frame_index].pc += 1;
        self.seq_park_active();
        let park = self.seq_components[component].park.ok_or_else(|| {
            CompileError::codegen(
                "MIR interp: resume with respect to a component with no reactivation point",
            )
        })?;
        self.seq_switch_to(park)?;
        Ok(ExecResult::Switch)
    }

    /// 12.3 expressed as chapter 7: `self`'s final end composed with a resume
    /// of `target`, as one switch (a terminated component cannot be switched
    /// out of a second time). Mirrors `simrt_seq_terminate_resuming`.
    fn seq_op_terminate_resuming(
        &mut self,
        self_object: usize,
        target_object: usize,
    ) -> Result<ExecResult, CompileError> {
        let self_component = self.seq_require(self_object, "final end")?;
        let target_component = self.seq_require(target_object, "resume")?;
        let system = self.seq_components[target_component]
            .system
            .ok_or_else(|| {
                CompileError::codegen("MIR interp: a scheduled process must belong to a system")
            })?;
        if self.seq_components[target_component].state != SeqState::Detached {
            return Err(CompileError::codegen(
                "MIR interp: scheduling an object that is not detached; a detached object is required",
            ));
        }
        self.seq_park_active();
        self.seq_components[self_component].state = SeqState::Terminated;
        self.seq_components[self_component].park = None;
        if self.seq_systems[system].operative == Some(self_component) {
            self.seq_systems[system].operative = None;
        }
        self.seq_systems[system].operative = Some(target_component);
        self.seq_components[target_component].state = SeqState::Resumed;
        let park = self.seq_components[target_component].park.ok_or_else(|| {
            CompileError::codegen("MIR interp: terminate-resuming target has no reactivation point")
        })?;
        self.seq_switch_to(park)?;
        Ok(ExecResult::Switch)
    }

    fn build_component_stack(
        &self,
        name: &str,
        arg: Value,
    ) -> Result<Vec<CallFrame>, CompileError> {
        let index = self.functions.get(name).copied().ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: undefined function '{name}'"))
        })?;
        let function = &self.module.functions[index];
        let mut frame = CallFrame::new(function, vec![arg], None)?;
        frame.function_index = index;
        Ok(vec![frame])
    }

    /// Runs a freshly created component's body, attached to the generator
    /// (mirrors `simrt_seq_object_start`, minus the real stack switch:
    /// here it's just handing `Vm::frames` a fresh `Vec<CallFrame>`).
    fn seq_object_start(
        &mut self,
        frame_index: usize,
        component_id: usize,
    ) -> Result<ExecResult, CompileError> {
        let entry_name = self.seq_components[component_id]
            .entry_name
            .take()
            .ok_or_else(|| CompileError::codegen("MIR interp: component already started"))?;
        let object_index = self.seq_components[component_id].object;
        let generator = self.active_target;
        self.seq_components[component_id].attached_to = Some(generator);
        self.frames[frame_index].pc += 1;
        self.seq_park_active();
        let fresh = self.build_component_stack(&entry_name, Value::ObjectRef(object_index))?;
        self.frames = fresh;
        self.active_target = SeqTarget::Component(component_id);
        Ok(ExecResult::Switch)
    }

    // ------------------------------------------------------------- Simulation

    fn sim_ensure_active(&self) -> Result<(), CompileError> {
        if !self.sim.active {
            return Err(sim_not_active());
        }
        Ok(())
    }

    fn sim_cancel_unlocked(&mut self, process: usize) {
        self.sim.sqs.retain(|notice| notice.process != process);
    }

    fn sim_next_seq(&mut self) -> i64 {
        let seq = self.sim.next_seq;
        self.sim.next_seq += 1;
        seq
    }

    /// Insert (or replace) an event. `prior` places equal times earlier.
    ///
    /// Errors if the SQS would exceed [`MAX_SQS_LENGTH`], matching native and
    /// wasm (`runtime/runtime.c` `"SQS length limit exceeded"`).
    fn sim_insert_event(
        &mut self,
        evtime: f64,
        process: usize,
        prior: bool,
    ) -> Result<(), CompileError> {
        self.sim_cancel_unlocked(process);
        if self.sim.sqs.len() >= MAX_SQS_LENGTH {
            return Err(CompileError::codegen("SQS length limit exceeded"));
        }
        let seq = self.sim_next_seq();
        let idx = if prior {
            self.sim
                .sqs
                .iter()
                .position(|n| n.evtime >= evtime)
                .unwrap_or(self.sim.sqs.len())
        } else {
            self.sim
                .sqs
                .iter()
                .position(|n| n.evtime > evtime)
                .unwrap_or(self.sim.sqs.len())
        };
        self.sim.sqs.insert(
            idx,
            SimEventNotice {
                evtime,
                process,
                seq,
            },
        );
        Ok(())
    }

    fn sim_is_scheduled(&self, process: usize) -> bool {
        self.sim.sqs.iter().any(|n| n.process == process)
    }

    fn sim_advance_current(&mut self) {
        self.sim.current = self.sim.sqs.first().map(|n| n.process);
    }

    fn sim_head(&self) -> Option<usize> {
        self.sim.sqs.first().map(|n| n.process)
    }

    fn sim_time(&self) -> f64 {
        self.sim.sqs.first().map(|n| n.evtime).unwrap_or(0.0)
    }

    fn sim_running_or_main(&self) -> Result<usize, CompileError> {
        self.sim
            .running
            .or(self.sim.main_object)
            .ok_or_else(sim_not_active)
    }

    fn sim_begin(&mut self) -> Result<ExecResult, CompileError> {
        if self.sim.active {
            return Err(CompileError::runtime("nested Simulation is not supported"));
        }
        self.sim = SimState {
            active: true,
            next_seq: 1,
            ..SimState::default()
        };
        let main_index = self.alloc_object(8, SIM_MAIN_CLASS_ID)?;
        self.sim.main_object = Some(main_index);
        self.sim_insert_event(0.0, main_index, true)?;
        self.sim.current = Some(main_index);
        self.sim.running = Some(main_index);
        Ok(ExecResult::Continue)
    }

    fn sim_hold(&mut self, frame_index: usize, dt: LocalId) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let dt = expect_f64(self.frames[frame_index].get_local(dt)?, "SimHold dt")?;
        let self_process = self.sim_running_or_main()?;
        let now = self.sim_time();
        let delay = dt.max(0.0);
        self.sim_insert_event(now + delay, self_process, false)?;
        self.sim_advance_current();
        Ok(ExecResult::Continue)
    }

    fn sim_activate_direct(
        &mut self,
        frame_index: usize,
        process: LocalId,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let Some(idx) = self.object_or_none(frame_index, process)? else {
            return Ok(ExecResult::Continue);
        };
        if !self.sim_is_scheduled(idx) {
            let now = self.sim_time();
            self.sim_insert_event(now, idx, true)?;
            self.sim_advance_current();
        }
        Ok(ExecResult::Continue)
    }

    /// `mode`: 0 = delay (`time + max(t,0)`), 1 = at (`max(t,time)`).
    fn sim_activate_timed(
        &mut self,
        frame_index: usize,
        process: LocalId,
        t: LocalId,
        mode: i64,
        prior: bool,
        reac: bool,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let Some(idx) = self.object_or_none(frame_index, process)? else {
            return Ok(ExecResult::Continue);
        };
        if !reac && self.sim_is_scheduled(idx) {
            return Ok(ExecResult::Continue);
        }
        let t = expect_f64(self.frames[frame_index].get_local(t)?, "SimActivateTimed t")?;
        let now = self.sim_time();
        let at = if mode == 0 {
            now + t.max(0.0)
        } else {
            t.max(now)
        };
        if at <= now && prior {
            self.sim_insert_event(now, idx, true)?;
        } else {
            self.sim_insert_event(at.max(now), idx, prior)?;
        }
        self.sim_advance_current();
        Ok(ExecResult::Continue)
    }

    /// Insert `process` at the same time as `other`, immediately before or
    /// after it; a no-op if `other` isn't scheduled.
    fn sim_activate_relative(
        &mut self,
        frame_index: usize,
        process: LocalId,
        other: LocalId,
        before: bool,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let Some(idx) = self.object_or_none(frame_index, process)? else {
            return Ok(ExecResult::Continue);
        };
        let Some(other_idx) = self.object_or_none(frame_index, other)? else {
            return Ok(ExecResult::Continue);
        };
        if idx == other_idx {
            return Ok(ExecResult::Continue);
        }
        let Some(pos) = self.sim.sqs.iter().position(|n| n.process == other_idx) else {
            return Ok(ExecResult::Continue);
        };
        let other_time = self.sim.sqs[pos].evtime;
        self.sim_cancel_unlocked(idx);
        let pos = self
            .sim
            .sqs
            .iter()
            .position(|n| n.process == other_idx)
            .expect("other process was scheduled just before this insertion");
        let mut insert_at = if before { pos } else { pos + 1 };
        // MAIN after X: after all same-time peers (simtst96); else immediately
        // after Y (simtst97).
        if !before && self.sim.main_object == Some(idx) {
            while insert_at < self.sim.sqs.len() && self.sim.sqs[insert_at].evtime == other_time {
                insert_at += 1;
            }
        }
        let seq = self.sim_next_seq();
        self.sim.sqs.insert(
            insert_at,
            SimEventNotice {
                evtime: other_time,
                process: idx,
                seq,
            },
        );
        self.sim_advance_current();
        Ok(ExecResult::Continue)
    }

    fn sim_passivate(&mut self) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let self_process = self.sim_running_or_main()?;
        self.sim_cancel_unlocked(self_process);
        self.sim_advance_current();
        Ok(ExecResult::Continue)
    }

    fn sim_cancel(
        &mut self,
        frame_index: usize,
        process: LocalId,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        if let Some(idx) = self.object_or_none(frame_index, process)? {
            self.sim_cancel_unlocked(idx);
            self.sim_advance_current();
        }
        Ok(ExecResult::Continue)
    }

    fn sim_finish_main(&mut self) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let main = self.sim.main_object.ok_or_else(sim_not_active)?;
        self.sim_cancel_unlocked(main);
        self.sim_advance_current();
        Ok(ExecResult::Continue)
    }

    /// Chapter 12 scheduling expressed as a chapter 7 transfer: whichever
    /// process now heads the SQS becomes operative, mirroring
    /// `simrt_sim_transfer_to_head`.
    fn sim_transfer_to_head(&mut self, frame_index: usize) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let main = self.sim.main_object.ok_or_else(sim_not_active)?;
        let head = self.sim_head().unwrap_or(main);
        let running = self.sim.running.unwrap_or(main);
        self.sim.current = Some(head);
        if head == running {
            return Ok(ExecResult::Continue);
        }
        self.sim.running = Some(head);
        if head == main {
            if running != main {
                return self.seq_op_detach(frame_index, running);
            }
            return Ok(ExecResult::Continue);
        }
        self.seq_op_resume(frame_index, head)
    }

    /// The active process reaches its final end: it leaves the SQS and the
    /// next process (or MAIN) takes over. Mirrors `simrt_sim_terminate_current`.
    fn sim_terminate_current(&mut self, process: usize) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        self.sim_cancel_unlocked(process);
        let main = self.sim.main_object.ok_or_else(sim_not_active)?;
        let head = self.sim_head().unwrap_or(main);
        self.sim.current = Some(head);
        self.sim.running = Some(head);
        if head == main {
            self.seq_op_terminate(process)
        } else {
            self.seq_op_terminate_resuming(process, head)
        }
    }

    fn sim_idle(
        &mut self,
        frame_index: usize,
        dest: LocalId,
        process: LocalId,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let idle = match self.object_or_none(frame_index, process)? {
            None => true,
            Some(idx) => !self.sim_is_scheduled(idx),
        };
        self.frames[frame_index].set_local(dest, Value::Bool(idle));
        Ok(ExecResult::Continue)
    }

    fn sim_terminated(
        &mut self,
        frame_index: usize,
        dest: LocalId,
        process: LocalId,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let terminated = match self.object_or_none(frame_index, process)? {
            None => false,
            Some(idx) if Some(idx) == self.sim.main_object => false,
            Some(idx) => self
                .seq_by_object
                .get(&idx)
                .map(|&component| self.seq_components[component].state == SeqState::Terminated)
                .unwrap_or(false),
        };
        self.frames[frame_index].set_local(dest, Value::Bool(terminated));
        Ok(ExecResult::Continue)
    }

    fn sim_evtime(
        &mut self,
        frame_index: usize,
        dest: LocalId,
        process: LocalId,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let t = match self.object_or_none(frame_index, process)? {
            None => 0.0,
            Some(idx) => self
                .sim
                .sqs
                .iter()
                .find(|n| n.process == idx)
                .map(|n| n.evtime)
                .ok_or_else(|| CompileError::codegen("MIR interp: evtime of idle process"))?,
        };
        self.frames[frame_index].set_local(dest, Value::F64(t));
        Ok(ExecResult::Continue)
    }

    fn sim_nextev(
        &mut self,
        frame_index: usize,
        dest: LocalId,
        process: LocalId,
    ) -> Result<ExecResult, CompileError> {
        self.sim_ensure_active()?;
        let next = match self.object_or_none(frame_index, process)? {
            None => None,
            Some(idx) => match self.sim.sqs.iter().position(|n| n.process == idx) {
                None => None,
                Some(pos) => self.sim.sqs.get(pos + 1).map(|n| n.process),
            },
        };
        self.set_object_or_none(frame_index, dest, next);
        Ok(ExecResult::Continue)
    }
}

fn sim_not_active() -> CompileError {
    crate::diagnostics::simulation_not_active()
}
