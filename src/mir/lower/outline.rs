//! Submodule of [`crate::mir::lower`].

use super::*;

/// Lowers a single local procedure into its own [`Function`]. Parameters
/// become the function's leading locals (bound from Cranelift block
/// parameters, see `src/codegen/cranelift/emit.rs`); a function procedure
/// additionally gets an implicit, zero-initialized local named after the
/// procedure that its body assigns into (`f := expr;`) and the trailing
/// synthesized `Op::Return` reads back.
pub(in crate::mir::lower) fn lower_procedure(
    procedure: &ProcedureDeclaration,
    signatures: &HashMap<String, ProcSignature>,
    name_param_procs: &HashMap<String, &ProcedureDeclaration>,
    ref_alias_procs: &HashMap<String, &ProcedureDeclaration>,
    layouts: &HashMap<String, ClassLayout>,
    classes: &HashMap<String, ClassDeclaration>,
    strings: &mut Vec<String>,
    has_simulation: bool,
    source: &str,
) -> Result<(Function, Vec<Function>), CompileError> {
    let mut builder = FunctionBuilder::new(
        procedure.name.clone(),
        strings,
        signatures,
        name_param_procs,
        ref_alias_procs,
        layouts,
        classes,
    )
    .with_source_text(source);
    // Outlined free procedures (Simulation-block helpers, nested procs that
    // don't capture enclosing names) must see hold/passivate/time/current
    // the same way methods and `$__init` do.
    builder.simulation_context = has_simulation;

    let entry = builder.new_block();
    builder.switch_to(entry);

    if procedure.is_external {
        // Stub: bind formals for ABI shape, return zero/none without a body.
        let _ = builder.bind_formal_params(
            &procedure
                .parameters
                .iter()
                .filter(|param| !param.is_procedure)
                .cloned()
                .collect::<Vec<_>>(),
        );
        builder.param_count = builder.locals.len();
        let result_ty = match &procedure.result_type {
            Some(ty) => Some(mir_type_for(ty)?),
            None => None,
        };
        let result_local = result_ty.map(|ty| {
            let id = builder.new_local(procedure.name.clone(), ty);
            match ty {
                MirType::I64 => builder.push(Op::ConstI64 { dest: id, value: 0 }, 0..0),
                MirType::Bool => builder.push(
                    Op::ConstBool {
                        dest: id,
                        value: false,
                    },
                    0..0,
                ),
                MirType::F64 | MirType::LongF64 => builder.push(
                    Op::ConstF64 {
                        dest: id,
                        value: 0.0,
                    },
                    0..0,
                ),
                MirType::Text => builder.push(Op::TextNotext { dest: id }, 0..0),
                MirType::ObjectRef => builder.push(Op::ConstNone { dest: id }, 0..0),
                _ => builder.push(Op::ConstI64 { dest: id, value: 0 }, 0..0),
            }
            id
        });
        builder.push(
            Op::Return {
                value: result_local,
            },
            0..0,
        );
        return Ok(builder.finish(entry, result_ty));
    }

    builder.bind_formal_params(&procedure.parameters)?;
    let free_cells = signatures
        .get(&procedure.name)
        .map(|sig| sig.free_cell_params.clone())
        .unwrap_or_default();
    let free_envs = builder.bind_free_cell_param_envs(&free_cells);
    builder.param_count = builder.locals.len();
    let free_tys: Vec<MirType> = free_cells
        .iter()
        .map(|name| infer_free_cell_value_ty(procedure, name))
        .collect();
    builder.bind_free_cell_thunk_helpers(&free_cells, &free_envs, &free_tys);

    let result_ty = match &procedure.result_type {
        Some(ty) => Some(mir_type_for(ty)?),
        None => None,
    };
    let result_local = result_ty.map(|ty| {
        let id = builder.new_local(procedure.name.clone(), ty);
        builder.scope.insert(procedure.name.clone(), id);
        match ty {
            MirType::I64 => builder.push(Op::ConstI64 { dest: id, value: 0 }, 0..0),
            MirType::Bool => builder.push(
                Op::ConstBool {
                    dest: id,
                    value: false,
                },
                0..0,
            ),
            MirType::F64 | MirType::LongF64 => builder.push(
                Op::ConstF64 {
                    dest: id,
                    value: 0.0,
                },
                0..0,
            ),
            MirType::Text => builder.push(Op::TextNotext { dest: id }, 0..0),
            MirType::ObjectRef => builder.push(Op::ConstNone { dest: id }, 0..0),
            // `mir_type_for` never maps a procedure result type to `ArrayI64`/`RefI64`.
            MirType::ArrayI64
            | MirType::ArrayF64
            | MirType::ArrayText
            | MirType::RefI64
            | MirType::FuncRef => {
                unreachable!("procedure results are never array/ref-pointer types")
            }
        }
        id
    });

    builder.predeclare_labels_in_block(&procedure.body);
    builder.lower_block_body(&procedure.body)?;
    builder.push(
        Op::Return {
            value: result_local,
        },
        0..0,
    );

    Ok(builder.finish(entry, result_ty))
}

