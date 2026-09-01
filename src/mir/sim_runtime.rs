//! Simulation / SQS for targets with no native scheduler (WebAssembly).
//!
//! Mirrors `runtime/runtime.c`'s `g_sim` state machine, emitted as MIR over the
//! same spill-buffer sequencing primitives as [`super::seq_runtime`].
//!
//! Two properties of the MIR these functions are written in matter throughout:
//! a block that runs off its end falls into the block declared after it, and a
//! block's ops stop at its first terminator. Every block below therefore ends
//! in an explicit jump, branch, return or abort.

use std::collections::HashSet;

use crate::layout::{SIM_MAIN_CLASS_ID, SIM_MAIN_SIZE};

use super::build::{FunctionBuilder, PTR};
use super::seq_runtime::{self, SEQ_DETACH, SEQ_RESUME, SEQ_TERMINATE, SEQ_TERMINATE_RESUMING};
use super::{BinOp, CmpOp, Function, LocalId, MirType, Op};

/// Fixed linear-memory header for simulation state. Sits immediately after the
/// chapter-7 sequencing static block; see `codegen::wasm::TEXT_BASE`.
pub const SIM_STATE_BASE: i64 = seq_runtime::STATE_BASE + seq_runtime::STATE_BYTES;
pub const SIM_STATE_BYTES: i64 = 64;

const SIM_OFF_ACTIVE: i64 = 0;
/// The three process references in the simulation state. Read and written
/// **only** through the [`SIM_CURRENT_STORE`] family, because on wasm they do
/// not live here at all — a Simula reference has no linear-memory
/// representation under WasmGC, so codegen keeps them in reference globals.
const SIM_OFF_CURRENT: i64 = 8;
const SIM_OFF_RUNNING: i64 = 16;
const SIM_OFF_SQS: i64 = 24;
const SIM_OFF_LEN: i64 = 32;
const SIM_OFF_CAP: i64 = 40;
const SIM_OFF_NEXT_SEQ: i64 = 48;
const SIM_OFF_MAIN: i64 = 56;

/// `evtime` is a binary64 stored through the i64 field ops, which reinterpret
/// rather than convert when the value's local is typed `f64`.
const NOTICE_EVTIME: i64 = 0;
/// The scheduled process. Reached **only** through
/// [`SIM_NOTICE_PROCESS_STORE`] / [`SIM_NOTICE_PROCESS_LOAD`], by notice
/// index rather than by address: on wasm the column lives in a host-traced
/// array beside the linear set, not in this word.
const NOTICE_PROCESS: i64 = 8;
const NOTICE_SEQ: i64 = 16;
const NOTICE_SIZE: i64 = 24;

const SQS_INITIAL_CAP: i64 = 16;
const SQS_MAX_LEN: i64 = 65536;

const COMPONENT_OF: &str = "__simrt_seq_component_of";
const COMP_STATE: i64 = 40;
const STATE_TERMINATED: i64 = 3;

pub const SIM_BEGIN: &str = "__simrt_sim_begin";
pub const SIM_END: &str = "__simrt_sim_end";
pub const SIM_HOLD: &str = "__simrt_sim_hold";
pub const SIM_ACTIVATE_DIRECT: &str = "__simrt_sim_activate_direct";
pub const SIM_ACTIVATE_TIMED: &str = "__simrt_sim_activate_timed";
pub const SIM_ACTIVATE_RELATIVE: &str = "__simrt_sim_activate_relative";
pub const SIM_PASSIVATE: &str = "__simrt_sim_passivate";
pub const SIM_TRANSFER_TO_HEAD: &str = "__simrt_sim_transfer_to_head";
pub const SIM_TERMINATE_CURRENT: &str = "__simrt_sim_terminate_current";
pub const SIM_CANCEL: &str = "__simrt_sim_cancel";
pub const SIM_FINISH_MAIN: &str = "__simrt_sim_finish_main";
pub const SIM_TIME: &str = "__simrt_sim_time";
pub const SIM_IS_MAIN_CURRENT: &str = "__simrt_sim_is_main_current";
pub const SIM_HAS_CURRENT: &str = "__simrt_sim_has_current";
pub const SIM_CURRENT: &str = "__simrt_sim_current";
pub const SIM_MAIN: &str = "__simrt_sim_main";
pub const SIM_IDLE: &str = "__simrt_sim_idle";
pub const SIM_TERMINATED: &str = "__simrt_sim_terminated";
pub const SIM_EVTIME: &str = "__simrt_sim_evtime";
pub const SIM_NEXTEV: &str = "__simrt_sim_nextev";

/// Accessors for the four places chapter 12 keeps a Simula reference
/// (Phase 4-R4 seams).
///
/// The MIR bodies below read and write the linear simulation state, which is
/// what the interpreter and native want. Wasm replaces the bodies wholesale
/// — CURRENT / RUNNING / MAIN become reference globals and the SQS's process
/// column becomes a WasmGC array — exactly as `seq_runtime`'s
/// [`CORO_ARG_STORE`](super::seq_runtime::CORO_ARG_STORE) family does for the
/// chapter 7 records. Routing every access through a named function is the
/// point: there is then exactly one body to replace per location, and no
/// `FieldStoreI64` of an `ObjectRef` left in the simulation state.
pub const SIM_CURRENT_STORE: &str = "__simrt_sim_current_store";
pub const SIM_CURRENT_LOAD: &str = "__simrt_sim_current_load";
pub const SIM_RUNNING_STORE: &str = "__simrt_sim_running_store";
pub const SIM_RUNNING_LOAD: &str = "__simrt_sim_running_load";
pub const SIM_MAIN_STORE: &str = "__simrt_sim_main_store";
pub const SIM_MAIN_LOAD: &str = "__simrt_sim_main_load";
/// `sqs[index].process := value`, growing the process column as needed.
pub const SIM_NOTICE_PROCESS_STORE: &str = "__simrt_sim_notice_process_store";
/// `return sqs[index].process` (`none` past the end).
pub const SIM_NOTICE_PROCESS_LOAD: &str = "__simrt_sim_notice_process_load";

const ENSURE_ACTIVE: &str = "__simrt_sim_ensure_active";
const INSERT_EVENT: &str = "__simrt_sim_insert_event";
const CANCEL_UNLOCKED: &str = "__simrt_sim_cancel_unlocked";
const ADVANCE_CURRENT: &str = "__simrt_sim_advance_current";
const GROW: &str = "__simrt_sim_grow";
const IS_SCHEDULED: &str = "__simrt_sim_is_scheduled";
const RUNNING_OR_MAIN: &str = "__simrt_sim_running_or_main";
const HEAD: &str = "__simrt_sim_head";
const NEXT_SEQ: &str = "__simrt_sim_next_seq";

