//! Chapter 7 quasi-parallel sequencing for targets with no stack switching.
//!
//! `runtime/sequencing.c` expresses the Standard over a real stack-switching
//! primitive, `coro_switch(from, to)`. Everything above that primitive is
//! bookkeeping: which component is operative, where each one's reactivation
//! point lives, which system a generator reports to. None of it needs stacks.
//!
//! So this is that same state machine, emitted as MIR, over a coroutine whose
//! "stack" is a heap buffer of spilled frames (see [`super::asyncify`]). The one
//! idea that makes the port exact is that a coroutine is identified with its
//! spill buffer: `park`, `attached_to` and `origin` all point at buffers, and
//! reactivating a component means rewinding the buffer its reactivation point
//! lives in — which, as chapter 7 requires, need not be its own.
//!
//! `coro_switch` becomes: name the buffer to enter next, then ask to unwind.
//! The caller's transformed frames spill themselves on the way out and the
//! trampoline enters the named buffer.

use std::collections::HashSet;

use super::build::{FunctionBuilder, PTR};
use super::{BinOp, CallSig, CmpOp, Function, MirType, Op};

/// Base of the runtime's static state in linear memory. The wasm backend lays
/// its data image out around this; see `codegen::wasm`.
/// Sequencing runtime static state previously sat at 512, leaving only ~60
/// iovec slots for string literals. Bump the base so large DosTestBatch units
/// (hundreds of strings) still fit the fixed iovec table below STATE.
pub const STATE_BASE: i64 = 8192;
pub const STATE_BYTES: i64 = 64;

// Offsets within the static state.
/// 0 running normally, 1 unwinding to the trampoline, 2 rewinding a buffer.
const STATE_PHASE: i64 = 0;
/// The buffer whose frames are on the physical stack right now.
pub const STATE_CURRENT: i64 = 8;
/// The buffer the trampoline should enter once the unwind completes.
const STATE_NEXT: i64 = 16;
/// Head of the list of every component, for the two lookups chapter 7 needs.
const STATE_COMPONENTS: i64 = 24;
/// Innermost active system head (7.2), as a list threaded outwards.
const STATE_SYSTEM_FRAMES: i64 = 32;
const STATE_OUTERMOST_SYSTEM: i64 = 40;
const STATE_MAIN_CORO: i64 = 48;

const PHASE_NORMAL: i64 = 0;
const PHASE_UNWINDING: i64 = 1;
const PHASE_REWINDING: i64 = 2;

// A coroutine: an entry point plus the buffers its suspended frames live in.
const CORO_ENTRY: i64 = 8;
/// The object the entry point is called with. Read and written **only**
/// through [`CORO_ARG_STORE`] / [`CORO_ARG_LOAD`], because on wasm the value
/// does not live here at all — see [`CORO_GC_SLOT`].
const CORO_ARG: i64 = 16;
const CORO_STARTED: i64 = 24;
/// Scalar/PC spill buffer (`resume | scalars… | ref_bytes | scalar_bytes`).
const CORO_FRAMES: i64 = 32;
const CORO_SP: i64 = 40;
const CORO_CAP: i64 = 48;
/// GC-ref spine (`refs…`), parallel to [`CORO_FRAMES`]. Both frame lengths
/// live in the scalar frame, because the wasm spine is an `(array (ref null
/// eq))` with no room for an `i64` word. **Unused on wasm**, where the spine
/// is a field of the coroutine's GC side record ([`CORO_GC_SLOT`]).
pub const CORO_REF_FRAMES: i64 = 56;
pub const CORO_REF_SP: i64 = 64;
pub const CORO_REF_CAP: i64 = 72;
/// Byte offset (**not** an address) into the ref spine of the frame currently
/// being filled (push) or restored (pop).
pub const CORO_REF_CUR: i64 = 80;
/// Index of this coroutine's host-traced GC side record (Phase 4-R4).
///
/// Linear memory cannot hold a WasmGC reference, so on wasm `CORO_ARG` and
/// the ref spine live in a `seq_gc_slot` struct kept in a global registry
/// array, and this scalar word is the coroutine's index into that registry.
/// A dedicated word rather than a reused one ([`CORO_ARG`] and
/// [`CORO_REF_FRAMES`] are both dead on wasm) so the slot exists before
/// anything wants it and no ordering constraint is hidden in an aliased
/// field. Interpreter and native never read it: [`SEQ_GC_SLOT_NEW`] hands
/// them 0 and their `CORO_ARG` / `CORO_REF_FRAMES` words stay authoritative.
pub const CORO_GC_SLOT: i64 = 88;
const CORO_SIZE: i64 = 96;

/// Initial spill room per coroutine buffer (scalar and ref each get one).
/// A scalar frame costs a word for the resume point, one per scalar local, and
/// the two length words. [`frame_push`] doubles (or jumps to the needed size)
/// when this is exceeded, so large activations like DosTestBatch `simtst98` no
/// longer need a magic constant large enough for every program.
pub const FRAMES_INITIAL_BYTES: i64 = 8192;

// A component (7.1). Mirrors `struct simrt_component`.
const COMP_HEAD: i64 = 8;
const COMP_PARK: i64 = 16;
const COMP_ATTACHED_TO: i64 = 24;
const COMP_SYSTEM: i64 = 32;
const COMP_STATE: i64 = 40;
/// The component's Simula object — the chapter 7 root. Read and written
/// **only** through [`COMP_OBJECT_STORE`] / [`COMP_OBJECT_LOAD`]; on wasm the
/// object lives in the component's GC side record instead ([`COMP_GC_SLOT`]).
const COMP_OBJECT: i64 = 48;
const COMP_ORIGIN: i64 = 56;
const COMP_BLOCK_INSTANCE: i64 = 64;
const COMP_NEXT: i64 = 72;
/// The component's index into the sequencing GC registry — [`CORO_GC_SLOT`]
/// for component records.
pub const COMP_GC_SLOT: i64 = 80;
const COMP_SIZE: i64 = 88;

// 7.1 states of execution.
const STATE_ATTACHED: i64 = 0;
const STATE_DETACHED: i64 = 1;
const STATE_RESUMED: i64 = 2;
const STATE_TERMINATED: i64 = 3;

// A quasi-parallel system (7.2).
const SYS_MAIN_PARK: i64 = 8;
const SYS_OPERATIVE: i64 = 16;
const SYS_SIZE: i64 = 24;

// One active system head: which block, whose instance, and the system itself.
const FRAME_BLOCK: i64 = 8;
const FRAME_SYSTEM: i64 = 16;
const FRAME_OWNER: i64 = 24;
const FRAME_NEXT: i64 = 32;
const FRAME_SIZE: i64 = 40;

