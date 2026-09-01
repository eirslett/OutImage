//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn lower_procedure_call(
        &mut self,
        call: &ProcedureCall,
        span: Span,
    ) -> Result<(), CompileError> {
        if is_deferred_scheduling_name(&call.name) {
            return Err(scheduling_unsupported_error(&call.name, span));
        }
        let rewritten = self
            .resolve_formal_procedure_name(&call.name)
            .map(str::to_string);
        if rewritten.is_none()
            && let Some(FormalProcTarget::Method { object, method }) =
                self.resolve_formal_proc_target(&call.name).cloned()
        {
            let _ = self.lower_object_method_call(object, &method, &call.arguments, span)?;
            return Ok(());
        }
        let call_name = rewritten.as_deref().unwrap_or(call.name.as_str());
        if is_basicio_method(call_name) {
            let receivers: Vec<LocalId> = self.method_this_chain().map(|(id, _)| id).collect();
            for receiver in receivers {
                if self
                    .ref_qual
                    .get(&receiver)
                    .is_some_and(|qual| is_basicio_class(qual))
                {
                    let _ =
                        self.lower_basicio_method(receiver, call_name, &call.arguments, span)?;
                    return Ok(());
                }
            }
        }
        match call_name.to_ascii_lowercase().as_str() {
            "detach" => {
                if self.connection_depth == 0
                    && matches!(self.inline_detach_names_receiver.last(), Some(false))
                {
                    // The detach attribute here belongs to the block instance
                    // the procedure was declared in, not to the object running
                    // it, and 7.3.1 gives a prefixed block instance's detach no
                    // effect. (simtst69 requires `Sjekk(7); Detach; Sjekk(8)`
                    // to record 7 and 8 with nothing in between.)
                    return Ok(());
                }
                let Some(this_id) = self.method_this else {
                    // No object in scope, so the detach attribute is a block
                    // instance's. The outermost block is prefixed (chapter 11)
                    // and any other block reaching here was prefixed too, since
                    // a plain block has no detach attribute at all; either way
                    // 7.3.1 gives it no effect.
                    return Ok(());
                };
                // 7.3.1: switch stacks. The frame stays exactly where it is, so
                // there is nothing to save and nothing to split.
                self.push(Op::SeqDetach { object: this_id }, span);
                Ok(())
            }
            "hold" => {
                if self.simulation_context {
                    if call.arguments.len() != 1 {
                        return Err(spanned_error(
                            format!("hold expects 1 argument, found {}", call.arguments.len()),
                            span,
                        ));
                    }
                    // 12.3: the active process is rescheduled at time+dt, and
                    // whichever process now heads the sequencing set takes over.
                    // The same two ops serve MAIN and a process body, because
                    // both are components of the SIMULATION system.
                    let dt = self.lower_hold_dt(&call.arguments[0])?;
                    self.push(Op::SimHold { dt }, span.clone());
                    self.push(Op::SimTransferToHead, span.clone());
                    Ok(())
                } else {
                    Err(scheduling_unsupported_error("hold", span))
                }
            }
            "passivate" => {
                if self.simulation_context {
                    if !call.arguments.is_empty() {
                        return Err(spanned_error(
                            format!(
                                "passivate expects 0 arguments, found {}",
                                call.arguments.len()
                            ),
                            span,
                        ));
                    }
                    self.push(Op::SimPassivate, span.clone());
                    self.push(Op::SimTransferToHead, span.clone());
                    Ok(())
                } else {
                    Err(scheduling_unsupported_error("passivate", span))
                }
            }
            "wait" => {
                if self.simulation_context {
                    // 12.4: `wait(q)` is `this process.into(q); passivate`.
                    let Some(this_id) = self.method_this else {
                        return Err(spanned_error(
                            "MIR lowering: 'wait' is only supported in Process bodies \
                             (MAIN wait is unsupported because MAIN is not a Linkage object)",
                            span,
                        ));
                    };
                    if call.arguments.len() != 1 {
                        return Err(spanned_error(
                            format!("wait expects 1 argument, found {}", call.arguments.len()),
                            span,
                        ));
                    }
                    let head = self.lower_expr(&call.arguments[0])?;
                    if self.local_ty(head) != MirType::ObjectRef {
                        return Err(spanned_error(
                            "wait requires a Head object reference",
                            call.arguments[0].span.clone(),
                        ));
                    }
                    self.push(
                        Op::SimsetInto {
                            object: this_id,
                            head,
                        },
                        span.clone(),
                    );
                    self.push(Op::SimPassivate, span.clone());
                    self.push(Op::SimTransferToHead, span.clone());
                    Ok(())
                } else {
                    Err(scheduling_unsupported_error("wait", span))
                }
            }
            "cancel" => {
                if !self.simulation_context {
                    return Err(scheduling_unsupported_error("cancel", span));
                }
                if call.arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("cancel expects 1 argument, found {}", call.arguments.len()),
                        span,
                    ));
                }
                let process = self.lower_expr(&call.arguments[0])?;
                if self.local_ty(process) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "cancel requires an object reference",
                        call.arguments[0].span.clone(),
                    ));
                }
                self.push(Op::SimCancel { process }, span);
                Ok(())
            }
            "call" | "resume" => self.lower_call_or_resume(call, span),
            "outtext" => {
                if call.arguments.len() != 1 {
                    return Err(spanned_error("OutText expects exactly one argument", span));
                }
                let argument = &call.arguments[0];
                match &argument.kind {
                    ExprKind::StringLiteral(text) => {
                        let string_id = self.intern_string(text);
                        self.push(Op::CallOutText { string_id }, span);
                    }
                    ExprKind::Notext => {
                        let string_id = self.intern_string("");
                        self.push(Op::CallOutText { string_id }, span);
                    }
                    _ => {
                        let value = self.lower_expr(argument)?;
                        if self.local_ty(value) != MirType::Text {
                            return Err(spanned_error(
                                "OutText requires a text expression",
                                argument.span.clone(),
                            ));
                        }
                        self.push(Op::CallOutTextLocal { src: value }, span);
                    }
                }
                Ok(())
            }
            "outint" => {
                if call.arguments.len() != 2 {
                    return Err(spanned_error(
                        format!(
                            "OutInt expects 2 arguments (i, w), found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                let value = self.lower_expr(&call.arguments[0])?;
                let value = self.coerce_value(
                    MirType::I64,
                    value,
                    "OutInt requires an integer value argument",
                    call.arguments[0].span.clone(),
                )?;
                let width = self.lower_expr(&call.arguments[1])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    "OutInt requires an integer width argument",
                    call.arguments[1].span.clone(),
                )?;
                self.push(Op::CallOutInt { value, width }, span);
                Ok(())
            }
            "outreal" | "outfix" => {
                if call.arguments.len() != 3 {
                    return Err(spanned_error(
                        format!(
                            "{} expects 3 arguments (r, n, w), found {}",
                            call.name,
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                let value = self.lower_expr(&call.arguments[0])?;
                // Keep LONG REAL distinct so Outreal can use a 3-digit exponent.
                let value_ty = match self.local_ty(value) {
                    MirType::LongF64 => MirType::LongF64,
                    _ => MirType::F64,
                };
                let value = self.coerce_value(
                    value_ty,
                    value,
                    format!("{} requires a real value argument", call.name),
                    call.arguments[0].span.clone(),
                )?;
                let digits = self.lower_expr(&call.arguments[1])?;
                let digits = self.coerce_value(
                    MirType::I64,
                    digits,
                    format!("{} requires an integer digits argument", call.name),
                    call.arguments[1].span.clone(),
                )?;
                let width = self.lower_expr(&call.arguments[2])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    format!("{} requires an integer width argument", call.name),
                    call.arguments[2].span.clone(),
                )?;
                // `call_name` keeps source casing (`Outreal`); compare case-insensitively.
                if call_name.eq_ignore_ascii_case("outreal") {
                    self.push(
                        Op::CallOutReal {
                            value,
                            digits,
                            width,
                        },
                        span,
                    );
                } else {
                    self.push(
                        Op::CallOutFix {
                            value,
                            digits,
                            width,
                        },
                        span,
                    );
                }
                Ok(())
            }
            "outfrac" => {
                if call.arguments.len() != 3 {
                    return Err(spanned_error(
                        format!(
                            "outfrac expects 3 arguments (i, n, w), found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                let value = self.lower_expr(&call.arguments[0])?;
                let value = self.coerce_value(
                    MirType::I64,
                    value,
                    "outfrac requires an integer value argument",
                    call.arguments[0].span.clone(),
                )?;
                let digits = self.lower_expr(&call.arguments[1])?;
                let digits = self.coerce_value(
                    MirType::I64,
                    digits,
                    "outfrac requires an integer digits argument",
                    call.arguments[1].span.clone(),
                )?;
                let width = self.lower_expr(&call.arguments[2])?;
                let width = self.coerce_value(
                    MirType::I64,
                    width,
                    "outfrac requires an integer width argument",
                    call.arguments[2].span.clone(),
                )?;
                self.push(
                    Op::CallOutFrac {
                        value,
                        digits,
                        width,
                    },
                    span,
                );
                Ok(())
            }
            "filewrite" => {
                if call.arguments.len() != 2 {
                    return Err(spanned_error(
                        format!(
                            "fileWrite expects 2 arguments, found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                let path = lower_filesystem_text_arg(self, &call.arguments[0], "fileWrite")?;
                let contents = lower_filesystem_text_arg(self, &call.arguments[1], "fileWrite")?;
                self.push(Op::CallFileWrite { path, contents }, span);
                Ok(())
            }
            "fileexists" | "fileread" => Err(spanned_error(
                format!(
                    "{} returns a value and cannot be used as a statement",
                    call.name
                ),
                span,
            )),
            "outimage" => {
                self.push(Op::CallOutImage, span);
                Ok(())
            }
            "outchar" => {
                if call.arguments.len() != 1 {
                    return Err(spanned_error(
                        format!("OutChar expects 1 argument, found {}", call.arguments.len()),
                        span,
                    ));
                }
                let ch = self.lower_expr(&call.arguments[0])?;
                if self.local_ty(ch) != MirType::I64 {
                    return Err(spanned_error(
                        "OutChar requires a character argument",
                        call.arguments[0].span.clone(),
                    ));
                }
                self.push(Op::CallOutChar { ch }, span);
                Ok(())
            }
            "breakoutimage" => {
                if !call.arguments.is_empty() {
                    return Err(spanned_error(
                        format!(
                            "BreakOutImage expects 0 arguments, found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                self.push(Op::CallBreakOutImage, span);
                Ok(())
            }
            "terminate_program" => {
                if !call.arguments.is_empty() {
                    return Err(spanned_error(
                        format!(
                            "terminate_program expects 0 arguments, found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                self.push(Op::CallTerminateProgram, span);
                Ok(())
            }
            "inimage" => {
                if !call.arguments.is_empty() {
                    return Err(spanned_error(
                        format!(
                            "InImage expects 0 arguments, found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                self.push(Op::CallInImage, span);
                Ok(())
            }
            "inchar" => {
                // Statement form discards the character (side-effecting read).
                if !call.arguments.is_empty() {
                    return Err(spanned_error(
                        format!("InChar expects 0 arguments, found {}", call.arguments.len()),
                        span,
                    ));
                }
                let dest = self.temp(MirType::I64);
                self.push(Op::CallInChar { dest }, span);
                Ok(())
            }
            "endfile" => {
                // Statement form is meaningless; allow discarding the bool.
                if !call.arguments.is_empty() {
                    return Err(spanned_error(
                        format!(
                            "Endfile expects 0 arguments, found {}",
                            call.arguments.len()
                        ),
                        span,
                    ));
                }
                let dest = self.temp(MirType::Bool);
                self.push(Op::CallEndfile { dest }, span);
                Ok(())
            }
            "sysin" | "sysout" => Err(spanned_error(
                format!(
                    "{} returns a value and cannot be used as a statement",
                    call.name
                ),
                span,
            )),
            "inline" => {
                // Statement form is meaningless (result discarded); allow it as a
                // no-op side-effecting stdin read for parity with expression use.
                if !call.arguments.is_empty() {
                    return Err(spanned_error("InLine does not take arguments", span));
                }
                let dest = self.temp(MirType::Text);
                self.push(Op::CallInLine { dest }, span);
                Ok(())
            }
            "upcase" | "lowcase" => {
                let frame = self.lower_upcase_arg(call, span.clone())?;
                if call.name.eq_ignore_ascii_case("upcase") {
                    self.push(Op::TextUpcase { frame }, span);
                } else {
                    self.push(Op::TextLowcase { frame }, span);
                }
                Ok(())
            }
            "error" if !self.user_procedure_shadows_builtin(call_name) => {
                // Standard ENVIRONMENT `error(t)` takes one text; CIM tests
                // sometimes pass extra diagnostics (`error("mod", i, j, …)`).
                // Evaluate all arguments for side effects, then abort on the
                // first text (or a synthesized message).
                // User-defined `procedure error` (including class methods)
                // shadows this builtin.
                if call.arguments.is_empty() {
                    return Err(spanned_error("error expects at least 1 argument", span));
                }
                let mut message = None;
                for argument in &call.arguments {
                    let value = self.lower_expr(argument)?;
                    if message.is_none() && self.local_ty(value) == MirType::Text {
                        message = Some(value);
                    }
                }
                let message = match message {
                    Some(id) => id,
                    None => {
                        let id = self.temp(MirType::Text);
                        let string_id = self.intern_string("error");
                        self.push(
                            Op::TextFromLiteral {
                                dest: id,
                                string_id,
                            },
                            span.clone(),
                        );
                        id
                    }
                };
                let dest = self.temp(MirType::I64);
                self.push(
                    Op::CallEnv {
                        dest,
                        name: "error".into(),
                        args: vec![message],
                    },
                    span,
                );
                Ok(())
            }
            "histo" => {
                if call.arguments.len() != 4 {
                    return Err(spanned_error(
                        format!("histo expects 4 arguments, found {}", call.arguments.len()),
                        span,
                    ));
                }
                let a = self.lower_expr(&call.arguments[0])?;
                let b = self.lower_expr(&call.arguments[1])?;
                if self.local_ty(a) != MirType::ArrayF64 || self.local_ty(b) != MirType::ArrayF64 {
                    return Err(spanned_error(
                        "histo requires two real array arguments",
                        call.arguments[0].span.clone(),
                    ));
                }
                let c = self.lower_expr_as_f64(&call.arguments[2], "histo")?;
                let d = self.lower_expr_as_f64(&call.arguments[3], "histo")?;
                let dest = self.temp(MirType::I64);
                self.push(
                    Op::CallEnv {
                        dest,
                        name: "histo".into(),
                        args: vec![a, b, c, d],
                    },
                    span,
                );
                Ok(())
            }
            _ => {
                // Locals / name formals shadow procedures (`r(0)` vs procedure `R`).
                if self.name_is_subscriptable(call_name) {
                    let variable = Variable::Subscripted {
                        name: call_name.to_string(),
                        subscripts: call.arguments.clone(),
                    };
                    let place = self.resolve_place(&variable, span.clone())?;
                    let _ = self.read_place(&place, span);
                    return Ok(());
                }
                if let Some(&(func, env)) = self.formal_proc_refs.get(call_name).or_else(|| {
                    self.formal_proc_refs
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(call_name))
                        .map(|(_, pair)| pair)
                }) {
                    if !call.arguments.is_empty() {
                        return Err(spanned_error(
                            format!(
                                "MIR lowering: outlined formal procedure '{call_name}' call with arguments is not supported yet"
                            ),
                            span,
                        ));
                    }
                    return self.lower_formal_proc_ref_call(func, env, span);
                }
                // `inspect` connection only: attribute procedure on the
                // connected qualification shadows a same-named free procedure
                // (`inspect … when B do P2` → `B.P2`, not global `P2`; simtst71).
                // Prefixed blocks also bump `connection_depth` but must still
                // resolve local/virtual overrides before default methods
                // (simtst92). `inspect rA do` qualifies as `A` (no `P2`) → free.
                if self.inspect_connection_depth > 0
                    && let Some(this_id) = self.method_this
                {
                    if is_basicio_method(call_name) {
                        let receivers: Vec<LocalId> =
                            self.method_this_chain().map(|(id, _)| id).collect();
                        for receiver in receivers {
                            if self.object_is_basicio(receiver) {
                                let _ = self.lower_basicio_method(
                                    receiver,
                                    call_name,
                                    &call.arguments,
                                    span,
                                )?;
                                return Ok(());
                            }
                        }
                    }
                    if self.object_method_name(this_id, call_name).is_some() {
                        let _ = self.lower_object_method_call(
                            this_id,
                            call_name,
                            &call.arguments,
                            span,
                        )?;
                        return Ok(());
                    }
                    let receivers: Vec<LocalId> =
                        self.method_this_chain().map(|(id, _)| id).collect();
                    for this_id in receivers {
                        if self.object_method_name(this_id, call_name).is_some() {
                            let _ = self.lower_object_method_call(
                                this_id,
                                call_name,
                                &call.arguments,
                                span,
                            )?;
                            return Ok(());
                        }
                    }
                    if is_simset_method(call_name) {
                        let _ =
                            self.lower_simset_method(this_id, call_name, &call.arguments, span)?;
                        return Ok(());
                    }
                }
                if let Some(procedure) = self.lookup_name_param_proc(call_name) {
                    self.inline_name_procedure(procedure, &call.arguments, span, false)?;
                    return Ok(());
                }
                if self.prefixed_block_proc_applies(call_name) {
                    if let Some(procedure) = self.lookup_ref_alias_proc(call_name) {
                        self.inline_ref_alias_procedure(
                            procedure,
                            &call.arguments,
                            span.clone(),
                            false,
                        )?;
                        return Ok(());
                    }
                    if let Some(signature) = self.signatures.get(call_name).cloned().or_else(|| {
                        self.signatures
                            .iter()
                            .find(|(name, _)| name.eq_ignore_ascii_case(call_name))
                            .map(|(_, sig)| sig.clone())
                    }) {
                        let resolved_name = self
                            .signatures
                            .keys()
                            .find(|name| name.eq_ignore_ascii_case(call_name))
                            .cloned()
                            .unwrap_or_else(|| call_name.to_string());
                        if !resolved_name.contains('$') {
                            let args = self.lower_call_arguments(
                                &resolved_name,
                                &signature,
                                &call.arguments,
                                span.clone(),
                            )?;
                            self.push(
                                Op::Call {
                                    dest: None,
                                    name: resolved_name,
                                    args,
                                },
                                span,
                            );
                            return Ok(());
                        }
                    }
                }
                // Class / connection attribute procedures beat same-named free
                // procedures — including those force-inlined for enclosing
                // captures (simtst98: `virtproc` inside `a` is `a`'s match, not
                // the global that closes over `trace`). Invisible protected/
                // hidden attributes return `None` so the free procedure remains
                // reachable from inspect / outside.
                if let Some(this_id) = self.method_this {
                    if is_basicio_method(call_name) {
                        let receivers: Vec<LocalId> =
                            self.method_this_chain().map(|(id, _)| id).collect();
                        for receiver in receivers {
                            if self.object_is_basicio(receiver) {
                                let _ = self.lower_basicio_method(
                                    receiver,
                                    call_name,
                                    &call.arguments,
                                    span,
                                )?;
                                return Ok(());
                            }
                        }
                    }
                    // Own the stacked quals — `method_this_chain` borrows `self`.
                    let receivers: Vec<(LocalId, Option<String>)> = self
                        .method_this_chain()
                        .map(|(id, qual)| (id, qual.map(str::to_owned)))
                        .collect();
                    for (this_id, stacked_qual) in &receivers {
                        let qual = stacked_qual
                            .as_deref()
                            .or_else(|| self.ref_qual.get(this_id).map(String::as_str));
                        let Some(qual) = qual else {
                            continue;
                        };
                        if self
                            .object_method_name_at(*this_id, call_name, qual)
                            .is_none()
                        {
                            continue;
                        }
                        // Restore this receiver's qualification for dispatch /
                        // field lookup inside the call (inspect may have
                        // overwritten `ref_qual` on a shared object id).
                        let saved = self.ref_qual.get(this_id).cloned();
                        self.note_object_qual(*this_id, qual.to_string());
                        let call_result = self.lower_object_method_call(
                            *this_id,
                            call_name,
                            &call.arguments,
                            span.clone(),
                        );
                        match saved {
                            Some(q) => self.note_object_qual(*this_id, q),
                            None => {
                                self.ref_qual.remove(this_id);
                            }
                        }
                        let _ = call_result?;
                        return Ok(());
                    }
                    if self
                        .try_lower_enclosing_object_method(
                            this_id,
                            call_name,
                            &call.arguments,
                            span.clone(),
                        )?
                        .is_some()
                    {
                        return Ok(());
                    }
                    if is_simset_method(call_name) {
                        let _ =
                            self.lower_simset_method(this_id, call_name, &call.arguments, span)?;
                        return Ok(());
                    }
                }
                if let Some(procedure) = self.lookup_ref_alias_proc(call_name) {
                    self.inline_ref_alias_procedure(procedure, &call.arguments, span, false)?;
                    return Ok(());
                }
                if let Some(signature) = self.signatures.get(call_name).cloned().or_else(|| {
                    self.signatures
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(call_name))
                        .map(|(_, sig)| sig.clone())
                }) {
                    let resolved_name = self
                        .signatures
                        .keys()
                        .find(|name| name.eq_ignore_ascii_case(call_name))
                        .cloned()
                        .unwrap_or_else(|| call_name.to_string());
                    let args = self.lower_call_arguments(
                        &resolved_name,
                        &signature,
                        &call.arguments,
                        span.clone(),
                    )?;
                    self.push(
                        Op::Call {
                            dest: None,
                            name: resolved_name,
                            args,
                        },
                        span,
                    );
                    return Ok(());
                }
                if self
                    .try_lower_free_basicio(call_name, &call.arguments, span.clone())?
                    .is_some()
                {
                    return Ok(());
                }
                Err(spanned_error(
                    format!("MIR lowering: call to unknown procedure '{call_name}'"),
                    span,
                ))
            }
        }
    }

    /// Inlines a call-by-name / formal-procedure procedure at the call site.
    /// Value formals become temporaries; name formals bind to the actual
    /// expressions and are re-evaluated on each use; formal procedure formals
    /// rewrite calls to the actual procedure identifier.
    pub(in crate::mir::lower) fn inline_name_procedure(
        &mut self,
        procedure: &ProcedureDeclaration,
        arguments: &[Expr],
        span: Span,
        as_expression: bool,
    ) -> Result<Option<LocalId>, CompileError> {
        if self
            .inline_stack
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&procedure.name))
        {
            return Err(spanned_error(
                format!(
                    "MIR lowering: recursive call-by-name/formal-procedure procedure '{}' is not supported yet (parameters are inlined at each call site; recursion needs outlined thunks, which are not implemented — rewrite with value parameters, or avoid self-calls)",
                    procedure.name
                ),
                span,
            ));
        }
        if arguments.len() != procedure.parameters.len() {
            return Err(spanned_error(
                format!(
                    "procedure '{}' expects {} argument(s), found {}",
                    procedure.name,
                    procedure.parameters.len(),
                    arguments.len()
                ),
                span,
            ));
        }

        let saved_bindings = self.name_bindings.clone();
        let saved_formal_tys = self.name_formal_tys.clone();
        let saved_formal_procs = self.formal_proc_bindings.clone();
        let saved_formal_labels = self.formal_label_bindings.clone();
        let saved_formal_switches = self.formal_switch_bindings.clone();
        self.name_env_stack.push(saved_bindings.clone());
        self.name_formal_ty_stack.push(saved_formal_tys.clone());
        self.formal_proc_env_stack.push(saved_formal_procs.clone());
        self.formal_label_env_stack
            .push(saved_formal_labels.clone());
        self.formal_switch_env_stack
            .push(saved_formal_switches.clone());
        let mut scope_restore: Vec<(String, Option<LocalId>)> = Vec::new();
        let mut ref_qual_restore: Vec<(LocalId, Option<String>)> = Vec::new();
        let detach_names_receiver = self
            .method_this
            .is_some_and(|this| self.object_method_name(this, &procedure.name).is_some());
        self.inline_detach_names_receiver
            .push(detach_names_receiver);
        self.inline_stack.push(procedure.name.clone());
        self.inline_debug_scopes.push(procedure.span.clone());
        self.record_debug_scope(
            procedure.name.clone(),
            procedure.span.clone(),
            DebugScopeKind::Procedure,
        );
        self.inline_scope_restores.push(Vec::new());
        self.inline_body_locals.push(HashSet::new());
        // Procedure text is not part of a call-site `inspect` connection block
        // (§4.8). Clear connection binding so free names stay lexical even when
        // the call occurs inside `inspect` (simtst73 `P(R)` vs attribute `i`).
        let saved_connection_depth = self.connection_depth;
        let saved_inspect_connection_depth = self.inspect_connection_depth;
        self.connection_depth = 0;
        self.inspect_connection_depth = 0;

        for (param, argument) in procedure.parameters.iter().zip(arguments) {
            if param.is_label {
                let target = expr_as_designational(argument)?;
                // By-value LABEL: freeze designational conditions/subscripts at
                // call time so later mutations (e.g. `b := not b` in the body)
                // do not change the destination (DosTestBatch simtst31).
                let target = if param.mode == ParamMode::Name {
                    target
                } else {
                    self.freeze_designational_value(target, argument.span.clone())?
                };
                self.formal_label_bindings
                    .insert(param.name.clone(), target);
                continue;
            }
            if param.is_switch {
                let actual = switch_identifier_actual(argument)?;
                self.formal_switch_bindings
                    .insert(param.name.clone(), actual);
                continue;
            }
            if param.is_procedure {
                if let Some((object_expr, method)) = remote_method_actual(argument) {
                    let object = self.with_caller_name_env(|this| this.lower_expr(object_expr))?;
                    if self.local_ty(object) != MirType::ObjectRef {
                        return Err(spanned_error(
                            format!(
                                "MIR lowering: formal procedure parameter '{}' remote actual requires an object reference",
                                param.name
                            ),
                            argument.span.clone(),
                        ));
                    }
                    self.formal_proc_bindings.insert(
                        param.name.clone(),
                        FormalProcTarget::Method {
                            object,
                            method: method.to_string(),
                        },
                    );
                    continue;
                }
                if let Some((object_name, method)) = remote_method_variable_actual(argument) {
                    let object = self.with_caller_name_env(|this| {
                        this.lower_expr(&Expr {
                            kind: ExprKind::Variable(Variable::Simple(object_name.to_string())),
                            span: argument.span.clone(),
                        })
                    })?;
                    if self.local_ty(object) != MirType::ObjectRef {
                        return Err(spanned_error(
                            format!(
                                "MIR lowering: formal procedure parameter '{}' remote actual requires an object reference",
                                param.name
                            ),
                            argument.span.clone(),
                        ));
                    }
                    self.formal_proc_bindings.insert(
                        param.name.clone(),
                        FormalProcTarget::Method {
                            object,
                            method: method.to_string(),
                        },
                    );
                    continue;
                }
                let actual_name = procedure_identifier_actual(argument)?;
                // Connection / class body: bare method name as formal-proc actual
                // (walk inspect / prefix receiver chain so `E` in `D begin` finds
                // method `E` declared on an outer class).
                let method_receivers: Vec<LocalId> =
                    self.method_this_chain().map(|(id, _)| id).collect();
                let mut bound_method = false;
                for this_id in method_receivers {
                    if self.object_method_name(this_id, &actual_name).is_some() {
                        self.formal_proc_bindings.insert(
                            param.name.clone(),
                            FormalProcTarget::Method {
                                object: this_id,
                                method: actual_name.clone(),
                            },
                        );
                        bound_method = true;
                        break;
                    }
                }
                if bound_method {
                    continue;
                }
                // Pass-through of an outer formal (free procedure or method).
                if let Some(target) = self.resolve_formal_proc_target(&actual_name).cloned() {
                    self.formal_proc_bindings.insert(param.name.clone(), target);
                    continue;
                }
                let canonical = self.resolve_known_procedure(&actual_name).ok_or_else(|| {
                    spanned_error(
                        format!(
                            "MIR lowering: formal procedure parameter '{}' requires a known procedure actual, found '{actual_name}'",
                            param.name
                        ),
                        argument.span.clone(),
                    )
                })?;
                self.formal_proc_bindings
                    .insert(param.name.clone(), FormalProcTarget::Procedure(canonical));
                continue;
            }
            match param.mode {
                ParamMode::Name => {
                    self.name_bindings
                        .insert(param.name.clone(), argument.clone());
                    if let Ok(ty) = mir_type_for(&param.ty) {
                        self.name_formal_tys.insert(param.name.clone(), ty);
                    }
                }
                ParamMode::Value => {
                    let ty = mir_type_for(&param.ty)?;
                    // Value actuals evaluate in the caller environment.
                    let value = self.with_caller_name_env(|this| this.lower_expr(argument))?;
                    let value = self.coerce_value(
                        ty,
                        value,
                        format!(
                            "argument type mismatch calling '{}' (expected {ty})",
                            procedure.name
                        ),
                        argument.span.clone(),
                    )?;
                    let id = self.new_local(param.name.clone(), ty);
                    let previous = self.scope.insert(param.name.clone(), id);
                    // Formals must beat same-named class attributes on a
                    // resumable `__this` (simtst98: `outattr`'s `i` vs `a.i`).
                    self.note_inline_body_local(&param.name);
                    scope_restore.push((param.name.clone(), previous));
                    self.sync_inline_scope_restore(&scope_restore);
                    if let Type::ObjectRef(qual) = &param.ty {
                        self.note_object_qual(id, qual.clone());
                    }
                    self.store_value_param(id, value, ty, argument.span.clone());
                }
                ParamMode::Reference => {
                    let actual_id = self.reference_param_actual_local(argument)?;
                    let expected = mir_type_for(&param.ty)?;
                    let actual_ty = self.local_ty(actual_id);
                    if actual_ty != expected {
                        return Err(spanned_error(
                            format!(
                                "argument type mismatch calling '{}' (expected {expected}, found {actual_ty})",
                                procedure.name
                            ),
                            argument.span.clone(),
                        ));
                    }
                    // §4.6.3: text/`ref(C)` formals are *local* variables
                    // holding a copy of the reference, so `:-` in the body must
                    // not reach the actual. Arrays keep aliasing (see
                    // `inline_ref_alias_procedure`).
                    let bound_id = match expected {
                        MirType::Text => {
                            let bound = self.new_local(param.name.clone(), MirType::Text);
                            self.push(Op::TextNotext { dest: bound }, param.span.clone());
                            self.push(
                                Op::TextRefAssign {
                                    dest: bound,
                                    src: actual_id,
                                },
                                param.span.clone(),
                            );
                            bound
                        }
                        MirType::ObjectRef => {
                            let bound = self.new_local(param.name.clone(), MirType::ObjectRef);
                            self.push(
                                Op::Copy {
                                    dest: bound,
                                    src: actual_id,
                                },
                                param.span.clone(),
                            );
                            bound
                        }
                        _ => actual_id,
                    };
                    let previous = self.scope.insert(param.name.clone(), bound_id);
                    self.note_inline_body_local(&param.name);
                    scope_restore.push((param.name.clone(), previous));
                    self.sync_inline_scope_restore(&scope_restore);
                    if let Type::ObjectRef(qual) = &param.ty {
                        if bound_id != actual_id {
                            self.note_object_qual(bound_id, qual.clone());
                            self.note_object_qual_from_assign(bound_id, actual_id);
                            ref_qual_restore.push((bound_id, None));
                        } else if !self.ref_qual.contains_key(&actual_id) {
                            self.ref_qual.insert(actual_id, qual.clone());
                            ref_qual_restore.push((actual_id, None));
                        }
                    }
                }
            }
        }

        let result_local = if let Some(result_ty) = &procedure.result_type {
            let ty = mir_type_for(result_ty)?;
            let id = self.new_local(procedure.name.clone(), ty);
            let previous = self.scope.insert(procedure.name.clone(), id);
            self.note_inline_body_local(&procedure.name);
            scope_restore.push((procedure.name.clone(), previous));
            self.sync_inline_scope_restore(&scope_restore);
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
                MirType::ArrayI64
                | MirType::ArrayF64
                | MirType::ArrayText
                | MirType::RefI64
                | MirType::FuncRef => {
                    unreachable!("procedure results are never array/ref-pointer types")
                }
            }
            Some(id)
        } else {
            None
        };

        self.with_fresh_label_scope(&procedure.body, |this| {
            this.lower_block_body(&procedure.body)
        })?;

        for (id, previous) in ref_qual_restore.into_iter().rev() {
            match previous {
                Some(qual) => {
                    self.ref_qual.insert(id, qual);
                }
                None => {
                    self.ref_qual.remove(&id);
                }
            }
        }
        for (name, previous) in scope_restore.into_iter().rev() {
            match previous {
                Some(id) => {
                    self.scope.insert(name, id);
                }
                None => {
                    self.scope.remove(&name);
                }
            }
        }
        self.name_bindings = self.name_env_stack.pop().unwrap_or(saved_bindings);
        self.name_formal_tys = self.name_formal_ty_stack.pop().unwrap_or(saved_formal_tys);
        self.formal_proc_bindings = self
            .formal_proc_env_stack
            .pop()
            .unwrap_or(saved_formal_procs);
        self.formal_label_bindings = self
            .formal_label_env_stack
            .pop()
            .unwrap_or(saved_formal_labels);
        self.formal_switch_bindings = self
            .formal_switch_env_stack
            .pop()
            .unwrap_or(saved_formal_switches);
        self.connection_depth = saved_connection_depth;
        self.inspect_connection_depth = saved_inspect_connection_depth;
        self.inline_stack.pop();
        self.inline_debug_scopes.pop();
        self.inline_detach_names_receiver.pop();
        self.inline_scope_restores.pop();
        self.inline_body_locals.pop();

        if as_expression {
            let Some(id) = result_local else {
                return Err(spanned_error(
                    format!(
                        "MIR lowering: procedure '{}' does not return a value and cannot be used in an expression",
                        procedure.name
                    ),
                    span,
                ));
            };
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// Inlines a text/`ref(C)` (and mixed array) call-by-reference procedure by
    /// binding each reference formal to the caller's [`LocalId`] (true alias
    /// for `:-` / stores). Value formals become temporaries.
    pub(in crate::mir::lower) fn inline_ref_alias_procedure(
        &mut self,
        procedure: &ProcedureDeclaration,
        arguments: &[Expr],
        span: Span,
        as_expression: bool,
    ) -> Result<Option<LocalId>, CompileError> {
        if self
            .inline_stack
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&procedure.name))
        {
            return Err(spanned_error(
                format!(
                    "MIR lowering: recursive call-by-reference procedure '{}' is not supported yet (text/ref aliasing is inlined at each call site)",
                    procedure.name
                ),
                span,
            ));
        }
        if arguments.len() != procedure.parameters.len() {
            return Err(spanned_error(
                format!(
                    "procedure '{}' expects {} argument(s), found {}",
                    procedure.name,
                    procedure.parameters.len(),
                    arguments.len()
                ),
                span,
            ));
        }

        let mut scope_restore: Vec<(String, Option<LocalId>)> = Vec::new();
        let mut ref_qual_restore: Vec<(LocalId, Option<String>)> = Vec::new();
        let detach_names_receiver = self
            .method_this
            .is_some_and(|this| self.object_method_name(this, &procedure.name).is_some());
        self.inline_detach_names_receiver
            .push(detach_names_receiver);
        self.inline_stack.push(procedure.name.clone());
        self.inline_debug_scopes.push(procedure.span.clone());
        self.record_debug_scope(
            procedure.name.clone(),
            procedure.span.clone(),
            DebugScopeKind::Procedure,
        );
        self.inline_body_locals.push(HashSet::new());
        let saved_connection_depth = self.connection_depth;
        let saved_inspect_connection_depth = self.inspect_connection_depth;
        self.connection_depth = 0;
        self.inspect_connection_depth = 0;

        // Evaluate every actual before any formal enters `scope`, so a formal
        // named `rav` cannot shadow `rav.attr` in a later actual (simtst30
        // `testreference(rav.tv, rav.rav, rav.rbv, …)` inside `P2`).
        enum PendingRefAliasFormal<'p> {
            Reference {
                param: &'p FormalParameter,
                actual_id: LocalId,
            },
            Value {
                param: &'p FormalParameter,
                value: LocalId,
                ty: MirType,
                span: Span,
            },
        }
        let mut pending: Vec<PendingRefAliasFormal<'_>> = Vec::new();
        for (param, argument) in procedure.parameters.iter().zip(arguments) {
            match param.mode {
                ParamMode::Reference => {
                    let actual_id = self.reference_param_actual_local(argument)?;
                    let expected = mir_type_for(&param.ty)?;
                    let actual_ty = self.local_ty(actual_id);
                    if actual_ty != expected {
                        return Err(spanned_error(
                            format!(
                                "argument type mismatch calling '{}' (expected {expected}, found {actual_ty})",
                                procedure.name
                            ),
                            argument.span.clone(),
                        ));
                    }
                    pending.push(PendingRefAliasFormal::Reference { param, actual_id });
                }
                ParamMode::Value => {
                    let ty = mir_type_for(&param.ty)?;
                    let value = self.lower_expr(argument)?;
                    let value = self.coerce_value(
                        ty,
                        value,
                        format!(
                            "argument type mismatch calling '{}' (expected {ty})",
                            procedure.name
                        ),
                        argument.span.clone(),
                    )?;
                    pending.push(PendingRefAliasFormal::Value {
                        param,
                        value,
                        ty,
                        span: argument.span.clone(),
                    });
                }
                ParamMode::Name => {
                    return Err(spanned_error(
                        format!(
                            "MIR lowering: internal error: name parameter '{}' in ref-alias procedure",
                            param.name
                        ),
                        param.span.clone(),
                    ));
                }
            }
        }
        for bind in pending {
            match bind {
                PendingRefAliasFormal::Reference { param, actual_id } => {
                    // §4.6.3: the formal is a *local* variable holding a copy of
                    // the reference, so `:-` inside the body must not reach the
                    // actual. Arrays are the exception the standard calls out:
                    // the formal cannot be changed at all and denotes the same
                    // array throughout, so it keeps aliasing the actual.
                    let bound_id = match mir_type_for(&param.ty)? {
                        MirType::Text => {
                            let bound = self.new_local(param.name.clone(), MirType::Text);
                            self.push(Op::TextNotext { dest: bound }, param.span.clone());
                            self.push(
                                Op::TextRefAssign {
                                    dest: bound,
                                    src: actual_id,
                                },
                                param.span.clone(),
                            );
                            bound
                        }
                        MirType::ObjectRef => {
                            let bound = self.new_local(param.name.clone(), MirType::ObjectRef);
                            self.push(
                                Op::Copy {
                                    dest: bound,
                                    src: actual_id,
                                },
                                param.span.clone(),
                            );
                            bound
                        }
                        _ => actual_id,
                    };
                    let previous = self.scope.insert(param.name.clone(), bound_id);
                    self.note_inline_body_local(&param.name);
                    scope_restore.push((param.name.clone(), previous));
                    if let Type::ObjectRef(qual) = &param.ty {
                        if bound_id != actual_id {
                            // Binding the formal is a `:-`, so carry over the
                            // actual's instance layout as well as its access
                            // qualification; field offsets and capture refresh
                            // both key off the instance layout.
                            self.note_object_qual(bound_id, qual.clone());
                            self.note_object_qual_from_assign(bound_id, actual_id);
                        } else if !self.ref_qual.contains_key(&actual_id) {
                            self.ref_qual.insert(actual_id, qual.clone());
                        }
                        ref_qual_restore.push((bound_id, None));
                    }
                }
                PendingRefAliasFormal::Value {
                    param,
                    value,
                    ty,
                    span,
                } => {
                    let id = self.new_local(param.name.clone(), ty);
                    let previous = self.scope.insert(param.name.clone(), id);
                    self.note_inline_body_local(&param.name);
                    scope_restore.push((param.name.clone(), previous));
                    if let Type::ObjectRef(qual) = &param.ty {
                        self.note_object_qual(id, qual.clone());
                    }
                    self.store_value_param(id, value, ty, span);
                }
            }
        }

        let result_local = if let Some(result_ty) = &procedure.result_type {
            let ty = mir_type_for(result_ty)?;
            let id = self.new_local(procedure.name.clone(), ty);
            let previous = self.scope.insert(procedure.name.clone(), id);
            scope_restore.push((procedure.name.clone(), previous));
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
                MirType::ArrayI64
                | MirType::ArrayF64
                | MirType::ArrayText
                | MirType::RefI64
                | MirType::FuncRef => {
                    unreachable!("procedure results are never array/ref-pointer types")
                }
            }
            // Declared `ref(T)` result qualification wins over the actual's
            // dynamic class (simtst46: `rP(rb).iP` uses `A.iP`, not `B.iP`).
            if let Type::ObjectRef(qual) = result_ty {
                self.note_object_qual(id, qual.clone());
            }
            Some(id)
        } else {
            None
        };

        self.with_fresh_label_scope(&procedure.body, |this| {
            this.lower_block_body(&procedure.body)
        })?;

        for (id, previous) in ref_qual_restore.into_iter().rev() {
            match previous {
                Some(qual) => {
                    self.ref_qual.insert(id, qual);
                }
                None => {
                    self.ref_qual.remove(&id);
                }
            }
        }
        for (name, previous) in scope_restore.into_iter().rev() {
            match previous {
                Some(id) => {
                    self.scope.insert(name, id);
                }
                None => {
                    self.scope.remove(&name);
                }
            }
        }
        self.connection_depth = saved_connection_depth;
        self.inspect_connection_depth = saved_inspect_connection_depth;
        self.inline_stack.pop();
        self.inline_debug_scopes.pop();
        self.inline_detach_names_receiver.pop();
        self.inline_body_locals.pop();

        if as_expression {
            let Some(id) = result_local else {
                return Err(spanned_error(
                    format!(
                        "MIR lowering: procedure '{}' does not return a value and cannot be used in an expression",
                        procedure.name
                    ),
                    span,
                ));
            };
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// Runs `f` with [`Self::name_bindings`] restored to the caller's Jensen
    /// environment (popped from [`Self::name_env_stack`] for the duration of
    /// `f`, so nested name-actual evaluation walks outward — needed when a
    /// formal is bound to the same identifier as an outer name formal, e.g.
    /// `P(..., i)` inlining into `Q(..., i)` with `name i` on both).
    ///
    /// Also temporarily restores outer [`Self::scope`] bindings for the current
    /// frame's formals so a formal named `rav` does not hide the caller's
    /// `rav` while re-evaluating a name actual like `rav.tva1`. The current
    /// [`Self::inline_scope_restores`] frame is popped for the duration of `f`
    /// so a nested `with_caller_name_env` applies the *caller's* restores next
    /// (simtst63: `Q`'s `i` → `P`'s `i` → outer `i`).
    pub(in crate::mir::lower) fn with_caller_name_env<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, CompileError>,
    ) -> Result<R, CompileError> {
        let caller = self.name_env_stack.pop().unwrap_or_default();
        let caller_tys = self.name_formal_ty_stack.pop().unwrap_or_default();
        let caller_formal = self.formal_proc_env_stack.pop().unwrap_or_default();
        let saved = std::mem::replace(&mut self.name_bindings, caller);
        let saved_tys = std::mem::replace(&mut self.name_formal_tys, caller_tys);
        let saved_formal = std::mem::replace(&mut self.formal_proc_bindings, caller_formal);

        let restores = self.inline_scope_restores.pop().unwrap_or_default();
        let mut saved_callee_scope = Vec::new();
        for (name, previous) in &restores {
            let current = self.scope.remove(name).or_else(|| {
                let key = self
                    .scope
                    .keys()
                    .find(|key| key.eq_ignore_ascii_case(name))
                    .cloned()?;
                self.scope.remove(&key)
            });
            saved_callee_scope.push((name.clone(), current));
            if let Some(id) = previous {
                self.scope.insert(name.clone(), *id);
            }
        }

        let result = f(self);

        for (name, current) in saved_callee_scope {
            match current {
                Some(id) => {
                    self.scope.insert(name, id);
                }
                None => {
                    self.scope.remove(&name);
                }
            }
        }
        self.inline_scope_restores.push(restores);

        // Restore the caller frame we borrowed, then the callee bindings.
        self.name_env_stack
            .push(std::mem::replace(&mut self.name_bindings, saved));
        self.name_formal_ty_stack
            .push(std::mem::replace(&mut self.name_formal_tys, saved_tys));
        self.formal_proc_env_stack.push(std::mem::replace(
            &mut self.formal_proc_bindings,
            saved_formal,
        ));
        result
    }

    /// Freeze by-value designational actuals: evaluate conditions/subscripts
    /// once into temporaries so later `goto` uses the call-time destination.
    pub(in crate::mir::lower) fn freeze_designational_value(
        &mut self,
        target: DesignationalExpr,
        span: Span,
    ) -> Result<DesignationalExpr, CompileError> {
        match target {
            DesignationalExpr::Label(name) => Ok(DesignationalExpr::Label(name)),
            DesignationalExpr::Paren(inner) => Ok(DesignationalExpr::Paren(Box::new(
                self.freeze_designational_value(*inner, span)?,
            ))),
            DesignationalExpr::SwitchDesignator { name, subscript } => {
                let index = self.with_caller_name_env(|this| this.lower_expr(&subscript))?;
                let index = match self.local_ty(index) {
                    MirType::I64 => index,
                    MirType::F64 | MirType::LongF64 => self.f64_to_i64(index, span.clone()),
                    other => {
                        return Err(spanned_error(
                            format!("switch designator subscript must be integer, found {other}"),
                            span,
                        ));
                    }
                };
                let frozen_name = format!("$labelidx{}", self.temp_counter);
                self.temp_counter += 1;
                let frozen = self.new_local(frozen_name.clone(), MirType::I64);
                self.scope.insert(frozen_name.clone(), frozen);
                self.push(
                    Op::Copy {
                        dest: frozen,
                        src: index,
                    },
                    span.clone(),
                );
                Ok(DesignationalExpr::SwitchDesignator {
                    name,
                    subscript: Box::new(Expr::new(
                        ExprKind::Variable(Variable::Simple(frozen_name)),
                        span,
                    )),
                })
            }
            DesignationalExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let cond = self.with_caller_name_env(|this| this.lower_bool_expr(&condition))?;
                let frozen_name = format!("$labelcond{}", self.temp_counter);
                self.temp_counter += 1;
                let frozen = self.new_local(frozen_name.clone(), MirType::Bool);
                self.scope.insert(frozen_name.clone(), frozen);
                self.push(
                    Op::Copy {
                        dest: frozen,
                        src: cond,
                    },
                    span.clone(),
                );
                let then_expr =
                    Box::new(self.freeze_designational_value(*then_expr, span.clone())?);
                let else_expr =
                    Box::new(self.freeze_designational_value(*else_expr, span.clone())?);
                Ok(DesignationalExpr::If {
                    condition: Box::new(Expr::new(
                        ExprKind::Variable(Variable::Simple(frozen_name)),
                        span,
                    )),
                    then_expr,
                    else_expr,
                })
            }
        }
    }

    pub(in crate::mir::lower) fn sync_inline_scope_restore(
        &mut self,
        scope_restore: &[(String, Option<LocalId>)],
    ) {
        if let Some(frame) = self.inline_scope_restores.last_mut() {
            *frame = scope_restore.to_vec();
        }
    }

    /// Records a name declared in the current inlined procedure body so it
    /// shadows a same-named call-by-name formal (simtst39).
    pub(in crate::mir::lower) fn note_inline_body_local(&mut self, name: &str) {
        if let Some(frame) = self.inline_body_locals.last_mut() {
            frame.insert(name.to_ascii_lowercase());
        }
    }

    pub(in crate::mir::lower) fn is_inline_body_local(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.inline_body_locals
            .last()
            .is_some_and(|frame| frame.contains(&lower))
    }

    /// Nested `begin ref(C) Y` inside a resumable `$__init` is promoted to an
    /// object field (see [`layout::collect_scalar_fields_from_statement`]). Reads
    /// and writes must use that field so `Y` survives detach/re-entry (simtst76).
    /// Inlined free procedure inside a class body: a shadowed enclosing-capture
    /// field beats a same-named class attribute, so lexical outer variables
    /// stay visible (`P` uses outer `i` while class `A` also declares `i`).
    ///
    /// Off inside `inspect` connection blocks — bare names there are the
    /// connected object's attributes (§4.8), not `__simrt_encl_*`
    /// sibling-capture slots (simtst63).
    pub(in crate::mir::lower) fn shadowed_enclosing_capture_place(
        &self,
        name: &str,
    ) -> Option<Place> {
        if self.inline_stack.is_empty() || self.access_level_substitutions {
            return None;
        }
        let encl = enclosing_capture_field_name(name);
        self.method_this_chain().find_map(|(this_id, qual)| {
            self.method_field_info_at(this_id, &encl, qual).map(
                |(offset, field_ty, object_qual)| {
                    self.object_field_place(this_id, offset, &encl, field_ty, object_qual)
                },
            )
        })
    }

    pub(in crate::mir::lower) fn try_resumable_promoted_field_place(
        &self,
        name: &str,
    ) -> Option<Place> {
        for (this_id, qual) in self.method_this_chain() {
            let layout = self.layout_for_object(this_id)?;
            if !layout.runs_on_own_stack {
                return None;
            }
            let (offset, field_ty, object_qual) = self.method_field_info_at(this_id, name, qual)?;
            if name.starts_with("__simrt_encl_")
                || name.eq_ignore_ascii_case(ENCLOSING_OBJECT_FIELD_NAME)
            {
                continue;
            }
            return Some(self.object_field_place(this_id, offset, name, field_ty, object_qual));
        }
        None
    }

    /// Maps a formal procedure name to its bound actual, if any.
    pub(in crate::mir::lower) fn resolve_formal_proc_target(
        &self,
        name: &str,
    ) -> Option<&FormalProcTarget> {
        self.formal_proc_bindings.get(name).or_else(|| {
            self.formal_proc_bindings
                .iter()
                .find(|(formal, _)| formal.eq_ignore_ascii_case(name))
                .map(|(_, target)| target)
        })
    }

    /// Case-insensitive lookup of a call-by-name / formal-procedure procedure.
    pub(in crate::mir::lower) fn lookup_name_param_proc(
        &self,
        name: &str,
    ) -> Option<&'a ProcedureDeclaration> {
        self.name_param_procs.get(name).copied().or_else(|| {
            self.name_param_procs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, procedure)| *procedure)
        })
    }

    /// True when a user procedure/method named `name` is in scope and should
    /// shadow an ENVIRONMENT builtin of the same identifier (e.g. `error`).
    pub(in crate::mir::lower) fn user_procedure_shadows_builtin(&self, name: &str) -> bool {
        if self.signatures.contains_key(name)
            || self
                .signatures
                .keys()
                .any(|key| key.eq_ignore_ascii_case(name))
            || self.lookup_name_param_proc(name).is_some()
            || self.lookup_ref_alias_proc(name).is_some()
        {
            return true;
        }
        for (this_id, _) in self.method_this_chain() {
            if self.object_method_name(this_id, name).is_some() {
                return true;
            }
        }
        false
    }

    /// Whether `name` is a local array or a call-by-name formal (typically an
    /// array actual) and should win over a same-named procedure for `name(args)`.
    pub(in crate::mir::lower) fn name_is_subscriptable(&self, name: &str) -> bool {
        if self.name_bindings.contains_key(name)
            || self
                .name_bindings
                .keys()
                .any(|key| key.eq_ignore_ascii_case(name))
        {
            return true;
        }
        if self.scope.get(name).is_some_and(|&id| {
            matches!(
                self.local_ty(id),
                MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
            )
        }) || self.scope.iter().any(|(key, &id)| {
            key.eq_ignore_ascii_case(name)
                && matches!(
                    self.local_ty(id),
                    MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
                )
        }) {
            return true;
        }
        // Class / connection body: enclosing arrays (incl. boolean → ArrayBool)
        // live as fields on `__this` (or an outer inspect/method receiver).
        let is_array_field = |ty: FieldType| {
            matches!(
                ty,
                FieldType::ArrayI64
                    | FieldType::ArrayBool
                    | FieldType::ArrayF64
                    | FieldType::ArrayText
                    | FieldType::ObjectRef
            )
        };
        if self
            .lookup_method_field(name)
            .is_some_and(|(_, _, ty, _)| is_array_field(ty))
        {
            return true;
        }
        false
    }

    /// Case-insensitive lookup of a text/`ref` alias / enclosing-capture procedure.
    pub(in crate::mir::lower) fn lookup_ref_alias_proc(
        &self,
        name: &str,
    ) -> Option<&'a ProcedureDeclaration> {
        self.ref_alias_procs.get(name).copied().or_else(|| {
            self.ref_alias_procs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, procedure)| *procedure)
        })
    }

    pub(in crate::mir::lower) fn annotate_call_result(
        &mut self,
        dest: LocalId,
        signature: &ProcSignature,
    ) {
        if let Some(qual) = &signature.result_object_qual {
            self.note_object_qual(dest, qual.clone());
        }
    }

    /// Maps a formal procedure name to a free procedure identifier, if bound.
    pub(in crate::mir::lower) fn resolve_formal_procedure_name(&self, name: &str) -> Option<&str> {
        match self.resolve_formal_proc_target(name)? {
            FormalProcTarget::Procedure(actual) => Some(actual.as_str()),
            FormalProcTarget::Method { .. } => None,
        }
    }

    pub(in crate::mir::lower) fn resolve_formal_label(
        &self,
        name: &str,
    ) -> Option<&DesignationalExpr> {
        self.formal_label_bindings.get(name).or_else(|| {
            self.formal_label_bindings
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, expr)| expr)
        })
    }

    pub(in crate::mir::lower) fn resolve_formal_switch(&self, name: &str) -> Option<&str> {
        self.formal_switch_bindings
            .get(name)
            .map(|s| s.as_str())
            .or_else(|| {
                self.formal_switch_bindings
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, s)| s.as_str())
            })
    }

    /// Returns the canonical declared name for a known local procedure.
    pub(in crate::mir::lower) fn resolve_known_procedure(&self, name: &str) -> Option<String> {
        if self.signatures.contains_key(name)
            || self.name_param_procs.contains_key(name)
            || self.ref_alias_procs.contains_key(name)
        {
            return Some(name.to_string());
        }
        self.signatures
            .keys()
            .find(|key| key.eq_ignore_ascii_case(name))
            .cloned()
            .or_else(|| {
                self.name_param_procs
                    .keys()
                    .find(|key| key.eq_ignore_ascii_case(name))
                    .cloned()
            })
            .or_else(|| {
                self.ref_alias_procs
                    .keys()
                    .find(|key| key.eq_ignore_ascii_case(name))
                    .cloned()
            })
    }

    /// Resolves a call-by-reference actual to a [`LocalId`] for **inlined**
    /// procedures. A simple variable actual aliases the caller's existing
    /// local directly, so writes through the formal remain visible to the
    /// caller (true reference semantics). Any other actual — `new A`, a
    /// literal, an arbitrary expression, or even a subscripted variable —
    /// is evaluated once into a fresh, unaliased temporary; the call still
    /// lowers, but writes through the formal do not propagate back (there
    /// is nothing meaningful to alias).
    pub(in crate::mir::lower) fn reference_param_actual_local(
        &mut self,
        argument: &Expr,
    ) -> Result<LocalId, CompileError> {
        if let ExprKind::Variable(Variable::Simple(name)) = &argument.kind
            && let Some(&id) = self.scope.get(name)
        {
            return Ok(id);
        }
        self.lower_expr(argument)
    }

    /// Re-evaluates a call-by-name actual in the caller environment.
    pub(in crate::mir::lower) fn lower_name_actual(
        &mut self,
        formal: &str,
    ) -> Result<LocalId, CompileError> {
        let Some(actual) = self.name_bindings.get(formal).cloned().or_else(|| {
            self.name_bindings
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(formal))
                .map(|(_, expr)| expr.clone())
        }) else {
            return Err(spanned_error(
                format!("undefined call-by-name parameter '{formal}'"),
                0..0,
            ));
        };
        let formal_ty = self.name_formal_tys.get(formal).copied().or_else(|| {
            self.name_formal_tys
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(formal))
                .map(|(_, ty)| *ty)
        });
        let value = self.with_caller_name_env(|this| this.lower_expr(&actual))?;
        if let Some(ty) = formal_ty {
            self.coerce_value(
                ty,
                value,
                format!("name parameter '{formal}' type mismatch"),
                0..0,
            )
        } else {
            Ok(value)
        }
    }

    pub(in crate::mir::lower) fn name_formal_ty(&self, formal: &str) -> Option<MirType> {
        self.name_formal_tys.get(formal).copied().or_else(|| {
            self.name_formal_tys
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(formal))
                .map(|(_, ty)| *ty)
        })
    }

    /// Resolves an assignable place for a call-by-name formal in the caller
    /// environment (re-evaluating subscript actuals).
    pub(in crate::mir::lower) fn resolve_name_actual_place(
        &mut self,
        formal: &str,
        span: Span,
    ) -> Result<Place, CompileError> {
        let Some(actual) = self.name_bindings.get(formal).cloned() else {
            return Err(spanned_error(
                format!("undefined call-by-name parameter '{formal}'"),
                span,
            ));
        };
        self.with_caller_name_env(
            |this| match variable_from_name_actual(&actual, span.clone()) {
                Ok(variable) => this.resolve_place(&variable, span),
                // Not a variable actual (`new A`, a literal, an arbitrary
                // expression, …): evaluate it once into a temp and use that as
                // the assignment target. Writes are visible to later reads of
                // the name formal within the same call but do not propagate to
                // the caller — matching the fact that there is nothing to alias.
                Err(_) => {
                    let value = this.lower_expr(&actual)?;
                    Ok(Place::Local(value))
                }
            },
        )
    }

    /// Lowers a call site's argument expressions against `signature`,
    /// checking arity and (since value parameters are the only mode the
    /// scalar subset lowers) that each argument's type matches the
    /// corresponding formal parameter's declared type.
    pub(in crate::mir::lower) fn lower_call_arguments(
        &mut self,
        proc_name: &str,
        signature: &ProcSignature,
        arguments: &[Expr],
        span: Span,
    ) -> Result<Vec<LocalId>, CompileError> {
        // External stubs from `external procedure pa, pb` have an unknown
        // formal list; evaluate actuals for side effects and call with no args.
        if signature.external_stub && signature.params.is_empty() {
            for argument in arguments {
                let _ = self.lower_expr(argument)?;
            }
            return Ok(Vec::new());
        }

        let expected_arg_count = signature.params.len()
            - signature.name_thunk_starts.len() * 2
            - signature.formal_proc_param_indices.len()
            - signature.free_cell_params.len();
        if arguments.len() != expected_arg_count {
            return Err(spanned_error(
                format!(
                    "procedure '{proc_name}' expects {} argument(s), found {}",
                    expected_arg_count,
                    arguments.len()
                ),
                span,
            ));
        }

        let mut mir_args = Vec::with_capacity(signature.params.len());
        let mut arg_index = 0;
        let mut param_index = 0;
        let mut thunk_cursor = 0;
        let mut formal_proc_cursor = 0;
        let user_param_end = signature.params.len() - signature.free_cell_params.len();
        while param_index < user_param_end {
            if signature
                .name_thunk_starts
                .get(thunk_cursor)
                .is_some_and(|&start| start == param_index)
            {
                let argument = &arguments[arg_index];
                let assigned = signature.name_thunk_assigned[thunk_cursor];
                let (get, set, env) =
                    self.lower_name_thunk_actual(proc_name, argument, assigned)?;
                mir_args.push(get);
                mir_args.push(set);
                mir_args.push(env);
                arg_index += 1;
                param_index += 3;
                thunk_cursor += 1;
                continue;
            }
            if signature
                .formal_proc_param_indices
                .get(formal_proc_cursor)
                .is_some_and(|&start| start == param_index)
            {
                let argument = &arguments[arg_index];
                let (func, env) = self.lower_formal_proc_actual(argument, span.clone())?;
                mir_args.push(func);
                mir_args.push(env);
                arg_index += 1;
                param_index += 2;
                formal_proc_cursor += 1;
                continue;
            }

            let argument = &arguments[arg_index];
            let expected_ty = signature.params[param_index];
            let value = self.lower_expr(argument)?;
            let value = self.coerce_value(
                expected_ty,
                value,
                format!("argument type mismatch calling '{proc_name}' (expected {expected_ty})"),
                argument.span.clone(),
            )?;
            // §4.6.2: a value text formal is `FP :- copy(AP)`, and a value
            // array formal gets its own descriptor. Call-by-reference formals
            // (§4.6.3) pass the handle, so the callee shares the frame/object
            // but rebinding the formal stays local to it.
            let value = if matches!(
                expected_ty,
                MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
            ) && signature.value_array_params.contains(&param_index)
            {
                let copied = self.temp(expected_ty);
                self.push(
                    Op::ArrayCopy {
                        dest: copied,
                        src: value,
                    },
                    argument.span.clone(),
                );
                copied
            } else if expected_ty == MirType::Text
                && signature.value_text_params.contains(&param_index)
            {
                let copied = self.temp(MirType::Text);
                self.push(
                    Op::TextCopy {
                        dest: copied,
                        src: value,
                    },
                    argument.span.clone(),
                );
                copied
            } else if expected_ty == MirType::Text {
                self.bind_text_reference_actual(value, argument.span.clone())
            } else {
                value
            };
            mir_args.push(value);
            arg_index += 1;
            param_index += 1;
        }

        for name in &signature.free_cell_params {
            // A free-cell parameter is a linear cell address, so forward the
            // address this scope already holds rather than the boxed
            // name-thunk env built over it (see `bind_free_cell_thunk_helpers`).
            if let Some(&addr) = self.lookup_free_cell_addr(name) {
                mir_args.push(addr);
                continue;
            }
            let Some(&id) = self.scope.get(name).or_else(|| {
                self.scope
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, id)| id)
            }) else {
                return Err(spanned_error(
                    format!("undeclared variable '{name}'"),
                    span.clone(),
                ));
            };
            if !matches!(self.local_ty(id), MirType::I64 | MirType::Bool) {
                return Err(spanned_error(
                    format!(
                        "outlined procedure '{proc_name}' free cell '{name}' requires an integer or boolean variable"
                    ),
                    span.clone(),
                ));
            }
            let addr = self.temp(MirType::RefI64);
            self.push(
                Op::LocalAddr {
                    dest: addr,
                    local: id,
                },
                span.clone(),
            );
            mir_args.push(addr);
        }

        Ok(mir_args)
    }

    /// Builds the `(func, env)` fat pointer for an outlined formal-procedure actual.
    pub(in crate::mir::lower) fn lower_formal_proc_actual(
        &mut self,
        argument: &Expr,
        span: Span,
    ) -> Result<(LocalId, LocalId), CompileError> {
        match &argument.kind {
            ExprKind::Paren(inner) => return self.lower_formal_proc_actual(inner, span),
            ExprKind::Variable(Variable::Simple(name)) => {
                if let Some(&(func, env)) = self.formal_proc_refs.get(name).or_else(|| {
                    self.formal_proc_refs
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, pair)| pair)
                }) {
                    return Ok((func, env));
                }
                // Pass-through of an inlined formal bound to a free procedure:
                // materialize a shim for the bound actual.
                if let Some(FormalProcTarget::Procedure(actual)) =
                    self.resolve_formal_proc_target(name).cloned()
                {
                    return self.formal_proc_fat_pointer_for_named(&actual, span);
                }
                return self.formal_proc_fat_pointer_for_named(name, span);
            }
            _ => {}
        }
        let actual_name = procedure_identifier_actual(argument)?;
        self.formal_proc_fat_pointer_for_named(&actual_name, span)
    }

    pub(in crate::mir::lower) fn formal_proc_fat_pointer_for_named(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<(LocalId, LocalId), CompileError> {
        let resolved = self.resolve_known_procedure(name).ok_or_else(|| {
            spanned_error(
                format!(
                    "MIR lowering: formal procedure actual '{name}' is not a known outlined procedure"
                ),
                span.clone(),
            )
        })?;
        let signature = self.signatures.get(&resolved).cloned().ok_or_else(|| {
            spanned_error(
                format!(
                    "MIR lowering: formal procedure actual '{resolved}' has no outlined signature"
                ),
                span.clone(),
            )
        })?;
        // Invoke shim: `fn(env: RefI64)` unpacks free cells and calls `resolved`.
        let shim_name = formal_proc_invoke_name(&resolved);
        if !self.pending_helpers.iter().any(|f| f.name == shim_name)
            && !self.signatures.contains_key(&shim_name)
        {
            self.pending_helpers.push(build_formal_proc_invoke_helper(
                &shim_name, &resolved, &signature,
            ));
        }
        let func = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: func,
                name: shim_name,
            },
            span.clone(),
        );
        let free = &signature.free_cell_params;
        let bytes = (free.len() as i64 * 8).max(8);
        let env = self.temp(MirType::RefI64);
        self.push(Op::StackAlloc { dest: env, bytes }, span.clone());
        for (index, cell_name) in free.iter().enumerate() {
            let addr = if let Some(&addr) = self.lookup_free_cell_addr(cell_name) {
                addr
            } else {
                let Some(&id) = self.scope.get(cell_name).or_else(|| {
                    self.scope
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(cell_name))
                        .map(|(_, id)| id)
                }) else {
                    return Err(spanned_error(
                        format!("undeclared variable '{cell_name}'"),
                        span.clone(),
                    ));
                };
                if !matches!(self.local_ty(id), MirType::I64 | MirType::Bool) {
                    return Err(spanned_error(
                        format!(
                            "formal procedure free cell '{cell_name}' requires an integer or boolean variable"
                        ),
                        span.clone(),
                    ));
                }
                let addr = self.temp(MirType::RefI64);
                self.push(
                    Op::LocalAddr {
                        dest: addr,
                        local: id,
                    },
                    span.clone(),
                );
                addr
            };
            let bits = self.temp(MirType::I64);
            self.push(
                Op::Copy {
                    dest: bits,
                    src: addr,
                },
                span.clone(),
            );
            self.push(
                Op::StoreRefI64 {
                    ptr: env,
                    src: bits,
                    offset: (index as i64) * 8,
                },
                span.clone(),
            );
        }
        Ok((func, env))
    }

    /// Calls an outlined formal-procedure fat pointer as a statement (`F;`).
    pub(in crate::mir::lower) fn lower_formal_proc_ref_call(
        &mut self,
        func: LocalId,
        env: LocalId,
        span: Span,
    ) -> Result<(), CompileError> {
        self.push(
            Op::CallIndirect {
                dest: None,
                callee: func,
                args: vec![env],
                sig: CallSig {
                    params: vec![MirType::RefI64],
                    result: None,
                },
            },
            span,
        );
        Ok(())
    }
    ///
    /// Simple variables build the triple from the shared
    /// `__simrt_name_get_ref` / `__simrt_name_set_ref` helpers
    /// ([`Op::FuncAddr`]) plus [`Op::LocalAddr`] on the variable's cell. An
    /// actual that is itself an enclosing name-thunk formal passes its three
    /// locals straight through. Simple remote integer fields (`r.x`) use
    /// per-offset `__simrt_name_{get,set}_field_N` helpers with an env that
    /// packs the object reference. Read-only formals also accept expression
    /// actuals via per-call-site get helpers (re-eval) or a temporary cell
    /// fallback. Assigned formals still reject non-L-value expressions.
    pub(in crate::mir::lower) fn lower_name_thunk_actual(
        &mut self,
        proc_name: &str,
        argument: &Expr,
        formal_assigned: bool,
    ) -> Result<(LocalId, LocalId, LocalId), CompileError> {
        match &argument.kind {
            ExprKind::Paren(inner) => {
                self.lower_name_thunk_actual(proc_name, inner, formal_assigned)
            }
            ExprKind::Variable(Variable::Simple(name)) => {
                if let Some(&(get, set, env)) = self.name_thunks.get(name).or_else(|| {
                    self.name_thunks
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, triple)| triple)
                }) {
                    return Ok((get, set, env));
                }
                if let Some(&id) = self.scope.get(name).or_else(|| {
                    self.scope
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, id)| id)
                }) {
                    if !matches!(self.local_ty(id), MirType::I64 | MirType::Bool) {
                        return Err(spanned_error(
                            format!(
                                "argument type mismatch calling '{proc_name}' (expected integer or boolean variable for name parameter, found {})",
                                self.local_ty(id)
                            ),
                            argument.span.clone(),
                        ));
                    }
                    return Ok(self.name_thunk_triple_for_cell(id, argument.span.clone()));
                }
                // Class / connection body: bare attribute used as name actual.
                if let Some((this_id, offset, field_ty, _)) = {
                    // Prefer `__simrt_encl_*` only outside connection blocks.
                    // Inside `inspect`, bare names are the connected attributes
                    // (§4.8); encl slots are sibling-capture artifacts.
                    if !self.inline_stack.is_empty() && !self.access_level_substitutions {
                        let mut found = None;
                        for (this_id, qual) in self.method_this_chain() {
                            let encl = enclosing_capture_field_name(name);
                            if let Some((offset, field_ty, _)) =
                                self.method_field_info_at(this_id, &encl, qual)
                            {
                                found = Some((this_id, offset, field_ty, None));
                                break;
                            }
                        }
                        found.or_else(|| self.lookup_method_field(name))
                    } else {
                        self.lookup_method_field(name)
                    }
                } {
                    if field_ty != FieldType::I64 {
                        return Err(spanned_error(
                            format!(
                                "argument type mismatch calling '{proc_name}' (expected integer field for name parameter '{name}')"
                            ),
                            argument.span.clone(),
                        ));
                    }
                    if self.object_field_is_by_ref_capture(this_id, name, field_ty, offset) {
                        let cell = self.capture_cell_pointer(
                            this_id,
                            offset,
                            MirType::I64,
                            argument.span.clone(),
                        );
                        return Ok(self.name_thunk_triple_for_pointer(cell, argument.span.clone()));
                    }
                    return Ok(self.name_thunk_triple_for_field(
                        this_id,
                        offset,
                        argument.span.clone(),
                    ));
                }
                // Parameterless type procedure / other expression used as a
                // read-only name actual (`P(sqri)`).
                if !formal_assigned {
                    return self.lower_readonly_expr_name_thunk_actual(proc_name, argument);
                }
                Err(spanned_error(
                    format!("undeclared variable '{name}'"),
                    argument.span.clone(),
                ))
            }
            ExprKind::RemoteAccess { object, attribute } => {
                if let ExprKind::Variable(Variable::Simple(object_name)) = &object.kind {
                    return self.lower_simple_remote_field_name_thunk(
                        proc_name,
                        object_name,
                        attribute,
                        argument.span.clone(),
                    );
                }
                if formal_assigned {
                    return Err(spanned_error(
                        format!(
                            "outlined call-by-name procedure '{proc_name}' requires a simple `ref.attr` integer field as a remote name actual when the formal is assigned"
                        ),
                        argument.span.clone(),
                    ));
                }
                self.lower_readonly_expr_name_thunk_actual(proc_name, argument)
            }
            ExprKind::Variable(Variable::Remote { object, attribute }) => {
                if let Variable::Simple(object_name) = object.as_ref() {
                    return self.lower_simple_remote_field_name_thunk(
                        proc_name,
                        object_name,
                        attribute,
                        argument.span.clone(),
                    );
                }
                if formal_assigned {
                    return Err(spanned_error(
                        format!(
                            "outlined call-by-name procedure '{proc_name}' requires a simple `ref.attr` integer field as a remote name actual when the formal is assigned"
                        ),
                        argument.span.clone(),
                    ));
                }
                self.lower_readonly_expr_name_thunk_actual(proc_name, argument)
            }
            ExprKind::FunctionCall { name, .. }
                if self.scope.get(name).is_some_and(|&id| {
                    matches!(
                        self.local_ty(id),
                        MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
                    )
                }) =>
            {
                let array = self
                    .scope
                    .get(name)
                    .copied()
                    .expect("guard checked presence");
                self.lower_array_element_name_thunk_actual(
                    proc_name,
                    argument,
                    array,
                    formal_assigned,
                )
            }
            _ if !formal_assigned => {
                self.lower_readonly_expr_name_thunk_actual(proc_name, argument)
            }
            _ => Err(spanned_error(
                format!(
                    "outlined call-by-name procedure '{proc_name}' requires a simple integer variable as each name actual when the formal is assigned (expression thunks are not supported yet)"
                ),
                argument.span.clone(),
            )),
        }
    }

    /// Read-only name actual: prefer a true re-eval get helper; otherwise
    /// evaluate once into a fresh cell routed through the scalar get/set helpers.
    pub(in crate::mir::lower) fn lower_readonly_expr_name_thunk_actual(
        &mut self,
        proc_name: &str,
        argument: &Expr,
    ) -> Result<(LocalId, LocalId, LocalId), CompileError> {
        // A bare identifier naming a parameterless type procedure (`P(sqri)`):
        // re-run the procedure on every read of the formal.
        if let ExprKind::Variable(Variable::Simple(name)) = &argument.kind
            && !self.scope_has_name(name)
            && !self.name_thunks.contains_key(name)
            && !self.name_bindings.contains_key(name)
            && let Some(triple) = self.try_lower_type_proc_name_thunk_actual(name, argument)?
        {
            return Ok(triple);
        }
        if let Some(triple) = self.try_lower_expr_reeval_thunk(argument) {
            return Ok(triple);
        }
        let value = self.lower_expr(argument)?;
        let value = self.coerce_value(
            MirType::I64,
            value,
            format!(
                "argument type mismatch calling '{proc_name}' (expected integer expression for name parameter)"
            ),
            argument.span.clone(),
        )?;
        let cell = self.temp(MirType::I64);
        self.push(
            Op::StoreLocal {
                local: cell,
                src: value,
            },
            argument.span.clone(),
        );
        Ok(self.name_thunk_triple_for_cell(cell, argument.span.clone()))
    }

    /// Outlined call-by-name actual `r.x` (simple object + integer attribute):
    /// builds a `(get, set, env)` triple through the per-offset field helpers
    /// so every read/write re-touches the live field (including across a
    /// recursive call chain that shares the same `env`).
    pub(in crate::mir::lower) fn lower_simple_remote_field_name_thunk(
        &mut self,
        proc_name: &str,
        object_name: &str,
        attribute: &str,
        span: Span,
    ) -> Result<(LocalId, LocalId, LocalId), CompileError> {
        let Some(&object_id) = self.scope.get(object_name) else {
            return Err(spanned_error(
                format!("undeclared variable '{object_name}'"),
                span,
            ));
        };
        if self.local_ty(object_id) != MirType::ObjectRef {
            return Err(spanned_error(
                format!(
                    "argument type mismatch calling '{proc_name}' (expected object reference for remote name actual '{object_name}.{attribute}', found {})",
                    self.local_ty(object_id)
                ),
                span,
            ));
        }
        let (offset, field_ty) = self.field_info_for(object_id, attribute, span.clone())?;
        if field_ty != FieldType::I64 {
            return Err(spanned_error(
                format!(
                    "outlined call-by-name procedure '{proc_name}' requires an integer remote field as a name actual (found non-integer '{object_name}.{attribute}')"
                ),
                span,
            ));
        }
        Ok(self.name_thunk_triple_for_field(object_id, offset, span))
    }

    /// Coerce a numeric expression to [`MirType::F64`] for ENVIRONMENT helpers.
    pub(in crate::mir::lower) fn lower_expr_as_f64(
        &mut self,
        expr: &Expr,
        proc_name: &str,
    ) -> Result<LocalId, CompileError> {
        let src = self.lower_expr(expr)?;
        match self.local_ty(src) {
            MirType::F64 | MirType::LongF64 => Ok(src),
            MirType::I64 => {
                let dest = self.temp(MirType::F64);
                self.push(Op::I64ToF64 { dest, src }, expr.span.clone());
                Ok(dest)
            }
            other => Err(spanned_error(
                format!("{proc_name} requires a numeric argument, found {other}"),
                expr.span.clone(),
            )),
        }
    }

    /// Address of an integer name variable used as a §9.9 random stream.
    ///
    /// Native codegen passes this [`MirType::RefI64`] to `simrt_*` which
    /// updates `*stream` in place. Only simple integer variables (and
    /// call-by-name integer formals) are supported in this MVP.
    pub(in crate::mir::lower) fn lower_random_stream_addr(
        &mut self,
        argument: &Expr,
        proc_name: &str,
    ) -> Result<LocalId, CompileError> {
        match &argument.kind {
            ExprKind::Paren(inner) => self.lower_random_stream_addr(inner, proc_name),
            ExprKind::Variable(Variable::Simple(name)) => {
                if let Some(&(_, _, env)) = self.name_thunks.get(name) {
                    return Ok(env);
                }
                if let Some(&id) = self.scope.get(name).or_else(|| {
                    self.scope
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, id)| id)
                }) {
                    if self.local_ty(id) != MirType::I64 {
                        return Err(spanned_error(
                            format!(
                                "{proc_name} stream argument requires an integer variable, found {}",
                                self.local_ty(id)
                            ),
                            argument.span.clone(),
                        ));
                    }
                    let env = self.temp(MirType::RefI64);
                    self.push(
                        Op::LocalAddr {
                            dest: env,
                            local: id,
                        },
                        argument.span.clone(),
                    );
                    return Ok(env);
                }
                // Enclosing integer snapshotted on `__this` (e.g. `U2` used as
                // `normal(..., U2)` inside a Process body). Materialize a cell,
                // take its address for the runtime stream pointer, then write
                // the updated seed back to the capture field.
                if let Some((this_id, offset, field_ty, _)) = self.lookup_method_field(name) {
                    if field_ty != FieldType::I64 {
                        return Err(spanned_error(
                            format!("{proc_name} stream argument requires an integer variable"),
                            argument.span.clone(),
                        ));
                    }
                    let cell = self.temp(MirType::I64);
                    self.push(
                        Op::FieldLoadI64 {
                            dest: cell,
                            object: this_id,
                            offset,
                            class_qual: None,
                        },
                        argument.span.clone(),
                    );
                    let env = self.temp(MirType::RefI64);
                    self.push(
                        Op::LocalAddr {
                            dest: env,
                            local: cell,
                        },
                        argument.span.clone(),
                    );
                    self.pending_stream_field_writeback = Some((this_id, offset, cell));
                    return Ok(env);
                }
                Err(spanned_error(
                    format!("undeclared variable '{name}'"),
                    argument.span.clone(),
                ))
            }
            _ => Err(spanned_error(
                format!(
                    "{proc_name} stream argument must be a simple integer variable (call-by-name)"
                ),
                argument.span.clone(),
            )),
        }
    }

    /// Writes a temp stream cell back onto an enclosing capture field after
    /// [`Op::CallEnv`] updates `*stream`.
    pub(in crate::mir::lower) fn flush_stream_field_writeback(&mut self, span: Span) {
        if let Some((object, offset, cell)) = self.pending_stream_field_writeback.take() {
            self.push(
                Op::FieldStoreI64 {
                    object,
                    offset,
                    value: cell,
                    class_qual: None,
                },
                span,
            );
        }
    }

    /// Builds a fresh `(get, set, env)` thunk triple pointing at `cell` (an
    /// [`MirType::I64`] local): `env` is `&cell` and `get`/`set` are
    /// [`Op::FuncAddr`]s of the shared helper functions.
    pub(in crate::mir::lower) fn name_thunk_triple_for_cell(
        &mut self,
        cell: LocalId,
        span: Span,
    ) -> (LocalId, LocalId, LocalId) {
        let addr = self.temp(MirType::RefI64);
        self.push(
            Op::LocalAddr {
                dest: addr,
                local: cell,
            },
            span.clone(),
        );
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
            span.clone(),
        );
        self.name_thunk_triple_for_pointer(env, span)
    }

    /// Same triple, but the cell is already addressed — a by-reference capture
    /// slot holds exactly such a pointer, so the thunk must target the declaring
    /// frame's variable rather than the slot holding the pointer.
    pub(in crate::mir::lower) fn name_thunk_triple_for_pointer(
        &mut self,
        env: LocalId,
        span: Span,
    ) -> (LocalId, LocalId, LocalId) {
        let env = if self.local_ty(env) == MirType::RefI64 {
            let boxed = self.temp(MirType::ObjectRef);
            self.note_object_qual(boxed, NAME_INT_ENV_CLASS_NAME.to_string());
            self.push(
                Op::NewObject {
                    dest: boxed,
                    class_id: NAME_INT_ENV_CLASS_ID,
                    size: NAME_INT_ENV_SIZE,
                },
                span.clone(),
            );
            self.push(
                Op::FieldStoreI64 {
                    object: boxed,
                    offset: NAME_INT_ENV_ADDR_OFFSET,
                    value: env,
                    class_qual: Some(NAME_INT_ENV_CLASS_NAME.to_string()),
                },
                span.clone(),
            );
            boxed
        } else {
            env
        };
        let get = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: get,
                name: NAME_THUNK_GET_HELPER.to_string(),
            },
            span.clone(),
        );
        let set = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: set,
                name: NAME_THUNK_SET_HELPER.to_string(),
            },
            span.clone(),
        );
        (get, set, env)
    }

    /// Read-only call-by-name actual that is a bare identifier naming a
    /// parameterless integer type procedure. Returns `None` when `name` is not
    /// such a procedure, or when its shape is unsupported — the caller then
    /// falls back to snapshotting one call into a temp cell.
    pub(in crate::mir::lower) fn try_lower_type_proc_name_thunk_actual(
        &mut self,
        name: &str,
        argument: &Expr,
    ) -> Result<Option<(LocalId, LocalId, LocalId)>, CompileError> {
        let name = self
            .resolve_formal_procedure_name(name)
            .map(str::to_string)
            .unwrap_or_else(|| name.to_string());
        // Call-site-inlined type procedures (`sqri`, which closes over the
        // enclosing `i`) have no MIR function to call. A body of the form
        // `sqri := expr` is a pure abbreviation for `expr`, so build the
        // re-eval helper over `expr` in the caller's scope instead.
        if let Some(procedure) = self
            .lookup_ref_alias_proc(&name)
            .or_else(|| self.lookup_name_param_proc(&name))
        {
            let Some(result_expr) = type_procedure_simple_result_expr(procedure) else {
                return Ok(None);
            };
            return Ok(self.try_lower_expr_reeval_thunk(result_expr));
        }
        // Outlined parameterless integer procedure: route every read through a
        // per-call-site get helper that calls it with its free cells.
        let Some(resolved) = self.resolve_known_procedure(&name) else {
            return Ok(None);
        };
        // Mangled class methods (`Class$method`) need a receiver.
        if resolved.contains('$') {
            return Ok(None);
        }
        let Some(signature) = self.signatures.get(&resolved).cloned() else {
            return Ok(None);
        };
        if signature.result != Some(MirType::I64)
            || signature.external_stub
            || !signature.name_thunk_starts.is_empty()
            || !signature.formal_proc_param_indices.is_empty()
            || signature.params.len() != signature.free_cell_params.len()
        {
            return Ok(None);
        }
        let triple = self.name_thunk_triple_for_outlined_type_proc(
            &resolved,
            &signature,
            argument.span.clone(),
        )?;
        Ok(Some(triple))
    }

    /// Builds a `(get, set, env)` triple whose `get` calls the outlined
    /// parameterless integer procedure `target`; `env` packs the addresses of
    /// the free cells `target` expects as trailing parameters, and `set` is the
    /// shared readonly no-op.
    pub(in crate::mir::lower) fn name_thunk_triple_for_outlined_type_proc(
        &mut self,
        target: &str,
        signature: &ProcSignature,
        span: Span,
    ) -> Result<(LocalId, LocalId, LocalId), CompileError> {
        let helper_name = format!(
            "{NAME_THUNK_GET_EXPR_PREFIX}{}_{}",
            self.name.replace('$', "_"),
            self.expr_helper_counter
        );
        self.expr_helper_counter += 1;
        self.pending_helpers.push(build_name_thunk_get_call_helper(
            &helper_name,
            target,
            signature,
        ));

        let bytes = (signature.free_cell_params.len() as i64 * 8).max(8);
        let cells = self.temp(MirType::RefI64);
        self.push(Op::StackAlloc { dest: cells, bytes }, span.clone());
        for (index, cell) in signature.free_cell_params.iter().enumerate() {
            let addr = self.free_cell_addr(target, cell, span.clone())?;
            self.push(
                Op::StoreRefI64 {
                    ptr: cells,
                    src: addr,
                    offset: (index as i64) * 8,
                },
                span.clone(),
            );
        }
        // The formal's `env` slot is `ObjectRef` (a `ref_cell` for `dec(r.x)`),
        // so the pointer to this all-scalar address vector has to be boxed.
        let env = self.box_int_cell_env(cells, span.clone());

        let get = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: get,
                name: helper_name,
            },
            span.clone(),
        );
        let set = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: set,
                name: NAME_THUNK_SET_READONLY.to_string(),
            },
            span,
        );
        Ok((get, set, env))
    }

    /// The [`Self::free_cell_addrs`] entry for `name`, matched case-insensitively
    /// like the other formal lookups.
    pub(in crate::mir::lower) fn lookup_free_cell_addr(&self, name: &str) -> Option<&LocalId> {
        self.free_cell_addrs.get(name).or_else(|| {
            self.free_cell_addrs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, addr)| addr)
        })
    }

    /// Address passed for an outlined procedure's free cell `cell`: the cell
    /// address an enclosing free-cell parameter already carries, otherwise the
    /// address of the caller's local.
    pub(in crate::mir::lower) fn free_cell_addr(
        &mut self,
        proc_name: &str,
        cell: &str,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if let Some(&addr) = self.lookup_free_cell_addr(cell) {
            return Ok(addr);
        }
        let Some(&id) = self.scope.get(cell).or_else(|| {
            self.scope
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(cell))
                .map(|(_, id)| id)
        }) else {
            return Err(spanned_error(format!("undeclared variable '{cell}'"), span));
        };
        if !matches!(self.local_ty(id), MirType::I64 | MirType::Bool) {
            return Err(spanned_error(
                format!(
                    "outlined procedure '{proc_name}' free cell '{cell}' requires an integer or boolean variable"
                ),
                span,
            ));
        }
        let addr = self.temp(MirType::RefI64);
        self.push(
            Op::LocalAddr {
                dest: addr,
                local: id,
            },
            span,
        );
        Ok(addr)
    }

    /// Tries to build a true re-eval get thunk for a read-only integer
    /// expression actual. Returns `None` when the expression shape is not
    /// supported (caller falls back to a temp-cell snapshot).
    pub(in crate::mir::lower) fn try_lower_expr_reeval_thunk(
        &mut self,
        argument: &Expr,
    ) -> Option<(LocalId, LocalId, LocalId)> {
        let local_tys: Vec<MirType> = self.locals.iter().map(|local| local.ty).collect();
        let resolve_int_field = |object_name: &str, attribute: &str| -> Option<i64> {
            let &id = self.scope.get(object_name)?;
            if local_tys[id.0] != MirType::ObjectRef {
                return None;
            }
            let (offset, field_ty) = self
                .field_info_for(id, attribute, argument.span.clone())
                .ok()?;
            if field_ty != FieldType::I64 {
                return None;
            }
            Some(offset)
        };
        let (captures, field_offsets) = collect_expr_captures(
            argument,
            &self.scope,
            &self.name_thunks,
            |id| local_tys[id.0],
            resolve_int_field,
        )?;
        let helper_name = format!(
            "{NAME_THUNK_GET_EXPR_PREFIX}{}_{}",
            self.name.replace('$', "_"),
            self.expr_helper_counter
        );
        self.expr_helper_counter += 1;
        let helper =
            build_expr_reeval_get_helper(helper_name.clone(), argument, &captures, &field_offsets)?;
        self.pending_helpers.push(helper);
        Some(self.name_thunk_triple_for_expr(&helper_name, &captures, argument.span.clone()))
    }

    /// Packs `captures` into a [`NAME_PACK_ENV_CLASS_NAME`] object and returns
    /// a `(get, set, env)` triple whose `get` is `helper_name` and whose `set`
    /// is the shared readonly no-op.
    pub(in crate::mir::lower) fn name_thunk_triple_for_expr(
        &mut self,
        helper_name: &str,
        captures: &[(String, ExprCapture)],
        span: Span,
    ) -> (LocalId, LocalId, LocalId) {
        let env = self.temp(MirType::ObjectRef);
        self.note_object_qual(env, NAME_PACK_ENV_CLASS_NAME.to_string());
        self.push(
            Op::NewObject {
                dest: env,
                class_id: NAME_PACK_ENV_CLASS_ID,
                size: crate::layout::NAME_PACK_ENV_SIZE,
            },
            span.clone(),
        );

        for (index, (_, capture)) in captures.iter().enumerate() {
            let slot = match *capture {
                ExprCapture::Cell(cell) => {
                    let addr = self.temp(MirType::RefI64);
                    self.push(
                        Op::LocalAddr {
                            dest: addr,
                            local: cell,
                        },
                        span.clone(),
                    );
                    self.box_int_cell_env(addr, span.clone())
                }
                ExprCapture::Object(object) => object,
                ExprCapture::Thunk {
                    get,
                    env: thunk_env,
                } => {
                    let pair = self.temp(MirType::ObjectRef);
                    self.note_object_qual(pair, NAME_THUNK_PAIR_CLASS_NAME.to_string());
                    self.push(
                        Op::NewObject {
                            dest: pair,
                            class_id: NAME_THUNK_PAIR_CLASS_ID,
                            size: NAME_THUNK_PAIR_SIZE,
                        },
                        span.clone(),
                    );
                    let get_bits = self.temp(MirType::I64);
                    self.push(
                        Op::Copy {
                            dest: get_bits,
                            src: get,
                        },
                        span.clone(),
                    );
                    self.push(
                        Op::FieldStoreI64 {
                            object: pair,
                            offset: NAME_THUNK_PAIR_GET_OFFSET,
                            value: get_bits,
                            class_qual: Some(NAME_THUNK_PAIR_CLASS_NAME.to_string()),
                        },
                        span.clone(),
                    );
                    let boxed_env = if self.local_ty(thunk_env) == MirType::RefI64 {
                        self.box_int_cell_env(thunk_env, span.clone())
                    } else {
                        thunk_env
                    };
                    self.push(
                        Op::FieldStoreI64 {
                            object: pair,
                            offset: NAME_THUNK_PAIR_ENV_OFFSET,
                            value: boxed_env,
                            class_qual: Some(NAME_THUNK_PAIR_CLASS_NAME.to_string()),
                        },
                        span.clone(),
                    );
                    pair
                }
            };
            self.push(
                Op::FieldStoreI64 {
                    object: env,
                    offset: name_pack_env_slot_offset(index),
                    value: slot,
                    class_qual: Some(NAME_PACK_ENV_CLASS_NAME.to_string()),
                },
                span.clone(),
            );
        }

        let get = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: get,
                name: helper_name.to_string(),
            },
            span.clone(),
        );
        let set = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: set,
                name: NAME_THUNK_SET_READONLY.to_string(),
            },
            span,
        );
        (get, set, env)
    }

    /// Outlined call-by-name integer formal actual `a(i)` (a 1-D integer
    /// array element): resolves the index and builds the `(get, set, env)`
    /// triple through the shared `__simrt_name_get_arr1` /
    /// `__simrt_name_set_arr1` helpers (see
    /// [`Self::name_thunk_triple_for_arr1_elem`]) so every read/write
    /// re-evaluates `array[index]` rather than snapshotting it once.
    ///
    /// The index expression must be either a simple integer variable (its
    /// own cell is shared as `index_cell`, so a later mutation of that
    /// variable is *not* observed — matching the interpreter, which
    /// evaluates the subscript once per Jensen-style rebinding at the
    /// initial call) or an integer constant literal (spilled to a fresh
    /// cell). Anything more complex falls back to the whole-expression
    /// temp-cell path when the formal is read-only, or errors when the
    /// formal is assigned (no write-back target for a computed index).
    pub(in crate::mir::lower) fn lower_array_element_name_thunk_actual(
        &mut self,
        proc_name: &str,
        argument: &Expr,
        array: LocalId,
        formal_assigned: bool,
    ) -> Result<(LocalId, LocalId, LocalId), CompileError> {
        let ExprKind::FunctionCall {
            name: array_name,
            arguments,
        } = &argument.kind
        else {
            unreachable!("caller matched ExprKind::FunctionCall");
        };
        let span = argument.span.clone();
        if self.local_ty(array) == MirType::ArrayText {
            return Err(spanned_error(
                format!(
                    "outlined call-by-name procedure '{proc_name}' requires an integer name actual, found text array element '{array_name}(...)'"
                ),
                span,
            ));
        }
        if arguments.len() != 1 {
            return Err(spanned_error(
                format!(
                    "outlined call-by-name procedure '{proc_name}' does not support multi-dimensional array element name actuals ('{array_name}' has {} subscript(s), expected 1)",
                    arguments.len()
                ),
                span,
            ));
        }
        let index_expr = &arguments[0];

        // Simple integer variable index: share its cell directly.
        if let ExprKind::Variable(Variable::Simple(index_name)) = &index_expr.kind
            && let Some(&index_cell) = self.scope.get(index_name)
            && self.local_ty(index_cell) == MirType::I64
        {
            return Ok(self.name_thunk_triple_for_arr1_elem(array, index_cell, span));
        }

        // Integer constant literal index: spill to a fresh cell.
        if let ExprKind::NumberLiteral {
            kind: ArithmeticLiteralKind::Integer,
            ..
        } = &index_expr.kind
        {
            let value = self.lower_expr(index_expr)?;
            return Ok(self.name_thunk_triple_for_arr1_elem(array, value, span));
        }

        if !formal_assigned {
            // Read-only formal with a complex index: fall back to
            // evaluating the whole `array(index)` expression once into a
            // temp cell (still routed through the plain scalar get/set
            // helpers; the index itself is not re-evaluated on later reads).
            let value = self.lower_expr(argument)?;
            let value = self.coerce_value(
                MirType::I64,
                value,
                format!(
                    "argument type mismatch calling '{proc_name}' (expected integer expression for name parameter)"
                ),
                span.clone(),
            )?;
            let cell = self.temp(MirType::I64);
            self.push(
                Op::StoreLocal {
                    local: cell,
                    src: value,
                },
                span.clone(),
            );
            return Ok(self.name_thunk_triple_for_cell(cell, span));
        }

        Err(spanned_error(
            format!(
                "outlined call-by-name procedure '{proc_name}' requires a simple integer variable or integer constant as the array index in an assigned name actual (found a complex expression indexing '{array_name}')"
            ),
            index_expr.span.clone(),
        ))
    }

    /// Builds a fresh `(get, set, env)` thunk triple for the integer array
    /// element `array[*index_cell]`: `env` is a [`NAME_ARR1_ENV_CLASS_NAME`]
    /// object holding the array descriptor and `&index_cell`. Helpers re-read
    /// both on every call so a shared `env` always observes the current index
    /// and array contents.
    pub(in crate::mir::lower) fn name_thunk_triple_for_arr1_elem(
        &mut self,
        array: LocalId,
        index_cell: LocalId,
        span: Span,
    ) -> (LocalId, LocalId, LocalId) {
        let env = self.temp(MirType::ObjectRef);
        self.note_object_qual(env, NAME_ARR1_ENV_CLASS_NAME.to_string());
        self.push(
            Op::NewObject {
                dest: env,
                class_id: NAME_ARR1_ENV_CLASS_ID,
                size: NAME_ARR1_ENV_SIZE,
            },
            span.clone(),
        );
        self.push(
            Op::FieldStoreI64 {
                object: env,
                offset: NAME_ARR1_ENV_ARRAY_OFFSET,
                value: array,
                class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
            },
            span.clone(),
        );

        let index_addr = self.temp(MirType::RefI64);
        self.push(
            Op::LocalAddr {
                dest: index_addr,
                local: index_cell,
            },
            span.clone(),
        );
        self.push(
            Op::FieldStoreI64 {
                object: env,
                offset: NAME_ARR1_ENV_INDEX_OFFSET,
                value: index_addr,
                class_qual: Some(NAME_ARR1_ENV_CLASS_NAME.to_string()),
            },
            span.clone(),
        );

        let get = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: get,
                name: NAME_THUNK_GET_ARR1.to_string(),
            },
            span.clone(),
        );
        let set = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: set,
                name: NAME_THUNK_SET_ARR1.to_string(),
            },
            span,
        );
        (get, set, env)
    }

    /// Ensures per-offset field get/set helpers exist among
    /// [`Self::pending_helpers`] (deduped later by [`dedupe_functions_by_name`]).
    pub(in crate::mir::lower) fn ensure_field_name_thunk_helpers(&mut self, offset: i64) {
        let get_name = name_thunk_get_field_name(offset);
        if self
            .pending_helpers
            .iter()
            .any(|function| function.name == get_name)
        {
            return;
        }
        self.pending_helpers
            .push(build_name_thunk_get_field_helper(offset));
        self.pending_helpers
            .push(build_name_thunk_set_field_helper(offset));
    }

    /// Builds a fresh `(get, set, env)` thunk triple for the integer object
    /// field at `object[offset]`: `env` is [`Op::LocalAddr`] of the object
    /// reference local (so each get/set reloads the current pointer — Jensen
    /// re-eval of `r` in `r.x`); `get`/`set` are [`Op::FuncAddr`]s of the
    /// per-offset helpers (see [`build_name_thunk_get_field_helper`] /
    /// [`build_name_thunk_set_field_helper`]).
    pub(in crate::mir::lower) fn name_thunk_triple_for_field(
        &mut self,
        object: LocalId,
        offset: i64,
        span: Span,
    ) -> (LocalId, LocalId, LocalId) {
        self.ensure_field_name_thunk_helpers(offset);

        let env = self.temp(MirType::ObjectRef);
        self.note_object_qual(env, REF_CELL_CLASS_NAME.to_string());
        self.push(
            Op::LocalAddr {
                dest: env,
                local: object,
            },
            span.clone(),
        );

        let get = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: get,
                name: name_thunk_get_field_name(offset),
            },
            span.clone(),
        );
        let set = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: set,
                name: name_thunk_set_field_name(offset),
            },
            span,
        );
        (get, set, env)
    }

    /// Bind a call-by-value formal: scalars and text/ref handles copy by value;
    /// arrays deep-copy their descriptors (§4.6.2). Text `:=` still deep-copies.
    /// §4.6.3: a call-by-reference text formal is "a local copy of the
    /// reference". A `text` local is a pointer to the frame that *is* the text
    /// variable, so passing the actual's pointer would make the formal the
    /// caller's variable and let `:-` inside the callee rebind it. Give the
    /// callee its own frame denoting the same text object instead: the
    /// characters stay shared (so `:=` and `upcase` are visible to the caller)
    /// while `:-` and `setpos` stay local to the callee.
    pub(in crate::mir::lower) fn bind_text_reference_actual(
        &mut self,
        value: LocalId,
        span: Span,
    ) -> LocalId {
        let bound = self.temp(MirType::Text);
        self.push(Op::TextNotext { dest: bound }, span.clone());
        self.push(
            Op::TextRefAssign {
                dest: bound,
                src: value,
            },
            span,
        );
        bound
    }

    pub(in crate::mir::lower) fn store_value_param(
        &mut self,
        dest: LocalId,
        src: LocalId,
        ty: MirType,
        span: Span,
    ) {
        match ty {
            // §4.6.2: `FP :- copy(AP)` — a value text formal owns its frame.
            MirType::Text => {
                self.push(Op::TextCopy { dest, src }, span);
            }
            MirType::ObjectRef => {
                self.push(Op::Copy { dest, src }, span);
            }
            MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText => {
                self.push(Op::ArrayCopy { dest, src }, span);
            }
            _ => {
                self.push(Op::Copy { dest, src }, span);
            }
        }
    }
}
