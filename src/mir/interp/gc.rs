//! Mark-sweep garbage collection for the MIR interpreter.
//!
//! The interpreter is the semantic oracle, so what it reclaims defines the
//! observable reclaimability rules native and wasm must match. The rules:
//!
//! - **Precise tracing, no conservative stack scan.** Every heap word that can
//!   hold an `ObjectRef` / text / array handle carries a [`SlotTag`] written at
//!   the same time as the word (see [`super::HeapObject::tags`],
//!   [`super::Vm::cell_tags`], `ArrayStorage::I64::cell_tags`). Frame locals are
//!   typed [`Value`]s already, so they need no side table.
//! - **Free-list reuse, never compaction.** `ObjectRef`, text, and array
//!   handles *are* indices into `Vm::objects` / `Vm::text_heap` / `Vm::arrays`,
//!   so a sweep may recycle a slot but must never renumber a live one.
//! - **No observable side effects.** In particular, collecting the object that
//!   owns an open file does not close the file — the collector only drops the
//!   `object_identities` entry that maps a dead object index to its BASICIO
//!   identity, never the BASICIO entry itself.
//!
//! Known MVP retention: a [`super::SeqComponent`] is never freed, and each one
//! roots its own object, so every object that ever became a chapter 7 component
//! (every `Process`, every detachable class instance) lives for the whole run.
//! Freeing components means proving no reactivation chain can reach them, which
//! Phase 2 deliberately leaves out; ordinary class instances, texts, and arrays
//! are collected normally.

#[cfg(test)]
use super::seq_ops::SeqTarget;
use super::{ArrayStorage, CallFrame, RefTarget, Value, Vm};

/// Object/array allocations between automatic collections when the caller does
/// not pick a threshold. Small enough that a long-running Simulation stays
/// bounded, large enough that short programs collect at most a few times.
pub(super) const DEFAULT_GC_THRESHOLD: u64 = 1024;

/// `SIM_GC_EVERY=N` overrides [`DEFAULT_GC_THRESHOLD`] (0 disables
/// automatic collection). An implementation extension for stress runs, in the
/// same family as `SIM_GC_STATS`.
pub(super) fn threshold_from_env() -> u64 {
    std::env::var("SIM_GC_EVERY")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_THRESHOLD)
}

/// What a heap word denotes, recorded when the word is written so the mark
/// phase can follow it without guessing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum SlotTag {
    /// An integer, real, boolean, character, or a null handle.
    #[default]
    Scalar,
    /// `Vm::objects` index + 1.
    Object,
    /// `Vm::text_heap` index + 1.
    Text,
    /// `Vm::arrays` index + 1.
    Array,
    /// `Vm::refs` index + 1 (never swept).
    Ref,
    /// `Vm::func_heap` index + 1 (never swept).
    Func,
}

/// Cumulative collector counters for one interpreter run.
#[derive(Clone, Debug, Default)]
pub struct GcStats {
    pub collections: u64,
    pub objects_freed: u64,
    pub texts_freed: u64,
    pub arrays_freed: u64,
    /// Allocations that landed in a swept slot instead of growing a heap.
    pub slots_reused: u64,
    /// Nanoseconds spent inside [`Vm::collect`], summed over the run.
    pub pause_ns: u64,
}

/// Test/embedder control over the collector (`interpret_module_with_gc`).
#[derive(Clone, Debug, Default)]
pub struct GcOptions {
    /// Collect every N object/array allocations. `Some(1)` is stress mode,
    /// `Some(0)` disables automatic collection, `None` keeps the default
    /// threshold.
    pub collect_every: Option<u64>,
    /// Run one final collection after the program ends, so a test can observe
    /// what the last statement dropped.
    pub force_collect_at_end: bool,
}

/// Mark bits plus the grey worklists for one collection.
struct Marks {
    objects: Vec<bool>,
    texts: Vec<bool>,
    arrays: Vec<bool>,
    object_work: Vec<usize>,
    array_work: Vec<usize>,
}

