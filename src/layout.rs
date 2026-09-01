//! Object / class layout for native codegen.
//!
//! Each object begins with an 8-byte `class_id` header; integer/boolean/text
//! attributes follow at successive 8-byte offsets. Constructor parameters
//! (value integer/boolean) become leading fields. Prefix classes (via
//! concatenation), simple methods, virtuals, class-body init, and split-body
//! `inner` (concatenated initials + tails) are supported. Real fields occupy
//! the same 8-byte slots as integers (IEEE-754 `f64` bit patterns).

use std::collections::{HashMap, HashSet};

use crate::ast::{
    AssignOperator, Assignment, AssignmentRhs, BinaryOp, Block, ClassDeclaration, Expr, ExprKind,
    ForListElement, FormalParameter, ParamMode, ProcedureCall, ProcedureDeclaration, RelationOp,
    Specifier, Statement, StatementKind, Variable,
};
use crate::concatenate::{self, is_fictitious_detach};
use crate::error::CompileError;
use crate::simulation::{
    self, block_is_simulation_prefixed, is_head_class, is_link_class, is_process_class,
    is_simulation_class,
};
use crate::types::Type;

/// Byte size of the object header (`class_id: i64`) and of each scalar field.
pub const OBJECT_HEADER_SIZE: i64 = 8;
pub const I64_FIELD_SIZE: i64 = 8;

/// Synthetic SIMSET ring links on `Linkage` (and every subclass after concat).
pub const SIMSET_SUC_FIELD: &str = "SUC";
pub const SIMSET_PRED_FIELD: &str = "PRED";
/// Fixed byte offsets when SIMSET slots are injected immediately after the
/// object header (no constructor parameters on Linkage itself).
pub const SIMSET_SUC_OFFSET: i64 = OBJECT_HEADER_SIZE;
pub const SIMSET_PRED_OFFSET: i64 = OBJECT_HEADER_SIZE + I64_FIELD_SIZE;

/// Textually enclosing object for nested local classes (`This A` from `Class C`
/// declared inside `A` — simtst76 `This A.Detach`).
pub const ENCLOSING_OBJECT_FIELD_NAME: &str = "__simrt_enclosing";

/// Heap box for an address-taken / by-reference `ref`.
/// One `ObjectRef` field at [`REF_CELL_VALUE_OFFSET`]; never a
/// linear-memory cell and never a root-handle index.
pub const REF_CELL_CLASS_NAME: &str = "__simrt_ref_cell";
pub const REF_CELL_CLASS_ID: i64 = 1_000_001;
pub const REF_CELL_VALUE_FIELD: &str = "value";
pub const REF_CELL_VALUE_OFFSET: i64 = OBJECT_HEADER_SIZE;
pub const REF_CELL_SIZE: i64 = OBJECT_HEADER_SIZE + I64_FIELD_SIZE;

/// Call-by-name `a(i)` env: the array descriptor (any array kind, stored as
/// an `ObjectRef`/`eqref`) plus the address of the index cell.
pub const NAME_ARR1_ENV_CLASS_NAME: &str = "__simrt_name_arr1_env";
pub const NAME_ARR1_ENV_CLASS_ID: i64 = 1_000_002;
pub const NAME_ARR1_ENV_ARRAY_FIELD: &str = "array";
pub const NAME_ARR1_ENV_INDEX_FIELD: &str = "index_ptr";
pub const NAME_ARR1_ENV_ARRAY_OFFSET: i64 = OBJECT_HEADER_SIZE;
pub const NAME_ARR1_ENV_INDEX_OFFSET: i64 = OBJECT_HEADER_SIZE + I64_FIELD_SIZE;
pub const NAME_ARR1_ENV_SIZE: i64 = OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE;

/// Call-by-name integer-cell env: a GC object holding the linear address of
/// the `i64` home, so outlined name-thunk `env` parameters can be `ObjectRef`
/// for both `dec(i)` and `dec(r.x)`.
pub const NAME_INT_ENV_CLASS_NAME: &str = "__simrt_name_int_env";
pub const NAME_INT_ENV_CLASS_ID: i64 = 1_000_003;
pub const NAME_INT_ENV_ADDR_FIELD: &str = "addr";
pub const NAME_INT_ENV_ADDR_OFFSET: i64 = OBJECT_HEADER_SIZE;
pub const NAME_INT_ENV_SIZE: i64 = OBJECT_HEADER_SIZE + I64_FIELD_SIZE;

/// Expression-thunk env: a fixed row of eqref slots (integer cells boxed as
/// [`NAME_INT_ENV_CLASS_NAME`], objects stored directly, nested name formals
/// as [`NAME_THUNK_PAIR_CLASS_NAME`]). The outlined name-formal `env`
/// parameter is this object — never a linear i64 pack.
pub const NAME_PACK_ENV_CLASS_NAME: &str = "__simrt_name_pack_env";
pub const NAME_PACK_ENV_CLASS_ID: i64 = 1_000_004;
pub const NAME_PACK_ENV_SLOT_COUNT: usize = 8;
pub const NAME_PACK_ENV_SIZE: i64 =
    OBJECT_HEADER_SIZE + (NAME_PACK_ENV_SLOT_COUNT as i64) * I64_FIELD_SIZE;

pub fn name_pack_env_slot_offset(index: usize) -> i64 {
    OBJECT_HEADER_SIZE + (index as i64) * I64_FIELD_SIZE
}

/// SIMULATION's MAIN marker process. No class declares it and nothing reads an
/// attribute off it — `main`/`current`/`running` and the SQS only ever compare
/// it for identity — so it is header-only. It still needs to be a real object
/// rather than a [`crate::mir::build::FunctionBuilder::alloc`] record, because
/// under WasmGC the process columns it flows into are `eqref`.
pub const SIM_MAIN_CLASS_NAME: &str = "__simrt_sim_main";
pub const SIM_MAIN_CLASS_ID: i64 = 1_000_006;
pub const SIM_MAIN_SIZE: i64 = OBJECT_HEADER_SIZE;

/// Nested name-formal capture inside an expression thunk: `(get, env)`.
pub const NAME_THUNK_PAIR_CLASS_NAME: &str = "__simrt_name_thunk_pair";
pub const NAME_THUNK_PAIR_CLASS_ID: i64 = 1_000_005;
pub const NAME_THUNK_PAIR_GET_FIELD: &str = "get";
pub const NAME_THUNK_PAIR_ENV_FIELD: &str = "env";
pub const NAME_THUNK_PAIR_GET_OFFSET: i64 = OBJECT_HEADER_SIZE;
pub const NAME_THUNK_PAIR_ENV_OFFSET: i64 = OBJECT_HEADER_SIZE + I64_FIELD_SIZE;
pub const NAME_THUNK_PAIR_SIZE: i64 = OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE;
/// Storage kind of an object attribute in the native layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    I64,
    Bool,
    /// IEEE-754 `f64` stored in an 8-byte slot (bit pattern via i64 load/store).
    F64,
    /// Opaque `SimrtTextFrame*` stored in a pointer-sized slot.
    Text,
    /// Object reference (`ref(C)`) stored as a pointer-sized slot.
    ObjectRef,
    /// Enclosing array descriptor (pointer-sized); element kind distinguishes
    /// integer/character vs boolean vs real vs text arrays.
    ArrayI64,
    /// Boolean array descriptor (same pointer ABI as [`Self::ArrayI64`]).
    ArrayBool,
    ArrayF64,
    ArrayText,
}

/// Free names from the declaring block to snapshot onto each class instance,
/// plus `ref(C)` qualifications when known from block declarations.
#[derive(Debug, Clone, Default)]
struct SiblingCaptureInfo {
    captures: HashMap<String, FieldType>,
    /// Declared types in the enclosing block — refine capture heuristics only
    /// (never force unused locals onto nested classes).
    declared_types: HashMap<String, FieldType>,
    /// Lower-cased variable name → declared object-reference class.
    ref_quals: HashMap<String, String>,
    /// Lower-cased formal-procedure parameter names from enclosing procedures
    /// (local classes may snapshot these as `__simrt_fp_*` receivers).
    formal_proc_params: HashSet<String>,
    /// When this class is nested inside another class, the outer class's
    /// source name (`This Outer` needs a snapshotted outer object).
    enclosing_class: Option<String>,
}

/// Sanitize span-qualified class names (`A@123`) for linker/object symbols.
///
/// Mach-O (and C) treat `@` specially in symbol names — a reloc to
/// `simrt_proc_C$__init` can be mis-resolved against `simrt_proc_C@1670$__init`
/// (simtst76: Resume in the first `A` jumped into the second block's `C`).
fn sanitize_symbol_class_name(class_name: &str) -> String {
    class_name.replace('@', "_s")
}

/// Mangles a class method into a MIR / native symbol stem so it cannot collide
/// with a same-named local procedure (`ClassName$method`).
pub fn mangle_method_name(class_name: &str, method_name: &str) -> String {
    format!("{}${method_name}", sanitize_symbol_class_name(class_name))
}

/// Mangles the synthetic class-body initializer (`ClassName$__init`).
pub fn mangle_init_name(class_name: &str) -> String {
    format!("{}$__init", sanitize_symbol_class_name(class_name))
}

/// Mangles the class body as a component entry point (`ClassName$__coro`),
/// used when the object runs on its own stack. It takes only the object: the
/// generator has already written the constructor parameters into fields, since
/// the body may suspend before it could read them from registers.
pub fn mangle_coro_entry_name(class_name: &str) -> String {
    format!("{}$__coro", sanitize_symbol_class_name(class_name))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    pub offset: i64,
    pub size: i64,
    pub ty: FieldType,
    /// Declared class for [`FieldType::ObjectRef`] (`ref(Point)` → `"Point"`).
    pub class_qual: Option<String>,
}

/// Layout of one class's instances after concatenation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassLayout {
    pub name: String,
    /// Unqualified source name when [`Self::name`] is span-qualified (`A@123`).
    pub declared_name: String,
    /// Source span of the class declaration (for resolving `new A` among
    /// same-named classes in disjoint scopes).
    pub decl_span: crate::error::Span,
    pub fields: Vec<FieldLayout>,
    /// Immediate prefix class's declared name (`point` in `point class polar`),
    /// if any — carried through concatenation (`concatenate_one` sets
    /// `merged.prefix = class.prefix.clone()` on the *flattened* declaration)
    /// so codegen backends can recover the real Simula prefix-class chain
    /// without re-deriving it from field-shape heuristics (see
    /// `wasm_gc.rs::register_class`, which declares WasmGC struct subtyping
    /// along this chain).
    pub prefix: Option<String>,
    /// Simple names of non-fictitious class procedures (Phase 5 method MVP).
    pub methods: Vec<String>,
    /// Virtual procedure names from the concatenated `virtual:` part
    /// (excluding fictitious `detach`).
    pub virtual_methods: Vec<String>,
    /// Constructor parameters in declaration order (also present in `fields`).
    pub constructor_params: Vec<(String, FieldType)>,
    /// Whether `new` must call the synthetic `ClassName$__init` body.
    pub needs_init: bool,
    /// Whether the class body can suspend (§7), so its objects each get a call
    /// stack of their own and their body is entered as a component rather than
    /// run to completion inside `new`.
    pub runs_on_own_stack: bool,
    /// Free enclosing locals snapshotted onto the instance at `new`
    /// (interpreter `enclosing_locals`). Stored as ordinary fields with the
    /// inferred scalar / text / object-ref type from body use sites.
    pub enclosing_captures: Vec<(String, FieldType)>,
    /// Total allocation size in bytes, including the `class_id` header.
    pub size: i64,
    pub class_id: i64,
    /// Identifier of the system head declaring this class (7.2), or zero when
    /// it is declared in a class body or a procedure body and its objects can
    /// therefore only be independent components. See
    /// [`system_head_id`].
    pub system_block: i64,
}

impl ClassLayout {
    pub fn field_offset(&self, name: &str) -> Option<i64> {
        self.field(name).map(|field| field.offset)
    }

    pub fn field_type(&self, name: &str) -> Option<FieldType> {
        self.field(name).map(|field| field.ty)
    }

    fn field(&self, name: &str) -> Option<&FieldLayout> {
        self.fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
    }

    pub fn method_name(&self, name: &str) -> Option<&str> {
        self.methods
            .iter()
            .find(|method| method.eq_ignore_ascii_case(name))
            .or_else(|| {
                self.virtual_methods
                    .iter()
                    .find(|method| method.eq_ignore_ascii_case(name))
            })
            .map(|method| method.as_str())
    }

    pub fn is_virtual_method(&self, name: &str) -> bool {
        self.virtual_methods
            .iter()
            .any(|method| method.eq_ignore_ascii_case(name))
    }
}

/// Walks `program` blocks, concatenates class declarations, and builds a
/// name → layout map. Class ids are assigned in declaration-collection order
/// (stable for a given program).
pub fn layouts_for_program(
    program: &crate::ast::Program,
) -> Result<HashMap<String, ClassLayout>, CompileError> {
    let mut raw = Vec::new();
    let mut sibling_captures: HashMap<String, SiblingCaptureInfo> = HashMap::new();
    for block in &program.blocks {
        collect_classes_with_sibling_captures(block, &mut raw, &mut sibling_captures, None);
    }
    let mut procedures = HashMap::new();
    for block in &program.blocks {
        collect_procedures_for_captures(block, &mut procedures);
    }
    expand_sibling_captures_from_called_procedures(&raw, &procedures, &mut sibling_captures);
    expand_sibling_captures_from_resumed_peers(&raw, &mut sibling_captures);
    // A component parked on its own stack cannot see the declaring frame's
    // names directly, so a generated class hands its captures on to the classes
    // it in turn generates.
    expand_captures_through_generated_classes(&raw, &mut sibling_captures);
    inject_basicio_system_classes(&mut raw);
    if program_needs_simulation_system_classes(program) {
        inject_missing_system_classes(&mut raw);
    }
    let mut layouts = layouts_from_classes_with_sibling_captures(&raw, &sibling_captures)?;
    inject_runtime_helper_classes(&mut layouts);
    let system_heads = system_heads_for_program(program);
    for layout in layouts.values_mut() {
        layout.system_block = system_heads
            .get(&layout.decl_span.start)
            .copied()
            .unwrap_or(0);
    }
    Ok(layouts)
}

/// A block is a system head for the classes it declares directly, as long as it
/// is a subblock or prefixed block (7.2) rather than a class body or a
/// procedure body. The block's identity is that of its first class declaration,
/// which is unique in the source and available wherever the block is, so both
/// the declaration site and the generator can name the same system without the
/// handle being threaded between them.
pub fn system_head_id(block: &Block) -> i64 {
    block
        .classes
        .first()
        .map_or(0, |class| class.span.start as i64)
}

/// System head of each class, keyed by the start of its declaration span.
fn system_heads_for_program(program: &crate::ast::Program) -> HashMap<usize, i64> {
    let mut out = HashMap::new();
    for block in &program.blocks {
        collect_system_heads(block, true, &mut out);
    }
    out
}

fn collect_system_heads(block: &Block, is_system_head: bool, out: &mut HashMap<usize, i64>) {
    let id = if is_system_head {
        system_head_id(block)
    } else {
        0
    };
    for class in &block.classes {
        out.insert(class.span.start, id);
        collect_system_heads(&class.body, false, out);
    }
    for procedure in &block.procedures {
        collect_system_heads(&procedure.body, false, out);
    }
    for inner in &block.body {
        collect_system_heads(inner, true, out);
    }
    for statement in &block.statements {
        collect_system_heads_in_statement(statement, out);
    }
}

fn collect_system_heads_in_statement(statement: &Statement, out: &mut HashMap<usize, i64>) {
    match &statement.kind {
        StatementKind::Compound(block) => collect_system_heads(block, true, out),
        StatementKind::Labeled { statement, .. } => {
            collect_system_heads_in_statement(statement, out)
        }
        StatementKind::If(if_stmt) => {
            collect_system_heads_in_statement(&if_stmt.then_branch, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_system_heads_in_statement(else_branch, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_system_heads_in_statement(&while_stmt.body, out);
        }
        StatementKind::For(for_stmt) => collect_system_heads_in_statement(&for_stmt.body, out),
        StatementKind::Inspect(inspect) => {
            for clause in &inspect.when_clauses {
                collect_system_heads_in_statement(&clause.body, out);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_system_heads_in_statement(do_clause, out);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_system_heads_in_statement(otherwise, out);
            }
        }
        _ => {}
    }
}

fn collect_procedures_for_captures<'a>(
    block: &'a Block,
    out: &mut HashMap<String, &'a ProcedureDeclaration>,
) {
    for procedure in &block.procedures {
        out.insert(procedure.name.to_ascii_lowercase(), procedure);
        collect_procedures_for_captures(&procedure.body, out);
    }
    for class in &block.classes {
        collect_procedures_for_captures(&class.body, out);
    }
    for inner in &block.body {
        collect_procedures_for_captures(inner, out);
    }
    for statement in &block.statements {
        collect_procedures_for_captures_from_statement(statement, out);
    }
}

fn collect_procedures_for_captures_from_statement<'a>(
    statement: &'a Statement,
    out: &mut HashMap<String, &'a ProcedureDeclaration>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => collect_procedures_for_captures(block, out),
        StatementKind::If(if_stmt) => {
            collect_procedures_for_captures_from_statement(&if_stmt.then_branch, out);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_procedures_for_captures_from_statement(else_branch, out);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_procedures_for_captures_from_statement(&while_stmt.body, out)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_procedures_for_captures_from_statement(statement, out)
        }
        _ => {}
    }
}

/// Class bodies that call outer procedures (e.g. `Sjekk`) need those
/// procedures' free enclosing names snapshotted as captures.
fn expand_sibling_captures_from_called_procedures(
    classes: &[ClassDeclaration],
    procedures: &HashMap<String, &ProcedureDeclaration>,
    sibling_captures: &mut HashMap<String, SiblingCaptureInfo>,
) {
    for class in classes {
        let mut called = HashSet::new();
        collect_called_procedure_names_from_block(&class.body, &mut called);
        for statement in &class.tail_statements {
            collect_called_procedure_names_from_statement(statement, &mut called);
        }
        if called.is_empty() {
            continue;
        }
        // The class's own methods are not enclosing-block siblings: an
        // unqualified call to one runs on `__this` and reaches the attributes
        // directly. Treating them as siblings would snapshot the class's own
        // attributes into `__simrt_encl_*` slots, and the writeback after the
        // call would then undo whatever the method wrote.
        let own_methods: HashSet<String> = class
            .body
            .procedures
            .iter()
            .map(|procedure| procedure.name.to_ascii_lowercase())
            .collect();
        let entry = sibling_captures.entry(class.name.clone()).or_default();
        for name in called {
            if own_methods.contains(&name) {
                continue;
            }
            let Some(procedure) = procedures.get(&name) else {
                continue;
            };
            let mut hints = HashMap::new();
            collect_capture_hints_from_procedure(procedure, &mut hints);
            for (free_name, ty) in hints {
                merge_capture_hint(&mut entry.captures, &free_name, ty);
            }
        }
    }
}

