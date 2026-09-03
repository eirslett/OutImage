//! Lowers the scalar / array / text / Phase-5-MVP object subset of the AST
//! into a [`super::Module`].
//!
//! Supports a single top-level `begin ... end` block, treating nested
//! `begin ... end` compounds as flattened into the same function: nested
//! declarations are hoisted into the function's
//! flat local space and nested statements are lowered in place. There is no
//! lexical shadowing yet, matching the fact that we only lower whole
//! programs that don't rely on it.
//!
//! Simple nested (local) procedures declared in the main block (Simula
//! §5.4/§4.6.5) lower to their own [`Function`]s alongside `main`: value
//! parameters and an optional integer/boolean result are supported. Each
//! function-procedure gets an implicit local named after the procedure
//! itself (matching the Simula `procedure.name`-as-result-variable
//! convention) that `f := expr;` assigns into and the
//! synthesized trailing `Op::Return` reads back.
//!
//! **Call-by-name (Jensen):** non-recursive name-param procedures are *not*
//! emitted as separate MIR functions — call sites inline the body (see
//! [`FunctionBuilder::inline_name_procedure`]). Recursive procedures whose
//! name formals are all integers are **outlined** with a three-scalar thunk
//! ABI per name formal: `(get: FuncRef, set: FuncRef, env: RefI64)`. Reads
//! lower to `call_indirect get(env)`; writes lower to
//! `call_indirect set(env, value)` (see [`Op::CallIndirect`] and the shared
//! `__simrt_name_get_ref` / `__simrt_name_set_ref` helper functions built
//! once per module in [`lower_program`]). Simple-variable actuals build the
//! triple from [`Op::FuncAddr`] (helpers) + [`Op::LocalAddr`] (the
//! variable's cell). Read-only formals accept integer expression actuals via
//! per-call-site `__simrt_name_get_expr_N` get helpers that re-evaluate the
//! expression on every read (free integer locals and name-thunk formals are
//! captured in a packed env); unsupported shapes fall back to a temp-cell
//! snapshot. Assigned formals still require a simple variable (or a
//! pass-through of an enclosing name-thunk formal), an `a(i)` L-value, or a
//! simple remote integer field `r.x` (per-offset field get/set helpers).
//!
//! **Call-by-reference:** outlined procedures may take `integer`/`text` array
//! formals in reference mode — the callee receives the descriptor pointer and
//! aliases the caller. Text / `ref(C)` call-by-reference is inlined at each
//! call site by sharing the caller's [`LocalId`] (so `:-` updates the actual),
//! including when mixed with call-by-name formals in the same procedure
//! ([`FunctionBuilder::inline_name_procedure`]). Value text formals (outlined
//! or inlined) bind the actual's text handle; assignment (`:=`) deep-copies.
//! Value arrays deep-copy descriptors via [`Op::ArrayCopy`]. Formal
//! procedure parameters are inlined like call-by-name: the formal name
//! rewrites to the actual procedure identifier at each call site (simple
//! identifier actuals only). Mixing formal procedures with text/`ref`
//! call-by-reference is still rejected. `external` procedures remain hard
//! errors — see [`build_signature`] / [`validate_name_param_procedure`].
//!
//! **Enclosing ObjectRefs:** free `ref` names used in ObjectRef-ish positions
//! inside a class body (e.g. `wait(q)`, `resume(b)`) become hidden ObjectRef
//! attributes snapshotted from the enclosing block at `new`, matching the
//! interpreter's `enclosing_locals` clone.
//!
//! Phase 5 MVP objects: flat/prefix classes with integer/boolean/real/text
//! attributes, constructor parameters (value integer/boolean/real), `ref(C)`
//! locals, `none`, `new C(...)`, `:-` for object refs, remote field load/store,
//! class-body init, and methods (value params; void or integer result; simple
//! virtual dispatch) callable as `obj.m(...)`, parameterless `obj.m`, and
//! unqualified sibling `m(...)` inside method bodies. Method bodies rewrite
//! bare field names to [`Op::FieldLoadI64`] / [`Op::FieldStoreI64`] on an
//! implicit `__this` receiver, and `qua` / `inspect` (when/do/otherwise).
//! Reference
//! relations (`=:=`) remain hard errors.
//!
//! Anything else outside the supported subset (unsupported Simulation
//! features that remain deferred) is a hard [`CompileError`]. Flat statement-level
//! `goto` / labels lower to CFG [`Op::Jump`]s. Simulation: `Simulation` blocks
//! lower to a MAIN statement-index loop over the runtime SQS with `hold` /
//! `passivate` / timed `activate` / `cancel` / `time` / `current` / `wait`
//! (SIMSET into + passivate).
//!
//! [`lower_program_lenient`] is a best-effort variant that keeps going after
//! an unsupported statement (turning it into [`Op::Nop`]) and returns every
//! error it collected instead of aborting on the first one; it's meant for
//! tooling (e.g. a future `--emit=mir` preview) that wants partial output.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::{
    ActivateStatement, ArrayDeclaration, AssignOperator, Assignment, AssignmentRhs, BinaryOp,
    Block, ClassDeclaration, DesignationalExpr, Expr, ExprKind, ExternalDeclaration,
    ForListElement, ForStatement, FormalParameter, GotoStatement, IfStatement, InspectStatement,
    ObjectGenerator, ParamMode, ProcedureCall, ProcedureDeclaration, Program, RelationOp,
    SimulationTiming, Specifier, Statement, StatementKind, UnaryOp, Variable, WhileStatement,
};
use crate::basicio::{
    self, FreeBasicioTarget, basicio_class_supports_free_method, free_basicio_target,
    is_basicio_class, is_basicio_method,
};
use crate::concatenate::{
    self, AccessLevel, AttributeKind, accessible_remote_storage_name,
    attribute_declared_in_prefix_chain, declared_procedure_name, is_fictitious_detach,
    is_subclass_of, is_virtual_quantity, prefix_chain, substitute_remote_attribute,
    virtual_match_level, visible_attribute_binding,
};
use crate::error::CompileError;
#[cfg(test)]
use crate::error::Phase;
use crate::layout::{
    self, ClassLayout, ENCLOSING_OBJECT_FIELD_NAME, FieldType, NAME_ARR1_ENV_ARRAY_OFFSET,
    NAME_ARR1_ENV_CLASS_ID, NAME_ARR1_ENV_CLASS_NAME, NAME_ARR1_ENV_INDEX_OFFSET,
    NAME_ARR1_ENV_SIZE, NAME_INT_ENV_ADDR_OFFSET, NAME_INT_ENV_CLASS_ID, NAME_INT_ENV_CLASS_NAME,
    NAME_INT_ENV_SIZE, NAME_PACK_ENV_CLASS_ID, NAME_PACK_ENV_CLASS_NAME, NAME_PACK_ENV_SLOT_COUNT,
    NAME_THUNK_PAIR_CLASS_ID, NAME_THUNK_PAIR_CLASS_NAME, NAME_THUNK_PAIR_ENV_OFFSET,
    NAME_THUNK_PAIR_GET_OFFSET, NAME_THUNK_PAIR_SIZE, REF_CELL_CLASS_ID, REF_CELL_CLASS_NAME,
    REF_CELL_SIZE, REF_CELL_VALUE_OFFSET, SIMSET_PRED_OFFSET, SIMSET_SUC_OFFSET,
    declared_class_name, enclosing_capture_field_name, enclosing_capture_source_name,
    formal_proc_capture_source_name, mangle_coro_entry_name, mangle_init_name, mangle_method_name,
    name_pack_env_slot_offset,
};
use crate::simulation::{block_is_simulation_prefixed, is_simset_method, is_simulation_builtin};
use crate::text::TextIntrinsic;
use crate::types::{ArithmeticLiteralKind, Declaration, Type};