pub fn function_names() -> HashSet<String> {
    functions()
        .into_iter()
        .map(|function| function.name)
        .collect()
}

pub fn functions() -> Vec<Function> {
    vec![
        sim_ref_store(SIM_CURRENT_STORE, SIM_OFF_CURRENT),
        sim_ref_load(SIM_CURRENT_LOAD, SIM_OFF_CURRENT),
        sim_ref_store(SIM_RUNNING_STORE, SIM_OFF_RUNNING),
        sim_ref_load(SIM_RUNNING_LOAD, SIM_OFF_RUNNING),
        sim_ref_store(SIM_MAIN_STORE, SIM_OFF_MAIN),
        sim_ref_load(SIM_MAIN_LOAD, SIM_OFF_MAIN),
        notice_process_store(),
        notice_process_load(),
        ensure_active(),
        next_seq(),
        grow(),
        cancel_unlocked(),
        insert_event(),
        advance_current(),
        is_scheduled(),
        running_or_main(),
        head(),
        sim_begin(),
        sim_end(),
        sim_hold(),
        sim_activate_direct(),
        sim_activate_timed(),
        sim_activate_relative(),
        sim_passivate(),
        sim_transfer_to_head(),
        sim_terminate_current(),
        sim_cancel(),
        sim_finish_main(),
        sim_time(),
        sim_is_main_current(),
        sim_has_current(),
        sim_current(),
        sim_main(),
        sim_idle(),
        sim_terminated(),
        sim_evtime(),
        sim_nextev(),
    ]
}

fn load_state(f: &mut FunctionBuilder, offset: i64) -> LocalId {
    let base = f.konst(SIM_STATE_BASE);
    f.load(base, offset)
}

fn store_state(f: &mut FunctionBuilder, offset: i64, value: LocalId) {
    let base = f.konst(SIM_STATE_BASE);
    f.store(base, offset, value);
}

fn store_state_const(f: &mut FunctionBuilder, offset: i64, value: i64) {
    let bits = f.konst(value);
    store_state(f, offset, bits);
}

/// Linear-memory body of one of the CURRENT / RUNNING / MAIN stores.
fn sim_ref_store(name: &str, offset: i64) -> Function {
    let mut f = FunctionBuilder::new(name);
    let value = f.param("value", MirType::ObjectRef);
    store_state(&mut f, offset, value);
    f.ret();
    f.finish()
}

/// Linear-memory body of one of the CURRENT / RUNNING / MAIN loads.
fn sim_ref_load(name: &str, offset: i64) -> Function {
    let mut f = FunctionBuilder::returning(name, MirType::ObjectRef);
    let base = f.konst(SIM_STATE_BASE);
    let value = f.load_object(base, offset);
    f.ret_value(value);
    f.finish()
}

fn notice_process_store() -> Function {
    let mut f = FunctionBuilder::new(SIM_NOTICE_PROCESS_STORE);
    let index = f.param("index", MirType::I64);
    let value = f.param("process", MirType::ObjectRef);
    let sqs = load_state(&mut f, SIM_OFF_SQS);
    let notice = notice_at(&mut f, sqs, index);
    f.store(notice, NOTICE_PROCESS, value);
    f.ret();
    f.finish()
}

fn notice_process_load() -> Function {
    let mut f = FunctionBuilder::returning(SIM_NOTICE_PROCESS_LOAD, MirType::ObjectRef);
    let index = f.param("index", MirType::I64);
    let sqs = load_state(&mut f, SIM_OFF_SQS);
    let notice = notice_at(&mut f, sqs, index);
    let value = f.load_object(notice, NOTICE_PROCESS);
    f.ret_value(value);
    f.finish()
}

fn store_ref(f: &mut FunctionBuilder, name: &str, value: LocalId) {
    f.call(name, &[value]);
}

fn store_none(f: &mut FunctionBuilder, name: &str) {
    let none = f.none_object();
    f.call(name, &[none]);
}

fn load_ref(f: &mut FunctionBuilder, name: &str) -> LocalId {
    f.call_value(name, &[], MirType::ObjectRef)
}

fn store_notice_process(f: &mut FunctionBuilder, index: LocalId, value: LocalId) {
    f.call(SIM_NOTICE_PROCESS_STORE, &[index, value]);
}

fn load_notice_process(f: &mut FunctionBuilder, index: LocalId) -> LocalId {
    f.call_value(SIM_NOTICE_PROCESS_LOAD, &[index], MirType::ObjectRef)
}

/// The notice index 0 constant the head-of-set accessors want.
fn first_index(f: &mut FunctionBuilder) -> LocalId {
    f.konst(0)
}

/// Drops the three process references a Simulation holds. Separate from the
/// scalar zero-fill in [`sim_begin`] / [`sim_end`] because a reference slot is
/// cleared by writing `none`, not by writing the integer 0.
fn clear_process_refs(f: &mut FunctionBuilder) {
    for name in [SIM_CURRENT_STORE, SIM_RUNNING_STORE, SIM_MAIN_STORE] {
        store_none(f, name);
    }
}

fn f64_zero(f: &mut FunctionBuilder) -> LocalId {
    let dest = f.local("z", MirType::F64);
    f.push(Op::ConstF64 { dest, value: 0.0 });
    dest
}

/// Reads an event time back as the binary64 it was written as. The field ops
/// reinterpret when the local is a float, so this must not go through
/// [`Op::I64ToF64`], which converts (and, in the wasm backend, floors).
fn load_f64(f: &mut FunctionBuilder, base: LocalId, offset: i64) -> LocalId {
    let dest = f.local("ev", MirType::F64);
    f.push(Op::FieldLoadI64 {
        dest,
        object: base,
        offset,
        class_qual: None,
    });
    dest
}

fn notice_at(f: &mut FunctionBuilder, sqs: LocalId, index: LocalId) -> LocalId {
    let stride = f.konst(NOTICE_SIZE);
    let off = f.binary(BinOp::Mul, index, stride);
    f.binary(BinOp::Add, sqs, off)
}

/// Copies notice `src_index` over notice `dst_index`.
///
/// `evtime`/`seq` are plain words and are copied as such. `process` goes
/// through the accessors by index, because the column it lives in is not
/// addressable on every backend — the same reason [`NOTICE_PROCESS`] says so.
fn copy_notice(f: &mut FunctionBuilder, sqs: LocalId, src_index: LocalId, dst_index: LocalId) {
    let src = notice_at(f, sqs, src_index);
    let dst = notice_at(f, sqs, dst_index);
    for offset in [NOTICE_EVTIME, NOTICE_SEQ] {
        let word = f.load(src, offset);
        f.store(dst, offset, word);
    }
    let process = load_notice_process(f, src_index);
    store_notice_process(f, dst_index, process);
}

