//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn lower_declaration(
        &mut self,
        decl: &Declaration,
    ) -> Result<(), CompileError> {
        let ty = mir_type_for(&decl.ty)?;
        let object_qual = match &decl.ty {
            Type::ObjectRef(qual) => Some(qual.clone()),
            _ => None,
        };
        for item in &decl.items {
            let id = self.new_local(item.name.clone(), ty);
            self.scope.insert(item.name.clone(), id);
            self.note_inline_body_local(&item.name);
            if item.is_constant {
                self.constants.insert(item.name.clone());
            }
            if let Some(qual) = &object_qual {
                self.note_object_qual(id, qual.clone());
            }
            if let Some(initializer) = &item.initializer {
                // Text locals must exist as notext frames before `:=` can
                // assign into them (matching interpreter `assign_value_from`
                // when the destination is notext).
                if ty == MirType::Text {
                    self.push(Op::TextNotext { dest: id }, 0..0);
                }
                let value = self.lower_expr(initializer)?;
                let value = self.coerce_value(
                    ty,
                    value,
                    format!(
                        "initializer for '{}' has the wrong type (expected {ty})",
                        item.name
                    ),
                    initializer.span.clone(),
                )?;
                match ty {
                    MirType::Text => {
                        self.push(
                            Op::TextAssign {
                                dest: id,
                                src: value,
                            },
                            initializer.span.clone(),
                        );
                    }
                    _ => {
                        self.push(
                            Op::StoreLocal {
                                local: id,
                                src: value,
                            },
                            initializer.span.clone(),
                        );
                    }
                }
            } else {
                match ty {
                    MirType::I64 => self.push(Op::ConstI64 { dest: id, value: 0 }, 0..0),
                    MirType::Bool => self.push(
                        Op::ConstBool {
                            dest: id,
                            value: false,
                        },
                        0..0,
                    ),
                    MirType::F64 | MirType::LongF64 => self.push(
                        Op::ConstF64 {
                            dest: id,
                            value: 0.0,
                        },
                        0..0,
                    ),
                    MirType::Text => self.push(Op::TextNotext { dest: id }, 0..0),
                    MirType::ObjectRef => self.push(Op::ConstNone { dest: id }, 0..0),
                    // Arrays go through `lower_array_declaration` instead.
                    MirType::ArrayI64
                    | MirType::ArrayF64
                    | MirType::ArrayText
                    | MirType::RefI64
                    | MirType::FuncRef => {
                        unreachable!("scalar declarations never produce array/ref-pointer types")
                    }
                }
            }
        }
        Ok(())
    }

    /// A subblock or prefixed block declaring a class is a quasi-parallel
    /// system, and the block instance is its main component (7.2). Procedure
    /// and class bodies are neither, so they lower through
    /// [`Self::lower_block_body`] directly.
    pub(in crate::mir::lower) fn lower_block(&mut self, block: &Block) -> Result<(), CompileError> {
        if !self.block_heads_a_system(block) {
            return self.lower_block_body(block);
        }
        let system = self.temp(MirType::RefI64);
        self.push(
            Op::SeqSystemEnter {
                dest: system,
                block: crate::layout::system_head_id(block),
            },
            0..0,
        );
        let lowered = self.lower_block_body(block);
        self.push(Op::SeqSystemExit { system }, 0..0);
        lowered
    }

    pub(in crate::mir::lower) fn block_heads_a_system(&self, block: &Block) -> bool {
        block
            .classes
            .iter()
            .any(|class| self.class_runs_on_own_stack(declared_class_name(&class.name)))
    }

    pub(in crate::mir::lower) fn lower_block_body(
        &mut self,
        block: &Block,
    ) -> Result<(), CompileError> {
        if block_is_simulation_prefixed(&block.prefix) {
            return self.lower_simulation_block(block);
        }
        if let Some(prefix) = &block.prefix {
            return self.lower_prefixed_block(block, prefix);
        }
        let scope_restore = if block_is_decl_prefix_only(block) {
            // Expanded detach-procedure prefixes (simtst69 `P1`): ref locals must
            // survive across resumable `$__init` segment boundaries.
            Vec::new()
        } else {
            self.enter_nested_block_scope(block)
        };
        for switch in &block.switches {
            self.switches
                .insert(switch.name.to_ascii_lowercase(), switch.elements.clone());
        }
        for decl in &block.declarations {
            self.lower_declaration(decl)?;
        }
        for array in &block.arrays {
            self.lower_array_declaration(array)?;
        }
        for statement in &block.statements {
            self.lower_statement(statement)?;
        }
        for inner in &block.body {
            if inner.prefix.is_some() {
                self.lower_block(inner)?;
            } else {
                let pushed = self.enter_block_debug_scope(
                    block_debug_name(inner),
                    block_source_span(inner),
                    inner,
                );
                self.lower_block(inner)?;
                if pushed {
                    self.pop_debug_scope();
                }
            }
        }
        if !block_is_decl_prefix_only(block) {
            self.exit_nested_block_scope(scope_restore);
        }
        Ok(())
    }

    /// Snapshot outer bindings for names declared in `block` so nested
    /// `begin`…`end` scopes restore correctly after the block ends.
    pub(in crate::mir::lower) fn enter_nested_block_scope(
        &mut self,
        block: &Block,
    ) -> Vec<(String, Option<LocalId>)> {
        let mut restore = Vec::new();
        for decl in &block.declarations {
            for item in &decl.items {
                let previous = self.scope.get(&item.name).copied().or_else(|| {
                    self.scope
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(&item.name))
                        .map(|(_, id)| *id)
                });
                restore.push((item.name.clone(), previous));
            }
        }
        for array in &block.arrays {
            for segment in &array.segments {
                for name in &segment.names {
                    let previous = self.scope.get(name).copied().or_else(|| {
                        self.scope
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case(name))
                            .map(|(_, id)| *id)
                    });
                    restore.push((name.clone(), previous));
                }
            }
        }
        restore
    }

    pub(in crate::mir::lower) fn exit_nested_block_scope(
        &mut self,
        restore: Vec<(String, Option<LocalId>)>,
    ) {
        for (name, previous) in restore.into_iter().rev() {
            match previous {
                Some(id) => {
                    self.scope.insert(name, id);
                }
                None => {
                    self.scope.remove(&name);
                }
            }
        }
    }

    /// Prefixed block (§4.10.1): allocate an anonymous object of the prefix
    /// class, then lower prefix body + block statements as one additional
    /// main part (same CFG) so virtual labels match `goto` from the prefix.
    pub(in crate::mir::lower) fn lower_prefixed_block(
        &mut self,
        block: &Block,
        prefix: &Expr,
    ) -> Result<(), CompileError> {
        let span = prefix.span.clone();
        let (class_name, arguments) = match &prefix.kind {
            ExprKind::Variable(Variable::Simple(name)) => (name.as_str(), &[][..]),
            ExprKind::FunctionCall { name, arguments } => (name.as_str(), arguments.as_slice()),
            ExprKind::This(_) => {
                return Err(spanned_error(
                    "'this' is not permitted as a block prefix",
                    span,
                ));
            }
            _ => {
                return Err(spanned_error(
                    "block prefix must be a class identifier or class generator",
                    span,
                ));
            }
        };
        // Skip Class$__init: its body is concatenated below into this function.
        // Labels from the class prefix must share a scope with the block body
        // (`goto L` ↔ `L:`), but must not collide with the enclosing function's
        // labels (simtst98: main `goto L` vs class `a`'s `L:` after `detach`).
        let object = self.lower_new_object_ex(class_name, arguments, span.clone(), false)?;
        // The block instance runs here, in place: it is the main component of
        // whatever system it heads, never a component of its own.
        self.push(Op::SeqBlockInstance { object }, span.clone());
        let prefix_class = self.concatenated_class(class_name)?;
        let (initial, finals) = concatenate::prefixed_block_statements(&prefix_class, block);
        let block_stmt_count = block.statements.len();
        let prefix_initial_len = initial.len().saturating_sub(block_stmt_count);
        let (prefix_initial, block_initial) = initial.split_at(prefix_initial_len);
        let saved_prefixed = self.prefixed_block_access.clone();
        let saved_prefixed_procs = std::mem::take(&mut self.prefixed_block_procs);
        self.prefixed_block_access = Some(class_name.to_string());
        for procedure in &block.procedures {
            self.prefixed_block_procs
                .insert(procedure.name.to_ascii_lowercase());
        }
        let pushed = self.enter_block_debug_scope(
            class_name.to_string(),
            block_source_span_with_prefix(block, prefix),
            block,
        );
        let result = self.with_fresh_label_scope_predeclare(
            |this| {
                this.predeclare_labels_in_statements(prefix_initial);
                this.predeclare_labels_in_statements(block_initial);
                for inner in &block.body {
                    this.predeclare_labels_in_block(inner);
                }
                this.predeclare_labels_in_statements(&finals);
            },
            |this| {
                this.with_connection_this(object, class_name, |this| {
                    for switch in &block.switches {
                        this.switches
                            .insert(switch.name.to_ascii_lowercase(), switch.elements.clone());
                    }
                    for decl in &block.declarations {
                        this.lower_declaration(decl)?;
                    }
                    for array in &block.arrays {
                        this.lower_array_declaration(array)?;
                    }
                    // Class array attributes and `integer i=12` stores normally
                    // live in `$__init`; prefixed blocks skip that helper.
                    this.lower_class_array_attrs(&prefix_class.body)?;
                    this.emit_attribute_initializers_tree(&prefix_class.body)?;
                    for statement in prefix_initial {
                        this.lower_statement(statement)?;
                    }
                    for inner in &block.body {
                        if inner.prefix.is_some() {
                            this.lower_block(inner)?;
                        } else {
                            let pushed = this.enter_block_debug_scope(
                                block_debug_name(inner),
                                block_source_span(inner),
                                inner,
                            );
                            this.lower_block(inner)?;
                            if pushed {
                                this.pop_debug_scope();
                            }
                        }
                    }
                    for statement in block_initial {
                        this.lower_statement(statement)?;
                    }
                    for statement in &finals {
                        this.lower_statement(statement)?;
                    }
                    this.writeback_enclosing_captures(object, class_name, &[], span.clone())?;
                    Ok(())
                })
            },
        );
        if pushed {
            self.pop_debug_scope();
        }
        self.prefixed_block_access = saved_prefixed;
        self.prefixed_block_procs = saved_prefixed_procs;
        result
    }

    /// Prefix-merged class declarations for the program. [`Self::classes`] is
    /// raw (pre-concatenation); remote attribute storage names and protection
    /// maps live on the concatenated view (§5.5.2 / simtst48 `qua`).
    pub(in crate::mir::lower) fn concatenated_class_map(
        &self,
    ) -> HashMap<String, ClassDeclaration> {
        let raw: Vec<ClassDeclaration> = self.classes.values().cloned().collect();
        concatenate::concatenate_classes(&raw).unwrap_or_default()
    }

    /// Concatenated class declaration for `name` (prefix chain merged).
    pub(in crate::mir::lower) fn concatenated_class(
        &self,
        name: &str,
    ) -> Result<ClassDeclaration, CompileError> {
        self.concatenated_class_map()
            .into_iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, class)| class)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "MIR lowering: undefined class '{name}' for prefixed block"
                ))
            })
    }

    /// Storage name for remote / connection access of `attribute` at `qual`,
    /// following the concatenated class's identifier-substitution chain.
    pub(in crate::mir::lower) fn remote_storage_name(&self, qual: &str, attribute: &str) -> String {
        let Ok(class) = self.concatenated_class(qual) else {
            return attribute.to_string();
        };
        let mut name = attribute.to_string();
        for _ in 0..64 {
            let Some(next) = class
                .identifier_substitutions
                .iter()
                .find(|(from, _)| from.eq_ignore_ascii_case(&name))
                .map(|(_, to)| to.clone())
            else {
                break;
            };
            if next.eq_ignore_ascii_case(&name) {
                break;
            }
            name = next;
        }
        name
    }

    /// Whether a caller local is bit-compatible with an enclosing-capture field.
    pub(in crate::mir::lower) fn capture_local_compatible(
        local_ty: MirType,
        field_ty: FieldType,
    ) -> bool {
        let expected = mir_type_for_field(field_ty);
        local_ty == expected
            || matches!(
                (local_ty, expected),
                (MirType::F64, MirType::LongF64) | (MirType::LongF64, MirType::F64)
            )
            || (expected == MirType::ObjectRef
                && matches!(
                    local_ty,
                    MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
                ))
    }

    pub(in crate::mir::lower) fn enclosing_capture_slots(
        &self,
        class_name: &str,
    ) -> Vec<(String, i64, FieldType)> {
        self.find_layout(class_name)
            .map(|layout| {
                layout
                    .enclosing_captures
                    .iter()
                    .filter_map(|(name, field_ty)| {
                        layout
                            .field_offset(name)
                            .map(|offset| (name.clone(), offset, *field_ty))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve a capture source to a SSA local: prefer a same-named scope
    /// binding, else load the field from the method/`inspect` receiver chain
    /// (outlined methods like `X$E` keep outer `seq`/`seqi` only as `__this`
    /// fields — `new F` must still snapshot them; simtst62).
    /// Resolve a capture source to a SSA local: prefer a same-named scope
    /// binding, else load the *capture field itself* from the method/`inspect`
    /// receiver chain (outlined methods like `X$E` keep outer `seq`/`seqi` only
    /// as `__this` fields — `new F` must still snapshot them; simtst62).
    ///
    /// Never fall back to a same-named plain attribute: class `A` with both
    /// attribute `i` and `__simrt_encl_i` (outer `i`) must not refresh the
    /// capture from `A.i` (simtst73).
    pub(in crate::mir::lower) fn capture_source_value(
        &mut self,
        source_name: &str,
        capture_field_name: &str,
        field_ty: FieldType,
        span: Span,
    ) -> Option<LocalId> {
        if let Some(src) = self.scope_lookup(source_name)
            && !self.local_is_current_formal(src)
        {
            if Self::capture_local_compatible(self.local_ty(src), field_ty) {
                return Some(src);
            }
            return None;
        }
        let (this_id, offset, src_ty, _) = self.lookup_method_field(capture_field_name)?;
        if !Self::capture_local_compatible(mir_type_for_field(src_ty), field_ty) {
            return None;
        }
        let tmp = self.temp(mir_type_for_field(field_ty));
        if self.object_field_is_by_ref_capture(this_id, capture_field_name, src_ty, offset) {
            // The enclosing instance shares this variable by pointer (a class
            // on its own stack, see [`Self::capture_by_reference`]): the slot
            // holds the home's address, so the *value* is one load further in.
            let value_ty = mir_type_for_field(field_ty);
            let cell = self.capture_cell_pointer(this_id, offset, value_ty, span.clone());
            if value_ty == MirType::ObjectRef {
                self.push(
                    Op::FieldLoadI64 {
                        dest: tmp,
                        object: cell,
                        offset: REF_CELL_VALUE_OFFSET,
                        class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                    },
                    span,
                );
            } else {
                self.push(
                    Op::LoadRefI64 {
                        dest: tmp,
                        ptr: cell,
                        offset: 0,
                    },
                    span,
                );
            }
            return Some(tmp);
        }
        self.push(
            Op::FieldLoadI64 {
                dest: tmp,
                object: this_id,
                offset,
                class_qual: None,
            },
            span,
        );
        Some(tmp)
    }

    /// Enclosing **scalars** are shared by reference rather than copied onto
    /// each instance: §5.5 makes an enclosing variable *one* variable, and a
    /// component parked on its own stack cannot have its copy refreshed (or
    /// write back after every transfer), so the capture slot holds a pointer
    /// to the declaring frame's cell instead.
    ///
    /// `ref` captures are **value snapshots** for every class that does not run
    /// on its own stack: an ObjectRef slot holds a live object reference (a
    /// WasmGC ref on wasm), not a linear address, so a pointer word there is
    /// untraceable. Every entry into such a class's code is a
    /// call from the declaring scope, so [`Self::refresh_enclosing_captures`] /
    /// [`Self::writeback_enclosing_captures`] around construction and method
    /// calls carry the value both ways (simtst47: `ra2 :- This A`).
    ///
    /// A class on its own stack (`detach`/`resume`, and every Process) is the
    /// one case that snapshotting cannot express: it can be resumed by a
    /// scheduler that the declaring scope never names, so an assignment made
    /// while it is operative has nowhere to be written back to (simtst96:
    /// `h :- been` inside a `process class car` must update the simulation
    /// block's `ref(head) h`, which MAIN reads after an unrelated `passivate`).
    /// Those keep the shared cell, and with it the last `LocalAddr(ObjectRef)`
    /// on wasm — the remaining 4-R3 debt, which needs a real heap cell
    /// (`ref_cell`) rather than a linear home word.
    ///
    /// Text/array captures stay by value too: their slots already hold shared
    /// descriptors. Formal-procedure receiver slots (`__simrt_fp_*`) also stay
    /// by value — they store the receiver object itself, not a pointer to a
    /// variable cell.
    ///
    /// Callers that walk [`Self::enclosing_capture_slots`] already know the field
    /// is a capture; [`Self::object_field_is_by_ref_capture`] is for the ones that
    /// only have a name, since a capture keeps its bare source name unless a
    /// class attribute shadows it.
    pub(in crate::mir::lower) fn capture_by_reference(
        field_name: &str,
        field_ty: FieldType,
        own_stack: bool,
    ) -> bool {
        if formal_proc_capture_source_name(field_name).is_some() {
            return false;
        }
        match field_ty {
            FieldType::I64 | FieldType::Bool | FieldType::F64 => true,
            FieldType::ObjectRef => own_stack,
            _ => false,
        }
    }

    /// [`Self::capture_by_reference`] for a capture slot of `class_name`.
    pub(in crate::mir::lower) fn class_capture_by_reference(
        &self,
        class_name: &str,
        field_name: &str,
        field_ty: FieldType,
    ) -> bool {
        Self::capture_by_reference(
            field_name,
            field_ty,
            self.class_runs_on_own_stack(class_name),
        )
    }

    /// Whether the field slot at `offset` on `this_id` is an enclosing capture
    /// held by reference. Matched by offset (not the source identifier): an
    /// access-level mangled attribute (`k$b`) and a bare capture `k` can share
    /// a source name, and only the capture slot holds a pointer (simtst98).
    pub(in crate::mir::lower) fn object_field_is_by_ref_capture(
        &self,
        this_id: LocalId,
        field_name: &str,
        field_ty: FieldType,
        offset: i64,
    ) -> bool {
        self.layout_for_object(this_id).is_some_and(|layout| {
            Self::capture_by_reference(field_name, field_ty, layout.runs_on_own_stack)
                && layout
                    .enclosing_captures
                    .iter()
                    .any(|(name, _)| layout.field_offset(name) == Some(offset))
        })
    }

    /// [`Place`] for a field of `this_id`: a by-reference capture slot holds a
    /// pointer to the declaring frame's cell, so it reads through that pointer
    /// rather than yielding the pointer itself.
    pub(in crate::mir::lower) fn object_field_place(
        &self,
        this_id: LocalId,
        offset: i64,
        field_name: &str,
        field_ty: FieldType,
        object_qual: Option<String>,
    ) -> Place {
        if self.object_field_is_by_ref_capture(this_id, field_name, field_ty, offset) {
            return Place::CaptureCell {
                object: this_id,
                offset,
                value_ty: mir_type_for_field(field_ty),
                qual: object_qual,
            };
        }
        remote_place(this_id, offset, field_ty, object_qual)
    }

    /// Address of a capture's home cell: a live local's stack slot, an enclosing
    /// object's attribute, or — when the enclosing scope captured it by
    /// reference too — the pointer that scope already holds. Only
    /// [`Self::capture_by_reference`] slots take an address, so `field_ty` is
    /// a scalar, or `ObjectRef` for a class on its own stack.
    pub(in crate::mir::lower) fn capture_source_address(
        &mut self,
        source_name: &str,
        capture_field_name: &str,
        field_ty: FieldType,
        span: Span,
    ) -> Option<LocalId> {
        // Same formal-shadow rule as [`Self::capture_source_value`]: a method
        // formal that reuses the enclosing name is not the capture home.
        if let Some(src) = self.scope_lookup(source_name)
            && !self.local_is_current_formal(src)
        {
            if !Self::capture_local_compatible(self.local_ty(src), field_ty) {
                return None;
            }
            let dest = self.temp(MirType::RefI64);
            self.push(Op::LocalAddr { dest, local: src }, span);
            return Some(dest);
        }
        let (this_id, offset, src_ty, _) = self.lookup_method_field(capture_field_name)?;
        if !Self::capture_local_compatible(mir_type_for_field(src_ty), field_ty) {
            return None;
        }
        let dest = if src_ty == FieldType::ObjectRef {
            self.temp(MirType::ObjectRef)
        } else {
            self.temp(MirType::RefI64)
        };
        if self.object_field_is_by_ref_capture(this_id, capture_field_name, src_ty, offset) {
            // The enclosing instance holds a pointer to the same home; pass it on.
            self.push(
                Op::FieldLoadI64 {
                    dest,
                    object: this_id,
                    offset,
                    class_qual: None,
                },
                span,
            );
        } else if src_ty == FieldType::ObjectRef {
            let value = self.temp(MirType::ObjectRef);
            self.push(
                Op::FieldLoadI64 {
                    dest: value,
                    object: this_id,
                    offset,
                    class_qual: None,
                },
                span.clone(),
            );
            self.push(
                Op::NewObject {
                    dest,
                    class_id: REF_CELL_CLASS_ID,
                    size: REF_CELL_SIZE,
                },
                span.clone(),
            );
            self.push(
                Op::FieldStoreI64 {
                    object: dest,
                    offset: REF_CELL_VALUE_OFFSET,
                    value,
                    class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                },
                span,
            );
        } else {
            self.push(
                Op::FieldAddr {
                    dest,
                    object: this_id,
                    offset,
                },
                span,
            );
        }
        Some(dest)
    }

    /// A fresh `ref_cell` holding `none`, for a by-reference `ref` capture whose
    /// source has no home in this scope.
    pub(in crate::mir::lower) fn empty_capture_cell(&mut self, span: Span) -> LocalId {
        let cell = self.temp(MirType::ObjectRef);
        self.note_object_qual(cell, REF_CELL_CLASS_NAME.to_string());
        self.push(
            Op::NewObject {
                dest: cell,
                class_id: REF_CELL_CLASS_ID,
                size: REF_CELL_SIZE,
            },
            span.clone(),
        );
        let none = self.temp(MirType::ObjectRef);
        self.push(Op::ConstNone { dest: none }, span.clone());
        self.push(
            Op::FieldStoreI64 {
                object: cell,
                offset: REF_CELL_VALUE_OFFSET,
                value: none,
                class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
            },
            span,
        );
        cell
    }

    pub(in crate::mir::lower) fn refresh_enclosing_captures(
        &mut self,
        object: LocalId,
        class_name: &str,
        span: Span,
    ) -> Result<Vec<(i64, LocalId)>, CompileError> {
        let mut refreshed = Vec::new();
        for (name, offset, field_ty) in self.enclosing_capture_slots(class_name) {
            // A by-reference slot already points at the variable's home; copying
            // a value over the pointer would destroy the sharing.
            if self.class_capture_by_reference(class_name, &name, field_ty) {
                continue;
            }
            let source_name = enclosing_capture_source_name(&name)
                .or_else(|| formal_proc_capture_source_name(&name))
                .unwrap_or(name.as_str());
            if let Some(src) = self.capture_source_value(source_name, &name, field_ty, span.clone())
            {
                // Snapshot the stored value — `src` may be a live local that a
                // name-thunk mutates during the subsequent call (simtst73).
                let snap = self.temp(mir_type_for_field(field_ty));
                self.push(Op::Copy { dest: snap, src }, span.clone());
                self.push(
                    Op::FieldStoreI64 {
                        object,
                        offset,
                        value: src,
                        class_qual: Some(class_name.to_string()),
                    },
                    span.clone(),
                );
                refreshed.push((offset, snap));
            }
        }
        Ok(refreshed)
    }

    /// Copy mutated enclosing-capture fields back to caller locals (same as
    /// after a normal `$__init` call). Loads into temps first so a capture that
    /// aliases the receiver local cannot clobber the object mid-writeback.
    /// When the source is only a method/`__this` *capture* field (outlined
    /// bodies / resumable `$__init`), store back through that capture so nested
    /// `new`/`resume` still share outer `seq`/`seqi` (simtst62) — never through
    /// a same-named plain attribute (simtst73).
    ///
    /// `refreshed` comes from the matching [`Self::refresh_enclosing_captures`]:
    /// if a capture field still holds the refreshed snapshot, skip writing it
    /// back to a scope local (the method may have updated that local via a
    /// name-thunk `LocalAddr`).
    pub(in crate::mir::lower) fn writeback_enclosing_captures(
        &mut self,
        object: LocalId,
        class_name: &str,
        refreshed: &[(i64, LocalId)],
        span: Span,
    ) -> Result<(), CompileError> {
        let captures = self.enclosing_capture_slots(class_name);
        let mut pending_locals: Vec<(LocalId, LocalId, Option<LocalId>)> = Vec::new();
        // (destination object, field offset, value, destination is a by-ref
        // capture cell rather than the variable itself)
        let mut pending_fields: Vec<(LocalId, i64, LocalId, bool)> = Vec::new();
        for (name, offset, field_ty) in captures {
            if self.class_capture_by_reference(class_name, &name, field_ty) {
                continue;
            }
            let source_name = enclosing_capture_source_name(&name)
                .or_else(|| formal_proc_capture_source_name(&name))
                .unwrap_or(name.as_str());
            let tmp = self.temp(mir_type_for_field(field_ty));
            self.push(
                Op::FieldLoadI64 {
                    dest: tmp,
                    object,
                    offset,
                    class_qual: Some(class_name.to_string()),
                },
                span.clone(),
            );
            if let Some(local) = self.scope_lookup(source_name)
                && !self.local_is_current_formal(local)
            {
                if Self::capture_local_compatible(self.local_ty(local), field_ty) {
                    let old = refreshed
                        .iter()
                        .find(|(off, _)| *off == offset)
                        .map(|(_, src)| *src);
                    pending_locals.push((local, tmp, old));
                    continue;
                }
            }
            if let Some((this_id, field_off, src_ty, _)) = self.lookup_method_field(&name) {
                // Don't write a capture back onto the same object/slot we just
                // loaded from (would be a no-op / self-alias).
                if this_id == object && field_off == offset {
                    continue;
                }
                if Self::capture_local_compatible(mir_type_for_field(src_ty), field_ty) {
                    let through_cell =
                        self.object_field_is_by_ref_capture(this_id, &name, src_ty, field_off);
                    pending_fields.push((this_id, field_off, tmp, through_cell));
                }
            }
        }
        for (local, tmp, old) in pending_locals {
            if let Some(old) = old {
                let changed = self.temp(MirType::Bool);
                self.push(
                    Op::Compare {
                        dest: changed,
                        op: CmpOp::Ne,
                        left: tmp,
                        right: old,
                    },
                    span.clone(),
                );
                let do_wb = self.new_block();
                let skip = self.new_block();
                self.push(
                    Op::Branch {
                        cond: changed,
                        then_block: do_wb,
                        else_block: skip,
                    },
                    span.clone(),
                );
                self.switch_to(do_wb);
                self.push(Op::StoreLocal { local, src: tmp }, span.clone());
                self.push(Op::Jump { target: skip }, span.clone());
                self.switch_to(skip);
            } else {
                self.push(Op::StoreLocal { local, src: tmp }, span.clone());
            }
        }
        for (this_id, field_off, tmp, through_cell) in pending_fields {
            if through_cell {
                // The enclosing instance shares the variable by pointer, so the
                // value belongs in the home it points at, not in the slot.
                let value_ty = self.local_ty(tmp);
                let cell = self.capture_cell_pointer(this_id, field_off, value_ty, span.clone());
                if value_ty == MirType::ObjectRef {
                    self.push(
                        Op::FieldStoreI64 {
                            object: cell,
                            offset: REF_CELL_VALUE_OFFSET,
                            value: tmp,
                            class_qual: Some(REF_CELL_CLASS_NAME.to_string()),
                        },
                        span.clone(),
                    );
                } else {
                    self.push(
                        Op::StoreRefI64 {
                            ptr: cell,
                            src: tmp,
                            offset: 0,
                        },
                        span.clone(),
                    );
                }
                continue;
            }
            self.push(
                Op::FieldStoreI64 {
                    object: this_id,
                    offset: field_off,
                    value: tmp,
                    class_qual: None,
                },
                span.clone(),
            );
        }
        Ok(())
    }

    /// `Simulation begin … end` → MAIN statement-index loop over the runtime SQS.
    pub(in crate::mir::lower) fn lower_simulation_block(
        &mut self,
        block: &Block,
    ) -> Result<(), CompileError> {
        let prev = self.simulation_context;
        self.simulation_context = true;

        for decl in &block.declarations {
            self.lower_declaration(decl)?;
        }
        for array in &block.arrays {
            self.lower_array_declaration(array)?;
        }

        self.push(Op::SimBegin, 0..0);
        if let Some(head_layout) = self.find_layout("Head") {
            self.push(
                Op::SimsetSetHeadClassId {
                    class_id: head_layout.class_id,
                },
                0..0,
            );
        }
        for statement in &block.statements {
            self.lower_statement(statement)?;
        }

        // MAIN's final end (12.3): it leaves the sequencing set, and the
        // processes still scheduled run on until the set is empty, at which
        // point the last of them hands control back here.
        self.push(Op::SimFinishMain, 0..0);
        self.push(Op::SimTransferToHead, 0..0);
        self.push(Op::SimEnd, 0..0);
        for inner in &block.body {
            self.lower_block(inner)?;
        }
        self.simulation_context = prev;
        Ok(())
    }

    pub(in crate::mir::lower) fn lower_hold_dt(
        &mut self,
        expr: &Expr,
    ) -> Result<LocalId, CompileError> {
        let value = self.lower_expr(expr)?;
        match self.local_ty(value) {
            MirType::F64 | MirType::LongF64 => Ok(value),
            MirType::I64 => {
                let dest = self.temp(MirType::F64);
                self.push(Op::I64ToF64 { dest, src: value }, expr.span.clone());
                Ok(dest)
            }
            _ => Err(spanned_error(
                "hold requires a real or integer argument",
                expr.span.clone(),
            )),
        }
    }

    /// Allocates every array attribute in `block` (and nested bodies) onto
    /// `__this`. Used by resumable `__init` on first entry; non-resumable
    /// init goes through [`Self::lower_class_init_body`].
    pub(in crate::mir::lower) fn lower_class_array_attrs(
        &mut self,
        block: &Block,
    ) -> Result<(), CompileError> {
        for array in &block.arrays {
            self.lower_array_declaration(array)?;
        }
        for inner in &block.body {
            self.lower_class_array_attrs(inner)?;
        }
        Ok(())
    }

    /// Field stores for attribute initializers / constants (e.g. `integer i=12`).
    /// Used by class `$__init`/`$__coro` and by prefixed blocks (which inline the
    /// class body without going through `$__init`).
    pub(in crate::mir::lower) fn emit_attribute_initializers(
        &mut self,
        block: &Block,
    ) -> Result<(), CompileError> {
        let Some(this_id) = self.method_this else {
            return Ok(());
        };
        let class_name = self.ref_qual.get(&this_id).cloned().unwrap_or_default();
        for decl in &block.declarations {
            let field_ty = match mir_type_for(&decl.ty)? {
                MirType::I64 => FieldType::I64,
                MirType::Bool => FieldType::Bool,
                MirType::F64 => FieldType::F64,
                MirType::LongF64 => FieldType::F64,
                MirType::Text => FieldType::Text,
                MirType::ObjectRef => FieldType::ObjectRef,
                _ => continue,
            };
            for item in &decl.items {
                let Some(initializer) = &item.initializer else {
                    continue;
                };
                let Some(offset) = self
                    .find_layout(&class_name)
                    .and_then(|layout| layout.field_offset(&item.name))
                    .or_else(|| {
                        let storage = self.remote_storage_name(&class_name, &item.name);
                        self.find_layout(&class_name)
                            .and_then(|layout| layout.field_offset(&storage))
                    })
                else {
                    continue;
                };
                let value = self.lower_expr(initializer)?;
                let value = self.coerce_value(
                    mir_type_for_field(field_ty),
                    value,
                    format!("initializer for '{}' has the wrong type", item.name),
                    initializer.span.clone(),
                )?;
                self.write_constructor_param_field(
                    this_id,
                    offset,
                    field_ty,
                    value,
                    initializer.span.clone(),
                );
            }
        }
        Ok(())
    }

    pub(in crate::mir::lower) fn emit_attribute_initializers_tree(
        &mut self,
        block: &Block,
    ) -> Result<(), CompileError> {
        self.emit_attribute_initializers(block)?;
        for inner in &block.body {
            self.emit_attribute_initializers_tree(inner)?;
        }
        Ok(())
    }

    /// Lowers class-body array attributes and initial statements without
    /// re-declaring scalar class attributes as locals (those are object
    /// fields via `__this`).
    pub(in crate::mir::lower) fn lower_class_init_body(
        &mut self,
        block: &Block,
    ) -> Result<(), CompileError> {
        self.emit_attribute_initializers(block)?;
        for array in &block.arrays {
            self.lower_array_declaration(array)?;
        }
        for statement in &block.statements {
            self.lower_statement(statement)?;
        }
        for inner in &block.body {
            self.lower_class_init_body(inner)?;
        }
        Ok(())
    }

    /// Deliberate null remote load so native/wasm abort when a coroutine
    /// resume is illegal (terminated / bad PC). Matches existing null-object
    /// traps without a new runtime helper.
    pub(in crate::mir::lower) fn emit_null_object_trap(&mut self, span: Span) {
        let none = self.temp(MirType::ObjectRef);
        self.push(Op::ConstNone { dest: none }, span.clone());
        let dummy = self.temp(MirType::I64);
        self.push(
            Op::FieldLoadI64 {
                dest: dummy,
                object: none,
                offset: 0,
                class_qual: None,
            },
            span,
        );
    }

    /// `call(x)` / `resume(x)` from MAIN (or residual mid-segment): re-enter
    /// `Class$__init`. Top-level `resume(x)` inside object bodies is a suspend
    /// boundary (see [`SuspendBoundary::Resume`]).
    pub(in crate::mir::lower) fn lower_call_or_resume(
        &mut self,
        call: &ProcedureCall,
        span: Span,
    ) -> Result<(), CompileError> {
        let kind = call.name.to_ascii_lowercase();
        if call.arguments.len() != 1 {
            return Err(spanned_error(
                format!("{kind} expects 1 argument, found {}", call.arguments.len()),
                span,
            ));
        }
        // 7.3.2 / 7.3.3 are runtime state transitions plus a stack switch; the
        // object's own frame is already where it needs to be.
        let object = self.lower_expr(&call.arguments[0])?;
        if self.local_ty(object) != MirType::ObjectRef {
            return Err(spanned_error(
                format!("{kind} requires an object reference"),
                span,
            ));
        }
        let op = if kind == "resume" {
            Op::SeqResume { object }
        } else {
            Op::SeqCall { object }
        };
        self.around_seq_transfer(object, span, |this, span| {
            this.push(op.clone(), span);
            Ok(())
        })
    }

    pub(in crate::mir::lower) fn lower_block_collecting(
        &mut self,
        block: &Block,
        errors: &mut Vec<CompileError>,
    ) {
        if let Err(error) = self.lower_block(block) {
            errors.push(error);
            self.push(Op::Nop, 0..0);
        }
    }

    /// Lowers one `array` declaration (Standard §5.2): integer and text
    /// arrays of any dimensionality. Other element types are a hard error
    /// naming the exact reason so callers get a clear diagnostic instead of a
    /// silent miscompile.
    pub(in crate::mir::lower) fn lower_array_declaration(
        &mut self,
        array: &ArrayDeclaration,
    ) -> Result<(), CompileError> {
        // Boolean and character arrays share the integer descriptor ABI (0/1
        // and codepoints respectively), matching the interpreter.
        let array_ty = match &array.element_type {
            Type::Integer { .. } | Type::Boolean | Type::Character | Type::ObjectRef(_) => {
                MirType::ArrayI64
            }
            Type::Real { .. } => MirType::ArrayF64,
            Type::Text => MirType::ArrayText,
            other => {
                return Err(CompileError::codegen(format!(
                    "MIR lowering: array element type '{other}' is not supported yet (only integer, boolean, character, real, text, and object-reference arrays are lowered)"
                )));
            }
        };
        let elem_ty = array_element_mir_type(&array.element_type)?;

        for segment in &array.segments {
            if segment.bounds.is_empty() {
                return Err(CompileError::codegen(
                    "MIR lowering: array declaration must have at least one dimension",
                ));
            }

            let mut bound_pairs = Vec::with_capacity(segment.bounds.len());
            let mut span = segment.bounds[0].lower.span.start..segment.bounds[0].upper.span.end;

            for bound in &segment.bounds {
                let low = self.lower_expr(&bound.lower)?;
                let low = self.coerce_value(
                    MirType::I64,
                    low,
                    "array lower bound must be an integer expression",
                    bound.lower.span.clone(),
                )?;
                let high = self.lower_expr(&bound.upper)?;
                let high = self.coerce_value(
                    MirType::I64,
                    high,
                    "array upper bound must be an integer expression",
                    bound.upper.span.clone(),
                )?;
                span = span.start.min(bound.lower.span.start)..span.end.max(bound.upper.span.end);
                bound_pairs.push((low, high));
            }

            for name in &segment.names {
                let is_class_field = if let Some(this_id) = self.method_this {
                    self.method_field_info(this_id, name)
                        .is_some_and(|(_, field_ty, _)| {
                            matches!(
                                field_ty,
                                FieldType::ArrayI64
                                    | FieldType::ArrayBool
                                    | FieldType::ArrayF64
                                    | FieldType::ArrayText
                            )
                        })
                } else {
                    false
                };
                let id = self.new_local(name.clone(), array_ty);
                if !is_class_field {
                    self.scope.insert(name.clone(), id);
                    self.note_inline_body_local(name);
                }
                self.note_array_elem_ty(id, elem_ty);
                if let Type::ObjectRef(qual) = &array.element_type {
                    self.note_array_elem_qual(id, qual.clone());
                }
                self.push(
                    Op::AllocArray {
                        dest: id,
                        bounds: bound_pairs.clone(),
                    },
                    span.clone(),
                );
                // Class attribute: snapshot the descriptor onto `__this`.
                // Do not keep `id` in scope — resume re-enters `__init` with
                // fresh locals, so later uses must load from the object field.
                if is_class_field {
                    let this_id = self.method_this.expect("class field implies __this");
                    let (offset, _, _) = self
                        .method_field_info(this_id, name)
                        .expect("is_class_field guard");
                    self.push(
                        Op::FieldStoreI64 {
                            object: this_id,
                            offset,
                            value: id,
                            class_qual: None,
                        },
                        span.clone(),
                    );
                }
            }
        }
        Ok(())
    }
}

pub(in crate::mir::lower) fn block_has_own_debug_data(block: &Block) -> bool {
    !block.declarations.is_empty() || !block.arrays.is_empty()
}

pub(in crate::mir::lower) fn block_debug_name(block: &Block) -> String {
    if !block.name.is_empty() {
        return block.name.clone();
    }
    if let Some(prefix) = &block.prefix {
        match &prefix.kind {
            ExprKind::Variable(Variable::Simple(name)) => return name.clone(),
            ExprKind::FunctionCall { name, .. } => return name.clone(),
            _ => {}
        }
    }
    "<block>".into()
}

pub(in crate::mir::lower) fn block_source_span(block: &Block) -> Span {
    accumulate_block_span(block, None)
}

pub(in crate::mir::lower) fn block_source_span_with_prefix(block: &Block, prefix: &Expr) -> Span {
    accumulate_block_span(block, Some(prefix))
}

fn accumulate_block_span(block: &Block, prefix: Option<&Expr>) -> Span {
    let mut start = usize::MAX;
    let mut end = 0usize;
    let mut acc = |span: &Span| {
        if span.start < span.end {
            start = start.min(span.start);
            end = end.max(span.end);
        }
    };
    if let Some(prefix) = prefix.or(block.prefix.as_ref()) {
        acc(&prefix.span);
    }
    for decl in &block.declarations {
        acc(&decl.span);
    }
    for array in &block.arrays {
        acc(&array.span);
    }
    for switch in &block.switches {
        acc(&switch.span);
    }
    for statement in &block.statements {
        acc(&statement.span);
    }
    for inner in &block.body {
        let inner_span = accumulate_block_span(inner, None);
        acc(&inner_span);
    }
    if start == usize::MAX {
        0..0
    } else {
        start..end
    }
}