// Names the asyncify transform emits calls to.
pub const START: &str = "__simrt_seq_start";
pub const PHASE_IS_UNWINDING: &str = "__simrt_asyncify_unwinding";
pub const PHASE_IS_REWINDING: &str = "__simrt_asyncify_rewinding";
pub const FRAME_PUSH: &str = "__simrt_asyncify_frame_push";
pub const FRAME_POP: &str = "__simrt_asyncify_frame_pop";
/// Store a GC-ref candidate into the frame's ref region (Phase 4c seam).
pub const SPILL_STORE_REF: &str = "__simrt_asyncify_spill_store_ref";
/// Load a GC-ref candidate from the frame's ref region (Phase 4c seam).
pub const SPILL_LOAD_REF: &str = "__simrt_asyncify_spill_load_ref";
/// Give a fresh coroutine its ref spine (Phase 4-R2 seam).
pub const REFS_CREATE: &str = "__simrt_asyncify_refs_create";
/// Widen a coroutine's ref spine (Phase 4-R2 seam).
pub const REFS_GROW: &str = "__simrt_asyncify_refs_grow";
/// Reserve a sequencing GC side-record slot (Phase 4-R4 seam).
///
/// Returns the new slot's index, which the caller stores in the linear
/// record's [`CORO_GC_SLOT`] / [`COMP_GC_SLOT`] word. Interpreter and native
/// have nothing to root — their records hold the references directly — so the
/// MIR body returns 0 and nothing reads it back.
pub const SEQ_GC_SLOT_NEW: &str = "__simrt_seq_gc_slot_new";
/// `coro.arg := value` (Phase 4-R4 seam).
pub const CORO_ARG_STORE: &str = "__simrt_coro_arg_store";
/// `return coro.arg` (Phase 4-R4 seam).
pub const CORO_ARG_LOAD: &str = "__simrt_coro_arg_load";
/// `component.object := value` (Phase 4-R4 seam).
pub const COMP_OBJECT_STORE: &str = "__simrt_seq_component_object_store";
/// `return component.object` (Phase 4-R4 seam).
pub const COMP_OBJECT_LOAD: &str = "__simrt_seq_component_object_load";
pub const SEQ_SYSTEM_ENTER: &str = "__simrt_seq_system_enter";
pub const SEQ_SYSTEM_EXIT: &str = "__simrt_seq_system_exit";
pub const SEQ_OBJECT_CREATE: &str = "__simrt_seq_object_create";
pub const SEQ_OBJECT_START: &str = "__simrt_seq_object_start";
pub const SEQ_BLOCK_INSTANCE: &str = "__simrt_seq_block_instance";
pub const SEQ_DETACH: &str = "__simrt_seq_detach";
pub const SEQ_CALL: &str = "__simrt_seq_call";
pub const SEQ_RESUME: &str = "__simrt_seq_resume";
pub const SEQ_TERMINATE: &str = "__simrt_seq_terminate";
pub const SEQ_TERMINATE_RESUMING: &str = "__simrt_seq_terminate_resuming";

const CORO_CREATE: &str = "__simrt_coro_create";
const CORO_SWITCH: &str = "__simrt_coro_switch";
const COMPONENT_OF: &str = "__simrt_seq_component_of";
const COMPONENT_ON: &str = "__simrt_seq_component_running_on";
const REQUIRE: &str = "__simrt_seq_require";
const REGISTER: &str = "__simrt_seq_register";
const OUTERMOST_SYSTEM: &str = "__simrt_seq_outermost_system";
const SYSTEM_FOR_BLOCK: &str = "__simrt_seq_system_for_block";
const LEAVE: &str = "__simrt_seq_leave";

/// A component entry takes its object and returns nothing; the trampoline calls
/// every buffer's entry through this one signature.
pub fn entry_signature() -> CallSig {
    CallSig {
        params: vec![MirType::ObjectRef],
        result: None,
    }
}

/// Every function this module synthesizes. These implement suspension rather
/// than undergoing it, so [`super::asyncify`] leaves them alone.
pub fn function_names() -> HashSet<String> {
    functions()
        .into_iter()
        .map(|function| function.name)
        .collect()
}

pub fn functions() -> Vec<Function> {
    vec![
        state_base(),
        phase_is(PHASE_IS_UNWINDING, PHASE_UNWINDING),
        phase_is(PHASE_IS_REWINDING, PHASE_REWINDING),
        frame_push(),
        frame_pop(),
        spill_store_ref(),
        spill_load_ref(),
        frames_grow(),
        refs_grow(),
        refs_create(),
        seq_gc_slot_new(),
        coro_arg_store(),
        coro_arg_load(),
        component_object_store(),
        component_object_load(),
        coro_create(),
        coro_switch(),
        start(),
        component_of(),
        component_running_on(),
        register(),
        require(),
        system_enter(),
        system_exit(),
        outermost_system(),
        system_for_block(),
        object_create(),
        object_start(),
        block_instance(),
        leave(),
        detach(),
        call(),
        resume(),
        terminate(),
        terminate_resuming(),
    ]
}

/// The static state's address as a value. MIR reaches memory only through a
/// pointer, and a constant is the only way to name a fixed one.
fn state_base() -> Function {
    let mut f = FunctionBuilder::returning("__simrt_seq_state", PTR);
    let base = f.konst(STATE_BASE);
    f.ret_value(base);
    f.finish()
}

fn load_state(f: &mut FunctionBuilder, offset: i64) -> super::LocalId {
    let base = f.call_value("__simrt_seq_state", &[], PTR);
    f.load(base, offset)
}

fn store_state(f: &mut FunctionBuilder, offset: i64, value: super::LocalId) {
    let base = f.call_value("__simrt_seq_state", &[], PTR);
    f.store(base, offset, value);
}

fn store_state_const(f: &mut FunctionBuilder, offset: i64, value: i64) {
    let value = f.konst(value);
    store_state(f, offset, value);
}

fn phase_is(name: &str, phase: i64) -> Function {
    let mut f = FunctionBuilder::returning(name, MirType::I64);
    let current = load_state(&mut f, STATE_PHASE);
    let yes = f.block();
    let no = f.block();
    let cond = f.compare_const(CmpOp::Eq, current, phase);
    f.branch(cond, yes, no);
    f.at(yes);
    let one = f.konst(1);
    f.ret_value(one);
    f.at(no);
    let zero = f.konst(0);
    f.ret_value(zero);
    f.finish()
}