fn ensure_active() -> Function {
    let mut f = FunctionBuilder::new(ENSURE_ACTIVE);
    let ok = f.block();
    let bad = f.block();
    let active = load_state(&mut f, SIM_OFF_ACTIVE);
    let live = f.compare_const(CmpOp::Ne, active, 0);
    f.branch(live, ok, bad);
    f.at(bad);
    f.abort("hold/activate/time requires an active Simulation");
    f.at(ok);
    f.ret();
    f.finish()
}

fn next_seq() -> Function {
    let mut f = FunctionBuilder::returning(NEXT_SEQ, MirType::I64);
    let seq = load_state(&mut f, SIM_OFF_NEXT_SEQ);
    let next = f.add_const(seq, 1);
    store_state(&mut f, SIM_OFF_NEXT_SEQ, next);
    f.ret_value(seq);
    f.finish()
}

fn grow() -> Function {
    let mut f = FunctionBuilder::new(GROW);
    let bad = f.block();
    let pick = f.block();
    let initial = f.block();
    let double = f.block();
    let clamp = f.block();
    let use_doubled = f.block();
    let store = f.block();

    let cap = load_state(&mut f, SIM_OFF_CAP);
    let full = f.compare_const(CmpOp::Ge, cap, SQS_MAX_LEN);
    f.branch(full, bad, pick);

    f.at(bad);
    f.abort("SQS length limit exceeded");

    let new_cap = f.local("new_cap", MirType::I64);
    f.at(pick);
    let empty = f.compare_const(CmpOp::Eq, cap, 0);
    f.branch(empty, initial, double);

    f.at(initial);
    f.push(Op::ConstI64 {
        dest: new_cap,
        value: SQS_INITIAL_CAP,
    });
    f.jump(store);

    f.at(double);
    let doubled = f.mul_const(cap, 2);
    let over = f.compare_const(CmpOp::Gt, doubled, SQS_MAX_LEN);
    f.branch(over, clamp, use_doubled);

    f.at(clamp);
    f.push(Op::ConstI64 {
        dest: new_cap,
        value: SQS_MAX_LEN,
    });
    f.jump(store);

    f.at(use_doubled);
    f.assign(new_cap, doubled);
    f.jump(store);

    f.at(store);
    store_state(&mut f, SIM_OFF_CAP, new_cap);
    f.ret();
    f.finish()
}

/// Compacts every notice naming `process` out of the SQS.
fn cancel_unlocked() -> Function {
    let mut f = FunctionBuilder::new(CANCEL_UNLOCKED);
    let process = f.param("process", MirType::ObjectRef);
    let loop_head = f.block();
    let loop_body = f.block();
    let keep = f.block();
    let advance = f.block();
    let loop_done = f.block();

    let sqs = load_state(&mut f, SIM_OFF_SQS);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let out = f.local("out", MirType::I64);
    let i = f.local("i", MirType::I64);
    f.push(Op::ConstI64 {
        dest: out,
        value: 0,
    });
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.jump(loop_head);

    f.at(loop_head);
    let done = f.compare(CmpOp::Ge, i, len);
    f.branch(done, loop_done, loop_body);

    f.at(loop_body);
    let cancelled = {
        let named = load_notice_process(&mut f, i);
        f.compare(CmpOp::Eq, named, process)
    };
    f.branch(cancelled, advance, keep);

    f.at(keep);
    copy_notice(&mut f, sqs, i, out);
    let kept = f.add_const(out, 1);
    f.assign(out, kept);
    f.jump(advance);

    f.at(advance);
    let next = f.add_const(i, 1);
    f.assign(i, next);
    f.jump(loop_head);

    f.at(loop_done);
    store_state(&mut f, SIM_OFF_LEN, out);
    f.ret();
    f.finish()
}

/// Files a notice for `process` at `evtime`, replacing any it already has.
/// `prior != 0` places it ahead of the notices already holding that time.
fn insert_event() -> Function {
    let mut f = FunctionBuilder::new(INSERT_EVENT);
    let evtime = f.param("evtime", MirType::F64);
    let process = f.param("process", MirType::ObjectRef);
    let prior = f.param("prior", MirType::I64);

    let bad = f.block();
    let room = f.block();
    let grow_block = f.block();
    let find = f.block();
    let scan = f.block();
    let scan_check = f.block();
    let prior_yes = f.block();
    let prior_no = f.block();
    let found = f.block();
    let scan_next = f.block();
    let scan_done = f.block();
    let shift = f.block();
    let shift_body = f.block();
    let shift_done = f.block();

    f.call(CANCEL_UNLOCKED, &[process]);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let over = f.compare_const(CmpOp::Ge, len, SQS_MAX_LEN);
    f.branch(over, bad, room);

    f.at(bad);
    f.abort("SQS length limit exceeded");

    f.at(room);
    let cap = load_state(&mut f, SIM_OFF_CAP);
    let full = f.compare(CmpOp::Ge, len, cap);
    f.branch(full, grow_block, find);

    f.at(grow_block);
    f.call(GROW, &[]);
    f.jump(find);

    f.at(find);
    let sqs = load_state(&mut f, SIM_OFF_SQS);
    let idx = f.local("idx", MirType::I64);
    f.assign(idx, len);
    let i = f.local("i", MirType::I64);
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.jump(scan);

    f.at(scan);
    let exhausted = f.compare(CmpOp::Ge, i, len);
    f.branch(exhausted, scan_done, scan_check);

    f.at(scan_check);
    let notice = notice_at(&mut f, sqs, i);
    let ev = load_f64(&mut f, notice, NOTICE_EVTIME);
    let ahead = f.compare_const(CmpOp::Ne, prior, 0);
    f.branch(ahead, prior_yes, prior_no);

    f.at(prior_yes);
    let at_or_after = f.compare(CmpOp::Ge, ev, evtime);
    f.branch(at_or_after, found, scan_next);

    f.at(prior_no);
    let after = f.compare(CmpOp::Gt, ev, evtime);
    f.branch(after, found, scan_next);

    f.at(found);
    f.assign(idx, i);
    f.jump(scan_done);

    f.at(scan_next);
    let next = f.add_const(i, 1);
    f.assign(i, next);
    f.jump(scan);

    f.at(scan_done);
    let j = f.local("j", MirType::I64);
    f.assign(j, len);
    f.jump(shift);

    f.at(shift);
    let settled = f.compare(CmpOp::Le, j, idx);
    f.branch(settled, shift_done, shift_body);

    f.at(shift_body);
    let prev = f.add_const(j, -1);
    copy_notice(&mut f, sqs, prev, j);
    let down = f.add_const(j, -1);
    f.assign(j, down);
    f.jump(shift);

    f.at(shift_done);
    let slot = notice_at(&mut f, sqs, idx);
    f.store(slot, NOTICE_EVTIME, evtime);
    store_notice_process(&mut f, idx, process);
    let seq = f.call_value(NEXT_SEQ, &[], MirType::I64);
    f.store(slot, NOTICE_SEQ, seq);
    let grown = f.add_const(len, 1);
    store_state(&mut f, SIM_OFF_LEN, grown);
    f.ret();
    f.finish()
}