/// When class A generates an object of class B, B's enclosing names have to
/// reach it through A: the generator runs in A's frame, so a name A never
/// snapshotted cannot be handed on. Repeated to a fixed point so a chain of
/// generators carries a name from the block that declares it all the way down.
fn expand_captures_through_generated_classes(
    classes: &[ClassDeclaration],
    sibling_captures: &mut HashMap<String, SiblingCaptureInfo>,
) {
    let generated: Vec<(String, HashSet<String>)> = classes
        .iter()
        .map(|class| {
            let mut names = HashSet::new();
            collect_generated_class_names_from_block(&class.body, &mut names);
            (class.name.clone(), names)
        })
        .collect();

    let by_declared: HashMap<String, String> = classes
        .iter()
        .map(|class| {
            (
                declared_class_name(&class.name).to_ascii_lowercase(),
                class.name.clone(),
            )
        })
        .collect();

    // The names a class needs from an enclosing block: what its body reads
    // free, growing as generated classes contribute theirs.
    let mut needed: HashMap<String, HashMap<String, FieldType>> = classes
        .iter()
        .map(|class| {
            let mut hints = HashMap::new();
            collect_capture_hints_from_block(&class.body, &mut hints);
            for statement in &class.tail_statements {
                collect_capture_hints_from_statement(statement, &mut hints);
            }
            let mut own = HashSet::new();
            add_block_bound_names_for_captures(&class.body, &mut own);
            own.insert(declared_class_name(&class.name).to_ascii_lowercase());
            hints.retain(|name, _| !own.contains(&name.to_ascii_lowercase()));
            (class.name.clone(), hints)
        })
        .collect();

    for _ in 0..classes.len().max(1) {
        let mut changed = false;
        for (generator, targets) in &generated {
            let targets: Vec<String> = targets
                .iter()
                .flat_map(|target| class_family_declared_names(classes, target))
                .collect();
            for target in &targets {
                let Some(target_key) = by_declared.get(&target.to_ascii_lowercase()) else {
                    continue;
                };
                if target_key == generator {
                    continue;
                }
                let Some(target_needs) = needed.get(target_key).cloned() else {
                    continue;
                };
                let target_quals = sibling_captures
                    .get(target_key)
                    .map(|info| info.ref_quals.clone())
                    .unwrap_or_default();
                let mut own = HashSet::new();
                if let Some(class) = classes.iter().find(|class| &class.name == generator) {
                    add_block_bound_names_for_captures(&class.body, &mut own);
                }
                let entry = sibling_captures.entry(generator.clone()).or_default();
                let generator_needs = needed.entry(generator.clone()).or_default();
                for (name, ty) in &target_needs {
                    if own.contains(&name.to_ascii_lowercase()) {
                        continue;
                    }
                    let before = generator_needs.len();
                    merge_capture_hint(generator_needs, name, *ty);
                    if generator_needs.len() != before {
                        changed = true;
                    }
                    merge_capture_hint(&mut entry.captures, name, *ty);
                    if let Some(qual) = target_quals.get(&name.to_ascii_lowercase()) {
                        entry
                            .ref_quals
                            .entry(name.to_ascii_lowercase())
                            .or_insert_with(|| qual.clone());
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Classes a body may generate objects of, taken from the qualifications it
/// declares. An over-approximation of the `new` sites, which only costs a
/// capture field on a class that turns out never to generate one.
fn collect_generated_class_names_from_block(block: &Block, out: &mut HashSet<String>) {
    for decl in &block.declarations {
        if let Type::ObjectRef(qual) = &decl.ty {
            out.insert(qual.clone());
        }
    }
    for array in &block.arrays {
        if let Type::ObjectRef(qual) = &array.element_type {
            out.insert(qual.clone());
        }
    }
    for inner in &block.body {
        collect_generated_class_names_from_block(inner, out);
    }
    for procedure in &block.procedures {
        collect_generated_class_names_from_block(&procedure.body, out);
    }
}

/// A reference qualified by a prefix can hold an object of any subclass, so a
/// conclusion drawn from the qualification alone has to hold for the whole
/// family: `ref(Coroutine) c` in simtst88 names a `Changer`, and only the
/// subclasses have enclosing names to carry.
fn class_family_declared_names(classes: &[ClassDeclaration], qual: &str) -> Vec<String> {
    let qual = declared_class_name(qual).to_ascii_lowercase();
    let prefixes: HashMap<String, Option<String>> = classes
        .iter()
        .map(|class| {
            (
                declared_class_name(&class.name).to_ascii_lowercase(),
                class
                    .prefix
                    .as_deref()
                    .map(|prefix| declared_class_name(prefix).to_ascii_lowercase()),
            )
        })
        .collect();
    let mut family = Vec::new();
    for name in prefixes.keys() {
        let mut current = Some(name.clone());
        for _ in 0..=classes.len() {
            let Some(class) = current else { break };
            if class == qual {
                family.push(name.clone());
                break;
            }
            current = prefixes.get(&class).cloned().flatten();
        }
    }
    family
}

/// When class A `Call`/`Resume`s a peer class B, A must snapshot B's free
/// enclosing names too — re-enter syncs captures between the two objects
/// (`copy_enclosing_captures_between`), and B may need a local that A alone
/// can refresh from MAIN (simtst62: X resumes Y which uses `xx` assigned
/// after `new Y`).
fn expand_sibling_captures_from_resumed_peers(
    classes: &[ClassDeclaration],
    sibling_captures: &mut HashMap<String, SiblingCaptureInfo>,
) {
    let free_by_decl: HashMap<String, HashMap<String, FieldType>> = classes
        .iter()
        .map(|class| {
            let mut hints = HashMap::new();
            collect_capture_hints_from_block(&class.body, &mut hints);
            for statement in &class.tail_statements {
                collect_capture_hints_from_statement(statement, &mut hints);
            }
            let mut own = HashSet::new();
            add_block_bound_names_for_captures(&class.body, &mut own);
            own.insert(declared_class_name(&class.name).to_ascii_lowercase());
            hints.retain(|name, _| !own.contains(&name.to_ascii_lowercase()));
            (declared_class_name(&class.name).to_ascii_lowercase(), hints)
        })
        .collect();

    for class in classes {
        let mut resumed_vars = HashSet::new();
        collect_call_resume_object_names_from_block(&class.body, &mut resumed_vars);
        for statement in &class.tail_statements {
            collect_call_resume_object_names_from_statement(statement, &mut resumed_vars);
        }
        if resumed_vars.is_empty() {
            continue;
        }
        let caller_key = class.name.clone();
        let peer_free: Vec<(String, FieldType)> = {
            let entry = sibling_captures.entry(caller_key.clone()).or_default();
            let mut out = Vec::new();
            for var in &resumed_vars {
                let Some(qual) = entry
                    .ref_quals
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(var))
                    .map(|(_, q)| declared_class_name(q).to_ascii_lowercase())
                else {
                    continue;
                };
                for peer in class_family_declared_names(classes, &qual) {
                    if peer == declared_class_name(&class.name).to_ascii_lowercase() {
                        continue;
                    }
                    let Some(hints) = free_by_decl.get(&peer) else {
                        continue;
                    };
                    for (name, ty) in hints {
                        out.push((name.clone(), *ty));
                    }
                }
            }
            out
        };
        if peer_free.is_empty() {
            continue;
        }
        let entry = sibling_captures.entry(caller_key).or_default();
        for (name, ty) in peer_free {
            merge_capture_hint(&mut entry.captures, &name, ty);
        }
    }
}

fn collect_call_resume_object_names_from_block(block: &Block, names: &mut HashSet<String>) {
    for statement in &block.statements {
        collect_call_resume_object_names_from_statement(statement, names);
    }
    for procedure in &block.procedures {
        collect_call_resume_object_names_from_block(&procedure.body, names);
    }
    for inner in &block.body {
        collect_call_resume_object_names_from_block(inner, names);
    }
}

fn collect_call_resume_object_names_from_statement(
    statement: &Statement,
    names: &mut HashSet<String>,
) {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => {
            let lower = call.name.to_ascii_lowercase();
            if matches!(lower.as_str(), "call" | "resume") {
                if let Some(arg) = call.arguments.first() {
                    if let ExprKind::Variable(Variable::Simple(name)) = &arg.kind {
                        names.insert(name.to_ascii_lowercase());
                    }
                }
            }
        }
        StatementKind::Compound(block) => collect_call_resume_object_names_from_block(block, names),
        StatementKind::If(if_stmt) => {
            collect_call_resume_object_names_from_statement(&if_stmt.then_branch, names);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_call_resume_object_names_from_statement(else_branch, names);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_call_resume_object_names_from_statement(&while_stmt.body, names)
        }
        StatementKind::For(for_stmt) => {
            collect_call_resume_object_names_from_statement(&for_stmt.body, names)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_call_resume_object_names_from_statement(statement, names)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_call_resume_object_names_from_statement(&when.body, names);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_call_resume_object_names_from_statement(do_clause, names);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_call_resume_object_names_from_statement(otherwise, names);
            }
        }
        _ => {}
    }
}

fn collect_called_procedure_names_from_block(block: &Block, names: &mut HashSet<String>) {
    for statement in &block.statements {
        collect_called_procedure_names_from_statement(statement, names);
    }
    // Array bounds run when the object is created, so a procedure called there
    // (`Real array X(P:1)`, simtst74) belongs to the class body's call graph.
    for array in &block.arrays {
        for segment in &array.segments {
            for bound in &segment.bounds {
                collect_called_procedure_names_from_bound(&bound.lower, names);
                collect_called_procedure_names_from_bound(&bound.upper, names);
            }
        }
    }
    for procedure in &block.procedures {
        collect_called_procedure_names_from_block(&procedure.body, names);
    }
    for inner in &block.body {
        collect_called_procedure_names_from_block(inner, names);
    }
}

/// A bound may call a parameterless procedure by bare identifier, which parses
/// as a variable read (`Real array X(P:1)`). Record such identifiers as callee
/// candidates; callers keep only the ones that name a declared procedure.
fn collect_called_procedure_names_from_bound(expr: &Expr, names: &mut HashSet<String>) {
    collect_called_procedure_names_from_expr(expr, names);
    collect_bare_identifier_callees(expr, names);
}

fn collect_bare_identifier_callees(expr: &Expr, names: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(name)) => {
            names.insert(name.to_ascii_lowercase());
        }
        ExprKind::Variable(Variable::Subscripted { subscripts, .. }) => {
            for subscript in subscripts {
                collect_bare_identifier_callees(subscript, names);
            }
        }
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            collect_bare_identifier_callees(inner, names)
        }
        ExprKind::Unary { operand, .. } => collect_bare_identifier_callees(operand, names),
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            collect_bare_identifier_callees(left, names);
            collect_bare_identifier_callees(right, names);
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_bare_identifier_callees(condition, names);
            collect_bare_identifier_callees(then_expr, names);
            collect_bare_identifier_callees(else_expr, names);
        }
        ExprKind::FunctionCall { arguments, .. } => {
            for argument in arguments {
                collect_bare_identifier_callees(argument, names);
            }
        }
        _ => {}
    }
}

fn collect_called_procedure_names_from_statement(
    statement: &Statement,
    names: &mut HashSet<String>,
) {
    match &statement.kind {
        StatementKind::ProcedureCall(call) => {
            names.insert(call.name.to_ascii_lowercase());
            for argument in &call.arguments {
                collect_called_procedure_names_from_expr(argument, names);
            }
        }
        StatementKind::Compound(block) => collect_called_procedure_names_from_block(block, names),
        StatementKind::If(if_stmt) => {
            collect_called_procedure_names_from_statement(&if_stmt.then_branch, names);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_called_procedure_names_from_statement(else_branch, names);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_called_procedure_names_from_statement(&while_stmt.body, names)
        }
        StatementKind::For(for_stmt) => {
            collect_called_procedure_names_from_statement(&for_stmt.body, names)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_called_procedure_names_from_statement(statement, names)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_called_procedure_names_from_statement(&when.body, names);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_called_procedure_names_from_statement(do_clause, names);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_called_procedure_names_from_statement(otherwise, names);
            }
        }
        StatementKind::Assignment(assignment) => {
            if let AssignmentRhs::Expr(expr) = &assignment.rhs {
                collect_called_procedure_names_from_expr(expr, names);
            }
        }
        StatementKind::Expr(expr) => collect_called_procedure_names_from_expr(expr, names),
        _ => {}
    }
}

fn collect_called_procedure_names_from_expr(expr: &Expr, names: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::FunctionCall { name, arguments } => {
            names.insert(name.to_ascii_lowercase());
            for argument in arguments {
                collect_called_procedure_names_from_expr(argument, names);
            }
        }
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            collect_called_procedure_names_from_expr(inner, names)
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Relation { left, right, .. } => {
            collect_called_procedure_names_from_expr(left, names);
            collect_called_procedure_names_from_expr(right, names);
        }
        ExprKind::Unary { operand, .. } => collect_called_procedure_names_from_expr(operand, names),
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_called_procedure_names_from_expr(condition, names);
            collect_called_procedure_names_from_expr(then_expr, names);
            collect_called_procedure_names_from_expr(else_expr, names);
        }
        ExprKind::RemoteCall {
            object, arguments, ..
        } => {
            collect_called_procedure_names_from_expr(object, names);
            for argument in arguments {
                collect_called_procedure_names_from_expr(argument, names);
            }
        }
        ExprKind::RemoteAccess { object, .. } => {
            collect_called_procedure_names_from_expr(object, names)
        }
        ExprKind::New { arguments, .. } => {
            if let Some(arguments) = arguments {
                for argument in arguments {
                    collect_called_procedure_names_from_expr(argument, names);
                }
            }
        }
        _ => {}
    }
}

/// Whether `program` uses Simulation / `process class` prefixes that need the
/// injected system classes (`Process`, `Link`, …) for concatenation.
pub fn program_needs_simulation_system_classes(program: &crate::ast::Program) -> bool {
    program
        .blocks
        .iter()
        .any(block_needs_simulation_system_classes)
}

/// Whether `program` uses Simulation scheduling (`Simulation` blocks /
/// `process` classes) — not merely SIMSET / Link / Head.
///
/// Used to enable SQS ops (`sim.cancel` on detach, `hold` / `passivate`, …).
/// SIMSET-only programs must stay false so plain `detach`/`resume` do not
/// require an active Simulation runtime.
pub fn program_uses_simulation_scheduling(program: &crate::ast::Program) -> bool {
    program.blocks.iter().any(block_uses_simulation_scheduling)
}

fn block_uses_simulation_scheduling(block: &Block) -> bool {
    if block_is_simulation_prefixed(&block.prefix) {
        return true;
    }
    if block.classes.iter().any(|class| {
        is_process_class(&class.name)
            || is_simulation_class(&class.name)
            || class
                .prefix
                .as_deref()
                .is_some_and(|p| is_process_class(p) || is_simulation_class(p))
    }) {
        return true;
    }
    if block.declarations.iter().any(|decl| {
        matches!(
            &decl.ty,
            Type::ObjectRef(q) if is_process_class(q) || is_simulation_class(q)
        )
    }) {
        return true;
    }
    block.body.iter().any(block_uses_simulation_scheduling)
        || block
            .statements
            .iter()
            .any(statement_uses_simulation_scheduling)
}

fn statement_uses_simulation_scheduling(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Compound(block) => block_uses_simulation_scheduling(block),
        StatementKind::If(if_stmt) => {
            statement_uses_simulation_scheduling(&if_stmt.then_branch)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|s| statement_uses_simulation_scheduling(s))
        }
        StatementKind::While(while_stmt) => statement_uses_simulation_scheduling(&while_stmt.body),
        StatementKind::For(for_stmt) => statement_uses_simulation_scheduling(&for_stmt.body),
        StatementKind::Labeled { statement, .. } => statement_uses_simulation_scheduling(statement),
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|clause| statement_uses_simulation_scheduling(&clause.body))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|s| statement_uses_simulation_scheduling(s))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|s| statement_uses_simulation_scheduling(s))
        }
        _ => false,
    }
}

fn block_needs_simulation_system_classes(block: &Block) -> bool {
    if block_is_simulation_prefixed(&block.prefix) {
        return true;
    }
    if block.classes.iter().any(|class| {
        class.prefix.as_deref().is_some_and(|p| {
            is_process_class(p) || is_simulation_class(p) || is_link_class(p) || is_head_class(p)
        }) || is_process_class(&class.name)
            || is_simulation_class(&class.name)
            || is_link_class(&class.name)
            || is_head_class(&class.name)
    }) {
        return true;
    }
    if block.declarations.iter().any(|decl| {
        matches!(
            &decl.ty,
            Type::ObjectRef(q)
                if is_head_class(q)
                    || is_link_class(q)
                    || is_process_class(q)
                    || q.eq_ignore_ascii_case("Linkage")
        )
    }) {
        return true;
    }
    block.body.iter().any(block_needs_simulation_system_classes)
        || block
            .statements
            .iter()
            .any(statement_needs_simulation_system_classes)
}

fn statement_needs_simulation_system_classes(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Compound(block) => block_needs_simulation_system_classes(block),
        StatementKind::If(if_stmt) => {
            statement_needs_simulation_system_classes(&if_stmt.then_branch)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|s| statement_needs_simulation_system_classes(s))
        }
        StatementKind::While(while_stmt) => {
            statement_needs_simulation_system_classes(&while_stmt.body)
        }
        StatementKind::For(for_stmt) => statement_needs_simulation_system_classes(&for_stmt.body),
        StatementKind::Labeled { statement, .. } => {
            statement_needs_simulation_system_classes(statement)
        }
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|clause| statement_needs_simulation_system_classes(&clause.body))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|s| statement_needs_simulation_system_classes(s))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|s| statement_needs_simulation_system_classes(s))
        }
        _ => false,
    }
}