use super::{
    BasicBlock, BinOp, BlockId, CallSig, CmpOp, DebugScope, DebugScopeKind, Function, Local,
    LocalId, MirType, Module, Op, Span, SpannedOp, UnOp,
};

/// Name of the shared MIR helper function used as the `get` thunk for every
/// outlined call-by-name integer formal's `env` cell (see
/// [`build_name_thunk_get_helper`]).
const NAME_THUNK_GET_HELPER: &str = "__simrt_name_get_ref";
/// Name of the shared MIR helper function used as the `set` thunk for every
/// outlined call-by-name integer formal's `env` cell (see
/// [`build_name_thunk_set_helper`]).
const NAME_THUNK_SET_HELPER: &str = "__simrt_name_set_ref";
/// Name of the shared MIR helper function used as the `get` thunk for an
/// outlined call-by-name integer formal whose actual is a 1-D integer array
/// element (`a(i)`) — see [`build_name_thunk_get_arr1_helper`] and
/// [`FunctionBuilder::name_thunk_triple_for_arr1_elem`].
const NAME_THUNK_GET_ARR1: &str = "__simrt_name_get_arr1";
/// Name of the shared MIR helper function used as the `set` thunk for an
/// outlined call-by-name integer formal whose actual is a 1-D integer array
/// element (`a(i)`) — see [`build_name_thunk_set_arr1_helper`].
const NAME_THUNK_SET_ARR1: &str = "__simrt_name_set_arr1";
/// Prefix for per-offset get helpers for outlined name formals whose actual is
/// a simple integer object field (`r.x`) — e.g. `__simrt_name_get_field_8`
/// (see [`build_name_thunk_get_field_helper`]). The field byte offset is baked
/// into the helper name/body because [`Op::FieldLoadI64`] takes a constant.
const NAME_THUNK_GET_FIELD_PREFIX: &str = "__simrt_name_get_field_";
/// Prefix for per-offset set helpers matching [`NAME_THUNK_GET_FIELD_PREFIX`].
const NAME_THUNK_SET_FIELD_PREFIX: &str = "__simrt_name_set_field_";
/// No-op set helper for read-only expression name actuals (get-only thunks).
const NAME_THUNK_SET_READONLY: &str = "__simrt_name_set_readonly";
/// Prefix for per-call-site get helpers that re-evaluate an integer expression
/// name actual (`__simrt_name_get_expr_0`, …).
const NAME_THUNK_GET_EXPR_PREFIX: &str = "__simrt_name_get_expr_";
/// Prefix for per-procedure formal-proc invoke shims (`__simrt_fp_invoke_Q1`).
const FORMAL_PROC_INVOKE_PREFIX: &str = "__simrt_fp_invoke_";

/// MIR helper that resumes the SQS current process (`Class$__init`) until it
/// yields or terminates.
const SIM_RUN_CURRENT: &str = "__simrt_sim_run_current";

mod builder;
mod classes;
mod collect;
mod name_thunks;
mod outline;
mod partition;
mod util;

use classes::*;
use collect::*;
use name_thunks::*;
use outline::*;
use partition::*;
use util::*;

#[cfg(test)]
mod tests;

