//! Phase 4 — map Simula layouts onto host WasmGC heap types.
//!
//! Wasm reclamation is the **host** collector's job. This module builds the
//! type section those refs inhabit; wasm codegen **always** lowers ObjectRef /
//! Text / Array through WasmGC (no bump-heap object fallback). Interpreter and
//! native keep their own collectors. Transitional root-handle debt for
//! spills/attrs is tracked under Phases 4-R2…R4.
//!
//! # Mapping choices
//!
//! | Simula | WasmGC |
//! | --- | --- |
//! | `integer` / `character` / packed `i64` slots | `i64` |
//! | `boolean` | `i64` word in class structs (reinterpret at load/store) |
//! | `real` / `long real` | `f64` / i64 bits in class structs |
//! | TEXTOBJ character storage | `(array i8)` |
//! | text frame | `struct { chars, start, length, pos, constant }` |
//! | arrays | descriptor structs `{ elems, ndims, bounds }` |
//! | `ref(T)` array elements | `(array (mut (ref null eq)))` spine on an `array_i64` subtype |
//! | class instance | `struct` of slots (slot 0 = `class_id`; linkage `suc`/`pred` are eqrefs) |
//! | coroutine `arg` / component `object` / parked-frame ref spine | `seq_gc_slot` structs in a global registry array |
//! | Simulation CURRENT / RUNNING / MAIN | mutable `(ref null eq)` module globals |
//! | SQS notice `process` | `(array (mut (ref null eq)))` spine, indexed by notice |
//! | class Text/Array / ObjectRef attrs | typed WasmGC fields (`eqref` / text-frame / array desc) |
//! | own-stack by-ref ObjectRef captures | `ref_cell` struct `{ (mut eqref) }` stored as an eqref field |
//! | `ref(T)` / `none` | `(ref null eq)` / `ref.null` |
//! | prefix / SIMSET | `linkage_base` + eqref ladder + class subtypes |
//!
//! Linear memory stays for WASI/IO scratch and scalar spill; this registry
//! never allocates bump object addresses.

use std::collections::HashMap;

use wasm_encoder::{
    AbstractHeapType, CodeSection, CompositeInnerType, CompositeType, ExportKind, ExportSection,
    FieldType as WasmFieldType, Function, FunctionSection, HeapType, Instruction, Module, RefType,
    StorageType, StructType, SubType, TypeSection, ValType,
};

use crate::layout::{
    ClassLayout, FieldType as LayoutFieldType, I64_FIELD_SIZE, OBJECT_HEADER_SIZE,
    SIMSET_PRED_OFFSET, SIMSET_SUC_OFFSET,
};

/// Index of a type in the WasmGC type section this registry builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GcTypeId(pub u32);

impl GcTypeId {
    pub fn as_u32(self) -> u32 {
        self.0
    }

    /// `(ref null $self)` as a value type.
    pub fn ref_null(self) -> ValType {
        ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(self.0),
        })
    }

    fn field_ref_null(self) -> WasmFieldType {
        WasmFieldType {
            element_type: StorageType::Val(self.ref_null()),
            mutable: true,
        }
    }
}

fn field_val(ty: ValType) -> WasmFieldType {
    WasmFieldType {
        element_type: StorageType::Val(ty),
        mutable: true,
    }
}

/// The three fields every linkage-family struct opens with — see
/// [`GcTypeRegistry::ensure_linkage_base`].
fn linkage_base_fields() -> Vec<WasmFieldType> {
    vec![
        field_val(ValType::I64), // class_id
        field_val(anyref_val()), // suc
        field_val(anyref_val()), // pred
    ]
}

/// Number of leading fields [`linkage_base_fields`] occupies.
const LINKAGE_BASE_FIELDS: usize = 3;

/// `(ref null eq)` — ObjectRef ABI under WasmGC (`ref.eq` requires `eq`).
pub fn anyref_val() -> ValType {
    ValType::Ref(RefType::EQREF)
}

fn anyref_null() -> ValType {
    anyref_val()
}

/// Heap type for `ref.null` / casts of the ObjectRef ABI.
pub fn object_ref_heap() -> HeapType {
    HeapType::Abstract {
        shared: false,
        ty: AbstractHeapType::Eq,
    }
}

/// Wasm type index of a registry id after it was appended at `base`.
pub fn rebased_index(id: GcTypeId, base: u32) -> u32 {
    base + id.as_u32()
}

/// `(ref null $index)` for an already-rebased wasm type index.
pub fn concrete_ref_null(index: u32) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(index),
    })
}

/// `HeapType::Concrete(index)` for an already-rebased wasm type index.
pub fn concrete_heap(index: u32) -> HeapType {
    HeapType::Concrete(index)
}

/// [`text_frame`](GcTypeRegistry::text_frame) field indices.
pub const TEXT_FRAME_FIELD_CHARS: u32 = 0;
pub const TEXT_FRAME_FIELD_START: u32 = 1;
pub const TEXT_FRAME_FIELD_LENGTH: u32 = 2;
pub const TEXT_FRAME_FIELD_POS: u32 = 3;
pub const TEXT_FRAME_FIELD_CONSTANT: u32 = 4;

/// Array descriptor struct field indices (shared by `array_i64` / `array_f64`
/// / `array_text`; see [`GcTypeRegistry::array_i64`] etc.).
pub const ARRAY_DESC_FIELD_ELEMS: u32 = 0;
pub const ARRAY_DESC_FIELD_NDIMS: u32 = 1;
pub const ARRAY_DESC_FIELD_BOUNDS: u32 = 2;
/// [`GcTypeRegistry::array_object`]'s extra field — the `(ref null eq)`
/// element spine a `ref(T) array` uses instead of [`ARRAY_DESC_FIELD_ELEMS`].
pub const ARRAY_DESC_FIELD_OBJECT_ELEMS: u32 = 3;

/// [`GcTypeRegistry::seq_gc_slot`] field indices.
///
/// `OBJECT` is a coroutine's `CORO_ARG` **or** a component's `COMP_OBJECT`
/// (one record type serves both — see that method); `SPINE` is the coro's
/// parked-frame ref spine and stays null on component slots.
pub const SEQ_GC_SLOT_FIELD_OBJECT: u32 = 0;
pub const SEQ_GC_SLOT_FIELD_SPINE: u32 = 1;

/// WasmGC struct field index of a layout byte offset.
///
/// Class structs are *slot*-indexed: one `i64` field per 8 layout bytes,
/// including the gaps `layout.rs` leaves between a class's own attributes
/// and the family-aligned enclosing-capture region. That makes a byte
/// offset mean the same field index in **every** class of a prefix family
/// (see [`GcTypeRegistry::register_class`]).
pub fn slot_index_for_offset(offset: i64) -> u32 {
    (offset / I64_FIELD_SIZE) as u32
}

/// Number of `i64` slots in a class's WasmGC struct (slot 0 = `class_id`).
///
/// Slot count is still byte-offset based so a prefix's field *indices*
/// stay aligned with its subclasses. Individual slots are no longer all
/// `i64`: declared ObjectRef / Text / Array attributes and `ref_cell`
/// capture slots are typed WasmGC refs (Phase 4-R4).
fn class_slot_count(layout: &ClassLayout) -> usize {
    let by_size = layout.size.max(OBJECT_HEADER_SIZE) / I64_FIELD_SIZE;
    let by_fields = layout
        .fields
        .iter()
        .map(|field| field.offset / I64_FIELD_SIZE + 1)
        .max()
        .unwrap_or(1);
    by_size.max(by_fields).max(1) as usize
}

/// Per-class WasmGC emit info (rebased type index + field map).
#[derive(Debug, Clone)]
pub struct ClassGcInfo {
    /// Rebased WasmGC struct type index.
    pub wasm_ty: u32,
    /// Byte offset → `(WasmGC field_index, layout field type)`.
    /// Offset 0 is the `class_id` header (`LayoutFieldType::I64`).
    pub fields_by_offset: HashMap<i64, (u32, LayoutFieldType)>,
    /// [`layout_is_linkage_family`] for the layout this info was built from.
    pub linkage_family: bool,
}

impl ClassGcInfo {
    /// Whether this class's own WasmGC struct is a `linkage_base` sibling
    /// (its first two attrs are the injected `SUC`/`PRED` ring pointers —
    /// see [`layout_is_linkage_family`]). Any *other* linkage-family class
    /// (Head/Link/Process/a user `Link`-subclass, or the runtime's own
    /// `Linkage`) may show up at runtime wherever this one is statically
    /// expected, so callers doing a type-agnostic read (just `class_id`, or
    /// SUC/PRED) must cast through `linkage_base` — casting to *this*
    /// class's own struct type traps on any sibling that isn't the exact
    /// same leaf class (simtst93: `Ref(Linkage) l` walking a ring of `Bead`s
    /// via `l:-l.Suc` must not be cast to `Linkage`'s own struct).
    ///
    /// This is the same predicate [`GcTypeRegistry::register_class`] applies
    /// to the layout, so it also tells the caller that fields 1 and 2 of
    /// `wasm_ty` are real `(ref null eq)` fields rather than `i64` slots.
    pub fn is_linkage_family(&self) -> bool {
        self.linkage_family
    }
}

/// [`linkage_base`](GcTypeRegistry::ensure_linkage_base) field indices.
pub const LINKAGE_FIELD_CLASS_ID: u32 = 0;
pub const LINKAGE_FIELD_SUC: u32 = 1;
pub const LINKAGE_FIELD_PRED: u32 = 2;

/// Whether `field_index` names one of `linkage_base`'s two `eqref` ring
/// pointers (as opposed to the `i64` `class_id` header or a trailing `i64`
/// attribute slot).
pub fn is_linkage_ref_field(field_index: u32) -> bool {
    field_index == LINKAGE_FIELD_SUC || field_index == LINKAGE_FIELD_PRED
}