/// Lowers a class method into a mangled [`Function`] whose first parameter is
/// the `__this` object reference. Bare field names in the body resolve to
/// remote loads/stores on that receiver (see [`FunctionBuilder::resolve_place`]).
pub(in crate::mir::lower) fn lower_method(
    class_name: &str,
    procedure: &ProcedureDeclaration,
    signatures: &HashMap<String, ProcSignature>,
    name_param_procs: &HashMap<String, &ProcedureDeclaration>,
    ref_alias_procs: &HashMap<String, &ProcedureDeclaration>,
    layouts: &HashMap<String, ClassLayout>,
    classes: &HashMap<String, ClassDeclaration>,
    strings: &mut Vec<String>,
    has_simulation: bool,
    source: &str,
) -> Result<(Function, Vec<Function>), CompileError> {
    let mangled = mangle_method_name(class_name, &procedure.name);
    let mut builder = FunctionBuilder::new(
        mangled,
        strings,
        signatures,
        name_param_procs,
        ref_alias_procs,
        layouts,
        classes,
    )
    .with_source_text(source);
    builder.simulation_context = has_simulation;

    let entry = builder.new_block();
    builder.switch_to(entry);

    let this_id = builder.new_local("__this".to_string(), MirType::ObjectRef);
    builder.scope.insert("__this".to_string(), this_id);
    builder.note_object_qual(this_id, class_name.to_string());
    builder.method_this = Some(this_id);
    // Methods are lowered from the raw (pre-concatenation) AST, so bare
    // attribute names must follow the declaring class's identifier
    // substitutions (`i` → `i$B` inside `B` methods; simtst60).
    builder.access_level_substitutions = true;

    builder.bind_formal_params(&procedure.parameters)?;
    builder.param_count = builder.locals.len();

    let result_ty = match &procedure.result_type {
        Some(ty) => Some(mir_type_for(ty)?),
        None => None,
    };
    let result_local = result_ty.map(|ty| {
        let id = builder.new_local(procedure.name.clone(), ty);
        builder.scope.insert(procedure.name.clone(), id);
        match ty {
            MirType::I64 => builder.push(Op::ConstI64 { dest: id, value: 0 }, 0..0),
            MirType::Bool => builder.push(
                Op::ConstBool {
                    dest: id,
                    value: false,
                },
                0..0,
            ),
            MirType::F64 | MirType::LongF64 => builder.push(
                Op::ConstF64 {
                    dest: id,
                    value: 0.0,
                },
                0..0,
            ),
            MirType::Text => builder.push(Op::TextNotext { dest: id }, 0..0),
            MirType::ObjectRef => builder.push(Op::ConstNone { dest: id }, 0..0),
            MirType::ArrayI64
            | MirType::ArrayF64
            | MirType::ArrayText
            | MirType::RefI64
            | MirType::FuncRef => {
                unreachable!("procedure results are never array/ref-pointer types")
            }
        }
        id
    });

    builder.predeclare_labels_in_block(&procedure.body);
    builder.lower_block_body(&procedure.body)?;
    builder.push(
        Op::Return {
            value: result_local,
        },
        0..0,
    );

    Ok(builder.finish(entry, result_ty))
}