/// Reserves spill room on the running coroutine and records `resume` as the
/// block to re-enter. Yields the **scalar** frame base.
///
/// Phase 4-R2 dual buffers:
/// ```text
/// CORO_FRAMES:     resume | scalar[0..S) | ref_bytes | scalar_bytes  (= 24 + 8S)
/// CORO_REF_FRAMES: ref[0..R)                                        (= 8R)
/// ```
/// Both frame lengths ride the scalar frame: on wasm the ref spine is an
/// `(array (ref null eq))`, which cannot hold an `i64` length word. See
/// [`SPILL_STORE_REF`] / [`SPILL_LOAD_REF`].
fn frame_push() -> Function {
    let mut f = FunctionBuilder::returning(FRAME_PUSH, PTR);
    let resume = f.param("resume", MirType::I64);
    let scalar_slots = f.param("scalar_slots", MirType::I64);
    let ref_slots = f.param("ref_slots", MirType::I64);

    let coro = load_state(&mut f, STATE_CURRENT);

    // --- scalar buffer: resume + scalars + both lengths ---
    let sp = f.load(coro, CORO_SP);
    let scalar_payload = f.mul_const(scalar_slots, 8);
    let scalar_bytes = f.add_const(scalar_payload, 24);
    let new_sp = f.binary(BinOp::Add, sp, scalar_bytes);

    let grow = f.block();
    let after_grow = f.block();
    let cap = f.load(coro, CORO_CAP);
    let needs_grow = f.compare(CmpOp::Gt, new_sp, cap);
    f.branch(needs_grow, grow, after_grow);
    f.at(grow);
    f.call(FRAMES_GROW, &[coro, new_sp]);
    f.jump(after_grow);

    f.at(after_grow);
    let frames = f.load(coro, CORO_FRAMES);
    let base = f.binary(BinOp::Add, frames, sp);
    f.store(base, 0, resume);
    let top = f.binary(BinOp::Add, base, scalar_bytes);
    f.store(coro, CORO_SP, new_sp);

    // --- ref spine: R elements, always pushed even when R == 0 ---
    let ref_sp = f.load(coro, CORO_REF_SP);
    let ref_bytes = f.mul_const(ref_slots, 8);
    let new_ref_sp = f.binary(BinOp::Add, ref_sp, ref_bytes);

    let grow_refs = f.block();
    let after_ref_grow = f.block();
    let ref_cap = f.load(coro, CORO_REF_CAP);
    let needs_ref_grow = f.compare(CmpOp::Gt, new_ref_sp, ref_cap);
    f.branch(needs_ref_grow, grow_refs, after_ref_grow);
    f.at(grow_refs);
    f.call(REFS_GROW, &[coro, new_ref_sp]);
    f.jump(after_ref_grow);

    f.at(after_ref_grow);
    let ref_length_word = f.add_const(top, -16);
    f.store(ref_length_word, 0, ref_bytes);
    let scalar_length_word = f.add_const(top, -8);
    f.store(scalar_length_word, 0, scalar_bytes);
    f.store(coro, CORO_REF_SP, new_ref_sp);
    f.store(coro, CORO_REF_CUR, ref_sp);

    f.ret_value(base);
    f.finish()
}

/// `*(CORO_REF_FRAMES + CORO_REF_CUR + 8*index) = value`
///
/// `frame` / `scalar_slots` are kept in the ABI for the asyncify call sites
/// (and for a future WasmGC retarget that may key off the scalar frame); the
/// dual-buffer implementation addresses through [`CORO_REF_CUR`]. Wasm
/// replaces this body with an `array.set` on the ref spine (Phase 4-R2).
fn spill_store_ref() -> Function {
    let mut f = FunctionBuilder::new(SPILL_STORE_REF);
    let _frame = f.param("frame", PTR);
    let _scalar_slots = f.param("scalar_slots", MirType::I64);
    let index = f.param("index", MirType::I64);
    let value = f.param("value", MirType::ObjectRef);

    let coro = load_state(&mut f, STATE_CURRENT);
    let addr = ref_slot_addr(&mut f, coro, index);
    f.store(addr, 0, value);
    f.ret();
    f.finish()
}

/// `return *(CORO_REF_FRAMES + CORO_REF_CUR + 8*index)`
fn spill_load_ref() -> Function {
    let mut f = FunctionBuilder::returning(SPILL_LOAD_REF, MirType::ObjectRef);
    let _frame = f.param("frame", PTR);
    let _scalar_slots = f.param("scalar_slots", MirType::I64);
    let index = f.param("index", MirType::I64);

    let coro = load_state(&mut f, STATE_CURRENT);
    let addr = ref_slot_addr(&mut f, coro, index);
    let value = f.load_object(addr, 0);
    f.ret_value(value);
    f.finish()
}

/// Linear address of ref slot `index` of the frame [`CORO_REF_CUR`] names.
fn ref_slot_addr(
    f: &mut FunctionBuilder,
    coro: super::LocalId,
    index: super::LocalId,
) -> super::LocalId {
    let ref_frames = f.load(coro, CORO_REF_FRAMES);
    let ref_cur = f.load(coro, CORO_REF_CUR);
    let ref_base = f.binary(BinOp::Add, ref_frames, ref_cur);
    let offset = f.mul_const(index, 8);
    f.binary(BinOp::Add, ref_base, offset)
}

const FRAMES_GROW: &str = "__simrt_asyncify_frames_grow";

/// Grows `coro`'s **scalar** spill buffer so it can hold at least `needed` bytes.
///
/// Old buffers are abandoned (bump-allocated heaps have no free). Frame bases
/// are only consumed during the current unwind/rewind of this coro, so moving
/// the buffer is safe. The parallel ref buffer is grown by [`refs_grow`] and is
/// never copied by this function — so abandoned scalar buffers no longer
/// duplicate GC-ref candidate words.
fn frames_grow() -> Function {
    grow_buffer(FRAMES_GROW, CORO_FRAMES, CORO_SP, CORO_CAP)
}

fn refs_grow() -> Function {
    grow_buffer(REFS_GROW, CORO_REF_FRAMES, CORO_REF_SP, CORO_REF_CAP)
}

/// Gives a fresh coroutine its empty ref spine.
///
/// Split out of [`coro_create`] because wasm replaces the whole body with an
/// `array.new_default` of the WasmGC spine, stored into the coroutine's GC
/// side record ([`CORO_GC_SLOT`]); everything else about a coroutine record
/// stays linear.
fn refs_create() -> Function {
    let mut f = FunctionBuilder::new(REFS_CREATE);
    let coro = f.param("coro", PTR);
    let ref_frames = f.alloc(FRAMES_INITIAL_BYTES);
    f.store(coro, CORO_REF_FRAMES, ref_frames);
    f.store_const(coro, CORO_REF_SP, 0);
    f.store_const(coro, CORO_REF_CAP, FRAMES_INITIAL_BYTES);
    f.store_const(coro, CORO_REF_CUR, 0);
    f.ret();
    f.finish()
}

