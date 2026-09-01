//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn new(
        name: String,
        strings: &'a mut Vec<String>,
        signatures: &'a HashMap<String, ProcSignature>,
        name_param_procs: &'a HashMap<String, &'a ProcedureDeclaration>,
        ref_alias_procs: &'a HashMap<String, &'a ProcedureDeclaration>,
        layouts: &'a HashMap<String, ClassLayout>,
        classes: &'a HashMap<String, ClassDeclaration>,
    ) -> Self {
        Self {
            name,
            locals: Vec::new(),
            param_count: 0,
            blocks: Vec::new(),
            current: BlockId(0),
            scope: HashMap::new(),
            constants: HashSet::new(),
            name_thunks: HashMap::new(),
            name_thunk_tys: HashMap::new(),
            free_cell_addrs: HashMap::new(),
            formal_proc_refs: HashMap::new(),
            ref_qual: HashMap::new(),
            array_elem_ty: HashMap::new(),
            array_elem_qual: HashMap::new(),
            strings,
            string_index: HashMap::new(),
            temp_counter: 0,
            signatures,
            name_param_procs,
            ref_alias_procs,
            name_bindings: HashMap::new(),
            name_formal_tys: HashMap::new(),
            formal_proc_bindings: HashMap::new(),
            formal_label_bindings: HashMap::new(),
            formal_switch_bindings: HashMap::new(),
            name_env_stack: Vec::new(),
            name_formal_ty_stack: Vec::new(),
            formal_proc_env_stack: Vec::new(),
            formal_label_env_stack: Vec::new(),
            formal_switch_env_stack: Vec::new(),
            inline_scope_restores: Vec::new(),
            inline_body_locals: Vec::new(),
            inline_stack: Vec::new(),
            inline_debug_scopes: Vec::new(),
            recorded_debug_scopes: Vec::new(),
            inline_detach_names_receiver: Vec::new(),
            layouts,
            classes,
            method_this: None,
            method_this_stack: Vec::new(),
            method_this_is_connection: false,
            text_span: 0..0,
            prefixed_block_access: None,
            prefixed_block_procs: HashSet::new(),
            access_level_substitutions: false,
            connection_depth: 0,
            inspect_connection_depth: 0,
            connection_kept_outers: HashSet::new(),
            labels: HashMap::new(),
            label_def_queue: VecDeque::new(),
            outer_labels: Vec::new(),
            switches: HashMap::new(),
            switch_dispatch: HashMap::new(),
            pending_helpers: Vec::new(),
            expr_helper_counter: 0,
            simulation_context: false,
            source_text: String::new(),
            pending_stream_field_writeback: None,
        }
    }

    pub(in crate::mir::lower) fn with_source_text(mut self, source: &str) -> Self {
        self.source_text = source.to_string();
        self
    }

    /// Allocate or look up the goto target block for statement label `name`.
    /// Prefer a predeclared (last-wins) binding from [`Self::labels`].
    pub(in crate::mir::lower) fn label_block(&mut self, name: &str) -> BlockId {
        let key = name.to_ascii_lowercase();
        if let Some(&id) = self.labels.get(&key) {
            return id;
        }
        // Frozen/by-value LABEL formals jump into the caller's label scope.
        for outer in self.outer_labels.iter().rev() {
            if let Some(&id) = outer.get(&key) {
                return id;
            }
        }
        let id = self.new_block();
        self.labels.insert(key, id);
        id
    }

    /// Resolve a statement label for `goto`: block in the current CFG, or
    /// escape to an enclosing activation when lowering an outlined procedure.
    pub(in crate::mir::lower) fn resolve_label_target(&self, name: &str) -> LabelTarget {
        let key = name.to_ascii_lowercase();
        if let Some(&id) = self.labels.get(&key) {
            return LabelTarget::Block(id);
        }
        // Frozen/by-value LABEL formals jump into the caller's label scope.
        for outer in self.outer_labels.iter().rev() {
            if let Some(&id) = outer.get(&key) {
                return LabelTarget::Block(id);
            }
        }
        LabelTarget::Escape(name.to_string())
    }

    /// Predeclare every statement label in `statements` (and nested compounds):
    /// one unique BB per occurrence; [`Self::labels`] binds each name to its
    /// **last** occurrence. Push BBs onto [`Self::label_def_queue`] in source
    /// order so [`StatementKind::Labeled`] can pop them while lowering.
    pub(in crate::mir::lower) fn predeclare_labels_in_statements(
        &mut self,
        statements: &[Statement],
    ) {
        let mut names = Vec::new();
        for statement in statements {
            collect_label_occurrence_names(statement, &mut names);
        }
        for name in names {
            let key = name.to_ascii_lowercase();
            let id = self.new_block();
            self.label_def_queue.push_back(id);
            self.labels.insert(key, id);
        }
    }

    pub(in crate::mir::lower) fn predeclare_labels_in_block(&mut self, block: &Block) {
        let mut names = Vec::new();
        collect_label_occurrence_names_in_block(block, &mut names);
        for name in names {
            let key = name.to_ascii_lowercase();
            let id = self.new_block();
            self.label_def_queue.push_back(id);
            self.labels.insert(key, id);
        }
    }

    /// Run `f` with a fresh label scope (for inlined procedure bodies).
    pub(in crate::mir::lower) fn with_fresh_label_scope<R>(
        &mut self,
        body: &Block,
        f: impl FnOnce(&mut Self) -> Result<R, CompileError>,
    ) -> Result<R, CompileError> {
        self.with_fresh_label_scope_predeclare(|this| this.predeclare_labels_in_block(body), f)
    }

    /// Like [`Self::with_fresh_label_scope`], but `predeclare` chooses which
    /// label occurrences enter the fresh map/queue (prefixed blocks need the
    /// concatenated class-body statements, not only the user's block).
    pub(in crate::mir::lower) fn with_fresh_label_scope_predeclare<R>(
        &mut self,
        predeclare: impl FnOnce(&mut Self),
        f: impl FnOnce(&mut Self) -> Result<R, CompileError>,
    ) -> Result<R, CompileError> {
        let saved_labels = std::mem::take(&mut self.labels);
        let saved_queue = std::mem::take(&mut self.label_def_queue);
        self.outer_labels.push(saved_labels);
        predeclare(self);
        let result = f(self);
        self.labels = self.outer_labels.pop().unwrap_or_default();
        self.label_def_queue = saved_queue;
        result
    }

    pub(in crate::mir::lower) fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len());
        self.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            ops: Vec::new(),
        });
        id
    }

    pub(in crate::mir::lower) fn switch_to(&mut self, block: BlockId) {
        self.current = block;
    }

    pub(in crate::mir::lower) fn push(&mut self, mut op: Op, span: Span) {
        // `FieldLoadI64`/`FieldStoreI64::class_qual` must reflect `object`'s
        // WasmGC qualifier *at this exact program point*, not whatever
        // `Local::class_qual` holds once lowering finishes (see the doc
        // comment on `Op::FieldLoadI64`) — always take the live snapshot
        // here, overriding whatever placeholder the call site passed.
        match &mut op {
            Op::FieldLoadI64 {
                object, class_qual, ..
            }
            | Op::FieldStoreI64 {
                object, class_qual, ..
            } => {
                *class_qual = self.locals.get(object.0).and_then(|l| l.class_qual.clone());
            }
            _ => {}
        }
        self.blocks[self.current.0].ops.push(SpannedOp { op, span });
    }

    pub(in crate::mir::lower) fn new_local(&mut self, name: String, ty: MirType) -> LocalId {
        self.new_local_qualified(name, ty, None)
    }

    pub(in crate::mir::lower) fn new_local_qualified(
        &mut self,
        name: String,
        ty: MirType,
        class_qual: Option<String>,
    ) -> LocalId {
        let id = LocalId(self.locals.len());
        let debug_scope = self
            .inline_debug_scopes
            .last()
            .cloned()
            .filter(|span| span.start < span.end);
        self.locals.push(Local {
            name,
            ty,
            class_qual,
            debug_scope,
        });
        id
    }

    pub(in crate::mir::lower) fn record_debug_scope(
        &mut self,
        name: String,
        span: Span,
        kind: DebugScopeKind,
    ) {
        if span.start >= span.end {
            return;
        }
        if self.recorded_debug_scopes.iter().any(|scope| {
            scope.kind == kind && scope.span == span && scope.name.eq_ignore_ascii_case(&name)
        }) {
            return;
        }
        self.recorded_debug_scopes
            .push(DebugScope { name, span, kind });
    }

    /// Push a nested/prefixed block onto the debug-scope stack so its locals
    /// inherit the block span. Returns whether a scope was pushed.
    pub(in crate::mir::lower) fn enter_block_debug_scope(
        &mut self,
        name: String,
        span: Span,
        block: &Block,
    ) -> bool {
        if !super::block::block_has_own_debug_data(block) || span.start >= span.end {
            return false;
        }
        self.record_debug_scope(name, span.clone(), DebugScopeKind::Block);
        self.inline_debug_scopes.push(span);
        true
    }

    pub(in crate::mir::lower) fn pop_debug_scope(&mut self) {
        self.inline_debug_scopes.pop();
    }

    pub(in crate::mir::lower) fn set_local_class_qual(&mut self, id: LocalId, qual: String) {
        if let Some(local) = self.locals.get_mut(id.0) {
            local.class_qual = Some(qual);
        }
    }

    pub(in crate::mir::lower) fn clear_local_class_qual(&mut self, id: LocalId) {
        if let Some(local) = self.locals.get_mut(id.0) {
            local.class_qual = None;
        }
    }

    /// Records that `id` is a `ref(qual)` for remote-field lowering and DWARF.
    pub(in crate::mir::lower) fn note_object_qual(&mut self, id: LocalId, qual: String) {
        self.ref_qual.insert(id, qual.clone());
        self.set_local_class_qual(id, qual);
    }

    /// Snapshot of both qualification slots for `id`, to restore after a
    /// *transient* narrowing scope (`inspect`/connection block, `for … while
    /// X is C do`). `ref_qual` is the static/declared access-level
    /// qualification; `Local::class_qual` is the wasm codegen's best current
    /// guess at the object's runtime WasmGC struct type. These must be saved
    /// and restored **independently**: narrowing a `ref(A) ra` holding a `B`
    /// instance to `B` inside `when B do …` should, on exit, put `class_qual`
    /// back to `B` (the real instance, unchanged by the connection ending) —
    /// not back to the *declared* `A`, which is what restoring solely from
    /// `ref_qual` would do and which produces an illegal `ref.cast` the next
    /// time this local's fields are read (simtst50/60/93 et al.).
    pub(in crate::mir::lower) fn snapshot_object_qual(
        &self,
        id: LocalId,
    ) -> (Option<String>, Option<String>) {
        let ref_qual = self.ref_qual.get(&id).cloned();
        let instance_qual = self.locals.get(id.0).and_then(|l| l.class_qual.clone());
        (ref_qual, instance_qual)
    }

    /// Restore a snapshot taken by [`Self::snapshot_object_qual`].
    pub(in crate::mir::lower) fn restore_object_qual(
        &mut self,
        id: LocalId,
        saved: (Option<String>, Option<String>),
    ) {
        let (ref_qual, instance_qual) = saved;
        match ref_qual {
            Some(qual) => {
                self.ref_qual.insert(id, qual);
            }
            None => {
                self.ref_qual.remove(&id);
            }
        }
        match instance_qual {
            Some(qual) => self.set_local_class_qual(id, qual),
            None => self.clear_local_class_qual(id),
        }
    }

    /// On `:-` into a local that already has a declared `ref(T)` qualification,
    /// keep `T` as the access level (§5.5.6). Adopt the source's span-qualified
    /// layout name when the destination only has the unqualified declared name
    /// (two `SIMSET` blocks each declare `ref(A) x` — simtst76). The pointed-to
    /// object's layout (for capture refresh / field offsets) always follows `src`.
    pub(in crate::mir::lower) fn note_object_qual_from_assign(
        &mut self,
        dest: LocalId,
        src: LocalId,
    ) {
        let Some(src_qual) = self.ref_qual.get(&src).cloned() else {
            return;
        };
        let instance_qual = self
            .locals
            .get(src.0)
            .and_then(|local| local.class_qual.clone())
            .unwrap_or(src_qual.clone());
        self.set_local_class_qual(dest, instance_qual);
        match self.ref_qual.get(&dest) {
            None => self.note_object_qual(dest, src_qual),
            Some(dest_qual) if dest_qual.contains('@') => {}
            Some(dest_qual)
                if src_qual.contains('@')
                    && declared_class_name(&src_qual).eq_ignore_ascii_case(dest_qual) =>
            {
                self.note_object_qual(dest, src_qual);
            }
            Some(_) => {}
        }
    }

    /// Layout name for an object's **instance** (creation / pointed-to class),
    /// used for enclosing-capture offsets. Differs from [`Self::ref_qual`] on
    /// `ref(A) x :- new C` (access `A`, instance `C` — simtst48).
    pub(in crate::mir::lower) fn instance_layout_name(&self, object: LocalId) -> Option<String> {
        self.locals
            .get(object.0)
            .and_then(|local| local.class_qual.clone())
            .or_else(|| self.ref_qual.get(&object).cloned())
    }

    pub(in crate::mir::lower) fn note_array_elem_ty(&mut self, id: LocalId, elem_ty: MirType) {
        self.array_elem_ty.insert(id, elem_ty);
    }

    pub(in crate::mir::lower) fn note_array_elem_qual(&mut self, id: LocalId, qual: String) {
        self.array_elem_qual.insert(id, qual);
    }

    /// After loading a remote field into `dest`, record array element typing /
    /// object-ref qualification from the field layout.
    pub(in crate::mir::lower) fn annotate_loaded_field(
        &mut self,
        dest: LocalId,
        object: LocalId,
        attribute: &str,
        field_ty: FieldType,
    ) {
        let class_qual = self.layout_for_object(object).and_then(|layout| {
            layout
                .fields
                .iter()
                .find(|field| field.name.eq_ignore_ascii_case(attribute))
                .and_then(|field| field.class_qual.clone())
        });
        match field_ty {
            FieldType::ArrayText => self.note_array_elem_ty(dest, MirType::Text),
            FieldType::ArrayF64 => self.note_array_elem_ty(dest, MirType::F64),
            FieldType::ArrayBool => self.note_array_elem_ty(dest, MirType::Bool),
            FieldType::ArrayI64 => {
                if let Some(qual) = class_qual {
                    self.note_array_elem_ty(dest, MirType::ObjectRef);
                    self.note_array_elem_qual(dest, qual);
                } else {
                    self.note_array_elem_ty(dest, MirType::I64);
                }
            }
            FieldType::ObjectRef => {
                if let Some(qual) = class_qual {
                    self.note_object_qual(dest, qual);
                }
            }
            _ => {}
        }
    }

    pub(in crate::mir::lower) fn temp(&mut self, ty: MirType) -> LocalId {
        let index = self.temp_counter;
        self.temp_counter += 1;
        self.new_local(format!("%t{index}"), ty)
    }

    /// Binds `parameters` as locals, in declaration order: an outlined
    /// call-by-name integer formal expands into three locals (`x$get`,
    /// `x$set`, `x$env`) recorded in `self.name_thunks` (its plain name is
    /// *not* added to `self.scope`); every other formal becomes a single
    /// local in `self.scope`, exactly as before. Callers can read
    /// `self.locals.len()` right after this returns to get the expanded
    /// parameter count for `Function::param_count`/`Self::finish`.
    pub(in crate::mir::lower) fn bind_formal_params(
        &mut self,
        parameters: &[FormalParameter],
    ) -> Result<(), CompileError> {
        for param in parameters {
            if param.is_procedure {
                let func = self.new_local(format!("{}$func", param.name), MirType::FuncRef);
                let env = self.new_local(format!("{}$env", param.name), MirType::RefI64);
                self.formal_proc_refs
                    .insert(param.name.clone(), (func, env));
                continue;
            }
            if is_name_thunk_formal(param)? {
                let get = self.new_local(format!("{}$get", param.name), MirType::FuncRef);
                let set = self.new_local(format!("{}$set", param.name), MirType::FuncRef);
                let env = self.new_local(format!("{}$env", param.name), MirType::ObjectRef);
                let value_ty = mir_type_for(&param.ty)?;
                self.name_thunks.insert(param.name.clone(), (get, set, env));
                self.name_thunk_tys.insert(param.name.clone(), value_ty);
                continue;
            }
            let ty = outlined_param_mir_type(param)?;
            let id = self.new_local(param.name.clone(), ty);
            self.scope.insert(param.name.clone(), id);
            if let Type::ObjectRef(qual) = &param.ty {
                self.note_object_qual(id, qual.clone());
            }
            if let Type::Array { element, .. } = &param.ty {
                self.note_array_elem_ty(id, array_element_mir_type(element)?);
                if let Type::ObjectRef(qual) = element.as_ref() {
                    self.note_array_elem_qual(id, qual.clone());
                }
            }
        }
        Ok(())
    }

    /// Trailing [`MirType::RefI64`] parameters for free enclosing integer cells.
    pub(in crate::mir::lower) fn bind_free_cell_param_envs(
        &mut self,
        names: &[String],
    ) -> Vec<LocalId> {
        let mut envs = Vec::with_capacity(names.len());
        for name in names {
            let env = self.new_local(format!("{name}$env"), MirType::RefI64);
            envs.push(env);
        }
        envs
    }

    /// Wires shared name-thunk get/set helpers to each free-cell env parameter.
    pub(in crate::mir::lower) fn bind_free_cell_thunk_helpers(
        &mut self,
        names: &[String],
        envs: &[LocalId],
        value_tys: &[MirType],
    ) {
        for ((name, &addr), &value_ty) in names.iter().zip(envs).zip(value_tys) {
            let get = self.temp(MirType::FuncRef);
            self.push(
                Op::FuncAddr {
                    dest: get,
                    name: NAME_THUNK_GET_HELPER.to_string(),
                },
                0..0,
            );
            let set = self.temp(MirType::FuncRef);
            self.push(
                Op::FuncAddr {
                    dest: set,
                    name: NAME_THUNK_SET_HELPER.to_string(),
                },
                0..0,
            );
            let env = self.box_int_cell_env(addr, 0..0);
            self.free_cell_addrs.insert(name.clone(), addr);
            self.name_thunks.insert(name.clone(), (get, set, env));
            self.name_thunk_tys.insert(name.clone(), value_ty);
        }
    }

    /// Wraps the linear address `addr` in a [`NAME_INT_ENV_CLASS_NAME`] object,
    /// so it can be passed where a name-thunk `env` parameter is expected. Those
    /// parameters are [`MirType::ObjectRef`] because `dec(r.x)` needs a
    /// `ref_cell` there; an integer cell's
    /// raw address cannot share the slot.
    pub(in crate::mir::lower) fn box_int_cell_env(&mut self, addr: LocalId, span: Span) -> LocalId {
        let env = self.temp(MirType::ObjectRef);
        self.note_object_qual(env, NAME_INT_ENV_CLASS_NAME.to_string());
        self.push(
            Op::NewObject {
                dest: env,
                class_id: NAME_INT_ENV_CLASS_ID,
                size: NAME_INT_ENV_SIZE,
            },
            span.clone(),
        );
        self.push(
            Op::FieldStoreI64 {
                object: env,
                offset: NAME_INT_ENV_ADDR_OFFSET,
                value: addr,
                class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
            },
            span,
        );
        env
    }

    pub(in crate::mir::lower) fn local_ty(&self, id: LocalId) -> MirType {
        self.locals[id.0].ty
    }

    pub(in crate::mir::lower) fn intern_string(&mut self, value: &str) -> usize {
        if let Some(&id) = self.string_index.get(value) {
            return id;
        }
        let id = self.strings.len();
        self.strings.push(value.to_string());
        self.string_index.insert(value.to_string(), id);
        id
    }

    /// Splits `self.locals` into `Function::params` (the leading
    /// `param_count` entries, in declaration order) and `Function::locals`
    /// (everything else), matching the indexing [`Function::local`] expects.
    /// Also returns any per-call-site helper functions queued while lowering.
    pub(in crate::mir::lower) fn finish(
        mut self,
        entry: BlockId,
        result: Option<MirType>,
    ) -> (Function, Vec<Function>) {
        let helpers = self.pending_helpers;
        let locals = self.locals.split_off(self.param_count);
        (
            Function {
                name: self.name,
                params: self.locals,
                locals,
                entry,
                blocks: self.blocks,
                labels: self.labels.clone(),
                result,
                array_elem_kinds: self.array_elem_ty,
                foreign: None,
                export: None,
                debug_scopes: self.recorded_debug_scopes,
            },
            helpers,
        )
    }
}