/// The MIR-relevant part of a procedure's heading: enough to type-check and
/// lower call sites without re-deriving it from the AST every time.
#[derive(Debug, Clone)]
struct ProcSignature {
    /// Expanded parameter types: each outlined call-by-name integer formal
    /// contributes three consecutive entries (`FuncRef, FuncRef, RefI64`)
    /// rather than a single [`MirType::RefI64`].
    params: Vec<MirType>,
    /// `Some(ty)` for a function procedure; `None` for a void procedure.
    result: Option<MirType>,
    /// When [`Self::result`] is [`MirType::ObjectRef`], the declared
    /// `ref(Class)` qualification so call results keep remote-field typing.
    result_object_qual: Option<String>,
    /// Start indices in `params` of each name-thunk triple (`get`, `set`,
    /// `env`), in declaration order.
    name_thunk_starts: Vec<usize>,
    /// Parallel to `name_thunk_starts`: whether the procedure body assigns
    /// that formal (non-L-value expression actuals only allowed when false —
    /// re-evaluated via per-call-site get helpers, with temp-cell fallback).
    name_thunk_assigned: Vec<bool>,
    /// Indices in `params` for call-by-value array formals (need
    /// [`Op::ArrayCopy`] at the call site). Reference arrays alias.
    value_array_params: Vec<usize>,
    /// Indices in `params` for call-by-value text formals. §4.6.2 defines
    /// these as `FP :- copy(AP)`, so the call site deep-copies the frame;
    /// call-by-reference text formals pass the handle instead.
    value_text_params: Vec<usize>,
    /// Trailing [`MirType::RefI64`] parameters for free enclosing integer
    /// cells captured by an outlined call-by-name procedure (simtst35 `P`
    /// closing over outer `i`). Not part of the user-visible argument list;
    /// call sites pass [`Op::LocalAddr`] (or forward an existing thunk env).
    free_cell_params: Vec<String>,
    /// Indices in `params` of formal-procedure [`MirType::FuncRef`] formals
    /// (outlined recursive formal-proc procedures, simtst34).
    formal_proc_param_indices: Vec<usize>,
    /// External procedure stub with unknown formal list (`external procedure
    /// pa, pb`): call sites may pass any arity; arguments are evaluated for
    /// side effects and discarded.
    external_stub: bool,
}

/// Drops a method [`ProcSignature`]'s leading `__this` [`MirType::ObjectRef`]
/// parameter (index 0), producing the signature callers use to lower the
/// user-visible argument list — adjusting `name_thunk_starts` (all `>= 1`,
/// since `__this` never participates in a thunk triple) back down by one.
fn user_signature_without_this(signature: &ProcSignature) -> ProcSignature {
    ProcSignature {
        params: signature.params[1..].to_vec(),
        result: signature.result,
        result_object_qual: signature.result_object_qual.clone(),
        name_thunk_starts: signature
            .name_thunk_starts
            .iter()
            .map(|start| start - 1)
            .collect(),
        name_thunk_assigned: signature.name_thunk_assigned.clone(),
        value_array_params: signature
            .value_array_params
            .iter()
            .map(|index| index - 1)
            .collect(),
        value_text_params: signature
            .value_text_params
            .iter()
            .map(|index| index - 1)
            .collect(),
        free_cell_params: signature.free_cell_params.clone(),
        formal_proc_param_indices: signature
            .formal_proc_param_indices
            .iter()
            .map(|index| index - 1)
            .collect(),
        external_stub: signature.external_stub,
    }
}

fn result_object_qual_of(result_type: &Option<Type>) -> Option<String> {
    match result_type {
        Some(Type::ObjectRef(qual)) => Some(qual.clone()),
        _ => None,
    }
}

/// A class method borrowed from the AST, ready to lower as a mangled
/// [`Function`] with an implicit `__this` receiver.
#[derive(Debug, Clone, Copy)]
struct ClassMethod<'a> {
    class_name: &'a str,
    procedure: &'a ProcedureDeclaration,
}

/// Owned class-body initializer (concatenated statements) for `ClassName$__init`.
#[derive(Debug, Clone)]
struct ClassInit {
    class_name: String,
    body: Block,
    /// Statements after `inner` (main final then prefix finals after concat).
    tail_statements: Vec<Statement>,
    constructor_params: Vec<(String, FieldType)>,
    /// Switch declarations visible from the class's declaring block (§5.6.13).
    enclosing_switches: HashMap<String, Vec<crate::ast::DesignationalExpr>>,
}

/// Lowers `program` into a [`Module`] containing `main` plus one [`Function`]
/// per local procedure and per class method declared in `program`.
///
/// Errors on anything outside the scalar / Phase-5 MVP subset.
/// See the module docs for the exact rules.
pub fn lower_program(program: &Program) -> Result<Module, CompileError> {
    lower_program_with_source(program, "")
}