/// Whether `layout` carries the injected SIMSET ring pointers — i.e. whether
/// `layout.rs`'s `class_needs_simset_slots` fired for it (Head / Link /
/// Process / `Linkage` itself / a user class prefixed by one of them).
///
/// This drives [`GcTypeRegistry::register_class`]'s choice between the
/// all-`i64` slot ladder and the `linkage_base` family, so it mirrors that
/// function's **name**-based rule. The shape of the first two attributes
/// alone would not do: an ordinary class whose first two enclosing captures
/// happen to be `ref` variables has `ObjectRef` at exactly these offsets
/// without being a ring member at all (simtst47's `Class A` capturing
/// `ra2`/`ra3`), and handing it typed ring pointers puts it outside the
/// all-`i64` slot ladder that class-agnostic capture writes rely on. The
/// shape is still required on top of the name, so a linkage class whose
/// slots were laid out some other way stays on the ladder.
pub fn layout_is_linkage_family(layout: &ClassLayout) -> bool {
    fn is_simset_name(name: &str) -> bool {
        let name = crate::layout::declared_class_name(name);
        name.eq_ignore_ascii_case("linkage")
            || crate::simulation::is_link_class(name)
            || crate::simulation::is_head_class(name)
    }
    let named = is_simset_name(&layout.declared_name)
        || is_simset_name(&layout.name)
        || layout.prefix.as_deref().is_some_and(is_simset_name);
    if !named {
        return false;
    }
    let field_ty = |offset: i64| {
        layout
            .fields
            .iter()
            .find(|field| field.offset == offset)
            .map(|field| field.ty)
    };
    field_ty(SIMSET_SUC_OFFSET) == Some(LayoutFieldType::ObjectRef)
        && field_ty(SIMSET_PRED_OFFSET) == Some(LayoutFieldType::ObjectRef)
}

/// Whether a layout field at `offset` is a GC reference (object / text / array).
fn field_is_gc_ref(ty: LayoutFieldType) -> bool {
    matches!(
        ty,
        LayoutFieldType::ObjectRef
            | LayoutFieldType::Text
            | LayoutFieldType::ArrayI64
            | LayoutFieldType::ArrayBool
            | LayoutFieldType::ArrayF64
            | LayoutFieldType::ArrayText
    )
}

/// Upper bound on [`GcEmitCtx::cast_target`]'s supertype walk.
const MAX_SUPERTYPE_DEPTH: u32 = 256;

/// How many rungs [`GcTypeRegistry::ensure_slot_ladder`] will build.
///
/// WasmGC engines bound how deep a subtype chain may be (V8 stops at 63),
/// and a class sits *above* its rung with its own prefix chain on top, so
/// the ladder has to leave headroom. Classes wider than this still get
/// their own struct and prefix chain; only class-agnostic access to their
/// highest slots loses the ladder's guarantee.
const MAX_SLOT_LADDER: usize = 40;

/// Context threaded through wasm emission when ObjectRef lowers to WasmGC.
#[derive(Debug, Clone)]
pub struct GcEmitCtx {
    /// Type-section index of registry id 0.
    pub base: u32,
    /// Rebased WasmGC type index of the shared SIMSET `linkage_base` struct, if any.
    pub linkage_base_ty: Option<u32>,
    /// Rebased WasmGC type index of the universal `{ class_id }` root every
    /// class struct extends (see [`GcTypeRegistry::ensure_class_base`]).
    pub class_base_ty: Option<u32>,
    /// Rebased struct type index → its rebased supertype index.
    pub supertypes: HashMap<u32, u32>,
    /// Rebased struct type index → its declared field count.
    pub field_counts: HashMap<u32, u32>,
    /// Rebased struct type indices whose fields 1/2 are `linkage_base`'s
    /// `eqref` ring pointers (see [`GcTypeRegistry::has_linkage_ref_fields`]).
    pub linkage_ref_types: std::collections::HashSet<u32>,
    /// Rebased struct type → per-field "is a WasmGC ref (not `i64`)" flags.
    /// Used by [`Self::cast_target`] so a walk does not land on an ancestor
    /// whose slot is still an `i64` handle/word while the leaf has a typed ref.
    pub field_is_ref: HashMap<u32, Vec<bool>>,
    /// `ClassLayout.class_id` → class emit info.
    pub by_class_id: HashMap<i64, ClassGcInfo>,
    /// Lowercased layout / declared name → class emit info.
    pub by_class_name: HashMap<String, ClassGcInfo>,
    /// Rebased WasmGC type index of `(array i8)` (TEXTOBJ character storage).
    pub text_chars_ty: u32,
    /// Rebased WasmGC type index of `text_frame` (see [`GcTypeRegistry::text_frame`]).
    pub text_frame_ty: u32,
    /// Rebased WasmGC type index of the flat `(array i64)` array-bounds store.
    pub bounds_array_ty: u32,
    /// Rebased WasmGC type index of `(array i64)` integer/boolean array elements.
    pub array_i64_elems_ty: u32,
    /// Rebased WasmGC type index of `(array f64)` real array elements.
    pub array_f64_elems_ty: u32,
    /// Rebased WasmGC type index of `(array (ref null text_frame))` text array elements.
    pub array_text_elems_ty: u32,
    /// Rebased WasmGC type index of `(array (ref null eq))` `ref(T)` array elements.
    pub array_object_elems_ty: u32,
    /// Rebased WasmGC type index of the integer/boolean array descriptor struct.
    pub array_i64_ty: u32,
    /// Rebased WasmGC type index of the real array descriptor struct.
    pub array_f64_ty: u32,
    /// Rebased WasmGC type index of the text array descriptor struct.
    pub array_text_ty: u32,
    /// Rebased WasmGC type index of the `ref(T)` array descriptor struct
    /// (a subtype of `array_i64_ty` — see [`GcTypeRegistry::array_object`]).
    pub array_object_ty: u32,
    /// Rebased WasmGC type index of the parked-frame ref spine
    /// (see [`GcTypeRegistry::spill_refs_array`]).
    pub spill_refs_array_ty: u32,
    /// Rebased WasmGC type index of the sequencing side record
    /// (see [`GcTypeRegistry::seq_gc_slot`]).
    pub seq_gc_slot_ty: u32,
    /// Rebased WasmGC type index of the sequencing side-record registry
    /// (see [`GcTypeRegistry::seq_gc_registry`]).
    pub seq_gc_registry_ty: u32,
    /// Rebased WasmGC type index of the SQS process spine
    /// (see [`GcTypeRegistry::sim_notice_procs`]).
    pub sim_notice_procs_ty: u32,
}

impl GcEmitCtx {
    pub fn from_registry(registry: &GcTypeRegistry, base: u32, mir: &crate::mir::Module) -> Self {
        let mut by_class_id = HashMap::new();
        let mut by_class_name = HashMap::new();
        for layout in &mir.class_layouts {
            if let Some(id) = registry.class_type(&layout.declared_name) {
                let wasm_ty = rebased_index(id, base);
                let mut fields_by_offset = HashMap::new();
                fields_by_offset.insert(0, (0u32, LayoutFieldType::I64));
                for field in &layout.fields {
                    fields_by_offset.insert(
                        field.offset,
                        (slot_index_for_offset(field.offset), field.ty),
                    );
                }
                let info = ClassGcInfo {
                    wasm_ty,
                    fields_by_offset,
                    linkage_family: layout_is_linkage_family(layout),
                };
                by_class_id.insert(layout.class_id, info.clone());
                by_class_name.insert(layout.declared_name.to_ascii_lowercase(), info.clone());
                by_class_name.insert(layout.name.to_ascii_lowercase(), info);
            }
        }
        let linkage_base_ty = registry.linkage_base_id().map(|id| rebased_index(id, base));
        let class_base_ty = registry.class_base_id().map(|id| rebased_index(id, base));
        let mut supertypes = HashMap::new();
        let mut field_counts = HashMap::new();
        let mut linkage_ref_types = std::collections::HashSet::new();
        let mut field_is_ref = HashMap::new();
        for raw in 0..registry.len() {
            let id = GcTypeId(raw);
            let ty = rebased_index(id, base);
            if let Some(count) = registry.field_count(id) {
                field_counts.insert(ty, count);
            }
            if let Some(supertype) = registry.supertype_of(id) {
                supertypes.insert(ty, rebased_index(supertype, base));
            }
            if registry.has_linkage_ref_fields(id) {
                linkage_ref_types.insert(ty);
            }
            if let Some(fields) = registry.struct_fields(id) {
                field_is_ref.insert(
                    ty,
                    fields
                        .iter()
                        .map(|field| field.element_type != StorageType::Val(ValType::I64))
                        .collect(),
                );
            }
        }
        // `populate_from_module` (called before this constructor whenever
        // WasmGC types are emitted at all) always requests these builtins, so
        // every id here is expected to already exist.
        let rebase = |id: Option<GcTypeId>, what: &str| -> u32 {
            rebased_index(
                id.unwrap_or_else(|| panic!("GcEmitCtx::from_registry: missing builtin {what}")),
                base,
            )
        };
        Self {
            base,
            linkage_base_ty,
            class_base_ty,
            supertypes,
            field_counts,
            linkage_ref_types,
            field_is_ref,
            by_class_id,
            by_class_name,
            text_chars_ty: rebase(registry.text_chars_id(), "text_chars"),
            text_frame_ty: rebase(registry.text_frame_id(), "text_frame"),
            bounds_array_ty: rebase(registry.bounds_array_id(), "bounds_array"),
            array_i64_elems_ty: rebase(registry.array_i64_elems_id(), "array_i64_elems"),
            array_f64_elems_ty: rebase(registry.array_f64_elems_id(), "array_f64_elems"),
            array_text_elems_ty: rebase(registry.array_text_elems_id(), "array_text_elems"),
            array_object_elems_ty: rebase(registry.array_object_elems_id(), "array_object_elems"),
            array_i64_ty: rebase(registry.array_i64_id(), "array_i64"),
            array_f64_ty: rebase(registry.array_f64_id(), "array_f64"),
            array_text_ty: rebase(registry.array_text_id(), "array_text"),
            array_object_ty: rebase(registry.array_object_id(), "array_object"),
            spill_refs_array_ty: rebase(registry.spill_refs_id(), "spill_refs_array"),
            seq_gc_slot_ty: rebase(registry.seq_gc_slot_id(), "seq_gc_slot"),
            seq_gc_registry_ty: rebase(registry.seq_gc_registry_id(), "seq_gc_registry"),
            sim_notice_procs_ty: rebase(registry.sim_notice_procs_id(), "sim_notice_procs"),
        }
    }

    /// Synthetic [`ClassGcInfo`] for the shared `linkage_base` struct itself
    /// (`class_id`, `suc`, `pred` at field indices 0/1/2 — see
    /// [`GcTypeRegistry::ensure_linkage_base`]). Any linkage-family class's
    /// `class_id`/`suc`/`pred` access can go through this instead of that
    /// class's own final struct type, which only the exact same leaf class
    /// satisfies (see [`ClassGcInfo::is_linkage_family`]).
    pub fn linkage_base_info(&self) -> Option<ClassGcInfo> {
        let wasm_ty = self.linkage_base_ty?;
        let mut fields_by_offset = HashMap::new();
        fields_by_offset.insert(0, (0u32, LayoutFieldType::I64));
        fields_by_offset.insert(SIMSET_SUC_OFFSET, (1u32, LayoutFieldType::ObjectRef));
        fields_by_offset.insert(SIMSET_PRED_OFFSET, (2u32, LayoutFieldType::ObjectRef));
        Some(ClassGcInfo {
            wasm_ty,
            fields_by_offset,
            linkage_family: true,
        })
    }