/// Lowers class-body initial statements (and split-body tails after `inner`)
/// into `ClassName$__init(__this, ...params)`. Constructor parameters are
/// stored into their attribute fields before the body runs; bare names in the
/// body resolve through `__this` (params are not left in the name scope).
/// Lowers `ClassName$__init`: store constructor params into fields, then run
/// the class body (and concatenated `inner` tails). When the class is a
/// component (§7), the body instead becomes a separate entry point run on the
/// object's own stack, and `$__init` stops after the parameters.
pub(in crate::mir::lower) fn lower_class_init(
    class_name: &str,
    body: &Block,
    tail_statements: &[Statement],
    constructor_params: &[(String, FieldType)],
    enclosing_switches: &HashMap<String, Vec<crate::ast::DesignationalExpr>>,
    signatures: &HashMap<String, ProcSignature>,
    name_param_procs: &HashMap<String, &ProcedureDeclaration>,
    ref_alias_procs: &HashMap<String, &ProcedureDeclaration>,
    layouts: &HashMap<String, ClassLayout>,
    classes: &HashMap<String, ClassDeclaration>,
    strings: &mut Vec<String>,
    has_simulation: bool,
    source: &str,
) -> Result<(Function, Vec<Function>), CompileError> {
    let mangled = mangle_init_name(class_name);
    let runs_on_own_stack = layouts
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(class_name))
        .is_some_and(|(_, layout)| layout.runs_on_own_stack);
    let mut builder = FunctionBuilder::new(
        mangled,
        strings,
        signatures,
        name_param_procs,
        ref_alias_procs,
        layouts,
        classes,
    )
    .with_source_text(source);
    builder.simulation_context = has_simulation;

    let entry = builder.new_block();
    builder.switch_to(entry);

    let this_id = builder.new_local("__this".to_string(), MirType::ObjectRef);
    builder.scope.insert("__this".to_string(), this_id);
    builder.note_object_qual(this_id, class_name.to_string());
    builder.method_this = Some(this_id);
    builder.restore_formal_proc_captures(this_id, class_name);

    let mut param_locals = Vec::new();
    for (name, field_ty) in constructor_params {
        let ty = mir_type_for_field(*field_ty);
        // Keep ABI locals but do not bind names into scope so body statements
        // resolve attributes via `__this` (matching interpreter semantics).
        let id = builder.new_local(name.clone(), ty);
        param_locals.push((name.clone(), id, *field_ty));
    }
    builder.param_count = 1 + constructor_params.len();

    for (name, elements) in enclosing_switches {
        builder
            .switches
            .insert(name.to_ascii_lowercase(), elements.clone());
    }
    register_switches_in_block(body, &mut builder.switches);
    // Concatenated `$__init` bodies share one label scope; gotos must bind to
    // the innermost (last) label occurrence across the prefix chain (simtst54).
    builder.predeclare_labels_in_block(body);
    builder.predeclare_labels_in_statements(tail_statements);

    for (name, id, field_ty) in &param_locals {
        let offset = constructor_param_offset(&builder, class_name, name)?;
        builder.write_constructor_param_field(this_id, offset, *field_ty, *id, 0..0);
    }

    if runs_on_own_stack {
        // The body is a component: `$__init` only records the constructor
        // parameters, and the body proper becomes the component entry emitted
        // alongside as a helper, to be entered on the object's own stack.
        builder.push(Op::Return { value: None }, 0..0);
        let (init, mut helpers) = builder.finish(entry, None);
        helpers.push(lower_coro_entry(
            class_name,
            body,
            tail_statements,
            enclosing_switches,
            signatures,
            name_param_procs,
            ref_alias_procs,
            layouts,
            classes,
            strings,
            has_simulation,
            source,
        )?);
        return Ok((init, helpers));
    }

    builder.lower_class_init_body(body)?;
    for statement in tail_statements {
        builder.lower_statement(statement)?;
    }
    builder.push(Op::Return { value: None }, 0..0);

    Ok(builder.finish(entry, None))
}