fn inject_basicio_system_classes(raw: &mut Vec<ClassDeclaration>) {
    let mut map: HashMap<String, ClassDeclaration> = HashMap::new();
    for class in raw.iter() {
        map.insert(class.name.clone(), class.clone());
    }
    // Stubs only — layouts_from_raw concatenates the prefix chain once.
    crate::basicio::inject_system_class_stubs(&mut map);
    for class in map.into_values() {
        let exists = raw
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&class.name));
        if !exists {
            raw.push(class);
        }
    }
}

/// Synthetic classes the MIR lowerer / wasm backend allocate as ordinary
/// objects (`ref_cell` homes, call-by-name `a(i)` envs). Injected after
/// user layouts so their reserved ids cannot collide with declaration order.
fn inject_runtime_helper_classes(layouts: &mut HashMap<String, ClassLayout>) {
    layouts.insert(
        REF_CELL_CLASS_NAME.to_string(),
        helper_class_layout(
            REF_CELL_CLASS_NAME,
            REF_CELL_CLASS_ID,
            REF_CELL_SIZE,
            vec![FieldLayout {
                name: REF_CELL_VALUE_FIELD.to_string(),
                offset: REF_CELL_VALUE_OFFSET,
                size: I64_FIELD_SIZE,
                ty: FieldType::ObjectRef,
                class_qual: None,
            }],
        ),
    );
    layouts.insert(
        NAME_INT_ENV_CLASS_NAME.to_string(),
        helper_class_layout(
            NAME_INT_ENV_CLASS_NAME,
            NAME_INT_ENV_CLASS_ID,
            NAME_INT_ENV_SIZE,
            vec![FieldLayout {
                name: NAME_INT_ENV_ADDR_FIELD.to_string(),
                offset: NAME_INT_ENV_ADDR_OFFSET,
                size: I64_FIELD_SIZE,
                ty: FieldType::I64,
                class_qual: None,
            }],
        ),
    );
    layouts.insert(
        NAME_ARR1_ENV_CLASS_NAME.to_string(),
        helper_class_layout(
            NAME_ARR1_ENV_CLASS_NAME,
            NAME_ARR1_ENV_CLASS_ID,
            NAME_ARR1_ENV_SIZE,
            vec![
                FieldLayout {
                    name: NAME_ARR1_ENV_ARRAY_FIELD.to_string(),
                    offset: NAME_ARR1_ENV_ARRAY_OFFSET,
                    size: I64_FIELD_SIZE,
                    ty: FieldType::ObjectRef,
                    class_qual: None,
                },
                FieldLayout {
                    name: NAME_ARR1_ENV_INDEX_FIELD.to_string(),
                    offset: NAME_ARR1_ENV_INDEX_OFFSET,
                    size: I64_FIELD_SIZE,
                    ty: FieldType::I64,
                    class_qual: None,
                },
            ],
        ),
    );
    layouts.insert(
        NAME_PACK_ENV_CLASS_NAME.to_string(),
        helper_class_layout(
            NAME_PACK_ENV_CLASS_NAME,
            NAME_PACK_ENV_CLASS_ID,
            NAME_PACK_ENV_SIZE,
            (0..NAME_PACK_ENV_SLOT_COUNT)
                .map(|index| FieldLayout {
                    name: format!("s{index}"),
                    offset: name_pack_env_slot_offset(index),
                    size: I64_FIELD_SIZE,
                    ty: FieldType::ObjectRef,
                    class_qual: None,
                })
                .collect(),
        ),
    );
    layouts.insert(
        SIM_MAIN_CLASS_NAME.to_string(),
        helper_class_layout(
            SIM_MAIN_CLASS_NAME,
            SIM_MAIN_CLASS_ID,
            SIM_MAIN_SIZE,
            Vec::new(),
        ),
    );
    layouts.insert(
        NAME_THUNK_PAIR_CLASS_NAME.to_string(),
        helper_class_layout(
            NAME_THUNK_PAIR_CLASS_NAME,
            NAME_THUNK_PAIR_CLASS_ID,
            NAME_THUNK_PAIR_SIZE,
            vec![
                FieldLayout {
                    name: NAME_THUNK_PAIR_GET_FIELD.to_string(),
                    offset: NAME_THUNK_PAIR_GET_OFFSET,
                    size: I64_FIELD_SIZE,
                    ty: FieldType::I64,
                    class_qual: None,
                },
                FieldLayout {
                    name: NAME_THUNK_PAIR_ENV_FIELD.to_string(),
                    offset: NAME_THUNK_PAIR_ENV_OFFSET,
                    size: I64_FIELD_SIZE,
                    ty: FieldType::ObjectRef,
                    class_qual: None,
                },
            ],
        ),
    );
}

fn helper_class_layout(
    name: &str,
    class_id: i64,
    size: i64,
    fields: Vec<FieldLayout>,
) -> ClassLayout {
    ClassLayout {
        name: name.to_string(),
        declared_name: name.to_string(),
        decl_span: crate::error::Span::default(),
        fields,
        methods: vec![],
        virtual_methods: vec![],
        constructor_params: vec![],
        needs_init: false,
        runs_on_own_stack: false,
        enclosing_captures: vec![],
        size,
        class_id,
        system_block: 0,
        prefix: None,
    }
}

fn inject_missing_system_classes(raw: &mut Vec<ClassDeclaration>) {
    let mut map: HashMap<String, ClassDeclaration> = HashMap::new();
    for class in raw.iter() {
        map.insert(class.name.clone(), class.clone());
    }
    simulation::inject_system_classes(&mut map);
    for class in map.into_values() {
        let exists = raw
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&class.name));
        if !exists {
            raw.push(class);
        }
    }
}

/// Builds layouts from an already-collected list of (pre-concatenation) class
/// declarations. Public for unit tests.
pub fn layouts_from_classes(
    classes: &[ClassDeclaration],
) -> Result<HashMap<String, ClassLayout>, CompileError> {
    layouts_from_classes_with_sibling_captures(classes, &HashMap::new())
}

fn layouts_from_classes_with_sibling_captures(
    classes: &[ClassDeclaration],
    sibling_captures: &HashMap<String, SiblingCaptureInfo>,
) -> Result<HashMap<String, ClassLayout>, CompileError> {
    if classes.is_empty() {
        return Ok(HashMap::new());
    }
    let concatenated = concatenate::concatenate_classes(classes)?;
    // Preserve a stable class_id order: first occurrence in `classes`, then
    // any names only present after concatenation (shouldn't happen).
    let mut order: Vec<String> = Vec::new();
    for class in classes {
        if !order
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&class.name))
        {
            order.push(class.name.clone());
        }
    }
    for name in concatenated.keys() {
        if !order
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            order.push(name.clone());
        }
    }

    let mut layouts = HashMap::new();
    for (class_id, name) in order.into_iter().enumerate() {
        let Some(class) = concatenated
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(&name))
            .map(|(_, class)| class)
        else {
            continue;
        };
        let empty_sibling = SiblingCaptureInfo::default();
        let sibling = sibling_captures
            .iter()
            .find(|(n, _)| {
                n.eq_ignore_ascii_case(&class.name)
                    || n.eq_ignore_ascii_case(declared_class_name(&class.name))
            })
            .map(|(_, info)| info)
            .unwrap_or(&empty_sibling);
        let layout = layout_for_class(class, class_id as i64, sibling)?;
        layouts.insert(class.name.clone(), layout);
    }
    // Prefixed methods are outlined against the declaring class layout, but
    // `__this` may be a subclass instance. Enclosing-capture slots sit after
    // attributes, so they would otherwise shift with each prefix level and
    // prefix methods would load the wrong cell (simtst98: `a$outa` vs `new z`).
    align_enclosing_captures_across_prefix_families(&mut layouts, classes);
    Ok(layouts)
}

/// Place every enclosing-capture (and `This Outer`) slot at the same absolute
/// offset for all classes in a prefix family, starting after the widest
/// attribute region in that family.
///
/// Matching is by *enclosing source name* (`k` and `__simrt_encl_k` share a
/// slot): each class keeps its own field spelling so an attribute named `k`
/// is never replaced by a bare capture of the outer `k` (simtst98).
fn align_enclosing_captures_across_prefix_families(
    layouts: &mut HashMap<String, ClassLayout>,
    classes: &[ClassDeclaration],
) {
    if layouts.len() < 2 {
        return;
    }

    let mut prefixes: HashMap<String, Option<String>> = HashMap::new();
    for class in classes {
        let name = declared_class_name(&class.name).to_ascii_lowercase();
        let prefix = class
            .prefix
            .as_deref()
            .map(|prefix| declared_class_name(prefix).to_ascii_lowercase());
        prefixes.insert(name, prefix);
    }

    let mut families: HashMap<String, Vec<String>> = HashMap::new();
    for (key, layout) in layouts.iter() {
        let mut current = layout.declared_name.to_ascii_lowercase();
        let root = loop {
            match prefixes.get(&current).cloned().flatten() {
                Some(prefix) => current = prefix,
                None => break current,
            }
        };
        families.entry(root).or_default().push(key.clone());
    }

    for members in families.values() {
        if members.len() < 2 {
            continue;
        }
        if !members.iter().any(|key| {
            layouts.get(key).is_some_and(|layout| {
                !layout.enclosing_captures.is_empty()
                    || layout
                        .fields
                        .iter()
                        .any(|field| field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME))
            })
        }) {
            continue;
        }

        let attr_base = members
            .iter()
            .filter_map(|key| layouts.get(key))
            .map(attribute_region_end)
            .max()
            .unwrap_or(OBJECT_HEADER_SIZE);

        // source_key → (representative field name, ty, class_qual)
        let mut by_source: HashMap<String, (String, FieldType, Option<String>)> = HashMap::new();
        let mut enclosing_outer_qual: Option<String> = None;
        for key in members {
            let Some(layout) = layouts.get(key) else {
                continue;
            };
            for (name, field_ty) in &layout.enclosing_captures {
                let source = capture_alignment_key(name);
                let class_qual = layout
                    .fields
                    .iter()
                    .find(|field| field.name.eq_ignore_ascii_case(name))
                    .and_then(|field| field.class_qual.clone());
                let entry = by_source.entry(source).or_insert((
                    name.clone(),
                    *field_ty,
                    class_qual.clone(),
                ));
                if entry.2.is_none() {
                    entry.2 = class_qual;
                }
            }
            if enclosing_outer_qual.is_none() {
                if let Some(field) = layout
                    .fields
                    .iter()
                    .find(|field| field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME))
                {
                    enclosing_outer_qual = field.class_qual.clone();
                }
            }
        }

        let mut sources: Vec<String> = by_source.keys().cloned().collect();
        sources.sort();
        let source_offsets: HashMap<String, i64> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.clone(), attr_base + (index as i64) * I64_FIELD_SIZE))
            .collect();
        let after_captures = attr_base + (sources.len() as i64) * I64_FIELD_SIZE;

        let needs_enclosing_object = members.iter().any(|key| {
            layouts.get(key).is_some_and(|layout| {
                layout
                    .fields
                    .iter()
                    .any(|field| field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME))
            })
        });
        let enclosing_object = if needs_enclosing_object {
            Some((after_captures, enclosing_outer_qual.clone()))
        } else {
            None
        };

        for key in members {
            let Some(layout) = layouts.get_mut(key) else {
                continue;
            };
            // Preserve this class's capture spellings; ensure every family
            // source has a slot at the shared offset (mangling when needed).
            let mut own: Vec<(String, FieldType, Option<String>, i64)> = Vec::new();
            let mut have: HashSet<String> = HashSet::new();
            for (name, field_ty) in layout.enclosing_captures.clone() {
                let source = capture_alignment_key(&name);
                let Some(&offset) = source_offsets.get(&source) else {
                    continue;
                };
                let class_qual = layout
                    .fields
                    .iter()
                    .find(|field| field.name.eq_ignore_ascii_case(&name))
                    .and_then(|field| field.class_qual.clone());
                own.push((name, field_ty, class_qual, offset));
                have.insert(source);
            }
            for (source, (repr_name, field_ty, class_qual)) in &by_source {
                if have.contains(source) {
                    continue;
                }
                let Some(&offset) = source_offsets.get(source) else {
                    continue;
                };
                let field_name = capture_field_name_avoiding_attributes(layout, repr_name, source);
                own.push((field_name, *field_ty, class_qual.clone(), offset));
            }
            own.sort_by_key(|entry| entry.3);
            rewrite_layout_capture_tail(layout, &own, enclosing_object.clone());
        }
    }
}

fn capture_alignment_key(field_name: &str) -> String {
    enclosing_capture_source_name(field_name)
        .or_else(|| formal_proc_capture_source_name(field_name))
        .unwrap_or(field_name)
        .to_ascii_lowercase()
}

fn capture_field_name_avoiding_attributes(
    layout: &ClassLayout,
    representative: &str,
    source: &str,
) -> String {
    let attr_collision = layout.fields.iter().any(|field| {
        field.name.eq_ignore_ascii_case(source)
            && !layout
                .enclosing_captures
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&field.name))
            && !field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME)
    });
    if attr_collision
        || layout.fields.iter().any(|field| {
            field.name.eq_ignore_ascii_case(representative)
                && !layout
                    .enclosing_captures
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(&field.name))
        })
    {
        if representative.starts_with(ENCLOSING_CAPTURE_PREFIX)
            || representative.starts_with(FORMAL_PROC_CAPTURE_PREFIX)
        {
            return representative.to_string();
        }
        return enclosing_capture_field_name(source);
    }
    representative.to_string()
}

fn attribute_region_end(layout: &ClassLayout) -> i64 {
    let capture_names: HashSet<String> = layout
        .enclosing_captures
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    layout
        .fields
        .iter()
        .filter(|field| {
            !capture_names.contains(&field.name.to_ascii_lowercase())
                && !field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME)
        })
        .map(|field| field.offset + field.size)
        .max()
        .unwrap_or(OBJECT_HEADER_SIZE)
}

fn rewrite_layout_capture_tail(
    layout: &mut ClassLayout,
    captures: &[(String, FieldType, Option<String>, i64)],
    enclosing_object: Option<(i64, Option<String>)>,
) {
    let capture_names: HashSet<String> = layout
        .enclosing_captures
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    layout.fields.retain(|field| {
        !capture_names.contains(&field.name.to_ascii_lowercase())
            && !field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME)
    });

    layout.enclosing_captures.clear();
    let mut end = attribute_region_end(layout);
    for (name, field_ty, class_qual, offset) in captures {
        layout.fields.push(FieldLayout {
            name: name.clone(),
            offset: *offset,
            size: I64_FIELD_SIZE,
            ty: *field_ty,
            class_qual: class_qual.clone(),
        });
        layout.enclosing_captures.push((name.clone(), *field_ty));
        end = end.max(*offset + I64_FIELD_SIZE);
    }
    if let Some((offset, class_qual)) = enclosing_object {
        layout.fields.push(FieldLayout {
            name: ENCLOSING_OBJECT_FIELD_NAME.to_string(),
            offset,
            size: I64_FIELD_SIZE,
            ty: FieldType::ObjectRef,
            class_qual,
        });
        end = end.max(offset + I64_FIELD_SIZE);
    }
    layout.size = end;
}

fn layout_for_class(
    class: &ClassDeclaration,
    class_id: i64,
    sibling: &SiblingCaptureInfo,
) -> Result<ClassLayout, CompileError> {
    let constructor_params = collect_constructor_params(&class.name, &class.parameters)?;
    let methods = validate_class_body_mvp(&class.name, &class.body)?;
    let virtual_methods = collect_virtual_method_names(class);
    let needs_simset = class_needs_simset_slots(class);
    let needs_init = class_body_needs_init(&class.body)
        || !constructor_params.is_empty()
        || !class.tail_statements.is_empty();
    let has_detach =
        block_can_suspend(&class.body) || class.tail_statements.iter().any(statement_can_suspend);

    let mut fields = Vec::new();
    let mut offset = OBJECT_HEADER_SIZE;
    // Linkage ring pointers come before constructor params / body attrs so
    // every Process/Head/Link shares [`SIMSET_SUC_OFFSET`] / [`SIMSET_PRED_OFFSET`].
    if needs_simset {
        fields.push(FieldLayout {
            name: SIMSET_SUC_FIELD.to_string(),
            offset,
            size: I64_FIELD_SIZE,
            ty: FieldType::ObjectRef,
            class_qual: None,
        });
        offset += I64_FIELD_SIZE;
        fields.push(FieldLayout {
            name: SIMSET_PRED_FIELD.to_string(),
            offset,
            size: I64_FIELD_SIZE,
            ty: FieldType::ObjectRef,
            class_qual: None,
        });
        offset += I64_FIELD_SIZE;
        debug_assert_eq!(fields[0].offset, SIMSET_SUC_OFFSET);
        debug_assert_eq!(fields[1].offset, SIMSET_PRED_OFFSET);
    }
    for (name, field_ty) in &constructor_params {
        let class_qual = class
            .parameters
            .iter()
            .find(|param| param.name.eq_ignore_ascii_case(name))
            .and_then(|param| match &param.ty {
                Type::ObjectRef(qual) => Some(qual.clone()),
                Type::Array { element, .. } => match element.as_ref() {
                    Type::ObjectRef(qual) => Some(qual.clone()),
                    _ => None,
                },
                _ => None,
            });
        fields.push(FieldLayout {
            name: name.clone(),
            offset,
            size: I64_FIELD_SIZE,
            ty: *field_ty,
            class_qual,
        });
        offset += I64_FIELD_SIZE;
    }
    collect_specification_fields(
        &class.name,
        &class.specifications,
        &mut fields,
        &mut offset,
        &constructor_params,
    )?;
    collect_virtual_value_fields(
        &class.name,
        &class.virtual_part,
        &mut fields,
        &mut offset,
        &constructor_params,
    )?;
    collect_scalar_fields(
        &class.name,
        &class.body,
        &mut fields,
        &mut offset,
        &constructor_params,
    )?;
    collect_array_fields(
        &class.name,
        &class.body,
        &mut fields,
        &mut offset,
        &constructor_params,
    )?;

    let mut enclosing_captures = collect_enclosing_captures(
        class,
        &fields,
        &sibling.captures,
        &sibling.declared_types,
        &sibling.ref_quals,
    );
    for name in collect_formal_proc_capture_names(class, &sibling.formal_proc_params) {
        let field_name = formal_proc_capture_field_name(&name);
        if enclosing_captures
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&field_name))
            || fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(&field_name))
        {
            continue;
        }
        enclosing_captures.push((field_name, FieldType::ObjectRef));
    }
    enclosing_captures.sort_by_key(|a| a.0.to_ascii_lowercase());
    for (name, field_ty) in &enclosing_captures {
        let source = enclosing_capture_source_name(name).unwrap_or(name.as_str());
        let class_qual = match field_ty {
            FieldType::ObjectRef | FieldType::ArrayI64 => sibling
                .ref_quals
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(source))
                .map(|(_, q)| q.clone()),
            _ => None,
        };
        fields.push(FieldLayout {
            name: name.clone(),
            offset,
            size: I64_FIELD_SIZE,
            ty: *field_ty,
            class_qual,
        });
        offset += I64_FIELD_SIZE;
    }

    // Nested local class: snapshot the textually enclosing object for `This Outer`.
    if let Some(outer) = &sibling.enclosing_class {
        if !fields
            .iter()
            .any(|field| field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME))
        {
            fields.push(FieldLayout {
                name: ENCLOSING_OBJECT_FIELD_NAME.to_string(),
                offset,
                size: I64_FIELD_SIZE,
                ty: FieldType::ObjectRef,
                class_qual: Some(outer.clone()),
            });
            offset += I64_FIELD_SIZE;
        }
    }

    Ok(ClassLayout {
        name: class.name.clone(),
        declared_name: declared_class_name(&class.name).to_string(),
        decl_span: class.span.clone(),
        // Filled in by `layouts_for_program`, which alone sees the block
        // structure the classes were collected from.
        system_block: 0,
        prefix: class
            .prefix
            .as_deref()
            .map(|prefix| declared_class_name(prefix).to_string()),
        fields,
        methods,
        virtual_methods,
        constructor_params,
        needs_init: needs_init || has_detach,
        runs_on_own_stack: has_detach,
        enclosing_captures,
        size: offset,
        class_id,
    })
}