    /// Whether a `struct.get`/`struct.set` naming `(ty, field_index)` moves a
    /// `(ref null eq)` ring pointer rather than an `i64` slot word.
    pub fn is_direct_link_field(&self, ty: u32, field_index: u32) -> bool {
        is_linkage_ref_field(field_index) && self.linkage_ref_types.contains(&ty)
    }

    /// Whether field `field_index` of `ty` is a WasmGC reference (eqref,
    /// text-frame, array descriptor, …) rather than an `i64` word.
    pub fn field_is_wasm_ref(&self, ty: u32, field_index: u32) -> bool {
        self.field_is_ref
            .get(&ty)
            .and_then(|fields| fields.get(field_index as usize))
            .copied()
            .unwrap_or(false)
    }

    /// The shallowest ancestor of `ty` that still declares `field_index` —
    /// the type a `ref.cast` guarding that field access should name.
    ///
    /// A field index is a byte-offset slot number, and every class in a
    /// prefix family agrees on byte offsets, so the ancestor addresses the
    /// exact same slot while accepting *any* relative of the statically
    /// guessed class at runtime. That matters because the static guess is
    /// routinely a sibling or a subclass of what the local really holds:
    /// `Local::class_qual` is one mutable slot per `LocalId` that a later
    /// `:-` overwrites (simtst45's `ra IS A` after `ra :- new B`), a
    /// prefixed method body runs for every descendant (simtst98's `a`
    /// procedures under `new z`), and [`resolve_gc_field`]'s
    /// offset-only fallback picks the widest owner it can find. Walking to
    /// the ancestor turns all three from a trap into a correct read; slot 0
    /// (`class_id`) always walks the whole way up to
    /// [`GcTypeRegistry::ensure_class_base`], which every class extends.
    ///
    /// [`resolve_gc_field`]: super::wasm
    pub fn cast_target(&self, ty: u32, field_index: u32) -> u32 {
        let want_ref = self.field_is_wasm_ref(ty, field_index);
        let mut best = ty;
        // The chain is finite by construction; bound the walk anyway so a
        // future cyclic registration can't hang codegen.
        for _ in 0..MAX_SUPERTYPE_DEPTH {
            let Some(&parent) = self.supertypes.get(&best) else {
                break;
            };
            match self.field_counts.get(&parent) {
                Some(&count) if count > field_index => {
                    if self.field_is_wasm_ref(parent, field_index) != want_ref {
                        break;
                    }
                    best = parent;
                }
                _ => break,
            }
        }
        best
    }

    pub fn text_frame_ref(&self) -> ValType {
        concrete_ref_null(self.text_frame_ty)
    }

    pub fn text_frame_heap(&self) -> HeapType {
        concrete_heap(self.text_frame_ty)
    }

    pub fn text_chars_heap(&self) -> HeapType {
        concrete_heap(self.text_chars_ty)
    }

    pub fn array_i64_ref(&self) -> ValType {
        concrete_ref_null(self.array_i64_ty)
    }

    pub fn array_f64_ref(&self) -> ValType {
        concrete_ref_null(self.array_f64_ty)
    }

    pub fn array_text_ref(&self) -> ValType {
        concrete_ref_null(self.array_text_ty)
    }

    pub fn array_object_ref(&self) -> ValType {
        concrete_ref_null(self.array_object_ty)
    }

    pub fn class_type_for_id(&self, class_id: i64) -> Option<u32> {
        self.by_class_id.get(&class_id).map(|info| info.wasm_ty)
    }

    pub fn class_type_for_name(&self, name: &str) -> Option<u32> {
        self.class_info_for_name(name).map(|info| info.wasm_ty)
    }

    pub fn class_info_for_id(&self, class_id: i64) -> Option<&ClassGcInfo> {
        self.by_class_id.get(&class_id)
    }

    pub fn class_info_for_name(&self, name: &str) -> Option<&ClassGcInfo> {
        let key = name.to_ascii_lowercase();
        if let Some(info) = self.by_class_name.get(&key) {
            return Some(info);
        }
        // `ref(Point@1)` / span-qualified names → declared stem.
        let declared = crate::layout::declared_class_name(name).to_ascii_lowercase();
        self.by_class_name.get(&declared)
    }
}

/// Accumulates WasmGC type definitions for one compilation unit.
#[derive(Debug, Default)]
pub struct GcTypeRegistry {
    next: u32,
    /// Encoded composite types, in index order.
    encoded: Vec<EncodedType>,
    text_chars: Option<GcTypeId>,
    text_frame: Option<GcTypeId>,
    bounds_array: Option<GcTypeId>,
    array_i64_elems: Option<GcTypeId>,
    array_f64_elems: Option<GcTypeId>,
    array_text_elems: Option<GcTypeId>,
    array_object_elems: Option<GcTypeId>,
    array_i64: Option<GcTypeId>,
    array_f64: Option<GcTypeId>,
    array_text: Option<GcTypeId>,
    array_object: Option<GcTypeId>,
    spill_refs: Option<GcTypeId>,
    seq_gc_slot: Option<GcTypeId>,
    seq_gc_registry: Option<GcTypeId>,
    sim_notice_procs: Option<GcTypeId>,
    /// `{ i64 class_id, eqref suc, eqref pred }` — see [`Self::ensure_linkage_base`].
    linkage_base: Option<GcTypeId>,
    /// The slot ladder: entry `n - 1` is a non-final struct of `n` `i64`
    /// fields declared `sub` entry `n - 2`. See [`Self::ensure_slot_ladder`].
    slot_ladder: Vec<GcTypeId>,
    /// Per-prefix eqref rungs inserted between a linkage prefix (`link`,
    /// `linkage_base`, …) and a subclass that adds trailing eqref slots.
    /// Keyed by the prefix's type id; `rungs[i]` has `prefix_width + i + 1`
    /// fields (the extra ones are eqref). See [`Self::ensure_linkage_eqref_extension`].
    linkage_eqref_ext: HashMap<u32, Vec<GcTypeId>>,
    /// Keyed by [`ClassLayout::declared_name`] (case-insensitive via lowercased key).
    classes: HashMap<String, GcTypeId>,
}

#[derive(Debug)]
enum EncodedType {
    Array {
        element: StorageType,
        mutable: bool,
    },
    Struct {
        fields: Vec<WasmFieldType>,
        is_final: bool,
    },
    StructSubtype {
        supertype_idx: u32,
        fields: Vec<WasmFieldType>,
        is_final: bool,
    },
}