pub fn lower_program_with_source(program: &Program, source: &str) -> Result<Module, CompileError> {
    let mut module = Module::default();
    let layouts = layout::layouts_for_program(program)?;
    let classes = class_map_for_program(program);
    // System classes (Link/Head/Process) are injected for SIMSET *or* Simulation;
    // SQS scheduling context is Simulation / process only (not plain SIMSET).
    let has_simulation = layout::program_uses_simulation_scheduling(program);
    let inits = collect_class_inits(program, &layouts)?;

    let mut procedures = Vec::new();
    let mut methods = Vec::new();
    for block in &program.blocks {
        collect_procedures_with_enclosing_names(block, &HashSet::new(), &mut procedures);
        collect_class_methods(block, &mut methods);
    }
    collect_nested_procedures_from_methods(&methods, &layouts, &mut procedures);
    let external_stubs = collect_external_procedure_stubs(program);
    module.unresolved_externals = collect_unresolved_externals(&procedures, &external_stubs);
    let partitioned = partition_procedures(&procedures)?;
    // Methods with formal procedure / label / switch params cannot outline —
    // force call-site inlining via the name-param map (mangled keys).
    let (outline_methods, mut method_inline_procs) = partition_class_methods(&methods);
    let mut name_param_procs = partitioned.name_param_procs;
    for (mangled, procedure) in method_inline_procs.drain() {
        name_param_procs.insert(mangled, procedure);
    }
    let mut signatures = build_signatures(
        &partitioned.value_procedures,
        &outline_methods,
        &inits,
        &partitioned.name_outline_free_cells,
    )?;
    for stub in &external_stubs {
        if !signatures.contains_key(&stub.procedure.name)
            && !is_mir_known_external(&stub.procedure.name)
        {
            let mut signature = build_signature(&stub.procedure, Vec::new())?;
            if stub.foreign.is_some() {
                signature.external_stub = false;
            }
            signatures.insert(stub.procedure.name.clone(), signature);
        }
    }
    {
        // Component entry points take only the object; the constructor
        // parameters are already in fields by the time the body runs.
        for layout in layouts.values().filter(|l| l.runs_on_own_stack) {
            signatures.insert(
                mangle_coro_entry_name(&layout.name),
                ProcSignature {
                    params: vec![MirType::ObjectRef],
                    result: None,
                    result_object_qual: None,
                    name_thunk_starts: Vec::new(),
                    name_thunk_assigned: Vec::new(),
                    value_array_params: Vec::new(),
                    value_text_params: Vec::new(),
                    free_cell_params: Vec::new(),
                    formal_proc_param_indices: Vec::new(),
                    external_stub: false,
                },
            );
        }
    }
    if has_simulation {
        signatures.insert(
            SIM_RUN_CURRENT.to_string(),
            ProcSignature {
                params: Vec::new(),
                result: None,
                result_object_qual: None,
                name_thunk_starts: Vec::new(),
                name_thunk_assigned: Vec::new(),
                value_array_params: Vec::new(),
                value_text_params: Vec::new(),
                free_cell_params: Vec::new(),
                formal_proc_param_indices: Vec::new(),
                external_stub: false,
            },
        );
    }

    let mut builder = FunctionBuilder::new(
        "main".to_string(),
        &mut module.strings,
        &signatures,
        &name_param_procs,
        &partitioned.ref_alias_procs,
        &layouts,
        &classes,
    )
    .with_source_text(source);
    let entry = builder.new_block();
    builder.switch_to(entry);
    // `Suc` / `Pred` return none at the ring's `Head` (§SIMSET), which the
    // runtime detects by class id. SIMSET-only programs never reach
    // `Op::SimBegin`, so register it once on entry instead (simtst93/94/96).
    if let Some(head_layout) = builder.find_layout("Head") {
        let class_id = head_layout.class_id;
        builder.push(Op::SimsetSetHeadClassId { class_id }, 0..0);
    }
    for block in &program.blocks {
        builder.predeclare_labels_in_block(block);
        builder.lower_block(block)?;
    }
    builder.push(Op::Return { value: None }, 0..0);
    let (main, helpers) = builder.finish(entry, None);
    module.functions.extend(helpers);
    module.functions.push(main);

    if needs_name_thunk_helpers(&partitioned.value_procedures, &methods) {
        module.functions.push(build_name_thunk_get_helper());
        module.functions.push(build_name_thunk_set_helper());
        module.functions.push(build_name_thunk_get_arr1_helper());
        module.functions.push(build_name_thunk_set_arr1_helper());
        module
            .functions
            .push(build_name_thunk_set_readonly_helper());
    }

    for procedure in &partitioned.value_procedures {
        let (func, helpers) = lower_procedure(
            procedure,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            source,
        )?;
        module.functions.extend(helpers);
        module.functions.push(func);
    }
    for stub in &external_stubs {
        if is_mir_known_external(&stub.procedure.name) {
            continue;
        }
        if module
            .functions
            .iter()
            .any(|func| func.name.eq_ignore_ascii_case(&stub.procedure.name))
        {
            continue;
        }
        let (mut func, helpers) = lower_procedure(
            &stub.procedure,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            source,
        )?;
        func.foreign = stub.foreign.clone();
        module.functions.extend(helpers);
        module.functions.push(func);
    }
    for method in &outline_methods {
        let (func, helpers) = lower_method(
            method.class_name,
            method.procedure,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            source,
        )?;
        module.functions.extend(helpers);
        module.functions.push(func);
    }
    for init in &inits {
        let (func, helpers) = lower_class_init(
            &init.class_name,
            &init.body,
            &init.tail_statements,
            &init.constructor_params,
            &init.enclosing_switches,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            source,
        )?;
        module.functions.extend(helpers);
        module.functions.push(func);
    }
    if has_simulation {
        module.functions.push(build_sim_run_current(
            &layouts,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &classes,
            &mut module.strings,
            source,
        )?);
    }

    module.class_layouts = layouts.into_values().collect();
    module.class_layouts.sort_by_key(|layout| layout.class_id);
    dedupe_functions_by_name(&mut module.functions);
    crate::mir::ref_cell::install_ref_cell_homes(&mut module);
    install_public_exports(&mut module, program);
    Ok(module)
}

fn public_procedure_module_names(program: &Program) -> HashSet<String> {
    let mut names = HashSet::new();
    for block in &program.blocks {
        // Chapter 6 procedure modules are parsed as a wrapper block with the
        // procedure(s) and no statements (`wrap_top_level_procedure`). A
        // `begin … end` program that also declares procedures and has
        // statements keeps those procedures block-local.
        if block.statements.is_empty() && block.classes.is_empty() {
            for procedure in &block.procedures {
                names.insert(procedure.name.to_ascii_lowercase());
            }
        }
    }
    names
}

fn collect_procedure_identifications(program: &Program) -> HashMap<String, String> {
    let mut ids = HashMap::new();
    for block in &program.blocks {
        collect_procedure_identifications_in_block(block, &mut ids);
    }
    ids
}