/// Source-level class identifier, stripping a MIR span qualifier (`A@123` → `A`).
pub fn declared_class_name(name: &str) -> &str {
    name.rsplit_once('@')
        .filter(|(_, suffix)| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
        .map(|(base, _)| base)
        .unwrap_or(name)
}

fn class_needs_simset_slots(class: &ClassDeclaration) -> bool {
    fn is_linkage_family(name: &str) -> bool {
        name.eq_ignore_ascii_case("linkage") || is_link_class(name) || is_head_class(name)
    }
    is_linkage_family(&class.name) || class.prefix.as_deref().is_some_and(is_linkage_family)
}

/// Lower-cased names of virtual value quantities declared on `class` (§5.6.7).
fn virtual_value_names(class: &ClassDeclaration) -> HashSet<String> {
    let mut names = HashSet::new();
    for spec in &class.virtual_part {
        match &spec.specifier {
            Specifier::Procedure | Specifier::Label | Specifier::Switch => {}
            _ => {
                for name in &spec.names {
                    if !is_fictitious_detach(name) {
                        names.insert(name.to_ascii_lowercase());
                    }
                }
            }
        }
    }
    names
}

/// Free simple names used in typed positions in the class body that are not
/// already attributes/methods/params — snapshotted from the enclosing block at
/// `new` (matches interpreter `enclosing_locals` for scalars/text/refs).
///
/// When a sibling procedure closes over a name that the class also declares as
/// an attribute (`P` uses outer `i` while `A` has attribute `i`), the outer
/// binding is still captured under [`ENCLOSING_CAPTURE_PREFIX`] so inlined free
/// procedures keep lexical access to the outer local.
fn collect_enclosing_captures(
    class: &ClassDeclaration,
    fields: &[FieldLayout],
    sibling_captures: &HashMap<String, FieldType>,
    declared_types: &HashMap<String, FieldType>,
    ref_quals: &HashMap<String, String>,
) -> Vec<(String, FieldType)> {
    let _ = ref_quals;
    let mut hints: HashMap<String, FieldType> = HashMap::new();
    collect_capture_hints_from_block(&class.body, &mut hints);
    for statement in &class.tail_statements {
        collect_capture_hints_from_statement(statement, &mut hints);
    }
    // Sibling procedures in the declaring block (e.g. `savei` used from a
    // class body) contribute their free names so inlined calls can resolve
    // them via `__this` capture fields.
    for (name, ty) in sibling_captures {
        merge_capture_hint(&mut hints, name, *ty);
    }
    // Refine heuristic types from enclosing declarations without adding
    // unused locals (Simulation `for i` must not become a Process capture).
    for (name, ty) in declared_types {
        refine_capture_hint_type(&mut hints, name, *ty);
    }
    // Names declared only in nested begin-blocks (not class-level attributes)
    // are locals of those blocks — not enclosing captures. Without this,
    // a block-local `ref(C) Y` is snapshotted onto A and later writeback
    // before Resume/Call clobbers the real Y to none (simtst76).
    // Class-level attributes that shadow an outer name still get a mangled
    // `__simrt_encl_*` capture (see layouts_shadowed_enclosing_capture).
    let mut nested_bound = HashSet::new();
    add_nested_block_bound_names_for_captures(&class.body, &mut nested_bound);
    let mut out: Vec<(String, FieldType)> = Vec::new();
    for (name, ty) in hints {
        if is_fictitious_detach(&name) || is_reserved_enclosing_name(&name) {
            continue;
        }
        if class
            .parameters
            .iter()
            .any(|param| param.name.eq_ignore_ascii_case(&name))
            || class
                .body
                .procedures
                .iter()
                .any(|procedure| procedure.name.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        if nested_bound.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        if virtual_value_names(class).contains(&name.to_ascii_lowercase()) {
            continue;
        }
        // `ref(C) cc` locals of the creating nested begin are excluded via
        // compound-bound-name filtering (see add_block_bound_names_for_captures).
        // Do **not** blanket-skip every `ref(ThisClass)` capture: peer coroutines
        // need e.g. X to carry main's `xx` so Resume(Y) can sync it (simtst62).
        let shadowed = fields
            .iter()
            .any(|field| field.name.eq_ignore_ascii_case(&name));
        if shadowed {
            let has_outer_binding = declared_types
                .iter()
                .any(|(outer, _)| outer.eq_ignore_ascii_case(&name))
                || sibling_captures
                    .iter()
                    .any(|(outer, _)| outer.eq_ignore_ascii_case(&name));
            if !has_outer_binding {
                continue;
            }
        }
        let field_name = if shadowed {
            enclosing_capture_field_name(&name)
        } else {
            name
        };
        out.push((field_name, ty));
    }
    out.sort_by_key(|a| a.0.to_ascii_lowercase());
    out
}

/// Free procedure names called in `class` that match an enclosing formal
/// procedure parameter — snapshotted as ObjectRef receiver slots so outlined
/// `C$__init` can restore the binding across `detach` / `resume`.
fn collect_formal_proc_capture_names(
    class: &ClassDeclaration,
    enclosing_formals: &HashSet<String>,
) -> Vec<String> {
    if enclosing_formals.is_empty() {
        return Vec::new();
    }
    let mut called = HashSet::new();
    collect_called_procedure_names_from_block(&class.body, &mut called);
    for statement in &class.tail_statements {
        collect_called_procedure_names_from_statement(statement, &mut called);
    }
    let mut own_methods = HashSet::new();
    for procedure in &class.body.procedures {
        own_methods.insert(procedure.name.to_ascii_lowercase());
    }
    let mut out: Vec<String> = called
        .into_iter()
        .filter(|name| {
            enclosing_formals.contains(name)
                && !own_methods.contains(name)
                && !is_reserved_enclosing_name(name)
                && !is_fictitious_detach(name)
        })
        .collect();
    out.sort();
    out
}

/// Field name for an enclosing local that collides with a class attribute.
pub fn enclosing_capture_field_name(source_name: &str) -> String {
    format!(
        "{ENCLOSING_CAPTURE_PREFIX}{}",
        source_name.to_ascii_lowercase()
    )
}

/// Inverse of [`enclosing_capture_field_name`].
pub fn enclosing_capture_source_name(field_name: &str) -> Option<&str> {
    field_name.strip_prefix(ENCLOSING_CAPTURE_PREFIX)
}

const ENCLOSING_CAPTURE_PREFIX: &str = "__simrt_encl_";
const FORMAL_PROC_CAPTURE_PREFIX: &str = "__simrt_fp_";

/// Field name for an enclosing formal-procedure binding snapshotted onto a
/// local class instance (receiver object for `FormalProcTarget::Method`).
pub fn formal_proc_capture_field_name(source_name: &str) -> String {
    format!(
        "{FORMAL_PROC_CAPTURE_PREFIX}{}",
        source_name.to_ascii_lowercase()
    )
}

/// Inverse of [`formal_proc_capture_field_name`].
pub fn formal_proc_capture_source_name(field_name: &str) -> Option<&str> {
    field_name.strip_prefix(FORMAL_PROC_CAPTURE_PREFIX)
}

fn merge_capture_hint(hints: &mut HashMap<String, FieldType>, name: &str, ty: FieldType) {
    let next = match hints.get(name).copied() {
        None => ty,
        Some(prev) => merge_field_types(prev, ty),
    };
    hints.insert(name.to_string(), next);
}

fn merge_field_types(prev: FieldType, ty: FieldType) -> FieldType {
    use FieldType::*;
    let is_array = |t: FieldType| matches!(t, ArrayI64 | ArrayBool | ArrayF64 | ArrayText);
    if is_array(prev) || is_array(ty) {
        return match (prev, ty) {
            (ArrayText, _) | (_, ArrayText) => ArrayText,
            (ArrayF64, _) | (_, ArrayF64) => ArrayF64,
            (ArrayBool, _) | (_, ArrayBool) => ArrayBool,
            _ => ArrayI64,
        };
    }
    match (prev, ty) {
        (ObjectRef, _) | (_, ObjectRef) => ObjectRef,
        (Text, _) | (_, Text) => Text,
        (F64, _) | (_, F64) => F64,
        (Bool, _) | (_, Bool) => Bool,
        (existing, _) => existing,
    }
}

#[derive(Clone, Copy)]
enum CaptureCtx {
    ObjectRef,
    I64,
    F64,
    Bool,
    Text,
    Untyped,
}

impl CaptureCtx {
    fn field_type(self) -> Option<FieldType> {
        match self {
            Self::ObjectRef => Some(FieldType::ObjectRef),
            Self::I64 => Some(FieldType::I64),
            Self::F64 => Some(FieldType::F64),
            Self::Bool => Some(FieldType::Bool),
            Self::Text => Some(FieldType::Text),
            // Free names in untyped positions are snapshotted as integer by
            // default; later typed uses / declared sibling types refine via
            // `merge_field_types` / `apply_declared_enclosing_types`.
            Self::Untyped => Some(FieldType::I64),
        }
    }
}

fn is_reserved_enclosing_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "true"
            | "false"
            | "none"
            | "this"
            | "detach"
            | "call"
            | "resume"
            | "hold"
            | "passivate"
            | "wait"
            | "cancel"
            | "time"
            | "current"
            | "main"
            | "nextev"
            | "activate"
            | "reactivate"
            // Linkage / Head attributes — bare `suc` in a Link body is
            // `this.suc`, not an enclosing free variable. Capturing it as
            // `__simrt_encl_suc` made writeback clobber SIMSET_SUC (simtst96).
            | "suc"
            | "pred"
            | "first"
            | "last"
            | "previous"
            | "cardinal"
            | "empty"
            | "outtext"
            | "outimage"
            | "outint"
            | "outchar"
            | "breakoutimage"
            | "inimage"
            | "inchar"
            | "endfile"
            | "sysin"
            | "sysout"
            | "outreal"
            | "outfix"
            | "outbool"
            | "inline"
            | "inner"
            | "copy"
            | "blanks"
            | "notext"
    )
}

fn collect_capture_hints_from_block(block: &Block, hints: &mut HashMap<String, FieldType>) {
    for statement in &block.statements {
        collect_capture_hints_from_statement(statement, hints);
    }
    for procedure in &block.procedures {
        collect_capture_hints_from_procedure(procedure, hints);
    }
    for inner in &block.body {
        collect_capture_hints_from_block(inner, hints);
    }
}

/// Free names in a procedure body (excluding its own parameters, result name,
/// and locals) become enclosing-capture candidates for the containing class.
fn collect_capture_hints_from_procedure(
    procedure: &ProcedureDeclaration,
    hints: &mut HashMap<String, FieldType>,
) {
    let mut local = HashMap::new();
    collect_capture_hints_from_block(&procedure.body, &mut local);
    let mut own = HashSet::new();
    own.insert(procedure.name.to_ascii_lowercase());
    for param in &procedure.parameters {
        own.insert(param.name.to_ascii_lowercase());
    }
    add_block_bound_names_for_captures(&procedure.body, &mut own);
    for (name, ty) in local {
        if !own.contains(&name.to_ascii_lowercase()) {
            merge_capture_hint(hints, &name, ty);
        }
    }
}

fn add_block_bound_names_for_captures(block: &Block, names: &mut HashSet<String>) {
    for decl in &block.declarations {
        for item in &decl.items {
            names.insert(item.name.to_ascii_lowercase());
        }
    }
    for array in &block.arrays {
        for segment in &array.segments {
            for name in &segment.names {
                names.insert(name.to_ascii_lowercase());
            }
        }
    }
    for switch in &block.switches {
        names.insert(switch.name.to_ascii_lowercase());
    }
    for procedure in &block.procedures {
        names.insert(procedure.name.to_ascii_lowercase());
        add_block_bound_names_for_captures(&procedure.body, names);
    }
    for class in &block.classes {
        names.insert(class.name.to_ascii_lowercase());
    }
    for inner in &block.body {
        add_block_bound_names_for_captures(inner, names);
    }
    // Nested `begin … end` compounds are statements, not `block.body` entries.
    // Their locals (e.g. simtst62 `ref(F) ff` inside E) must count as bound so
    // they are not snapshotted as enclosing captures and clobbered to none by
    // Resume/Call refresh+writeback.
    for statement in &block.statements {
        add_nested_bound_names_from_statement(statement, names);
    }
}

/// Bindings introduced by nested begin-blocks / compounds inside `block`, but
/// not by `block`'s own top-level declarations (class attributes stay eligible
/// for mangled enclosing captures when they shadow an outer name).
fn add_nested_block_bound_names_for_captures(block: &Block, names: &mut HashSet<String>) {
    for statement in &block.statements {
        add_nested_bound_names_from_statement(statement, names);
    }
    for procedure in &block.procedures {
        // Nested blocks inside methods are not class enclosing-capture sources.
        let _ = procedure;
    }
    for inner in &block.body {
        add_block_bound_names_for_captures(inner, names);
    }
}

fn add_nested_bound_names_from_statement(statement: &Statement, names: &mut HashSet<String>) {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => {
            add_nested_bound_names_from_statement(statement, names)
        }
        StatementKind::Compound(block) => add_block_bound_names_for_captures(block, names),
        StatementKind::If(if_stmt) => {
            add_nested_bound_names_from_statement(&if_stmt.then_branch, names);
            if let Some(else_branch) = &if_stmt.else_branch {
                add_nested_bound_names_from_statement(else_branch, names);
            }
        }
        StatementKind::While(while_stmt) => {
            add_nested_bound_names_from_statement(&while_stmt.body, names)
        }
        StatementKind::For(for_stmt) => {
            add_nested_bound_names_from_statement(&for_stmt.body, names)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                add_nested_bound_names_from_statement(&when.body, names);
            }
            if let Some(do_clause) = &inspect.do_clause {
                add_nested_bound_names_from_statement(do_clause, names);
            }
            if let Some(otherwise) = &inspect.otherwise {
                add_nested_bound_names_from_statement(otherwise, names);
            }
        }
        _ => {}
    }
}

fn collect_capture_hints_from_statement(
    statement: &Statement,
    hints: &mut HashMap<String, FieldType>,
) {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => {
            collect_capture_hints_from_statement(statement, hints)
        }
        StatementKind::Compound(block) => collect_capture_hints_from_block(block, hints),
        StatementKind::If(if_stmt) => {
            collect_capture_hints_from_expr(&if_stmt.condition, CaptureCtx::Bool, hints);
            collect_capture_hints_from_statement(&if_stmt.then_branch, hints);
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_capture_hints_from_statement(else_branch, hints);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_capture_hints_from_expr(&while_stmt.condition, CaptureCtx::Bool, hints);
            collect_capture_hints_from_statement(&while_stmt.body, hints);
        }
        StatementKind::For(for_stmt) => {
            for element in &for_stmt.elements {
                match element {
                    ForListElement::Value { expr, while_cond }
                    | ForListElement::Reference { expr, while_cond } => {
                        let ctx = if matches!(element, ForListElement::Reference { .. }) {
                            CaptureCtx::ObjectRef
                        } else {
                            CaptureCtx::I64
                        };
                        collect_capture_hints_from_expr(expr, ctx, hints);
                        if let Some(cond) = while_cond {
                            collect_capture_hints_from_expr(cond, CaptureCtx::Bool, hints);
                        }
                    }
                    ForListElement::StepUntil { start, step, until } => {
                        collect_capture_hints_from_expr(start, CaptureCtx::I64, hints);
                        collect_capture_hints_from_expr(step, CaptureCtx::I64, hints);
                        collect_capture_hints_from_expr(until, CaptureCtx::I64, hints);
                    }
                }
            }
            if !for_stmt.variable.is_empty() {
                merge_capture_hint(hints, &for_stmt.variable, FieldType::I64);
            }
            collect_capture_hints_from_statement(&for_stmt.body, hints);
        }
        StatementKind::Assignment(assignment) => {
            collect_capture_hints_from_assignment(assignment, hints)
        }
        StatementKind::ProcedureCall(call) => collect_capture_hints_from_call(call, hints),
        StatementKind::Activate(activate) => {
            collect_capture_hints_from_expr(&activate.target, CaptureCtx::ObjectRef, hints);
            if let Some(timing) = &activate.timing {
                collect_capture_hints_from_timing(timing, hints);
            }
        }
        StatementKind::Reactivate(reactivate) => {
            collect_capture_hints_from_expr(&reactivate.target, CaptureCtx::ObjectRef, hints);
            if let Some(timing) = &reactivate.timing {
                collect_capture_hints_from_timing(timing, hints);
            }
        }
        StatementKind::Expr(expr) => {
            collect_capture_hints_from_expr(expr, CaptureCtx::Untyped, hints)
        }
        StatementKind::Inspect(inspect) => {
            collect_capture_hints_from_expr(&inspect.object, CaptureCtx::ObjectRef, hints);
            for when in &inspect.when_clauses {
                collect_capture_hints_from_statement(&when.body, hints);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_capture_hints_from_statement(do_clause, hints);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_capture_hints_from_statement(otherwise, hints);
            }
        }
        _ => {}
    }
}