fn advance_current() -> Function {
    let mut f = FunctionBuilder::new(ADVANCE_CURRENT);
    let clear = f.block();
    let set = f.block();
    let len = load_state(&mut f, SIM_OFF_LEN);
    let empty = f.compare_const(CmpOp::Eq, len, 0);
    f.branch(empty, clear, set);

    f.at(clear);
    store_none(&mut f, SIM_CURRENT_STORE);
    f.ret();

    f.at(set);
    let zero = first_index(&mut f);
    let first = load_notice_process(&mut f, zero);
    store_ref(&mut f, SIM_CURRENT_STORE, first);
    f.ret();
    f.finish()
}

fn is_scheduled() -> Function {
    let mut f = FunctionBuilder::returning(IS_SCHEDULED, MirType::I64);
    let process = f.param("process", MirType::ObjectRef);
    let loop_head = f.block();
    let loop_body = f.block();
    let advance = f.block();
    let found = f.block();
    let not_found = f.block();

    let len = load_state(&mut f, SIM_OFF_LEN);
    let i = f.local("i", MirType::I64);
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.jump(loop_head);

    f.at(loop_head);
    let exhausted = f.compare(CmpOp::Ge, i, len);
    f.branch(exhausted, not_found, loop_body);

    f.at(loop_body);
    let named = load_notice_process(&mut f, i);
    let same = f.compare(CmpOp::Eq, named, process);
    f.branch(same, found, advance);

    f.at(advance);
    let next = f.add_const(i, 1);
    f.assign(i, next);
    f.jump(loop_head);

    f.at(found);
    let yes = f.konst(1);
    f.ret_value(yes);

    f.at(not_found);
    let no = f.konst(0);
    f.ret_value(no);
    f.finish()
}

/// The component physically executing; MAIN stands in for "none yet".
fn running_or_main() -> Function {
    let mut f = FunctionBuilder::returning(RUNNING_OR_MAIN, MirType::ObjectRef);
    let use_running = f.block();
    let use_main = f.block();
    let running = load_ref(&mut f, SIM_RUNNING_LOAD);
    let unset = f.is_none_object(running);
    f.branch(unset, use_main, use_running);

    f.at(use_running);
    f.ret_value(running);

    f.at(use_main);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    f.ret_value(main);
    f.finish()
}

fn head() -> Function {
    let mut f = FunctionBuilder::returning(HEAD, MirType::ObjectRef);
    let none = f.block();
    let some = f.block();
    let len = load_state(&mut f, SIM_OFF_LEN);
    let empty = f.compare_const(CmpOp::Eq, len, 0);
    f.branch(empty, none, some);

    f.at(none);
    let nothing = f.none_object();
    f.ret_value(nothing);

    f.at(some);
    let zero = first_index(&mut f);
    let first = load_notice_process(&mut f, zero);
    f.ret_value(first);
    f.finish()
}

fn sim_begin() -> Function {
    let mut f = FunctionBuilder::new(SIM_BEGIN);
    let ok = f.block();
    let bad = f.block();
    let active = load_state(&mut f, SIM_OFF_ACTIVE);
    let nested = f.compare_const(CmpOp::Ne, active, 0);
    f.branch(nested, bad, ok);

    f.at(bad);
    f.abort("nested Simulation is not supported");

    f.at(ok);
    for off in [
        SIM_OFF_ACTIVE,
        SIM_OFF_SQS,
        SIM_OFF_LEN,
        SIM_OFF_CAP,
        SIM_OFF_NEXT_SEQ,
    ] {
        store_state_const(&mut f, off, 0);
    }
    clear_process_refs(&mut f);
    store_state_const(&mut f, SIM_OFF_NEXT_SEQ, 1);
    store_state_const(&mut f, SIM_OFF_ACTIVE, 1);
    // No realloc here, so the set is sized for the limit up front and `cap`
    // only tracks how much of it the C runtime would have taken.
    let sqs_buf = f.alloc(SQS_MAX_LEN * NOTICE_SIZE);
    store_state(&mut f, SIM_OFF_SQS, sqs_buf);
    store_state_const(&mut f, SIM_OFF_CAP, SQS_INITIAL_CAP);
    // MAIN is a marker: nothing dereferences it and no class declares it, so
    // [`crate::layout::SIM_MAIN_CLASS_NAME`] is header-only. It is still an
    // `ObjectRef` rather than a bare [`FunctionBuilder::alloc`] record, because
    // the three process slots and the SQS process column it flows into are
    // reference-typed. Passing it to the accessor (rather than storing the
    // word) is what keeps every *reader* of MAIN seeing one stable value.
    let main = f.local("main", MirType::ObjectRef);
    f.push(Op::NewObject {
        dest: main,
        class_id: SIM_MAIN_CLASS_ID,
        size: SIM_MAIN_SIZE,
    });
    store_ref(&mut f, SIM_MAIN_STORE, main);
    let zero_time = f64_zero(&mut f);
    let prior = f.konst(1);
    f.call(INSERT_EVENT, &[zero_time, main, prior]);
    store_ref(&mut f, SIM_CURRENT_STORE, main);
    store_ref(&mut f, SIM_RUNNING_STORE, main);
    f.ret();
    f.finish()
}

fn sim_end() -> Function {
    let mut f = FunctionBuilder::new(SIM_END);
    for off in [
        SIM_OFF_ACTIVE,
        SIM_OFF_SQS,
        SIM_OFF_LEN,
        SIM_OFF_CAP,
        SIM_OFF_NEXT_SEQ,
    ] {
        store_state_const(&mut f, off, 0);
    }
    clear_process_refs(&mut f);
    f.ret();
    f.finish()
}