/// Reserves a GC side-record slot for a coroutine or component record.
///
/// Only wasm has anything to reserve (see [`CORO_GC_SLOT`]); the interpreter
/// and native store references straight into the linear record, so this is a
/// constant here and the word the caller writes is never read back.
fn seq_gc_slot_new() -> Function {
    let mut f = FunctionBuilder::returning(SEQ_GC_SLOT_NEW, MirType::I64);
    let none = f.konst(0);
    f.ret_value(none);
    f.finish()
}

/// The four accessors that hide *where* a sequencing record's object lives.
///
/// On the interpreter and native that is the linear record itself, which is
/// what these bodies do. On wasm the value is a field of the record's GC side
/// record, so codegen replaces the body wholesale (as it already does for the
/// ref spine) — the point of routing every access through a named function is
/// that there is exactly one place to replace.
fn coro_arg_store() -> Function {
    let mut f = FunctionBuilder::new(CORO_ARG_STORE);
    let coro = f.param("coro", PTR);
    let value = f.param("value", MirType::ObjectRef);
    f.store(coro, CORO_ARG, value);
    f.ret();
    f.finish()
}

fn coro_arg_load() -> Function {
    let mut f = FunctionBuilder::returning(CORO_ARG_LOAD, MirType::ObjectRef);
    let coro = f.param("coro", PTR);
    let value = f.load_object(coro, CORO_ARG);
    f.ret_value(value);
    f.finish()
}

fn component_object_store() -> Function {
    let mut f = FunctionBuilder::new(COMP_OBJECT_STORE);
    let component = f.param("component", PTR);
    let value = f.param("value", MirType::ObjectRef);
    f.store(component, COMP_OBJECT, value);
    f.ret();
    f.finish()
}

fn component_object_load() -> Function {
    let mut f = FunctionBuilder::returning(COMP_OBJECT_LOAD, MirType::ObjectRef);
    let component = f.param("component", PTR);
    let value = f.load_object(component, COMP_OBJECT);
    f.ret_value(value);
    f.finish()
}

fn grow_buffer(name: &str, frames_off: i64, sp_off: i64, cap_off: i64) -> Function {
    let mut f = FunctionBuilder::new(name);
    let coro = f.param("coro", PTR);
    let needed = f.param("needed", MirType::I64);

    let pick = f.block();
    let use_doubled = f.block();
    let use_needed = f.block();
    let allocate = f.block();
    let copy_head = f.block();
    let copy_body = f.block();
    let copy_done = f.block();

    let old_cap = f.load(coro, cap_off);
    let doubled = f.mul_const(old_cap, 2);
    let new_cap = f.local("new_cap", MirType::I64);
    let doubled_enough = f.compare(CmpOp::Ge, doubled, needed);
    f.branch(doubled_enough, use_doubled, use_needed);

    f.at(use_doubled);
    f.assign(new_cap, doubled);
    f.jump(pick);

    f.at(use_needed);
    f.assign(new_cap, needed);
    f.jump(pick);

    f.at(pick);
    let too_small = f.compare(CmpOp::Lt, new_cap, old_cap);
    let bump = f.block();
    f.branch(too_small, bump, allocate);
    f.at(bump);
    f.assign(new_cap, old_cap);
    f.jump(allocate);

    f.at(allocate);
    let new_frames = f.alloc_bytes(new_cap);
    let old_frames = f.load(coro, frames_off);
    let sp = f.load(coro, sp_off);
    let i = f.local("i", MirType::I64);
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.jump(copy_head);

    f.at(copy_head);
    let done = f.compare(CmpOp::Ge, i, sp);
    f.branch(done, copy_done, copy_body);

    f.at(copy_body);
    let src = f.binary(BinOp::Add, old_frames, i);
    let dst = f.binary(BinOp::Add, new_frames, i);
    let word = f.load(src, 0);
    f.store(dst, 0, word);
    let next = f.add_const(i, 8);
    f.assign(i, next);
    f.jump(copy_head);

    f.at(copy_done);
    f.store(coro, frames_off, new_frames);
    f.store(coro, cap_off, new_cap);
    f.ret();
    f.finish()
}

/// Takes back the frame this activation spilled. Yields its **scalar** base,
/// points [`CORO_REF_CUR`] at the matching ref frame, and returns the
/// coroutine to running normally once both buffers are empty.
fn frame_pop() -> Function {
    let mut f = FunctionBuilder::returning(FRAME_POP, PTR);
    let coro = load_state(&mut f, STATE_CURRENT);

    // Both lengths sit at the top of the scalar frame.
    let frames = f.load(coro, CORO_FRAMES);
    let sp = f.load(coro, CORO_SP);
    let top = f.binary(BinOp::Add, frames, sp);
    let length_word = f.add_const(top, -8);
    let bytes = f.load(length_word, 0);
    let ref_length_word = f.add_const(top, -16);
    let ref_bytes = f.load(ref_length_word, 0);
    let new_sp = f.binary(BinOp::Sub, sp, bytes);
    f.store(coro, CORO_SP, new_sp);
    let base = f.binary(BinOp::Add, frames, new_sp);

    // Pop the matching ref frame and publish it for SPILL_LOAD_REF.
    let ref_sp = f.load(coro, CORO_REF_SP);
    let new_ref_sp = f.binary(BinOp::Sub, ref_sp, ref_bytes);
    f.store(coro, CORO_REF_SP, new_ref_sp);
    f.store(coro, CORO_REF_CUR, new_ref_sp);

    let done = f.block();
    let more = f.block();
    let empty = f.compare_const(CmpOp::Eq, new_sp, 0);
    f.branch(empty, done, more);
    f.at(done);
    store_state_const(&mut f, STATE_PHASE, PHASE_NORMAL);
    f.ret_value(base);
    f.at(more);
    f.ret_value(base);
    f.finish()
}

fn coro_create() -> Function {
    let mut f = FunctionBuilder::returning(CORO_CREATE, PTR);
    let entry = f.param("entry", MirType::FuncRef);
    let arg = f.param("arg", MirType::ObjectRef);
    let coro = f.alloc(CORO_SIZE);
    // Before anything reaches for it: `CORO_ARG_STORE` and `REFS_CREATE` both
    // write through this slot on wasm.
    let gc_slot = f.call_value(SEQ_GC_SLOT_NEW, &[], MirType::I64);
    f.store(coro, CORO_GC_SLOT, gc_slot);
    f.store(coro, CORO_ENTRY, entry);
    f.call(CORO_ARG_STORE, &[coro, arg]);
    f.store_const(coro, CORO_STARTED, 0);
    let frames = f.alloc(FRAMES_INITIAL_BYTES);
    f.store(coro, CORO_FRAMES, frames);
    f.store_const(coro, CORO_SP, 0);
    f.store_const(coro, CORO_CAP, FRAMES_INITIAL_BYTES);
    f.call(REFS_CREATE, &[coro]);
    f.ret_value(coro);
    f.finish()
}