pub(in crate::mir::lower) fn constructor_param_offset(
    builder: &FunctionBuilder<'_>,
    class_name: &str,
    name: &str,
) -> Result<i64, CompileError> {
    builder
        .find_layout(class_name)
        .and_then(|layout| layout.field_offset(name))
        .ok_or_else(|| {
            CompileError::codegen(format!(
                "MIR lowering: internal error: missing field '{name}' for class '{class_name}' init"
            ))
        })
}

/// Lowers a class body as the entry point of its own component: an ordinary
/// linear function taking just the object, ending in the 7.3.4 termination.
/// Suspension needs no cooperation from this function -- `detach` switches
/// stacks and the frame stays where it is.
#[allow(clippy::too_many_arguments)]
pub(in crate::mir::lower) fn lower_coro_entry(
    class_name: &str,
    body: &Block,
    tail_statements: &[Statement],
    enclosing_switches: &HashMap<String, Vec<crate::ast::DesignationalExpr>>,
    signatures: &HashMap<String, ProcSignature>,
    name_param_procs: &HashMap<String, &ProcedureDeclaration>,
    ref_alias_procs: &HashMap<String, &ProcedureDeclaration>,
    layouts: &HashMap<String, ClassLayout>,
    classes: &HashMap<String, ClassDeclaration>,
    strings: &mut Vec<String>,
    has_simulation: bool,
    source: &str,
) -> Result<Function, CompileError> {
    let mut builder = FunctionBuilder::new(
        mangle_coro_entry_name(class_name),
        strings,
        signatures,
        name_param_procs,
        ref_alias_procs,
        layouts,
        classes,
    )
    .with_source_text(source);
    builder.simulation_context = has_simulation;

    let entry = builder.new_block();
    builder.switch_to(entry);

    let this_id = builder.new_local("__this".to_string(), MirType::ObjectRef);
    builder.scope.insert("__this".to_string(), this_id);
    builder.note_object_qual(this_id, class_name.to_string());
    builder.method_this = Some(this_id);
    builder.restore_formal_proc_captures(this_id, class_name);
    builder.param_count = 1;

    for (name, elements) in enclosing_switches {
        builder
            .switches
            .insert(name.to_ascii_lowercase(), elements.clone());
    }
    register_switches_in_block(body, &mut builder.switches);
    builder.predeclare_labels_in_block(body);
    builder.predeclare_labels_in_statements(tail_statements);

    // 12.1's `Process` puts a `detach` ahead of `inner`, so generating a process
    // object yields a detached component rather than running it; that detach
    // comes from the bundled class body via concatenation, not from here.
    let is_process = builder.class_is_scheduled_process(class_name);

    builder.lower_class_init_body(body)?;
    for statement in tail_statements {
        builder.lower_statement(statement)?;
    }
    builder.emit_seq_terminate(this_id, is_process, 0..0)?;

    let (func, helpers) = builder.finish(entry, None);
    if !helpers.is_empty() {
        return Err(CompileError::codegen(format!(
            "MIR lowering: internal error: unexpected helper functions for '{class_name}' component entry"
        )));
    }
    Ok(func)
}