/// 12.3 reschedules the process executing the hold, which is not always the
/// head of the set: an `activate ... prior` can file a notice ahead of it
/// without taking the PSC away.
fn sim_hold() -> Function {
    let mut f = FunctionBuilder::new(SIM_HOLD);
    let dt = f.param("dt", MirType::F64);
    let use_dt = f.block();
    let use_zero = f.block();
    let merged = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let self_process = f.call_value(RUNNING_OR_MAIN, &[], MirType::ObjectRef);
    let now = f.call_value(SIM_TIME, &[], MirType::F64);
    let zero = f64_zero(&mut f);
    let delay = f.local("delay", MirType::F64);
    let forward = f.compare(CmpOp::Ge, dt, zero);
    f.branch(forward, use_dt, use_zero);

    f.at(use_dt);
    f.assign(delay, dt);
    f.jump(merged);

    f.at(use_zero);
    f.assign(delay, zero);
    f.jump(merged);

    f.at(merged);
    let at = f.local("at", MirType::F64);
    f.push(Op::Binary {
        dest: at,
        op: BinOp::Add,
        left: now,
        right: delay,
    });
    let prior = f.konst(0);
    f.call(INSERT_EVENT, &[at, self_process, prior]);
    f.call(ADVANCE_CURRENT, &[]);
    f.ret();
    f.finish()
}

/// 12.2 direct activation: the notice goes in front of the one at the lower end
/// of the set and X becomes active. The caller follows this with a transfer.
fn sim_activate_direct() -> Function {
    let mut f = FunctionBuilder::new(SIM_ACTIVATE_DIRECT);
    let process = f.param("process", MirType::ObjectRef);
    let done = f.block();
    let check = f.block();
    let insert = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, done, check);

    f.at(check);
    let scheduled = f.call_value(IS_SCHEDULED, &[process], MirType::I64);
    let already = f.compare_const(CmpOp::Ne, scheduled, 0);
    f.branch(already, done, insert);

    f.at(insert);
    let now = f.call_value(SIM_TIME, &[], MirType::F64);
    let prior = f.konst(1);
    f.call(INSERT_EVENT, &[now, process, prior]);
    f.call(ADVANCE_CURRENT, &[]);
    f.jump(done);

    f.at(done);
    f.ret();
    f.finish()
}

/// `mode` 0 schedules at `time + max(t, 0)`, 1 at `max(t, time)`. `prior != 0`
/// uses prior ordering; `reac != 0` reschedules an already-scheduled process.
fn sim_activate_timed() -> Function {
    let mut f = FunctionBuilder::new(SIM_ACTIVATE_TIMED);
    let process = f.param("process", MirType::ObjectRef);
    let t = f.param("t", MirType::F64);
    let mode = f.param("mode", MirType::I64);
    let prior = f.param("prior", MirType::I64);
    let reac = f.param("reac", MirType::I64);

    let done = f.block();
    let check = f.block();
    let skip = f.block();
    let go = f.block();
    let delay_mode = f.block();
    let use_t = f.block();
    let use_zero = f.block();
    let merged = f.block();
    let at_mode = f.block();
    let use_t2 = f.block();
    let use_now = f.block();
    let have_at = f.block();
    let fast = f.block();
    let normal = f.block();
    let clamp = f.block();
    let use_at = f.block();
    let advance = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, done, check);

    f.at(check);
    let reactivate = f.compare_const(CmpOp::Ne, reac, 0);
    f.branch(reactivate, go, skip);

    f.at(skip);
    let scheduled = f.call_value(IS_SCHEDULED, &[process], MirType::I64);
    let already = f.compare_const(CmpOp::Ne, scheduled, 0);
    f.branch(already, done, go);

    f.at(go);
    let now = f.call_value(SIM_TIME, &[], MirType::F64);
    let at = f.local("at", MirType::F64);
    let relative = f.compare_const(CmpOp::Eq, mode, 0);
    f.branch(relative, delay_mode, at_mode);

    f.at(delay_mode);
    let zero = f64_zero(&mut f);
    let delay = f.local("delay", MirType::F64);
    let forward = f.compare(CmpOp::Ge, t, zero);
    f.branch(forward, use_t, use_zero);

    f.at(use_t);
    f.assign(delay, t);
    f.jump(merged);

    f.at(use_zero);
    f.assign(delay, zero);
    f.jump(merged);

    f.at(merged);
    f.push(Op::Binary {
        dest: at,
        op: BinOp::Add,
        left: now,
        right: delay,
    });
    f.jump(have_at);

    f.at(at_mode);
    let past = f.compare(CmpOp::Lt, t, now);
    f.branch(past, use_now, use_t2);

    f.at(use_t2);
    f.assign(at, t);
    f.jump(have_at);

    f.at(use_now);
    f.assign(at, now);
    f.jump(have_at);

    f.at(have_at);
    let not_later = f.compare(CmpOp::Le, at, now);
    let ahead = f.compare_const(CmpOp::Ne, prior, 0);
    let immediate = f.binary(BinOp::And, not_later, ahead);
    f.branch(immediate, fast, normal);

    f.at(fast);
    let one = f.konst(1);
    f.call(INSERT_EVENT, &[now, process, one]);
    f.jump(advance);

    f.at(normal);
    let future = f.compare(CmpOp::Ge, at, now);
    f.branch(future, use_at, clamp);

    f.at(clamp);
    f.assign(at, now);
    f.jump(use_at);

    f.at(use_at);
    f.call(INSERT_EVENT, &[at, process, prior]);
    f.jump(advance);

    f.at(advance);
    f.call(ADVANCE_CURRENT, &[]);
    f.jump(done);

    f.at(done);
    f.ret();
    f.finish()
}