fn collect_procedure_identifications_in_block(block: &Block, ids: &mut HashMap<String, String>) {
    for procedure in &block.procedures {
        if let Some(identification) = &procedure.identification {
            ids.insert(procedure.name.to_ascii_lowercase(), identification.clone());
        }
        collect_procedure_identifications_in_block(&procedure.body, ids);
    }
    for nested in &block.body {
        collect_procedure_identifications_in_block(nested, ids);
    }
    for class in &block.classes {
        collect_procedure_identifications_in_block(&class.body, ids);
    }
}

fn install_public_exports(module: &mut Module, program: &Program) {
    let public = public_procedure_module_names(program);
    let identifications = collect_procedure_identifications(program);
    for function in &mut module.functions {
        if function.export.is_some() || !function.is_scalar_exportable() {
            continue;
        }
        let key = function.name.to_ascii_lowercase();
        if let Some(identification) = identifications.get(&key)
            && crate::mir::parse_export_identification(identification).is_some()
        {
            function.export = Some(identification.clone());
            continue;
        }
        if public.contains(&key) {
            function.export = Some(function.name.clone());
        }
    }
}

/// Best-effort variant of [`lower_program`]: keeps lowering after an
/// unsupported declaration or statement instead of aborting, collecting
/// every error encountered alongside the (possibly partial) module.
pub fn lower_program_lenient(program: &Program) -> (Module, Vec<CompileError>) {
    let mut module = Module::default();
    let mut errors = Vec::new();

    let layouts = match layout::layouts_for_program(program) {
        Ok(layouts) => layouts,
        Err(error) => {
            errors.push(error);
            HashMap::new()
        }
    };

    let mut procedures = Vec::new();
    let mut methods = Vec::new();
    for block in &program.blocks {
        collect_procedures_with_enclosing_names(block, &HashSet::new(), &mut procedures);
        collect_class_methods(block, &mut methods);
    }
    collect_nested_procedures_from_methods(&methods, &layouts, &mut procedures);
    let inits = match collect_class_inits(program, &layouts) {
        Ok(inits) => inits,
        Err(error) => {
            errors.push(error);
            Vec::new()
        }
    };
    let partitioned = match partition_procedures(&procedures) {
        Ok(parts) => parts,
        Err(error) => {
            errors.push(error);
            PartitionedProcedures {
                value_procedures: Vec::new(),
                name_param_procs: HashMap::new(),
                ref_alias_procs: HashMap::new(),
                name_outline_free_cells: HashMap::new(),
            }
        }
    };
    let (outline_methods, mut method_inline_procs) = partition_class_methods(&methods);
    let mut name_param_procs = partitioned.name_param_procs;
    for (mangled, procedure) in method_inline_procs.drain() {
        name_param_procs.insert(mangled, procedure);
    }
    let classes = class_map_for_program(program);
    let has_simulation = layout::program_uses_simulation_scheduling(program);

    let mut signatures = match build_signatures(
        &partitioned.value_procedures,
        &outline_methods,
        &inits,
        &partitioned.name_outline_free_cells,
    ) {
        Ok(signatures) => signatures,
        Err(error) => {
            errors.push(error);
            HashMap::new()
        }
    };
    if has_simulation {
        signatures.insert(
            SIM_RUN_CURRENT.to_string(),
            ProcSignature {
                params: Vec::new(),
                result: None,
                result_object_qual: None,
                name_thunk_starts: Vec::new(),
                name_thunk_assigned: Vec::new(),
                value_array_params: Vec::new(),
                value_text_params: Vec::new(),
                free_cell_params: Vec::new(),
                formal_proc_param_indices: Vec::new(),
                external_stub: false,
            },
        );
    }

    let mut builder = FunctionBuilder::new(
        "main".to_string(),
        &mut module.strings,
        &signatures,
        &name_param_procs,
        &partitioned.ref_alias_procs,
        &layouts,
        &classes,
    );

    let entry = builder.new_block();
    builder.switch_to(entry);
    for block in &program.blocks {
        builder.lower_block_collecting(block, &mut errors);
    }
    builder.push(Op::Return { value: None }, 0..0);
    let (main, helpers) = builder.finish(entry, None);
    module.functions.extend(helpers);
    module.functions.push(main);

    if needs_name_thunk_helpers(&partitioned.value_procedures, &methods) {
        module.functions.push(build_name_thunk_get_helper());
        module.functions.push(build_name_thunk_set_helper());
        module.functions.push(build_name_thunk_get_arr1_helper());
        module.functions.push(build_name_thunk_set_arr1_helper());
        module
            .functions
            .push(build_name_thunk_set_readonly_helper());
    }

    for procedure in &partitioned.value_procedures {
        match lower_procedure(
            procedure,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            "",
        ) {
            Ok((function, helpers)) => {
                module.functions.extend(helpers);
                module.functions.push(function);
            }
            Err(error) => errors.push(error),
        }
    }
    for method in &outline_methods {
        match lower_method(
            method.class_name,
            method.procedure,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            "",
        ) {
            Ok((function, helpers)) => {
                module.functions.extend(helpers);
                module.functions.push(function);
            }
            Err(error) => errors.push(error),
        }
    }
    for init in &inits {
        match lower_class_init(
            &init.class_name,
            &init.body,
            &init.tail_statements,
            &init.constructor_params,
            &init.enclosing_switches,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &layouts,
            &classes,
            &mut module.strings,
            has_simulation,
            "",
        ) {
            Ok((function, helpers)) => {
                module.functions.extend(helpers);
                module.functions.push(function);
            }
            Err(error) => errors.push(error),
        }
    }
    if has_simulation {
        match build_sim_run_current(
            &layouts,
            &signatures,
            &name_param_procs,
            &partitioned.ref_alias_procs,
            &classes,
            &mut module.strings,
            "",
        ) {
            Ok(function) => module.functions.push(function),
            Err(error) => errors.push(error),
        }
    }

    module.class_layouts = layouts.into_values().collect();
    module.class_layouts.sort_by_key(|layout| layout.class_id);
    dedupe_functions_by_name(&mut module.functions);
    crate::mir::ref_cell::install_ref_cell_homes(&mut module);
    install_public_exports(&mut module, program);
    (module, errors)
}