/// The stack-switching primitive, minus the stack switch: name the buffer to
/// enter and ask the physical stack to unwind. `from` is the running coroutine
/// by construction, and the frames spilling on the way out are its own.
fn coro_switch() -> Function {
    let mut f = FunctionBuilder::new(CORO_SWITCH);
    let _from = f.param("from", PTR);
    let to = f.param("to", PTR);
    let bad = f.block();
    let ok = f.block();
    let none = f.compare_const(CmpOp::Eq, to, 0);
    f.branch(none, bad, ok);
    f.at(bad);
    f.abort("sim: quasi-parallel switch to a component with no reactivation point");
    f.at(ok);
    store_state(&mut f, STATE_NEXT, to);
    store_state_const(&mut f, STATE_PHASE, PHASE_UNWINDING);
    f.ret();
    f.finish()
}

/// The trampoline, and the program's entry point. Every suspension unwinds to
/// here, and every reactivation starts here, so the physical stack holds one
/// coroutine's frames at a time.
fn start() -> Function {
    let mut f = FunctionBuilder::new(START);

    let zero_entry = f.konst(0);
    let zero_arg = f.none_object();
    let main_coro = f.call_value(CORO_CREATE, &[zero_entry, zero_arg], PTR);
    store_state(&mut f, STATE_MAIN_CORO, main_coro);
    store_state(&mut f, STATE_CURRENT, main_coro);

    let head = f.block();
    let rewind = f.block();
    let fresh = f.block();
    let enter = f.block();
    let enter_main = f.block();
    let enter_entry = f.block();
    let returned = f.block();
    let again = f.block();
    let done = f.block();
    f.jump(head);

    f.at(head);
    let coro = load_state(&mut f, STATE_CURRENT);
    let started = f.load(coro, CORO_STARTED);
    let running = f.compare_const(CmpOp::Ne, started, 0);
    f.branch(running, rewind, fresh);

    f.at(rewind);
    store_state_const(&mut f, STATE_PHASE, PHASE_REWINDING);
    f.jump(enter);

    f.at(fresh);
    store_state_const(&mut f, STATE_PHASE, PHASE_NORMAL);
    let coro = load_state(&mut f, STATE_CURRENT);
    f.store_const(coro, CORO_STARTED, 1);
    f.jump(enter);

    // The outermost block instance is the main component of the outermost
    // system (7.2, chapter 11), and its body is `main` rather than a class body,
    // so it is the one coroutine entered by name.
    f.at(enter);
    let coro = load_state(&mut f, STATE_CURRENT);
    let main_coro = load_state(&mut f, STATE_MAIN_CORO);
    let is_main = f.compare(CmpOp::Eq, coro, main_coro);
    f.branch(is_main, enter_main, enter_entry);

    f.at(enter_main);
    f.call("main", &[]);
    f.jump(returned);

    f.at(enter_entry);
    let coro = load_state(&mut f, STATE_CURRENT);
    let entry = f.load(coro, CORO_ENTRY);
    let arg = f.call_value(CORO_ARG_LOAD, &[coro], MirType::ObjectRef);
    f.push(Op::CallIndirect {
        dest: None,
        callee: entry,
        args: vec![arg],
        sig: entry_signature(),
    });
    f.jump(returned);

    f.at(returned);
    let phase = load_state(&mut f, STATE_PHASE);
    let unwound = f.compare_const(CmpOp::Eq, phase, PHASE_UNWINDING);
    f.branch(unwound, again, done);

    f.at(again);
    store_state_const(&mut f, STATE_PHASE, PHASE_NORMAL);
    let next = load_state(&mut f, STATE_NEXT);
    store_state(&mut f, STATE_CURRENT, next);
    f.jump(head);

    // Only the main component's body returns here: a class body ends in
    // `seq.terminate`, which never comes back.
    f.at(done);
    f.ret();
    f.finish()
}

/// Objects are named by reference throughout chapter 7, and a reference's
/// static qualification may be a superclass, so a component is found by object
/// identity. Programs make few components, so the list is walked.
fn component_of() -> Function {
    let mut f = FunctionBuilder::returning(COMPONENT_OF, PTR);
    let object = f.param("object", MirType::ObjectRef);
    let missing = f.block();
    let search = f.block();
    let step = f.block();
    let compare = f.block();
    let hit = f.block();
    let carry_on = f.block();

    let none = f.is_none_object(object);
    f.branch(none, missing, search);

    f.at(missing);
    let zero = f.konst(0);
    f.ret_value(zero);

    f.at(search);
    let it = f.local("it", PTR);
    let head = load_state(&mut f, STATE_COMPONENTS);
    f.assign(it, head);
    f.jump(step);

    f.at(step);
    let exhausted = f.compare_const(CmpOp::Eq, it, 0);
    f.branch(exhausted, missing, compare);

    f.at(compare);
    let candidate = f.call_value(COMP_OBJECT_LOAD, &[it], MirType::ObjectRef);
    let same = f.compare(CmpOp::Eq, candidate, object);
    f.branch(same, hit, carry_on);

    f.at(hit);
    f.ret_value(it);

    f.at(carry_on);
    let following = f.load(it, COMP_NEXT);
    f.assign(it, following);
    f.jump(step);
    f.finish()
}

/// The component whose head is `coro`, for walking out along `origin`.
fn component_running_on() -> Function {
    let mut f = FunctionBuilder::returning(COMPONENT_ON, PTR);
    let coro = f.param("coro", PTR);
    let step = f.block();
    let check = f.block();
    let hit = f.block();
    let carry_on = f.block();
    let missing = f.block();

    let it = f.local("it", PTR);
    let head = load_state(&mut f, STATE_COMPONENTS);
    f.assign(it, head);
    f.jump(step);

    f.at(step);
    let exhausted = f.compare_const(CmpOp::Eq, it, 0);
    f.branch(exhausted, missing, check);

    f.at(missing);
    let zero = f.konst(0);
    f.ret_value(zero);

    f.at(check);
    let candidate = f.load(it, COMP_HEAD);
    let same = f.compare(CmpOp::Eq, candidate, coro);
    f.branch(same, hit, carry_on);

    f.at(hit);
    f.ret_value(it);

    f.at(carry_on);
    let following = f.load(it, COMP_NEXT);
    f.assign(it, following);
    f.jump(step);
    f.finish()
}

fn register() -> Function {
    let mut f = FunctionBuilder::new(REGISTER);
    let component = f.param("component", PTR);
    let head = load_state(&mut f, STATE_COMPONENTS);
    f.store(component, COMP_NEXT, head);
    store_state(&mut f, STATE_COMPONENTS, component);
    f.ret();
    f.finish()
}