impl GcTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc(&mut self, encoded: EncodedType) -> GcTypeId {
        let id = GcTypeId(self.next);
        self.next += 1;
        self.encoded.push(encoded);
        id
    }

    /// `(array i8)` — TEXTOBJ character storage.
    pub fn text_chars(&mut self) -> GcTypeId {
        if let Some(id) = self.text_chars {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::I8,
            mutable: true,
        });
        self.text_chars = Some(id);
        id
    }

    /// Text frame struct (see module docs).
    ///
    /// Fields, in order: `chars` (shared `(array i8)` — the whole "main"
    /// object's character storage), `start` (1-based offset of this view
    /// *into* `chars`; a `sub`-text shares `chars` with its parent and only
    /// shifts `start`/`length`), `length`, `pos`, `constant` (the Simula
    /// `constant` attribute — bump mode packs this into [`FRAME_OFF_PAD`] in
    /// `src/codegen/wasm.rs`; kept as a real field here rather than an unused
    /// pad word).
    pub fn text_frame(&mut self) -> GcTypeId {
        if let Some(id) = self.text_frame {
            return id;
        }
        let chars = self.text_chars();
        let id = self.alloc(EncodedType::Struct {
            fields: vec![
                chars.field_ref_null(),
                field_val(ValType::I32), // start (1-based in Simula; stored as i32)
                field_val(ValType::I32), // length
                field_val(ValType::I32), // pos
                field_val(ValType::I32), // constant
            ],
            is_final: true,
        });
        self.text_frame = Some(id);
        id
    }

    fn bounds_array(&mut self) -> GcTypeId {
        if let Some(id) = self.bounds_array {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(ValType::I64),
            mutable: true,
        });
        self.bounds_array = Some(id);
        id
    }

    fn array_elems_i64(&mut self) -> GcTypeId {
        if let Some(id) = self.array_i64_elems {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(ValType::I64),
            mutable: true,
        });
        self.array_i64_elems = Some(id);
        id
    }

    fn array_elems_f64(&mut self) -> GcTypeId {
        if let Some(id) = self.array_f64_elems {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(ValType::F64),
            mutable: true,
        });
        self.array_f64_elems = Some(id);
        id
    }

    fn array_elems_text(&mut self) -> GcTypeId {
        if let Some(id) = self.array_text_elems {
            return id;
        }
        let frame = self.text_frame();
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(frame.ref_null()),
            mutable: true,
        });
        self.array_text_elems = Some(id);
        id
    }

    fn array_elems_object(&mut self) -> GcTypeId {
        if let Some(id) = self.array_object_elems {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(anyref_null()),
            mutable: true,
        });
        self.array_object_elems = Some(id);
        id
    }

    fn array_descriptor_fields(&mut self, elems: GcTypeId) -> Vec<WasmFieldType> {
        let bounds = self.bounds_array();
        vec![
            elems.field_ref_null(),
            field_val(ValType::I32), // ndims
            bounds.field_ref_null(),
        ]
    }

    fn array_descriptor(&mut self, elems: GcTypeId) -> GcTypeId {
        let fields = self.array_descriptor_fields(elems);
        self.alloc(EncodedType::Struct {
            fields,
            is_final: true,
        })
    }

    /// Integer / boolean / character array descriptor.
    ///
    /// Registered non-final so [`Self::array_object`] can extend it: a
    /// `ref(T) array` is an `ArrayI64` MIR value, so both descriptors have to
    /// fit the one `(ref null $array_i64)` wasm type that MIR type maps to.
    pub fn array_i64(&mut self) -> GcTypeId {
        if let Some(id) = self.array_i64 {
            return id;
        }
        let elems = self.array_elems_i64();
        let fields = self.array_descriptor_fields(elems);
        let id = self.alloc(EncodedType::Struct {
            fields,
            is_final: false,
        });
        self.array_i64 = Some(id);
        id
    }

    /// Real array descriptor.
    pub fn array_f64(&mut self) -> GcTypeId {
        if let Some(id) = self.array_f64 {
            return id;
        }
        let elems = self.array_elems_f64();
        let id = self.array_descriptor(elems);
        self.array_f64 = Some(id);
        id
    }

    /// Text array descriptor.
    pub fn array_text(&mut self) -> GcTypeId {
        if let Some(id) = self.array_text {
            return id;
        }
        let elems = self.array_elems_text();
        let id = self.array_descriptor(elems);
        self.array_text = Some(id);
        id
    }

    /// `ref(T) array` descriptor: [`Self::array_i64`] plus a trailing
    /// `(array (mut (ref null eq)))` element spine (Phase 4-R4).
    ///
    /// A `ref(T) array` has no MIR array type of its own — it is an
    /// `ArrayI64` whose `Function::array_elem_ty` is `ObjectRef` — so its
    /// descriptor travels through locals, parameters and indirect-call
    /// signatures all typed `(ref null $array_i64)` from `MirType::ArrayI64`
    /// alone. Declaring it a WasmGC **subtype** of `array_i64` is what keeps
    /// those assignments valid while still giving element access a typed
    /// spine the host collector traces directly (no root handles). Field 0
    /// (`array_i64`'s `i64` spine) stays null on these descriptors; codegen
    /// casts to this type and uses [`ARRAY_DESC_FIELD_OBJECT_ELEMS`] instead.
    pub fn array_object(&mut self) -> GcTypeId {
        if let Some(id) = self.array_object {
            return id;
        }
        let base = self.array_i64();
        let i64_elems = self.array_elems_i64();
        let object_elems = self.array_elems_object();
        let mut fields = self.array_descriptor_fields(i64_elems);
        fields.push(object_elems.field_ref_null());
        let id = self.alloc(EncodedType::StructSubtype {
            supertype_idx: base.as_u32(),
            fields,
            is_final: true,
        });
        self.array_object = Some(id);
        id
    }

    /// Mutable `(array (ref null eq))` — Phase 4-R2 parked-frame ref spine.
    ///
    /// Always emitted into wasm modules (see [`populate_from_module`]). Live
    /// `SPILL_STORE_REF` / `SPILL_LOAD_REF` retarget to `array.set` /
    /// `array.get` on this type. The spine itself hangs off the owning
    /// coroutine's [`seq_gc_slot`](Self::seq_gc_slot) (Phase 4-R4), so no
    /// root handle names it.
    pub fn spill_refs_array(&mut self) -> GcTypeId {
        if let Some(id) = self.spill_refs {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(anyref_null()),
            mutable: true,
        });
        self.spill_refs = Some(id);
        id
    }

    /// `{ (mut eqref) object, (mut (ref null $spill_refs_array)) spine }` —
    /// the host-traced side record of one sequencing record (Phase 4-R4).
    ///
    /// A chapter 7 coroutine record and a component record both live in
    /// linear memory, so neither can hold a WasmGC reference: `CORO_ARG`,
    /// `COMP_OBJECT` and the parked-frame ref spine used to be `i64` indices
    /// into a shared handle table. This struct is where those three now live
    /// instead, reached from the linear record through a scalar slot index
    /// into [`seq_gc_registry`](Self::seq_gc_registry).
    ///
    /// One struct serves both record kinds rather than two: a component's
    /// `object` and a coroutine's `arg` are the same shape, components are
    /// not 1:1 with coroutines (a prefixed block instance registers a
    /// component with no coroutine at all), and a component slot simply
    /// leaves `spine` null. That keeps the registry, its growth path and
    /// the load/store helpers single-copy.
    pub fn seq_gc_slot(&mut self) -> GcTypeId {
        if let Some(id) = self.seq_gc_slot {
            return id;
        }
        let spine = self.spill_refs_array();
        let id = self.alloc(EncodedType::Struct {
            fields: vec![field_val(anyref_null()), spine.field_ref_null()],
            is_final: true,
        });
        self.seq_gc_slot = Some(id);
        id
    }

    /// `(array (mut (ref null $seq_gc_slot)))` — the growable registry a
    /// linear sequencing record indexes to find its
    /// [`seq_gc_slot`](Self::seq_gc_slot).
    ///
    /// Held by a mutable module global and replaced wholesale (allocate,
    /// `array.copy`, `global.set`) when it fills, exactly as the ref spine
    /// grows. Slot indices are therefore stable for the life of the run.
    pub fn seq_gc_registry(&mut self) -> GcTypeId {
        if let Some(id) = self.seq_gc_registry {
            return id;
        }
        let slot = self.seq_gc_slot();
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(slot.ref_null()),
            mutable: true,
        });
        self.seq_gc_registry = Some(id);
        id
    }

    /// `(array (mut (ref null eq)))` — the SQS's process column (Phase 4-R4).
    ///
    /// An event notice is three words in linear memory (`evtime`, `process`,
    /// `seq`), and the middle one is a Simula reference, so it was an `i64`
    /// root-handle index. Splitting the column out into its own spine keeps
    /// the two scalars where they are — the SQS is sorted by them, and the
    /// shift/compaction loops are plain word copies — while the references
    /// become host-traced array elements indexed by the *same* notice index.
    ///
    /// A separate type from [`spill_refs_array`](Self::spill_refs_array),
    /// though structurally identical: the two grow on different schedules
    /// (per-coroutine versus once for the whole simulation) and naming them
    /// apart keeps a `ref.cast` mistake between them impossible to write.
    pub fn sim_notice_procs(&mut self) -> GcTypeId {
        if let Some(id) = self.sim_notice_procs {
            return id;
        }
        let id = self.alloc(EncodedType::Array {
            element: StorageType::Val(anyref_null()),
            mutable: true,
        });
        self.sim_notice_procs = Some(id);
        id
    }

    /// Grow the **slot ladder** to at least `slots` rungs and return the
    /// `slots`-wide one (capped at [`MAX_SLOT_LADDER`]; `None` for 0).
    ///
    /// Rung `n` is a non-final struct of `n` `i64` fields declared `sub`
    /// rung `n - 1`, so every class of `n` slots can hang off rung `n` and
    /// is thereby a subtype of *every* narrower rung. Reading slot `k` of
    /// an object then only needs a cast to rung `k + 1`, which any object
    /// with enough slots satisfies no matter what its class is — and some
    /// accesses genuinely have no class to name: `lower.rs` shares one
    /// `__simrt_name_get_field_<offset>` thunk per byte offset across
    /// every class whose attribute is passed as a `name` parameter.
    ///
    /// Since class slot counts follow byte offsets, rung 1 is the universal
    /// `{ class_id }` root ([`Self::class_base_id`]). SIMSET ring members do
    /// *not* ride the ladder: their SUC/PRED slots are typed refs, so they
    /// hang off [`Self::ensure_linkage_base`] instead. Trailing eqref attrs
    /// on ring subclasses ride [`Self::ensure_linkage_eqref_extension`].
    fn ensure_slot_ladder(&mut self, slots: usize) -> Option<GcTypeId> {
        let wanted = slots.min(MAX_SLOT_LADDER);
        while self.slot_ladder.len() < wanted {
            let width = self.slot_ladder.len() + 1;
            let fields = vec![field_val(ValType::I64); width];
            let id = match self.slot_ladder.last().copied() {
                Some(parent) => self.alloc(EncodedType::StructSubtype {
                    supertype_idx: parent.as_u32(),
                    fields,
                    is_final: false,
                }),
                None => self.alloc(EncodedType::Struct {
                    fields,
                    is_final: false,
                }),
            };
            self.slot_ladder.push(id);
        }
        self.slot_ladder.get(wanted.checked_sub(1)?).copied()
    }

    /// Shared ancestor for linkage-family trailing eqref slots.
    ///
    /// `town.nam_` (Text) and `townpoint.t` (`ref(town)`) both live at
    /// offset 24 as eqref, but they are sibling `link` subclasses — neither
    /// is a subtype of the other. Casting to `town` to read field 3 therefore
    /// traps on a `townpoint` (simtst96). Inserting non-final rungs
    /// `{ prefix…, eqref, eqref, … }` under the Simula prefix gives
    /// [`GcEmitCtx::cast_target`] a type both siblings satisfy.
    ///
    /// `width` is the total field count of the rung (prefix fields plus the
    /// extra eqrefs). Capped so the chain stays inside [`MAX_SLOT_LADDER`].
    fn ensure_linkage_eqref_extension(&mut self, parent: GcTypeId, width: usize) -> GcTypeId {
        let parent_width =
            self.field_count(parent)
                .expect("linkage eqref extension parent is a struct") as usize;
        if width <= parent_width {
            return parent;
        }
        let extra = (width - parent_width).min(MAX_SLOT_LADDER.saturating_sub(parent_width.max(1)));
        if extra == 0 {
            return parent;
        }
        let key = parent.as_u32();
        let existing = self.linkage_eqref_ext.get(&key).map(Vec::len).unwrap_or(0);
        for _ in existing..extra {
            let prev = self
                .linkage_eqref_ext
                .get(&key)
                .and_then(|rungs| rungs.last().copied())
                .unwrap_or(parent);
            let mut fields = self
                .struct_fields(prev)
                .expect("linkage eqref rung is a struct")
                .to_vec();
            fields.push(field_val(anyref_val()));
            let id = self.alloc(EncodedType::StructSubtype {
                supertype_idx: prev.as_u32(),
                fields,
                is_final: false,
            });
            self.linkage_eqref_ext.entry(key).or_default().push(id);
        }
        self.linkage_eqref_ext[&key][extra - 1]
    }

    /// If `child_fields` continues `parent` with one or more eqref slots
    /// (the simtst96 `nam_` / `t` case), return the widest shared rung
    /// those slots can be accessed through. `None` when there is nothing
    /// to insert — the child already extends `parent` directly, or the
    /// next slot is an `i64` word (`gone`, a real, …).
    fn linkage_eqref_bridge(
        &mut self,
        parent: GcTypeId,
        child_fields: &[WasmFieldType],
    ) -> Option<GcTypeId> {
        let parent_n = self.field_count(parent)? as usize;
        if parent_n < LINKAGE_BASE_FIELDS || parent_n >= child_fields.len() {
            return None;
        }
        let extra = child_fields[parent_n..]
            .iter()
            .take_while(|field| field.element_type == StorageType::Val(anyref_val()))
            .count();
        if extra == 0 {
            return None;
        }
        Some(self.ensure_linkage_eqref_extension(parent, parent_n + extra))
    }

    /// Rung 1 of the slot ladder — the universal `{ class_id }` supertype —
    /// once any class has been registered.
    pub fn class_base_id(&self) -> Option<GcTypeId> {
        self.slot_ladder.first().copied()
    }

    /// Declared field count of a struct type (`None` for arrays / unknown ids).
    pub fn field_count(&self, id: GcTypeId) -> Option<u32> {
        self.struct_fields(id).map(|fields| fields.len() as u32)
    }

    /// Whether `id` is `linkage_base` or a subtype of it — i.e. whether
    /// fields 1 and 2 are the shared SIMSET ring pointers. An ordinary class
    /// whose first two attributes happen to be typed `eqref`s must **not**
    /// count: those are user `ref`s, not `SUC`/`PRED` (simtst47).
    pub fn has_linkage_ref_fields(&self, id: GcTypeId) -> bool {
        let Some(base) = self.linkage_base else {
            return false;
        };
        if id == base {
            return true;
        }
        let mut cur = id;
        for _ in 0..MAX_SUPERTYPE_DEPTH {
            match self.supertype_of(cur) {
                Some(parent) if parent == base => return true,
                Some(parent) => cur = parent,
                None => return false,
            }
        }
        false
    }

    /// Immediate WasmGC supertype of a `StructSubtype`, if any.
    pub fn supertype_of(&self, id: GcTypeId) -> Option<GcTypeId> {
        match self.encoded.get(id.as_u32() as usize)? {
            EncodedType::StructSubtype { supertype_idx, .. } => Some(GcTypeId(*supertype_idx)),
            _ => None,
        }
    }

    /// The shared SIMSET supertype `{ i64 class_id, eqref suc, eqref pred }`,
    /// once [`Self::ensure_linkage_base`] has been asked for it.
    pub fn linkage_base_id(&self) -> Option<GcTypeId> {
        self.linkage_base
    }

    /// `{ i64 class_id, (ref null eq) suc, (ref null eq) pred }` — the shared
    /// supertype every SIMSET ring member reaches (Phase 4-R2).
    ///
    /// Unlike the all-`i64` [slot ladder](Self::ensure_slot_ladder), the two
    /// ring pointers are *real* WasmGC references: a SIMSET ring is the one
    /// place where every value a slot can hold is known to be an object, so
    /// there is nothing to box through the root-handle table and the host
    /// collector can trace the ring directly. It extends
    /// [`class_base`](Self::class_base_id) (rung 1, `{ i64 class_id }`) so
    /// class-agnostic `class_id` reads still work on linkage objects.
    pub fn ensure_linkage_base(&mut self) -> GcTypeId {
        if let Some(id) = self.linkage_base {
            return id;
        }
        let class_base = self
            .ensure_slot_ladder(1)
            .expect("slot ladder rung 1 always exists");
        let id = self.alloc(EncodedType::StructSubtype {
            supertype_idx: class_base.as_u32(),
            fields: linkage_base_fields(),
            is_final: false,
        });
        self.linkage_base = Some(id);
        id
    }

    /// WasmGC storage for the slot at `offset` in `layout`.
    ///
    /// Linkage `SUC`/`PRED` are eqrefs. Declared ObjectRef attributes and
    /// `ref_cell` capture slots are eqrefs. Non-linkage Text/Array attributes
    /// use their precise descriptor types. Linkage-family trailing ref slots
    /// are **uniform eqref** (not `text_frame`): siblings in one ring
    /// (`town.nam_` vs `townpoint.t`) share offsets with different layout
    /// types, and a typed text-frame at offset 24 makes `ref.cast` to `town`
    /// trap on a `townpoint` (simtst96).
    fn wasm_field_for_slot(
        &mut self,
        layout: &ClassLayout,
        offset: i64,
        is_linkage: bool,
    ) -> WasmFieldType {
        if offset == 0 {
            return field_val(ValType::I64);
        }
        if is_linkage && (offset == SIMSET_SUC_OFFSET || offset == SIMSET_PRED_OFFSET) {
            return field_val(anyref_val());
        }
        let Some(field) = layout.fields.iter().find(|field| field.offset == offset) else {
            return field_val(ValType::I64);
        };
        if is_linkage {
            return if field_is_gc_ref(field.ty) {
                field_val(anyref_val())
            } else {
                field_val(ValType::I64)
            };
        }
        match field.ty {
            LayoutFieldType::ObjectRef => field_val(anyref_val()),
            LayoutFieldType::Text => self.text_frame().field_ref_null(),
            LayoutFieldType::ArrayI64 | LayoutFieldType::ArrayBool => {
                self.array_i64().field_ref_null()
            }
            LayoutFieldType::ArrayF64 => self.array_f64().field_ref_null(),
            LayoutFieldType::ArrayText => self.array_text().field_ref_null(),
            LayoutFieldType::I64 | LayoutFieldType::Bool | LayoutFieldType::F64 => {
                field_val(ValType::I64)
            }
        }
    }

    /// Register (or look up) a class layout as a WasmGC struct.
    ///
    /// The struct is a flat run of `i64` **slots**, one per 8 bytes of
    /// [`ClassLayout::size`]: slot 0 is `class_id`, and an attribute at byte
    /// offset `n` is slot `n / 8` ([`slot_index_for_offset`]). Slots the
    /// layout leaves empty are still declared, and `boolean`/`real`
    /// attributes occupy an `i64` slot too (the value is zero-extended /
    /// bit-reinterpreted by `emit_field_load_gc`/`emit_field_store_gc` in
    /// `wasm.rs`) rather than getting a native `i32`/`f64` field.
    ///
    /// That uniformity is what makes WasmGC subtyping usable here at all.
    /// Simula concatenation puts a prefix's attributes at the same byte
    /// offsets in every subclass, and `layout.rs`'s
    /// `align_enclosing_captures_across_prefix_families` does the same for
    /// enclosing-capture slots — but only *by byte offset*. A per-attribute
    /// field list breaks that alignment as soon as a subclass declares
    /// attributes of its own: they shift every following capture down by a
    /// field index, and a `boolean` capture landing where the prefix has a
    /// gap turns an `i32` field into an `i64` one. Either way the subclass
    /// stops being a structural extension of its prefix, so it had to be
    /// registered as an unrelated final struct and every access through a
    /// `ref(prefix)` qualifier trapped (simtst98: `a`'s outlined procedures
    /// running against a `new z`). Slot-indexed `i64` fields make the
    /// prefix's field list an exact prefix of the subclass's by
    /// construction, so the whole family really is one WasmGC subtype
    /// chain.
    ///
    /// `prefix_targets` is the set of (lowercased) declared class names that
    /// appear as *some* class's `ClassLayout.prefix` — i.e. every class that
    /// has at least one direct subclass — computed once up front from the
    /// full, already class_id-sorted `mir.class_layouts` list (content, not
    /// iteration order, so it's deterministic across compiles). Those classes
    /// register as `is_final: false` so a subclass's own struct can declare
    /// them as its WasmGC `sub`-type supertype (Simula's class concatenation
    /// guarantees the prefix's fields are an exact, same-order prefix of the
    /// subclass's own `fields`, satisfying WasmGC's structural subtyping
    /// rule). `mir.class_layouts` is processed in class_id order (assigned in
    /// declaration order — a prefix must be declared before its subclass —
    /// see `layout.rs`), so a class's prefix is always already registered by
    /// the time this runs, making the lookup below unconditional-safe.
    ///
    /// Subtyping makes a wrong `ref.cast` fatal where it used to be
    /// harmless (structurally-identical *final* structs canonicalize to one
    /// engine type, so naming the wrong one did nothing), and codegen's
    /// static guess at an object's class is wrong often enough to matter.
    /// Two mitigations carry that weight, both required:
    /// `Op::FieldLoadI64`/`FieldStoreI64` snapshot `class_qual` per
    /// instruction instead of letting a later `:-` rewrite it (see that
    /// field's doc comment), and `wasm.rs` casts to
    /// [`GcEmitCtx::cast_target`] rather than to the guessed class itself.
    pub fn register_class(
        &mut self,
        layout: &ClassLayout,
        prefix_targets: &std::collections::HashSet<String>,
    ) -> GcTypeId {
        let key = layout.declared_name.to_ascii_lowercase();
        if let Some(&id) = self.classes.get(&key) {
            return id;
        }
        // Ensure shared builtins exist before we emit fields that point at them.
        let _ = self.text_frame();
        let _ = self.array_i64();
        let _ = self.array_f64();
        let _ = self.array_text();

        let slots = class_slot_count(layout);
        let is_linkage = layout_is_linkage_family(layout);
        let mut fields = Vec::with_capacity(slots);
        for i in 0..slots {
            let offset = i as i64 * I64_FIELD_SIZE;
            fields.push(self.wasm_field_for_slot(layout, offset, is_linkage));
        }
        let is_final = !prefix_targets.contains(&key);
        let all_i64_after_header = fields
            .iter()
            .skip(1)
            .all(|field| field.element_type == StorageType::Val(ValType::I64));
        let fallback = if is_linkage {
            self.ensure_linkage_base()
        } else if all_i64_after_header {
            self.ensure_slot_ladder(slots)
                .expect("class_slot_count is at least 1")
        } else {
            // Mixed typed-ref fields cannot sit on the all-`i64` slot ladder.
            self.ensure_slot_ladder(1)
                .expect("class_base always exists")
        };
        // The Simula prefix is the more informative supertype (it keeps the
        // family reachable above `MAX_SLOT_LADDER`), but the fallback root is
        // the guaranteed one, so fall back to it whenever the prefix is
        // unregistered or somehow not an extension.
        let preferred = layout
            .prefix
            .as_deref()
            .and_then(|prefix| {
                self.class_type(prefix)
                    .or_else(|| self.class_type(crate::layout::declared_class_name(prefix)))
            })
            .unwrap_or(fallback);
        let mut supertype = if self.is_valid_subtype_extension(preferred, &fields) {
            preferred
        } else {
            fallback
        };
        // Sibling ring classes that add trailing eqrefs at the same offsets
        // (`town.nam_` vs `townpoint.t`) need a shared ancestor wider than
        // `link` / `linkage_base`, or `cast_target` names a leaf and
        // `ref.cast` traps on the other sibling (simtst96).
        if is_linkage {
            if let Some(bridged) = self.linkage_eqref_bridge(supertype, &fields) {
                supertype = bridged;
            }
        }
        let id = self.alloc(EncodedType::StructSubtype {
            supertype_idx: supertype.as_u32(),
            fields,
            is_final,
        });
        self.classes.insert(key, id);
        id
    }

    /// The struct's own field list, for classes registered as `Struct` /
    /// `StructSubtype` (`None` for arrays or an out-of-range id).
    fn struct_fields(&self, id: GcTypeId) -> Option<&[WasmFieldType]> {
        match self.encoded.get(id.as_u32() as usize)? {
            EncodedType::Struct { fields, .. } | EncodedType::StructSubtype { fields, .. } => {
                Some(fields)
            }
            EncodedType::Array { .. } => None,
        }
    }

    /// Whether `id` was registered `is_final: false` — WasmGC forbids
    /// declaring a `sub`-type of a final type regardless of field shape, so
    /// this must hold before [`Self::is_valid_subtype_extension`] alone
    /// would greenlight a `StructSubtype`.
    fn is_open_for_subtyping(&self, id: GcTypeId) -> bool {
        matches!(
            self.encoded.get(id.as_u32() as usize),
            Some(EncodedType::Struct {
                is_final: false,
                ..
            }) | Some(EncodedType::StructSubtype {
                is_final: false,
                ..
            })
        )
    }

    /// Whether `child_fields` is a valid WasmGC subtype extension of
    /// `supertype`'s own fields — i.e. `supertype`'s fields appear, in the
    /// same order with matching storage type and mutability, as an exact
    /// prefix of `child_fields`. Simula prefix concatenation guarantees this
    /// *for a class's own declared attributes*, but enclosing-capture slots
    /// are placed by a separate `align_enclosing_captures_across_prefix_families`
    /// pass in `layout.rs` that aligns them by **byte offset**, padding for
    /// the *native* backend's raw-offset addressing — it doesn't (and can't,
    /// without also emitting explicit padding fields) guarantee the WasmGC
    /// struct's *field-index* prefix stays aligned once a subclass adds new
    /// attributes of its own between the two (simtst32/45/48/55/64/65: `A`
    /// has `[ia, captures...]`, `B` has `[ia, ib, captures...]` — `ib` shifts
    /// every capture over by one field index, so `B`'s struct is genuinely
    /// *not* an extension of `A`'s and declaring it `sub A` is an invalid
    /// module, not just a runtime type mismatch). Falling back to an
    /// independent final struct for exactly these classes is safe (loses
    /// the `ref(A)`-holding-a-`B`-instance polymorphism for this specific
    /// hierarchy, no different from the pre-subtyping baseline) without
    /// forcing every other class in the program back to that limitation.
    fn is_valid_subtype_extension(
        &self,
        supertype: GcTypeId,
        child_fields: &[WasmFieldType],
    ) -> bool {
        if !self.is_open_for_subtyping(supertype) {
            return false;
        }
        let Some(super_fields) = self.struct_fields(supertype) else {
            return false;
        };
        super_fields.len() <= child_fields.len()
            && super_fields
                .iter()
                .zip(child_fields.iter())
                .all(|(a, b)| a == b)
    }

    pub fn class_type(&self, declared_name: &str) -> Option<GcTypeId> {
        self.classes
            .get(&declared_name.to_ascii_lowercase())
            .copied()
    }

    /// Registry id of [`Self::text_chars`], if it was ever requested.
    pub fn text_chars_id(&self) -> Option<GcTypeId> {
        self.text_chars
    }

    /// Registry id of [`Self::text_frame`], if it was ever requested.
    pub fn text_frame_id(&self) -> Option<GcTypeId> {
        self.text_frame
    }

    /// Registry id of [`Self::bounds_array`], if it was ever requested.
    pub fn bounds_array_id(&self) -> Option<GcTypeId> {
        self.bounds_array
    }

    /// Registry id of the flat `(array i64)` backing [`Self::array_i64`].
    pub fn array_i64_elems_id(&self) -> Option<GcTypeId> {
        self.array_i64_elems
    }

    /// Registry id of the flat `(array f64)` backing [`Self::array_f64`].
    pub fn array_f64_elems_id(&self) -> Option<GcTypeId> {
        self.array_f64_elems
    }

    /// Registry id of the flat `(array (ref null text_frame))` backing
    /// [`Self::array_text`].
    pub fn array_text_elems_id(&self) -> Option<GcTypeId> {
        self.array_text_elems
    }

    /// Registry id of the flat `(array (ref null eq))` backing
    /// [`Self::array_object`].
    pub fn array_object_elems_id(&self) -> Option<GcTypeId> {
        self.array_object_elems
    }

    /// Registry id of [`Self::array_i64`], if it was ever requested.
    pub fn array_i64_id(&self) -> Option<GcTypeId> {
        self.array_i64
    }

    /// Registry id of [`Self::array_f64`], if it was ever requested.
    pub fn array_f64_id(&self) -> Option<GcTypeId> {
        self.array_f64
    }

    /// Registry id of [`Self::array_text`], if it was ever requested.
    pub fn array_text_id(&self) -> Option<GcTypeId> {
        self.array_text
    }

    /// Registry id of [`Self::array_object`], if it was ever requested.
    pub fn array_object_id(&self) -> Option<GcTypeId> {
        self.array_object
    }

    /// Registry id of [`Self::spill_refs_array`], if it was ever requested.
    pub fn spill_refs_id(&self) -> Option<GcTypeId> {
        self.spill_refs
    }

    /// Registry id of [`Self::seq_gc_slot`], if it was ever requested.
    pub fn seq_gc_slot_id(&self) -> Option<GcTypeId> {
        self.seq_gc_slot
    }

    /// Registry id of [`Self::seq_gc_registry`], if it was ever requested.
    pub fn seq_gc_registry_id(&self) -> Option<GcTypeId> {
        self.seq_gc_registry
    }

    /// Registry id of [`Self::sim_notice_procs`], if it was ever requested.
    pub fn sim_notice_procs_id(&self) -> Option<GcTypeId> {
        self.sim_notice_procs
    }

    /// Number of composite types defined so far.
    pub fn len(&self) -> u32 {
        self.next
    }

    pub fn is_empty(&self) -> bool {
        self.next == 0
    }

    /// Emit the accumulated types into a fresh [`TypeSection`] (indices from 0).
    pub fn emit_types(&self) -> TypeSection {
        let mut types = TypeSection::new();
        self.append_to(&mut types, 0);
        types
    }

    /// Append these composites onto an existing type section, rebasing every
    /// `HeapType::Concrete` so field references stay valid when function types
    /// already occupy indices `[0, base)`.
    ///
    /// Returns `base` — the wasm type index of registry id 0.
    pub fn append_to(&self, types: &mut TypeSection, base: u32) -> u32 {
        debug_assert_eq!(base, types.len());
        for encoded in &self.encoded {
            match encoded {
                EncodedType::Array { element, mutable } => {
                    let element = rebase_storage(*element, base);
                    types.ty().array(&element, *mutable);
                }
                EncodedType::Struct { fields, is_final } => {
                    let fields: Vec<_> = fields
                        .iter()
                        .map(|field| rebase_field(*field, base))
                        .collect();
                    types.ty().subtype(&SubType {
                        is_final: *is_final,
                        supertype_idx: None,
                        composite_type: CompositeType {
                            shared: false,
                            inner: CompositeInnerType::Struct(StructType {
                                fields: fields.into_boxed_slice(),
                            }),
                        },
                    });
                }
                EncodedType::StructSubtype {
                    supertype_idx,
                    fields,
                    is_final,
                } => {
                    let fields: Vec<_> = fields
                        .iter()
                        .map(|field| rebase_field(*field, base))
                        .collect();
                    types.ty().subtype(&SubType {
                        is_final: *is_final,
                        supertype_idx: Some(base + supertype_idx),
                        composite_type: CompositeType {
                            shared: false,
                            inner: CompositeInnerType::Struct(StructType {
                                fields: fields.into_boxed_slice(),
                            }),
                        },
                    });
                }
            }
        }
        base
    }

    /// Register shared builtins, the spill-refs spine, and every class layout
    /// from a MIR module. Used by wasm codegen (WasmGC is always on).
    pub fn populate_from_module(&mut self, mir: &crate::mir::Module) {
        let _ = self.ensure_slot_ladder(1);
        let _ = self.text_frame();
        let _ = self.array_i64();
        let _ = self.array_f64();
        let _ = self.array_text();
        let _ = self.array_object();
        let _ = self.spill_refs_array();
        let _ = self.seq_gc_registry();
        let _ = self.sim_notice_procs();
        // Unconditional so SIMSET lowering always has a type to name, even in
        // programs whose only ring members come from an unregistered corner.
        let _ = self.ensure_linkage_base();
        // `class_id` is assigned in declaration order (`layout.rs`);
        // `mir.class_layouts` is now sorted by `class_id` at construction
        // (see `mir/lower.rs`), but sort defensively here too so type
        // registration order — and hence every wasm type index — stays
        // fully deterministic across compiles regardless of upstream order.
        let mut layouts: Vec<&ClassLayout> = mir.class_layouts.iter().collect();
        layouts.sort_by_key(|layout| layout.class_id);
        let prefix_targets: std::collections::HashSet<String> = layouts
            .iter()
            .filter_map(|layout| layout.prefix.as_deref())
            .flat_map(|name| {
                [
                    name.to_ascii_lowercase(),
                    crate::layout::declared_class_name(name).to_ascii_lowercase(),
                ]
            })
            .collect();
        let mut by_name: HashMap<String, &ClassLayout> = HashMap::new();
        for layout in &layouts {
            by_name.insert(layout.declared_name.to_ascii_lowercase(), layout);
            by_name
                .entry(crate::layout::declared_class_name(&layout.name).to_ascii_lowercase())
                .or_insert(layout);
        }
        // Declaration order does *not* imply prefix-before-subclass: the
        // SIMSET / SIMULATION runtime classes a program's own `link class`
        // and `process class` declarations sit under are appended after
        // them, so registering in `class_id` order alone would leave
        // `car`/`town` with an unregistered `Process`/`Link` prefix and
        // strand them as bare `linkage_base` siblings, out of reach of each
        // other's slots (simtst96). Pull each prefix in first instead.
        let mut pending = std::collections::HashSet::new();
        for layout in &layouts {
            self.register_class_with_prefix(layout, &by_name, &prefix_targets, &mut pending);
        }
    }

    /// [`Self::register_class`], with `layout`'s prefix chain registered
    /// first so the subtype link can actually be made. `pending` breaks a
    /// cyclic prefix chain (malformed input) instead of recursing forever.
    fn register_class_with_prefix(
        &mut self,
        layout: &ClassLayout,
        by_name: &HashMap<String, &ClassLayout>,
        prefix_targets: &std::collections::HashSet<String>,
        pending: &mut std::collections::HashSet<String>,
    ) -> GcTypeId {
        let key = layout.declared_name.to_ascii_lowercase();
        if let Some(&id) = self.classes.get(&key) {
            return id;
        }
        if pending.insert(key.clone()) {
            if let Some(prefix) = layout.prefix.as_deref() {
                let prefix_key = crate::layout::declared_class_name(prefix).to_ascii_lowercase();
                if let Some(&prefix_layout) = by_name
                    .get(&prefix.to_ascii_lowercase())
                    .or_else(|| by_name.get(&prefix_key))
                {
                    self.register_class_with_prefix(
                        prefix_layout,
                        by_name,
                        prefix_targets,
                        pending,
                    );
                }
            }
            pending.remove(&key);
        }
        self.register_class(layout, prefix_targets)
    }
}