impl Marks {
    fn new(objects: usize, texts: usize, arrays: usize) -> Self {
        Self {
            objects: vec![false; objects],
            texts: vec![false; texts],
            arrays: vec![false; arrays],
            object_work: Vec::new(),
            array_work: Vec::new(),
        }
    }

    fn mark_object(&mut self, index: usize) {
        if let Some(mark) = self.objects.get_mut(index) {
            if !*mark {
                *mark = true;
                self.object_work.push(index);
            }
        }
    }

    fn mark_array(&mut self, index: usize) {
        if let Some(mark) = self.arrays.get_mut(index) {
            if !*mark {
                *mark = true;
                self.array_work.push(index);
            }
        }
    }

    fn mark_text(&mut self, index: usize) {
        if let Some(mark) = self.texts.get_mut(index) {
            *mark = true;
        }
    }

    /// Follows one tagged heap word. Handle encoding is index + 1 throughout,
    /// so 0 is the null handle and needs no marking.
    fn mark_word(&mut self, raw: i64, tag: SlotTag) {
        if raw <= 0 {
            return;
        }
        let index = raw as usize - 1;
        match tag {
            SlotTag::Object => self.mark_object(index),
            SlotTag::Array => self.mark_array(index),
            SlotTag::Text => self.mark_text(index),
            // `refs` and `func_heap` are never swept, so their words need no
            // mark bit — but the tag still has to exist to keep them from
            // being mistaken for one of the collected heaps.
            SlotTag::Scalar | SlotTag::Ref | SlotTag::Func => {}
        }
    }

    /// A `Value::Text` owns its `TextFrame` (and through it an `Rc` on the
    /// TEXTOBJ), so a text local keeps its characters alive without any heap
    /// slot; only the descriptor *home* slot needs marking.
    fn mark_value(&mut self, value: &Value) {
        match value {
            Value::ObjectRef(index) => self.mark_object(*index),
            Value::Array(index) if *index != usize::MAX => self.mark_array(*index),
            _ => {}
        }
    }

    fn mark_frames(&mut self, frames: &[CallFrame]) {
        for frame in frames {
            for value in &frame.locals {
                self.mark_value(value);
            }
            for home in frame.text_homes.values() {
                self.mark_text(*home);
            }
        }
    }
}