/// Resolves an object reference to its component, rejecting the cases the
/// Standard calls errors before the operation is attempted.
fn require() -> Function {
    let mut f = FunctionBuilder::returning(REQUIRE, PTR);
    let object = f.param("object", MirType::ObjectRef);
    let component = f.call_value(COMPONENT_OF, &[object], PTR);
    let bad = f.block();
    let ok = f.block();
    let none = f.compare_const(CmpOp::Eq, component, 0);
    f.branch(none, bad, ok);
    f.at(bad);
    f.abort("sim: a sequencing statement named an object that never became a component");
    f.at(ok);
    f.ret_value(component);
    f.finish()
}

fn system_enter() -> Function {
    let mut f = FunctionBuilder::returning(SEQ_SYSTEM_ENTER, PTR);
    let block = f.param("block", MirType::I64);
    let system = f.alloc(SYS_SIZE);
    let current = load_state(&mut f, STATE_CURRENT);
    f.store(system, SYS_MAIN_PARK, current);
    f.store_const(system, SYS_OPERATIVE, 0);

    let frame = f.alloc(FRAME_SIZE);
    f.store(frame, FRAME_BLOCK, block);
    f.store(frame, FRAME_SYSTEM, system);
    f.store(frame, FRAME_OWNER, current);
    let outer = load_state(&mut f, STATE_SYSTEM_FRAMES);
    f.store(frame, FRAME_NEXT, outer);
    store_state(&mut f, STATE_SYSTEM_FRAMES, frame);
    f.ret_value(system);
    f.finish()
}

/// Blocks nest, but coroutines interleave, so the frame being left is not
/// necessarily the innermost one.
fn system_exit() -> Function {
    let mut f = FunctionBuilder::new(SEQ_SYSTEM_EXIT);
    let system = f.param("system", PTR);
    let prev = f.local("prev", PTR);
    let it = f.local("it", PTR);
    f.push(Op::ConstI64 {
        dest: prev,
        value: 0,
    });
    let head = load_state(&mut f, STATE_SYSTEM_FRAMES);
    f.assign(it, head);

    let step = f.block();
    let check = f.block();
    let unlink = f.block();
    let unlink_head = f.block();
    let unlink_mid = f.block();
    let carry_on = f.block();
    let done = f.block();
    f.jump(step);

    f.at(step);
    let exhausted = f.compare_const(CmpOp::Eq, it, 0);
    f.branch(exhausted, done, check);

    f.at(done);
    f.ret();

    f.at(check);
    let candidate = f.load(it, FRAME_SYSTEM);
    let same = f.compare(CmpOp::Eq, candidate, system);
    f.branch(same, unlink, carry_on);

    f.at(unlink);
    let following = f.load(it, FRAME_NEXT);
    let at_head = f.compare_const(CmpOp::Eq, prev, 0);
    f.branch(at_head, unlink_head, unlink_mid);

    f.at(unlink_head);
    store_state(&mut f, STATE_SYSTEM_FRAMES, following);
    f.ret();

    f.at(unlink_mid);
    f.store(prev, FRAME_NEXT, following);
    f.ret();

    f.at(carry_on);
    f.assign(prev, it);
    let following = f.load(it, FRAME_NEXT);
    f.assign(it, following);
    f.jump(step);
    f.finish()
}

/// The system of the outermost block instance, which chapter 11 makes a
/// prefixed block and 7.2 therefore makes "the outermost system".
fn outermost_system() -> Function {
    let mut f = FunctionBuilder::returning(OUTERMOST_SYSTEM, PTR);
    let existing = load_state(&mut f, STATE_OUTERMOST_SYSTEM);
    let make = f.block();
    let reuse = f.block();
    let absent = f.compare_const(CmpOp::Eq, existing, 0);
    f.branch(absent, make, reuse);

    f.at(reuse);
    f.ret_value(existing);

    f.at(make);
    let system = f.alloc(SYS_SIZE);
    let main_coro = load_state(&mut f, STATE_MAIN_CORO);
    f.store(system, SYS_MAIN_PARK, main_coro);
    f.store_const(system, SYS_OPERATIVE, 0);
    store_state(&mut f, STATE_OUTERMOST_SYSTEM, system);
    f.ret_value(system);
    f.finish()
}

/// The system of the instance of `block` this coroutine is executing inside.
/// Two objects of one class each running their own copy of an inner system head
/// produce frames with equal ids, and only the owner tells them apart.
fn system_for_block() -> Function {
    let mut f = FunctionBuilder::returning(SYSTEM_FOR_BLOCK, PTR);
    let block = f.param("block", MirType::I64);

    let independent = f.block();
    let search = f.block();
    let outward = f.block();
    let scan = f.block();
    let scan_check = f.block();
    let scan_next = f.block();
    let hit = f.block();
    let climb = f.block();
    let fallback = f.block();

    // A class declared in a class body or a procedure body has no system head,
    // so its objects can only ever be independent components (7.2).
    let none = f.compare_const(CmpOp::Eq, block, 0);
    f.branch(none, independent, search);

    f.at(independent);
    let zero = f.konst(0);
    f.ret_value(zero);

    f.at(search);
    let coro = f.local("coro", PTR);
    let frame = f.local("frame", PTR);
    let current = load_state(&mut f, STATE_CURRENT);
    f.assign(coro, current);
    f.jump(outward);

    f.at(outward);
    let exhausted = f.compare_const(CmpOp::Eq, coro, 0);
    f.branch(exhausted, fallback, scan);

    f.at(scan);
    let head = load_state(&mut f, STATE_SYSTEM_FRAMES);
    f.assign(frame, head);
    f.jump(scan_check);

    f.at(scan_check);
    let done = f.compare_const(CmpOp::Eq, frame, 0);
    f.branch(done, climb, scan_next);

    f.at(scan_next);
    let frame_block = f.load(frame, FRAME_BLOCK);
    let frame_owner = f.load(frame, FRAME_OWNER);
    let same_block = f.compare(CmpOp::Eq, frame_block, block);
    let same_owner = f.compare(CmpOp::Eq, frame_owner, coro);
    let both = f.binary(BinOp::And, same_block, same_owner);
    let following = f.block();
    f.branch(both, hit, following);

    f.at(hit);
    let system = f.load(frame, FRAME_SYSTEM);
    f.ret_value(system);

    f.at(following);
    let next_frame = f.load(frame, FRAME_NEXT);
    f.assign(frame, next_frame);
    f.jump(scan_check);

    // A class is always generated from within the block instance that declares
    // it, or from inside another object generated there, so following `origin`
    // walks outwards through the block instances whose systems are visible.
    f.at(climb);
    let component = f.call_value(COMPONENT_ON, &[coro], PTR);
    let unknown = f.compare_const(CmpOp::Eq, component, 0);
    let has_origin = f.block();
    f.branch(unknown, fallback, has_origin);

    f.at(has_origin);
    let origin = f.load(component, COMP_ORIGIN);
    f.assign(coro, origin);
    f.jump(outward);

    // Not instrumented (an injected system class, say): the outermost system
    // keeps such an object a component rather than failing outright.
    f.at(fallback);
    let system = f.call_value(OUTERMOST_SYSTEM, &[], PTR);
    f.ret_value(system);
    f.finish()
}

