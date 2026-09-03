//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    /// Lowers `inspect` (§4.8): `when` clauses match the object's declared
    /// qualification (`ref_qual`, updated by `:-` / `qua`); `do` runs when
    /// the object is not `none`; `otherwise` runs for `none` or when no `when`
    /// matched — matching `eval::execute_inspect`.
    ///
    /// Connection blocks (`when` / `do` bodies) temporarily make the connected
    /// object's attributes visible as bare names via [`Self::method_this`],
    /// matching interpreter connection-block attribute injection.
    pub(in crate::mir::lower) fn lower_inspect(
        &mut self,
        inspect: &InspectStatement,
        span: Span,
    ) -> Result<(), CompileError> {
        let object_id = self.lower_expr(&inspect.object)?;
        if self.local_ty(object_id) != MirType::ObjectRef {
            return Err(spanned_error(
                "MIR lowering: inspect requires an object reference expression",
                span,
            ));
        }

        let merge = self.new_block();
        let mut after_whens = None;

        if !inspect.when_clauses.is_empty() {
            let mut next_check = self.new_block();
            self.push(Op::Jump { target: next_check }, span.clone());

            for when in &inspect.when_clauses {
                self.switch_to(next_check);
                let match_block = self.new_block();
                next_check = self.new_block();

                let cond =
                    self.lower_inspect_when_match(object_id, &when.class_name, span.clone())?;
                self.push(
                    Op::Branch {
                        cond,
                        then_block: match_block,
                        else_block: next_check,
                    },
                    span.clone(),
                );

                self.switch_to(match_block);
                self.with_inspect_connection_this(object_id, &when.class_name, |this| {
                    this.lower_statement(&when.body)
                })?;
                self.push(Op::Jump { target: merge }, 0..0);
            }

            after_whens = Some(next_check);
        } else if let Some(do_clause) = &inspect.do_clause {
            let not_none = self.lower_object_is_not_none(object_id, span.clone());
            let do_block = self.new_block();
            let none_block = self.new_block();
            self.push(
                Op::Branch {
                    cond: not_none,
                    then_block: do_block,
                    else_block: none_block,
                },
                span.clone(),
            );
            self.switch_to(do_block);
            let block_qual = self.ref_qual.get(&object_id).cloned().unwrap_or_default();
            self.with_inspect_connection_this(object_id, &block_qual, |this| {
                this.lower_statement(do_clause)
            })?;
            self.push(Op::Jump { target: merge }, 0..0);
            self.switch_to(none_block);
            if let Some(otherwise) = &inspect.otherwise {
                self.lower_statement(otherwise)?;
            }
            self.push(Op::Jump { target: merge }, 0..0);
            self.switch_to(merge);
            return Ok(());
        }

        if let Some(otherwise) = &inspect.otherwise {
            if let Some(fallthrough) = after_whens {
                // All `when` clauses failed — always take `otherwise`.
                self.switch_to(fallthrough);
                self.lower_statement(otherwise)?;
                self.push(Op::Jump { target: merge }, 0..0);
            } else {
                // No `when`/`do` — still honor a lone `otherwise` (rare).
                self.lower_statement(otherwise)?;
                self.push(Op::Jump { target: merge }, 0..0);
            }
        } else if let Some(fallthrough) = after_whens {
            self.switch_to(fallthrough);
            self.push(Op::Jump { target: merge }, 0..0);
        }

        self.switch_to(merge);
        Ok(())
    }

    /// `inspect` when/do connection: same as [`Self::with_connection_this`],
    /// and also bumps [`Self::inspect_connection_depth`] so free procedures
    /// yield to methods of the connected qualification.
    pub(in crate::mir::lower) fn with_inspect_connection_this(
        &mut self,
        object: LocalId,
        class_name: &str,
        body: impl FnOnce(&mut Self) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        let saved_depth = self.inspect_connection_depth;
        self.inspect_connection_depth += 1;
        let result = self.with_connection_this(object, class_name, body);
        self.inspect_connection_depth = saved_depth;
        result
    }

    /// Enter a connection block: bare attribute names resolve through `object`
    /// with qualification `class_name`, shadowing outer locals of the same name.
    pub(in crate::mir::lower) fn with_connection_this(
        &mut self,
        object: LocalId,
        class_name: &str,
        body: impl FnOnce(&mut Self) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        self.push_this_receiver(object, true);
        let saved_qual = self.snapshot_object_qual(object);
        if !class_name.is_empty() {
            self.note_object_qual(object, class_name.to_string());
        }
        let saved_subs = self.access_level_substitutions;
        self.access_level_substitutions = true;
        let saved_connection_depth = self.connection_depth;
        self.connection_depth += 1;
        let saved_kept_outers = self.connection_kept_outers.clone();

        let mut saved_scope: Vec<(String, LocalId)> = Vec::new();
        let mut remove_names: Vec<String> = Vec::new();
        let mut keep_names: Vec<String> = Vec::new();
        if let Some(layout) = self.find_layout(class_name) {
            for field in layout
                .fields
                .iter()
                .filter(|f| !f.name.starts_with("__simrt_"))
            {
                let keep = layout.enclosing_captures.iter().any(|(name, _)| {
                    name.eq_ignore_ascii_case(&field.name)
                        || enclosing_capture_source_name(name)
                            .is_some_and(|src| src.eq_ignore_ascii_case(&field.name))
                });
                if keep {
                    keep_names.push(field.name.clone());
                } else {
                    remove_names.push(field.name.clone());
                }
            }
        }
        for name in keep_names {
            if self.scope_has_name(&name) {
                self.connection_kept_outers.insert(name);
            }
        }
        for name in remove_names {
            if let Some(id) = self.scope.remove(&name) {
                saved_scope.push((name, id));
            }
        }

        let result = body(self);

        for (name, id) in saved_scope {
            self.scope.insert(name, id);
        }
        self.connection_kept_outers = saved_kept_outers;
        self.connection_depth = saved_connection_depth;
        self.access_level_substitutions = saved_subs;
        self.pop_this_receiver();
        self.restore_object_qual(object, saved_qual);
        result
    }

    /// Push the current `__this` (if any) and make `object` the active receiver.
    pub(in crate::mir::lower) fn push_this_receiver(&mut self, object: LocalId, connection: bool) {
        if let Some(prev) = self.method_this {
            self.method_this_stack.push(ThisReceiver {
                id: prev,
                qual: self.ref_qual.get(&prev).cloned(),
                substitutions: self.access_level_substitutions,
                connection: self.method_this_is_connection,
            });
        }
        self.method_this = Some(object);
        self.method_this_is_connection = connection;
    }

    pub(in crate::mir::lower) fn pop_this_receiver(&mut self) {
        if let Some(prev) = self.method_this_stack.pop() {
            self.method_this = Some(prev.id);
            self.method_this_is_connection = prev.connection;
            self.access_level_substitutions = prev.substitutions;
        } else {
            self.method_this = None;
            self.method_this_is_connection = false;
        }
    }

    /// Walk current + outer `inspect`/method receivers (innermost first), each
    /// with the qualification that should be used for attribute lookup.
    pub(in crate::mir::lower) fn method_this_chain(
        &self,
    ) -> impl Iterator<Item = (LocalId, Option<&str>)> + '_ {
        let current = self
            .method_this
            .map(|id| (id, self.ref_qual.get(&id).map(String::as_str)));
        let outer = self
            .method_this_stack
            .iter()
            .rev()
            .map(|recv| (recv.id, recv.qual.as_deref()));
        current.into_iter().chain(outer)
    }

    /// Program-text access level for §5.5.3–§5.5.6 attribute visibility.
    ///
    /// Uses the *lexical* class (the text being compiled), never the object an
    /// `inspect` is connected to. Prefixed-block *user* statements are a
    /// fictitious inner prefix level of their prefix class (`prefixed_block:
    /// true` so the prefix's own `hidden` applies). Class-body statements
    /// inlined into a prefixed block (or into `$__init` / `$__coro`) keep the
    /// access level of the class text they came from — selected by span.
    pub(in crate::mir::lower) fn access_level(&self) -> AccessLevel<'_> {
        if let Some(class) = self.class_name_for_text_span() {
            return AccessLevel::class_text(class);
        }
        if let Some(class) = self.prefixed_block_access.as_deref() {
            return AccessLevel {
                class: Some(class),
                prefixed_block: true,
            };
        }
        if !self.method_this_is_connection {
            if let Some(id) = self.method_this {
                if let Some(qual) = self.ref_qual.get(&id) {
                    return AccessLevel::class_text(qual.as_str());
                }
            }
        }
        for recv in self.method_this_stack.iter().rev() {
            if !recv.connection {
                if let Some(qual) = recv.qual.as_deref() {
                    return AccessLevel::class_text(qual);
                }
            }
        }
        AccessLevel::outside()
    }

    /// Whether a bare call to `call_name` binds to a procedure declared by the
    /// active prefixed block rather than to the prefix's own attribute.
    ///
    /// The block is the class's inner part, so its declarations hide same-named
    /// prefix attributes for calls written in the block. Statements inlined
    /// from the prefix's *own* body keep their lexical binding (§5.5) — unless
    /// the name is a virtual quantity, whose match is then the block's
    /// declaration (simtst92). Same-named free procedures never win here
    /// (simtst98: `d begin` must still reach `a`'s virtual `virtproc`).
    pub(in crate::mir::lower) fn prefixed_block_proc_applies(&self, call_name: &str) -> bool {
        if !self
            .prefixed_block_procs
            .iter()
            .any(|name| name.eq_ignore_ascii_case(call_name))
        {
            return false;
        }
        let Some(text_class) = self.class_name_for_text_span() else {
            return true;
        };
        prefix_chain(text_class, self.classes).iter().any(|level| {
            self.classes
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(level))
                .is_some_and(|(_, class)| is_virtual_quantity(class, call_name))
        })
    }

    /// Raw class whose declaration source covers [`Self::text_span`], if any.
    /// Narrowest covering span wins (innermost class when nested).
    pub(in crate::mir::lower) fn class_name_for_text_span(&self) -> Option<&str> {
        let span = &self.text_span;
        if span.start == span.end && span.start == 0 {
            return None;
        }
        let mut best: Option<(&str, usize)> = None;
        for (name, class) in self.classes {
            if class.span.start <= span.start && span.end <= class.span.end {
                let len = class.span.end.saturating_sub(class.span.start);
                if best.is_none_or(|(_, best_len)| len < best_len) {
                    best = Some((name.as_str(), len));
                }
            }
        }
        best.map(|(name, _)| name)
    }

    /// Reload snapshotted formal-procedure receivers from `__this` so calls
    /// like `E` inside a local class body resolve after `detach` / `resume`
    /// re-enters the outlined `Class$__init`.
    pub(in crate::mir::lower) fn restore_formal_proc_captures(
        &mut self,
        this_id: LocalId,
        class_name: &str,
    ) {
        let Some(layout) = self.find_layout(class_name) else {
            return;
        };
        let restores: Vec<(String, i64)> = layout
            .enclosing_captures
            .iter()
            .filter_map(|(name, _)| {
                let source = formal_proc_capture_source_name(name)?;
                let offset = layout.field_offset(name)?;
                Some((source.to_string(), offset))
            })
            .collect();
        for (formal, offset) in restores {
            let object = self.temp(MirType::ObjectRef);
            self.push(
                Op::FieldLoadI64 {
                    dest: object,
                    object: this_id,
                    offset,
                    class_qual: None,
                },
                0..0,
            );
            if let Some(qual) = self
                .layouts
                .values()
                .find(|layout| layout.method_name(&formal).is_some())
                .map(|layout| layout.name.clone())
            {
                self.note_object_qual(object, qual);
            }
            self.formal_proc_bindings.insert(
                formal.clone(),
                FormalProcTarget::Method {
                    object,
                    method: formal,
                },
            );
        }
    }

    /// First matching field on the method/`inspect` receiver chain.
    pub(in crate::mir::lower) fn lookup_method_field(
        &self,
        name: &str,
    ) -> Option<(LocalId, i64, FieldType, Option<String>)> {
        for (this_id, qual) in self.method_this_chain() {
            if let Some((offset, field_ty, object_qual)) =
                self.method_field_info_at(this_id, name, qual)
            {
                return Some((this_id, offset, field_ty, object_qual));
            }
            let encl = enclosing_capture_field_name(name);
            if let Some((offset, field_ty, object_qual)) =
                self.method_field_info_at(this_id, &encl, qual)
            {
                return Some((this_id, offset, field_ty, object_qual));
            }
        }
        None
    }

    /// Like [`Self::lookup_method_field`], but yields the [`Place`] so a
    /// by-reference capture slot reads through its pointer instead of returning
    /// the pointer itself.
    pub(in crate::mir::lower) fn method_field_place(&self, name: &str) -> Option<Place> {
        for (this_id, qual) in self.method_this_chain() {
            if let Some((offset, field_ty, object_qual)) =
                self.method_field_info_at(this_id, name, qual)
            {
                return Some(self.object_field_place(this_id, offset, name, field_ty, object_qual));
            }
            let encl = enclosing_capture_field_name(name);
            if let Some((offset, field_ty, object_qual)) =
                self.method_field_info_at(this_id, &encl, qual)
            {
                return Some(self.object_field_place(
                    this_id,
                    offset,
                    &encl,
                    field_ty,
                    object_qual,
                ));
            }
        }
        None
    }

    /// Whether `object` matches a `when Class` clause: runtime class is `Class`
    /// or a subclass (`X in Class`, §4.8) — not the static `ref` qualification.
    pub(in crate::mir::lower) fn lower_inspect_when_match(
        &mut self,
        object: LocalId,
        when_class: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let matching_ids: Vec<i64> = self
            .layouts
            .values()
            .filter(|layout| {
                layout.name.eq_ignore_ascii_case(when_class)
                    || layout.declared_name.eq_ignore_ascii_case(when_class)
                    || is_subclass_of(&layout.name, when_class, self.classes)
                    || is_subclass_of(
                        layout::declared_class_name(&layout.name),
                        when_class,
                        self.classes,
                    )
            })
            .map(|layout| layout.class_id)
            .collect();

        if matching_ids.is_empty() {
            let dest = self.temp(MirType::Bool);
            self.push(Op::ConstBool { dest, value: false }, span);
            return Ok(dest);
        }

        let class_id = self.temp(MirType::I64);
        self.push(
            Op::ObjectClassIdSafe {
                dest: class_id,
                object,
            },
            span.clone(),
        );

        let mut result: Option<LocalId> = None;
        for expected_id in matching_ids {
            let expected = self.temp(MirType::I64);
            self.push(
                Op::ConstI64 {
                    dest: expected,
                    value: expected_id,
                },
                0..0,
            );
            let eq = self.temp(MirType::Bool);
            self.push(
                Op::Compare {
                    dest: eq,
                    op: CmpOp::Eq,
                    left: class_id,
                    right: expected,
                },
                span.clone(),
            );
            result = Some(match result {
                None => eq,
                Some(prev) => {
                    let dest = self.temp(MirType::Bool);
                    self.push(
                        Op::Binary {
                            dest,
                            op: BinOp::Or,
                            left: prev,
                            right: eq,
                        },
                        span.clone(),
                    );
                    dest
                }
            });
        }

        let matched = result.expect("matching_ids non-empty");
        let is_none = self.lower_object_is_none(object, span.clone());
        let not_none = self.temp(MirType::Bool);
        self.push(
            Op::Unary {
                dest: not_none,
                op: UnOp::Not,
                src: is_none,
            },
            0..0,
        );
        let dest = self.temp(MirType::Bool);
        self.push(
            Op::Binary {
                dest,
                op: BinOp::And,
                left: not_none,
                right: matched,
            },
            span,
        );
        Ok(dest)
    }

    pub(in crate::mir::lower) fn lower_object_is_none(
        &mut self,
        object: LocalId,
        span: Span,
    ) -> LocalId {
        let dest = self.temp(MirType::Bool);
        self.push(Op::ObjectIsNone { dest, object }, span);
        dest
    }

    pub(in crate::mir::lower) fn lower_object_is_not_none(
        &mut self,
        object: LocalId,
        span: Span,
    ) -> LocalId {
        let is_none = self.lower_object_is_none(object, span);
        let dest = self.temp(MirType::Bool);
        self.push(
            Op::Unary {
                dest,
                op: UnOp::Not,
                src: is_none,
            },
            0..0,
        );
        dest
    }

    /// §3.3.4 `X is C` / `X in C`. Always uses the object's runtime `class_id`
    /// (object header), ignoring `qua`/`this` reference qualification.
    pub(in crate::mir::lower) fn lower_object_relation(
        &mut self,
        op: RelationOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let class_name = class_identifier_from_expr(right).ok_or_else(|| {
            spanned_error(
                "object relation requires a class identifier on the right",
                span.clone(),
            )
        })?;
        let object = self.lower_expr(left)?;
        if self.local_ty(object) != MirType::ObjectRef {
            return Err(spanned_error(
                format!("relation '{op:?}' requires an object-reference left operand"),
                span,
            ));
        }

        let matching_ids: Vec<i64> = self
            .layouts
            .values()
            .filter(|layout| match op {
                RelationOp::Is => layout.name.eq_ignore_ascii_case(class_name),
                RelationOp::In => {
                    layout.name.eq_ignore_ascii_case(class_name)
                        || is_subclass_of(&layout.name, class_name, self.classes)
                }
                _ => false,
            })
            .map(|layout| layout.class_id)
            .collect();

        if matching_ids.is_empty() {
            return Err(spanned_error(
                format!("undefined class '{class_name}' in object relation"),
                span,
            ));
        }

        let class_id = self.temp(MirType::I64);
        self.push(
            Op::ObjectClassIdSafe {
                dest: class_id,
                object,
            },
            span.clone(),
        );

        let mut result: Option<LocalId> = None;
        for expected_id in matching_ids {
            let expected = self.temp(MirType::I64);
            self.push(
                Op::ConstI64 {
                    dest: expected,
                    value: expected_id,
                },
                0..0,
            );
            let eq = self.temp(MirType::Bool);
            self.push(
                Op::Compare {
                    dest: eq,
                    op: CmpOp::Eq,
                    left: class_id,
                    right: expected,
                },
                span.clone(),
            );
            result = Some(match result {
                None => eq,
                Some(prev) => {
                    let combined = self.temp(MirType::Bool);
                    self.push(
                        Op::Binary {
                            dest: combined,
                            op: BinOp::Or,
                            left: prev,
                            right: eq,
                        },
                        span.clone(),
                    );
                    combined
                }
            });
        }
        Ok(result.expect("matching_ids non-empty"))
    }

    /// Lowers a text-frame attribute read (`t.length` / `t.pos` / `t.more` /
    /// `t.getchar` / `t.constant` / `t.start` / `t.main`). Unsupported Chapter 8
    /// attributes are hard errors naming the attribute so callers get a clear
    /// diagnostic.
    pub(in crate::mir::lower) fn lower_upcase_arg(
        &mut self,
        call: &ProcedureCall,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let name = call.name.to_ascii_lowercase();
        if call.arguments.len() != 1 {
            return Err(spanned_error(
                format!("{name} expects 1 argument, found {}", call.arguments.len()),
                span,
            ));
        }
        let frame = self.lower_expr(&call.arguments[0])?;
        if self.local_ty(frame) != MirType::Text {
            return Err(spanned_error(
                format!("{name} requires a text argument"),
                call.arguments[0].span.clone(),
            ));
        }
        Ok(frame)
    }

    pub(in crate::mir::lower) fn lower_text_attribute(
        &mut self,
        frame: LocalId,
        attribute: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        match TextIntrinsic::parse(attribute) {
            Some(TextIntrinsic::Length) => {
                let dest = self.temp(MirType::I64);
                self.push(Op::TextLength { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Constant) => {
                let dest = self.temp(MirType::Bool);
                self.push(Op::TextConstant { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Start) => {
                let dest = self.temp(MirType::I64);
                self.push(Op::TextStart { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Main) => {
                let dest = self.temp(MirType::Text);
                self.push(Op::TextMain { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Pos) => {
                let dest = self.temp(MirType::I64);
                self.push(Op::TextPos { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::More) => {
                let dest = self.temp(MirType::Bool);
                self.push(Op::TextMore { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Getchar) => {
                let dest = self.temp(MirType::I64);
                self.push(Op::TextGetchar { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Getint) => {
                let dest = self.temp(MirType::I64);
                self.push(Op::TextGetint { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Getfrac) => {
                let dest = self.temp(MirType::I64);
                self.push(Op::TextGetfrac { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Getreal) => {
                let dest = self.temp(MirType::F64);
                self.push(Op::TextGetreal { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Strip) => {
                let dest = self.temp(MirType::Text);
                self.push(Op::TextStrip { dest, frame }, span);
                Ok(dest)
            }
            Some(_) => Err(spanned_error(
                format!(
                    "MIR lowering: text attribute '{attribute}' is not supported in the native backend yet"
                ),
                span,
            )),
            None => Err(spanned_error(
                format!("text type has no attribute '{attribute}'"),
                span,
            )),
        }
    }

    /// Receiver for text procedures (`setpos` / `putchar` / …).
    ///
    /// Bare `t.putchar(…)` mutates the variable's frame in place. Any other
    /// expression receiver gets a fresh descriptor (`notext` + `:-`) that
    /// shares the character object but keeps an independent POS, matching
    /// Standard text-value semantics and the interpreter.
    pub(in crate::mir::lower) fn text_procedure_receiver(
        &mut self,
        object: &Expr,
        object_id: LocalId,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if matches!(&object.kind, ExprKind::Variable(Variable::Simple(_))) {
            return Ok(object_id);
        }
        let dest = self.temp(MirType::Text);
        self.push(Op::TextNotext { dest }, span.clone());
        self.push(
            Op::TextRefAssign {
                dest,
                src: object_id,
            },
            span,
        );
        Ok(dest)
    }

    /// Lowers `t.setpos(i)` / `t.getchar()` (and rejects other text procedures).
    pub(in crate::mir::lower) fn lower_text_remote_call(
        &mut self,
        frame: LocalId,
        attribute: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<LocalId, CompileError> {
        match TextIntrinsic::parse(attribute) {
            Some(TextIntrinsic::Setpos) => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("setpos expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let index = self.lower_expr(&arguments[0])?;
                if self.local_ty(index) != MirType::I64 {
                    return Err(spanned_error(
                        "setpos requires an integer argument",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::TextSetpos { frame, index }, span);
                // Statement-form remote calls still need an expression result;
                // match the interpreter's dummy integer.
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            Some(TextIntrinsic::Getchar) => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("getchar expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::TextGetchar { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Putchar) => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("putchar expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let ch = self.lower_expr(&arguments[0])?;
                if self.local_ty(ch) != MirType::I64 {
                    return Err(spanned_error(
                        "putchar requires a character argument",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::TextPutchar { frame, ch }, span);
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            Some(TextIntrinsic::Getint) => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("getint expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::TextGetint { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Getfrac) => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("getfrac expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::TextGetfrac { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Putint) => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("putint expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let value = self.coerce_value(
                    MirType::I64,
                    value,
                    "putint requires an integer argument",
                    arguments[0].span.clone(),
                )?;
                self.push(Op::TextPutint { frame, value }, span);
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            Some(TextIntrinsic::Putfrac) => {
                if arguments.len() != 2 {
                    return Err(spanned_error(
                        format!("putfrac expects 2 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let places = self.lower_expr(&arguments[1])?;
                if self.local_ty(value) != MirType::I64 {
                    return Err(spanned_error(
                        "putfrac requires an integer value argument",
                        arguments[0].span.clone(),
                    ));
                }
                if self.local_ty(places) != MirType::I64 {
                    return Err(spanned_error(
                        "putfrac requires an integer places argument",
                        arguments[1].span.clone(),
                    ));
                }
                self.push(
                    Op::TextPutfrac {
                        frame,
                        value,
                        places,
                    },
                    span,
                );
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            Some(TextIntrinsic::Getreal) => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("getreal expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::F64);
                self.push(Op::TextGetreal { dest, frame }, span);
                Ok(dest)
            }
            Some(TextIntrinsic::Putfix) => {
                if arguments.len() != 2 {
                    return Err(spanned_error(
                        format!("putfix expects 2 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let places = self.lower_expr(&arguments[1])?;
                let value = self.coerce_value(
                    MirType::F64,
                    value,
                    "putfix requires a real value argument",
                    arguments[0].span.clone(),
                )?;
                if self.local_ty(places) != MirType::I64 {
                    return Err(spanned_error(
                        "putfix requires an integer places argument",
                        arguments[1].span.clone(),
                    ));
                }
                self.push(
                    Op::TextPutfix {
                        frame,
                        value,
                        places,
                    },
                    span,
                );
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            Some(TextIntrinsic::Putreal) => {
                if arguments.len() != 2 {
                    return Err(spanned_error(
                        format!("putreal expects 2 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let places = self.lower_expr(&arguments[1])?;
                let exp_digits = if self.local_ty(value) == MirType::LongF64 {
                    3
                } else {
                    2
                };
                let value = self.coerce_value(
                    MirType::F64,
                    value,
                    "putreal requires a real value argument",
                    arguments[0].span.clone(),
                )?;
                if self.local_ty(places) != MirType::I64 {
                    return Err(spanned_error(
                        "putreal requires an integer places argument",
                        arguments[1].span.clone(),
                    ));
                }
                self.push(
                    Op::TextPutreal {
                        frame,
                        value,
                        places,
                        exp_digits,
                    },
                    span,
                );
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            Some(TextIntrinsic::Sub) => {
                if arguments.len() != 2 {
                    return Err(spanned_error(
                        format!("sub expects 2 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let i = self.lower_expr(&arguments[0])?;
                let i = self.coerce_value(
                    MirType::I64,
                    i,
                    "sub requires integer arguments",
                    arguments[0].span.clone(),
                )?;
                let n = self.lower_expr(&arguments[1])?;
                let n = self.coerce_value(
                    MirType::I64,
                    n,
                    "sub requires integer arguments",
                    arguments[1].span.clone(),
                )?;
                let dest = self.temp(MirType::Text);
                self.push(Op::TextSub { dest, frame, i, n }, span);
                Ok(dest)
            }
            Some(
                TextIntrinsic::Length
                | TextIntrinsic::Pos
                | TextIntrinsic::More
                | TextIntrinsic::Strip,
            ) => {
                // Parameterless attributes may also be written with `()`.
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("{attribute} expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                self.lower_text_attribute(frame, attribute, span)
            }
            Some(_) => Err(spanned_error(
                format!(
                    "MIR lowering: text procedure '{attribute}' is not supported in the native backend yet"
                ),
                span,
            )),
            None => Err(spanned_error(
                format!("text type has no procedure attribute '{attribute}'"),
                span,
            )),
        }
    }

    pub(in crate::mir::lower) fn lower_object_generator(
        &mut self,
        generator: &ObjectGenerator,
        span: Span,
    ) -> Result<(), CompileError> {
        let _ = self.lower_new_object(&generator.class_name, &generator.arguments, span)?;
        Ok(())
    }

    /// Lowers `object qua ClassName`. `none` stays `none`; a reference is
    /// copied when its qualification matches `class_name` or is a subclass
    /// (qualification must match `class_name` or a subclass).
    pub(in crate::mir::lower) fn lower_qua(
        &mut self,
        object: &Expr,
        class_name: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let object_id = self.lower_expr(object)?;
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

        // Unqualified refs (`none`, SIMSET `suc`/`pred` handles, …): `qua`
        // still asserts the static target class for subsequent remote access.
        if !self.ref_qual.contains_key(&object_id) {
            let dest = self.temp(MirType::ObjectRef);
            self.push(
                Op::Copy {
                    dest,
                    src: object_id,
                },
                span,
            );
            self.note_object_qual(dest, target_name);
            return Ok(dest);
        }

        let source_qual = self.ref_qual.get(&object_id).cloned().ok_or_else(|| {
            spanned_error(
                "MIR lowering: internal error: missing qualification for qua operand",
                span.clone(),
            )
        })?;

        if source_qual.eq_ignore_ascii_case(class_name)
            || is_subclass_of(&source_qual, class_name, self.classes)
            || is_subclass_of(class_name, &source_qual, self.classes)
        {
            let dest = self.temp(MirType::ObjectRef);
            self.push(
                Op::Copy {
                    dest,
                    src: object_id,
                },
                span,
            );
            // Keep the operand's instance qualifier only while it is at least
            // as precise as the `qua` target. A *super*class one — `towns.first`
            // qualified `Head`/`Link`, then `qua town` (simtst96) — would send
            // every later field access on `dest` to the wrong WasmGC struct
            // type, and `ref.cast` to a supertype's own struct traps.
            let instance = self.instance_layout_name(object_id).filter(|instance| {
                instance.eq_ignore_ascii_case(&target_name)
                    || is_subclass_of(instance, &target_name, self.classes)
            });
            self.set_local_class_qual(dest, instance.unwrap_or_else(|| target_name.clone()));
            self.ref_qual.insert(dest, target_name);
            Ok(dest)
        } else {
            Err(spanned_error(
                format!("object of class '{source_qual}' cannot be qualified as '{class_name}'"),
                span,
            ))
        }
    }

    /// Lowers `this ClassName` inside a class method body. Flat classes only
    /// (prefix qualification is validated against the method's class name).
    pub(in crate::mir::lower) fn lower_this(
        &mut self,
        class_name: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let this_id = self
            .method_this
            .ok_or_else(|| spanned_error("'this' used outside object context", span.clone()))?;
        let method_class = self.ref_qual.get(&this_id).cloned().ok_or_else(|| {
            spanned_error(
                "MIR lowering: internal error: missing class qualification for 'this'",
                span.clone(),
            )
        })?;
        let method_declared = layout::declared_class_name(&method_class);
        let same = class_name.eq_ignore_ascii_case(&method_class)
            || class_name.eq_ignore_ascii_case(method_declared);
        if same {
            let dest = self.temp(MirType::ObjectRef);
            self.push(Op::Copy { dest, src: this_id }, span.clone());
            self.note_object_qual(dest, method_class);
            return Ok(dest);
        }
        // Prefixed instance (`a class b`): prefix and object share one block.
        if is_subclass_of(&method_class, class_name, self.classes)
            || is_subclass_of(method_declared, class_name, self.classes)
        {
            let dest = self.temp(MirType::ObjectRef);
            self.push(Op::Copy { dest, src: this_id }, span.clone());
            let resolved = self
                .find_layout_at(class_name, Some(&span))
                .map(|layout| layout.name.clone())
                .unwrap_or_else(|| class_name.to_string());
            self.note_object_qual(dest, resolved);
            return Ok(dest);
        }
        // Nested local class: only walk __simrt_enclosing when the layout has it.
        if self.layout_has_enclosing_object(&method_class) {
            return self.load_enclosing_for_class(this_id, class_name, span);
        }
        Err(spanned_error(
            format!("'{class_name}' is not a prefix class of '{method_class}'"),
            span,
        ))
    }

    pub(in crate::mir::lower) fn layout_has_enclosing_object(&self, class_qual: &str) -> bool {
        self.find_layout(class_qual).is_some_and(|layout| {
            layout
                .fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME))
        })
    }

    /// Walk `__simrt_enclosing` from `start` until an object qualifies as
    /// `class_name` (nested local classes — simtst76 `This A`).
    pub(in crate::mir::lower) fn load_enclosing_for_class(
        &mut self,
        start: LocalId,
        class_name: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let mut current = start;
        for _ in 0..32 {
            if let Some(qual) = self.ref_qual.get(&current).cloned() {
                let declared = layout::declared_class_name(&qual);
                if class_name.eq_ignore_ascii_case(&qual)
                    || class_name.eq_ignore_ascii_case(declared)
                {
                    let dest = self.temp(MirType::ObjectRef);
                    self.push(Op::Copy { dest, src: current }, span.clone());
                    let resolved = self
                        .find_layout_at(class_name, Some(&span))
                        .map(|layout| layout.name.clone())
                        .unwrap_or(qual);
                    self.note_object_qual(dest, resolved);
                    return Ok(dest);
                }
            }
            let (next, outer_qual) = self.load_enclosing_object(current, span.clone())?;
            current = next;
            if class_name.eq_ignore_ascii_case(&outer_qual)
                || class_name.eq_ignore_ascii_case(layout::declared_class_name(&outer_qual))
            {
                return Ok(current);
            }
        }
        Err(spanned_error(
            format!("'{class_name}' is not a prefix class of the current object"),
            span,
        ))
    }

    /// A class declared inside another class body sees that class's attributes,
    /// but they belong to a different object, reached through the enclosing
    /// link. Only worth walking once the receiver chain has been exhausted.
    pub(in crate::mir::lower) fn try_lower_enclosing_object_method(
        &mut self,
        this_id: LocalId,
        name: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<Option<LocalId>, CompileError> {
        let mut receiver = this_id;
        for _ in 0..32 {
            let Some(qual) = self.ref_qual.get(&receiver).cloned() else {
                return Ok(None);
            };
            if !self.layout_has_enclosing_object(&qual) {
                return Ok(None);
            }
            let (enclosing, _) = self.load_enclosing_object(receiver, span.clone())?;
            if self.object_method_name(enclosing, name).is_some() {
                return self
                    .lower_object_method_call(enclosing, name, arguments, span)
                    .map(Some);
            }
            receiver = enclosing;
        }
        Ok(None)
    }

    pub(in crate::mir::lower) fn load_enclosing_object(
        &mut self,
        object: LocalId,
        span: Span,
    ) -> Result<(LocalId, String), CompileError> {
        let qual = self.ref_qual.get(&object).cloned().ok_or_else(|| {
            spanned_error(
                "MIR lowering: internal error: missing class qualification for enclosing load",
                span.clone(),
            )
        })?;
        let (field_offset, outer_qual) = {
            let layout = self.find_layout(&qual).ok_or_else(|| {
                spanned_error(
                    format!("MIR lowering: internal error: missing layout for '{qual}'"),
                    span.clone(),
                )
            })?;
            let field = layout
                .fields
                .iter()
                .find(|field| field.name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME))
                .ok_or_else(|| {
                    spanned_error(
                        format!("MIR lowering: class '{qual}' has no textually enclosing object"),
                        span.clone(),
                    )
                })?;
            let outer_qual = field.class_qual.clone().ok_or_else(|| {
                spanned_error(
                    format!(
                        "MIR lowering: internal error: missing enclosing qualification on '{qual}'"
                    ),
                    span.clone(),
                )
            })?;
            (field.offset, outer_qual)
        };
        let dest = self.temp(MirType::ObjectRef);
        self.push(
            Op::FieldLoadI64 {
                dest,
                object,
                offset: field_offset,
                class_qual: None,
            },
            span.clone(),
        );
        let resolved = self
            .find_layout_at(&outer_qual, None)
            .map(|layout| layout.name.clone())
            .unwrap_or(outer_qual.clone());
        self.note_object_qual(dest, resolved);
        Ok((dest, outer_qual))
    }

    pub(in crate::mir::lower) fn lower_new_object(
        &mut self,
        class_name: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<LocalId, CompileError> {
        self.lower_new_object_ex(class_name, arguments, span, true)
    }

    /// Allocate a class instance. When `call_init` is false, constructor
    /// parameters are stored into fields but `Class$__init` is not called
    /// (prefixed blocks inline the body so virtual labels can match).
    pub(in crate::mir::lower) fn lower_new_object_ex(
        &mut self,
        class_name: &str,
        arguments: &[Expr],
        span: Span,
        call_init: bool,
    ) -> Result<LocalId, CompileError> {
        let (qual_name, class_id, size, needs_init, constructor_params, text_fields, captures) = {
            let layout = self
                .find_layout_at(class_name, Some(&span))
                .ok_or_else(|| {
                    spanned_error(
                        format!("MIR lowering: undefined class '{class_name}'"),
                        span.clone(),
                    )
                })?;
            let capture_names: Vec<String> = layout
                .enclosing_captures
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            let text_fields: Vec<i64> = layout
                .fields
                .iter()
                .filter(|field| {
                    field.ty == FieldType::Text
                        && !capture_names
                            .iter()
                            .any(|name| name.eq_ignore_ascii_case(&field.name))
                })
                .map(|field| field.offset)
                .collect();
            let captures: Vec<(String, i64, FieldType)> = layout
                .enclosing_captures
                .iter()
                .filter_map(|(name, field_ty)| {
                    layout
                        .field_offset(name)
                        .map(|offset| (name.clone(), offset, *field_ty))
                })
                .collect();
            (
                layout.name.clone(),
                layout.class_id,
                layout.size,
                layout.needs_init,
                layout.constructor_params.clone(),
                text_fields,
                captures,
            )
        };

        if arguments.len() != constructor_params.len() {
            return Err(spanned_error(
                format!(
                    "class '{}' expects {} parameters, found {}",
                    qual_name,
                    constructor_params.len(),
                    arguments.len()
                ),
                span,
            ));
        }

        let mut arg_ids = Vec::with_capacity(arguments.len());
        for (argument, (_param_name, field_ty)) in arguments.iter().zip(&constructor_params) {
            let expected = mir_type_for_field(*field_ty);
            let value = self.lower_expr(argument)?;
            let value = self.coerce_value(
                expected,
                value,
                format!(
                    "argument type mismatch for class '{qual_name}' parameter (expected {expected})"
                ),
                argument.span.clone(),
            )?;
            arg_ids.push(value);
        }

        let dest = self.temp(MirType::ObjectRef);
        self.note_object_qual(dest, qual_name.clone());
        self.push(
            Op::NewObject {
                dest,
                class_id,
                size,
            },
            span.clone(),
        );
        // Nested local class: link to the textually enclosing object for `This Outer`.
        if let Some(outer) = self.method_this {
            if let Some(layout) = self.find_layout(&qual_name) {
                if let Some(offset) = layout.field_offset(ENCLOSING_OBJECT_FIELD_NAME) {
                    self.push(
                        Op::FieldStoreI64 {
                            object: dest,
                            offset,
                            value: outer,
                            class_qual: Some(qual_name.clone()),
                        },
                        span.clone(),
                    );
                }
            }
        }
        // Snapshot enclosing locals onto the instance (interpreter
        // `enclosing_locals`) before the class body runs.
        for (name, offset, field_ty) in &captures {
            let source_name = enclosing_capture_source_name(name)
                .or_else(|| formal_proc_capture_source_name(name))
                .unwrap_or(name.as_str());
            if let Some(src) = formal_proc_capture_source_name(name)
                .and_then(|formal| self.resolve_formal_proc_target(formal))
                .and_then(|target| match target {
                    FormalProcTarget::Method { object, .. } => Some(*object),
                    FormalProcTarget::Procedure(_) => None,
                })
            {
                self.push(
                    Op::FieldStoreI64 {
                        object: dest,
                        offset: *offset,
                        value: src,
                        class_qual: Some(qual_name.clone()),
                    },
                    span.clone(),
                );
                continue;
            }
            let by_ref = self.class_capture_by_reference(&qual_name, name, *field_ty);
            let src = if by_ref {
                self.capture_source_address(source_name, name, *field_ty, span.clone())
                    // A `ref` capture with no home in this scope is still read
                    // *through* the slot by nested by-value captures
                    // ([`Self::capture_source_value`]), so give it an empty cell
                    // rather than a null pointer. Free-variable scanning picks up
                    // class names used in `is`/`qua`/`new` too (simtst96's `car`,
                    // `townpoint`), and those never have a home.
                    .or_else(|| {
                        (*field_ty == FieldType::ObjectRef)
                            .then(|| self.empty_capture_cell(span.clone()))
                    })
            } else {
                self.capture_source_value(source_name, name, *field_ty, span.clone())
            };
            if let Some(src) = src {
                self.push(
                    Op::FieldStoreI64 {
                        object: dest,
                        offset: *offset,
                        value: src,
                        class_qual: Some(qual_name.clone()),
                    },
                    span.clone(),
                );
            }
        }
        // calloc leaves text slots as NULL; text assign ops no-op on NULL
        // dests, so install notext frames before any user code runs.
        for offset in text_fields {
            let frame = self.temp(MirType::Text);
            self.push(Op::TextNotext { dest: frame }, span.clone());
            self.push(
                Op::FieldStoreI64 {
                    object: dest,
                    offset,
                    value: frame,
                    class_qual: Some(qual_name.clone()),
                },
                span.clone(),
            );
        }
        // Ring self-link after enclosing-capture / text installs. Capture
        // slots are raw LocalAddr words in ObjectRef-typed fields under
        // WasmGC; installing them before `init_head` means a mistaken
        // field-index store cannot leave SUC/PRED half-initialized, and
        // `init_head` is the last word on the ring shape before user code
        // (simtst96: `towns.empty` vs `towns.first` after `new head`).
        if crate::simulation::is_head_class(&qual_name) {
            self.push(Op::SimsetInitHead { head: dest }, span.clone());
        }
        if needs_init && call_init && self.class_runs_on_own_stack(&qual_name) {
            // The body is a component on its own stack: record the constructor
            // parameters, then hand the object to the sequencing runtime. It
            // runs attached (7.1) until it detaches or terminates.
            let mut args = Vec::with_capacity(1 + arg_ids.len());
            args.push(dest);
            args.extend(arg_ids.iter().copied());
            let init_name = mangle_init_name(&qual_name);
            self.push(
                Op::Call {
                    dest: None,
                    name: init_name,
                    args,
                },
                span.clone(),
            );
            self.emit_seq_object_generation(dest, &qual_name, span.clone())?;
        } else if needs_init && call_init {
            let mut args = Vec::with_capacity(1 + arg_ids.len());
            args.push(dest);
            args.extend(arg_ids.iter().copied());
            let init_name = mangle_init_name(&qual_name);
            self.push(
                Op::Call {
                    dest: None,
                    name: init_name,
                    args,
                },
                span.clone(),
            );
            // Write mutated enclosing captures back to caller locals.
            self.writeback_enclosing_captures(dest, &qual_name, &[], span.clone())?;
        } else if needs_init && !call_init {
            // Prefixed-block path: store constructor params now; body runs
            // inline in the caller so virtual labels can match.
            for ((name, field_ty), &value) in constructor_params.iter().zip(arg_ids.iter()) {
                let offset = self
                    .find_layout(&qual_name)
                    .and_then(|layout| layout.field_offset(name))
                    .ok_or_else(|| {
                        spanned_error(
                            format!("MIR lowering: missing field '{name}' for class '{qual_name}'"),
                            span.clone(),
                        )
                    })?;
                self.write_constructor_param_field(dest, offset, *field_ty, value, span.clone());
            }
        }
        if is_basicio_class(&qual_name)
            && matches!(
                qual_name.to_ascii_lowercase().as_str(),
                "infile"
                    | "outfile"
                    | "printfile"
                    | "directfile"
                    | "inbytefile"
                    | "outbytefile"
                    | "directbytefile"
            )
        {
            let path = if let Some(&path) = arg_ids.first() {
                path
            } else {
                let empty = self.temp(MirType::Text);
                self.push(Op::TextNotext { dest: empty }, span.clone());
                empty
            };
            let mode = match qual_name.to_ascii_lowercase().as_str() {
                "infile" => 0,
                "outfile" => 1,
                "inbytefile" => 2,
                "outbytefile" => 3,
                "directfile" => 4,
                "directbytefile" => 5,
                "printfile" => 6,
                _ => 1,
            };
            self.push(
                Op::CallBasicioRegisterFile {
                    object: dest,
                    path,
                    mode,
                },
                span.clone(),
            );
        }
        Ok(dest)
    }

    pub(in crate::mir::lower) fn find_layout(&self, class_name: &str) -> Option<&ClassLayout> {
        self.find_layout_at(class_name, None)
    }

    /// Resolve a class layout by source name. When `use_span` is set and several
    /// same-named classes exist in disjoint scopes, pick the declaration that
    /// most closely precedes the use site.
    pub(in crate::mir::lower) fn find_layout_at(
        &self,
        class_name: &str,
        use_span: Option<&crate::error::Span>,
    ) -> Option<&ClassLayout> {
        // Span-qualified names (`A@1553`, `C@1670`) must resolve exactly
        // (ref_qual / mangled symbols — simtst76 Resume(Y) into wrong `C`).
        if class_name.contains('@') {
            return self
                .layouts
                .values()
                .find(|layout| layout.name.eq_ignore_ascii_case(class_name));
        }
        let mut matches: Vec<&ClassLayout> = self
            .layouts
            .values()
            .filter(|layout| {
                layout.name.eq_ignore_ascii_case(class_name)
                    || layout.declared_name.eq_ignore_ascii_case(class_name)
            })
            .collect();
        if matches.is_empty() {
            return None;
        }
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        // Unqualified `new A` among homonyms needs the use-site span; when
        // ref_qual stores a unique exact name (`C` vs `C@1670`) span is absent.
        if use_span.is_none() {
            let exact: Vec<&&ClassLayout> = matches
                .iter()
                .filter(|layout| layout.name.eq_ignore_ascii_case(class_name))
                .collect();
            if exact.len() == 1 {
                return Some(*exact[0]);
            }
        }
        let use_span = use_span?;
        matches.sort_by_key(|layout| layout.decl_span.start);
        matches
            .iter()
            .rev()
            .find(|layout| layout.decl_span.start <= use_span.start)
            .copied()
            .or_else(|| matches.last().copied())
    }

    /// Whether `id` is a formal parameter of the function being lowered
    /// (parameters occupy the first [`Self::param_count`] locals).
    ///
    /// A formal that happens to share a name with one of the class's
    /// enclosing-capture fields is a *different* variable: the capture stands
    /// for the enclosing block's variable, which the formal shadows for the
    /// whole method text (§5.5). Snapshotting or writing back through it would
    /// overwrite the argument (simtst96: `procedure put(h)` inside a class that
    /// captures an outer `ref(head) h`).
    pub(in crate::mir::lower) fn local_is_current_formal(&self, id: LocalId) -> bool {
        id.0 < self.param_count
    }

    pub(in crate::mir::lower) fn scope_lookup(&self, name: &str) -> Option<LocalId> {
        if let Some(&id) = self.scope.get(name) {
            return Some(id);
        }
        self.scope
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, id)| *id)
    }

    pub(in crate::mir::lower) fn layout_for_object(&self, object: LocalId) -> Option<&ClassLayout> {
        let qual = self.ref_qual.get(&object)?;
        self.find_layout(qual)
    }

    /// Whether a connected BASICIO receiver should absorb bare method `method`.
    /// `inspect InFile do outtext(…)` must fall through to SYSOUT — InFile has
    /// no output procedures (simtst96 Windows: free `outtext`/`outimage` were
    /// writing into the infile image / stream).
    pub(in crate::mir::lower) fn object_supports_basicio_method(
        &self,
        object: LocalId,
        method: &str,
    ) -> bool {
        self.ref_qual
            .get(&object)
            .is_some_and(|qual| basicio_class_supports_free_method(qual, method))
    }

    pub(in crate::mir::lower) fn object_is_printfile(&self, object: LocalId) -> bool {
        self.ref_qual
            .get(&object)
            .is_some_and(|qual| qual.eq_ignore_ascii_case("PrintFile"))
    }

    pub(in crate::mir::lower) fn object_method_name(
        &self,
        object: LocalId,
        attribute: &str,
    ) -> Option<String> {
        let qual = self.ref_qual.get(&object).map(String::as_str)?;
        self.object_method_name_at(object, attribute, qual)
    }

    /// Like [`Self::object_method_name`], but attribute visibility is judged at
    /// `qual` (needed when `inspect this a` overwrites `ref_qual` on an object
    /// that is also the enclosing class instance — fall back uses the stacked
    /// qualification).
    pub(in crate::mir::lower) fn object_method_name_at(
        &self,
        object: LocalId,
        attribute: &str,
        qual: &str,
    ) -> Option<String> {
        let layout = self
            .find_layout(qual)
            .or_else(|| self.layout_for_object(object))?;
        let access = self.access_level();
        if let Some(binding) = visible_attribute_binding(access, qual, attribute, self.classes) {
            let mangled = match binding.kind {
                AttributeKind::Virtual => {
                    // Dispatch default: match at `qual`'s runtime family. Full
                    // virtual dispatch (below) still covers every subclass
                    // class_id, including a virtual left unmatched at the
                    // declaring level (simtst55/57: `A` declares `virtual:
                    // procedure P` and only subclasses match it).
                    virtual_match_level(qual, &binding.level, attribute, self.classes)
                        .or_else(|| {
                            virtual_match_level(
                                &layout.name,
                                &binding.level,
                                attribute,
                                self.classes,
                            )
                        })
                        .and_then(|match_level| {
                            let proc_name =
                                declared_procedure_name(&match_level, attribute, self.classes)?;
                            Some(mangle_method_name(&match_level, &proc_name))
                        })
                }
                AttributeKind::Procedure => {
                    declared_procedure_name(&binding.level, attribute, self.classes)
                        .map(|proc_name| mangle_method_name(&binding.level, &proc_name))
                }
                AttributeKind::Variable => return None,
            };
            if let Some(mangled) = mangled
                && (self.signatures.contains_key(&mangled)
                    || self.lookup_name_param_proc(&mangled).is_some())
            {
                return Some(mangled);
            }
            if binding.kind == AttributeKind::Virtual {
                return self
                    .virtual_dispatch_targets(&binding.level, attribute)
                    .into_iter()
                    .next_back()
                    .map(|(_, name)| name);
            }
            return None;
        }
        // Prefixed-block / nested-block procedures are collected onto the
        // enclosing class layout (`X$B` for `procedure B` inside `A begin` in
        // class `X`) but are not class attributes for §5.5 visibility. When no
        // attribute of that name exists in the prefix chain, allow the layout
        // method so bare calls still resolve (simtst62). Invisible attributes
        // must not take this path.
        if attribute_declared_in_prefix_chain(qual, attribute, self.classes) {
            return None;
        }
        let method_name = layout.method_name(attribute)?;
        let defining = defining_class_for_method(self.classes, &layout.name, method_name);
        let mangled = mangle_method_name(defining, method_name);
        if self.signatures.contains_key(&mangled) || self.lookup_name_param_proc(&mangled).is_some()
        {
            Some(mangled)
        } else {
            None
        }
    }

    /// Desugars parameterless `obj.m` (no `()`) into the same call path as
    /// `obj.m()`. Methods that expect arguments still error clearly.
    pub(in crate::mir::lower) fn try_lower_parameterless_method(
        &mut self,
        object_id: LocalId,
        attribute: &str,
        span: Span,
    ) -> Result<Option<LocalId>, CompileError> {
        let Some(mangled) = self.object_method_name(object_id, attribute) else {
            return Ok(None);
        };
        let signature = self.signatures.get(&mangled).cloned().ok_or_else(|| {
            spanned_error(
                format!("MIR lowering: internal error: missing signature for method '{mangled}'"),
                span.clone(),
            )
        })?;
        // `params[0]` is `__this`; any further formals require an explicit call.
        // Free-cell / name-thunk expansions are not used on methods here.
        let user_params = signature.params.len().saturating_sub(1)
            - signature.name_thunk_starts.len() * 2
            - signature.formal_proc_param_indices.len()
            - signature.free_cell_params.len();
        if user_params > 0 {
            return Err(spanned_error(
                format!("MIR lowering: method '{attribute}' requires arguments; call it with ()"),
                span,
            ));
        }
        Ok(Some(self.lower_object_method_call(
            object_id,
            attribute,
            &[],
            span,
        )?))
    }

    /// Bare BASICIO identifiers inside `inspect` / prefixed blocks bind to the
    /// connected file object before free SYSIN/SYSOUT stubs (`InChar`, `InInt`,
    /// `pos`, … — simtst85/96).
    pub(in crate::mir::lower) fn try_lower_connected_basicio_identifier(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<Option<LocalId>, CompileError> {
        if !is_basicio_method(name) {
            return Ok(None);
        }
        if self.scope_has_name(name) || self.name_bindings.contains_key(name) {
            return Ok(None);
        }
        let receivers: Vec<LocalId> = self.method_this_chain().map(|(id, _)| id).collect();
        for receiver in receivers {
            if self.object_supports_basicio_method(receiver, name) {
                return Ok(Some(self.lower_basicio_method(
                    receiver,
                    name,
                    &[],
                    span,
                )?));
            }
        }
        Ok(None)
    }

    /// Free BASICIO names under the Standard `inspect SYSIN/SYSOUT` embedding
    /// (`POS`, `SETPOS`, `IMAGE`, …) desugar to the matching terminal object.
    pub(in crate::mir::lower) fn try_lower_free_basicio(
        &mut self,
        name: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<Option<LocalId>, CompileError> {
        if self.scope_has_name(name) || self.name_bindings.contains_key(name) {
            return Ok(None);
        }
        let Some(target) = free_basicio_target(name) else {
            return Ok(None);
        };
        let object = self.temp(MirType::ObjectRef);
        match target {
            FreeBasicioTarget::SysIn => {
                self.push(Op::CallSysIn { dest: object }, span.clone());
                self.note_object_qual(object, "InFile".into());
            }
            FreeBasicioTarget::SysOut => {
                self.push(Op::CallSysOut { dest: object }, span.clone());
                self.note_object_qual(object, "PrintFile".into());
            }
        }
        Ok(Some(
            self.lower_basicio_method(object, name, arguments, span)?,
        ))
    }

    /// Bare identifier naming a 0-argument local/inlined procedure (or a
    /// 0-argument method on `__this`): evaluate it as a call. Used for
    /// Simula's optional-`()` syntax (`if expcom then ...`).
    pub(in crate::mir::lower) fn try_lower_parameterless_procedure(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<Option<LocalId>, CompileError> {
        // Prefer locals/fields — a same-named variable shadows the procedure.
        if self.scope_has_name(name) || self.name_bindings.contains_key(name) {
            return Ok(None);
        }
        if let Some(this_id) = self.method_this
            && self.method_field_info(this_id, name).is_some()
        {
            return Ok(None);
        }
        if let Some(FormalProcTarget::Method { object, method }) =
            self.resolve_formal_proc_target(name).cloned()
        {
            return self.try_lower_parameterless_method(object, &method, span);
        }
        let name = self
            .resolve_formal_procedure_name(name)
            .map(str::to_string)
            .unwrap_or_else(|| name.to_string());
        if let Some(procedure) = self.lookup_name_param_proc(&name) {
            if !procedure.parameters.is_empty() {
                return Ok(None);
            }
            return self.inline_name_procedure(procedure, &[], span, true);
        }
        if let Some(procedure) = self.lookup_ref_alias_proc(&name) {
            if !procedure.parameters.is_empty() {
                return Ok(None);
            }
            return self.inline_ref_alias_procedure(procedure, &[], span, true);
        }
        if let Some(resolved) = self.resolve_known_procedure(&name) {
            // Skip mangled methods (`Class$method`) — those need a receiver.
            if resolved.contains('$') {
                return Ok(None);
            }
            if let Some(signature) = self.signatures.get(&resolved).cloned() {
                let expected = signature.params.len()
                    - signature.name_thunk_starts.len() * 2
                    - signature.free_cell_params.len();
                if expected != 0 && !(signature.external_stub && signature.params.is_empty()) {
                    return Ok(None);
                }
                if signature.result.is_none() {
                    return Ok(None);
                }
                let args = self.lower_call_arguments(&resolved, &signature, &[], span.clone())?;
                let dest = self.temp(signature.result.unwrap());
                self.push(
                    Op::Call {
                        dest: Some(dest),
                        name: resolved,
                        args,
                    },
                    span,
                );
                self.annotate_call_result(dest, &signature);
                return Ok(Some(dest));
            }
        }
        if let Some(this_id) = self.method_this
            && let Some(result) =
                self.try_lower_parameterless_method(this_id, &name, span.clone())?
        {
            return Ok(Some(result));
        }
        if let Some(this_id) = self.method_this {
            if is_basicio_method(&name) && self.object_supports_basicio_method(this_id, &name) {
                return Ok(Some(self.lower_basicio_method(
                    this_id,
                    &name,
                    &[],
                    span,
                )?));
            }
            if is_simset_method(&name) {
                return Ok(Some(self.lower_simset_method(this_id, &name, &[], span)?));
            }
        }
        Ok(None)
    }

    pub(in crate::mir::lower) fn scope_has_name(&self, name: &str) -> bool {
        self.scope.contains_key(name) || self.scope.keys().any(|key| key.eq_ignore_ascii_case(name))
    }

    pub(in crate::mir::lower) fn method_field_info(
        &self,
        this_id: LocalId,
        name: &str,
    ) -> Option<(i64, FieldType, Option<String>)> {
        let qual = self.ref_qual.get(&this_id).map(String::as_str);
        self.method_field_info_at(this_id, name, qual)
    }

    pub(in crate::mir::lower) fn method_field_info_at(
        &self,
        this_id: LocalId,
        name: &str,
        qual: Option<&str>,
    ) -> Option<(i64, FieldType, Option<String>)> {
        let layout = self.layout_for_object(this_id)?;
        let qual = qual.or_else(|| self.ref_qual.get(&this_id).map(String::as_str))?;
        // Compiler slots and already-mangled concatenated storage names
        // (`i$d` in a prefixed-block body) are concrete fields, not source
        // identifiers subject to §5.5.3 visibility.
        let lookup = if name.starts_with("__simrt_")
            || (name.contains('$') && layout.field_offset(name).is_some())
        {
            name.to_string()
        } else if self.access_level_substitutions {
            // Connection / method text: only a variable binding visible at the
            // lexical access level may resolve to an object field (§5.5.3–6).
            match visible_attribute_binding(self.access_level(), qual, name, self.classes) {
                Some(binding) if binding.kind == AttributeKind::Variable => {
                    let concatenated = self.concatenated_class_map();
                    let storage = substitute_remote_attribute(&binding.level, name, &concatenated);
                    if layout.field_offset(&storage).is_some() {
                        storage
                    } else if layout.field_offset(name).is_some() {
                        name.to_string()
                    } else {
                        return None;
                    }
                }
                None => {
                    // Not a visible class attribute — fall through to an
                    // enclosing-block capture if the object has one. Prefer the
                    // mangled `__simrt_encl_*` slot when the class also has a
                    // same-named (hidden/protected) attribute field.
                    let encl = enclosing_capture_field_name(name);
                    if layout
                        .enclosing_captures
                        .iter()
                        .any(|(capture, _)| capture.eq_ignore_ascii_case(&encl))
                        && layout.field_offset(&encl).is_some()
                    {
                        encl
                    } else if layout
                        .enclosing_captures
                        .iter()
                        .any(|(capture, _)| capture.eq_ignore_ascii_case(name))
                        && layout.field_offset(name).is_some()
                    {
                        name.to_string()
                    } else {
                        return None;
                    }
                }
                Some(_) => return None,
            }
        } else {
            name.to_string()
        };
        let offset = layout.field_offset(&lookup)?;
        let field_ty = layout.field_type(&lookup)?;
        let object_qual = self
            .attribute_object_qual(&layout.name, &lookup)
            .or_else(|| self.attribute_object_qual(&layout.name, name))
            .or_else(|| {
                layout
                    .fields
                    .iter()
                    .find(|field| {
                        field.name.eq_ignore_ascii_case(&lookup)
                            || field.name.eq_ignore_ascii_case(name)
                    })
                    .and_then(|field| field.class_qual.clone())
            });
        Some((offset, field_ty, object_qual))
    }

    /// Declared `ref(C)` qualification for a class attribute, when known.
    pub(in crate::mir::lower) fn attribute_object_qual(
        &self,
        class_name: &str,
        attribute: &str,
    ) -> Option<String> {
        let class = self
            .classes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(class_name))
            .map(|(_, class)| class)?;
        for decl in &class.body.declarations {
            if decl
                .items
                .iter()
                .any(|item| item.name.eq_ignore_ascii_case(attribute))
                && let Type::ObjectRef(qual) = &decl.ty
            {
                return Some(qual.clone());
            }
        }
        None
    }

    /// Lowers a SIMSET linkage method (`into`/`out`/`suc`/…).
    pub(in crate::mir::lower) fn lower_simset_method(
        &mut self,
        object: LocalId,
        name: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if self.local_ty(object) != MirType::ObjectRef {
            return Err(spanned_error(
                format!("{name} requires an object reference"),
                span,
            ));
        }
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "out" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("out expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                self.push(Op::SimsetOut { object }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "into" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("into expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let head = self.lower_expr(&arguments[0])?;
                if self.local_ty(head) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "into requires a Head object reference",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::SimsetInto { object, head }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "precede" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("precede expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let ptr = self.lower_expr(&arguments[0])?;
                if self.local_ty(ptr) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "precede requires an object reference",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::SimsetPrecede { object, ptr }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "follow" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("follow expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let ptr = self.lower_expr(&arguments[0])?;
                if self.local_ty(ptr) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "follow requires an object reference",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::SimsetFollow { object, ptr }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "suc" | "first" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("{lower} expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::ObjectRef);
                self.push(Op::SimsetSuc { dest, object }, span);
                // Static type is Linkage; `qua` / `inspect when` refine further.
                self.note_object_qual(dest, "Linkage".into());
                Ok(dest)
            }
            "pred" | "last" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("{lower} expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::ObjectRef);
                self.push(Op::SimsetPred { dest, object }, span);
                self.note_object_qual(dest, "Linkage".into());
                Ok(dest)
            }
            "prev" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("prev expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                // Raw PRED (may be Head); matches interpreter `prev`.
                let dest = self.temp(MirType::ObjectRef);
                self.push(
                    Op::FieldLoadI64 {
                        dest,
                        object,
                        offset: SIMSET_PRED_OFFSET,
                        class_qual: None,
                    },
                    span,
                );
                Ok(dest)
            }
            "empty" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("empty expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::SimsetEmpty { dest, head: object }, span);
                Ok(dest)
            }
            "cardinal" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("cardinal expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::SimsetCardinal { dest, head: object }, span);
                Ok(dest)
            }
            "clear" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("clear expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let loop_bb = self.new_block();
                let body_bb = self.new_block();
                let done_bb = self.new_block();
                self.push(Op::Jump { target: loop_bb }, span.clone());
                self.switch_to(loop_bb);
                let first = self.temp(MirType::ObjectRef);
                self.push(
                    Op::SimsetSuc {
                        dest: first,
                        object,
                    },
                    span.clone(),
                );
                let is_none = self.temp(MirType::Bool);
                self.push(
                    Op::ObjectIsNone {
                        dest: is_none,
                        object: first,
                    },
                    span.clone(),
                );
                self.push(
                    Op::Branch {
                        cond: is_none,
                        then_block: done_bb,
                        else_block: body_bb,
                    },
                    span.clone(),
                );
                self.switch_to(body_bb);
                self.push(Op::SimsetOut { object: first }, span.clone());
                self.push(Op::Jump { target: loop_bb }, span.clone());
                self.switch_to(done_bb);
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            other => Err(spanned_error(
                format!("unsupported SIMSET method '{other}'"),
                span,
            )),
        }
    }

    /// Lowers a BASICIO file method (`open`/`close`/`outtext`/…).
    pub(in crate::mir::lower) fn lower_basicio_method(
        &mut self,
        object: LocalId,
        name: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if self.local_ty(object) != MirType::ObjectRef {
            return Err(spanned_error(
                format!("{name} requires an object reference"),
                span,
            ));
        }
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "open" => {
                if arguments.is_empty() {
                    let dest = self.temp(MirType::Bool);
                    self.push(Op::CallBasicioOpenByte { dest, object }, span);
                    return Ok(dest);
                }
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!(
                            "open expects 0 (bytefile) or 1 text (fileimage) argument, found {}",
                            arguments.len()
                        ),
                        span,
                    ));
                }
                let fileimage = self.lower_expr(&arguments[0])?;
                if self.local_ty(fileimage) != MirType::Text {
                    return Err(spanned_error(
                        "open requires a text fileimage argument",
                        arguments[0].span.clone(),
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::CallBasicioOpen {
                        dest,
                        object,
                        fileimage,
                    },
                    span,
                );
                Ok(dest)
            }
            "close" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("close expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallBasicioClose { dest, object }, span);
                Ok(dest)
            }
            "isopen" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("isopen expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallBasicioIsOpen { dest, object }, span);
                Ok(dest)
            }
            "outtext" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("outtext expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let text = self.lower_expr(&arguments[0])?;
                if self.local_ty(text) != MirType::Text {
                    return Err(spanned_error(
                        "outtext requires a text argument",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::CallBasicioOutText { object, text }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "outchar" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("outchar expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let ch = self.lower_expr(&arguments[0])?;
                if self.local_ty(ch) != MirType::I64 {
                    return Err(spanned_error(
                        "outchar requires a character argument",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::CallBasicioOutChar { object, ch }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "outimage" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("outimage expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                self.push(Op::CallBasicioOutImage { object }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "outrecord" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("outrecord expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                self.push(Op::CallBasicioOutImage { object }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "breakoutimage" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!(
                            "breakoutimage expects 0 arguments, found {}",
                            arguments.len()
                        ),
                        span,
                    ));
                }
                self.push(Op::CallBasicioBreakOutImage { object }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "inimage" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("inimage expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                self.push(Op::CallBasicioInImage { object }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "inrecord" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("inrecord expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallBasicioInRecord { dest, object }, span);
                Ok(dest)
            }
            "inchar" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("inchar expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioInChar { dest, object }, span);
                Ok(dest)
            }
            "endfile" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("endfile expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallBasicioEndfile { dest, object }, span);
                Ok(dest)
            }
            "inbyte" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("inbyte expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioInByte { dest, object }, span);
                Ok(dest)
            }
            "outbyte" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("outbyte expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                if self.local_ty(value) != MirType::I64 {
                    return Err(spanned_error(
                        "outbyte requires an integer argument",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::CallBasicioOutByte { object, value }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "locate" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("locate expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let loc = self.lower_expr(&arguments[0])?;
                if self.local_ty(loc) != MirType::I64 {
                    return Err(spanned_error(
                        "locate requires an integer argument",
                        arguments[0].span.clone(),
                    ));
                }
                self.push(Op::CallBasicioLocate { object, loc }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "location" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("location expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioLocation { dest, object }, span);
                Ok(dest)
            }
            "lastloc" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("lastloc expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioLastloc { dest, object }, span);
                Ok(dest)
            }
            "outreal" | "outfix" => {
                if arguments.len() != 3 {
                    return Err(spanned_error(
                        format!(
                            "{lower} expects 3 arguments (r, n, w), found {}",
                            arguments.len()
                        ),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let value_ty = match self.local_ty(value) {
                    MirType::LongF64 => MirType::LongF64,
                    _ => MirType::F64,
                };
                let value = self.coerce_value(
                    value_ty,
                    value,
                    format!("{lower} requires a real value argument"),
                    arguments[0].span.clone(),
                )?;
                let digits = self.lower_expr(&arguments[1])?;
                let digits = self.coerce_value(
                    MirType::I64,
                    digits,
                    format!("{lower} requires an integer digits argument"),
                    arguments[1].span.clone(),
                )?;
                let width = self.lower_expr(&arguments[2])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    format!("{lower} requires an integer width argument"),
                    arguments[2].span.clone(),
                )?;
                if lower == "outreal" {
                    let exp_digits = if self.object_is_printfile(object) {
                        3
                    } else if value_ty == MirType::LongF64 {
                        3
                    } else {
                        2
                    };
                    self.push(
                        Op::CallBasicioOutReal {
                            object,
                            value,
                            digits,
                            width,
                            exp_digits,
                        },
                        span.clone(),
                    );
                } else {
                    self.push(
                        Op::CallBasicioOutFix {
                            object,
                            value,
                            digits,
                            width,
                        },
                        span.clone(),
                    );
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "outfrac" => {
                if arguments.len() != 3 {
                    return Err(spanned_error(
                        format!(
                            "outfrac expects 3 arguments (i, n, w), found {}",
                            arguments.len()
                        ),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let value = self.coerce_value(
                    MirType::I64,
                    value,
                    "outfrac requires an integer value argument",
                    arguments[0].span.clone(),
                )?;
                let digits = self.lower_expr(&arguments[1])?;
                let digits = self.coerce_value(
                    MirType::I64,
                    digits,
                    "outfrac requires an integer digits argument",
                    arguments[1].span.clone(),
                )?;
                let width = self.lower_expr(&arguments[2])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    "outfrac requires an integer width argument",
                    arguments[2].span.clone(),
                )?;
                self.push(
                    Op::CallBasicioOutFrac {
                        object,
                        value,
                        digits,
                        width,
                    },
                    span.clone(),
                );
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "outint" => {
                if arguments.len() != 2 {
                    return Err(spanned_error(
                        format!(
                            "outint expects 2 arguments (i, w), found {}",
                            arguments.len()
                        ),
                        span,
                    ));
                }
                let value = self.lower_expr(&arguments[0])?;
                let value = self.coerce_value(
                    MirType::I64,
                    value,
                    "outint requires an integer value argument",
                    arguments[0].span.clone(),
                )?;
                let width = self.lower_expr(&arguments[1])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    "outint requires an integer width argument",
                    arguments[1].span.clone(),
                )?;
                self.push(
                    Op::CallBasicioOutInt {
                        object,
                        value,
                        width,
                    },
                    span.clone(),
                );
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "line" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("line expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioLine { dest, object }, span);
                Ok(dest)
            }
            "image" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("image expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Text);
                self.push(Op::CallBasicioImage { dest, object }, span);
                Ok(dest)
            }
            "pos" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("pos expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioPos { dest, object }, span);
                Ok(dest)
            }
            "length" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("length expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioLength { dest, object }, span);
                Ok(dest)
            }
            "page" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("page expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 1 }, span);
                Ok(dest)
            }
            "setpos" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("setpos expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let index = self.lower_expr(&arguments[0])?;
                let index = self.coerce_value(
                    MirType::I64,
                    index,
                    "setpos requires an integer argument",
                    arguments[0].span.clone(),
                )?;
                self.push(Op::CallBasicioSetpos { object, index }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "setaccess" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("setaccess expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let mode = self.lower_expr(&arguments[0])?;
                if self.local_ty(mode) != MirType::Text {
                    return Err(spanned_error(
                        "setaccess requires a text argument",
                        arguments[0].span.clone(),
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallBasicioSetAccess { dest, object, mode }, span);
                Ok(dest)
            }
            "linesperpage" => {
                if arguments.len() > 1 {
                    return Err(spanned_error(
                        format!(
                            "linesperpage expects 0 or 1 argument, found {}",
                            arguments.len()
                        ),
                        span,
                    ));
                }
                let n = if let Some(arg) = arguments.first() {
                    let n = self.lower_expr(arg)?;
                    self.coerce_value(
                        MirType::I64,
                        n,
                        "linesperpage requires an integer argument",
                        arg.span.clone(),
                    )?
                } else {
                    // 0-arg form: read current page length (set with n=0 keeps it).
                    let n = self.temp(MirType::I64);
                    self.push(Op::ConstI64 { dest: n, value: 0 }, span.clone());
                    n
                };
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioLinesPerPage { dest, object, n }, span);
                Ok(dest)
            }
            "spacing" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("spacing expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let _ = self.lower_expr(&arguments[0])?;
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "eject" => {
                if arguments.len() > 1 {
                    return Err(spanned_error(
                        format!("eject expects 0 or 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let n = if let Some(arg) = arguments.first() {
                    let n = self.lower_expr(arg)?;
                    self.coerce_value(
                        MirType::I64,
                        n,
                        "eject requires an integer argument",
                        arg.span.clone(),
                    )?
                } else {
                    let n = self.temp(MirType::I64);
                    self.push(Op::ConstI64 { dest: n, value: 1 }, span.clone());
                    n
                };
                self.push(Op::CallBasicioEject { object, line: n }, span.clone());
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
            "inint" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("inint expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioInInt { dest, object }, span);
                Ok(dest)
            }
            "infrac" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("infrac expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallBasicioInFrac { dest, object }, span);
                Ok(dest)
            }
            "inreal" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("inreal expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::F64);
                self.push(Op::CallBasicioInReal { dest, object }, span);
                Ok(dest)
            }
            "intext" => {
                if arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("intext expects 1 argument, found {}", arguments.len()),
                        span,
                    ));
                }
                let width = self.lower_expr(&arguments[0])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    "intext requires an integer width",
                    arguments[0].span.clone(),
                )?;
                let dest = self.temp(MirType::Text);
                self.push(
                    Op::CallBasicioInText {
                        dest,
                        object,
                        width,
                    },
                    span,
                );
                Ok(dest)
            }
            "lastitem" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("lastitem expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallBasicioLastItem { dest, object }, span);
                Ok(dest)
            }
            "filename" => {
                if !arguments.is_empty() {
                    return Err(spanned_error(
                        format!("filename expects 0 arguments, found {}", arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Text);
                self.push(Op::CallBasicioFilename { dest, object }, span);
                Ok(dest)
            }
            other => Err(spanned_error(
                format!("unsupported BASICIO method '{other}'"),
                span,
            )),
        }
    }

    /// Process system attributes (§12.1): `idle`, `terminated`, `evtime`,
    /// `nextev`.
    pub(in crate::mir::lower) fn try_lower_process_builtin_attribute(
        &mut self,
        object: LocalId,
        attribute: &str,
        span: Span,
    ) -> Result<Option<LocalId>, CompileError> {
        let qual = self
            .ref_qual
            .get(&object)
            .map(|q| declared_class_name(q).to_string());
        let is_process = qual
            .as_deref()
            .is_some_and(|name| self.class_is_scheduled_process(name));
        if !is_process {
            return Ok(None);
        }
        match attribute.to_ascii_lowercase().as_str() {
            "idle" => {
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::SimIdle {
                        dest,
                        process: object,
                    },
                    span,
                );
                Ok(Some(dest))
            }
            "terminated" => {
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::SimTerminated {
                        dest,
                        process: object,
                    },
                    span,
                );
                Ok(Some(dest))
            }
            "evtime" => {
                let dest = self.temp(MirType::LongF64);
                self.push(
                    Op::SimEvtime {
                        dest,
                        process: object,
                    },
                    span,
                );
                Ok(Some(dest))
            }
            "nextev" => {
                let dest = self.temp(MirType::ObjectRef);
                self.push(
                    Op::SimNextev {
                        dest,
                        process: object,
                    },
                    span,
                );
                self.note_object_qual(dest, "Process".into());
                Ok(Some(dest))
            }
            _ => Ok(None),
        }
    }

    /// Lowers `obj.m(args)` for a known class method. Virtual methods use
    /// runtime `class_id` dispatch among the static class and its subclasses.
    /// Void methods still produce a dummy `i64` 0 so statement-form
    /// `ExprKind::RemoteCall` can discard the value (matching text `setpos`).
    pub(in crate::mir::lower) fn lower_object_method_call(
        &mut self,
        object_id: LocalId,
        attribute: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if is_fictitious_detach(attribute) {
            for argument in arguments {
                self.lower_expr(argument)?;
            }
            let dest = self.temp(MirType::I64);
            self.push(Op::ConstI64 { dest, value: 0 }, span);
            return Ok(dest);
        }
        if is_simset_method(attribute) {
            return self.lower_simset_method(object_id, attribute, arguments, span);
        }
        if is_basicio_method(attribute) {
            return self.lower_basicio_method(object_id, attribute, arguments, span);
        }
        if arguments.is_empty()
            && let Some(result) =
                self.try_lower_process_builtin_attribute(object_id, attribute, span.clone())?
        {
            return Ok(result);
        }
        let (static_name, is_virtual, default_mangled) = {
            let Some(layout) = self.layout_for_object(object_id) else {
                return Err(spanned_error(
                    format!(
                        "MIR lowering: cannot resolve method '{attribute}' on an object reference of unknown class"
                    ),
                    span,
                ));
            };
            let Some(method_name) = layout.method_name(attribute) else {
                if attribute.eq_ignore_ascii_case("detach") {
                    return Err(scheduling_unsupported_error("detach", span));
                }
                // `obj.arr(i)` parses as a remote call; fall back to array element access.
                if let Ok((offset, field_ty)) =
                    self.field_info_for(object_id, attribute, span.clone())
                {
                    if matches!(
                        field_ty,
                        FieldType::ArrayI64
                            | FieldType::ArrayBool
                            | FieldType::ArrayF64
                            | FieldType::ArrayText
                    ) {
                        let array = self.temp(mir_type_for_field(field_ty));
                        self.push(
                            Op::FieldLoadI64 {
                                dest: array,
                                object: object_id,
                                offset,
                                class_qual: None,
                            },
                            span.clone(),
                        );
                        self.annotate_loaded_field(array, object_id, attribute, field_ty);
                        let mut indices = Vec::with_capacity(arguments.len());
                        for argument in arguments {
                            let index = self.lower_expr(argument)?;
                            let index = self.coerce_value(
                                MirType::I64,
                                index,
                                "array subscript must be an integer expression",
                                argument.span.clone(),
                            )?;
                            indices.push(index);
                        }
                        let place = Place::ArrayElement { array, indices };
                        return Ok(self.read_place(&place, span));
                    }
                }
                return Err(spanned_error(
                    format!(
                        "MIR lowering: class '{}' has no method '{attribute}' (remote calls require a known class procedure)",
                        layout.name
                    ),
                    span,
                ));
            };
            let defining_class = defining_class_for_method(self.classes, &layout.name, method_name);
            let access = self.access_level();
            let qual = self
                .ref_qual
                .get(&object_id)
                .map(String::as_str)
                .unwrap_or(layout.name.as_str());
            let binding = visible_attribute_binding(access, qual, attribute, self.classes);
            let (static_name, is_virtual) = match &binding {
                Some(b) if b.kind == AttributeKind::Virtual => (b.level.clone(), true),
                Some(b) if b.kind == AttributeKind::Procedure => (b.level.clone(), false),
                _ => (layout.name.clone(), layout.is_virtual_method(attribute)),
            };
            (
                static_name,
                is_virtual,
                mangle_method_name(defining_class, method_name),
            )
        };

        if is_virtual {
            let targets = self.virtual_dispatch_targets(&static_name, attribute);
            if targets.len() > 1 {
                let default = if self.signatures.contains_key(&default_mangled) {
                    default_mangled
                } else {
                    // Unmatched virtual on the static class: use a concrete
                    // subclass implementation as the dispatch default/signature.
                    targets.last().expect("targets non-empty").1.clone()
                };
                return self.lower_virtual_method_call(
                    object_id,
                    attribute,
                    &static_name,
                    arguments,
                    &targets,
                    &default,
                    span,
                );
            }
            if let Some((_, mangled)) = targets.first() {
                return self.lower_direct_method_call(object_id, mangled, arguments, span);
            }
        }

        // Prefer the binding's declared procedure when the virtual quantity is
        // hidden and an ordinary same-named procedure remains (§5.5.3).
        if let Some(mangled) = self.object_method_name(object_id, attribute) {
            return self.lower_direct_method_call(object_id, &mangled, arguments, span);
        }
        self.lower_direct_method_call(object_id, &default_mangled, arguments, span)
    }

    /// `(class_id, mangled_name)` for every class that is `static_class` or a
    /// subclass, bound to the virtual match visible at that runtime class
    /// (§5.5.3: `hidden` disables further matching in subclasses of the hider).
    pub(in crate::mir::lower) fn virtual_dispatch_targets(
        &self,
        static_class: &str,
        method: &str,
    ) -> Vec<(i64, String)> {
        let mut targets = Vec::new();
        let mut static_target = None;
        for layout in self.layouts.values() {
            let is_static = layout.name.eq_ignore_ascii_case(static_class);
            let is_sub = is_subclass_of(&layout.name, static_class, self.classes);
            if !(is_static || is_sub) {
                continue;
            }
            let Some(match_level) =
                virtual_match_level(&layout.name, static_class, method, self.classes)
            else {
                continue;
            };
            let Some(proc_name) = declared_procedure_name(&match_level, method, self.classes)
            else {
                continue;
            };
            let mangled = mangle_method_name(&match_level, &proc_name);
            if !self.signatures.contains_key(&mangled)
                && self.lookup_name_param_proc(&mangled).is_none()
            {
                continue;
            }
            let entry = (layout.class_id, mangled);
            if is_static {
                static_target = Some(entry);
            } else {
                targets.push(entry);
            }
        }
        targets.sort_by_key(|(id, _)| *id);
        if let Some(entry) = static_target {
            targets.push(entry);
        }
        targets
    }

    /// User-visible argument count for a mangled method (excludes `__this`
    /// and thunk/formal-proc ABI expansions).
    pub(in crate::mir::lower) fn method_user_arity(&self, mangled: &str) -> Option<usize> {
        if let Some(signature) = self.signatures.get(mangled) {
            let user_sig = user_signature_without_this(signature);
            return Some(
                user_sig.params.len()
                    - user_sig.name_thunk_starts.len() * 2
                    - user_sig.formal_proc_param_indices.len()
                    - user_sig.free_cell_params.len(),
            );
        }
        self.lookup_name_param_proc(mangled)
            .map(|procedure| procedure.parameters.len())
    }

    pub(in crate::mir::lower) fn lower_virtual_method_call(
        &mut self,
        object_id: LocalId,
        attribute: &str,
        static_class: &str,
        arguments: &[Expr],
        targets: &[(i64, String)],
        default_mangled: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        // Matching procedures of an unmatched virtual may differ in arity
        // (DosTestBatch simtst57: `AA.Dump(rf)` vs `AB.Dump`). Lower and
        // dispatch only among implementations that accept this call site.
        let call_arity = arguments.len();
        let arity_targets: Vec<(i64, String)> = targets
            .iter()
            .filter(|(_, mangled)| self.method_user_arity(mangled) == Some(call_arity))
            .cloned()
            .collect();
        let targets = if arity_targets.is_empty() {
            targets
        } else {
            arity_targets.as_slice()
        };
        let signature_mangled = if targets.iter().any(|(_, m)| m == default_mangled) {
            default_mangled.to_string()
        } else {
            targets
                .last()
                .map(|(_, m)| m.clone())
                .unwrap_or_else(|| default_mangled.to_string())
        };
        let signature = self.signatures.get(&signature_mangled).cloned().ok_or_else(|| {
            spanned_error(
                format!(
                    "MIR lowering: internal error: missing signature for method '{signature_mangled}'"
                ),
                span.clone(),
            )
        })?;
        let user_sig = user_signature_without_this(&signature);
        let user_args =
            self.lower_call_arguments(&signature_mangled, &user_sig, arguments, span.clone())?;

        let class_id = self.temp(MirType::I64);
        self.push(
            Op::ObjectClassIdSafe {
                dest: class_id,
                object: object_id,
            },
            span.clone(),
        );

        let merge = self.new_block();
        // Prefer the virtual specification's type when the static class only
        // declares an unmatched virtual (`virtual: procedure P`). Otherwise a
        // typed subclass default (e.g. `text procedure P`) would force every
        // arm — including void overrides — to use `dest: Some(...)`.
        let result_local = match self.virtual_procedure_result_ty(static_class, attribute) {
            Some(None) => None,
            Some(Some(ty)) => Some(self.temp(ty)),
            None => signature.result.map(|ty| self.temp(ty)),
        };

        for (index, (target_id, mangled)) in targets.iter().enumerate() {
            let is_last = index + 1 == targets.len();
            if !is_last {
                let match_block = self.new_block();
                let next = self.new_block();
                let expected = self.temp(MirType::I64);
                self.push(
                    Op::ConstI64 {
                        dest: expected,
                        value: *target_id,
                    },
                    0..0,
                );
                let cond = self.temp(MirType::Bool);
                self.push(
                    Op::Compare {
                        dest: cond,
                        op: CmpOp::Eq,
                        left: class_id,
                        right: expected,
                    },
                    span.clone(),
                );
                self.push(
                    Op::Branch {
                        cond,
                        then_block: match_block,
                        else_block: next,
                    },
                    span.clone(),
                );
                self.switch_to(match_block);
                self.emit_virtual_target_call(
                    object_id,
                    mangled,
                    &user_args,
                    result_local,
                    span.clone(),
                )?;
                self.push(Op::Jump { target: merge }, 0..0);
                self.switch_to(next);
            } else {
                self.emit_virtual_target_call(
                    object_id,
                    mangled,
                    &user_args,
                    result_local,
                    span.clone(),
                )?;
                self.push(Op::Jump { target: merge }, 0..0);
            }
        }

        self.switch_to(merge);
        match result_local {
            Some(dest) => {
                self.annotate_call_result(dest, &signature);
                Ok(dest)
            }
            None => {
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
        }
    }

    /// Result type declared by `virtual: … procedure name` on `class` or a
    /// prefix. `Some(None)` = untyped (void) procedure; `Some(Some(ty))` =
    /// typed; `None` = no virtual declaration found.
    pub(in crate::mir::lower) fn virtual_procedure_result_ty(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<Option<MirType>> {
        let mut current = class_name;
        loop {
            let Some(class) = self.classes.get(current).or_else(|| {
                self.classes
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(current))
                    .map(|(_, class)| class)
            }) else {
                return None;
            };
            for spec in &class.virtual_part {
                if !spec
                    .names
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(method))
                {
                    continue;
                }
                return match &spec.specifier {
                    Specifier::Procedure => Some(None),
                    Specifier::TypeProcedure(ty) => match mir_type_for(ty) {
                        Ok(mir_ty) => Some(Some(mir_ty)),
                        Err(_) => None,
                    },
                    _ => None,
                };
            }
            match class.prefix.as_deref() {
                Some(prefix) => current = prefix,
                None => return None,
            }
        }
    }

    /// Emits one virtual-dispatch arm. Void overrides must use `dest: None`
    /// even when the shared slot expects a value (simtst55 `D$P` vs `E$P`).
    pub(in crate::mir::lower) fn emit_virtual_target_call(
        &mut self,
        object_id: LocalId,
        mangled: &str,
        user_args: &[LocalId],
        result_local: Option<LocalId>,
        span: Span,
    ) -> Result<(), CompileError> {
        let target_returns = self
            .signatures
            .get(mangled)
            .is_some_and(|sig| sig.result.is_some());
        let call_dest = if target_returns { result_local } else { None };
        self.emit_method_call(object_id, mangled, user_args, call_dest, span.clone())?;
        if let Some(dest) = result_local
            && !target_returns
        {
            self.emit_default_local_value(dest, span);
        }
        Ok(())
    }

    pub(in crate::mir::lower) fn emit_default_local_value(&mut self, dest: LocalId, span: Span) {
        match self.local_ty(dest) {
            MirType::I64 => self.push(Op::ConstI64 { dest, value: 0 }, span),
            MirType::Bool => self.push(Op::ConstBool { dest, value: false }, span),
            MirType::F64 | MirType::LongF64 => {
                self.push(Op::ConstF64 { dest, value: 0.0 }, span);
            }
            MirType::Text => self.push(Op::TextNotext { dest }, span),
            MirType::ObjectRef => self.push(Op::ConstNone { dest }, span),
            _ => self.push(Op::ConstI64 { dest, value: 0 }, span),
        }
    }

    pub(in crate::mir::lower) fn lower_direct_method_call(
        &mut self,
        object_id: LocalId,
        mangled: &str,
        arguments: &[Expr],
        span: Span,
    ) -> Result<LocalId, CompileError> {
        // Formal-procedure / label / switch methods are call-site inlined
        // (no outlined MIR body); keep `__this` as the receiver for the body.
        if let Some(procedure) = self.lookup_name_param_proc(mangled) {
            let class_name = self.instance_layout_name(object_id);
            let refreshed = if let Some(ref class_name) = class_name {
                self.refresh_enclosing_captures(object_id, class_name, span.clone())?
            } else {
                Vec::new()
            };
            self.push_this_receiver(object_id, false);
            let as_expression = procedure.result_type.is_some();
            let result =
                self.inline_name_procedure(procedure, arguments, span.clone(), as_expression);
            self.pop_this_receiver();
            let result = result?;
            if let Some(class_name) = class_name {
                self.writeback_enclosing_captures(
                    object_id,
                    &class_name,
                    &refreshed,
                    span.clone(),
                )?;
            }
            return Ok(result.unwrap_or_else(|| {
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                dest
            }));
        }
        let signature = self.signatures.get(mangled).cloned().ok_or_else(|| {
            spanned_error(
                format!("MIR lowering: internal error: missing signature for method '{mangled}'"),
                span.clone(),
            )
        })?;
        let user_sig = user_signature_without_this(&signature);
        let user_args = self.lower_call_arguments(mangled, &user_sig, arguments, span.clone())?;
        match signature.result {
            Some(result_ty) => {
                let dest = self.temp(result_ty);
                self.emit_method_call(object_id, mangled, &user_args, Some(dest), span)?;
                self.annotate_call_result(dest, &signature);
                Ok(dest)
            }
            None => {
                self.emit_method_call(object_id, mangled, &user_args, None, span)?;
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value: 0 }, 0..0);
                Ok(dest)
            }
        }
    }

    pub(in crate::mir::lower) fn emit_method_call(
        &mut self,
        object_id: LocalId,
        mangled: &str,
        user_args: &[LocalId],
        result: Option<LocalId>,
        span: Span,
    ) -> Result<(), CompileError> {
        // Refresh then writeback enclosing captures around the call so methods
        // see current caller locals and assignments like `ra2 :- This A` become
        // visible to the caller (simtst47) without restoring stale snapshots.
        let class_name = self.instance_layout_name(object_id);
        let refreshed = if let Some(ref class_name) = class_name {
            self.refresh_enclosing_captures(object_id, class_name, span.clone())?
        } else {
            Vec::new()
        };
        let mut args = Vec::with_capacity(1 + user_args.len());
        args.push(object_id);
        args.extend_from_slice(user_args);
        self.push(
            Op::Call {
                dest: result,
                name: mangled.to_string(),
                args,
            },
            span.clone(),
        );
        if let Some(class_name) = class_name {
            self.writeback_enclosing_captures(object_id, &class_name, &refreshed, span)?;
        }
        Ok(())
    }

    pub(in crate::mir::lower) fn field_info_for(
        &self,
        object: LocalId,
        attribute: &str,
        span: Span,
    ) -> Result<(i64, FieldType), CompileError> {
        let Some(qual) = self.ref_qual.get(&object) else {
            return Err(spanned_error(
                format!(
                    "MIR lowering: cannot resolve attribute '{attribute}' on an object reference of unknown class"
                ),
                span,
            ));
        };
        let Some(layout) = self.find_layout(qual) else {
            return Err(spanned_error(
                format!(
                    "MIR lowering: undefined class '{qual}' for remote attribute '{attribute}'"
                ),
                span,
            ));
        };
        // Outside the class hierarchy, skip protected subclass attributes so
        // `ref(B).i` can resolve to prefix `A.i` (simtst60). Inside B (methods /
        // inspect / prefixed blocks), the same remote `x.i` sees protected `i$B`
        // (simtst61). Shadowed field names come from concatenated substitutions.
        let concatenated = self.concatenated_class_map();
        let access_class = self
            .method_this
            .and_then(|id| self.ref_qual.get(&id).map(|s| s.as_str()));
        let storage = accessible_remote_storage_name(qual, attribute, access_class, &concatenated);
        if let (Some(offset), Some(field_ty)) = (
            layout
                .field_offset(&storage)
                .or_else(|| layout.field_offset(attribute)),
            layout
                .field_type(&storage)
                .or_else(|| layout.field_type(attribute)),
        ) {
            return Ok((offset, field_ty));
        }
        if layout.method_name(attribute).is_some() || layout.method_name(&storage).is_some() {
            return Err(spanned_error(
                format!(
                    "MIR lowering: method '{attribute}' of class '{}' cannot be an assignment target",
                    layout.name
                ),
                span,
            ));
        }
        Err(spanned_error(
            format!(
                "MIR lowering: class '{}' has no integer or boolean attribute '{attribute}'",
                layout.name
            ),
            span,
        ))
    }
}