fn rebase_storage(storage: StorageType, base: u32) -> StorageType {
    match storage {
        StorageType::Val(ValType::Ref(ref_ty)) => {
            StorageType::Val(ValType::Ref(rebase_ref(ref_ty, base)))
        }
        other => other,
    }
}

fn rebase_field(field: WasmFieldType, base: u32) -> WasmFieldType {
    WasmFieldType {
        element_type: rebase_storage(field.element_type, base),
        mutable: field.mutable,
    }
}

fn rebase_ref(ref_ty: RefType, base: u32) -> RefType {
    match ref_ty.heap_type {
        HeapType::Concrete(index) => RefType {
            nullable: ref_ty.nullable,
            heap_type: HeapType::Concrete(base + index),
        },
        _ => ref_ty,
    }
}

/// Wasm **always** emits WasmGC types and lowers ObjectRef/Text/Array through
/// host GC. There is no bump-heap object fallback on the wasm target (interp
/// and native keep their own collectors). [`with_force_enabled`] remains for
/// unit tests that need to assert registry behaviour in isolation.
pub fn env_enabled() -> bool {
    if let Some(forced) = FORCE_ENABLED.with(|cell| cell.get()) {
        return forced;
    }
    true
}

std::thread_local! {
    static FORCE_ENABLED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with [`env_enabled`] forced to `enabled` for this thread.
pub fn with_force_enabled<R>(enabled: bool, f: impl FnOnce() -> R) -> R {
    FORCE_ENABLED.with(|cell| {
        let previous = cell.replace(Some(enabled));
        let result = f();
        cell.set(previous);
        result
    })
}

/// Build a tiny WasmGC module that `struct.new`s a registered class and returns
/// `class_id + field0 + field1` truncated to `i32`. Used by Phase 4a host probes
/// (same `probe` export shape as `tests/wasm_gc_probe.rs`).
///
/// `layout` must have exactly two `I64` attribute fields (e.g. Point x/y).
pub fn point_sum_probe_module(layout: &ClassLayout) -> Result<Vec<u8>, String> {
    if layout.fields.len() != 2 || layout.fields.iter().any(|f| f.ty != LayoutFieldType::I64) {
        return Err("point_sum_probe_module expects two I64 fields".into());
    }

    let mut reg = GcTypeRegistry::new();
    let point = reg.register_class(layout, &std::collections::HashSet::new());
    let mut types = reg.emit_types();
    let func_ty = reg.len(); // next free index after composites
    types.ty().function([], [ValType::I32]);

    let mut functions = FunctionSection::new();
    functions.function(func_ty);

    let mut exports = ExportSection::new();
    exports.export("probe", ExportKind::Func, 0);

    let point_ref = point.ref_null();
    let mut body = Function::new([(1, point_ref)]);
    // class_id=1, x=40, y=2 → probe returns 43
    body.instruction(&Instruction::I64Const(1));
    body.instruction(&Instruction::I64Const(40));
    body.instruction(&Instruction::I64Const(2));
    body.instruction(&Instruction::StructNew(point.as_u32()));
    body.instruction(&Instruction::LocalSet(0));
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::StructGet {
        struct_type_index: point.as_u32(),
        field_index: 0,
    });
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::StructGet {
        struct_type_index: point.as_u32(),
        field_index: 1,
    });
    body.instruction(&Instruction::I64Add);
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::StructGet {
        struct_type_index: point.as_u32(),
        field_index: 2,
    });
    body.instruction(&Instruction::I64Add);
    body.instruction(&Instruction::I32WrapI64);
    body.instruction(&Instruction::End);

    let mut code = CodeSection::new();
    code.function(&body);

    let mut module = Module::new();
    module.section(&types);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);
    Ok(module.finish())
}