/// Creation is separate from starting because the body may suspend immediately,
/// and a `detach` down there must already be able to find its component.
fn object_create() -> Function {
    let mut f = FunctionBuilder::returning(SEQ_OBJECT_CREATE, PTR);
    let declaring_block = f.param("declaring_block", MirType::I64);
    let entry = f.param("entry", MirType::FuncRef);
    let object = f.param("object", MirType::ObjectRef);

    let component = f.alloc(COMP_SIZE);
    let gc_slot = f.call_value(SEQ_GC_SLOT_NEW, &[], MirType::I64);
    f.store(component, COMP_GC_SLOT, gc_slot);
    let head = f.call_value(CORO_CREATE, &[entry, object], PTR);
    f.store(component, COMP_HEAD, head);
    f.store(component, COMP_PARK, head);
    let current = load_state(&mut f, STATE_CURRENT);
    f.store(component, COMP_ORIGIN, current);
    let system = f.call_value(SYSTEM_FOR_BLOCK, &[declaring_block], PTR);
    f.store(component, COMP_SYSTEM, system);
    f.store_const(component, COMP_STATE, STATE_ATTACHED);
    f.call(COMP_OBJECT_STORE, &[component, object]);
    f.call(REGISTER, &[component]);
    f.ret_value(component);
    f.finish()
}

/// Runs the body attached to the generating block instance (7.1).
fn object_start() -> Function {
    let mut f = FunctionBuilder::new(SEQ_OBJECT_START);
    let component = f.param("component", PTR);
    let generator = load_state(&mut f, STATE_CURRENT);
    f.store(component, COMP_ATTACHED_TO, generator);
    let head = f.load(component, COMP_HEAD);
    f.call(CORO_SWITCH, &[generator, head]);
    f.ret();
    f.finish()
}

/// A prefixed block instance has a detach attribute without being an object
/// (7.3.1). It is registered only so a detach reaching it can be told apart
/// from one naming an object that never became a component.
fn block_instance() -> Function {
    let mut f = FunctionBuilder::new(SEQ_BLOCK_INSTANCE);
    let object = f.param("object", MirType::ObjectRef);
    let marker = f.alloc(COMP_SIZE);
    let gc_slot = f.call_value(SEQ_GC_SLOT_NEW, &[], MirType::I64);
    f.store(marker, COMP_GC_SLOT, gc_slot);
    f.call(COMP_OBJECT_STORE, &[marker, object]);
    f.store_const(marker, COMP_BLOCK_INSTANCE, 1);
    f.store_const(marker, COMP_STATE, STATE_ATTACHED);
    f.call(REGISTER, &[marker]);
    f.ret();
    f.finish()
}

/// Shared by detach (7.3.1) and by an object's final end (7.3.4), which "is the
/// same as that of a detach with respect to that object, except that the object
/// becomes terminated, not detached".
fn leave() -> Function {
    let mut f = FunctionBuilder::new(LEAVE);
    let component = f.param("component", PTR);
    let next_state = f.param("next_state", MirType::I64);

    let current = load_state(&mut f, STATE_CURRENT);
    let target = f.local("target", PTR);
    let state = f.load(component, COMP_STATE);

    let attached = f.block();
    let resumed = f.block();
    let bad = f.block();
    let switch = f.block();

    let is_attached = f.compare_const(CmpOp::Eq, state, STATE_ATTACHED);
    let not_attached = f.block();
    f.branch(is_attached, attached, not_attached);

    f.at(not_attached);
    let is_resumed = f.compare_const(CmpOp::Eq, state, STATE_RESUMED);
    f.branch(is_resumed, resumed, bad);

    f.at(bad);
    f.abort("sim: detach with respect to an object that is already detached or terminated");

    // 7.3.1 case 1: control returns to the block instance the object is
    // attached to, immediately after the generator or call statement.
    f.at(attached);
    let attached_to = f.load(component, COMP_ATTACHED_TO);
    f.assign(target, attached_to);
    f.jump(switch);

    // 7.3.1 case 3: control goes to the reactivation point of the main
    // component of the object's system, which thereby becomes operative --
    // not to whoever resumed this object.
    f.at(resumed);
    let system = f.load(component, COMP_SYSTEM);
    let no_system = f.compare_const(CmpOp::Eq, system, 0);
    let has_system = f.block();
    f.branch(no_system, bad, has_system);
    f.at(has_system);
    f.store_const(system, SYS_OPERATIVE, 0);
    let main_park = f.load(system, SYS_MAIN_PARK);
    f.assign(target, main_park);
    f.jump(switch);

    f.at(switch);
    f.store(component, COMP_STATE, next_state);
    // A terminated object "attains no reactivation point and loses its status
    // as a component head".
    let terminating = f.compare_const(CmpOp::Eq, next_state, STATE_TERMINATED);
    let ends = f.block();
    let parks = f.block();
    let go = f.block();
    f.branch(terminating, ends, parks);
    f.at(ends);
    f.store_const(component, COMP_PARK, 0);
    f.jump(go);
    f.at(parks);
    f.store(component, COMP_PARK, current);
    f.jump(go);
    f.at(go);
    f.call(CORO_SWITCH, &[current, target]);
    f.ret();
    f.finish()
}

fn detach() -> Function {
    let mut f = FunctionBuilder::new(SEQ_DETACH);
    let object = f.param("object", MirType::ObjectRef);
    let component = f.call_value(REQUIRE, &[object], PTR);
    let marker = f.load(component, COMP_BLOCK_INSTANCE);
    let no_effect = f.block();
    let leaves = f.block();
    // 7.3.1: "If X is an instance of a prefixed block the detach statement has
    // no effect."
    let is_marker = f.compare_const(CmpOp::Ne, marker, 0);
    f.branch(is_marker, no_effect, leaves);
    f.at(no_effect);
    f.ret();
    f.at(leaves);
    let detached = f.konst(STATE_DETACHED);
    f.call(LEAVE, &[component, detached]);
    f.ret();
    f.finish()
}

fn terminate() -> Function {
    let mut f = FunctionBuilder::new(SEQ_TERMINATE);
    let object = f.param("object", MirType::ObjectRef);
    let component = f.call_value(REQUIRE, &[object], PTR);
    let terminated = f.konst(STATE_TERMINATED);
    f.call(LEAVE, &[component, terminated]);
    f.ret();
    f.finish()
}