/// Insert `process` at the same time as `other`, immediately before or after it
/// in the set. A no-op when `other` is not scheduled.
fn sim_activate_relative() -> Function {
    let mut f = FunctionBuilder::new(SIM_ACTIVATE_RELATIVE);
    let process = f.param("process", MirType::ObjectRef);
    let other = f.param("other", MirType::ObjectRef);
    let before = f.param("before", MirType::I64);

    let done = f.block();
    let check_other = f.block();
    let check_same = f.block();
    let seek = f.block();
    let seek_head = f.block();
    let seek_body = f.block();
    let seek_found = f.block();
    let seek_next = f.block();
    let seek_done = f.block();
    let remove = f.block();
    let grow_block = f.block();
    let refind = f.block();
    let refind_head = f.block();
    let refind_body = f.block();
    let refind_found = f.block();
    let refind_next = f.block();
    let refind_done = f.block();
    let place = f.block();
    let before_yes = f.block();
    let before_no = f.block();
    let main_scan = f.block();
    let main_scan_body = f.block();
    let main_scan_next = f.block();
    let shift_init = f.block();
    let shift = f.block();
    let shift_body = f.block();
    let shift_done = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none_p = f.local("none_p", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none_p,
        object: process,
    });
    f.branch(none_p, done, check_other);

    f.at(check_other);
    let none_o = f.local("none_o", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none_o,
        object: other,
    });
    f.branch(none_o, done, check_same);

    f.at(check_same);
    let itself = f.compare(CmpOp::Eq, process, other);
    f.branch(itself, done, seek);

    // `other` has to be scheduled *before* the cancel, or `activate X after Y`
    // with an idle Y would silently take X out of the set.
    f.at(seek);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let i = f.local("i", MirType::I64);
    let seen = f.local("seen", MirType::I64);
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.push(Op::ConstI64 {
        dest: seen,
        value: 0,
    });
    f.jump(seek_head);

    f.at(seek_head);
    let exhausted = f.compare(CmpOp::Ge, i, len);
    f.branch(exhausted, seek_done, seek_body);

    f.at(seek_body);
    let named = load_notice_process(&mut f, i);
    let same = f.compare(CmpOp::Eq, named, other);
    f.branch(same, seek_found, seek_next);

    f.at(seek_found);
    f.push(Op::ConstI64 {
        dest: seen,
        value: 1,
    });
    f.jump(seek_done);

    f.at(seek_next);
    let next = f.add_const(i, 1);
    f.assign(i, next);
    f.jump(seek_head);

    f.at(seek_done);
    let missing = f.compare_const(CmpOp::Eq, seen, 0);
    f.branch(missing, done, remove);

    f.at(remove);
    f.call(CANCEL_UNLOCKED, &[process]);
    let live_len = f.local("live_len", MirType::I64);
    let after_cancel = load_state(&mut f, SIM_OFF_LEN);
    f.assign(live_len, after_cancel);
    let cap = load_state(&mut f, SIM_OFF_CAP);
    let full = f.compare(CmpOp::Ge, live_len, cap);
    f.branch(full, grow_block, refind);

    f.at(grow_block);
    f.call(GROW, &[]);
    f.jump(refind);

    // Cancelling `process` may have moved `other`, so its slot is looked up
    // again over the compacted set.
    f.at(refind);
    let set = load_state(&mut f, SIM_OFF_SQS);
    let pos = f.local("pos", MirType::I64);
    let y_time = f.local("y_time", MirType::F64);
    let k = f.local("k", MirType::I64);
    f.push(Op::ConstI64 {
        dest: pos,
        value: -1,
    });
    f.push(Op::ConstI64 { dest: k, value: 0 });
    f.jump(refind_head);

    f.at(refind_head);
    let past_end = f.compare(CmpOp::Ge, k, live_len);
    f.branch(past_end, refind_done, refind_body);

    f.at(refind_body);
    let candidate = notice_at(&mut f, set, k);
    let holder = load_notice_process(&mut f, k);
    let hit = f.compare(CmpOp::Eq, holder, other);
    f.branch(hit, refind_found, refind_next);

    f.at(refind_found);
    f.assign(pos, k);
    let when = load_f64(&mut f, candidate, NOTICE_EVTIME);
    f.assign(y_time, when);
    f.jump(refind_done);

    f.at(refind_next);
    let onward = f.add_const(k, 1);
    f.assign(k, onward);
    f.jump(refind_head);

    f.at(refind_done);
    let lost = f.compare_const(CmpOp::Eq, pos, -1);
    f.branch(lost, done, place);

    f.at(place);
    let insert_at = f.local("insert_at", MirType::I64);
    let ahead = f.compare_const(CmpOp::Ne, before, 0);
    f.branch(ahead, before_yes, before_no);

    f.at(before_yes);
    f.assign(insert_at, pos);
    f.jump(shift_init);

    f.at(before_no);
    let behind = f.add_const(pos, 1);
    f.assign(insert_at, behind);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    let is_main = f.compare(CmpOp::Eq, process, main);
    f.branch(is_main, main_scan, shift_init);

    // `activate main after X` runs every same-time peer of X before MAIN, so a
    // later tied winner can still take its turn (simtst96).
    f.at(main_scan);
    let at_end = f.compare(CmpOp::Ge, insert_at, live_len);
    f.branch(at_end, shift_init, main_scan_body);

    f.at(main_scan_body);
    let peer = notice_at(&mut f, set, insert_at);
    let peer_time = load_f64(&mut f, peer, NOTICE_EVTIME);
    let tied = f.compare(CmpOp::Eq, peer_time, y_time);
    f.branch(tied, main_scan_next, shift_init);

    f.at(main_scan_next);
    let past_peer = f.add_const(insert_at, 1);
    f.assign(insert_at, past_peer);
    f.jump(main_scan);

    f.at(shift_init);
    let j = f.local("j", MirType::I64);
    f.assign(j, live_len);
    f.jump(shift);

    f.at(shift);
    let settled = f.compare(CmpOp::Le, j, insert_at);
    f.branch(settled, shift_done, shift_body);

    f.at(shift_body);
    let prev = f.add_const(j, -1);
    copy_notice(&mut f, set, prev, j);
    let down = f.add_const(j, -1);
    f.assign(j, down);
    f.jump(shift);

    f.at(shift_done);
    let slot = notice_at(&mut f, set, insert_at);
    f.store(slot, NOTICE_EVTIME, y_time);
    store_notice_process(&mut f, insert_at, process);
    let seq = f.call_value(NEXT_SEQ, &[], MirType::I64);
    f.store(slot, NOTICE_SEQ, seq);
    let grown = f.add_const(live_len, 1);
    store_state(&mut f, SIM_OFF_LEN, grown);
    f.call(ADVANCE_CURRENT, &[]);
    f.jump(done);

    f.at(done);
    f.ret();
    f.finish()
}

fn sim_passivate() -> Function {
    let mut f = FunctionBuilder::new(SIM_PASSIVATE);
    f.call(ENSURE_ACTIVE, &[]);
    let self_process = f.call_value(RUNNING_OR_MAIN, &[], MirType::ObjectRef);
    f.call(CANCEL_UNLOCKED, &[self_process]);
    f.call(ADVANCE_CURRENT, &[]);
    f.ret();
    f.finish()
}

/// Chapter 12 scheduling expressed as chapter 7 transfers: becoming operative
/// is a resume, and yielding to MAIN is a detach.
fn sim_transfer_to_head() -> Function {
    let mut f = FunctionBuilder::new(SIM_TRANSFER_TO_HEAD);
    let done = f.block();
    let work = f.block();
    let detach_path = f.block();
    let skip = f.block();
    let do_detach = f.block();
    let resume_path = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    let head = f.call_value(HEAD, &[], MirType::ObjectRef);
    let running = f.call_value(RUNNING_OR_MAIN, &[], MirType::ObjectRef);
    store_ref(&mut f, SIM_CURRENT_STORE, head);
    let settled = f.compare(CmpOp::Eq, head, running);
    f.branch(settled, done, work);

    f.at(work);
    let empty = f.is_none_object(head);
    let is_main = f.compare(CmpOp::Eq, head, main);
    let to_main = f.binary(BinOp::Or, empty, is_main);
    f.branch(to_main, detach_path, resume_path);

    // Nothing left to run, or MAIN's turn: the running process becomes
    // non-operative with its reactivation point after the operation.
    f.at(detach_path);
    store_none(&mut f, SIM_RUNNING_STORE);
    let was_main = f.compare(CmpOp::Eq, running, main);
    f.branch(was_main, skip, do_detach);

    f.at(do_detach);
    f.call(SEQ_DETACH, &[running]);
    f.jump(skip);

    f.at(skip);
    f.jump(done);

    f.at(resume_path);
    store_ref(&mut f, SIM_RUNNING_STORE, head);
    f.call(SEQ_RESUME, &[head]);
    f.jump(done);

    f.at(done);
    f.ret();
    f.finish()
}