fn collect_capture_hints_from_assignment(
    assignment: &Assignment,
    hints: &mut HashMap<String, FieldType>,
) {
    if let Variable::Simple(name) = &assignment.lhs {
        // Prefer a concrete type from the RHS when possible; default integer.
        match &assignment.rhs {
            AssignmentRhs::Expr(expr) => {
                collect_capture_hints_from_expr(expr, CaptureCtx::Untyped, hints);
                if matches!(assignment.operator, AssignOperator::AssignAlt) {
                    // `:-` is reference / text-frame assign.
                    if matches!(
                        expr.kind,
                        ExprKind::None
                            | ExprKind::Notext
                            | ExprKind::New { .. }
                            | ExprKind::This(_)
                    ) {
                        merge_capture_hint(hints, name, FieldType::ObjectRef);
                    } else {
                        merge_capture_hint(hints, name, FieldType::Text);
                    }
                } else if matches!(
                    expr.kind,
                    ExprKind::StringLiteral(_)
                        | ExprKind::Notext
                        | ExprKind::Binary {
                            op: BinaryOp::TextConcat,
                            ..
                        }
                ) || matches!(
                    &expr.kind,
                    ExprKind::FunctionCall { name: callee, .. }
                        if matches!(
                            callee.to_ascii_lowercase().as_str(),
                            "blanks" | "copy" | "fileread" | "inline"
                        )
                ) {
                    // Text content assignment (`t := "…"`, `t := blanks(n)`).
                    merge_capture_hint(hints, name, FieldType::Text);
                } else if matches!(expr.kind, ExprKind::BooleanLiteral(_)) {
                    merge_capture_hint(hints, name, FieldType::Bool);
                } else {
                    merge_capture_hint(hints, name, FieldType::I64);
                }
            }
            AssignmentRhs::Chain(inner) => {
                collect_capture_hints_from_assignment(inner, hints);
                merge_capture_hint(hints, name, FieldType::I64);
            }
        }
    } else {
        match &assignment.rhs {
            AssignmentRhs::Expr(expr) => {
                collect_capture_hints_from_expr(expr, CaptureCtx::Untyped, hints)
            }
            AssignmentRhs::Chain(inner) => collect_capture_hints_from_assignment(inner, hints),
        }
        // Subscripted `:-` is text-frame / object-ref element assign; prefer
        // ArrayText over the Untyped→ArrayI64 heuristic so sibling captures
        // stay correct when declared types are not yet applied.
        let lhs_ctx = if matches!(assignment.operator, AssignOperator::AssignAlt) {
            CaptureCtx::Text
        } else {
            CaptureCtx::Untyped
        };
        collect_capture_hints_from_variable(&assignment.lhs, lhs_ctx, hints);
    }
}

fn collect_capture_hints_from_call(call: &ProcedureCall, hints: &mut HashMap<String, FieldType>) {
    let lower = call.name.to_ascii_lowercase();
    let arg_ctx = match lower.as_str() {
        "wait" | "into" | "precede" | "follow" | "resume" | "call" | "cancel" => {
            CaptureCtx::ObjectRef
        }
        "outint" => CaptureCtx::I64,
        "outreal" | "outfix" => CaptureCtx::F64,
        "outbool" => CaptureCtx::Bool,
        "outtext" | "copy" | "blanks" => CaptureCtx::Text,
        _ => CaptureCtx::Untyped,
    };
    for argument in &call.arguments {
        collect_capture_hints_from_expr(argument, arg_ctx, hints);
    }
}

fn collect_capture_hints_from_timing(
    timing: &crate::ast::SimulationTiming,
    hints: &mut HashMap<String, FieldType>,
) {
    match timing {
        crate::ast::SimulationTiming::Delay(expr)
        | crate::ast::SimulationTiming::At(expr)
        | crate::ast::SimulationTiming::Before(expr)
        | crate::ast::SimulationTiming::After(expr) => {
            collect_capture_hints_from_expr(expr, CaptureCtx::ObjectRef, hints);
        }
    }
}

fn collect_capture_hints_from_expr(
    expr: &Expr,
    ctx: CaptureCtx,
    hints: &mut HashMap<String, FieldType>,
) {
    match &expr.kind {
        ExprKind::Variable(variable) => {
            collect_capture_hints_from_variable(variable, ctx, hints);
        }
        ExprKind::Unary { operand, .. } => {
            let child = match ctx {
                CaptureCtx::Untyped => CaptureCtx::I64,
                other => other,
            };
            collect_capture_hints_from_expr(operand, child, hints);
        }
        ExprKind::Binary { op, left, right } => {
            let child = match op {
                BinaryOp::TextConcat => CaptureCtx::Text,
                BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Imp
                | BinaryOp::Eqv
                | BinaryOp::AndThen
                | BinaryOp::OrElse => CaptureCtx::Bool,
                _ => match ctx {
                    CaptureCtx::F64
                    | CaptureCtx::Text
                    | CaptureCtx::Bool
                    | CaptureCtx::ObjectRef => ctx,
                    _ => CaptureCtx::I64,
                },
            };
            collect_capture_hints_from_expr(left, child, hints);
            collect_capture_hints_from_expr(right, child, hints);
        }
        ExprKind::Relation { left, right, op } => {
            match op {
                RelationOp::Is | RelationOp::In | RelationOp::RefEq | RelationOp::RefNe => {
                    collect_capture_hints_from_expr(left, CaptureCtx::ObjectRef, hints);
                    collect_capture_hints_from_expr(right, CaptureCtx::ObjectRef, hints);
                }
                RelationOp::Eq | RelationOp::Ne => {
                    // Value `=`/`<>` may be arithmetic, character, or text.
                    // Prefer a single interpretation when the operands make it
                    // obvious — dual I64+Text hints otherwise collapse to Text
                    // via merge_field_types and break `c1 = '*'`.
                    if expr_looks_text(left) || expr_looks_text(right) {
                        collect_capture_hints_from_expr(left, CaptureCtx::Text, hints);
                        collect_capture_hints_from_expr(right, CaptureCtx::Text, hints);
                    } else if expr_looks_character(left) || expr_looks_character(right) {
                        collect_capture_hints_from_expr(left, CaptureCtx::I64, hints);
                        collect_capture_hints_from_expr(right, CaptureCtx::I64, hints);
                    } else if expr_looks_number(left) || expr_looks_number(right) {
                        // `ia(1) = 2` — numeric literal forces arithmetic, not
                        // text-array (FunctionCall alone is ambiguous).
                        collect_capture_hints_from_expr(left, CaptureCtx::I64, hints);
                        collect_capture_hints_from_expr(right, CaptureCtx::I64, hints);
                    } else if (expr_might_be_text_array_elem(left)
                        && (expr_is_simple_var(right) || expr_looks_text(right)))
                        || (expr_might_be_text_array_elem(right)
                            && (expr_is_simple_var(left) || expr_looks_text(left)))
                    {
                        // `timage <> ta(ti)` — text-ish simple name vs index form.
                        collect_capture_hints_from_expr(left, CaptureCtx::Text, hints);
                        collect_capture_hints_from_expr(right, CaptureCtx::Text, hints);
                    } else {
                        collect_capture_hints_from_expr(left, CaptureCtx::I64, hints);
                        collect_capture_hints_from_expr(right, CaptureCtx::I64, hints);
                    }
                }
                _ => {
                    let child = CaptureCtx::I64;
                    collect_capture_hints_from_expr(left, child, hints);
                    collect_capture_hints_from_expr(right, child, hints);
                }
            }
        }
        ExprKind::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_capture_hints_from_expr(condition, CaptureCtx::Bool, hints);
            collect_capture_hints_from_expr(then_expr, ctx, hints);
            collect_capture_hints_from_expr(else_expr, ctx, hints);
        }
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            collect_capture_hints_from_expr(inner, ctx, hints)
        }
        ExprKind::FunctionCall { name, arguments } => {
            let lower = name.to_ascii_lowercase();
            let arg_ctx = match lower.as_str() {
                "wait" | "into" | "precede" | "follow" | "resume" | "call" | "cancel" => {
                    CaptureCtx::ObjectRef
                }
                "outint" => CaptureCtx::I64,
                "outreal" | "outfix" => CaptureCtx::F64,
                "outbool" => CaptureCtx::Bool,
                "outtext" | "copy" | "blanks" => CaptureCtx::Text,
                _ => CaptureCtx::Untyped,
            };
            // `name(args)` may be array indexing (§5.2); snapshot the descriptor.
            if !matches!(
                lower.as_str(),
                "wait"
                    | "into"
                    | "precede"
                    | "follow"
                    | "resume"
                    | "call"
                    | "cancel"
                    | "outint"
                    | "outreal"
                    | "outfix"
                    | "outbool"
                    | "outtext"
                    | "copy"
                    | "blanks"
                    | "fileexists"
                    | "fileread"
                    | "filewrite"
            ) {
                let array_ty = match ctx {
                    CaptureCtx::Text => FieldType::ArrayText,
                    CaptureCtx::F64 => FieldType::ArrayF64,
                    _ => FieldType::ArrayI64,
                };
                merge_capture_hint(hints, name, array_ty);
            }
            let arg_ctx = match (ctx, arg_ctx) {
                (
                    CaptureCtx::Text
                    | CaptureCtx::F64
                    | CaptureCtx::Bool
                    | CaptureCtx::ObjectRef
                    | CaptureCtx::I64,
                    CaptureCtx::Untyped,
                ) => CaptureCtx::I64,
                (_, CaptureCtx::Untyped) => CaptureCtx::I64,
                (_, other) => other,
            };
            for argument in arguments {
                collect_capture_hints_from_expr(argument, arg_ctx, hints);
            }
        }
        ExprKind::RemoteAccess { object, attribute } => {
            let receiver = if crate::text::TextIntrinsic::parse(attribute).is_some() {
                CaptureCtx::Text
            } else {
                CaptureCtx::ObjectRef
            };
            collect_capture_hints_from_expr(object, receiver, hints);
        }
        ExprKind::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            let receiver = if crate::text::TextIntrinsic::parse(attribute).is_some() {
                CaptureCtx::Text
            } else {
                CaptureCtx::ObjectRef
            };
            collect_capture_hints_from_expr(object, receiver, hints);
            for argument in arguments {
                collect_capture_hints_from_expr(argument, CaptureCtx::Untyped, hints);
            }
        }
        ExprKind::New { arguments, .. } => {
            if let Some(args) = arguments {
                for argument in args {
                    collect_capture_hints_from_expr(argument, CaptureCtx::Untyped, hints);
                }
            }
        }
        ExprKind::BooleanLiteral(_)
        | ExprKind::NumberLiteral { .. }
        | ExprKind::StringLiteral(_)
        | ExprKind::CharacterLiteral(_)
        | ExprKind::Notext
        | ExprKind::None
        | ExprKind::This(_) => {}
    }
}

fn collect_capture_hints_from_variable(
    variable: &Variable,
    ctx: CaptureCtx,
    hints: &mut HashMap<String, FieldType>,
) {
    match variable {
        Variable::Simple(name) => {
            if let Some(ty) = ctx.field_type() {
                merge_capture_hint(hints, name, ty);
            }
        }
        Variable::Subscripted { name, subscripts } => {
            // Array descriptors from the enclosing block are snapshotted as
            // pointer-sized array slots on the instance.
            let array_ty = match ctx {
                CaptureCtx::Text => FieldType::ArrayText,
                CaptureCtx::F64 => FieldType::ArrayF64,
                _ => FieldType::ArrayI64,
            };
            merge_capture_hint(hints, name, array_ty);
            for subscript in subscripts {
                collect_capture_hints_from_expr(subscript, CaptureCtx::I64, hints);
            }
        }
        Variable::Qua { object, .. } => {
            collect_capture_hints_from_variable(object, CaptureCtx::ObjectRef, hints);
        }
        Variable::Remote { object, attribute } => {
            let receiver = if crate::text::TextIntrinsic::parse(attribute).is_some() {
                CaptureCtx::Text
            } else {
                CaptureCtx::ObjectRef
            };
            collect_capture_hints_from_variable(object, receiver, hints);
        }
        Variable::RemoteCall {
            object,
            attribute,
            arguments,
        } => {
            let receiver = if crate::text::TextIntrinsic::parse(attribute).is_some() {
                CaptureCtx::Text
            } else {
                CaptureCtx::ObjectRef
            };
            collect_capture_hints_from_variable(object, receiver, hints);
            for argument in arguments {
                collect_capture_hints_from_expr(argument, CaptureCtx::Untyped, hints);
            }
        }
    }
}

fn expr_looks_text(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::StringLiteral(_) | ExprKind::Notext => true,
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => expr_looks_text(inner),
        ExprKind::Binary {
            op: BinaryOp::TextConcat,
            ..
        } => true,
        ExprKind::FunctionCall { name, .. } => {
            let lower = name.to_ascii_lowercase();
            matches!(
                lower.as_str(),
                "copy" | "blanks" | "outtext" | "sub" | "strip"
            )
        }
        _ => false,
    }
}

fn expr_looks_number(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::NumberLiteral { .. } => true,
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => expr_looks_number(inner),
        _ => false,
    }
}

fn expr_is_simple_var(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Variable(Variable::Simple(_)) => true,
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => expr_is_simple_var(inner),
        _ => false,
    }
}

fn expr_looks_character(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::CharacterLiteral(_) => true,
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => expr_looks_character(inner),
        _ => false,
    }
}

fn expr_might_be_text_array_elem(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Variable(Variable::Subscripted { .. }) => true,
        // Simula array access often parses as a function call (`ta(ti)`).
        ExprKind::FunctionCall { .. } => true,
        ExprKind::Paren(inner) | ExprKind::Qua { object: inner, .. } => {
            expr_might_be_text_array_elem(inner)
        }
        _ => false,
    }
}

/// A `detach;` / `This X.Detach;` / `obj.Detach;` written as a statement of the
/// body itself. [`block_can_suspend`] walks the nested statements around it.
pub fn statement_is_top_level_detach(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => statement_is_top_level_detach(statement),
        StatementKind::ProcedureCall(call) => {
            is_fictitious_detach(&call.name) && call.arguments.is_empty()
        }
        StatementKind::Expr(expr) => expr_is_fictitious_detach(expr),
        _ => false,
    }
}

fn expr_is_fictitious_detach(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Paren(inner) => expr_is_fictitious_detach(inner),
        ExprKind::RemoteAccess { attribute, .. } => is_fictitious_detach(attribute),
        ExprKind::RemoteCall {
            attribute,
            arguments,
            ..
        } => is_fictitious_detach(attribute) && arguments.is_empty(),
        _ => false,
    }
}

fn expr_this_outer_detach_class(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Paren(inner) => expr_this_outer_detach_class(inner),
        ExprKind::RemoteAccess { object, attribute } if is_fictitious_detach(attribute) => {
            match &object.kind {
                ExprKind::This(class_name) => Some(class_name.clone()),
                _ => None,
            }
        }
        ExprKind::RemoteCall {
            object,
            attribute,
            arguments,
        } if is_fictitious_detach(attribute) && arguments.is_empty() => match &object.kind {
            ExprKind::This(class_name) => Some(class_name.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// When a top-level detach is `This Outer.Detach` from a nested local class,
/// returns `Outer` so MIR can detach the enclosing object (simtst76).
pub fn statement_detach_outer_class(statement: &Statement) -> Option<String> {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => statement_detach_outer_class(statement),
        StatementKind::Expr(expr) => expr_this_outer_detach_class(expr),
        _ => None,
    }
}

/// Top-level `resume(x);` — needs a continuation PC like `detach`.
fn statement_is_top_level_resume(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => statement_is_top_level_resume(statement),
        StatementKind::ProcedureCall(call) => {
            call.name.eq_ignore_ascii_case("resume") && call.arguments.len() == 1
        }
        _ => false,
    }
}

/// Whether a class body can suspend, at any nesting depth and including its own
/// procedure attributes. A component's stack keeps the whole body live across a
/// transfer, so a `detach` however deeply nested still makes the class one.
fn block_can_suspend(block: &Block) -> bool {
    block.statements.iter().any(statement_can_suspend)
        || block.body.iter().any(block_can_suspend)
        || block
            .procedures
            .iter()
            .any(|procedure| block_can_suspend(&procedure.body))
}

fn statement_can_suspend(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => statement_can_suspend(statement),
        StatementKind::ProcedureCall(call) => {
            (call.name.eq_ignore_ascii_case("detach") && call.arguments.is_empty())
                || (call.name.eq_ignore_ascii_case("resume") && call.arguments.len() == 1)
                || (call.name.eq_ignore_ascii_case("call") && call.arguments.len() == 1)
        }
        StatementKind::Compound(block) => block_can_suspend(block),
        StatementKind::If(if_stmt) => {
            statement_can_suspend(&if_stmt.then_branch)
                || if_stmt
                    .else_branch
                    .as_ref()
                    .is_some_and(|branch| statement_can_suspend(branch))
        }
        StatementKind::While(while_stmt) => statement_can_suspend(&while_stmt.body),
        StatementKind::For(for_stmt) => statement_can_suspend(&for_stmt.body),
        StatementKind::Inspect(inspect) => {
            inspect
                .when_clauses
                .iter()
                .any(|clause| statement_can_suspend(&clause.body))
                || inspect
                    .do_clause
                    .as_ref()
                    .is_some_and(|branch| statement_can_suspend(branch))
                || inspect
                    .otherwise
                    .as_ref()
                    .is_some_and(|branch| statement_can_suspend(branch))
        }
        _ => statement_is_top_level_detach(statement) || statement_is_top_level_resume(statement),
    }
}

fn collect_virtual_method_names(class: &ClassDeclaration) -> Vec<String> {
    let mut names = Vec::new();
    for spec in &class.virtual_part {
        for name in &spec.names {
            if is_fictitious_detach(name) {
                continue;
            }
            if !names
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(name))
            {
                names.push(name.clone());
            }
        }
    }
    names
}