/// Final end of `self` composed with resume of `target` (Ch.12 terminate-current).
fn terminate_resuming() -> Function {
    let mut f = FunctionBuilder::new(SEQ_TERMINATE_RESUMING);
    let self_object = f.param("self_object", MirType::ObjectRef);
    let target_object = f.param("target_object", MirType::ObjectRef);
    let self_component = f.call_value(REQUIRE, &[self_object], PTR);
    let target_component = f.call_value(REQUIRE, &[target_object], PTR);

    let bad = f.block();
    let live = f.block();
    let go = f.block();
    let clear = f.block();
    let set_target = f.block();

    let target_state = f.load(target_component, COMP_STATE);
    let detached = f.compare_const(CmpOp::Eq, target_state, STATE_DETACHED);
    f.branch(detached, live, bad);

    f.at(bad);
    f.abort("sim: scheduling an object that is not detached; a detached object is required");

    f.at(live);
    let system = f.load(target_component, COMP_SYSTEM);
    let no_system = f.compare_const(CmpOp::Eq, system, 0);
    f.branch(no_system, bad, go);

    f.at(go);
    f.store_const(self_component, COMP_STATE, STATE_TERMINATED);
    f.store_const(self_component, COMP_PARK, 0);
    let operative = f.load(system, SYS_OPERATIVE);
    let was_self = f.compare(CmpOp::Eq, operative, self_component);
    f.branch(was_self, clear, set_target);

    f.at(clear);
    f.store_const(system, SYS_OPERATIVE, 0);
    f.jump(set_target);

    f.at(set_target);
    f.store(system, SYS_OPERATIVE, target_component);
    f.store_const(target_component, COMP_STATE, STATE_RESUMED);
    let current = load_state(&mut f, STATE_CURRENT);
    let park = f.load(target_component, COMP_PARK);
    f.call(CORO_SWITCH, &[current, park]);
    f.ret();
    f.finish()
}

fn call() -> Function {
    let mut f = FunctionBuilder::new(SEQ_CALL);
    let object = f.param("object", MirType::ObjectRef);
    let target = f.call_value(REQUIRE, &[object], PTR);
    let state = f.load(target, COMP_STATE);
    let bad = f.block();
    let ok = f.block();
    let detached = f.compare_const(CmpOp::Eq, state, STATE_DETACHED);
    f.branch(detached, ok, bad);

    f.at(bad);
    f.abort("sim: 7.3.2 requires call to name a detached object");

    // The callee "becomes attached to the block instance containing the call
    // statement, whereby Y loses its status as a component head".
    f.at(ok);
    let current = load_state(&mut f, STATE_CURRENT);
    f.store_const(target, COMP_STATE, STATE_ATTACHED);
    f.store(target, COMP_ATTACHED_TO, current);
    let park = f.load(target, COMP_PARK);
    f.call(CORO_SWITCH, &[current, park]);
    f.ret();
    f.finish()
}

fn resume() -> Function {
    let mut f = FunctionBuilder::new(SEQ_RESUME);
    let object = f.param("object", MirType::ObjectRef);
    let target = f.call_value(REQUIRE, &[object], PTR);

    let system = f.load(target, COMP_SYSTEM);
    let not_local = f.block();
    let local = f.block();
    let absent = f.compare_const(CmpOp::Eq, system, 0);
    f.branch(absent, not_local, local);

    f.at(not_local);
    f.abort(
        "sim: 7.3.3 allows resume only for an object of a class declared in a subblock or \
         prefixed block",
    );

    f.at(local);
    let state = f.load(target, COMP_STATE);
    let no_effect = f.block();
    let live = f.block();
    // "If Y is a resumed object, the resume statement has no effect."
    let already = f.compare_const(CmpOp::Eq, state, STATE_RESUMED);
    f.branch(already, no_effect, live);

    f.at(no_effect);
    f.ret();

    f.at(live);
    let bad = f.block();
    let ok = f.block();
    let detached = f.compare_const(CmpOp::Eq, state, STATE_DETACHED);
    f.branch(detached, ok, bad);

    f.at(bad);
    f.abort("sim: 7.3.3 requires resume to name a detached object");

    f.at(ok);
    let current = load_state(&mut f, STATE_CURRENT);
    let operative = f.load(system, SYS_OPERATIVE);
    let main_was = f.block();
    let other_was = f.block();
    let switch = f.block();
    // The previously operative component becomes non-operative, with its
    // reactivation point immediately after the resume statement.
    let none = f.compare_const(CmpOp::Eq, operative, 0);
    f.branch(none, main_was, other_was);

    f.at(main_was);
    f.store(system, SYS_MAIN_PARK, current);
    f.jump(switch);

    f.at(other_was);
    f.store_const(operative, COMP_STATE, STATE_DETACHED);
    f.store(operative, COMP_PARK, current);
    f.jump(switch);

    f.at(switch);
    f.store(system, SYS_OPERATIVE, target);
    f.store_const(target, COMP_STATE, STATE_RESUMED);
    let park = f.load(target, COMP_PARK);
    f.call(CORO_SWITCH, &[current, park]);
    f.ret();
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_static_state_fits_the_reserved_block() {
        const { assert!(STATE_MAIN_CORO + 8 <= STATE_BYTES) };
    }

    #[test]
    fn every_synthesized_function_has_a_distinct_name() {
        let functions = functions();
        let names = function_names();
        assert_eq!(functions.len(), names.len());
    }

    /// The transform emits calls to these by name, so a rename that misses one
    /// has to fail here rather than in the backend.
    #[test]
    fn the_names_the_transform_uses_are_all_defined() {
        let names = function_names();
        for name in [
            START,
            PHASE_IS_UNWINDING,
            PHASE_IS_REWINDING,
            FRAME_PUSH,
            FRAME_POP,
            SPILL_STORE_REF,
            SPILL_LOAD_REF,
            FRAMES_GROW,
            REFS_GROW,
            REFS_CREATE,
            SEQ_GC_SLOT_NEW,
            CORO_ARG_STORE,
            CORO_ARG_LOAD,
            COMP_OBJECT_STORE,
            COMP_OBJECT_LOAD,
            SEQ_SYSTEM_ENTER,
            SEQ_SYSTEM_EXIT,
            SEQ_OBJECT_CREATE,
            SEQ_OBJECT_START,
            SEQ_BLOCK_INSTANCE,
            SEQ_DETACH,
            SEQ_CALL,
            SEQ_RESUME,
            SEQ_TERMINATE,
            SEQ_TERMINATE_RESUMING,
        ] {
            assert!(names.contains(name), "{name} is not synthesized");
        }
    }
}
