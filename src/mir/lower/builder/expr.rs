//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn lower_bool_expr(
        &mut self,
        expr: &Expr,
    ) -> Result<LocalId, CompileError> {
        let value = self.lower_expr(expr)?;
        if self.local_ty(value) != MirType::Bool {
            return Err(spanned_error(
                "condition must be boolean",
                expr.span.clone(),
            ));
        }
        Ok(value)
    }

    pub(in crate::mir::lower) fn lower_expr(
        &mut self,
        expr: &Expr,
    ) -> Result<LocalId, CompileError> {
        let span = expr.span.clone();
        match &expr.kind {
            ExprKind::BooleanLiteral(value) => {
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::ConstBool {
                        dest,
                        value: *value,
                    },
                    span,
                );
                Ok(dest)
            }
            ExprKind::NumberLiteral { lexeme, kind } => {
                self.lower_number_literal(lexeme, *kind, span)
            }
            ExprKind::Variable(variable) => {
                if let Variable::Simple(name) = variable {
                    if let Some(result) =
                        self.try_lower_connected_basicio_identifier(name, span.clone())?
                    {
                        return Ok(result);
                    }
                    if name.eq_ignore_ascii_case("InLine") {
                        let dest = self.temp(MirType::Text);
                        self.push(Op::CallInLine { dest }, span.clone());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("InChar") {
                        let dest = self.temp(MirType::I64);
                        self.push(Op::CallInChar { dest }, span.clone());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("InInt") {
                        // Free InInt (§10.7) — MVP stub (same as file.inint).
                        let dest = self.temp(MirType::I64);
                        self.push(Op::ConstI64 { dest, value: 0 }, span.clone());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("Endfile") {
                        let dest = self.temp(MirType::Bool);
                        self.push(Op::CallEndfile { dest }, span.clone());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("sysin") {
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::CallSysIn { dest }, span.clone());
                        self.note_object_qual(dest, "InFile".into());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("sysout") {
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::CallSysOut { dest }, span.clone());
                        self.note_object_qual(dest, "PrintFile".into());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("time") {
                        if !self.simulation_context {
                            return Err(scheduling_unsupported_error("time", span));
                        }
                        let dest = self.temp(MirType::F64);
                        self.push(Op::SimTime { dest }, span);
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("current") {
                        if !self.simulation_context {
                            return Err(scheduling_unsupported_error("current", span));
                        }
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::SimCurrent { dest }, span.clone());
                        self.note_object_qual(dest, "Process".into());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("main") {
                        if !self.simulation_context {
                            return Err(spanned_error(
                                "MIR lowering: 'main' is only available inside a Simulation block",
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::SimMain { dest }, span.clone());
                        self.note_object_qual(dest, "Process".into());
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("nextev") {
                        if !self.simulation_context {
                            return Err(scheduling_unsupported_error("nextev", span));
                        }
                        let current = self.temp(MirType::ObjectRef);
                        self.push(Op::SimCurrent { dest: current }, span.clone());
                        self.note_object_qual(current, "Process".into());
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(
                            Op::SimNextev {
                                dest,
                                process: current,
                            },
                            span.clone(),
                        );
                        self.note_object_qual(dest, "Process".into());
                        return Ok(dest);
                    }
                    if let Some(value) = crate::runtime::environment::environment_constant_i64(name)
                    {
                        let dest = self.temp(MirType::I64);
                        self.push(Op::ConstI64 { dest, value }, span);
                        return Ok(dest);
                    }
                    if let Some(value) = crate::runtime::environment::environment_constant_f64(name)
                    {
                        let dest = self.temp(MirType::F64);
                        self.push(Op::ConstF64 { dest, value }, span);
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("simulaid") {
                        let dest = self.temp(MirType::Text);
                        let string_id =
                            self.intern_string(&crate::runtime::environment::simulaid_string());
                        self.push(Op::TextFromLiteral { dest, string_id }, span);
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("datetime") {
                        let dest = self.temp(MirType::Text);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "datetime".into(),
                                args: vec![],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("cputime")
                        || name.eq_ignore_ascii_case("clocktime")
                    {
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: name.to_ascii_lowercase(),
                                args: vec![],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("sourceline") {
                        let dest = self.temp(MirType::I64);
                        let line = if self.source_text.is_empty() {
                            1
                        } else {
                            crate::source::span_to_line_col(&self.source_text, span.start).0 as i64
                        };
                        self.push(Op::ConstI64 { dest, value: line }, span);
                        return Ok(dest);
                    }
                    if name.eq_ignore_ascii_case("CURRENTLOWTEN")
                        || name.eq_ignore_ascii_case("CURRENTDECIMALMARK")
                    {
                        let dest = self.temp(MirType::I64);
                        let env_name = if name.eq_ignore_ascii_case("CURRENTLOWTEN") {
                            "current_lowten"
                        } else {
                            "current_decimalmark"
                        };
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: env_name.into(),
                                args: vec![],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    if !self.scope_has_name(name)
                        && (self.name_bindings.contains_key(name)
                            || self
                                .name_bindings
                                .keys()
                                .any(|key| key.eq_ignore_ascii_case(name)))
                    {
                        return self.lower_name_actual(name);
                    }
                    if let Some(result) = self.try_lower_free_basicio(name, &[], span.clone())? {
                        return Ok(result);
                    }
                }
                if let Variable::Remote { object, attribute } = variable {
                    if let Variable::Simple(name) = object.as_ref() {
                        if let Some(&frame) = self.scope.get(name)
                            && self.local_ty(frame) == MirType::Text
                        {
                            return self.lower_text_attribute(frame, attribute, span);
                        }
                        if name.eq_ignore_ascii_case("simulaid") {
                            let frame = self.temp(MirType::Text);
                            let string_id =
                                self.intern_string(&crate::runtime::environment::simulaid_string());
                            self.push(
                                Op::TextFromLiteral {
                                    dest: frame,
                                    string_id,
                                },
                                span.clone(),
                            );
                            return self.lower_text_attribute(frame, attribute, span);
                        }
                    }
                    // Nested remote chain (`t.main.pos`, `sysout.image.strip`,
                    // `f.getobj.attr`, …): lower the inner remote/method
                    // access as an ordinary expression first, then dispatch
                    // the outer attribute against its resulting text/object
                    // value, same as the un-nested `Variable::Simple` cases
                    // above.
                    if matches!(
                        object.as_ref(),
                        Variable::Remote { .. } | Variable::RemoteCall { .. }
                    ) {
                        let inner = Expr {
                            kind: ExprKind::Variable((**object).clone()),
                            span: span.clone(),
                        };
                        let object_id = self.lower_expr(&inner)?;
                        match self.local_ty(object_id) {
                            MirType::Text => {
                                return self.lower_text_attribute(object_id, attribute, span);
                            }
                            MirType::ObjectRef => {
                                if is_simset_method(attribute) {
                                    return self.lower_simset_method(
                                        object_id,
                                        attribute,
                                        &[],
                                        span,
                                    );
                                }
                                if is_basicio_method(attribute) {
                                    return self.lower_basicio_method(
                                        object_id,
                                        attribute,
                                        &[],
                                        span,
                                    );
                                }
                                if let Some(result) = self.try_lower_parameterless_method(
                                    object_id,
                                    attribute,
                                    span.clone(),
                                )? {
                                    return Ok(result);
                                }
                                let (offset, field_ty) =
                                    self.field_info_for(object_id, attribute, span.clone())?;
                                let object_qual = self
                                    .ref_qual
                                    .get(&object_id)
                                    .and_then(|class| self.attribute_object_qual(class, attribute));
                                let place = remote_place(object_id, offset, field_ty, object_qual);
                                return Ok(self.read_place(&place, span));
                            }
                            _ => {}
                        }
                    }
                    // Parameterless type procedure as remote object:
                    // `Mother.pname` when `Mother` is `ref(Person) procedure Mother`.
                    if let Variable::Simple(name) = object.as_ref()
                        && let Some(object_id) =
                            self.try_lower_parameterless_procedure(name, span.clone())?
                        && self.local_ty(object_id) == MirType::ObjectRef
                    {
                        if is_simset_method(attribute) {
                            return self.lower_simset_method(object_id, attribute, &[], span);
                        }
                        if is_basicio_method(attribute) {
                            return self.lower_basicio_method(object_id, attribute, &[], span);
                        }
                        if let Some(result) =
                            self.try_lower_parameterless_method(object_id, attribute, span.clone())?
                        {
                            return Ok(result);
                        }
                        let (offset, field_ty) =
                            self.field_info_for(object_id, attribute, span.clone())?;
                        let object_qual = self
                            .ref_qual
                            .get(&object_id)
                            .and_then(|class| self.attribute_object_qual(class, attribute));
                        let place = remote_place(object_id, offset, field_ty, object_qual);
                        return Ok(self.read_place(&place, span));
                    }
                    // Expression-form `v := r.m` parses as `Variable::Remote`
                    // (unlike statement `r.m;` → `RemoteAccess`). Desugar
                    // parameterless methods the same way. Also `IMAGE.LENGTH`
                    // when `IMAGE` is a BASICIO image place (text), not a
                    // field of an object local.
                    let object_place = self.resolve_place(object, span.clone())?;
                    if self.place_ty(&object_place) == MirType::Text {
                        let frame = self.read_place(&object_place, span.clone());
                        return self.lower_text_attribute(frame, attribute, span);
                    }
                    if self.place_ty(&object_place) == MirType::ObjectRef {
                        let object_id = match object_place {
                            Place::Local(id) => id,
                            other => self.read_place(&other, span.clone()),
                        };
                        if is_simset_method(attribute) {
                            return self.lower_simset_method(object_id, attribute, &[], span);
                        }
                        if is_basicio_method(attribute) {
                            return self.lower_basicio_method(object_id, attribute, &[], span);
                        }
                        if let Some(result) = self.try_lower_process_builtin_attribute(
                            object_id,
                            attribute,
                            span.clone(),
                        )? {
                            return Ok(result);
                        }
                        if let Some(result) =
                            self.try_lower_parameterless_method(object_id, attribute, span.clone())?
                        {
                            return Ok(result);
                        }
                        // Finish the field access against the object we already
                        // evaluated. Falling through to `resolve_place(variable)`
                        // would re-evaluate the object expression, running a
                        // name-bound type procedure (`y` bound to `x.Z`) twice
                        // for a single read of `y.attr` (simtst72).
                        let place =
                            self.remote_object_field_place(object_id, attribute, span.clone())?;
                        return Ok(self.read_place(&place, span));
                    }
                }
                // Bare parameterless procedure used as an expression (`if expcom then`)
                // — Simula allows omitting `()` for 0-argument procedures.
                if let Variable::Simple(name) = variable
                    && let Some(result) =
                        self.try_lower_parameterless_procedure(name, span.clone())?
                {
                    return Ok(result);
                }
                if let Variable::Simple(name) = variable
                    && let Some(result) = self.try_lower_free_basicio(name, &[], span.clone())?
                {
                    return Ok(result);
                }
                let place = self.resolve_place(variable, span.clone())?;
                Ok(self.read_place(&place, span))
            }
            ExprKind::Unary { op, operand } => self.lower_unary(*op, operand, span),
            ExprKind::Binary { op, left, right } => self.lower_binary(*op, left, right, span),
            ExprKind::Relation { op, left, right } => self.lower_relation(*op, left, right, span),
            ExprKind::If {
                condition,
                then_expr,
                else_expr,
            } => self.lower_expr_if(condition, then_expr, else_expr, span),
            ExprKind::Paren(inner) => self.lower_expr(inner),
            ExprKind::FunctionCall { name, arguments } => {
                if is_deferred_scheduling_name(name) {
                    return Err(scheduling_unsupported_error(name, span));
                }
                let lowered_name = name.to_ascii_lowercase();
                if matches!(
                    lowered_name.as_str(),
                    "detach" | "call" | "resume" | "hold" | "passivate"
                ) {
                    return Err(scheduling_unsupported_error(name, span));
                }
                if lowered_name == "time" {
                    if !self.simulation_context {
                        return Err(scheduling_unsupported_error("time", span));
                    }
                    if !arguments.is_empty() {
                        return Err(spanned_error(
                            format!("time expects 0 arguments, found {}", arguments.len()),
                            span,
                        ));
                    }
                    let dest = self.temp(MirType::F64);
                    self.push(Op::SimTime { dest }, span);
                    return Ok(dest);
                }
                if lowered_name == "current" {
                    if !self.simulation_context {
                        return Err(scheduling_unsupported_error("current", span));
                    }
                    if !arguments.is_empty() {
                        return Err(spanned_error(
                            format!("current expects 0 arguments, found {}", arguments.len()),
                            span,
                        ));
                    }
                    let dest = self.temp(MirType::ObjectRef);
                    self.push(Op::SimCurrent { dest }, span.clone());
                    self.note_object_qual(dest, "Process".into());
                    return Ok(dest);
                }
                // Built-in text frame procedures (§8.3) before user procedures /
                // array-index ambiguity resolution.
                match lowered_name.as_str() {
                    "fileexists" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("fileExists expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let path = lower_filesystem_text_arg(self, &arguments[0], "fileExists")?;
                        let dest = self.temp(MirType::Bool);
                        self.push(Op::CallFileExists { dest, path }, span);
                        return Ok(dest);
                    }
                    "fileread" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("fileRead expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let path = lower_filesystem_text_arg(self, &arguments[0], "fileRead")?;
                        let dest = self.temp(MirType::Text);
                        self.push(Op::CallFileRead { dest, path }, span);
                        return Ok(dest);
                    }
                    "filewrite" => {
                        return Err(spanned_error(
                            "fileWrite is a statement and cannot be used as an expression",
                            span,
                        ));
                    }
                    "inline" => {
                        if !arguments.is_empty() {
                            return Err(spanned_error(
                                format!("InLine expects 0 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::Text);
                        self.push(Op::CallInLine { dest }, span);
                        return Ok(dest);
                    }
                    "inchar" => {
                        if !arguments.is_empty() {
                            return Err(spanned_error(
                                format!("InChar expects 0 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::I64);
                        self.push(Op::CallInChar { dest }, span);
                        return Ok(dest);
                    }
                    "endfile" => {
                        if !arguments.is_empty() {
                            return Err(spanned_error(
                                format!("Endfile expects 0 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::Bool);
                        self.push(Op::CallEndfile { dest }, span);
                        return Ok(dest);
                    }
                    "sysin" => {
                        if !arguments.is_empty() {
                            return Err(spanned_error(
                                format!("sysin expects 0 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::CallSysIn { dest }, span.clone());
                        self.note_object_qual(dest, "InFile".into());
                        return Ok(dest);
                    }
                    "sysout" => {
                        if !arguments.is_empty() {
                            return Err(spanned_error(
                                format!("sysout expects 0 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::ObjectRef);
                        self.push(Op::CallSysOut { dest }, span.clone());
                        self.note_object_qual(dest, "PrintFile".into());
                        return Ok(dest);
                    }
                    "inimage" | "outchar" | "breakoutimage" | "outtext" | "outimage" | "outint" => {
                        return Err(spanned_error(
                            format!("{name} is a statement and cannot be used as an expression"),
                            span,
                        ));
                    }
                    "entier" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("entier expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let src = self.lower_expr(&arguments[0])?;
                        let src = match self.local_ty(src) {
                            MirType::F64 | MirType::LongF64 => src,
                            MirType::I64 => {
                                let dest = self.temp(MirType::F64);
                                self.push(Op::I64ToF64 { dest, src }, span.clone());
                                dest
                            }
                            other => {
                                return Err(spanned_error(
                                    format!("entier requires a numeric argument, found {other}"),
                                    arguments[0].span.clone(),
                                ));
                            }
                        };
                        let dest = self.temp(MirType::I64);
                        self.push(Op::F64ToI64 { dest, src }, span);
                        return Ok(dest);
                    }
                    "decimalmark" | "lowten" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 1 argument, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let value = self.lower_expr(&arguments[0])?;
                        if self.local_ty(value) != MirType::I64 {
                            return Err(spanned_error(
                                format!("{lowered_name} requires a character argument"),
                                arguments[0].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![value],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "sqrt" | "sin" | "cos" | "tan" | "cotan" | "arcsin" | "arccos" | "ln"
                    | "exp" | "arctan" | "addepsilon" | "subepsilon" | "sinh" | "cosh" | "tanh"
                    | "log10" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 1 argument, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let src = self.lower_expr(&arguments[0])?;
                        let src = match self.local_ty(src) {
                            MirType::F64 | MirType::LongF64 => src,
                            MirType::I64 => {
                                let dest = self.temp(MirType::F64);
                                self.push(Op::I64ToF64 { dest, src }, span.clone());
                                dest
                            }
                            other => {
                                return Err(spanned_error(
                                    format!(
                                        "{lowered_name} requires a numeric argument, found {other}"
                                    ),
                                    arguments[0].span.clone(),
                                ));
                            }
                        };
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![src],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "arctan2" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!("arctan2 expects 2 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let y = self.lower_expr_as_f64(&arguments[0], "arctan2")?;
                        let x = self.lower_expr_as_f64(&arguments[1], "arctan2")?;
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "arctan2".into(),
                                args: vec![y, x],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "mod" | "rem" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 2 arguments, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let left = self.lower_expr(&arguments[0])?;
                        let right = self.lower_expr(&arguments[1])?;
                        if self.local_ty(left) != MirType::I64
                            || self.local_ty(right) != MirType::I64
                        {
                            return Err(spanned_error(
                                format!("{lowered_name} requires integer arguments"),
                                span,
                            ));
                        }
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![left, right],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "sign" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("sign expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let src = self.lower_expr(&arguments[0])?;
                        let src = match self.local_ty(src) {
                            MirType::F64 | MirType::LongF64 => src,
                            MirType::I64 => {
                                let dest = self.temp(MirType::F64);
                                self.push(Op::I64ToF64 { dest, src }, span.clone());
                                dest
                            }
                            other => {
                                return Err(spanned_error(
                                    format!("sign requires a numeric argument, found {other}"),
                                    arguments[0].span.clone(),
                                ));
                            }
                        };
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "sign".into(),
                                args: vec![src],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "abs" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("abs expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let src = self.lower_expr(&arguments[0])?;
                        match self.local_ty(src) {
                            MirType::I64 => {
                                let dest = self.temp(MirType::I64);
                                self.push(
                                    Op::CallEnv {
                                        dest,
                                        name: "abs_int".into(),
                                        args: vec![src],
                                    },
                                    span,
                                );
                                return Ok(dest);
                            }
                            MirType::F64 | MirType::LongF64 => {
                                let dest = self.temp(MirType::F64);
                                self.push(
                                    Op::CallEnv {
                                        dest,
                                        name: "abs_real".into(),
                                        args: vec![src],
                                    },
                                    span,
                                );
                                return Ok(dest);
                            }
                            other => {
                                return Err(spanned_error(
                                    format!("abs requires a numeric argument, found {other}"),
                                    arguments[0].span.clone(),
                                ));
                            }
                        }
                    }
                    "draw" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!("draw expects 2 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr_as_f64(&arguments[0], "draw")?;
                        let stream = self.lower_random_stream_addr(&arguments[1], "draw")?;
                        let dest = self.temp(MirType::Bool);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "draw".into(),
                                args: vec![a, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "randint" => {
                        if arguments.len() != 3 {
                            return Err(spanned_error(
                                format!("randint expects 3 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr(&arguments[0])?;
                        let b = self.lower_expr(&arguments[1])?;
                        if self.local_ty(a) != MirType::I64 || self.local_ty(b) != MirType::I64 {
                            return Err(spanned_error("randint requires integer bounds", span));
                        }
                        let stream = self.lower_random_stream_addr(&arguments[2], "randint")?;
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "randint".into(),
                                args: vec![a, b, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "uniform" => {
                        if arguments.len() != 3 {
                            return Err(spanned_error(
                                format!("uniform expects 3 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr_as_f64(&arguments[0], "uniform")?;
                        let b = self.lower_expr_as_f64(&arguments[1], "uniform")?;
                        let stream = self.lower_random_stream_addr(&arguments[2], "uniform")?;
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "uniform".into(),
                                args: vec![a, b, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "normal" => {
                        if arguments.len() != 3 {
                            return Err(spanned_error(
                                format!("normal expects 3 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr_as_f64(&arguments[0], "normal")?;
                        let b = self.lower_expr_as_f64(&arguments[1], "normal")?;
                        let stream = self.lower_random_stream_addr(&arguments[2], "normal")?;
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "normal".into(),
                                args: vec![a, b, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "negexp" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!("negexp expects 2 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr_as_f64(&arguments[0], "negexp")?;
                        let stream = self.lower_random_stream_addr(&arguments[1], "negexp")?;
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "negexp".into(),
                                args: vec![a, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "poisson" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!("poisson expects 2 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr_as_f64(&arguments[0], "poisson")?;
                        let stream = self.lower_random_stream_addr(&arguments[1], "poisson")?;
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "poisson".into(),
                                args: vec![a, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "erlang" => {
                        if arguments.len() != 3 {
                            return Err(spanned_error(
                                format!("erlang expects 3 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr_as_f64(&arguments[0], "erlang")?;
                        let b = self.lower_expr_as_f64(&arguments[1], "erlang")?;
                        let stream = self.lower_random_stream_addr(&arguments[2], "erlang")?;
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "erlang".into(),
                                args: vec![a, b, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "discrete" | "histd" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 2 arguments, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let array = self.lower_expr(&arguments[0])?;
                        if self.local_ty(array) != MirType::ArrayF64 {
                            return Err(spanned_error(
                                format!("{lowered_name} requires a real array argument"),
                                arguments[0].span.clone(),
                            ));
                        }
                        let stream = self.lower_random_stream_addr(&arguments[1], &lowered_name)?;
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![array, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "linear" => {
                        if arguments.len() != 3 {
                            return Err(spanned_error(
                                format!("linear expects 3 arguments, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let a = self.lower_expr(&arguments[0])?;
                        let b = self.lower_expr(&arguments[1])?;
                        if self.local_ty(a) != MirType::ArrayF64
                            || self.local_ty(b) != MirType::ArrayF64
                        {
                            return Err(spanned_error(
                                "linear requires two real array arguments",
                                arguments[0].span.clone(),
                            ));
                        }
                        let stream = self.lower_random_stream_addr(&arguments[2], "linear")?;
                        let dest = self.temp(MirType::F64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: "linear".into(),
                                args: vec![a, b, stream],
                            },
                            span.clone(),
                        );
                        self.flush_stream_field_writeback(span);
                        return Ok(dest);
                    }
                    "lowerbound" | "upperbound" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 2 arguments, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let array = self.lower_expr(&arguments[0])?;
                        if !matches!(
                            self.local_ty(array),
                            MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
                        ) {
                            return Err(spanned_error(
                                format!("{lowered_name} requires an array argument"),
                                arguments[0].span.clone(),
                            ));
                        }
                        let dim = self.lower_expr(&arguments[1])?;
                        if self.local_ty(dim) != MirType::I64 {
                            return Err(spanned_error(
                                format!("{lowered_name} requires an integer dimension"),
                                arguments[1].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![array, dim],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "digit" | "letter" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 1 argument, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let value = self.lower_expr(&arguments[0])?;
                        if self.local_ty(value) != MirType::I64 {
                            return Err(spanned_error(
                                format!("{lowered_name} requires a character argument"),
                                arguments[0].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::Bool);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![value],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "char" | "isochar" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 1 argument, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let value = self.lower_expr(&arguments[0])?;
                        if self.local_ty(value) != MirType::I64 {
                            return Err(spanned_error(
                                format!("{lowered_name} requires an integer argument"),
                                arguments[0].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![value],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "rank" | "isorank" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 1 argument, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let value = self.lower_expr(&arguments[0])?;
                        if self.local_ty(value) != MirType::I64 {
                            return Err(spanned_error(
                                format!("{lowered_name} requires a character argument"),
                                arguments[0].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::I64);
                        self.push(
                            Op::CallEnv {
                                dest,
                                name: lowered_name.clone(),
                                args: vec![value],
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    "max" | "min" => {
                        if arguments.len() != 2 {
                            return Err(spanned_error(
                                format!(
                                    "{lowered_name} expects 2 arguments, found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let left = self.lower_expr(&arguments[0])?;
                        let right = self.lower_expr(&arguments[1])?;
                        match (self.local_ty(left), self.local_ty(right)) {
                            (MirType::I64, MirType::I64) => {
                                let dest = self.temp(MirType::I64);
                                let name = if lowered_name == "max" {
                                    "max_int"
                                } else {
                                    "min_int"
                                };
                                self.push(
                                    Op::CallEnv {
                                        dest,
                                        name: name.into(),
                                        args: vec![left, right],
                                    },
                                    span,
                                );
                                return Ok(dest);
                            }
                            (MirType::F64 | MirType::LongF64, MirType::F64 | MirType::LongF64) => {
                                let result_ty = if self.local_ty(left) == MirType::LongF64
                                    || self.local_ty(right) == MirType::LongF64
                                {
                                    MirType::LongF64
                                } else {
                                    MirType::F64
                                };
                                let dest = self.temp(result_ty);
                                let name = if lowered_name == "max" {
                                    "max_real"
                                } else {
                                    "min_real"
                                };
                                self.push(
                                    Op::CallEnv {
                                        dest,
                                        name: name.into(),
                                        args: vec![left, right],
                                    },
                                    span,
                                );
                                return Ok(dest);
                            }
                            (MirType::Text, MirType::Text) => {
                                let dest = self.temp(MirType::Text);
                                let name = if lowered_name == "max" {
                                    "max_text"
                                } else {
                                    "min_text"
                                };
                                self.push(
                                    Op::CallEnv {
                                        dest,
                                        name: name.into(),
                                        args: vec![left, right],
                                    },
                                    span,
                                );
                                return Ok(dest);
                            }
                            _ => {
                                return Err(spanned_error(
                                    format!(
                                        "{lowered_name} requires integer, real, or text arguments of the same kind"
                                    ),
                                    span,
                                ));
                            }
                        }
                    }
                    "blanks" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("blanks expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let n = self.lower_expr(&arguments[0])?;
                        if self.local_ty(n) != MirType::I64 {
                            return Err(spanned_error(
                                "blanks requires an integer argument",
                                arguments[0].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::Text);
                        self.push(Op::TextBlanks { dest, n }, span);
                        return Ok(dest);
                    }
                    "copy" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!("copy expects 1 argument, found {}", arguments.len()),
                                span,
                            ));
                        }
                        let src = self.lower_expr(&arguments[0])?;
                        if self.local_ty(src) != MirType::Text {
                            return Err(spanned_error(
                                "copy requires a text argument",
                                arguments[0].span.clone(),
                            ));
                        }
                        let dest = self.temp(MirType::Text);
                        self.push(Op::TextCopy { dest, src }, span);
                        return Ok(dest);
                    }
                    "upcase" | "lowcase" => {
                        if arguments.len() != 1 {
                            return Err(spanned_error(
                                format!(
                                    "{} expects 1 argument, found {}",
                                    name.to_ascii_lowercase(),
                                    arguments.len()
                                ),
                                span,
                            ));
                        }
                        let frame = self.lower_expr(&arguments[0])?;
                        if self.local_ty(frame) != MirType::Text {
                            return Err(spanned_error(
                                format!("{} requires a text argument", name.to_ascii_lowercase()),
                                arguments[0].span.clone(),
                            ));
                        }
                        if name.eq_ignore_ascii_case("upcase") {
                            self.push(Op::TextUpcase { frame }, span.clone());
                        } else {
                            self.push(Op::TextLowcase { frame }, span.clone());
                        }
                        return Ok(frame);
                    }
                    _ => {}
                }
                // `name(args)` is ambiguous at parse time between a procedure
                // call and an array element read (Standard §3.1/§5.2); the
                // parser always produces `FunctionCall` here (`Variable::
                // Subscripted` is only synthesized for assignment LHSes, see
                // `parse::variable`). Locals and call-by-name formals shadow
                // procedures (Simula is case-insensitive: formal `r` must not
                // resolve as sibling procedure `R`).
                if let Some(FormalProcTarget::Method { object, method }) =
                    self.resolve_formal_proc_target(name).cloned()
                {
                    return self.lower_object_method_call(object, &method, arguments, span);
                }
                let name = self
                    .resolve_formal_procedure_name(name)
                    .map(str::to_string)
                    .unwrap_or_else(|| name.clone());
                if self.name_is_subscriptable(&name) {
                    let variable = Variable::Subscripted {
                        name: name.clone(),
                        subscripts: arguments.clone(),
                    };
                    let place = self.resolve_place(&variable, span.clone())?;
                    Ok(self.read_place(&place, span))
                } else if let Some(procedure) = self.lookup_name_param_proc(&name) {
                    let Some(result) =
                        self.inline_name_procedure(procedure, arguments, span.clone(), true)?
                    else {
                        return Err(spanned_error(
                            format!(
                                "MIR lowering: procedure '{name}' does not return a value and cannot be used in an expression"
                            ),
                            span,
                        ));
                    };
                    Ok(result)
                } else if let Some(procedure) = self.lookup_ref_alias_proc(&name) {
                    let Some(result) =
                        self.inline_ref_alias_procedure(procedure, arguments, span.clone(), true)?
                    else {
                        return Err(spanned_error(
                            format!(
                                "MIR lowering: procedure '{name}' does not return a value and cannot be used in an expression"
                            ),
                            span,
                        ));
                    };
                    Ok(result)
                } else if let Some((resolved_name, signature)) = self
                    .signatures
                    .get(&name)
                    .map(|sig| (name.clone(), sig.clone()))
                    .or_else(|| {
                        self.signatures
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                            .map(|(key, sig)| (key.clone(), sig.clone()))
                    })
                {
                    let Some(result_ty) = signature.result else {
                        return Err(spanned_error(
                            format!(
                                "MIR lowering: procedure '{name}' does not return a value and cannot be used in an expression"
                            ),
                            span,
                        ));
                    };
                    let args = self.lower_call_arguments(
                        &resolved_name,
                        &signature,
                        arguments,
                        span.clone(),
                    )?;
                    let dest = self.temp(result_ty);
                    self.push(
                        Op::Call {
                            dest: Some(dest),
                            name: resolved_name,
                            args,
                        },
                        span,
                    );
                    self.annotate_call_result(dest, &signature);
                    Ok(dest)
                } else if let Some(this_id) = self.method_this {
                    if self.object_method_name(this_id, &name).is_some() {
                        let mangled = self
                            .object_method_name(this_id, &name)
                            .expect("method exists");
                        let signature = self.signatures.get(&mangled).cloned().ok_or_else(|| {
                            spanned_error(
                                format!(
                                    "MIR lowering: internal error: missing signature for method '{mangled}'"
                                ),
                                span.clone(),
                            )
                        })?;
                        if signature.result.is_none() {
                            return Err(spanned_error(
                                format!(
                                    "MIR lowering: procedure '{name}' does not return a value and cannot be used in an expression"
                                ),
                                span,
                            ));
                        }
                        return self.lower_object_method_call(this_id, &name, arguments, span);
                    }
                    if let Some(result) = self.try_lower_enclosing_object_method(
                        this_id,
                        &name,
                        arguments,
                        span.clone(),
                    )? {
                        return Ok(result);
                    }
                    if is_simset_method(&name) {
                        return self.lower_simset_method(this_id, &name, arguments, span);
                    }
                    if is_basicio_method(&name)
                        && self.object_supports_basicio_method(this_id, &name)
                    {
                        return self.lower_basicio_method(this_id, &name, arguments, span);
                    }
                    if let Some(result) =
                        self.try_lower_free_basicio(&name, arguments, span.clone())?
                    {
                        return Ok(result);
                    }
                    // Enclosing array snapshotted on `__this` / outer receivers
                    // (`ta(ti)`, boolean arrays as ArrayBool).
                    if self
                        .lookup_method_field(&name)
                        .is_some_and(|(_, _, ty, _)| {
                            matches!(
                                ty,
                                FieldType::ArrayI64
                                    | FieldType::ArrayBool
                                    | FieldType::ArrayF64
                                    | FieldType::ArrayText
                                    | FieldType::ObjectRef
                            )
                        })
                        || self.scope.get(&name).is_some_and(|&id| {
                            matches!(
                                self.local_ty(id),
                                MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText
                            )
                        })
                        || self.name_is_subscriptable(&name)
                    {
                        let variable = Variable::Subscripted {
                            name: name.clone(),
                            subscripts: arguments.clone(),
                        };
                        let place = self.resolve_place(&variable, span.clone())?;
                        return Ok(self.read_place(&place, span));
                    }
                    Err(spanned_error(
                        format!("MIR lowering: call to unknown procedure '{name}'"),
                        span,
                    ))
                } else if self.name_is_subscriptable(&name) {
                    let variable = Variable::Subscripted {
                        name: name.clone(),
                        subscripts: arguments.clone(),
                    };
                    let place = self.resolve_place(&variable, span.clone())?;
                    Ok(self.read_place(&place, span))
                } else if let Some(result) =
                    self.try_lower_free_basicio(&name, arguments, span.clone())?
                {
                    Ok(result)
                } else {
                    Err(spanned_error(
                        format!("MIR lowering: call to unknown procedure '{name}'"),
                        span,
                    ))
                }
            }
            ExprKind::StringLiteral(text) => {
                let dest = self.temp(MirType::Text);
                let string_id = self.intern_string(text);
                self.push(Op::TextFromLiteral { dest, string_id }, span);
                Ok(dest)
            }
            ExprKind::CharacterLiteral(ch) => {
                let dest = self.temp(MirType::I64);
                self.push(
                    Op::ConstI64 {
                        dest,
                        value: *ch as i64,
                    },
                    span,
                );
                Ok(dest)
            }
            ExprKind::Notext => {
                let dest = self.temp(MirType::Text);
                self.push(Op::TextNotext { dest }, span);
                Ok(dest)
            }
            ExprKind::RemoteAccess { object, attribute } => {
                let object_id = self.lower_expr(object)?;
                match self.local_ty(object_id) {
                    MirType::Text => self.lower_text_attribute(object_id, attribute, span),
                    MirType::ObjectRef => {
                        if is_fictitious_detach(attribute) {
                            // 7.3.1 names an object, and the object named here
                            // need not be the one whose body this is: `This
                            // A.Detach` from a nested class detaches the
                            // enclosing component.
                            self.push(Op::SeqDetach { object: object_id }, span.clone());
                            let dest = self.temp(MirType::I64);
                            self.push(Op::ConstI64 { dest, value: 0 }, span);
                            return Ok(dest);
                        }
                        if is_simset_method(attribute) {
                            return self.lower_simset_method(object_id, attribute, &[], span);
                        }
                        if is_basicio_method(attribute) {
                            return self.lower_basicio_method(object_id, attribute, &[], span);
                        }
                        if let Some(result) = self.try_lower_process_builtin_attribute(
                            object_id,
                            attribute,
                            span.clone(),
                        )? {
                            return Ok(result);
                        }
                        if let Some(result) =
                            self.try_lower_parameterless_method(object_id, attribute, span.clone())?
                        {
                            return Ok(result);
                        }
                        let (offset, field_ty) =
                            self.field_info_for(object_id, attribute, span.clone())?;
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
                        Ok(dest)
                    }
                    _ => Err(spanned_error(
                        "MIR lowering: remote attribute access requires an object reference or text value",
                        span,
                    )),
                }
            }
            ExprKind::RemoteCall {
                object,
                attribute,
                arguments,
            } => {
                let object_id = self.lower_expr(object)?;
                if self.local_ty(object_id) == MirType::Text {
                    // Match the interpreter: only a bare text variable is an
                    // L-value for setpos/putchar/get*/put*. Parenthesized or
                    // computed receivers (`(t).putchar`, `t.sub(i,n).setpos`)
                    // mutate a shallow descriptor copy so POS updates do not
                    // write back while still sharing the character object.
                    let frame = self.text_procedure_receiver(object, object_id, span.clone())?;
                    return self.lower_text_remote_call(frame, attribute, arguments, span);
                }
                if self.local_ty(object_id) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "MIR lowering: remote procedure call requires an object reference or text value",
                        span,
                    ));
                }
                self.lower_object_method_call(object_id, attribute, arguments, span)
            }
            ExprKind::None => {
                let dest = self.temp(MirType::ObjectRef);
                self.push(Op::ConstNone { dest }, span);
                Ok(dest)
            }
            ExprKind::New {
                class_name,
                arguments,
            } => {
                let args = arguments.as_deref().unwrap_or(&[]);
                self.lower_new_object(class_name, args, span)
            }
            ExprKind::This(class_name) => self.lower_this(class_name, span),
            ExprKind::Qua { object, class_name } => self.lower_qua(object, class_name, span),
        }
    }

    pub(in crate::mir::lower) fn lower_number_literal(
        &mut self,
        lexeme: &str,
        kind: ArithmeticLiteralKind,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        match kind {
            ArithmeticLiteralKind::Integer => {
                let normalized = lexeme.replace('_', "");
                let value: i64 = normalized.parse().map_err(|_| {
                    spanned_error(format!("invalid integer literal '{lexeme}'"), span.clone())
                })?;
                let dest = self.temp(MirType::I64);
                self.push(Op::ConstI64 { dest, value }, span);
                Ok(dest)
            }
            ArithmeticLiteralKind::Real => {
                let normalized = normalize_real_lexeme(lexeme);
                let value: f64 = normalized.parse().map_err(|_| {
                    spanned_error(format!("invalid real literal '{lexeme}'"), span.clone())
                })?;
                let dest = self.temp(MirType::F64);
                self.push(Op::ConstF64 { dest, value }, span);
                Ok(dest)
            }
            ArithmeticLiteralKind::LongReal => {
                let normalized = normalize_real_lexeme(lexeme);
                let value: f64 = normalized.parse().map_err(|_| {
                    spanned_error(
                        format!("invalid long real literal '{lexeme}'"),
                        span.clone(),
                    )
                })?;
                let dest = self.temp(MirType::LongF64);
                self.push(Op::ConstF64 { dest, value }, span);
                Ok(dest)
            }
        }
    }

    pub(in crate::mir::lower) fn lower_unary(
        &mut self,
        op: UnaryOp,
        operand: &Expr,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let src = self.lower_expr(operand)?;
        match op {
            UnaryOp::Plus => match self.local_ty(src) {
                MirType::I64 | MirType::F64 | MirType::LongF64 => Ok(src),
                _ => Err(spanned_error(
                    "unary '+' requires an arithmetic operand",
                    span,
                )),
            },
            UnaryOp::Minus => match self.local_ty(src) {
                MirType::I64 => {
                    let dest = self.temp(MirType::I64);
                    self.push(
                        Op::Unary {
                            dest,
                            op: UnOp::Neg,
                            src,
                        },
                        span,
                    );
                    Ok(dest)
                }
                ty @ (MirType::F64 | MirType::LongF64) => {
                    let dest = self.temp(ty);
                    self.push(
                        Op::Unary {
                            dest,
                            op: UnOp::Neg,
                            src,
                        },
                        span,
                    );
                    Ok(dest)
                }
                _ => Err(spanned_error(
                    "unary '-' requires an arithmetic operand",
                    span,
                )),
            },
            UnaryOp::Not => {
                if self.local_ty(src) != MirType::Bool {
                    return Err(spanned_error("'not' requires a boolean operand", span));
                }
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::Unary {
                        dest,
                        op: UnOp::Not,
                        src,
                    },
                    span,
                );
                Ok(dest)
            }
        }
    }

    pub(in crate::mir::lower) fn i64_to_f64(&mut self, src: LocalId, span: Span) -> LocalId {
        self.i64_to_float(src, MirType::F64, span)
    }

    pub(in crate::mir::lower) fn i64_to_float(
        &mut self,
        src: LocalId,
        dest_ty: MirType,
        span: Span,
    ) -> LocalId {
        debug_assert!(dest_ty.is_float());
        let dest = self.temp(dest_ty);
        self.push(Op::I64ToF64 { dest, src }, span);
        dest
    }

    /// Assignment / parameter conversion: `entier(E + 0.5)` (§3.3.3).
    /// Plain `entier(E)` uses [`Op::F64ToI64`] directly.
    pub(in crate::mir::lower) fn f64_to_i64(&mut self, src: LocalId, span: Span) -> LocalId {
        let half = self.temp(MirType::F64);
        self.push(
            Op::ConstF64 {
                dest: half,
                value: 0.5,
            },
            span.clone(),
        );
        let sum = self.temp(MirType::F64);
        self.push(
            Op::Binary {
                dest: sum,
                op: BinOp::Add,
                left: src,
                right: half,
            },
            span.clone(),
        );
        let dest = self.temp(MirType::I64);
        self.push(Op::F64ToI64 { dest, src: sum }, span);
        dest
    }

    pub(in crate::mir::lower) fn promote_numeric_pair(
        &mut self,
        left: LocalId,
        right: LocalId,
        span: Span,
    ) -> Result<(LocalId, LocalId, MirType), CompileError> {
        let left_ty = self.local_ty(left);
        let right_ty = self.local_ty(right);
        match (left_ty, right_ty) {
            (MirType::I64, MirType::I64) => Ok((left, right, MirType::I64)),
            (MirType::F64, MirType::F64) => Ok((left, right, MirType::F64)),
            (MirType::LongF64, MirType::LongF64) => Ok((left, right, MirType::LongF64)),
            (MirType::F64, MirType::LongF64) | (MirType::LongF64, MirType::F64) => {
                let left = self.coerce_value(
                    MirType::LongF64,
                    left,
                    "real/long real promote",
                    span.clone(),
                )?;
                let right =
                    self.coerce_value(MirType::LongF64, right, "real/long real promote", span)?;
                Ok((left, right, MirType::LongF64))
            }
            (MirType::I64, MirType::F64) => Ok((
                self.i64_to_float(left, MirType::F64, span),
                right,
                MirType::F64,
            )),
            (MirType::F64, MirType::I64) => Ok((
                left,
                self.i64_to_float(right, MirType::F64, span),
                MirType::F64,
            )),
            (MirType::I64, MirType::LongF64) => Ok((
                self.i64_to_float(left, MirType::LongF64, span),
                right,
                MirType::LongF64,
            )),
            (MirType::LongF64, MirType::I64) => Ok((
                left,
                self.i64_to_float(right, MirType::LongF64, span),
                MirType::LongF64,
            )),
            _ => Err(spanned_error(
                "arithmetic operands must both be integer or real",
                span,
            )),
        }
    }

    pub(in crate::mir::lower) fn lower_binary(
        &mut self,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if matches!(
            op,
            BinaryOp::AndThen | BinaryOp::OrElse | BinaryOp::Imp | BinaryOp::Eqv
        ) {
            return self.lower_boolean_connective(op, left, right, span);
        }

        let left_id = self.lower_expr(left)?;
        let right_id = self.lower_expr(right)?;

        if op == BinaryOp::TextConcat {
            if self.local_ty(left_id) != MirType::Text || self.local_ty(right_id) != MirType::Text {
                return Err(spanned_error(
                    "operands of text concatenation must both be text",
                    span,
                ));
            }
            let dest = self.temp(MirType::Text);
            self.push(
                Op::TextConcat {
                    dest,
                    left: left_id,
                    right: right_id,
                },
                span,
            );
            return Ok(dest);
        }

        match op {
            BinaryOp::And | BinaryOp::Or => {
                if self.local_ty(left_id) != MirType::Bool
                    || self.local_ty(right_id) != MirType::Bool
                {
                    return Err(spanned_error(
                        format!("operands of '{op:?}' must both be boolean"),
                        span,
                    ));
                }
                let mir_op = if op == BinaryOp::And {
                    BinOp::And
                } else {
                    BinOp::Or
                };
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest,
                        op: mir_op,
                        left: left_id,
                        right: right_id,
                    },
                    span,
                );
                Ok(dest)
            }
            BinaryOp::IntDiv => {
                if self.local_ty(left_id) != MirType::I64 || self.local_ty(right_id) != MirType::I64
                {
                    return Err(spanned_error("operands of '//' must both be integer", span));
                }
                let dest = self.temp(MirType::I64);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::IntDiv,
                        left: left_id,
                        right: right_id,
                    },
                    span,
                );
                Ok(dest)
            }
            BinaryOp::Div => {
                // Simula `/` is always real division (integers are promoted).
                let left_f = match self.local_ty(left_id) {
                    MirType::F64 | MirType::LongF64 => left_id,
                    MirType::I64 => self.i64_to_f64(left_id, span.clone()),
                    _ => {
                        return Err(spanned_error(
                            "operands of '/' must be integer or real",
                            span,
                        ));
                    }
                };
                let right_f = match self.local_ty(right_id) {
                    MirType::F64 | MirType::LongF64 => right_id,
                    MirType::I64 => self.i64_to_f64(right_id, span.clone()),
                    _ => {
                        return Err(spanned_error(
                            "operands of '/' must be integer or real",
                            span,
                        ));
                    }
                };
                let result_ty =
                    if self.local_ty(left_f).is_float() || self.local_ty(right_f).is_float() {
                        // Prefer long real if either operand is long.
                        if matches!(self.local_ty(left_f), MirType::LongF64)
                            || matches!(self.local_ty(right_f), MirType::LongF64)
                        {
                            MirType::LongF64
                        } else {
                            MirType::F64
                        }
                    } else {
                        MirType::F64
                    };
                let left_f = self.coerce_value(result_ty, left_f, "real division", span.clone())?;
                let right_f =
                    self.coerce_value(result_ty, right_f, "real division", span.clone())?;
                let dest = self.temp(result_ty);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::Div,
                        left: left_f,
                        right: right_f,
                    },
                    span,
                );
                Ok(dest)
            }
            BinaryOp::Pow => {
                // Simula `**` is always real exponentiation.
                let left_f = match self.local_ty(left_id) {
                    MirType::F64 | MirType::LongF64 => left_id,
                    MirType::I64 => self.i64_to_f64(left_id, span.clone()),
                    _ => {
                        return Err(spanned_error(
                            "operands of '**' must be integer or real",
                            span,
                        ));
                    }
                };
                let right_f = match self.local_ty(right_id) {
                    MirType::F64 | MirType::LongF64 => right_id,
                    MirType::I64 => self.i64_to_f64(right_id, span.clone()),
                    _ => {
                        return Err(spanned_error(
                            "operands of '**' must be integer or real",
                            span,
                        ));
                    }
                };
                let result_ty = if matches!(self.local_ty(left_f), MirType::LongF64)
                    || matches!(self.local_ty(right_f), MirType::LongF64)
                {
                    MirType::LongF64
                } else {
                    MirType::F64
                };
                let left_f = self.coerce_value(result_ty, left_f, "real power", span.clone())?;
                let right_f = self.coerce_value(result_ty, right_f, "real power", span.clone())?;
                let dest = self.temp(result_ty);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::Pow,
                        left: left_f,
                        right: right_f,
                    },
                    span,
                );
                Ok(dest)
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let (left_p, right_p, result_ty) =
                    self.promote_numeric_pair(left_id, right_id, span.clone())?;
                let mir_op = match op {
                    BinaryOp::Add => BinOp::Add,
                    BinaryOp::Sub => BinOp::Sub,
                    BinaryOp::Mul => BinOp::Mul,
                    _ => unreachable!(),
                };
                let dest = self.temp(result_ty);
                self.push(
                    Op::Binary {
                        dest,
                        op: mir_op,
                        left: left_p,
                        right: right_p,
                    },
                    span,
                );
                Ok(dest)
            }
            BinaryOp::Imp | BinaryOp::Eqv | BinaryOp::AndThen | BinaryOp::OrElse => {
                unreachable!("boolean connectives handled at the start of lower_binary")
            }
            BinaryOp::TextConcat => unreachable!("text concatenation handled above"),
        }
    }

    /// Short-circuit `and then` / `or else`, plus eager `imp` / `eqv`.
    pub(in crate::mir::lower) fn lower_boolean_connective(
        &mut self,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        match op {
            BinaryOp::AndThen => {
                let left_id = self.lower_bool_expr(left)?;
                let then_block = self.new_block();
                let else_block = self.new_block();
                let merge_block = self.new_block();
                let result = self.temp(MirType::Bool);
                self.push(
                    Op::Branch {
                        cond: left_id,
                        then_block,
                        else_block,
                    },
                    span.clone(),
                );

                self.switch_to(then_block);
                let right_id = self.lower_bool_expr(right)?;
                self.push(
                    Op::Copy {
                        dest: result,
                        src: right_id,
                    },
                    span.clone(),
                );
                self.push(
                    Op::Jump {
                        target: merge_block,
                    },
                    0..0,
                );

                self.switch_to(else_block);
                self.push(
                    Op::ConstBool {
                        dest: result,
                        value: false,
                    },
                    span,
                );
                self.push(
                    Op::Jump {
                        target: merge_block,
                    },
                    0..0,
                );

                self.switch_to(merge_block);
                Ok(result)
            }
            BinaryOp::OrElse => {
                let left_id = self.lower_bool_expr(left)?;
                let then_block = self.new_block();
                let else_block = self.new_block();
                let merge_block = self.new_block();
                let result = self.temp(MirType::Bool);
                self.push(
                    Op::Branch {
                        cond: left_id,
                        then_block,
                        else_block,
                    },
                    span.clone(),
                );

                self.switch_to(then_block);
                self.push(
                    Op::ConstBool {
                        dest: result,
                        value: true,
                    },
                    span.clone(),
                );
                self.push(
                    Op::Jump {
                        target: merge_block,
                    },
                    0..0,
                );

                self.switch_to(else_block);
                let right_id = self.lower_bool_expr(right)?;
                self.push(
                    Op::Copy {
                        dest: result,
                        src: right_id,
                    },
                    span,
                );
                self.push(
                    Op::Jump {
                        target: merge_block,
                    },
                    0..0,
                );

                self.switch_to(merge_block);
                Ok(result)
            }
            BinaryOp::Imp => {
                // a imp b  ≡  not a or b (eager)
                let left_id = self.lower_bool_expr(left)?;
                let right_id = self.lower_bool_expr(right)?;
                let not_left = self.temp(MirType::Bool);
                self.push(
                    Op::Unary {
                        dest: not_left,
                        op: UnOp::Not,
                        src: left_id,
                    },
                    span.clone(),
                );
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::Or,
                        left: not_left,
                        right: right_id,
                    },
                    span,
                );
                Ok(dest)
            }
            BinaryOp::Eqv => {
                // a eqv b  ≡  (a and b) or (not a and not b)
                let left_id = self.lower_bool_expr(left)?;
                let right_id = self.lower_bool_expr(right)?;
                let both = self.temp(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest: both,
                        op: BinOp::And,
                        left: left_id,
                        right: right_id,
                    },
                    span.clone(),
                );
                let not_left = self.temp(MirType::Bool);
                self.push(
                    Op::Unary {
                        dest: not_left,
                        op: UnOp::Not,
                        src: left_id,
                    },
                    span.clone(),
                );
                let not_right = self.temp(MirType::Bool);
                self.push(
                    Op::Unary {
                        dest: not_right,
                        op: UnOp::Not,
                        src: right_id,
                    },
                    span.clone(),
                );
                let neither = self.temp(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest: neither,
                        op: BinOp::And,
                        left: not_left,
                        right: not_right,
                    },
                    span.clone(),
                );
                let dest = self.temp(MirType::Bool);
                self.push(
                    Op::Binary {
                        dest,
                        op: BinOp::Or,
                        left: both,
                        right: neither,
                    },
                    span,
                );
                Ok(dest)
            }
            _ => unreachable!("not a boolean connective"),
        }
    }

    pub(in crate::mir::lower) fn lower_relation(
        &mut self,
        op: RelationOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        if matches!(op, RelationOp::Is | RelationOp::In) {
            return self.lower_object_relation(op, left, right, span);
        }

        let left_id = self.lower_expr(left)?;
        let right_id = self.lower_expr(right)?;

        if self.local_ty(left_id) == MirType::Text && self.local_ty(right_id) == MirType::Text {
            match op {
                RelationOp::Eq | RelationOp::Ne => {
                    let eq = self.temp(MirType::Bool);
                    self.push(
                        Op::TextContentEq {
                            dest: eq,
                            left: left_id,
                            right: right_id,
                        },
                        span.clone(),
                    );
                    if op == RelationOp::Ne {
                        let dest = self.temp(MirType::Bool);
                        self.push(
                            Op::Unary {
                                dest,
                                op: UnOp::Not,
                                src: eq,
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    return Ok(eq);
                }
                RelationOp::RefEq | RelationOp::RefNe => {
                    let eq = self.temp(MirType::Bool);
                    self.push(
                        Op::TextRefEq {
                            dest: eq,
                            left: left_id,
                            right: right_id,
                        },
                        span.clone(),
                    );
                    if op == RelationOp::RefNe {
                        let dest = self.temp(MirType::Bool);
                        self.push(
                            Op::Unary {
                                dest,
                                op: UnOp::Not,
                                src: eq,
                            },
                            span,
                        );
                        return Ok(dest);
                    }
                    return Ok(eq);
                }
                RelationOp::Lt | RelationOp::Le | RelationOp::Gt | RelationOp::Ge => {
                    let cmp = self.temp(MirType::I64);
                    self.push(
                        Op::TextContentCmp {
                            dest: cmp,
                            left: left_id,
                            right: right_id,
                        },
                        span.clone(),
                    );
                    let zero = self.temp(MirType::I64);
                    self.push(
                        Op::ConstI64 {
                            dest: zero,
                            value: 0,
                        },
                        span.clone(),
                    );
                    let cmp_op = match op {
                        RelationOp::Lt => CmpOp::Lt,
                        RelationOp::Le => CmpOp::Le,
                        RelationOp::Gt => CmpOp::Gt,
                        RelationOp::Ge => CmpOp::Ge,
                        _ => unreachable!(),
                    };
                    let dest = self.temp(MirType::Bool);
                    self.push(
                        Op::Compare {
                            dest,
                            op: cmp_op,
                            left: cmp,
                            right: zero,
                        },
                        span,
                    );
                    return Ok(dest);
                }
                _ => {
                    return Err(spanned_error(
                        format!(
                            "MIR lowering: relation '{op:?}' on text is not supported yet (only '=' / '<>' / '==' / '=/=' / '<' / '<=' / '>' / '>=')"
                        ),
                        span,
                    ));
                }
            }
        }

        let cmp_op = match op {
            RelationOp::Lt => CmpOp::Lt,
            RelationOp::Le => CmpOp::Le,
            RelationOp::Eq => CmpOp::Eq,
            RelationOp::Ge => CmpOp::Ge,
            RelationOp::Gt => CmpOp::Gt,
            RelationOp::Ne => CmpOp::Ne,
            RelationOp::RefEq => {
                if self.local_ty(left_id) != MirType::ObjectRef
                    || self.local_ty(right_id) != MirType::ObjectRef
                {
                    return Err(spanned_error(
                        "reference equality '==' requires object-reference operands",
                        span,
                    ));
                }
                CmpOp::Eq
            }
            RelationOp::RefNe => {
                if self.local_ty(left_id) != MirType::ObjectRef
                    || self.local_ty(right_id) != MirType::ObjectRef
                {
                    return Err(spanned_error(
                        "reference inequality '=/=' requires object-reference operands",
                        span,
                    ));
                }
                CmpOp::Ne
            }
            RelationOp::Is | RelationOp::In => unreachable!("handled above"),
        };

        let (left_cmp, right_cmp) = if matches!(op, RelationOp::RefEq | RelationOp::RefNe) {
            (left_id, right_id)
        } else {
            let left_ty = self.local_ty(left_id);
            let right_ty = self.local_ty(right_id);
            if left_ty == right_ty {
                (left_id, right_id)
            } else if left_ty.is_float() && right_ty.is_float()
                || matches!(
                    (left_ty, right_ty),
                    (MirType::I64, MirType::F64)
                        | (MirType::F64, MirType::I64)
                        | (MirType::I64, MirType::LongF64)
                        | (MirType::LongF64, MirType::I64)
                )
            {
                let (l, r, _) = self.promote_numeric_pair(left_id, right_id, span.clone())?;
                (l, r)
            } else {
                return Err(spanned_error(
                    "relation operands must have the same type",
                    span,
                ));
            }
        };

        let dest = self.temp(MirType::Bool);
        self.push(
            Op::Compare {
                dest,
                op: cmp_op,
                left: left_cmp,
                right: right_cmp,
            },
            span,
        );
        Ok(dest)
    }

    pub(in crate::mir::lower) fn lower_expr_if(
        &mut self,
        condition: &Expr,
        then_expr: &Expr,
        else_expr: &Expr,
        span: Span,
    ) -> Result<LocalId, CompileError> {
        let cond = self.lower_bool_expr(condition)?;
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge_block = self.new_block();

        self.push(
            Op::Branch {
                cond,
                then_block,
                else_block,
            },
            span.clone(),
        );

        // Nested if-/and-/or-expressions leave `current` at their own merge
        // block (and may already terminate `then_block`/`else_block`). Remember
        // the arm *end* blocks for the phi copies — never re-enter the starts.
        self.switch_to(then_block);
        let then_value = self.lower_expr(then_expr)?;
        let then_ty = self.local_ty(then_value);
        let then_end = self.current;

        self.switch_to(else_block);
        let else_value = self.lower_expr(else_expr)?;
        let else_ty = self.local_ty(else_value);
        let else_end = self.current;

        let result_ty = if else_ty == then_ty {
            then_ty
        } else if matches!(
            (then_ty, else_ty),
            (MirType::I64, MirType::F64)
                | (MirType::F64, MirType::I64)
                | (MirType::I64, MirType::LongF64)
                | (MirType::LongF64, MirType::I64)
                | (MirType::F64, MirType::LongF64)
                | (MirType::LongF64, MirType::F64)
        ) {
            if matches!(then_ty, MirType::LongF64) || matches!(else_ty, MirType::LongF64) {
                MirType::LongF64
            } else {
                MirType::F64
            }
        } else {
            return Err(spanned_error(
                "if-expression branches must have the same type",
                span,
            ));
        };

        let result = self.temp(result_ty);

        self.switch_to(then_end);
        let then_value = self.coerce_value(
            result_ty,
            then_value,
            "if-expression branches must have the same type",
            span.clone(),
        )?;
        self.push(
            Op::Copy {
                dest: result,
                src: then_value,
            },
            span.clone(),
        );
        self.push(
            Op::Jump {
                target: merge_block,
            },
            0..0,
        );

        self.switch_to(else_end);
        let else_value = self.coerce_value(
            result_ty,
            else_value,
            "if-expression branches must have the same type",
            span.clone(),
        )?;
        self.push(
            Op::Copy {
                dest: result,
                src: else_value,
            },
            span,
        );
        self.push(
            Op::Jump {
                target: merge_block,
            },
            0..0,
        );

        self.switch_to(merge_block);
        Ok(result)
    }
}