/// Validates class formal parameters and returns them as ordered layout fields.
/// Call-by-value integer/boolean/real/text/object-reference parameters are
/// accepted, as is call-by-reference for text/object-reference (their
/// *default* transmission mode per the Standard — e.g. `Class C(t); Text t;`
/// — sharing the call-by-value lowering, see `mir::lower::outlined_param_allowed`).
fn collect_constructor_params(
    class_name: &str,
    parameters: &[FormalParameter],
) -> Result<Vec<(String, FieldType)>, CompileError> {
    let mut out = Vec::new();
    for param in parameters {
        if param.is_procedure {
            return Err(CompileError::codegen(format!(
                "MIR lowering: class '{class_name}' parameter '{}' is a formal procedure parameter, which is not supported in the Phase 5 MVP",
                param.name
            )));
        }
        if param.mode == ParamMode::Name {
            return Err(CompileError::codegen(format!(
                "MIR lowering: class '{class_name}' parameter '{}' uses call-by-name transmission, which is not legal for class parameters",
                param.name
            )));
        }
        let field_ty = match &param.ty {
            Type::Integer { .. } | Type::Character => FieldType::I64,
            Type::Boolean => FieldType::Bool,
            Type::Real { .. } => FieldType::F64,
            Type::Text => FieldType::Text,
            Type::ObjectRef(_) => FieldType::ObjectRef,
            Type::Array { element, .. } => match element.as_ref() {
                Type::Integer { .. } | Type::Character | Type::ObjectRef(_) => FieldType::ArrayI64,
                Type::Boolean => FieldType::ArrayBool,
                Type::Real { .. } => FieldType::ArrayF64,
                Type::Text => FieldType::ArrayText,
                other => {
                    return Err(CompileError::codegen(format!(
                        "MIR lowering: class '{class_name}' parameter '{}' has array element type '{other}'; only integer, boolean, character, real, text, and object-reference arrays are supported",
                        param.name
                    )));
                }
            },
        };
        if param.mode == ParamMode::Reference
            && !matches!(
                field_ty,
                FieldType::Text
                    | FieldType::ObjectRef
                    | FieldType::ArrayI64
                    | FieldType::ArrayBool
                    | FieldType::ArrayF64
                    | FieldType::ArrayText
            )
        {
            return Err(CompileError::codegen(format!(
                "MIR lowering: class '{class_name}' parameter '{}' uses call-by-reference transmission; only text/object-reference/array parameters support call-by-reference in the Phase 5 MVP",
                param.name
            )));
        }
        if out
            .iter()
            .any(|(existing, _): &(String, FieldType)| existing.eq_ignore_ascii_case(&param.name))
        {
            return Err(CompileError::codegen(format!(
                "MIR lowering: duplicate class parameter '{}' in class '{class_name}'",
                param.name
            )));
        }
        out.push((param.name.clone(), field_ty));
    }
    Ok(out)
}

fn class_body_needs_init(body: &Block) -> bool {
    !body.arrays.is_empty()
        || body
            .statements
            .iter()
            .any(|statement| !matches!(statement.kind, StatementKind::Dummy))
        || body.body.iter().any(class_body_needs_init)
}

/// Validates the class body and returns the list of user method names.
fn validate_class_body_mvp(class_name: &str, body: &Block) -> Result<Vec<String>, CompileError> {
    let mut methods = Vec::new();
    for procedure in &body.procedures {
        if is_fictitious_detach(&procedure.name) {
            continue;
        }
        validate_class_method(class_name, procedure)?;
        if methods
            .iter()
            .any(|name: &String| name.eq_ignore_ascii_case(&procedure.name))
        {
            return Err(CompileError::codegen(format!(
                "MIR lowering: duplicate method '{}' in class '{class_name}'",
                procedure.name
            )));
        }
        methods.push(procedure.name.clone());
    }
    // Nested classes (`class.body.classes`) are already collected as their
    // own top-level `ClassLayout`s by `collect_classes` (recursing into each
    // class's body) — they are not folded into this outer class's methods
    // or fields. Array attributes are collected as descriptor fields by
    // [`collect_array_fields`] and allocated in `ClassName$__init`.
    for nested in &body.body {
        let nested_methods = validate_class_body_mvp(class_name, nested)?;
        for name in nested_methods {
            if methods
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&name))
            {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: duplicate method '{name}' in class '{class_name}'"
                )));
            }
            methods.push(name);
        }
    }
    // Prefixed / compound statements (`A begin procedure B; … end`) declare
    // methods of the enclosing class for MIR purposes.
    for statement in &body.statements {
        let stmt_methods = validate_class_methods_from_statement(class_name, statement)?;
        for name in stmt_methods {
            if methods
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&name))
            {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: duplicate method '{name}' in class '{class_name}'"
                )));
            }
            methods.push(name);
        }
    }
    Ok(methods)
}

fn validate_class_methods_from_statement(
    class_name: &str,
    statement: &Statement,
) -> Result<Vec<String>, CompileError> {
    match &statement.kind {
        StatementKind::Compound(block) => validate_class_body_mvp(class_name, block),
        StatementKind::If(if_stmt) => {
            let mut methods =
                validate_class_methods_from_statement(class_name, &if_stmt.then_branch)?;
            if let Some(else_branch) = &if_stmt.else_branch {
                methods.extend(validate_class_methods_from_statement(
                    class_name,
                    else_branch,
                )?);
            }
            Ok(methods)
        }
        StatementKind::While(while_stmt) => {
            validate_class_methods_from_statement(class_name, &while_stmt.body)
        }
        StatementKind::For(for_stmt) => {
            validate_class_methods_from_statement(class_name, &for_stmt.body)
        }
        StatementKind::Labeled { statement, .. } => {
            validate_class_methods_from_statement(class_name, statement)
        }
        StatementKind::Inspect(inspect) => {
            let mut methods = Vec::new();
            for when in &inspect.when_clauses {
                methods.extend(validate_class_methods_from_statement(
                    class_name, &when.body,
                )?);
            }
            if let Some(do_clause) = &inspect.do_clause {
                methods.extend(validate_class_methods_from_statement(
                    class_name, do_clause,
                )?);
            }
            if let Some(otherwise) = &inspect.otherwise {
                methods.extend(validate_class_methods_from_statement(
                    class_name, otherwise,
                )?);
            }
            Ok(methods)
        }
        _ => Ok(Vec::new()),
    }
}

fn validate_class_method(
    class_name: &str,
    procedure: &ProcedureDeclaration,
) -> Result<(), CompileError> {
    if procedure.is_external {
        return Err(CompileError::codegen(format!(
            "MIR lowering: class '{class_name}' method '{}' is external; external methods are not supported in the Phase 5 MVP",
            procedure.name
        )));
    }
    match &procedure.result_type {
        None => {}
        Some(Type::Integer { .. })
        | Some(Type::Character)
        | Some(Type::Boolean)
        | Some(Type::Real { .. })
        | Some(Type::Text)
        | Some(Type::ObjectRef(_)) => {}
        Some(other) => {
            return Err(CompileError::codegen(format!(
                "MIR lowering: class '{class_name}' method '{}' has result type '{other}'; only void, integer, character, boolean, real, text, or object-reference results are supported in the Phase 5 method MVP",
                procedure.name
            )));
        }
    }
    for param in &procedure.parameters {
        if param.is_procedure {
            // Formal procedure parameters are accepted; call sites force
            // call-site inlining of the method (same as free procedures).
            continue;
        }
        match param.mode {
            ParamMode::Value => match &param.ty {
                Type::Integer { .. }
                | Type::Character
                | Type::Boolean
                | Type::Real { .. }
                | Type::Text
                | Type::ObjectRef(_) => {}
                other => {
                    return Err(CompileError::codegen(format!(
                        "MIR lowering: class '{class_name}' method '{}' parameter '{}' has type '{other}'; only integer/character/boolean/real/text/object-reference value parameters are supported in the Phase 5 method MVP",
                        procedure.name, param.name
                    )));
                }
            },
            ParamMode::Reference => match &param.ty {
                // Text/object-ref are already pointer-sized handles, so
                // outlined call-by-reference shares the call-by-value
                // lowering (see `mir::lower::outlined_param_allowed`).
                Type::Text | Type::ObjectRef(_) => {}
                other => {
                    return Err(CompileError::codegen(format!(
                        "MIR lowering: class '{class_name}' method '{}' parameter '{}' has type '{other}'; only text/object-reference call-by-reference parameters are supported in the Phase 5 method MVP",
                        procedure.name, param.name
                    )));
                }
            },
            ParamMode::Name => match &param.ty {
                // Matches free-procedure inlining (`validate_name_param_procedure`):
                // integer/character outline as name-thunks; other scalar name
                // formals are accepted so the method can be call-site inlined
                // (see partition of methods with non-thunk name params).
                Type::Integer { .. }
                | Type::Character
                | Type::Boolean
                | Type::Real { .. }
                | Type::Text
                | Type::ObjectRef(_) => {}
                other => {
                    return Err(CompileError::codegen(format!(
                        "MIR lowering: class '{class_name}' method '{}' parameter '{}' has type '{other}'; only scalar call-by-name parameters are supported in the Phase 5 method MVP",
                        procedure.name, param.name
                    )));
                }
            },
        }
    }
    Ok(())
}

fn collect_specification_fields(
    class_name: &str,
    specifications: &[crate::ast::Specification],
    fields: &mut Vec<FieldLayout>,
    offset: &mut i64,
    constructor_params: &[(String, FieldType)],
) -> Result<(), CompileError> {
    for spec in specifications {
        let (field_ty, class_qual) = match &spec.specifier {
            Specifier::Type(Type::Integer { .. } | Type::Character) => (FieldType::I64, None),
            Specifier::Type(Type::Boolean) => (FieldType::Bool, None),
            Specifier::Type(Type::Real { .. }) => (FieldType::F64, None),
            Specifier::Type(Type::Text) => (FieldType::Text, None),
            Specifier::Type(Type::ObjectRef(qual)) => (FieldType::ObjectRef, Some(qual.clone())),
            Specifier::Type(other) => {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: class '{class_name}' has unsupported attribute type '{other}'; only integer, boolean, character, real, text, and object-reference attributes are supported"
                )));
            }
            Specifier::TypeArray(_)
            | Specifier::Array
            | Specifier::Label
            | Specifier::Switch
            | Specifier::Procedure
            | Specifier::TypeProcedure(_) => continue,
        };
        for name in &spec.names {
            if constructor_params
                .iter()
                .any(|(param, _)| param.eq_ignore_ascii_case(name))
            {
                continue;
            }
            if fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            fields.push(FieldLayout {
                name: name.clone(),
                offset: *offset,
                size: I64_FIELD_SIZE,
                ty: field_ty,
                class_qual: class_qual.clone(),
            });
            *offset += I64_FIELD_SIZE;
        }
    }
    Ok(())
}

/// §5.6.7: unmatched virtual value quantities occupy object fields (default 0).
fn collect_virtual_value_fields(
    class_name: &str,
    virtual_part: &[crate::ast::VirtualSpec],
    fields: &mut Vec<FieldLayout>,
    offset: &mut i64,
    constructor_params: &[(String, FieldType)],
) -> Result<(), CompileError> {
    for spec in virtual_part {
        let (field_ty, class_qual) = match &spec.specifier {
            Specifier::Type(Type::Integer { .. } | Type::Character) => (FieldType::I64, None),
            Specifier::Type(Type::Boolean) => (FieldType::Bool, None),
            Specifier::Type(Type::Real { .. }) => (FieldType::F64, None),
            Specifier::Type(Type::Text) => (FieldType::Text, None),
            Specifier::Type(Type::ObjectRef(qual)) => (FieldType::ObjectRef, Some(qual.clone())),
            Specifier::Type(other) => {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: class '{class_name}' has unsupported virtual attribute type '{other}'; only integer, boolean, character, real, text, and object-reference virtuals are supported"
                )));
            }
            Specifier::TypeProcedure(Type::Integer { .. } | Type::Character) => {
                (FieldType::I64, None)
            }
            Specifier::TypeProcedure(Type::Boolean) => (FieldType::Bool, None),
            Specifier::TypeProcedure(Type::Real { .. }) => (FieldType::F64, None),
            Specifier::TypeProcedure(Type::Text) => (FieldType::Text, None),
            Specifier::TypeProcedure(Type::ObjectRef(qual)) => {
                (FieldType::ObjectRef, Some(qual.clone()))
            }
            Specifier::TypeProcedure(other) => {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: class '{class_name}' has unsupported virtual procedure value type '{other}'"
                )));
            }
            Specifier::TypeArray(_) | Specifier::Array => (FieldType::ArrayI64, None),
            Specifier::Label | Specifier::Switch | Specifier::Procedure => continue,
        };
        for name in &spec.names {
            if is_fictitious_detach(name) {
                continue;
            }
            if constructor_params
                .iter()
                .any(|(param, _)| param.eq_ignore_ascii_case(name))
            {
                continue;
            }
            if fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            fields.push(FieldLayout {
                name: name.clone(),
                offset: *offset,
                size: I64_FIELD_SIZE,
                ty: field_ty,
                class_qual: class_qual.clone(),
            });
            *offset += I64_FIELD_SIZE;
        }
    }
    Ok(())
}

fn collect_scalar_fields(
    class_name: &str,
    body: &Block,
    fields: &mut Vec<FieldLayout>,
    offset: &mut i64,
    constructor_params: &[(String, FieldType)],
) -> Result<(), CompileError> {
    collect_scalar_fields_from_block(class_name, body, fields, offset, constructor_params)?;
    for statement in &body.statements {
        collect_scalar_fields_from_statement(
            class_name,
            statement,
            fields,
            offset,
            constructor_params,
        )?;
    }
    Ok(())
}

fn collect_scalar_fields_from_block(
    class_name: &str,
    body: &Block,
    fields: &mut Vec<FieldLayout>,
    offset: &mut i64,
    constructor_params: &[(String, FieldType)],
) -> Result<(), CompileError> {
    for decl in &body.declarations {
        let (field_ty, class_qual) = match &decl.ty {
            Type::Integer { .. } | Type::Character => (FieldType::I64, None),
            Type::Boolean => (FieldType::Bool, None),
            Type::Real { .. } => (FieldType::F64, None),
            Type::Text => (FieldType::Text, None),
            Type::ObjectRef(qual) => (FieldType::ObjectRef, Some(qual.clone())),
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: class '{class_name}' has unsupported attribute type '{other}'; only integer, boolean, character, real, text, and object-reference attributes are supported"
                )));
            }
        };
        for item in &decl.items {
            if constructor_params
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&item.name))
            {
                // Formal parameters are already attributes; skip body re-declarations.
                continue;
            }
            if fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(&item.name))
            {
                // Same name in another nested compound — share the slot.
                continue;
            }
            fields.push(FieldLayout {
                name: item.name.clone(),
                offset: *offset,
                size: I64_FIELD_SIZE,
                ty: field_ty,
                class_qual: class_qual.clone(),
            });
            *offset += I64_FIELD_SIZE;
        }
    }
    for nested in &body.body {
        collect_scalar_fields_from_block(class_name, nested, fields, offset, constructor_params)?;
        for statement in &nested.statements {
            collect_scalar_fields_from_statement(
                class_name,
                statement,
                fields,
                offset,
                constructor_params,
            )?;
        }
    }
    Ok(())
}

/// Nested `begin` compounds inside resumable class bodies (after `detach`)
/// declare locals like `ref(C) Y` that must survive re-entry — promote them
/// to object fields (simtst76 part2).
fn collect_scalar_fields_from_statement(
    class_name: &str,
    statement: &Statement,
    fields: &mut Vec<FieldLayout>,
    offset: &mut i64,
    constructor_params: &[(String, FieldType)],
) -> Result<(), CompileError> {
    match &statement.kind {
        StatementKind::Labeled { statement, .. } => collect_scalar_fields_from_statement(
            class_name,
            statement,
            fields,
            offset,
            constructor_params,
        ),
        StatementKind::Compound(block) => {
            collect_scalar_fields_from_block(
                class_name,
                block,
                fields,
                offset,
                constructor_params,
            )?;
            for statement in &block.statements {
                collect_scalar_fields_from_statement(
                    class_name,
                    statement,
                    fields,
                    offset,
                    constructor_params,
                )?;
            }
            Ok(())
        }
        StatementKind::If(if_stmt) => {
            collect_scalar_fields_from_statement(
                class_name,
                &if_stmt.then_branch,
                fields,
                offset,
                constructor_params,
            )?;
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_scalar_fields_from_statement(
                    class_name,
                    else_branch,
                    fields,
                    offset,
                    constructor_params,
                )?;
            }
            Ok(())
        }
        StatementKind::While(while_stmt) => collect_scalar_fields_from_statement(
            class_name,
            &while_stmt.body,
            fields,
            offset,
            constructor_params,
        ),
        StatementKind::For(for_stmt) => collect_scalar_fields_from_statement(
            class_name,
            &for_stmt.body,
            fields,
            offset,
            constructor_params,
        ),
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_scalar_fields_from_statement(
                    class_name,
                    &when.body,
                    fields,
                    offset,
                    constructor_params,
                )?;
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_scalar_fields_from_statement(
                    class_name,
                    do_clause,
                    fields,
                    offset,
                    constructor_params,
                )?;
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_scalar_fields_from_statement(
                    class_name,
                    otherwise,
                    fields,
                    offset,
                    constructor_params,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn collect_array_fields(
    class_name: &str,
    body: &Block,
    fields: &mut Vec<FieldLayout>,
    offset: &mut i64,
    constructor_params: &[(String, FieldType)],
) -> Result<(), CompileError> {
    for array in &body.arrays {
        let field_ty = match &array.element_type {
            Type::Integer { .. } | Type::Character => FieldType::ArrayI64,
            Type::Boolean => FieldType::ArrayBool,
            Type::ObjectRef(_) => FieldType::ArrayI64,
            Type::Real { .. } => FieldType::ArrayF64,
            Type::Text => FieldType::ArrayText,
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: class '{class_name}' has unsupported array attribute element type '{other}'; only integer, boolean, character, real, text, and object-reference arrays are supported"
                )));
            }
        };
        let class_qual = match &array.element_type {
            Type::ObjectRef(qual) => Some(qual.clone()),
            _ => None,
        };
        for segment in &array.segments {
            for name in &segment.names {
                if constructor_params
                    .iter()
                    .any(|(param, _)| param.eq_ignore_ascii_case(name))
                {
                    continue;
                }
                if fields
                    .iter()
                    .any(|field| field.name.eq_ignore_ascii_case(name))
                {
                    return Err(CompileError::codegen(format!(
                        "MIR lowering: duplicate attribute '{name}' in class '{class_name}'"
                    )));
                }
                fields.push(FieldLayout {
                    name: name.clone(),
                    offset: *offset,
                    size: I64_FIELD_SIZE,
                    ty: field_ty,
                    class_qual: class_qual.clone(),
                });
                *offset += I64_FIELD_SIZE;
            }
        }
    }
    for nested in &body.body {
        collect_array_fields(class_name, nested, fields, offset, constructor_params)?;
    }
    Ok(())
}