impl Vm<'_> {
    /// Counts one object/array allocation and arms the next safepoint once the
    /// threshold is crossed. Never collects here: `alloc_*` runs mid-op, with
    /// the fresh object not yet stored anywhere the tracer can see.
    pub(super) fn note_allocation(&mut self) {
        self.allocs_since_gc = self.allocs_since_gc.saturating_add(1);
        if self.gc_threshold > 0 && self.allocs_since_gc >= self.gc_threshold {
            self.pending_gc = true;
        }
    }

    /// One full mark-sweep pass over the interpreter heaps.
    pub(super) fn collect(&mut self) {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        let started = std::time::Instant::now();
        let mut marks = Marks::new(self.objects.len(), self.text_heap.len(), self.arrays.len());
        self.mark_roots(&mut marks);
        self.trace(&mut marks);
        self.sweep(&marks);
        self.gc_stats.collections += 1;
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            self.gc_stats.pause_ns = self
                .gc_stats
                .pause_ns
                .saturating_add(started.elapsed().as_nanos() as u64);
        }
        self.pending_gc = false;
        self.allocs_since_gc = 0;
    }

    /// The interpreter root set.
    fn mark_roots(&self, marks: &mut Marks) {
        // Live frame locals / params / text descriptor homes, on the active
        // stack and on every parked one (detach / resume reactivation chains
        // are reachable only this way).
        marks.mark_frames(&self.frames);
        if let Some(frames) = &self.parked_outermost {
            marks.mark_frames(frames);
        }
        for component in &self.seq_components {
            marks.mark_frames(&component.frames);
            marks.mark_object(component.object);
        }

        // SQS event notices and the current / running / MAIN processes.
        for object in [self.sim.current, self.sim.running, self.sim.main_object]
            .into_iter()
            .flatten()
        {
            marks.mark_object(object);
        }
        for notice in &self.sim.sqs {
            marks.mark_object(notice.process);
        }

        // SYSIN / SYSOUT file objects.
        for object in [self.sysin_object, self.sysout_object]
            .into_iter()
            .flatten()
        {
            marks.mark_object(object);
        }

        // Pending `name`-parameter thunks / `RefI64` targets. `Local` targets
        // are covered by the frame walk above and `Cell` targets by the cell
        // walk below; only a field address pins an object on its own.
        for target in &self.refs {
            if let RefTarget::ObjectField { object, .. } = target {
                marks.mark_object(*object);
            }
        }

        // Stack-allocated cells (enclosing-block captures, env packs). These
        // are never freed, so every tagged word in them is a root.
        for (index, raw) in self.cells.iter().enumerate() {
            let tag = self.cell_tags.get(index).copied().unwrap_or_default();
            marks.mark_word(*raw, tag);
        }

        // Host-pinned values (`HostCtx::root`).
        for value in &self.host_roots {
            marks.mark_value(value);
        }
    }

    /// Drains the grey worklists: object fields by tag, i64 array elements by
    /// cell tag. Text arrays and f64 arrays hold owned values, not handles.
    fn trace(&self, marks: &mut Marks) {
        loop {
            if let Some(index) = marks.object_work.pop() {
                let object = &self.objects[index];
                for (word, tag) in object.tags.iter().enumerate() {
                    if *tag == SlotTag::Scalar {
                        continue;
                    }
                    let start = word * 8;
                    if start + 8 > object.bytes.len() {
                        break;
                    }
                    let raw = i64::from_le_bytes(
                        object.bytes[start..start + 8]
                            .try_into()
                            .expect("8-byte slice"),
                    );
                    marks.mark_word(raw, *tag);
                }
                continue;
            }
            if let Some(index) = marks.array_work.pop() {
                if let ArrayStorage::I64 {
                    cells, cell_tags, ..
                } = &self.arrays[index]
                {
                    for (key, tag) in cell_tags {
                        if let Some(raw) = cells.get(key) {
                            marks.mark_word(*raw, *tag);
                        }
                    }
                }
                continue;
            }
            return;
        }
    }

    /// Recycles every unmarked slot. Payloads are dropped so a stale index
    /// fails loudly (bounds check / `ArrayStorage::Free`) instead of aliasing
    /// whatever is allocated into the slot next.
    fn sweep(&mut self, marks: &Marks) {
        for index in 0..self.objects.len() {
            if marks.objects[index] || self.objects[index].dead {
                continue;
            }
            let object = &mut self.objects[index];
            object.bytes = Vec::new();
            object.tags = Vec::new();
            object.dead = true;
            self.free_objects.push(index);
            self.gc_stats.objects_freed += 1;
            // Weak maps keyed by object index. Dropping a BASICIO identity
            // does *not* close the file (no finalizers, by design).
            self.object_identities.remove(&index);
            self.seq_by_object.remove(&index);
        }

        for index in 0..self.text_heap.len() {
            if marks.texts[index] || self.text_heap[index].is_none() {
                continue;
            }
            self.text_heap[index] = None;
            self.free_texts.push(index);
            self.gc_stats.texts_freed += 1;
        }

        for index in 0..self.arrays.len() {
            if marks.arrays[index] || matches!(self.arrays[index], ArrayStorage::Free) {
                continue;
            }
            self.arrays[index] = ArrayStorage::Free;
            self.free_arrays.push(index);
            self.gc_stats.arrays_freed += 1;
        }
    }

    /// `SIM_GC_STATS=1`: one summary line on **stderr** at the end of a
    /// run. An implementation extension (decision 4), never on stdout, so it
    /// cannot perturb program output.
    pub(super) fn report_gc_stats(&mut self) {
        if !self.gc_stats_enabled {
            return;
        }
        let stats = &self.gc_stats;
        let line = format!(
            "sim-gc: collections={} objects_freed={} texts_freed={} arrays_freed={} slots_reused={} objects_live={} texts_live={} arrays_live={} pause_ns={}\n",
            stats.collections,
            stats.objects_freed,
            stats.texts_freed,
            stats.arrays_freed,
            stats.slots_reused,
            self.objects.len() - self.free_objects.len(),
            self.text_heap.len() - self.free_texts.len(),
            self.arrays.len() - self.free_arrays.len(),
            stats.pause_ns,
        );
        self.host.write_stderr(&line);
    }

    /// Test helper: how many slots each heap currently has live.
    #[cfg(test)]
    pub(super) fn live_slot_counts(&self) -> (usize, usize, usize) {
        (
            self.objects.len() - self.free_objects.len(),
            self.text_heap.len() - self.free_texts.len(),
            self.arrays.len() - self.free_arrays.len(),
        )
    }

    /// Test helper: whether `target`'s parked stack is still walkable.
    #[cfg(test)]
    pub(super) fn parked_frame_count(&self, target: SeqTarget) -> usize {
        match target {
            SeqTarget::Outermost => self.parked_outermost.as_ref().map_or(0, Vec::len),
            SeqTarget::Component(id) => self.seq_components[id].frames.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::layout::{SIMSET_PRED_OFFSET, SIMSET_SUC_OFFSET};
    use crate::mir::{MirType, Module, lower_program};
    use crate::parse::test_support::parse_program;
    use crate::runtime::text::TextFrame;

    /// Smallest module that still gives [`Vm::new`] something to work with;
    /// these tests drive the heaps directly instead of running MIR.
    fn empty_module() -> Module {
        let program = parse_program("begin end;");
        lower_program(&program).expect("lowering an empty block succeeds")
    }

    /// A VM that only collects when the test says so.
    fn test_vm(module: &Module) -> Vm<'_> {
        let mut vm = Vm::new(module);
        vm.gc_threshold = 0;
        vm
    }

    /// Pushes a synthetic activation record holding `locals`, standing in for
    /// the "live frame locals" root category.
    fn push_root_frame(vm: &mut Vm<'_>, locals: Vec<Value>) {
        let block_id = vm.module.functions[0].entry;
        vm.frames.push(CallFrame {
            function_index: 0,
            locals,
            block_id,
            pc: 0,
            return_to: None,
            text_homes: HashMap::new(),
        });
    }

    fn link(vm: &mut Vm<'_>, from: usize, offset: i64, to: usize) {
        vm.store_object_i64(from, offset, to as i64 + 1, SlotTag::Object)
            .expect("SIMSET link fits the object");
    }

    #[test]
    fn collect_frees_unrooted_objects_and_reuses_their_slots() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let first = vm.alloc_object(16, 7).expect("alloc");
        let second = vm.alloc_object(16, 8).expect("alloc");

        vm.collect();

        assert_eq!(vm.gc_stats.collections, 1);
        assert!(
            vm.gc_stats.pause_ns > 0,
            "a real collection should record pause time, got {}",
            vm.gc_stats.pause_ns
        );
        assert_eq!(vm.gc_stats.objects_freed, 2);
        assert_eq!(vm.free_objects.len(), 2, "both slots go on the free list");
        assert!(vm.objects[first].dead && vm.objects[second].dead);
        assert!(
            vm.objects[first].bytes.is_empty(),
            "freed payloads are dropped so a stale index cannot read them"
        );

        let reused = vm.alloc_object(16, 9).expect("alloc");
        assert!(
            reused == first || reused == second,
            "expected slot reuse, allocated {reused} instead"
        );
        assert_eq!(vm.objects.len(), 2, "reuse must not grow the object heap");
        assert_eq!(vm.gc_stats.slots_reused, 1);
        assert_eq!(vm.load_object_i64(reused, 0).expect("class id"), 9);
    }

    #[test]
    fn objects_reachable_from_a_frame_local_survive() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let live = vm.alloc_object(16, 1).expect("alloc");
        let dropped = vm.alloc_object(16, 2).expect("alloc");
        push_root_frame(&mut vm, vec![Value::ObjectRef(live)]);

        vm.collect();

        assert_eq!(vm.gc_stats.objects_freed, 1);
        assert!(!vm.objects[live].dead);
        assert_eq!(vm.load_object_i64(live, 0).expect("class id"), 1);
        assert!(vm.objects[dropped].dead);
    }

    #[test]
    fn object_fields_and_array_elements_keep_their_targets_alive() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let owner = vm.alloc_object(24, 1).expect("alloc");
        let through_field = vm.alloc_object(16, 2).expect("alloc");
        let through_element = vm.alloc_object(16, 3).expect("alloc");
        let unreachable = vm.alloc_object(16, 4).expect("alloc");
        let array = vm
            .alloc_array(MirType::ArrayI64, vec![(1, 2)])
            .expect("alloc array");

        vm.store_object_i64(owner, 8, through_field as i64 + 1, SlotTag::Object)
            .expect("field store");
        // A plain integer that happens to equal a live object handle must not
        // retain anything — tracing is precise, not conservative.
        vm.store_object_i64(owner, 16, unreachable as i64 + 1, SlotTag::Scalar)
            .expect("field store");
        vm.array_store(
            array,
            &[1],
            MirType::ObjectRef,
            Value::ObjectRef(through_element),
        )
        .expect("array store");
        push_root_frame(&mut vm, vec![Value::ObjectRef(owner), Value::Array(array)]);

        vm.collect();

        assert!(!vm.objects[owner].dead);
        assert!(
            !vm.objects[through_field].dead,
            "object field is a root edge"
        );
        assert!(
            !vm.objects[through_element].dead,
            "tagged i64 array elements are traced"
        );
        assert!(
            vm.objects[unreachable].dead,
            "a Scalar-tagged word must not retain an object"
        );
        assert_eq!(vm.gc_stats.arrays_freed, 0);
    }

    #[test]
    fn simset_ring_survives_through_a_live_member_and_is_freed_without_one() {
        // head <-> a <-> b <-> head, exactly the shape `into` / `follow` build.
        fn build_ring(vm: &mut Vm<'_>) -> [usize; 3] {
            let ring = [
                vm.alloc_object(24, 1).expect("alloc"),
                vm.alloc_object(24, 2).expect("alloc"),
                vm.alloc_object(24, 3).expect("alloc"),
            ];
            for (position, &object) in ring.iter().enumerate() {
                link(vm, object, SIMSET_SUC_OFFSET, ring[(position + 1) % 3]);
                link(vm, object, SIMSET_PRED_OFFSET, ring[(position + 2) % 3]);
            }
            ring
        }

        let module = empty_module();

        let mut rooted = test_vm(&module);
        let ring = build_ring(&mut rooted);
        push_root_frame(&mut rooted, vec![Value::ObjectRef(ring[0])]);
        rooted.collect();
        assert_eq!(
            rooted.gc_stats.objects_freed, 0,
            "a live head keeps the whole ring reachable"
        );
        assert_eq!(
            rooted
                .load_object_i64(ring[0], SIMSET_SUC_OFFSET)
                .expect("suc"),
            ring[1] as i64 + 1
        );

        let mut unrooted = test_vm(&module);
        build_ring(&mut unrooted);
        unrooted.collect();
        assert_eq!(
            unrooted.gc_stats.objects_freed, 3,
            "a cycle with no external reference is collectible"
        );
    }

    #[test]
    fn text_and_array_slots_are_freed_and_reused() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let handle = vm.intern_text(&TextFrame::from_literal("abc", true));
        let array = vm
            .alloc_array(MirType::ArrayI64, vec![(1, 4)])
            .expect("alloc array");

        vm.collect();

        assert_eq!(vm.gc_stats.texts_freed, 1);
        assert_eq!(vm.gc_stats.arrays_freed, 1);
        assert!(vm.text_heap[handle as usize - 1].is_none());
        assert!(matches!(vm.arrays[array], ArrayStorage::Free));
        assert!(
            vm.array_load(array, &[1], MirType::I64).is_err(),
            "a stale array descriptor must fail loudly"
        );

        assert_eq!(
            vm.intern_text(&TextFrame::from_literal("xyz", true)),
            handle,
            "the swept text slot is reused"
        );
        assert_eq!(vm.text_heap.len(), 1);
        assert_eq!(
            vm.alloc_array(MirType::ArrayI64, vec![(1, 4)])
                .expect("alloc array"),
            array,
            "the swept array slot is reused"
        );
        assert_eq!(vm.arrays.len(), 1);
    }

    #[test]
    fn a_text_descriptor_home_on_a_frame_keeps_its_heap_slot() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let kept = vm.intern_text(&TextFrame::from_literal("kept", true));
        let dropped = vm.intern_text(&TextFrame::from_literal("dropped", true));
        push_root_frame(&mut vm, vec![Value::I64(0)]);
        vm.frames[0].text_homes.insert(0, kept as usize - 1);

        vm.collect();

        assert_eq!(vm.gc_stats.texts_freed, 1);
        assert!(vm.text_heap[kept as usize - 1].is_some());
        assert!(vm.text_heap[dropped as usize - 1].is_none());
    }

    #[test]
    fn a_field_address_keeps_its_object_alive() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let addressed = vm.alloc_object(16, 1).expect("alloc");
        let dropped = vm.alloc_object(16, 2).expect("alloc");
        vm.refs.push(RefTarget::ObjectField {
            object: addressed,
            offset: 8,
        });

        vm.collect();

        assert!(!vm.objects[addressed].dead, "a pending FieldAddr is a root");
        assert!(vm.objects[dropped].dead);
    }

    #[test]
    fn tagged_stack_cells_are_roots() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        let captured = vm.alloc_object(16, 1).expect("alloc");
        let dropped = vm.alloc_object(16, 2).expect("alloc");
        vm.cells.push(captured as i64 + 1);
        vm.cell_tags.push(SlotTag::Object);

        vm.collect();

        assert!(!vm.objects[captured].dead, "env-pack cells are roots");
        assert!(vm.objects[dropped].dead);
    }

    #[test]
    fn allocation_threshold_arms_the_next_safepoint_without_collecting() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        vm.gc_threshold = 2;

        vm.alloc_object(16, 1).expect("alloc");
        assert!(!vm.pending_gc, "one allocation is below the threshold");
        vm.alloc_object(16, 2).expect("alloc");
        assert!(vm.pending_gc, "crossing the threshold arms a collection");
        assert_eq!(
            vm.gc_stats.collections, 0,
            "allocation must never collect in place — the object is not rooted yet"
        );

        vm.collect();
        assert!(!vm.pending_gc);
        assert_eq!(vm.allocs_since_gc, 0);
    }

    #[test]
    fn live_slot_counts_shrink_after_a_collection() {
        let module = empty_module();
        let mut vm = test_vm(&module);
        for class_id in 0..8 {
            vm.alloc_object(16, class_id).expect("alloc");
        }
        assert_eq!(vm.live_slot_counts().0, 8);

        vm.collect();

        assert_eq!(vm.live_slot_counts().0, 0);
        assert_eq!(vm.parked_frame_count(SeqTarget::Outermost), 0);
    }
}