/// The active process reaches its final end: it leaves the set and the next
/// process takes over. Neither branch comes back.
fn sim_terminate_current() -> Function {
    let mut f = FunctionBuilder::new(SIM_TERMINATE_CURRENT);
    let process = f.param("process", MirType::ObjectRef);
    let terminate = f.block();
    let terminate_resume = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    f.call(CANCEL_UNLOCKED, &[process]);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    let head = f.call_value(HEAD, &[], MirType::ObjectRef);
    store_ref(&mut f, SIM_CURRENT_STORE, head);
    let empty = f.is_none_object(head);
    let is_main = f.compare(CmpOp::Eq, head, main);
    let to_main = f.binary(BinOp::Or, empty, is_main);
    f.branch(to_main, terminate, terminate_resume);

    f.at(terminate);
    store_none(&mut f, SIM_RUNNING_STORE);
    f.call(SEQ_TERMINATE, &[process]);
    f.ret();

    f.at(terminate_resume);
    store_ref(&mut f, SIM_RUNNING_STORE, head);
    f.call(SEQ_TERMINATE_RESUMING, &[process, head]);
    f.ret();
    f.finish()
}

fn sim_cancel() -> Function {
    let mut f = FunctionBuilder::new(SIM_CANCEL);
    let process = f.param("process", MirType::ObjectRef);
    let done = f.block();
    let live = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, done, live);

    f.at(live);
    f.call(CANCEL_UNLOCKED, &[process]);
    f.call(ADVANCE_CURRENT, &[]);
    f.jump(done);

    f.at(done);
    f.ret();
    f.finish()
}

fn sim_finish_main() -> Function {
    let mut f = FunctionBuilder::new(SIM_FINISH_MAIN);
    f.call(ENSURE_ACTIVE, &[]);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    f.call(CANCEL_UNLOCKED, &[main]);
    f.call(ADVANCE_CURRENT, &[]);
    f.ret();
    f.finish()
}

fn sim_time() -> Function {
    let mut f = FunctionBuilder::returning(SIM_TIME, MirType::F64);
    let empty = f.block();
    let head = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let idle = f.compare_const(CmpOp::Eq, len, 0);
    f.branch(idle, empty, head);

    f.at(empty);
    let zero = f64_zero(&mut f);
    f.ret_value(zero);

    f.at(head);
    let sqs = load_state(&mut f, SIM_OFF_SQS);
    let now = load_f64(&mut f, sqs, NOTICE_EVTIME);
    f.ret_value(now);
    f.finish()
}

/// 12.2's CURRENT is the active process: the one holding the PSC, not whichever
/// notice happens to head the set.
fn sim_is_main_current() -> Function {
    let mut f = FunctionBuilder::returning(SIM_IS_MAIN_CURRENT, MirType::I64);
    let yes = f.block();
    let no = f.block();
    let yes2 = f.block();
    let no2 = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let running = load_ref(&mut f, SIM_RUNNING_LOAD);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    let unset = f.is_none_object(running);
    f.branch(unset, yes, no);

    f.at(yes);
    let one = f.konst(1);
    f.ret_value(one);

    f.at(no);
    let is_main = f.compare(CmpOp::Eq, running, main);
    f.branch(is_main, yes2, no2);

    f.at(yes2);
    let one2 = f.konst(1);
    f.ret_value(one2);

    f.at(no2);
    let zero = f.konst(0);
    f.ret_value(zero);
    f.finish()
}

fn sim_has_current() -> Function {
    let mut f = FunctionBuilder::returning(SIM_HAS_CURRENT, MirType::I64);
    let yes = f.block();
    let no = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let any = f.compare_const(CmpOp::Gt, len, 0);
    f.branch(any, yes, no);

    f.at(yes);
    let one = f.konst(1);
    f.ret_value(one);

    f.at(no);
    let zero = f.konst(0);
    f.ret_value(zero);
    f.finish()
}

fn sim_current() -> Function {
    let mut f = FunctionBuilder::returning(SIM_CURRENT, MirType::ObjectRef);
    f.call(ENSURE_ACTIVE, &[]);
    let running = f.call_value(RUNNING_OR_MAIN, &[], MirType::ObjectRef);
    f.ret_value(running);
    f.finish()
}

fn sim_main() -> Function {
    let mut f = FunctionBuilder::returning(SIM_MAIN, MirType::ObjectRef);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    f.ret_value(main);
    f.finish()
}

fn sim_idle() -> Function {
    let mut f = FunctionBuilder::returning(SIM_IDLE, MirType::I64);
    let process = f.param("process", MirType::ObjectRef);
    let idle = f.block();
    let check = f.block();
    let idle_yes = f.block();
    let idle_no = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, idle, check);

    f.at(idle);
    let one = f.konst(1);
    f.ret_value(one);

    f.at(check);
    let scheduled = f.call_value(IS_SCHEDULED, &[process], MirType::I64);
    let unscheduled = f.compare_const(CmpOp::Eq, scheduled, 0);
    f.branch(unscheduled, idle_yes, idle_no);

    f.at(idle_yes);
    let yes = f.konst(1);
    f.ret_value(yes);

    f.at(idle_no);
    let no = f.konst(0);
    f.ret_value(no);
    f.finish()
}

fn sim_terminated() -> Function {
    let mut f = FunctionBuilder::returning(SIM_TERMINATED, MirType::I64);
    let process = f.param("process", MirType::ObjectRef);
    let zero_out = f.block();
    let check_main = f.block();
    let check_component = f.block();
    let finish = f.block();
    let yes = f.block();
    let no = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, zero_out, check_main);

    f.at(check_main);
    let main = load_ref(&mut f, SIM_MAIN_LOAD);
    let is_main = f.compare(CmpOp::Eq, process, main);
    f.branch(is_main, zero_out, check_component);

    f.at(check_component);
    let component = f.call_value(COMPONENT_OF, &[process], PTR);
    let unknown = f.compare_const(CmpOp::Eq, component, 0);
    f.branch(unknown, zero_out, finish);

    f.at(finish);
    let state = f.load(component, COMP_STATE);
    let ended = f.compare_const(CmpOp::Eq, state, STATE_TERMINATED);
    f.branch(ended, yes, no);

    f.at(yes);
    let one = f.konst(1);
    f.ret_value(one);

    f.at(no);
    let not_ended = f.konst(0);
    f.ret_value(not_ended);

    f.at(zero_out);
    let zero = f.konst(0);
    f.ret_value(zero);
    f.finish()
}