/// Overwrite heuristic capture hints with types declared in `block`.
fn apply_declared_enclosing_types(block: &Block, hints: &mut HashMap<String, FieldType>) {
    let mut declared = HashMap::new();
    collect_declared_enclosing_types(block, &mut declared);
    for (name, ty) in declared {
        refine_capture_hint_type(hints, &name, ty);
    }
}

fn collect_declared_enclosing_types(block: &Block, out: &mut HashMap<String, FieldType>) {
    for decl in &block.declarations {
        let field_ty = match &decl.ty {
            Type::Integer { .. } | Type::Character => FieldType::I64,
            Type::Boolean => FieldType::Bool,
            Type::Real { .. } => FieldType::F64,
            Type::Text => FieldType::Text,
            Type::ObjectRef(_) => FieldType::ObjectRef,
            _ => continue,
        };
        for item in &decl.items {
            out.insert(item.name.clone(), field_ty);
        }
    }
    for array in &block.arrays {
        let field_ty = match &array.element_type {
            Type::Integer { .. } | Type::Character | Type::ObjectRef(_) => FieldType::ArrayI64,
            Type::Boolean => FieldType::ArrayBool,
            Type::Real { .. } => FieldType::ArrayF64,
            Type::Text => FieldType::ArrayText,
            _ => continue,
        };
        for segment in &array.segments {
            for name in &segment.names {
                out.insert(name.clone(), field_ty);
            }
        }
    }
}

/// Update the type of an existing capture hint; do not introduce new names.
/// Block declarations refine heuristics for free names already observed (in
/// sibling procedures or class bodies) — they must not snapshot every local
/// onto every nested class (that stomped Simulation `for` indices via writeback).
fn refine_capture_hint_type(hints: &mut HashMap<String, FieldType>, name: &str, ty: FieldType) {
    if let Some((_, slot)) = hints
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        *slot = ty;
    }
}

fn collect_classes_with_sibling_captures(
    block: &Block,
    out: &mut Vec<ClassDeclaration>,
    sibling_captures: &mut HashMap<String, SiblingCaptureInfo>,
    enclosing_class: Option<&str>,
) {
    let mut sibling = HashMap::new();
    for procedure in &block.procedures {
        collect_capture_hints_from_procedure(procedure, &mut sibling);
    }
    // Declared types in this block win over usage heuristics (e.g. `ia(1) = 2`
    // must not snapshot an `integer array ia` as ArrayText via a sibling
    // procedure that also mentions a text name).
    apply_declared_enclosing_types(block, &mut sibling);
    let mut declared_types = HashMap::new();
    collect_declared_enclosing_types(block, &mut declared_types);
    let mut ref_quals = HashMap::new();
    for decl in &block.declarations {
        if let Type::ObjectRef(qual) = &decl.ty {
            for item in &decl.items {
                ref_quals.insert(item.name.to_ascii_lowercase(), qual.clone());
            }
        }
    }
    // `ref(C) array pa` — element qualification for enclosing capture fields.
    for array in &block.arrays {
        if let Type::ObjectRef(qual) = &array.element_type {
            for segment in &array.segments {
                for name in &segment.names {
                    ref_quals.insert(name.to_ascii_lowercase(), qual.clone());
                }
            }
        }
    }
    for class in &block.classes {
        out.push(class.clone());
        let entry = sibling_captures.entry(class.name.clone()).or_default();
        for (name, ty) in &sibling {
            merge_capture_hint(&mut entry.captures, name, *ty);
        }
        for (name, ty) in &declared_types {
            entry
                .declared_types
                .entry(name.clone())
                .and_modify(|existing| *existing = *ty)
                .or_insert(*ty);
        }
        for (name, qual) in &ref_quals {
            entry
                .ref_quals
                .entry(name.clone())
                .or_insert_with(|| qual.clone());
        }
        if let Some(outer) = enclosing_class {
            if entry.enclosing_class.is_none() {
                entry.enclosing_class = Some(outer.to_string());
            }
        }
        // A class's own body may declare further nested classes (e.g. `Point`
        // nested inside `Geometry`); these must also be collected so they can
        // be used as prefixes elsewhere (e.g. by a `Geometry`-prefixed block).
        collect_classes_with_sibling_captures(
            &class.body,
            out,
            sibling_captures,
            Some(&class.name),
        );
    }
    for procedure in &block.procedures {
        // Local classes nested inside procedures (simtst69 `Class C4` in `P1`,
        // simtst62 `Class C` in formal-proc method `B`).
        let mut nested_formals = HashSet::new();
        for param in &procedure.parameters {
            if param.is_procedure {
                nested_formals.insert(param.name.to_ascii_lowercase());
            }
        }
        // Propagate outer formal-proc names too (nested procedures).
        // Classes collected under this procedure get the union via a temporary
        // sibling map entry merge after collection — inject into a scoped set
        // by recording on each nested class entry below.
        let before = out.len();
        collect_classes_with_sibling_captures(
            &procedure.body,
            out,
            sibling_captures,
            enclosing_class,
        );
        if !nested_formals.is_empty() {
            for class in &out[before..] {
                let entry = sibling_captures.entry(class.name.clone()).or_default();
                entry
                    .formal_proc_params
                    .extend(nested_formals.iter().cloned());
            }
        }
    }
    for nested in &block.body {
        collect_classes_with_sibling_captures(nested, out, sibling_captures, enclosing_class);
    }
    for statement in &block.statements {
        collect_classes_from_statement(statement, out, sibling_captures, enclosing_class);
    }
}

