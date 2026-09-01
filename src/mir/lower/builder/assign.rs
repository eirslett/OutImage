//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn lower_assignment(
        &mut self,
        assignment: &Assignment,
        span: Span,
    ) -> Result<(), CompileError> {
        if let Variable::Simple(name) = &assignment.lhs {
            if name.eq_ignore_ascii_case("CURRENTLOWTEN")
                || name.eq_ignore_ascii_case("CURRENTDECIMALMARK")
            {
                let value = self.lower_assignment_rhs(&assignment.rhs, span.clone())?;
                if self.local_ty(value) != MirType::I64 {
                    return Err(spanned_error(
                        format!("{name} requires a character value"),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                let env_name = if name.eq_ignore_ascii_case("CURRENTLOWTEN") {
                    "lowten"
                } else {
                    "decimalmark"
                };
                self.push(
                    Op::CallEnv {
                        dest,
                        name: env_name.into(),
                        args: vec![value],
                    },
                    span,
                );
                return Ok(());
            }
            if self
                .constants
                .iter()
                .any(|constant| constant.eq_ignore_ascii_case(name))
            {
                return Err(CompileError::codegen_at(
                    format!("cannot assign to constant '{name}'"),
                    span,
                ));
            }
        }
        match assignment.operator {
            AssignOperator::AssignAlt => {
                let place = self.resolve_place(&assignment.lhs, span.clone())?;
                let value = self.lower_assignment_rhs(&assignment.rhs, span.clone())?;
                match place {
                    Place::Local(id)
                        if self.local_ty(id) == MirType::Text
                            && self.local_ty(value) == MirType::Text =>
                    {
                        self.push(
                            Op::TextRefAssign {
                                dest: id,
                                src: value,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::Local(id)
                        if self.local_ty(id) == MirType::ObjectRef
                            && self.local_ty(value) == MirType::ObjectRef =>
                    {
                        self.push(
                            Op::StoreLocal {
                                local: id,
                                src: value,
                            },
                            span,
                        );
                        self.note_object_qual_from_assign(id, value);
                        Ok(())
                    }
                    Place::RemoteText { object, offset }
                        if self.local_ty(value) == MirType::Text =>
                    {
                        let frame =
                            self.read_place(&Place::RemoteText { object, offset }, span.clone());
                        self.push(
                            Op::TextRefAssign {
                                dest: frame,
                                src: value,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::BasicioImage { object } if self.local_ty(value) == MirType::Text => {
                        self.push(
                            Op::CallBasicioSetImage {
                                object,
                                text: value,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::ArrayElement { array, indices }
                        if self.local_ty(array) == MirType::ArrayText
                            && self.local_ty(value) == MirType::Text =>
                    {
                        // Match interpreter: subscripted `:-` replaces the stored frame.
                        self.push(
                            Op::ArrayStore {
                                array,
                                indices,
                                value,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::ArrayElement { array, indices }
                        if matches!(
                            self.local_ty(array),
                            MirType::ArrayI64 | MirType::ObjectRef
                        ) && self.local_ty(value) == MirType::ObjectRef =>
                    {
                        // `ref(C) array` element `:-` — same i64-slot store ABI.
                        self.push(
                            Op::ArrayStore {
                                array,
                                indices,
                                value,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::RemoteI64 { object, offset }
                        if self.local_ty(value) == MirType::ObjectRef =>
                    {
                        self.push(
                            Op::FieldStoreI64 {
                                object,
                                offset,
                                value,
                                class_qual: None,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::RemoteObject { object, offset, .. }
                        if self.local_ty(value) == MirType::ObjectRef =>
                    {
                        self.push(
                            Op::FieldStoreI64 {
                                object,
                                offset,
                                value,
                                class_qual: None,
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::CaptureCell {
                        object,
                        offset,
                        value_ty: MirType::ObjectRef,
                        ..
                    } if self.local_ty(value) == MirType::ObjectRef => {
                        // Enclosing `ref` shared by pointer with a component on
                        // its own stack (simtst96).
                        let cell = self.capture_cell_pointer(
                            object,
                            offset,
                            MirType::ObjectRef,
                            span.clone(),
                        );
                        self.push(
                            Op::FieldStoreI64 {
                                object: cell,
                                offset: REF_CELL_VALUE_OFFSET,
                                value,
                                class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                            },
                            span,
                        );
                        Ok(())
                    }
                    Place::RemoteI64 { .. }
                    | Place::RemoteBool { .. }
                    | Place::RemoteF64 { .. } => Err(spanned_error(
                        "MIR lowering: reference assignment (':-') to a remote attribute is not supported in the Phase 5 MVP",
                        span,
                    )),
                    _ => Err(spanned_error(
                        "MIR lowering: reference assignment (':-') requires text or object-reference operands",
                        span,
                    )),
                }
            }
            AssignOperator::Assign => {
                let place = self.resolve_place(&assignment.lhs, span.clone())?;
                let value = self.lower_assignment_rhs(&assignment.rhs, span.clone())?;
                // Name formals convert through the formal type first so chained
                // `x := r := s := 3.14` keeps 3.14 even when the actual is integer
                // (DosTestBatch simtst37).
                let lhs_ty = match &assignment.lhs {
                    Variable::Simple(name) if !self.scope_has_name(name) => self
                        .name_formal_ty(name)
                        .unwrap_or_else(|| self.place_ty(&place)),
                    _ => self.place_ty(&place),
                };
                let formal_value = self.coerce_assign_value(lhs_ty, value, span.clone())?;
                let stored = if self.place_ty(&place) == lhs_ty {
                    formal_value
                } else {
                    self.coerce_assign_value(self.place_ty(&place), formal_value, span.clone())?
                };
                self.write_place(&place, stored, span);
                Ok(())
            }
        }
    }

    /// Promotes `src` to `expected` when Simula allows it (integer → real /
    /// long real; real ↔ long real as same bits).
    pub(in crate::mir::lower) fn coerce_value(
        &mut self,
        expected: MirType,
        src: LocalId,
        mismatch_message: impl Into<String>,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let actual = self.local_ty(src);
        if actual == expected {
            return Ok(src);
        }
        if expected == MirType::Bool && actual == MirType::I64 {
            let dest = self.temp(MirType::Bool);
            let zero = self.temp(MirType::I64);
            self.push(
                Op::ConstI64 {
                    dest: zero,
                    value: 0,
                },
                span.clone(),
            );
            self.push(
                Op::Compare {
                    dest,
                    op: CmpOp::Ne,
                    left: src,
                    right: zero,
                },
                span,
            );
            return Ok(dest);
        }
        // Note: Bool → I64 is *not* a general coerce (array indices etc. must
        // stay strict). Name-thunk writes use [`Self::bool_as_i64`] explicitly.
        if expected.is_float() && actual == MirType::I64 {
            return Ok(self.i64_to_float(src, expected, span));
        }
        if expected == MirType::I64 && actual.is_float() {
            return Ok(self.f64_to_i64(src, span));
        }
        if expected.is_float() && actual.is_float() {
            let dest = self.temp(expected);
            self.push(Op::Copy { dest, src }, span);
            return Ok(dest);
        }
        Err(spanned_error(mismatch_message, span))
    }

    /// Materializes a boolean as `0`/`1` i64 (name-thunk / free-cell ABI).
    pub(in crate::mir::lower) fn bool_as_i64(&mut self, cond: LocalId, span: Span) -> LocalId {
        let dest = self.temp(MirType::I64);
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge = self.new_block();
        self.push(
            Op::Branch {
                cond,
                then_block,
                else_block,
            },
            span.clone(),
        );
        self.switch_to(then_block);
        self.push(Op::ConstI64 { dest, value: 1 }, span.clone());
        self.push(Op::Jump { target: merge }, 0..0);
        self.switch_to(else_block);
        self.push(Op::ConstI64 { dest, value: 0 }, span.clone());
        self.push(Op::Jump { target: merge }, 0..0);
        self.switch_to(merge);
        dest
    }

    pub(in crate::mir::lower) fn coerce_assign_value(
        &mut self,
        expected: MirType,
        src: LocalId,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        self.coerce_value(expected, src, "assignment operand types do not match", span)
    }

    /// Lowers the right-hand side of an assignment. For a chained assignment
    /// (`A := B := C`), lowers the inner assignment first and reuses the
    /// value as seen through the inner LHS type (formal type for name params).
    pub(in crate::mir::lower) fn lower_assignment_rhs(
        &mut self,
        rhs: &AssignmentRhs,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        match rhs {
            AssignmentRhs::Expr(expr) => self.lower_expr(expr),
            AssignmentRhs::Chain(inner) => match inner.operator {
                AssignOperator::AssignAlt => {
                    self.lower_assignment(inner, span.clone())?;
                    let place = self.resolve_place(&inner.lhs, span.clone())?;
                    Ok(self.read_place(&place, span))
                }
                AssignOperator::Assign => {
                    let place = self.resolve_place(&inner.lhs, span.clone())?;
                    let value = self.lower_assignment_rhs(&inner.rhs, span.clone())?;
                    let lhs_ty = match &inner.lhs {
                        Variable::Simple(name) if !self.scope_has_name(name) => self
                            .name_formal_ty(name)
                            .unwrap_or_else(|| self.place_ty(&place)),
                        _ => self.place_ty(&place),
                    };
                    let formal_value = self.coerce_assign_value(lhs_ty, value, span.clone())?;
                    let place_ty = self.place_ty(&place);
                    let stored = if place_ty == lhs_ty {
                        formal_value
                    } else {
                        self.coerce_assign_value(place_ty, formal_value, span.clone())?
                    };
                    self.write_place(&place, stored, span.clone());
                    // The outer left part receives the value *of the inner left
                    // part* (§4.1.2), so hand back the target slot rather than
                    // the right-hand side. Re-reading a non-local place would
                    // re-run name-parameter getters, so that case keeps the
                    // already-converted value, and a name formal keeps its
                    // formal-type view.
                    match place {
                        Place::Local(id) if place_ty == lhs_ty => Ok(id),
                        _ => Ok(formal_value),
                    }
                }
            },
        }
    }

    /// Resolves an assignable/readable [`Variable`] to a [`Place`]: either a
    /// plain [`Local`] slot, or one element of a 1-D integer array (Phase 4).
    /// Subscript expressions are lowered here (so both reads and writes get
    /// their index computed exactly once per occurrence).
    pub(in crate::mir::lower) fn resolve_place(
        &mut self,
        variable: &Variable,
        span: Span,
    ) -> Result<Place, CompileError> {
        match variable {
            Variable::Simple(name) => {
                // Body locals declared while inlining shadow name formals
                // (simtst39) and also shadow same-named class attributes on a
                // resumable `__this` (simtst97: `outstate`'s `integer i` must
                // not alias Process parameter `i`, or `for i:=1 … until 10`
                // leaves the attribute at 11 and OOBs `[1:10]` arrays).
                // Otherwise name formals win over enclosing scope bindings
                // (simtst63: formal `x` vs outer `ref array x`).
                if self.is_inline_body_local(name) {
                    if let Some(&id) = self.scope.get(name) {
                        return Ok(Place::Local(id));
                    }
                    if let Some((key, &id)) = self
                        .scope
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    {
                        let _ = key;
                        return Ok(Place::Local(id));
                    }
                }
                // Resumable `$__init` nested `ref` locals are object fields; they
                // must beat plain locals so `Resume(Y)` survives re-entry (simtst76).
                // A promoted field must not hijack an inlined free procedure's
                // lexical name, though: `Real array X(P:1)` makes `X` an
                // attribute of the resumable class, while `P`'s own `x` is the
                // outer `ref(A) x` it captured (simtst74).
                if self.shadowed_enclosing_capture_place(name).is_none()
                    && let Some(place) = self.try_resumable_promoted_field_place(name)
                {
                    return Ok(place);
                }
                if self.name_bindings.contains_key(name)
                    || self
                        .name_bindings
                        .keys()
                        .any(|key| key.eq_ignore_ascii_case(name))
                {
                    return self.resolve_name_actual_place(name, span);
                }
                // Connection block (§4.8): bare names are attributes, shadowing
                // outer locals. Prefer attributes over outer locals kept for
                // enclosing captures, but not over locals declared inside the
                // connection (simtst50 `begin integer i; i := 6`). Gate on
                // `connection_depth` (not `access_level_substitutions`) so
                // method locals like `integer exp` still win (simtst06). Cleared
                // while inlining a free procedure defined outside the connection
                // (simtst73).
                if self.connection_depth > 0 {
                    if let Some((this_id, offset, field_ty, object_qual)) =
                        self.lookup_method_field(name)
                    {
                        let kept_outer = self
                            .connection_kept_outers
                            .iter()
                            .any(|n| n.eq_ignore_ascii_case(name));
                        if !self.scope_has_name(name) || kept_outer {
                            let _ = (this_id, offset, field_ty, object_qual);
                            if let Some(place) = self.method_field_place(name) {
                                return Ok(place);
                            }
                        }
                    }
                }
                if let Some(&id) = self.scope.get(name) {
                    return Ok(Place::Local(id));
                }
                if let Some((key, &id)) = self
                    .scope
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                {
                    let _ = key;
                    return Ok(Place::Local(id));
                }
                if let Some(&(get, set, env)) = self.name_thunks.get(name) {
                    let value_ty = self
                        .name_thunk_tys
                        .get(name)
                        .copied()
                        .or_else(|| {
                            self.name_thunk_tys
                                .iter()
                                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                                .map(|(_, ty)| *ty)
                        })
                        .unwrap_or(MirType::I64);
                    return Ok(Place::NameThunk {
                        get,
                        set,
                        env,
                        value_ty,
                    });
                }
                if let Some(place) = self.shadowed_enclosing_capture_place(name) {
                    return Ok(place);
                }
                // `Suc` / `Pred` are SIMSET *procedure* attributes that stop at
                // the ring's Head; `SUC` / `PRED` are the raw slots backing
                // them. A bare mention in a Link body means the procedure, so
                // `suc == none` terminates at the head instead of walking the
                // ring forever (simtst96).
                if let Some((this_id, offset, _, _)) = self.lookup_method_field(name)
                    && is_simset_method(name)
                    && (offset == SIMSET_SUC_OFFSET || offset == SIMSET_PRED_OFFSET)
                {
                    let value = self.lower_simset_method(this_id, name, &[], span.clone())?;
                    return Ok(Place::Local(value));
                }
                // Inside a class method / inspect chain, bare names that match
                // fields rewrite to remote load/store (connected object first,
                // then enclosing class instances).
                if let Some(place) = self.method_field_place(name) {
                    return Ok(place);
                }
                // Connection / method body on a BASICIO file: bare `image`
                // is the §10.3 image attribute (`inspect sysout do image:-…`).
                if let Some(this_id) = self.method_this
                    && name.eq_ignore_ascii_case("image")
                    && self
                        .ref_qual
                        .get(&this_id)
                        .is_some_and(|qual| is_basicio_class(qual))
                {
                    return Ok(Place::BasicioImage { object: this_id });
                }
                // `sysin`/`sysout` are singleton terminal objects, not scope
                // locals; fetch a fresh handle so `sysout.attr` remote chains
                // (nested or top-level) can resolve like any other object ref.
                if name.eq_ignore_ascii_case("sysin") {
                    let dest = self.temp(MirType::ObjectRef);
                    self.push(Op::CallSysIn { dest }, span);
                    self.note_object_qual(dest, "InFile".into());
                    return Ok(Place::Local(dest));
                }
                if name.eq_ignore_ascii_case("sysout") {
                    let dest = self.temp(MirType::ObjectRef);
                    self.push(Op::CallSysOut { dest }, span);
                    self.note_object_qual(dest, "PrintFile".into());
                    return Ok(Place::Local(dest));
                }
                // Simulation system quantities (§12.1) as remote receivers /
                // bare names (`current.nextev`, `if nextev==none`).
                if self.simulation_context {
                    if name.eq_ignore_ascii_case("current") {
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::SimCurrent { dest }, span.clone());
                        self.note_object_qual(dest, "Process".into());
                        return Ok(Place::Local(dest));
                    }
                    if name.eq_ignore_ascii_case("main") {
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::SimMain { dest }, span.clone());
                        self.note_object_qual(dest, "Process".into());
                        return Ok(Place::Local(dest));
                    }
                    if name.eq_ignore_ascii_case("nextev") {
                        // Parameterless `nextev` procedure (§12.1); MVP stub.
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::ConstNone { dest }, span.clone());
                        self.note_object_qual(dest, "Process".into());
                        return Ok(Place::Local(dest));
                    }
                }
                // Free `image` under the Standard SYSIN/SYSOUT embedding
                // (`IMAGE :- blanks(…)` / `IMAGE = …`).
                if name.eq_ignore_ascii_case("image")
                    && free_basicio_target(name) == Some(FreeBasicioTarget::SysOut)
                {
                    let object = self.temp(MirType::ObjectRef);
                    self.push(Op::CallSysOut { dest: object }, span);
                    self.note_object_qual(object, "PrintFile".into());
                    return Ok(Place::BasicioImage { object });
                }
                Err(spanned_error(format!("undeclared variable '{name}'"), span))
            }
            Variable::Subscripted { name, subscripts } => {
                let array = match self.scope.get(name).copied().or_else(|| {
                    self.scope
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, &id)| id)
                }) {
                    Some(id) => id,
                    None if self.name_bindings.contains_key(name)
                        || self
                            .name_bindings
                            .keys()
                            .any(|key| key.eq_ignore_ascii_case(name)) =>
                    {
                        // Call-by-name array formal: re-evaluate the actual
                        // (typically a simple array identifier) in the caller
                        // environment so element reads/writes alias it.
                        let formal = self
                            .name_bindings
                            .keys()
                            .find(|key| key.eq_ignore_ascii_case(name))
                            .cloned()
                            .unwrap_or_else(|| name.clone());
                        let array = self.lower_name_actual(&formal)?;
                        if !matches!(
                            self.local_ty(array),
                            MirType::ArrayI64
                                | MirType::ArrayF64
                                | MirType::ArrayText
                                | MirType::ObjectRef
                        ) {
                            return Err(spanned_error(
                                format!("call-by-name parameter '{name}' is not an array"),
                                span.clone(),
                            ));
                        }
                        array
                    }
                    None => {
                        // Enclosing array snapshotted onto `__this` (or an outer
                        // inspect/method receiver) as a pointer-sized capture.
                        let Some((this_id, offset, field_ty, elem_qual)) =
                            self.lookup_method_field(name)
                        else {
                            return Err(spanned_error(
                                format!("undeclared variable '{name}'"),
                                span.clone(),
                            ));
                        };
                        let array_ty = match field_ty {
                            FieldType::ArrayText => MirType::ArrayText,
                            FieldType::ArrayF64 => MirType::ArrayF64,
                            FieldType::ArrayI64
                            | FieldType::ArrayBool
                            | FieldType::ObjectRef
                            | FieldType::I64
                            | FieldType::Bool
                            | FieldType::F64
                            | FieldType::Text => MirType::ArrayI64,
                        };
                        let arr = self.temp(array_ty);
                        self.push(
                            Op::FieldLoadI64 {
                                dest: arr,
                                object: this_id,
                                offset,
                                class_qual: None,
                            },
                            span.clone(),
                        );
                        match field_ty {
                            FieldType::ArrayText => {
                                self.note_array_elem_ty(arr, MirType::Text);
                            }
                            FieldType::ArrayF64 => {
                                self.note_array_elem_ty(arr, MirType::F64);
                            }
                            FieldType::ArrayBool => {
                                self.note_array_elem_ty(arr, MirType::Bool);
                            }
                            FieldType::ArrayI64 => {
                                if let Some(qual) = elem_qual {
                                    self.note_array_elem_ty(arr, MirType::ObjectRef);
                                    self.note_array_elem_qual(arr, qual);
                                } else {
                                    self.note_array_elem_ty(arr, MirType::I64);
                                }
                            }
                            _ => {}
                        }
                        arr
                    }
                };
                if !matches!(
                    self.local_ty(array),
                    MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText | MirType::ObjectRef
                ) {
                    return Err(spanned_error(format!("'{name}' is not an array"), span));
                }
                // ObjectRef capture slots hold array descriptors; retype as ArrayI64.
                let array = if self.local_ty(array) == MirType::ObjectRef {
                    let typed = self.temp(MirType::ArrayI64);
                    self.push(
                        Op::Copy {
                            dest: typed,
                            src: array,
                        },
                        span.clone(),
                    );
                    if let Some(elem) = self.array_elem_ty.get(&array).copied() {
                        self.note_array_elem_ty(typed, elem);
                    }
                    if let Some(qual) = self.array_elem_qual.get(&array).cloned() {
                        self.note_array_elem_qual(typed, qual);
                    }
                    typed
                } else {
                    array
                };
                if subscripts.is_empty() {
                    return Err(spanned_error(
                        "array subscript list must not be empty",
                        span,
                    ));
                }
                let mut indices = Vec::with_capacity(subscripts.len());
                for subscript in subscripts {
                    let index = self.lower_expr(subscript)?;
                    let index = self.coerce_value(
                        MirType::I64,
                        index,
                        "array subscript must be an integer expression",
                        subscript.span.clone(),
                    )?;
                    indices.push(index);
                }
                Ok(Place::ArrayElement { array, indices })
            }
            Variable::Qua { object, class_name } => {
                let object_place = self.resolve_place(object, span.clone())?;
                let object_id = self.read_place(&object_place, span.clone());
                if self.local_ty(object_id) != MirType::ObjectRef {
                    return Err(spanned_error(
                        format!(
                            "qua requires object reference, found {}",
                            self.locals[object_id.0].ty
                        ),
                        span.clone(),
                    ));
                }
                let target_name = self
                    .find_layout(class_name)
                    .ok_or_else(|| {
                        spanned_error(
                            format!("MIR lowering: undefined class '{class_name}'"),
                            span.clone(),
                        )
                    })?
                    .name
                    .clone();
                let dest = self.temp(MirType::ObjectRef);
                self.push(
                    Op::Copy {
                        dest,
                        src: object_id,
                    },
                    span.clone(),
                );
                if let Some(instance) = self.instance_layout_name(object_id) {
                    self.set_local_class_qual(dest, instance);
                }
                self.ref_qual.insert(dest, target_name);
                Ok(Place::Local(dest))
            }
            Variable::Remote { object, attribute } => {
                let object_place = self.resolve_place(object, span.clone())?;
                let object_id = match object_place {
                    Place::Local(id) => id,
                    other => {
                        // Materialize a capture-field / remote object into a
                        // temp so `x.i` works when `x` itself is an enclosing
                        // ObjectRef snapshotted onto `__this`.
                        let value = self.read_place(&other, span.clone());
                        if self.local_ty(value) != MirType::ObjectRef
                            && self.local_ty(value) != MirType::Text
                        {
                            return Err(spanned_error(
                                "MIR lowering: nested remote attribute access is not supported in the Phase 5 MVP",
                                span,
                            ));
                        }
                        value
                    }
                };
                match self.local_ty(object_id) {
                    MirType::Text => {
                        if attribute.eq_ignore_ascii_case("main") {
                            return Ok(Place::TextMain { frame: object_id });
                        }
                        // Other text attributes are expression-only (see
                        // `lower_text_attribute`); they are not assignable places.
                        Err(spanned_error(
                            format!(
                                "MIR lowering: text attribute '{attribute}' cannot be an assignment target"
                            ),
                            span,
                        ))
                    }
                    MirType::ObjectRef => {
                        if attribute.eq_ignore_ascii_case("image") && is_basicio_method(attribute) {
                            return Ok(Place::BasicioImage { object: object_id });
                        }
                        // Other BASICIO / simset synthetic attributes
                        // (`pos`, `isopen`, `suc`, …) have no backing field
                        // offset; read them as a value and wrap the result
                        // as a plain local. This is correct for reads and
                        // for nested chains (`sysout.line.something`); they
                        // are not otherwise valid assignment targets.
                        if is_simset_method(attribute) {
                            let value =
                                self.lower_simset_method(object_id, attribute, &[], span.clone())?;
                            return Ok(Place::Local(value));
                        }
                        if is_basicio_method(attribute) {
                            let value =
                                self.lower_basicio_method(object_id, attribute, &[], span.clone())?;
                            return Ok(Place::Local(value));
                        }
                        if let Some(value) = self.try_lower_process_builtin_attribute(
                            object_id,
                            attribute,
                            span.clone(),
                        )? {
                            return Ok(Place::Local(value));
                        }
                        if let Some(value) =
                            self.try_lower_parameterless_method(object_id, attribute, span.clone())?
                        {
                            return Ok(Place::Local(value));
                        }
                        self.remote_object_field_place(object_id, attribute, span)
                    }
                    _ => Err(spanned_error(
                        "MIR lowering: remote attribute access requires an object reference or text value",
                        span,
                    )),
                }
            }
            Variable::RemoteCall {
                object,
                attribute,
                arguments,
            } => {
                // `t.sub(i,n)` / `obj.t.sub(i,n)` / capture-field `timage.sub(...)`
                // as an assignment target (text substring content assign).
                if TextIntrinsic::parse(attribute) == Some(TextIntrinsic::Sub) {
                    let object_place = self.resolve_place(object, span.clone())?;
                    if self.place_ty(&object_place) == MirType::Text {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!("sub expects 2 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let frame = self.read_place(&object_place, span.clone());
                        let start = self.lower_expr(&arguments[0])?;
                        let start = self.coerce_value(
                            MirType::I64,
                            start,
                            "sub requires integer arguments",
                            arguments[0].span.clone(),
                        )?;
                        let length = self.lower_expr(&arguments[1])?;
                        let length = self.coerce_value(
                            MirType::I64,
                            length,
                            "sub requires integer arguments",
                            arguments[1].span.clone(),
                        )?;
                        return Ok(Place::TextSub {
                            frame,
                            start,
                            length,
                        });
                    }
                }
                let text_frame = if let Variable::Simple(name) = object.as_ref() {
                    self.scope
                        .get(name)
                        .copied()
                        .filter(|&id| self.local_ty(id) == MirType::Text)
                } else {
                    None
                };
                // Side-effecting text procedures used as statement exprs go
                // through `ExprKind::RemoteCall`; as an assignment LHS they
                // are unsupported in the native backend.
                if text_frame.is_some() && TextIntrinsic::parse(attribute).is_some() {
                    let _ = arguments;
                    return Err(spanned_error(
                        format!(
                            "MIR lowering: text procedure '{attribute}' cannot be an assignment target"
                        ),
                        span,
                    ));
                }
                Err(spanned_error(
                    "MIR lowering: remote procedure call cannot be an assignment target in the Phase 5 MVP",
                    span,
                ))
            }
        }
    }

    /// Resolves `attribute` as a declared field of the already-materialized
    /// object `object_id`. Callers that evaluated the object expression
    /// themselves use this instead of re-resolving the whole `obj.attr`
    /// variable, so a side-effecting object expression (a name-bound type
    /// procedure such as `y` :- `x.Z`) is evaluated exactly once.
    pub(in crate::mir::lower) fn remote_object_field_place(
        &mut self,
        object_id: LocalId,
        attribute: &str,
        span: Span,
    ) -> Result<Place, CompileError> {
        let (offset, field_ty) = self.field_info_for(object_id, attribute, span.clone())?;
        // Array attributes: materialize the descriptor as a typed local so
        // element access keeps Bool/Text/ref element metadata (RemoteObject
        // loads as ObjectRef).
        if matches!(
            field_ty,
            FieldType::ArrayI64 | FieldType::ArrayBool | FieldType::ArrayF64 | FieldType::ArrayText
        ) {
            let dest = self.temp(mir_type_for_field(field_ty));
            self.push(
                Op::FieldLoadI64 {
                    dest,
                    object: object_id,
                    offset,
                    class_qual: None,
                },
                span.clone(),
            );
            self.annotate_loaded_field(dest, object_id, attribute, field_ty);
            return Ok(Place::Local(dest));
        }
        let object_qual = self
            .ref_qual
            .get(&object_id)
            .and_then(|class| self.attribute_object_qual(class, attribute));
        Ok(remote_place(object_id, offset, field_ty, object_qual))
    }

    pub(in crate::mir::lower) fn place_ty(&self, place: &Place) -> MirType {
        match place {
            Place::Local(id) => self.local_ty(*id),
            Place::ArrayElement { array, .. } => {
                self.array_elem_ty
                    .get(array)
                    .copied()
                    .unwrap_or(match self.local_ty(*array) {
                        MirType::ArrayText => MirType::Text,
                        MirType::ArrayF64 => MirType::F64,
                        _ => MirType::I64,
                    })
            }
            Place::RemoteI64 { .. } => MirType::I64,
            Place::RemoteObject { .. } => MirType::ObjectRef,
            Place::RemoteBool { .. } => MirType::Bool,
            Place::RemoteF64 { .. } => MirType::F64,
            Place::RemoteText { .. } => MirType::Text,
            Place::CaptureCell { value_ty, .. } => *value_ty,
            Place::NameThunk { value_ty, .. } => *value_ty,
            Place::TextMain { .. } | Place::TextSub { .. } | Place::BasicioImage { .. } => {
                MirType::Text
            }
        }
    }

    /// Loads the pointer a by-reference capture slot holds, so reads and writes
    /// reach the variable's declaring frame instead of a copy of its value.
    pub(in crate::mir::lower) fn capture_cell_pointer(
        &mut self,
        object: LocalId,
        offset: i64,
        value_ty: MirType,
        span: Span,
    ) -> LocalId {
        let cell = if value_ty == MirType::ObjectRef {
            let cell = self.temp(MirType::ObjectRef);
            self.note_object_qual(cell, REF_CELL_CLASS_NAME.to_string());
            cell
        } else {
            self.temp(MirType::RefI64)
        };
        self.push(
            Op::FieldLoadI64 {
                dest: cell,
                object,
                offset,
                class_qual: None,
            },
            span,
        );
        cell
    }

    /// Reads a [`Place`]'s current value. Plain locals double as their own
    /// SSA-style storage slot (see the module docs), so reading one is free;
    /// an array element or remote field needs an explicit load into a fresh temp.
    pub(in crate::mir::lower) fn read_place(&mut self, place: &Place, span: Span) -> LocalId {
        match place {
            Place::Local(id) => *id,
            Place::ArrayElement { array, indices } => {
                let dest = self.temp(self.place_ty(place));
                self.push(
                    Op::ArrayLoad {
                        dest,
                        array: *array,
                        indices: indices.clone(),
                    },
                    span.clone(),
                );
                if let Some(qual) = self.array_elem_qual.get(array).cloned() {
                    self.note_object_qual(dest, qual);
                }
                dest
            }
            Place::CaptureCell {
                object,
                offset,
                value_ty,
                qual,
            } => {
                let cell = self.capture_cell_pointer(*object, *offset, *value_ty, span.clone());
                let dest = self.temp(*value_ty);
                if *value_ty == MirType::ObjectRef {
                    self.push(
                        Op::FieldLoadI64 {
                            dest,
                            object: cell,
                            offset: REF_CELL_VALUE_OFFSET,
                            class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                        },
                        span,
                    );
                } else {
                    self.push(
                        Op::LoadRefI64 {
                            dest,
                            ptr: cell,
                            offset: 0,
                        },
                        span,
                    );
                }
                if let Some(qual) = qual {
                    self.note_object_qual(dest, qual.clone());
                }
                dest
            }
            Place::RemoteI64 { object, offset } => {
                let dest = self.temp(MirType::I64);
                self.push(
                    Op::FieldLoadI64 {
                        dest,
                        object: *object,
                        offset: *offset,
                        class_qual: None,
                    },
                    span,
                );
                dest
            }
            Place::RemoteObject {
                object,
                offset,
                qual,
            } => {
                let dest = self.temp(MirType::ObjectRef);
                self.push(
                    Op::FieldLoadI64 {
                        dest,
                        object: *object,
                        offset: *offset,
                        class_qual: None,
                    },
                    span,
                );
                if let Some(qual) = qual {
                    self.note_object_qual(dest, qual.clone());
                }
                dest
            }
            Place::RemoteBool { object, offset } => {
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::FieldLoadI64 {
                        dest,
                        object: *object,
                        offset: *offset,
                        class_qual: None,
                    },
                    span,
                );
                dest
            }
            Place::RemoteText { object, offset } => {
                let dest = self.temp(MirType::Text);
                self.push(
                    Op::FieldLoadI64 {
                        dest,
                        object: *object,
                        offset: *offset,
                        class_qual: None,
                    },
                    span,
                );
                dest
            }
            Place::RemoteF64 { object, offset } => {
                let dest = self.temp(MirType::F64);
                self.push(
                    Op::FieldLoadI64 {
                        dest,
                        object: *object,
                        offset: *offset,
                        class_qual: None,
                    },
                    span,
                );
                dest
            }
            Place::NameThunk {
                get, env, value_ty, ..
            } => {
                let raw = self.temp(MirType::I64);
                self.push(
                    Op::CallIndirect {
                        dest: Some(raw),
                        callee: *get,
                        args: vec![*env],
                        sig: CallSig {
                            params: vec![self.local_ty(*env)],
                            result: Some(MirType::I64),
                        },
                    },
                    span.clone(),
                );
                if *value_ty == MirType::Bool {
                    let dest = self.temp(MirType::Bool);
                    let zero = self.temp(MirType::I64);
                    self.push(
                        Op::ConstI64 {
                            dest: zero,
                            value: 0,
                        },
                        span.clone(),
                    );
                    self.push(
                        Op::Compare {
                            dest,
                            op: CmpOp::Ne,
                            left: raw,
                            right: zero,
                        },
                        span,
                    );
                    dest
                } else {
                    raw
                }
            }
            Place::TextMain { frame } => {
                let dest = self.temp(MirType::Text);
                self.push(
                    Op::TextMain {
                        dest,
                        frame: *frame,
                    },
                    span,
                );
                dest
            }
            Place::TextSub {
                frame,
                start,
                length,
            } => {
                let dest = self.temp(MirType::Text);
                self.push(
                    Op::TextSub {
                        dest,
                        frame: *frame,
                        i: *start,
                        n: *length,
                    },
                    span,
                );
                dest
            }
            Place::BasicioImage { object } => {
                let dest = self.temp(MirType::Text);
                self.push(
                    Op::CallBasicioImage {
                        dest,
                        object: *object,
                    },
                    span,
                );
                dest
            }
        }
    }

    /// Store a constructor actual into an instance field. Text parameters are
    /// always initialized via `copy(AP)` (Standard §4.6.2), matching procedure
    /// value-text formals — even when the class formal is reference mode, the
    /// Phase 5 MVP shares that lowering so later `:=` into the attribute is safe.
    pub(in crate::mir::lower) fn write_constructor_param_field(
        &mut self,
        this_id: LocalId,
        offset: i64,
        field_ty: FieldType,
        value: LocalId,
        span: Span,
    ) {
        if field_ty == FieldType::Text {
            let copied = self.temp(MirType::Text);
            self.push(
                Op::TextCopy {
                    dest: copied,
                    src: value,
                },
                span.clone(),
            );
            self.push(
                Op::FieldStoreI64 {
                    object: this_id,
                    offset,
                    value: copied,
                    class_qual: None,
                },
                span,
            );
            return;
        }
        self.write_place(&remote_place(this_id, offset, field_ty, None), value, span);
    }

    pub(in crate::mir::lower) fn write_place(&mut self, place: &Place, value: LocalId, span: Span) {
        match place {
            Place::Local(id) if self.local_ty(*id) == MirType::Text => {
                self.push(
                    Op::TextAssign {
                        dest: *id,
                        src: value,
                    },
                    span,
                );
            }
            Place::Local(id) => self.push(
                Op::StoreLocal {
                    local: *id,
                    src: value,
                },
                span,
            ),
            Place::CaptureCell {
                object,
                offset,
                value_ty,
                ..
            } => {
                let cell = self.capture_cell_pointer(*object, *offset, *value_ty, span.clone());
                if *value_ty == MirType::ObjectRef {
                    self.push(
                        Op::FieldStoreI64 {
                            object: cell,
                            offset: REF_CELL_VALUE_OFFSET,
                            value,
                            class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                        },
                        span,
                    );
                } else {
                    self.push(
                        Op::StoreRefI64 {
                            ptr: cell,
                            src: value,
                            offset: 0,
                        },
                        span,
                    );
                }
            }
            Place::NameThunk {
                set, env, value_ty, ..
            } => {
                let stored = if matches!(*value_ty, MirType::Bool | MirType::I64)
                    && self.local_ty(value) == MirType::Bool
                {
                    self.bool_as_i64(value, span.clone())
                } else {
                    value
                };
                self.push(
                    Op::CallIndirect {
                        dest: None,
                        callee: *set,
                        args: vec![*env, stored],
                        sig: CallSig {
                            params: vec![self.local_ty(*env), MirType::I64],
                            result: None,
                        },
                    },
                    span,
                );
            }
            Place::ArrayElement { array, indices } => {
                // Text `:=` must deep-copy into a fresh frame; `array_store_text`
                // only clones the descriptor (for `:-` alias safety).
                let value = if self.local_ty(*array) == MirType::ArrayText {
                    let copied = self.temp(MirType::Text);
                    self.push(
                        Op::TextCopy {
                            dest: copied,
                            src: value,
                        },
                        span.clone(),
                    );
                    copied
                } else {
                    value
                };
                self.push(
                    Op::ArrayStore {
                        array: *array,
                        indices: indices.clone(),
                        value,
                    },
                    span,
                );
            }
            Place::RemoteI64 { object, offset } => self.push(
                Op::FieldStoreI64 {
                    object: *object,
                    offset: *offset,
                    value,
                    class_qual: None,
                },
                span,
            ),
            Place::RemoteObject { object, offset, .. } => self.push(
                Op::FieldStoreI64 {
                    object: *object,
                    offset: *offset,
                    value,
                    class_qual: None,
                },
                span,
            ),
            Place::RemoteBool { object, offset } => self.push(
                Op::FieldStoreI64 {
                    object: *object,
                    offset: *offset,
                    value,
                    class_qual: None,
                },
                span,
            ),
            Place::RemoteF64 { object, offset } => self.push(
                Op::FieldStoreI64 {
                    object: *object,
                    offset: *offset,
                    value,
                    class_qual: None,
                },
                span,
            ),
            Place::RemoteText { object, offset } => {
                // Simula `:=` mutates the existing text frame in place.
                let frame = self.read_place(
                    &Place::RemoteText {
                        object: *object,
                        offset: *offset,
                    },
                    span.clone(),
                );
                self.push(
                    Op::TextAssign {
                        dest: frame,
                        src: value,
                    },
                    span,
                );
            }
            Place::TextMain { frame } => {
                let view = self.temp(MirType::Text);
                self.push(
                    Op::TextMain {
                        dest: view,
                        frame: *frame,
                    },
                    span.clone(),
                );
                self.push(
                    Op::TextAssign {
                        dest: view,
                        src: value,
                    },
                    span,
                );
            }
            Place::TextSub {
                frame,
                start,
                length,
            } => {
                let view = self.temp(MirType::Text);
                self.push(
                    Op::TextSub {
                        dest: view,
                        frame: *frame,
                        i: *start,
                        n: *length,
                    },
                    span.clone(),
                );
                self.push(
                    Op::TextAssign {
                        dest: view,
                        src: value,
                    },
                    span,
                );
            }
            Place::BasicioImage { object } => {
                self.push(
                    Op::CallBasicioSetImage {
                        object: *object,
                        text: value,
                    },
                    span,
                );
            }
        }
    }
}