/// An outer `__this` receiver stacked by `inspect` / a prefixed block / an
/// inlined method, with the state that decides how bare names resolve on it.
#[derive(Debug, Clone)]
struct ThisReceiver {
    id: LocalId,
    /// Qualification active when this receiver was pushed (`inspect this a`
    /// overwrites `ref_qual` for an id that is also the enclosing instance).
    qual: Option<String>,
    /// [`FunctionBuilder::access_level_substitutions`] at push time.
    substitutions: bool,
    /// Connected by `inspect` / a prefixed block rather than being the instance
    /// whose class text was being lowered.
    connection: bool,
}

/// Formal procedure actuals: a free procedure identifier, or a bound method
/// `object.method` (e.g. `S(x.T, …)`).
#[derive(Debug, Clone)]
enum FormalProcTarget {
    Procedure(String),
    Method { object: LocalId, method: String },
}

/// An assignable/readable location resolved from a [`Variable`] (see
/// [`FunctionBuilder::resolve_place`]).
#[derive(Debug, Clone)]
enum Place {
    /// A plain scalar (or array-descriptor / object-ref) [`Local`] slot.
    Local(LocalId),
    /// One element of an integer array: `array[i0, i1, …]`, where `array` is
    /// a [`MirType::ArrayI64`] local holding the descriptor pointer.
    ArrayElement {
        array: LocalId,
        indices: Vec<LocalId>,
    },
    /// Integer attribute at a fixed byte `offset` of an object reference.
    RemoteI64 { object: LocalId, offset: i64 },
    /// Object-reference attribute at a fixed byte `offset` (`ref(C)` slot).
    /// `qual` is the declared class name when known statically.
    RemoteObject {
        object: LocalId,
        offset: i64,
        qual: Option<String>,
    },
    /// Boolean attribute at a fixed byte `offset` (stored as 0/1 `i64`).
    RemoteBool { object: LocalId, offset: i64 },
    /// Real attribute at a fixed byte `offset` (IEEE-754 `f64` bits).
    RemoteF64 { object: LocalId, offset: i64 },
    /// Text attribute: pointer-sized slot holding a `SimrtTextFrame*`.
    RemoteText { object: LocalId, offset: i64 },
    /// An enclosing variable reached through a pointer held at byte `offset` of
    /// `object`, which points at the variable's home in the block instance that
    /// declares it. 5.5 makes an enclosing variable *one* variable, so a
    /// component on its own stack has to reach the declaring frame rather than
    /// carry a copy that another component's copy can overwrite. `value_ty` is
    /// a scalar, or `ObjectRef` for a class on its own stack (see
    /// [`FunctionBuilder::capture_by_reference`]).
    CaptureCell {
        object: LocalId,
        offset: i64,
        value_ty: MirType,
        qual: Option<String>,
    },
    /// Outlined call-by-name integer/boolean formal: a `(get, set, env)` thunk
    /// triple. Reads/writes go through [`Op::CallIndirect`] on `get`/`set`
    /// with `env` as the sole (`get`) or first (`set`) argument. Storage is
    /// always i64; `value_ty` is [`MirType::Bool`] or [`MirType::I64`].
    NameThunk {
        get: LocalId,
        set: LocalId,
        env: LocalId,
        value_ty: MirType,
    },
    /// `t.main`: the whole underlying frame of text value `frame`. Reads and
    /// writes go through a fresh [`Op::TextMain`] view (assignment copies
    /// characters into the shared buffer via [`Op::TextAssign`], matching
    /// the interpreter's `assign_value_from`).
    TextMain { frame: LocalId },
    /// `t.sub(start, length)` as an assignment target: a bounded view of
    /// text value `frame`, re-computed via [`Op::TextSub`] on each access.
    TextSub {
        frame: LocalId,
        start: LocalId,
        length: LocalId,
    },
    /// `f.image` (BASICIO §10.3) as an assignment target: reads go through
    /// [`Op::CallBasicioImage`], writes replace the buffer via
    /// [`Op::CallBasicioSetImage`].
    BasicioImage { object: LocalId },
}

fn remote_place(
    object: LocalId,
    offset: i64,
    field_ty: FieldType,
    object_qual: Option<String>,
) -> Place {
    match field_ty {
        FieldType::I64 => Place::RemoteI64 { object, offset },
        FieldType::ObjectRef
        | FieldType::ArrayI64
        | FieldType::ArrayBool
        | FieldType::ArrayF64
        | FieldType::ArrayText => Place::RemoteObject {
            object,
            offset,
            qual: object_qual,
        },
        FieldType::Bool => Place::RemoteBool { object, offset },
        FieldType::F64 => Place::RemoteF64 { object, offset },
        FieldType::Text => Place::RemoteText { object, offset },
    }
}

fn mir_type_for_field(field_ty: FieldType) -> MirType {
    match field_ty {
        FieldType::I64 => MirType::I64,
        FieldType::Bool => MirType::Bool,
        FieldType::F64 => MirType::F64,
        FieldType::Text => MirType::Text,
        FieldType::ObjectRef => MirType::ObjectRef,
        FieldType::ArrayI64 | FieldType::ArrayBool => MirType::ArrayI64,
        FieldType::ArrayF64 => MirType::ArrayF64,
        FieldType::ArrayText => MirType::ArrayText,
    }
}

/// Where a statement label resolves during lowering.
enum LabelTarget {
    Block(BlockId),
    /// Label is not in the current (inlined) CFG — runtime must abandon calls.
    Escape(String),
}