fn collect_classes_from_statement(
    statement: &Statement,
    out: &mut Vec<ClassDeclaration>,
    sibling_captures: &mut HashMap<String, SiblingCaptureInfo>,
    enclosing_class: Option<&str>,
) {
    match &statement.kind {
        StatementKind::Compound(block) => {
            collect_classes_with_sibling_captures(block, out, sibling_captures, enclosing_class)
        }
        StatementKind::If(if_stmt) => {
            collect_classes_from_statement(
                &if_stmt.then_branch,
                out,
                sibling_captures,
                enclosing_class,
            );
            if let Some(else_branch) = &if_stmt.else_branch {
                collect_classes_from_statement(else_branch, out, sibling_captures, enclosing_class);
            }
        }
        StatementKind::While(while_stmt) => {
            collect_classes_from_statement(&while_stmt.body, out, sibling_captures, enclosing_class)
        }
        StatementKind::For(for_stmt) => {
            collect_classes_from_statement(&for_stmt.body, out, sibling_captures, enclosing_class)
        }
        StatementKind::Labeled { statement, .. } => {
            collect_classes_from_statement(statement, out, sibling_captures, enclosing_class)
        }
        StatementKind::Inspect(inspect) => {
            for when in &inspect.when_clauses {
                collect_classes_from_statement(&when.body, out, sibling_captures, enclosing_class);
            }
            if let Some(do_clause) = &inspect.do_clause {
                collect_classes_from_statement(do_clause, out, sibling_captures, enclosing_class);
            }
            if let Some(otherwise) = &inspect.otherwise {
                collect_classes_from_statement(otherwise, out, sibling_captures, enclosing_class);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::test_support::parse_program;

    #[test]
    fn program_uses_simulation_scheduling_inspect_do_clause() {
        let program = parse_program(
            r#"begin
                inspect new InFile(blanks(1)) do
                simulation begin
                    process class car;
                    begin wait(start); end;
                    ref(head) start;
                end;
            end;"#,
        );
        assert!(
            program_uses_simulation_scheduling(&program),
            "inspect do simulation must enable SQS scheduling"
        );
    }

    #[test]
    fn program_uses_simulation_scheduling_false_for_simset_only() {
        let program = parse_program(
            r#"begin
                simset begin
                    link class town;
                    begin detach; end;
                end;
            end;"#,
        );
        assert!(
            !program_uses_simulation_scheduling(&program),
            "plain SIMSET must not enable SQS scheduling"
        );
    }

    /// A component keeps its continuation on its own stack, so being one costs
    /// the object no field of its own.
    #[test]
    fn layouts_detach_class_is_a_component_and_carries_no_extra_field() {
        let program = parse_program(
            r#"begin
                class Worker; begin
                    OutText("A"); OutImage;
                    detach;
                    OutText("B"); OutImage;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let worker = layouts.get("Worker").expect("Worker layout");
        assert!(worker.runs_on_own_stack);
        assert_eq!(worker.size, OBJECT_HEADER_SIZE);
    }

    #[test]
    fn layouts_capture_enclosing_object_ref_used_in_wait() {
        let program = parse_program(
            r#"Simulation begin
                ref(head) q;
                process class Worker; begin
                    wait(q);
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let worker = layouts
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("Worker"))
            .map(|(_, layout)| layout)
            .expect("Worker layout");
        assert_eq!(
            worker.enclosing_captures,
            vec![("q".to_string(), FieldType::ObjectRef)]
        );
        assert!(worker.field_offset("q").is_some());
        assert_eq!(worker.field_type("q"), Some(FieldType::ObjectRef));
    }

    #[test]
    fn layouts_capture_enclosing_integer_and_text() {
        let program = parse_program(
            r#"begin
                integer n;
                text t;
                class Worker; begin
                    OutInt(n, 0); OutImage;
                    OutText(t); OutImage;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let worker = layouts.get("Worker").expect("Worker layout");
        assert_eq!(
            worker.enclosing_captures,
            vec![
                ("n".to_string(), FieldType::I64),
                ("t".to_string(), FieldType::Text),
            ]
        );
        assert_eq!(worker.field_type("n"), Some(FieldType::I64));
        assert_eq!(worker.field_type("t"), Some(FieldType::Text));
    }

    #[test]
    fn layouts_simple_integer_class() {
        let program = parse_program(
            r#"begin
                class Point; begin integer x, y; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let point = layouts.get("Point").expect("Point layout");
        assert_eq!(point.class_id, 0);
        assert_eq!(point.size, OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE);
        assert_eq!(point.field_offset("x"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(
            point.field_offset("y"),
            Some(OBJECT_HEADER_SIZE + I64_FIELD_SIZE)
        );
        assert!(point.methods.is_empty());
    }

    #[test]
    fn layouts_class_with_methods() {
        let program = parse_program(
            r#"begin
                class Counter; begin
                    integer n;
                    procedure increment; begin n := n + 1; end;
                    integer procedure get; begin get := n; end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let counter = layouts.get("Counter").expect("Counter layout");
        assert_eq!(
            counter.methods,
            vec!["increment".to_string(), "get".to_string()]
        );
        assert_eq!(
            mangle_method_name(&counter.name, "increment"),
            "Counter$increment"
        );
    }

    #[test]
    fn layouts_boolean_attribute() {
        let program = parse_program(
            r#"begin
                class C; begin boolean flag; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let class = layouts.get("C").expect("C layout");
        assert_eq!(class.size, OBJECT_HEADER_SIZE + I64_FIELD_SIZE);
        assert_eq!(class.field_offset("flag"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(class.field_type("flag"), Some(FieldType::Bool));
    }

    #[test]
    fn layouts_prefix_point_polar() {
        let program = parse_program(
            r#"begin
                class Point; begin integer x, y; end;
                Point class Polar; begin integer r; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let point = layouts.get("Point").expect("Point layout");
        assert_eq!(point.size, OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE);
        assert_eq!(point.field_offset("x"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(
            point.field_offset("y"),
            Some(OBJECT_HEADER_SIZE + I64_FIELD_SIZE)
        );

        let polar = layouts.get("Polar").expect("Polar layout");
        assert_eq!(polar.size, OBJECT_HEADER_SIZE + 3 * I64_FIELD_SIZE);
        assert_eq!(polar.field_offset("x"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(
            polar.field_offset("y"),
            Some(OBJECT_HEADER_SIZE + I64_FIELD_SIZE)
        );
        assert_eq!(
            polar.field_offset("r"),
            Some(OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE)
        );
        assert_eq!(polar.field_type("x"), Some(FieldType::I64));
        assert_eq!(polar.field_type("r"), Some(FieldType::I64));
    }

    #[test]
    fn layouts_prefix_family_shares_enclosing_capture_offsets() {
        // Prefixed methods are outlined against the declaring class, but `__this`
        // may be a subclass instance (simtst98: `a$outa` called from `new z`).
        let program = parse_program(
            r#"begin
                boolean trace;
                integer k;
                class a;
                begin
                    integer i;
                    procedure outa; begin if trace then k := k; end;
                    detach;
                end;
                a class b;
                begin integer j; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("a").expect("a layout");
        let b = layouts.get("b").expect("b layout");
        assert!(
            !a.enclosing_captures.is_empty(),
            "a should capture enclosing names"
        );
        for (name, _) in &a.enclosing_captures {
            let a_off = a.field_offset(name);
            let b_off = b.field_offset(name);
            assert_eq!(
                a_off, b_off,
                "capture '{name}' must share an offset across the prefix family (a={a_off:?}, b={b_off:?})"
            );
        }
        assert!(
            a.size <= b.size,
            "subclass object must be at least as large as the aligned prefix"
        );
    }

    #[test]
    fn layouts_constructor_params_before_body_fields() {
        let program = parse_program(
            r#"begin
                class Point(x); integer x; begin integer y; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let point = layouts.get("Point").expect("Point layout");
        assert_eq!(
            point.constructor_params,
            vec![("x".to_string(), FieldType::I64)]
        );
        assert!(point.needs_init);
        assert_eq!(point.field_offset("x"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(
            point.field_offset("y"),
            Some(OBJECT_HEADER_SIZE + I64_FIELD_SIZE)
        );
        assert_eq!(point.size, OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE);
    }

    #[test]
    fn layouts_constructor_object_ref_keeps_class_qual() {
        let program = parse_program(
            r#"begin
                class Les(n, andre); integer n; ref(Les) andre;
                begin integer lnr; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let les = layouts.get("Les").expect("Les layout");
        let andre = les
            .fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case("andre"))
            .expect("andre field");
        assert_eq!(andre.ty, FieldType::ObjectRef);
        assert_eq!(andre.class_qual.as_deref(), Some("Les"));
    }

    #[test]
    fn layouts_shadowed_enclosing_capture_uses_mangled_field() {
        let program = parse_program(
            r#"begin
                integer i;
                procedure P(Q); procedure Q; begin Q(i); end;
                class A;
                begin
                    procedure R(k); name k; integer k; begin k := k + k; end;
                    integer i;
                    P(R);
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("A").expect("A layout");
        assert!(
            a.enclosing_captures
                .iter()
                .any(|(name, _)| name.starts_with("__simrt_encl_")),
            "expected mangled enclosing capture for shadowed i: {:?}",
            a.enclosing_captures
        );
    }

    #[test]
    fn layouts_resume_peer_shares_forward_ref_capture() {
        let program = parse_program(
            r#"begin
                ref(Y) yy; ref(X) xx;
                class Y; begin
                    detach; resume(xx);
                end;
                class X; begin
                    detach; resume(yy);
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let x = layouts.get("X").expect("X");
        eprintln!("X captures: {:?}", x.enclosing_captures);
        assert!(
            x.enclosing_captures
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("xx")),
            "X must capture xx for peer Y: {:?}",
            x.enclosing_captures
        );
        let y = layouts.get("Y").expect("Y");
        eprintln!("Y captures: {:?}", y.enclosing_captures);
        assert!(
            y.enclosing_captures
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("xx")),
            "Y must capture xx: {:?}",
            y.enclosing_captures
        );
    }

    /// A component's stack keeps every frame of the body live across a transfer,
    /// so where the `detach` sits does not matter. The statement-index splitter
    /// this replaced could only suspend at a fixed set of top-level shapes and
    /// had to treat a class like this one as non-resumable.
    #[test]
    fn a_detach_nested_in_loops_still_makes_a_component() {
        let program = parse_program(
            r#"begin
                class Worker;
                begin
                    integer i;
                    while i < 3 do
                    begin
                        for i := 1 step 1 until 3 do detach;
                    end;
                end;
                ref(Worker) w;
                w :- new Worker;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        assert!(
            layouts
                .get("Worker")
                .expect("Worker layout")
                .runs_on_own_stack
        );
    }

    /// A reference qualified by a prefix can hold any subclass, so a peer named
    /// through `ref(Coroutine)` contributes the *subclasses*' free names
    /// (simtst88: `resume(c)` where `c` is a `ref(Coroutine)` holding a Changer).
    #[test]
    fn layouts_resume_peer_through_prefix_qualified_reference_covers_subclasses() {
        let program = parse_program(
            r#"begin
                character c1;
                ref(Coroutine) r, w;
                class Coroutine; detach;
                Coroutine class Reader;
                while true do begin c1 := inchar; resume(w) end;
                Coroutine class Writer;
                while true do begin outchar(c1); resume(r) end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let reader = layouts.get("Reader").expect("Reader");
        assert!(
            reader
                .enclosing_captures
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("r")),
            "Reader resumes a Writer, which needs `r`: {:?}",
            reader.enclosing_captures
        );
    }

    /// A class generated from inside another object is created in *that* object's
    /// frame, so the generator has to carry the names the generated class needs
    /// (simtst69: `C2` generates `C5`, which reads the outer block's `rC2`).
    #[test]
    fn layouts_generator_carries_the_generated_classes_captures() {
        let program = parse_program(
            r#"begin
                ref(C2) rC2;
                class C2; begin ref(C5) rC5;
                    detach; rC5 :- new C5; call(rC5);
                end;
                class C5; begin
                    detach; inspect rC2 do detach;
                end;
                rC2 :- new C2; call(rC2);
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c2 = layouts.get("C2").expect("C2");
        assert!(
            c2.enclosing_captures
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("rC2")),
            "C2 generates C5, so it must carry rC2 for it: {:?}",
            c2.enclosing_captures
        );
    }

    #[test]
    fn layouts_t76p2_a_has_no_y_capture_and_pc_offsets() {
        let program = parse_program(
            r#"begin
                class A;
                begin
                    outtext("A"); detach;
                    begin ref(C) y;
                        class C;
                        begin outtext("D"); detach; outtext("F"); this A.detach; outtext("H"); detach; outtext("J"); end;
                        outtext("C"); y :- new C;
                        outtext("E"); resume(y);
                        outtext("I"); resume(y);
                        detach;
                    end;
                    outtext("L");
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("A").expect("A");
        eprintln!(
            "A fields: {:?}",
            a.fields
                .iter()
                .map(|f| (f.offset, &f.name))
                .collect::<Vec<_>>()
        );
        eprintln!("A captures: {:?}", a.enclosing_captures);
        assert!(
            !a.enclosing_captures
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("y")),
            "block-local y must not be an A enclosing capture: {:?}",
            a.enclosing_captures
        );
        assert!(
            a.field_offset("y").is_some(),
            "nested begin ref(C) y must be promoted to an A field"
        );
        assert!(a.runs_on_own_stack);
    }

    #[test]
    fn layouts_capture_free_names_of_procedures_called_from_array_bounds() {
        // simtst74: `Real array X(P:1)` runs `P` on object creation, so `P`'s
        // free names become captures of the class. `X` shadows the outer `x`
        // (Simula is case-insensitive), so the capture is mangled.
        let program = parse_program(
            r#"begin
                integer r;
                ref(A) x;
                class A;
                begin
                    real array X(Q:1);
                    detach;
                end;
                integer procedure Q;
                begin if x =/= none then r := 1; Q := 1 end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("A").expect("A");
        assert!(
            a.enclosing_captures
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&enclosing_capture_field_name("x"))),
            "outer `x` used by the bound procedure must be captured (shadowed by array `X`): {:?}",
            a.enclosing_captures
        );
        assert!(
            a.enclosing_captures
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("r")),
            "outer `r` used by the bound procedure must be captured: {:?}",
            a.enclosing_captures
        );
        assert!(
            a.field_offset("X").is_some(),
            "the array attribute itself still has its own field"
        );
    }

    #[test]
    fn layouts_simtst69_c2_promotes_nested_rc5_not_capture() {
        let program = parse_program(
            r#"begin
                class A;;
                A begin
                    class C2;
                    begin
                        ref(C5) rC5;
                        detach;
                    end;
                    class C5; begin end;
                    ref(C1) rC1;
                    ref(C2) rC2;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c2 = layouts
            .values()
            .find(|l| l.declared_name.eq_ignore_ascii_case("C2"))
            .expect("C2");
        eprintln!(
            "C2 fields: {:?}",
            c2.fields
                .iter()
                .map(|f| (f.offset, &f.name))
                .collect::<Vec<_>>()
        );
        eprintln!("C2 captures: {:?}", c2.enclosing_captures);
        assert!(c2.field_offset("rC5").is_some());
        assert!(
            !c2.enclosing_captures
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("rC5")),
            "rC5 must not be an enclosing capture"
        );
    }

    #[test]
    fn layouts_compound_locals_in_methods_are_not_enclosing_captures() {
        // simtst62: `ref(F) ff` lives in a nested begin inside method E — it
        // must not become an enclosing capture on X/C (Resume writeback would
        // then clobber the real `ff` to none).
        let program = parse_program(
            r#"begin
                text array seq(1:20); integer seqi;
                procedure trace(t); value t; text t;
                begin seqi := seqi + 1; seq(seqi) :- t; end;
                class X; begin
                    procedure B(E); procedure E; begin
                        real pi;
                        begin
                            ref(C) cc;
                            class C; begin
                                detach; E;
                            end;
                            cc :- new C;
                            resume(cc);
                        end;
                    end;
                    procedure E; begin
                        begin
                            ref(F) ff;
                            class F; begin detach; end;
                            ff :- new F;
                            resume(ff);
                        end;
                    end;
                    detach; B(E);
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let x = layouts.get("X").expect("X layout");
        assert!(
            !x.enclosing_captures
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("ff") || n.eq_ignore_ascii_case("cc")),
            "X must not capture compound locals ff/cc: {:?}",
            x.enclosing_captures
        );
        let c = layouts
            .values()
            .find(|l| l.declared_name.eq_ignore_ascii_case("C"))
            .expect("C layout");
        assert!(
            !c.enclosing_captures
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("ff") || n.eq_ignore_ascii_case("cc")),
            "C must not capture compound locals ff/cc: {:?}",
            c.enclosing_captures
        );
        assert!(
            c.enclosing_captures
                .iter()
                .any(|(n, _)| n.starts_with("__simrt_fp_")),
            "C should still snapshot formal-proc E: {:?}",
            c.enclosing_captures
        );
    }

    #[test]
    fn layouts_prefix_with_constructor_params() {
        let program = parse_program(
            r#"begin
                class Point(x); integer x; begin end;
                Point class Polar(r); integer r; begin end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let polar = layouts.get("Polar").expect("Polar layout");
        assert_eq!(
            polar.constructor_params,
            vec![
                ("x".to_string(), FieldType::I64),
                ("r".to_string(), FieldType::I64),
            ]
        );
        assert!(polar.needs_init);
        assert_eq!(polar.field_offset("x"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(
            polar.field_offset("r"),
            Some(OBJECT_HEADER_SIZE + I64_FIELD_SIZE)
        );
        assert_eq!(polar.size, OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE);
    }

    #[test]
    fn layouts_text_constructor_param() {
        let program = parse_program(
            r#"begin
                class C(t); value t; text t; begin end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let class = layouts.get("C").expect("C layout");
        assert_eq!(
            class.constructor_params,
            vec![("t".to_string(), FieldType::Text)]
        );
    }

    #[test]
    fn layouts_character_constructor_param() {
        let program = parse_program(
            r#"begin
                class C(ch); character ch; begin end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let class = layouts.get("C").expect("C layout");
        assert_eq!(
            class.constructor_params,
            vec![("ch".to_string(), FieldType::I64)]
        );
        assert_eq!(class.field_type("ch"), Some(FieldType::I64));
    }

    #[test]
    fn layouts_array_constructor_param() {
        let program = parse_program(
            r#"begin
                class A; begin end;
                class Q(ra1); ref(A) array ra1; begin end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let class = layouts.get("Q").expect("Q layout");
        assert_eq!(
            class.constructor_params,
            vec![("ra1".to_string(), FieldType::ArrayI64)]
        );
        assert_eq!(class.field_type("ra1"), Some(FieldType::ArrayI64));
        assert_eq!(
            class
                .fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case("ra1"))
                .and_then(|f| f.class_qual.as_deref()),
            Some("A")
        );
    }

    #[test]
    fn layouts_boolean_array_enclosing_capture() {
        // Usage in `if active(i)` is Untyped/Bool; declared type must win as
        // ArrayBool so MIR can treat `active(i)` as a subscript on `__this`.
        let program = parse_program(
            r#"begin
                boolean array active(1:3);
                procedure outstate; begin
                    integer i;
                    if active(1) then OutText("a");
                end;
                class P;
                begin
                    outstate;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let p = layouts.get("P").expect("P layout");
        assert!(
            p.enclosing_captures.iter().any(
                |(name, ty)| name.eq_ignore_ascii_case("active") && *ty == FieldType::ArrayBool
            ),
            "expected boolean array enclosing capture: {:?}",
            p.enclosing_captures
        );
    }

    #[test]
    fn layouts_ref_array_enclosing_capture_keeps_class_qual() {
        let program = parse_program(
            r#"begin
                class Node; begin end;
                ref(Node) array pa(1:2);
                procedure show; begin
                    if pa(1)==none then OutText("n");
                end;
                class P;
                begin
                    show;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let p = layouts.get("P").expect("P layout");
        let field = p
            .fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case("pa"))
            .expect("pa capture field");
        assert_eq!(field.ty, FieldType::ArrayI64);
        assert_eq!(field.class_qual.as_deref(), Some("Node"));
    }

    #[test]
    fn layouts_untyped_assignment_rhs_captures_simple_name() {
        // `x := U2` used Untyped and previously skipped the RHS identifier.
        let program = parse_program(
            r#"begin
                integer U2;
                class P;
                begin
                    integer x;
                    x := U2;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let p = layouts.get("P").expect("P layout");
        assert!(
            p.enclosing_captures
                .iter()
                .any(|(name, ty)| name.eq_ignore_ascii_case("U2") && *ty == FieldType::I64),
            "expected U2 enclosing capture: {:?}",
            p.enclosing_captures
        );
    }

    #[test]
    fn layouts_text_attribute() {
        let program = parse_program(
            r#"begin
                class C; begin text t; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let class = layouts.get("C").expect("C layout");
        assert_eq!(class.size, OBJECT_HEADER_SIZE + I64_FIELD_SIZE);
        assert_eq!(class.field_offset("t"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(class.field_type("t"), Some(FieldType::Text));
    }

    #[test]
    fn layouts_real_attribute() {
        let program = parse_program(
            r#"begin
                class C; begin real x; end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let class = layouts.get("C").expect("C layout");
        assert_eq!(class.size, OBJECT_HEADER_SIZE + I64_FIELD_SIZE);
        assert_eq!(class.field_offset("x"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(class.field_type("x"), Some(FieldType::F64));
    }

    #[test]
    fn layouts_virtual_methods() {
        let program = parse_program(
            r#"begin
                class C; virtual: procedure p; begin
                    integer x;
                    procedure p; begin end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert!(c.is_virtual_method("p"));
        assert_eq!(c.methods, vec!["p".to_string()]);
    }

    #[test]
    fn accepts_boolean_result_method() {
        let program = parse_program(
            r#"begin
                class C; begin
                    boolean procedure p; begin p := true; end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert_eq!(c.methods, vec!["p".to_string()]);
    }

    #[test]
    fn accepts_real_and_text_result_methods() {
        let program = parse_program(
            r#"begin
                class C; begin
                    real procedure r; begin r := 1.5; end;
                    text procedure t; begin t :- "hi"; end;
                    ref (C) procedure self; begin self :- this C; end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert_eq!(
            c.methods,
            vec!["r".to_string(), "t".to_string(), "self".to_string()]
        );
    }

    #[test]
    fn accepts_class_array_attributes() {
        let program = parse_program(
            r#"begin
                class C; begin
                    integer array ia(0:2);
                    boolean array ba(0:1);
                    real array ra(1:3);
                    text array ta(0:0);
                    ref (C) array oa(0:1);
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert!(c.needs_init, "array attrs require __init allocation");
        assert_eq!(c.field_type("ia"), Some(FieldType::ArrayI64));
        assert_eq!(c.field_type("ba"), Some(FieldType::ArrayBool));
        assert_eq!(c.field_type("ra"), Some(FieldType::ArrayF64));
        assert_eq!(c.field_type("ta"), Some(FieldType::ArrayText));
        assert_eq!(c.field_type("oa"), Some(FieldType::ArrayI64));
    }

    #[test]
    fn rejects_array_method_param() {
        let program = parse_program(
            r#"begin
                class C; begin
                    procedure p (a); array a; begin end;
                end;
            end;"#,
        );
        let error = layouts_for_program(&program).unwrap_err();
        let hay = format!("{} {}", error.message, error.notes.join(" ")).to_ascii_lowercase();
        assert!(
            hay.contains("array") || hay.contains("reference") || hay.contains("not lowered"),
            "message was: {} notes={:?}",
            error.message,
            error.notes
        );
    }

    #[test]
    fn accepts_text_and_object_ref_value_method_params() {
        let program = parse_program(
            r#"begin
                class C; begin
                    procedure show (t); value t; text t; begin outtext(t); end;
                    procedure link (r); value r; ref (C) r; begin end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert_eq!(c.methods, vec!["show".to_string(), "link".to_string()]);
    }

    #[test]
    fn accepts_name_integer_method_param() {
        // An unspecified-mode integer formal defaults to call-by-name (§5.4.2),
        // matching the free-procedure outlined name-thunk MVP.
        let program = parse_program(
            r#"begin
                class C; begin
                    procedure bump (n); integer n; begin n := n + 1; end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert_eq!(c.methods, vec!["bump".to_string()]);
    }

    #[test]
    fn accepts_name_text_method_param() {
        // Non-integer name formals on methods are allowed at layout time;
        // MIR may still inline them at call sites rather than outline thunks.
        let program = parse_program(
            r#"begin
                class C; begin
                    procedure show (t); name t; text t; begin outtext(t); end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert_eq!(c.methods, vec!["show".to_string()]);
    }

    #[test]
    fn nested_class_declaration_is_collected_not_rejected() {
        // `Inner` collides with the reserved `inner` keyword, so the nested
        // class here is named `Nested` instead.
        let program = parse_program(
            r#"begin
                class Outer; begin
                    class Nested; begin integer x; end;
                    procedure p; begin end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        assert!(layouts.contains_key("Outer"));
        assert!(layouts.contains_key("Nested"));
        let outer = layouts.get("Outer").expect("Outer layout");
        assert_eq!(outer.methods, vec!["p".to_string()]);
    }

    #[test]
    fn nested_local_class_gets_enclosing_object_field() {
        let program = parse_program(
            r#"begin
                class A; begin
                    begin
                        class C; begin detach; end;
                    end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert!(
            c.field_offset(ENCLOSING_OBJECT_FIELD_NAME).is_some(),
            "nested C should carry __simrt_enclosing: {:?}",
            c.fields.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn constructor_param_accepts_reference_text_and_object_ref() {
        let program = parse_program(
            r#"begin
                class Player(PName); text PName; begin end;
                class Cell(link); ref (Cell) link; begin end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let player = layouts.get("Player").expect("Player layout");
        assert_eq!(
            player.constructor_params,
            vec![("PName".to_string(), FieldType::Text)]
        );
        let cell = layouts.get("Cell").expect("Cell layout");
        assert_eq!(
            cell.constructor_params,
            vec![("link".to_string(), FieldType::ObjectRef)]
        );
    }

    #[test]
    fn rejects_name_constructor_param() {
        // The parser itself rejects call-by-name class parameters (illegal
        // per the Standard) before layout validation ever runs.
        let error = crate::parse::test_support::parse_program_result(
            r#"begin
                class C(n); name n; integer n; begin end;
            end;"#,
        )
        .unwrap_err();
        assert!(
            error.message.to_ascii_lowercase().contains("name"),
            "message was: {}",
            error.message
        );
    }

    #[test]
    fn layouts_capture_text_from_content_assignment() {
        let program = parse_program(
            r#"begin
                text timage; integer ti;
                class A; begin
                    procedure P1; begin
                        timage:= "hello";
                        ti:= ti + 1;
                    end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("A").expect("A layout");
        assert_eq!(
            a.field_type("timage"),
            Some(FieldType::Text),
            "captures: {:?}",
            a.enclosing_captures
        );
        assert_eq!(a.field_type("ti"), Some(FieldType::I64));
    }

    #[test]
    fn layouts_capture_text_survives_sub_assignment() {
        let program = parse_program(
            r#"begin
                text timage;
                class A; begin
                    procedure P1(t); text t; begin
                        timage:= "hello";
                        timage.sub(1, t.length):= t;
                    end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("A").expect("A layout");
        assert_eq!(
            a.field_type("timage"),
            Some(FieldType::Text),
            "captures: {:?}",
            a.enclosing_captures
        );
    }

    #[test]
    fn layouts_capture_text_array_from_sibling_via_prefixed_block_call() {
        let program = parse_program(
            r#"begin
                text array seq (1:30);
                integer seqi;
                procedure trace(t); value t; text t; begin
                    seqi := seqi + 1;
                    seq (seqi) :- t;
                end;
                class A;;
                class X; begin
                    A begin
                        trace("enter A");
                    end;
                end;
            end;"#,
        );
        // Declared text array must win over `seq(i) :- t` Untyped→ArrayI64 heuristic.
        let layouts = layouts_for_program(&program).unwrap();
        let x = layouts.get("X").expect("X");
        assert_eq!(
            x.field_type("seq"),
            Some(FieldType::ArrayText),
            "X captures: {:?}",
            x.enclosing_captures
        );
        assert_eq!(x.field_type("seqi"), Some(FieldType::I64));
    }

    #[test]
    fn layouts_declared_text_array_wins_over_untyped_subscript_refassign() {
        let program = parse_program(
            r#"begin
                text array seq (1:30);
                integer seqi;
                procedure trace(t); value t; text t; begin
                    seq (seqi) :- t;
                end;
                class X; begin
                    trace("x");
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let x = layouts.get("X").expect("X");
        assert_eq!(
            x.field_type("seq"),
            Some(FieldType::ArrayText),
            "X captures: {:?}",
            x.enclosing_captures
        );
    }

    #[test]
    fn layouts_capture_text_array_from_sibling_test() {
        let program = parse_program(
            r#"begin
                text timage; text array ta(0:3); integer ti;
                procedure Test; if timage <> ta(ti) then;
                class A; begin
                    procedure P1; begin Test; end;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let a = layouts.get("A").expect("A");
        assert_eq!(
            a.field_type("ta"),
            Some(FieldType::ArrayText),
            "{:?}",
            a.enclosing_captures
        );
        assert_eq!(
            a.field_type("timage"),
            Some(FieldType::Text),
            "{:?}",
            a.enclosing_captures
        );
    }

    #[test]
    fn layouts_shadowed_integer_with_text_procedure_fields() {
        let program = parse_program(
            r#"begin
                class A; begin
                    text procedure Tp; Tp :- COPY(" A.Tp ");
                    integer i; i := 1;
                end;
                A class B; begin
                    text procedure Tp; Tp :- COPY(" B.Tp ");
                    integer i; i := 2;
                end;
                B class C; begin
                    text procedure Tp; Tp :- COPY(" C.Tp ");
                    integer i; i := 3;
                end;
            end;"#,
        );
        let layouts = layouts_for_program(&program).unwrap();
        let c = layouts.get("C").expect("C layout");
        assert_eq!(c.field_offset("i"), Some(OBJECT_HEADER_SIZE));
        assert_eq!(
            c.field_offset("i$B"),
            Some(OBJECT_HEADER_SIZE + I64_FIELD_SIZE)
        );
        assert_eq!(
            c.field_offset("i$B$C"),
            Some(OBJECT_HEADER_SIZE + 2 * I64_FIELD_SIZE)
        );
    }
}