/// Expected result of [`point_sum_probe_module`].
pub const POINT_SUM_PROBE_EXPECTED: i32 = 43;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Span;
    use crate::layout::FieldLayout;

    fn point_layout() -> ClassLayout {
        ClassLayout {
            name: "Point".into(),
            declared_name: "Point".into(),
            decl_span: Span::default(),
            fields: vec![
                FieldLayout {
                    name: "x".into(),
                    offset: 8,
                    size: 8,
                    ty: LayoutFieldType::I64,
                    class_qual: None,
                },
                FieldLayout {
                    name: "y".into(),
                    offset: 16,
                    size: 8,
                    ty: LayoutFieldType::I64,
                    class_qual: None,
                },
            ],
            methods: vec![],
            virtual_methods: vec![],
            constructor_params: vec![],
            needs_init: false,
            runs_on_own_stack: false,
            enclosing_captures: vec![],
            size: 24,
            class_id: 0,
            system_block: 0,
            prefix: None,
        }
    }

    #[test]
    fn text_frame_and_array_descriptors_are_distinct() {
        let mut reg = GcTypeRegistry::new();
        let chars = reg.text_chars();
        let frame = reg.text_frame();
        let a_i = reg.array_i64();
        let a_f = reg.array_f64();
        let a_t = reg.array_text();
        assert_ne!(chars, frame);
        assert_ne!(a_i, a_f);
        assert_ne!(a_f, a_t);
        // Second calls are stable.
        assert_eq!(reg.text_frame(), frame);
        assert_eq!(reg.array_i64(), a_i);
    }

    #[test]
    fn class_struct_has_class_id_plus_attributes() {
        let mut reg = GcTypeRegistry::new();
        let id = reg.register_class(&point_layout(), &std::collections::HashSet::new());
        assert_eq!(reg.class_type("Point"), Some(id));
        assert_eq!(reg.class_type("point"), Some(id));
        match &reg.encoded[id.as_u32() as usize] {
            EncodedType::Struct { fields, .. } | EncodedType::StructSubtype { fields, .. } => {
                assert_eq!(fields.len(), 3); // class_id, x, y
                assert_eq!(fields[0].element_type, StorageType::Val(ValType::I64));
                assert_eq!(fields[1].element_type, StorageType::Val(ValType::I64));
                assert_eq!(fields[2].element_type, StorageType::Val(ValType::I64));
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn object_ref_attr_is_eqref() {
        let mut reg = GcTypeRegistry::new();
        let layout = ClassLayout {
            name: "Box".into(),
            declared_name: "Box".into(),
            decl_span: Span::default(),
            fields: vec![FieldLayout {
                name: "p".into(),
                offset: 8,
                size: 8,
                ty: LayoutFieldType::ObjectRef,
                class_qual: None,
            }],
            methods: vec![],
            virtual_methods: vec![],
            constructor_params: vec![],
            needs_init: false,
            runs_on_own_stack: false,
            enclosing_captures: vec![],
            size: 16,
            class_id: 1,
            system_block: 0,
            prefix: None,
        };
        let id = reg.register_class(&layout, &std::collections::HashSet::new());
        match &reg.encoded[id.as_u32() as usize] {
            EncodedType::Struct { fields, .. } | EncodedType::StructSubtype { fields, .. } => {
                assert_eq!(fields[1].element_type, StorageType::Val(anyref_val()));
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn text_attr_is_typed_text_frame() {
        let mut reg = GcTypeRegistry::new();
        let layout = ClassLayout {
            name: "Holder".into(),
            declared_name: "Holder".into(),
            decl_span: Span::default(),
            fields: vec![FieldLayout {
                name: "t".into(),
                offset: 8,
                size: 8,
                ty: LayoutFieldType::Text,
                class_qual: None,
            }],
            methods: vec![],
            virtual_methods: vec![],
            constructor_params: vec![],
            needs_init: false,
            runs_on_own_stack: false,
            enclosing_captures: vec![],
            size: 16,
            class_id: 2,
            system_block: 0,
            prefix: None,
        };
        let id = reg.register_class(&layout, &std::collections::HashSet::new());
        let frame = reg.text_frame();
        match &reg.encoded[id.as_u32() as usize] {
            EncodedType::Struct { fields, .. } | EncodedType::StructSubtype { fields, .. } => {
                assert_eq!(fields[1].element_type, StorageType::Val(frame.ref_null()));
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn own_stack_object_ref_capture_is_eqref() {
        let mut reg = GcTypeRegistry::new();
        let layout = ClassLayout {
            name: "Worker".into(),
            declared_name: "Worker".into(),
            decl_span: Span::default(),
            fields: vec![FieldLayout {
                name: "shared".into(),
                offset: 8,
                size: 8,
                ty: LayoutFieldType::ObjectRef,
                class_qual: None,
            }],
            methods: vec![],
            virtual_methods: vec![],
            constructor_params: vec![],
            needs_init: false,
            runs_on_own_stack: true,
            enclosing_captures: vec![("shared".into(), LayoutFieldType::ObjectRef)],
            size: 16,
            class_id: 3,
            system_block: 0,
            prefix: None,
        };
        let id = reg.register_class(&layout, &std::collections::HashSet::new());
        match &reg.encoded[id.as_u32() as usize] {
            EncodedType::Struct { fields, .. } | EncodedType::StructSubtype { fields, .. } => {
                assert_eq!(fields[1].element_type, StorageType::Val(anyref_val()));
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn point_probe_module_encodes() {
        let bytes = point_sum_probe_module(&point_layout()).expect("encode");
        assert!(bytes.len() > 8);
        assert_eq!(&bytes[..4], b"\0asm");
    }

    #[test]
    fn emit_types_section_is_non_empty() {
        let mut reg = GcTypeRegistry::new();
        let _ = reg.register_class(&point_layout(), &std::collections::HashSet::new());
        let types = reg.emit_types();
        let mut module = Module::new();
        module.section(&types);
        let bytes = module.finish();
        assert!(bytes.len() > 8);
    }

    #[test]
    fn array_object_extends_array_i64_with_an_eqref_spine() {
        let mut reg = GcTypeRegistry::new();
        let i64_desc = reg.array_i64();
        let object_desc = reg.array_object();
        assert_ne!(i64_desc, object_desc);
        assert_eq!(reg.array_object(), object_desc);
        // Assignable wherever `MirType::ArrayI64` typed the local.
        assert_eq!(reg.supertype_of(object_desc), Some(i64_desc));
        let fields = reg.struct_fields(object_desc).expect("struct");
        assert_eq!(fields.len() as u32, ARRAY_DESC_FIELD_OBJECT_ELEMS + 1);
        assert_eq!(&fields[..3], reg.struct_fields(i64_desc).expect("struct"));
        let elems = reg.array_object_elems_id().expect("elems registered");
        assert_eq!(
            fields[ARRAY_DESC_FIELD_OBJECT_ELEMS as usize].element_type,
            StorageType::Val(elems.ref_null())
        );
        match &reg.encoded[elems.as_u32() as usize] {
            EncodedType::Array { element, mutable } => {
                assert!(*mutable);
                assert_eq!(*element, StorageType::Val(anyref_null()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn spill_refs_array_is_stable() {
        let mut reg = GcTypeRegistry::new();
        let a = reg.spill_refs_array();
        let b = reg.spill_refs_array();
        assert_eq!(a, b);
        match &reg.encoded[a.as_u32() as usize] {
            EncodedType::Array { element, mutable } => {
                assert!(*mutable);
                assert_eq!(*element, StorageType::Val(anyref_null()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn seq_gc_slot_holds_an_object_and_a_spine_and_the_registry_indexes_it() {
        let mut reg = GcTypeRegistry::new();
        let registry = reg.seq_gc_registry();
        let slot = reg.seq_gc_slot_id().expect("slot registered");
        let spine = reg.spill_refs_id().expect("spine registered");
        assert_eq!(reg.seq_gc_registry(), registry);

        let fields = reg.struct_fields(slot).expect("struct");
        assert_eq!(fields.len() as u32, SEQ_GC_SLOT_FIELD_SPINE + 1);
        assert_eq!(
            fields[SEQ_GC_SLOT_FIELD_OBJECT as usize].element_type,
            StorageType::Val(anyref_null())
        );
        assert!(fields[SEQ_GC_SLOT_FIELD_OBJECT as usize].mutable);
        assert_eq!(
            fields[SEQ_GC_SLOT_FIELD_SPINE as usize].element_type,
            StorageType::Val(spine.ref_null())
        );
        assert!(fields[SEQ_GC_SLOT_FIELD_SPINE as usize].mutable);

        match &reg.encoded[registry.as_u32() as usize] {
            EncodedType::Array { element, mutable } => {
                assert!(*mutable);
                assert_eq!(*element, StorageType::Val(slot.ref_null()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn the_sqs_process_spine_is_its_own_mutable_eqref_array() {
        let mut reg = GcTypeRegistry::new();
        let procs = reg.sim_notice_procs();
        assert_eq!(reg.sim_notice_procs(), procs);
        assert_ne!(procs, reg.spill_refs_array());

        match &reg.encoded[procs.as_u32() as usize] {
            EncodedType::Array { element, mutable } => {
                assert!(*mutable);
                assert_eq!(*element, StorageType::Val(anyref_null()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn append_to_rebases_concrete_heap_types() {
        let mut reg = GcTypeRegistry::new();
        let chars = reg.text_chars();
        let frame = reg.text_frame();
        let mut types = TypeSection::new();
        // Pretend function types already occupy 0..10.
        for _ in 0..10 {
            types.ty().function([], []);
        }
        let base = reg.append_to(&mut types, 10);
        assert_eq!(base, 10);
        assert_eq!(types.len(), 10 + reg.len());
        // Standalone emit keeps local indices; appended module uses base+id.
        assert_eq!(chars.as_u32(), 0);
        assert_eq!(frame.as_u32(), 1);
        let _ = (chars, frame);
    }

    fn ring_layout(name: &str, prefix: Option<&str>, extra: &[(&str, i64)]) -> ClassLayout {
        let mut fields = vec![
            FieldLayout {
                name: "SUC".into(),
                offset: SIMSET_SUC_OFFSET,
                size: 8,
                ty: LayoutFieldType::ObjectRef,
                class_qual: None,
            },
            FieldLayout {
                name: "PRED".into(),
                offset: SIMSET_PRED_OFFSET,
                size: 8,
                ty: LayoutFieldType::ObjectRef,
                class_qual: None,
            },
        ];
        for (attr, offset) in extra {
            fields.push(FieldLayout {
                name: (*attr).into(),
                offset: *offset,
                size: 8,
                ty: LayoutFieldType::I64,
                class_qual: None,
            });
        }
        let size = 8 + 8 * fields.len() as i64;
        ClassLayout {
            name: name.into(),
            declared_name: name.into(),
            decl_span: Span::default(),
            fields,
            methods: vec![],
            virtual_methods: vec![],
            constructor_params: vec![],
            needs_init: false,
            runs_on_own_stack: false,
            enclosing_captures: vec![],
            size,
            class_id: 2,
            system_block: 0,
            prefix: prefix.map(Into::into),
        }
    }

    #[test]
    fn simset_class_gets_eqref_suc_and_pred() {
        let mut reg = GcTypeRegistry::new();
        let id = reg.register_class(
            &ring_layout("Link", Some("Linkage"), &[]),
            &Default::default(),
        );
        let fields = reg.struct_fields(id).expect("struct");
        assert_eq!(fields[0].element_type, StorageType::Val(ValType::I64));
        assert_eq!(fields[1].element_type, StorageType::Val(anyref_val()));
        assert_eq!(fields[2].element_type, StorageType::Val(anyref_val()));
        assert!(reg.has_linkage_ref_fields(id));
        assert_eq!(reg.supertype_of(id), reg.linkage_base_id());
    }

    #[test]
    fn linkage_text_attr_is_eqref_so_siblings_can_share_offsets() {
        let mut reg = GcTypeRegistry::new();
        let mut layout = ring_layout("town", Some("Link"), &[("nam_", 24)]);
        layout.fields.last_mut().unwrap().ty = LayoutFieldType::Text;
        let id = reg.register_class(&layout, &Default::default());
        let fields = reg.struct_fields(id).expect("struct");
        assert_eq!(fields[3].element_type, StorageType::Val(anyref_val()));
    }

    /// `town.nam_` and `townpoint.t` share offset 24. Both must be subtypes
    /// of one ancestor that already declares field 3 as eqref, so
    /// `cast_target` can name that ancestor instead of a leaf.
    #[test]
    fn linkage_siblings_share_eqref_ancestor_at_trailing_slot() {
        use std::collections::HashSet;

        let mut reg = GcTypeRegistry::new();
        let prefixes: HashSet<String> = ["link"].into_iter().map(str::to_string).collect();
        let link = reg.register_class(&ring_layout("Link", Some("Linkage"), &[]), &prefixes);

        let mut town = ring_layout("town", Some("Link"), &[("nam_", 24)]);
        town.fields.last_mut().unwrap().ty = LayoutFieldType::Text;
        town.class_id = 10;
        let mut townpoint = ring_layout("townpoint", Some("Link"), &[("t", 24)]);
        townpoint.fields.last_mut().unwrap().ty = LayoutFieldType::ObjectRef;
        townpoint.class_id = 11;

        let town_id = reg.register_class(&town, &prefixes);
        let townpoint_id = reg.register_class(&townpoint, &prefixes);

        let ancestors = |start: GcTypeId| {
            let mut chain = vec![start];
            let mut cur = start;
            for _ in 0..32 {
                match reg.supertype_of(cur) {
                    Some(parent) => {
                        chain.push(parent);
                        cur = parent;
                    }
                    None => break,
                }
            }
            chain
        };
        let town_chain = ancestors(town_id);
        let townpoint_chain = ancestors(townpoint_id);
        let shared = town_chain.iter().find(|id| {
            townpoint_chain.contains(id)
                && reg.field_count(**id).is_some_and(|count| count > 3)
                && reg
                    .struct_fields(**id)
                    .and_then(|fields| fields.get(3))
                    .is_some_and(|field| field.element_type == StorageType::Val(anyref_val()))
        });
        assert!(
            shared.is_some(),
            "town and townpoint must share an ancestor with eqref field 3; \
             town={town_chain:?} townpoint={townpoint_chain:?} link={link:?}"
        );
        assert!(town_chain.contains(&link));
        assert!(townpoint_chain.contains(&link));
    }

    #[test]
    fn simset_subclass_keeps_trailing_slots_as_i64() {
        let mut reg = GcTypeRegistry::new();
        let bead = reg.register_class(
            &ring_layout("Bead", Some("Link"), &[("n", 24), ("m", 32)]),
            &Default::default(),
        );
        let fields = reg.struct_fields(bead).expect("struct");
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[1].element_type, StorageType::Val(anyref_val()));
        assert_eq!(fields[3].element_type, StorageType::Val(ValType::I64));
        assert_eq!(fields[4].element_type, StorageType::Val(ValType::I64));
    }

    /// An ordinary class whose first two enclosing captures are `ref`
    /// variables has `ObjectRef` at the SUC/PRED offsets without being a ring
    /// member. Those slots are typed `eqref`s (4-R4) but must **not** be
    /// treated as `linkage_base` ring pointers (simtst47).
    #[test]
    fn object_ref_captures_at_ring_offsets_are_not_linkage() {
        let mut layout = ring_layout("A", None, &[]);
        layout.fields[0].name = "__enclosing_ra2".into();
        layout.fields[1].name = "__enclosing_ra3".into();
        assert!(!layout_is_linkage_family(&layout));

        let mut reg = GcTypeRegistry::new();
        let id = reg.register_class(&layout, &Default::default());
        let fields = reg.struct_fields(id).expect("struct");
        assert_eq!(fields[0].element_type, StorageType::Val(ValType::I64));
        assert_eq!(fields[1].element_type, StorageType::Val(anyref_val()));
        assert_eq!(fields[2].element_type, StorageType::Val(anyref_val()));
        assert!(!reg.has_linkage_ref_fields(id));
    }

    #[test]
    fn with_force_enabled_overrides_env() {
        let outer = env_enabled();
        with_force_enabled(true, || assert!(env_enabled()));
        with_force_enabled(false, || assert!(!env_enabled()));
        assert_eq!(env_enabled(), outer);
    }
}