/// Builds a single [`Function`], tracking the in-progress block list, the
/// flat local space, and the `name -> LocalId` scope used to resolve
/// [`Variable::Simple`] references.
struct FunctionBuilder<'a> {
    name: String,
    locals: Vec<Local>,
    /// How many of `locals`' leading entries are parameters (see
    /// [`Self::finish`], which splits on this to build [`Function::params`]
    /// / [`Function::locals`]); zero for `main`.
    param_count: usize,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    scope: HashMap<String, LocalId>,
    /// Locals declared with `=` constant initialization (§5.8); assignment rejected.
    constants: HashSet<String>,
    /// Outlined call-by-name integer formals bound in this function:
    /// `formal name -> (get, set, env)` thunk-triple locals. Checked by
    /// [`Self::resolve_place`] before `scope` (see [`Self::bind_formal_params`]);
    /// the formal's plain name is deliberately *not* also inserted into
    /// `scope`.
    name_thunks: HashMap<String, (LocalId, LocalId, LocalId)>,
    /// Logical value type for each [`Self::name_thunks`] entry ([`MirType::I64`]
    /// or [`MirType::Bool`]); storage through get/set helpers is always i64.
    name_thunk_tys: HashMap<String, MirType>,
    /// Raw [`MirType::RefI64`] cell address behind a free-cell env parameter
    /// (see [`Self::bind_free_cell_thunk_helpers`]). A free cell is always an
    /// integer or boolean home, so it stays a linear-memory address; only the
    /// *name-thunk* view of it is boxed into a
    /// [`NAME_INT_ENV_CLASS_NAME`] object. Forwarding the same cell on to
    /// another outlined procedure's free-cell parameter uses this address, so
    /// the box is never unpacked again.
    free_cell_addrs: HashMap<String, LocalId>,
    /// Outlined formal-procedure formals: `name -> (func, env)` fat pointer.
    formal_proc_refs: HashMap<String, (LocalId, LocalId)>,
    /// Declared (or `new`-produced) class qualification for [`MirType::ObjectRef`]
    /// locals — used to resolve remote attribute offsets statically.
    ref_qual: HashMap<LocalId, String>,
    /// Element MIR type for array descriptor locals ([`MirType::ArrayI64`] may
    /// hold integer, boolean, character, or object-reference elements). Used so
    /// `ba(i)` reads as [`MirType::Bool`] rather than I64.
    array_elem_ty: HashMap<LocalId, MirType>,
    /// Declared class for [`MirType::ObjectRef`] elements of a ref-array
    /// descriptor (`ref(A) array ra` → `"A"`), so `ra(i).attr` can resolve.
    array_elem_qual: HashMap<LocalId, String>,
    strings: &'a mut Vec<String>,
    string_index: HashMap<String, usize>,
    temp_counter: usize,
    /// `name -> signature` table for every local procedure and class method
    /// in the program, built once up front by [`build_signatures`].
    signatures: &'a HashMap<String, ProcSignature>,
    /// Local procedures with call-by-name formals; inlined at call sites.
    name_param_procs: &'a HashMap<String, &'a ProcedureDeclaration>,
    /// Text / `ref(C)` call-by-reference procedures; inlined by sharing the
    /// caller's [`LocalId`].
    ref_alias_procs: &'a HashMap<String, &'a ProcedureDeclaration>,
    /// Active Jensen bindings: formal name → actual expression (re-evaluated
    /// on each read; assignment targets re-resolved on each write).
    name_bindings: HashMap<String, Expr>,
    /// Declared MIR type of each active name formal (for assignment conversion
    /// and chained-assignment values; actuals may have a different type).
    name_formal_tys: HashMap<String, MirType>,
    /// Formal procedure bindings: formal name → actual procedure or bound method.
    formal_proc_bindings: HashMap<String, FormalProcTarget>,
    /// LABEL formal → actual designational expression (inlined at `goto`).
    formal_label_bindings: HashMap<String, DesignationalExpr>,
    /// SWITCH formal → actual switch identifier (inlined at `goto s(i)`).
    formal_switch_bindings: HashMap<String, String>,
    /// Caller `name_bindings` snapshots at each [`Self::inline_name_procedure`]
    /// entry — actuals are evaluated in this environment, not under the
    /// current formals (avoids `y → y` recursion when names coincide).
    name_env_stack: Vec<HashMap<String, Expr>>,
    name_formal_ty_stack: Vec<HashMap<String, MirType>>,
    /// Caller `formal_proc_bindings` snapshots paired with [`Self::name_env_stack`].
    formal_proc_env_stack: Vec<HashMap<String, FormalProcTarget>>,
    formal_label_env_stack: Vec<HashMap<String, DesignationalExpr>>,
    formal_switch_env_stack: Vec<HashMap<String, String>>,
    /// Per inlined frame: `(formal_name, previous_scope_binding)` so
    /// [`Self::with_caller_name_env`] can unshadow formals while re-evaluating
    /// name actuals (e.g. formal `rav` must not hide outer `rav` in `rav.tva1`).
    inline_scope_restores: Vec<Vec<(String, Option<LocalId>)>>,
    /// Names declared in the current inlined procedure body (not formals).
    /// These shadow call-by-name formals (simtst39); other enclosing scope
    /// names do not (simtst63).
    inline_body_locals: Vec<HashSet<String>>,
    /// Stack of inlined procedure names (name-param and ref-alias; detects recursion).
    inline_stack: Vec<String>,
    /// Source span of each active flattened debug scope (inlined procedures and
    /// nested/prefixed blocks). Locals allocated while the stack is non-empty
    /// inherit the innermost span as [`Local::debug_scope`].
    inline_debug_scopes: Vec<Span>,
    /// Unique debug scopes recorded for [`Function::debug_scopes`].
    recorded_debug_scopes: Vec<DebugScope>,
    /// Per inlined procedure: whether a bare `detach` in its body names the
    /// receiver at the call site. A procedure declared in an enclosing block
    /// rather than in the receiver's class has its own owner, so its detach
    /// attribute is that block instance's -- see the `detach` lowering.
    inline_detach_names_receiver: Vec<bool>,
    layouts: &'a HashMap<String, ClassLayout>,
    /// Raw (pre-layout) class declarations for prefix / subclass checks (`qua`).
    classes: &'a HashMap<String, ClassDeclaration>,
    /// When lowering a class method: the `__this` receiver local used to
    /// rewrite bare field names into remote loads/stores.
    method_this: Option<LocalId>,
    /// Outer receivers pushed by nested `inspect` / prefixed blocks so bare
    /// names can fall back to the enclosing class instance when the connected
    /// object does not declare the attribute (`station` body uses `nr` inside
    /// `inspect … when kund do`). Each entry stores the qualification that was
    /// active for that receiver when it was pushed (needed when `inspect this a`
    /// overwrites `ref_qual` on the same object id as the enclosing class).
    method_this_stack: Vec<ThisReceiver>,
    /// Whether [`Self::method_this`] was connected by `inspect` / a prefixed
    /// block instead of being the instance whose class text is being lowered.
    /// Attribute lookup on a connected receiver obeys §5.5.3–§5.5.6 protection.
    method_this_is_connection: bool,
    /// Source span of the statement/expression being lowered: the class whose
    /// *text* it is decides attribute visibility (§5.5.6). Concatenated `$__init`
    /// bodies mix prefix levels and inlined procedure bodies come from elsewhere,
    /// so the access level cannot be a per-function constant.
    text_span: Span,
    /// Access level for text at a prefix level that is not inside any class
    /// declaration: the body of a prefixed block (§4.10.1).
    prefixed_block_access: Option<String>,
    /// Simple names of procedures declared in the active prefixed block (not
    /// the prefix class). Virtual matches like simtst92's `P2` live here and
    /// must beat `C$P2`; globals of the same name must not (simtst98 `d begin`
    /// `virtproc`).
    prefixed_block_procs: HashSet<String>,
    /// When true, bare attribute names resolve through
    /// [`substitute_remote_attribute`] for the current `ref_qual` (connection
    /// blocks). Class `$__init` bodies already contain rewritten `i$B` names
    /// and must use exact field matches so prefix statements keep writing the
    /// prefix-level attribute.
    access_level_substitutions: bool,
    /// Nesting depth of [`Self::with_connection_this`] (`inspect` / when/do /
    /// prefixed blocks). Prefers connected attributes over outer locals.
    connection_depth: usize,
    /// Nesting depth of `inspect` connection blocks only (not prefixed blocks).
    /// Used so `inspect … when B do P2` prefers `B.P2` over a free `P2`
    /// (simtst71) without stealing virtual overrides inside `C begin … end`
    /// (simtst92).
    inspect_connection_depth: usize,
    /// Attribute names whose outer locals were intentionally kept when entering
    /// a connection (enclosing-capture sources). Bare uses in the connection
    /// body still bind to the attribute; block locals are not listed here.
    connection_kept_outers: HashSet<String>,
    /// Statement labels → basic blocks for [`Op::Jump`] (`goto` targets).
    /// Keys are lowercased for case-insensitive Simula matching.
    ///
    /// Each label *occurrence* gets its own BB (fallthrough). Gotos bind to the
    /// **last** occurrence of that name in the current label scope (innermost /
    /// last definition wins — virtual label matching across concatenated class
    /// bodies, and fresh scopes per inlined procedure so `LOOP`/`PRINT` do not
    /// collide across call sites).
    labels: HashMap<String, BlockId>,
    /// BBs for upcoming [`StatementKind::Labeled`] occurrences, in source order
    /// (filled by [`Self::predeclare_labels_in_statements`]).
    label_def_queue: VecDeque<BlockId>,
    /// Caller label maps stacked by [`Self::with_fresh_label_scope`] so a
    /// by-value LABEL formal (`goto LFD`) can still resolve the caller's
    /// labels after the callee's fresh scope cleared `labels` (simtst31).
    outer_labels: Vec<HashMap<String, BlockId>>,
    /// Switch declarations in scope for `goto s(i)` designators (§4.5).
    /// Keys are lowercased.
    switches: HashMap<String, Vec<crate::ast::DesignationalExpr>>,
    /// Compiled dispatch chain per switch (entry block, shared index local),
    /// built lazily on first `goto`/reference. A switch's element list may
    /// legally refer to itself or to another switch that refers back to it
    /// (§4.5); reusing the cached entry block instead of re-lowering the
    /// element chain inline avoids unbounded recursion for such cycles.
    switch_dispatch: HashMap<String, (BlockId, LocalId)>,
    /// Per-call-site expression re-eval get helpers queued while lowering
    /// this function (drained by [`Self::finish`]).
    pending_helpers: Vec<Function>,
    /// Monotonic counter for `__simrt_name_get_expr_N` names in this function.
    expr_helper_counter: usize,
    /// True while lowering a Simulation-prefixed block (or class inits for a
    /// Simulation program) so `hold` / `activate` / SQS ops are allowed.
    simulation_context: bool,
    /// Source text for accurate §9.6 `sourceline` (empty → line 1).
    source_text: String,
    /// After [`Self::lower_random_stream_addr`] materializes an enclosing
    /// integer capture into a temp cell, store that cell back to
    /// `object[offset]` once the `CallEnv` that mutates `*stream` completes.
    pending_stream_field_writeback: Option<(LocalId, i64, LocalId)>,
}