/// Resume the SQS current process by re-entering `Class$__init` for its
/// `class_id`, then cancelling it when the continuation PC is terminated.
pub(in crate::mir::lower) fn build_sim_run_current(
    layouts: &HashMap<String, ClassLayout>,
    signatures: &HashMap<String, ProcSignature>,
    name_param_procs: &HashMap<String, &ProcedureDeclaration>,
    ref_alias_procs: &HashMap<String, &ProcedureDeclaration>,
    classes: &HashMap<String, ClassDeclaration>,
    strings: &mut Vec<String>,
    source: &str,
) -> Result<Function, CompileError> {
    let mut builder = FunctionBuilder::new(
        SIM_RUN_CURRENT.to_string(),
        strings,
        signatures,
        name_param_procs,
        ref_alias_procs,
        layouts,
        classes,
    )
    .with_source_text(source);
    builder.simulation_context = true;
    let entry = builder.new_block();
    builder.switch_to(entry);

    let process = builder.temp(MirType::ObjectRef);
    builder.push(Op::SimCurrent { dest: process }, 0..0);
    let class_id = builder.temp(MirType::I64);
    builder.push(
        Op::ObjectClassIdSafe {
            dest: class_id,
            object: process,
        },
        0..0,
    );

    let done = builder.new_block();
    let bad = builder.new_block();

    let mut resumable: Vec<&ClassLayout> = layouts
        .values()
        .filter(|layout| layout.runs_on_own_stack)
        .collect();
    resumable.sort_by_key(|layout| layout.class_id);

    let mut check_bb = entry;
    for (index, layout) in resumable.iter().enumerate() {
        let match_bb = builder.new_block();
        let next_bb = if index + 1 < resumable.len() {
            builder.new_block()
        } else {
            bad
        };
        if check_bb != entry {
            builder.switch_to(check_bb);
        }
        let expected = builder.temp(MirType::I64);
        builder.push(
            Op::ConstI64 {
                dest: expected,
                value: layout.class_id,
            },
            0..0,
        );
        let eq = builder.temp(MirType::Bool);
        builder.push(
            Op::Compare {
                dest: eq,
                op: CmpOp::Eq,
                left: class_id,
                right: expected,
            },
            0..0,
        );
        builder.push(
            Op::Branch {
                cond: eq,
                then_block: match_bb,
                else_block: next_bb,
            },
            0..0,
        );

        builder.switch_to(match_bb);
        let init_name = mangle_init_name(&layout.name);
        let mut args = Vec::with_capacity(1 + layout.constructor_params.len());
        args.push(process);
        for (_, field_ty) in &layout.constructor_params {
            let placeholder = match field_ty {
                FieldType::I64 => {
                    let id = builder.temp(MirType::I64);
                    builder.push(Op::ConstI64 { dest: id, value: 0 }, 0..0);
                    id
                }
                FieldType::Bool => {
                    let id = builder.temp(MirType::Bool);
                    builder.push(
                        Op::ConstBool {
                            dest: id,
                            value: false,
                        },
                        0..0,
                    );
                    id
                }
                FieldType::F64 => {
                    let id = builder.temp(MirType::F64);
                    builder.push(
                        Op::ConstF64 {
                            dest: id,
                            value: 0.0,
                        },
                        0..0,
                    );
                    id
                }
                FieldType::Text => {
                    let id = builder.temp(MirType::Text);
                    builder.push(Op::TextNotext { dest: id }, 0..0);
                    id
                }
                FieldType::ObjectRef
                | FieldType::ArrayI64
                | FieldType::ArrayBool
                | FieldType::ArrayF64
                | FieldType::ArrayText => {
                    let id = builder.temp(MirType::ObjectRef);
                    builder.push(Op::ConstNone { dest: id }, 0..0);
                    id
                }
            };
            args.push(placeholder);
        }
        builder.push(
            Op::Call {
                dest: None,
                name: init_name,
                args,
            },
            0..0,
        );
        // Termination is handled inside `__init` via `SimCancel` when the last
        // segment completes; nothing else to do here.
        builder.push(Op::Jump { target: done }, 0..0);
        check_bb = next_bb;
    }

    if resumable.is_empty() {
        builder.push(Op::Jump { target: bad }, 0..0);
    }

    builder.switch_to(bad);
    builder.emit_null_object_trap(0..0);
    builder.push(Op::Return { value: None }, 0..0);

    builder.switch_to(done);
    builder.push(Op::Return { value: None }, 0..0);

    let (func, helpers) = builder.finish(entry, None);
    assert!(
        helpers.is_empty(),
        "sim_run_current should not emit name-thunk helpers"
    );
    Ok(func)
}
