//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn lower_if(
        &mut self,
        if_stmt: &IfStatement,
        span: Span,
    ) -> Result<(), CompileError> {
        let cond = self.lower_bool_expr(&if_stmt.condition)?;
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge_block = self.new_block();

        self.push(
            Op::Branch {
                cond,
                then_block,
                else_block,
            },
            span,
        );

        self.switch_to(then_block);
        self.lower_statement(&if_stmt.then_branch)?;
        // Synthetic edge back to the merge point; no single source token owns it.
        self.push(
            Op::Jump {
                target: merge_block,
            },
            0..0,
        );

        self.switch_to(else_block);
        if let Some(else_branch) = &if_stmt.else_branch {
            self.lower_statement(else_branch)?;
        }
        self.push(
            Op::Jump {
                target: merge_block,
            },
            0..0,
        );

        self.switch_to(merge_block);
        Ok(())
    }

    pub(in crate::mir::lower) fn lower_while(
        &mut self,
        while_stmt: &WhileStatement,
        span: Span,
    ) -> Result<(), CompileError> {
        let header = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();

        self.push(Op::Jump { target: header }, span.clone());

        self.switch_to(header);
        let cond = self.lower_bool_expr(&while_stmt.condition)?;
        self.push(
            Op::Branch {
                cond,
                then_block: body,
                else_block: exit,
            },
            span,
        );

        self.switch_to(body);
        self.lower_statement(&while_stmt.body)?;
        self.push(Op::Jump { target: header }, 0..0);

        self.switch_to(exit);
        Ok(())
    }

    /// Lowers `for` statements. Supports `step until`, single value / reference
    /// elements, and `while` variants (`for i := e while b do …`,
    /// `for r :- e while b do …`).
    pub(in crate::mir::lower) fn lower_for(
        &mut self,
        for_stmt: &ForStatement,
        span: Span,
    ) -> Result<(), CompileError> {
        for element in &for_stmt.elements {
            match element {
                ForListElement::StepUntil { start, step, until } => {
                    self.lower_for_step_until(for_stmt, start, step, until, span.clone())?;
                }
                ForListElement::Value {
                    expr,
                    while_cond: None,
                } => {
                    self.lower_for_assign_once(
                        for_stmt,
                        expr,
                        AssignOperator::Assign,
                        span.clone(),
                    )?;
                }
                ForListElement::Value {
                    expr,
                    while_cond: Some(while_cond),
                } => {
                    self.lower_for_assign_while(
                        for_stmt,
                        expr,
                        while_cond,
                        AssignOperator::Assign,
                        span.clone(),
                    )?;
                }
                ForListElement::Reference {
                    expr,
                    while_cond: None,
                } => {
                    self.lower_for_assign_once(
                        for_stmt,
                        expr,
                        AssignOperator::AssignAlt,
                        span.clone(),
                    )?;
                }
                ForListElement::Reference {
                    expr,
                    while_cond: Some(while_cond),
                } => {
                    self.lower_for_assign_while(
                        for_stmt,
                        expr,
                        while_cond,
                        AssignOperator::AssignAlt,
                        span.clone(),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(in crate::mir::lower) fn lower_for_assign_once(
        &mut self,
        for_stmt: &ForStatement,
        expr: &Expr,
        operator: AssignOperator,
        span: Span,
    ) -> Result<(), CompileError> {
        let place =
            self.resolve_place(&Variable::Simple(for_stmt.variable.clone()), span.clone())?;
        let value = self.lower_expr(expr)?;
        match operator {
            AssignOperator::Assign => {
                let value = self.coerce_assign_value(self.place_ty(&place), value, span.clone())?;
                self.write_place(&place, value, span.clone());
            }
            AssignOperator::AssignAlt => {
                if self.place_ty(&place) != MirType::ObjectRef
                    && self.place_ty(&place) != MirType::Text
                {
                    return Err(spanned_error(
                        "MIR lowering: 'for' reference element requires a text or object-reference control variable",
                        span,
                    ));
                }
                if self.local_ty(value) != self.place_ty(&place) {
                    return Err(spanned_error(
                        "MIR lowering: 'for' reference element operand types do not match",
                        expr.span.clone(),
                    ));
                }
                match &place {
                    Place::Local(id) if self.local_ty(*id) == MirType::Text => {
                        self.push(
                            Op::TextRefAssign {
                                dest: *id,
                                src: value,
                            },
                            span.clone(),
                        );
                    }
                    Place::Local(id) => {
                        self.push(
                            Op::StoreLocal {
                                local: *id,
                                src: value,
                            },
                            span.clone(),
                        );
                        self.note_object_qual_from_assign(*id, value);
                    }
                    other => {
                        self.write_place(other, value, span.clone());
                    }
                }
            }
        }
        self.lower_statement(&for_stmt.body)?;
        Ok(())
    }

    pub(in crate::mir::lower) fn lower_for_assign_while(
        &mut self,
        for_stmt: &ForStatement,
        expr: &Expr,
        while_cond: &Expr,
        operator: AssignOperator,
        span: Span,
    ) -> Result<(), CompileError> {
        let header = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();
        self.push(Op::Jump { target: header }, span.clone());
        self.switch_to(header);
        // Assign control variable, then test while condition (Standard §4.4).
        {
            let place =
                self.resolve_place(&Variable::Simple(for_stmt.variable.clone()), span.clone())?;
            let value = self.lower_expr(expr)?;
            match operator {
                AssignOperator::Assign => {
                    let value =
                        self.coerce_assign_value(self.place_ty(&place), value, span.clone())?;
                    self.write_place(&place, value, span.clone());
                }
                AssignOperator::AssignAlt => {
                    if self.place_ty(&place) != MirType::ObjectRef
                        && self.place_ty(&place) != MirType::Text
                    {
                        return Err(spanned_error(
                            "MIR lowering: 'for' reference element requires a text or object-reference control variable",
                            span,
                        ));
                    }
                    if self.local_ty(value) != self.place_ty(&place) {
                        return Err(spanned_error(
                            "MIR lowering: 'for' reference element operand types do not match",
                            expr.span.clone(),
                        ));
                    }
                    match &place {
                        Place::Local(id) if self.local_ty(*id) == MirType::Text => {
                            self.push(
                                Op::TextRefAssign {
                                    dest: *id,
                                    src: value,
                                },
                                span.clone(),
                            );
                        }
                        Place::Local(id) => {
                            self.push(
                                Op::StoreLocal {
                                    local: *id,
                                    src: value,
                                },
                                span.clone(),
                            );
                            self.note_object_qual_from_assign(*id, value);
                        }
                        other => {
                            self.write_place(other, value, span.clone());
                        }
                    }
                }
            }
        }
        let cond = self.lower_bool_expr(while_cond)?;
        self.push(
            Op::Branch {
                cond,
                then_block: body,
                else_block: exit,
            },
            while_cond.span.clone(),
        );
        self.switch_to(body);
        // `for x :- … while x is C do` — after the test succeeds, treat `x` as
        // class `C` for remote attribute lookup in the body (simtst96
        // `for r:-r.suc while r is town do r.gone:=…`).
        let saved_qual =
            self.narrow_for_control_from_is_while(&for_stmt.variable, while_cond, span.clone())?;
        let body_result = self.lower_statement(&for_stmt.body);
        if let Some((id, prev)) = saved_qual {
            self.restore_object_qual(id, prev);
        }
        body_result?;
        self.push(Op::Jump { target: header }, span.clone());
        self.switch_to(exit);
        Ok(())
    }

    /// When `while_cond` is `control is Class` / `control in Class`, set
    /// `ref_qual` on the control local to `Class` for the loop body.
    /// Returns `(local, previous_qual)` so the caller can restore.
    pub(in crate::mir::lower) fn narrow_for_control_from_is_while(
        &mut self,
        control: &str,
        while_cond: &Expr,
        span: Span,
    ) -> Result<Option<(LocalId, (Option<String>, Option<String>))>, CompileError> {
        let ExprKind::Relation { left, op, right } = &while_cond.kind else {
            return Ok(None);
        };
        if !matches!(op, RelationOp::Is | RelationOp::In) {
            return Ok(None);
        }
        let ExprKind::Variable(Variable::Simple(left_name)) = &left.kind else {
            return Ok(None);
        };
        if !left_name.eq_ignore_ascii_case(control) {
            return Ok(None);
        }
        let class_name = match &right.kind {
            ExprKind::Variable(Variable::Simple(name)) => name.as_str(),
            ExprKind::This(name) => name.as_str(),
            _ => return Ok(None),
        };
        let Some(&id) = self.scope.get(control).or_else(|| {
            self.scope
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(control))
                .map(|(_, id)| id)
        }) else {
            return Ok(None);
        };
        if self.local_ty(id) != MirType::ObjectRef {
            return Ok(None);
        }
        let target = self
            .find_layout_at(class_name, Some(&span))
            .map(|layout| layout.name.clone())
            .unwrap_or_else(|| class_name.to_string());
        let previous = self.snapshot_object_qual(id);
        self.note_object_qual(id, target);
        Ok(Some((id, previous)))
    }

    /// `for C := start step stepExpr until limit do body` — matches
    /// `eval::execute_for_element` / `step_until_condition`.
    pub(in crate::mir::lower) fn lower_for_step_until(
        &mut self,
        for_stmt: &ForStatement,
        start: &Expr,
        step: &Expr,
        until: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        let var = Variable::Simple(for_stmt.variable.clone());
        // Re-resolve the control place on each use so call-by-name control
        // variables (e.g. formal `p` bound to `a(i)`) re-evaluate subscripts.
        let initial_place = self.resolve_place(&var, span.clone())?;
        let control_ty = self.place_ty(&initial_place);
        let is_float = matches!(control_ty, MirType::F64 | MirType::LongF64);
        if control_ty != MirType::I64 && !is_float {
            return Err(spanned_error(
                "MIR lowering: 'for' step-until requires an integer or real control variable",
                span,
            ));
        }
        let numeric_ty = if is_float { MirType::F64 } else { MirType::I64 };

        let start_id = self.lower_expr(start)?;
        let start_id = self.coerce_value(
            numeric_ty,
            start_id,
            "for start expression must match the control variable type",
            start.span.clone(),
        )?;
        let place = self.resolve_place(&var, span.clone())?;
        self.write_place(&place, start_id, span.clone());

        let until_id = self.lower_expr(until)?;
        let until_id = self.coerce_value(
            numeric_ty,
            until_id,
            "for until expression must match the control variable type",
            until.span.clone(),
        )?;

        let header = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();

        let delta_id = self.lower_expr(step)?;
        let mut delta_id = self.coerce_value(
            numeric_ty,
            delta_id,
            "for step expression must match the control variable type",
            step.span.clone(),
        )?;

        self.push(Op::Jump { target: header }, span.clone());

        self.switch_to(header);
        let place = self.resolve_place(&var, span.clone())?;
        let current_id = self.read_place(&place, span.clone());
        let current_id = if is_float && self.local_ty(current_id) == MirType::LongF64 {
            self.coerce_value(MirType::F64, current_id, "for control", span.clone())?
        } else {
            current_id
        };
        let cond = if is_float {
            self.lower_step_until_cond_f64(delta_id, current_id, until_id, span.clone())
        } else {
            self.lower_step_until_cond(delta_id, current_id, until_id, span.clone())
        };
        self.push(
            Op::Branch {
                cond,
                then_block: body,
                else_block: exit,
            },
            span.clone(),
        );

        self.switch_to(body);
        self.lower_statement(&for_stmt.body)?;
        delta_id = self.lower_expr(step)?;
        delta_id = self.coerce_value(
            numeric_ty,
            delta_id,
            "for step expression must match the control variable type",
            step.span.clone(),
        )?;
        let place = self.resolve_place(&var, span.clone())?;
        let current_id = self.read_place(&place, span.clone());
        let current_id = if is_float && self.local_ty(current_id) == MirType::LongF64 {
            self.coerce_value(MirType::F64, current_id, "for control", span.clone())?
        } else {
            current_id
        };
        let next = if is_float {
            self.lower_f64_bin(BinOp::Add, current_id, delta_id, span.clone())
        } else {
            self.lower_i64_bin(BinOp::Add, current_id, delta_id, span.clone())
        };
        let next = if control_ty == MirType::LongF64 {
            self.coerce_value(MirType::LongF64, next, "for control", span.clone())?
        } else {
            next
        };
        let place = self.resolve_place(&var, span.clone())?;
        self.write_place(&place, next, span.clone());
        self.push(Op::Jump { target: header }, 0..0);

        self.switch_to(exit);
        Ok(())
    }

    /// `delta * (current - until) <= 0`, matching `eval::step_until_condition`
    /// for integer operands.
    pub(in crate::mir::lower) fn lower_step_until_cond(
        &mut self,
        delta: LocalId,
        current: LocalId,
        until: LocalId,
        span: Span,
    ) -> LocalId {
        let diff = self.lower_i64_bin(BinOp::Sub, current, until, span.clone());
        let product = self.lower_i64_bin(BinOp::Mul, delta, diff, span.clone());
        let zero = self.temp(MirType::I64);
        self.push(
            Op::ConstI64 {
                dest: zero,
                value: 0,
            },
            0..0,
        );
        let dest = self.temp(MirType::Bool);
        self.push(
            Op::Compare {
                dest,
                op: CmpOp::Le,
                left: product,
                right: zero,
            },
            span,
        );
        dest
    }

    /// Real analogue of [`Self::lower_step_until_cond`].
    pub(in crate::mir::lower) fn lower_step_until_cond_f64(
        &mut self,
        delta: LocalId,
        current: LocalId,
        until: LocalId,
        span: Span,
    ) -> LocalId {
        let diff = self.lower_f64_bin(BinOp::Sub, current, until, span.clone());
        let product = self.lower_f64_bin(BinOp::Mul, delta, diff, span.clone());
        let zero = self.temp(MirType::F64);
        self.push(
            Op::ConstF64 {
                dest: zero,
                value: 0.0,
            },
            0..0,
        );
        let dest = self.temp(MirType::Bool);
        self.push(
            Op::Compare {
                dest,
                op: CmpOp::Le,
                left: product,
                right: zero,
            },
            span,
        );
        dest
    }

    pub(in crate::mir::lower) fn lower_i64_bin(
        &mut self,
        op: BinOp,
        left: LocalId,
        right: LocalId,
        span: Span,
    ) -> LocalId {
        let dest = self.temp(MirType::I64);
        self.push(
            Op::Binary {
                dest,
                op,
                left,
                right,
            },
            span,
        );
        dest
    }

    pub(in crate::mir::lower) fn lower_f64_bin(
        &mut self,
        op: BinOp,
        left: LocalId,
        right: LocalId,
        span: Span,
    ) -> LocalId {
        let dest = self.temp(MirType::F64);
        self.push(
            Op::Binary {
                dest,
                op,
                left,
                right,
            },
            span,
        );
        dest
    }
}