fn sim_evtime() -> Function {
    let mut f = FunctionBuilder::returning(SIM_EVTIME, MirType::F64);
    let process = f.param("process", MirType::ObjectRef);
    let none_out = f.block();
    let scan = f.block();
    let loop_head = f.block();
    let loop_body = f.block();
    let advance = f.block();
    let found = f.block();
    let bad = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, none_out, scan);

    f.at(none_out);
    let zero = f64_zero(&mut f);
    f.ret_value(zero);

    f.at(scan);
    let sqs = load_state(&mut f, SIM_OFF_SQS);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let i = f.local("i", MirType::I64);
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.jump(loop_head);

    f.at(loop_head);
    let exhausted = f.compare(CmpOp::Ge, i, len);
    f.branch(exhausted, bad, loop_body);

    f.at(loop_body);
    let notice = notice_at(&mut f, sqs, i);
    let named = load_notice_process(&mut f, i);
    let same = f.compare(CmpOp::Eq, named, process);
    f.branch(same, found, advance);

    f.at(advance);
    let next = f.add_const(i, 1);
    f.assign(i, next);
    f.jump(loop_head);

    f.at(found);
    let when = load_f64(&mut f, notice, NOTICE_EVTIME);
    f.ret_value(when);

    f.at(bad);
    f.abort("evtime of idle process");
    f.finish()
}

/// 12.1 nextev: the next process in the set after `process`, or none if it is
/// idle or last.
fn sim_nextev() -> Function {
    let mut f = FunctionBuilder::returning(SIM_NEXTEV, MirType::ObjectRef);
    let process = f.param("process", MirType::ObjectRef);
    let none_out = f.block();
    let scan = f.block();
    let loop_head = f.block();
    let loop_body = f.block();
    let advance = f.block();
    let found = f.block();
    let has_next = f.block();

    f.call(ENSURE_ACTIVE, &[]);
    let none = f.local("none", MirType::Bool);
    f.push(Op::ObjectIsNone {
        dest: none,
        object: process,
    });
    f.branch(none, none_out, scan);

    f.at(none_out);
    let nothing = f.none_object();
    f.ret_value(nothing);

    f.at(scan);
    let len = load_state(&mut f, SIM_OFF_LEN);
    let i = f.local("i", MirType::I64);
    f.push(Op::ConstI64 { dest: i, value: 0 });
    f.jump(loop_head);

    f.at(loop_head);
    let exhausted = f.compare(CmpOp::Ge, i, len);
    f.branch(exhausted, none_out, loop_body);

    f.at(loop_body);
    let named = load_notice_process(&mut f, i);
    let same = f.compare(CmpOp::Eq, named, process);
    f.branch(same, found, advance);

    f.at(advance);
    let next = f.add_const(i, 1);
    f.assign(i, next);
    f.jump(loop_head);

    f.at(found);
    let next_i = f.add_const(i, 1);
    let last = f.compare(CmpOp::Ge, next_i, len);
    f.branch(last, none_out, has_next);

    f.at(has_next);
    let following = load_notice_process(&mut f, next_i);
    f.ret_value(following);
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::BlockId;

    #[test]
    fn the_static_state_fits_the_reserved_block() {
        const { assert!(SIM_OFF_MAIN + 8 <= SIM_STATE_BYTES) };
    }

    #[test]
    fn every_synthesized_function_has_a_distinct_name() {
        let functions = functions();
        let names = function_names();
        assert_eq!(functions.len(), names.len());
    }

    /// A block that runs off its end falls into the next one, and the ops after
    /// a block's first terminator are dead. Both silently mis-wire a state
    /// machine written by hand, so every block has to end exactly once.
    #[test]
    fn every_block_ends_exactly_once_and_deliberately() {
        fn terminates(op: &Op) -> bool {
            matches!(
                op,
                Op::Jump { .. } | Op::Branch { .. } | Op::Return { .. } | Op::Abort { .. }
            )
        }

        for function in functions() {
            for block in &function.blocks {
                let terminators = block
                    .ops
                    .iter()
                    .filter(|spanned| terminates(&spanned.op))
                    .count();
                assert_eq!(
                    terminators, 1,
                    "{}: {} has {terminators} terminators",
                    function.name, block.id
                );
                assert!(
                    terminates(&block.ops.last().expect("a non-empty block").op),
                    "{}: {} runs off its end",
                    function.name,
                    block.id
                );
            }
        }
    }

    /// A branch or jump to the block it sits in is an unconditional spin.
    #[test]
    fn no_block_transfers_only_to_itself() {
        for function in functions() {
            for block in &function.blocks {
                for spanned in &block.ops {
                    let stuck = match &spanned.op {
                        Op::Jump { target } => *target == block.id,
                        Op::Branch {
                            then_block,
                            else_block,
                            ..
                        } => *then_block == block.id && *else_block == block.id,
                        _ => false,
                    };
                    assert!(!stuck, "{}: {} branches to itself", function.name, block.id);
                }
            }
        }
    }

    /// Event times are binary64 written through the i64 field ops, which
    /// reinterpret. Converting instead would floor every fractional time.
    #[test]
    fn event_times_are_never_numerically_converted() {
        for function in functions() {
            for spanned in function.blocks.iter().flat_map(|block| &block.ops) {
                assert!(
                    !matches!(spanned.op, Op::F64ToI64 { .. } | Op::I64ToF64 { .. }),
                    "{} converts between the SQS's stored bits and a time",
                    function.name
                );
            }
        }
    }

    /// Every block has to be reachable from the entry, or a `f.at` landed on a
    /// block nothing branches to.
    #[test]
    fn every_block_is_reachable() {
        for function in functions() {
            let mut seen = HashSet::new();
            let mut queue = vec![function.entry];
            while let Some(id) = queue.pop() {
                if !seen.insert(id) {
                    continue;
                }
                for spanned in &function.block(id).ops {
                    match &spanned.op {
                        Op::Jump { target } => queue.push(*target),
                        Op::Branch {
                            then_block,
                            else_block,
                            ..
                        } => {
                            queue.push(*then_block);
                            queue.push(*else_block);
                        }
                        _ => {}
                    }
                }
            }
            for index in 0..function.blocks.len() {
                assert!(
                    seen.contains(&BlockId(index)),
                    "{}: block {index} is unreachable",
                    function.name
                );
            }
        }
    }
}
