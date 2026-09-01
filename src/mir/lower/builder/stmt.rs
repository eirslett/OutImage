//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl<'a> FunctionBuilder<'a> {
    pub(in crate::mir::lower) fn lower_statement(
        &mut self,
        statement: &Statement,
    ) -> Result<(), CompileError> {
        let span = statement.span.clone();
        let saved_text_span = self.text_span.clone();
        if span.start != span.end {
            self.text_span = span.clone();
        }
        let result = self.lower_statement_inner(statement, span);
        self.text_span = saved_text_span;
        result
    }

    pub(in crate::mir::lower) fn lower_statement_inner(
        &mut self,
        statement: &Statement,
        span: Span,
    ) -> Result<(), CompileError> {
        match &statement.kind {
            StatementKind::Dummy => {
                self.push(Op::Nop, span);
                Ok(())
            }
            StatementKind::ProcedureCall(call) => self.lower_procedure_call(call, span),
            StatementKind::Assignment(assignment) => self.lower_assignment(assignment, span),
            StatementKind::If(if_stmt) => self.lower_if(if_stmt, span),
            StatementKind::While(while_stmt) => self.lower_while(while_stmt, span),
            StatementKind::Compound(block) => {
                if block.prefix.is_some() {
                    self.lower_block(block)
                } else {
                    let scope_span = if span.start < span.end {
                        span.clone()
                    } else {
                        super::block::block_source_span(block)
                    };
                    let pushed = self.enter_block_debug_scope(
                        super::block::block_debug_name(block),
                        scope_span,
                        block,
                    );
                    let result = self.lower_block(block);
                    if pushed {
                        self.pop_debug_scope();
                    }
                    result
                }
            }
            StatementKind::Labeled { label, statement } => {
                let label_bb = self
                    .label_def_queue
                    .pop_front()
                    .unwrap_or_else(|| self.label_block(label));
                // Fallthrough into the labelled statement.
                self.push(Op::Jump { target: label_bb }, span);
                self.switch_to(label_bb);
                self.lower_statement(statement)
            }
            StatementKind::Expr(expr) => {
                self.lower_expr(expr)?;
                Ok(())
            }
            StatementKind::For(for_stmt) => self.lower_for(for_stmt, span),
            StatementKind::Goto(goto_stmt) => self.lower_goto(goto_stmt, span),
            StatementKind::ObjectGenerator(generator) => {
                self.lower_object_generator(generator, span)?;
                Ok(())
            }
            StatementKind::Inner { .. } => {
                // Parse/concat remove `inner` markers into `tail_statements`;
                // treat a residual marker as a no-op.
                self.push(Op::Nop, span);
                Ok(())
            }
            StatementKind::Inspect(inspect) => self.lower_inspect(inspect, span),
            StatementKind::Activate(activate) => self.lower_activate(activate, span, false),
            StatementKind::Reactivate(reactivate) => self.lower_activate(
                &ActivateStatement {
                    target: reactivate.target.clone(),
                    timing: reactivate.timing.clone(),
                    prior: false,
                },
                span,
                true,
            ),
        }
    }

    pub(in crate::mir::lower) fn lower_activate(
        &mut self,
        activate: &ActivateStatement,
        span: Span,
        reac: bool,
    ) -> Result<(), CompileError> {
        if !self.simulation_context {
            // Pre-Simulation Ch.7: immediate resume (timing evaluated for errors).
            if let Some(timing) = &activate.timing {
                match timing {
                    SimulationTiming::Delay(expr)
                    | SimulationTiming::At(expr)
                    | SimulationTiming::Before(expr)
                    | SimulationTiming::After(expr) => {
                        let _ = self.lower_expr(expr)?;
                    }
                }
            }
            return self.lower_call_or_resume(
                &ProcedureCall {
                    name: "resume".into(),
                    arguments: vec![activate.target.clone()],
                },
                span,
            );
        }
        let object = self.lower_expr(&activate.target)?;
        if self.local_ty(object) != MirType::ObjectRef {
            return Err(spanned_error(
                "activate requires an object reference",
                activate.target.span.clone(),
            ));
        }
        match &activate.timing {
            None => {
                if reac {
                    // Reactivate direct: cancel then schedule at now with prior.
                    self.push(Op::SimCancel { process: object }, span.clone());
                }
                self.push(Op::SimActivateDirect { process: object }, span.clone());
                // 12.2: "an empty scheduling clause indicates direct activation,
                // whereby an active phase of X is initiated immediately … the
                // formerly active process object becomes suspended".
                self.push(Op::SimTransferToHead, span.clone());
            }
            Some(SimulationTiming::Delay(expr)) => {
                let t = self.lower_hold_dt(expr)?;
                self.push(
                    Op::SimActivateTimed {
                        process: object,
                        t,
                        mode: 0,
                        prior: activate.prior,
                        reac,
                    },
                    span.clone(),
                );
            }
            Some(SimulationTiming::At(expr)) => {
                let t = self.lower_hold_dt(expr)?;
                self.push(
                    Op::SimActivateTimed {
                        process: object,
                        t,
                        mode: 1,
                        prior: activate.prior,
                        reac,
                    },
                    span.clone(),
                );
            }
            Some(SimulationTiming::Before(expr)) => {
                let other = self.lower_expr(expr)?;
                if self.local_ty(other) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "activate before requires an object reference",
                        expr.span.clone(),
                    ));
                }
                self.push(
                    Op::SimActivateRelative {
                        process: object,
                        other,
                        before: true,
                    },
                    span.clone(),
                );
            }
            Some(SimulationTiming::After(expr)) => {
                let other = self.lower_expr(expr)?;
                if self.local_ty(other) != MirType::ObjectRef {
                    return Err(spanned_error(
                        "activate after requires an object reference",
                        expr.span.clone(),
                    ));
                }
                self.push(
                    Op::SimActivateRelative {
                        process: object,
                        other,
                        before: false,
                    },
                    span.clone(),
                );
            }
        }
        // A timing clause only files an event notice: unlike direct activation
        // it does not start an active phase, so the activating process keeps the
        // PSC and runs on until it suspends itself (simtst87).
        Ok(())
    }

    pub(in crate::mir::lower) fn lower_goto(
        &mut self,
        goto_stmt: &GotoStatement,
        span: Span,
    ) -> Result<(), CompileError> {
        self.lower_goto_target(&goto_stmt.target, span)?;
        // Label gotos never fall through; open a fresh block so later statements
        // are not appended after a terminator. Switch / if-designators may be a
        // no-op when out of range (§4.5), so keep `current` as the continuation.
        if !designational_expr_may_fallthrough(&goto_stmt.target) {
            let cont = self.new_block();
            self.switch_to(cont);
        }
        Ok(())
    }

    pub(in crate::mir::lower) fn lower_goto_target(
        &mut self,
        target: &DesignationalExpr,
        span: Span,
    ) -> Result<(), CompileError> {
        match target {
            DesignationalExpr::Label(name) => {
                if let Some(bound) = self.resolve_formal_label(name).cloned() {
                    return self.lower_goto_target(&bound, span);
                }
                match self.resolve_label_target(name) {
                    LabelTarget::Block(target) => self.push(Op::Jump { target }, span),
                    LabelTarget::Escape(label) => self.push(Op::GotoEscape { label }, span),
                }
                Ok(())
            }
            DesignationalExpr::Paren(inner) => self.lower_goto_target(inner, span),
            DesignationalExpr::SwitchDesignator { name, subscript } => {
                self.lower_switch_goto(name, subscript, span)
            }
            DesignationalExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let cond = self.lower_bool_expr(condition)?;
                let then_block = self.new_block();
                let else_block = self.new_block();
                self.push(
                    Op::Branch {
                        cond,
                        then_block,
                        else_block,
                    },
                    span.clone(),
                );
                self.switch_to(then_block);
                self.lower_goto_target(then_expr, span.clone())?;
                self.switch_to(else_block);
                self.lower_goto_target(else_expr, span)?;
                Ok(())
            }
        }
    }

    pub(in crate::mir::lower) fn lower_switch_goto(
        &mut self,
        name: &str,
        subscript: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        let resolved = self
            .resolve_formal_switch(name)
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.to_string());
        let key = resolved.to_ascii_lowercase();
        let index = self.lower_expr(subscript)?;
        let index = match self.local_ty(index) {
            MirType::I64 => index,
            MirType::F64 | MirType::LongF64 => {
                let dest = self.temp(MirType::I64);
                self.push(Op::F64ToI64 { dest, src: index }, span.clone());
                dest
            }
            other => {
                return Err(spanned_error(
                    format!("switch designator subscript must be integer, found {other}"),
                    subscript.span.clone(),
                ));
            }
        };

        // A switch's own element list may refer back to itself (or to
        // another switch that refers back to it in turn, §4.5); reuse the
        // already-compiled dispatch block for `key` instead of re-lowering
        // its element chain, which would otherwise recurse without bound.
        if let Some(&(entry_block, index_slot)) = self.switch_dispatch.get(&key) {
            self.push(
                Op::Copy {
                    dest: index_slot,
                    src: index,
                },
                span.clone(),
            );
            self.push(
                Op::Jump {
                    target: entry_block,
                },
                span,
            );
            return Ok(());
        }

        let elements = self
            .switches
            .get(&key)
            .cloned()
            .ok_or_else(|| spanned_error(format!("undefined switch '{name}'"), span.clone()))?;

        let index_slot = self.temp(MirType::I64);
        self.push(
            Op::Copy {
                dest: index_slot,
                src: index,
            },
            span.clone(),
        );
        let entry_block = self.new_block();
        // Register before lowering elements so self/mutually-referential
        // designators within this switch's own list reuse this block.
        self.switch_dispatch
            .insert(key.clone(), (entry_block, index_slot));
        self.push(
            Op::Jump {
                target: entry_block,
            },
            span.clone(),
        );
        self.switch_to(entry_block);

        // Out-of-range subscript: Simula §4.5 makes the designator a no-op —
        // fall through here into the statement after the `goto` (simtst54).
        let oob = self.new_block();
        for (i, element) in elements.iter().enumerate() {
            let match_bb = self.new_block();
            let next = if i + 1 < elements.len() {
                self.new_block()
            } else {
                oob
            };
            let expected = self.temp(MirType::I64);
            self.push(
                Op::ConstI64 {
                    dest: expected,
                    value: (i + 1) as i64,
                },
                0..0,
            );
            let eq = self.temp(MirType::Bool);
            self.push(
                Op::Compare {
                    dest: eq,
                    op: CmpOp::Eq,
                    left: index_slot,
                    right: expected,
                },
                span.clone(),
            );
            self.push(
                Op::Branch {
                    cond: eq,
                    then_block: match_bb,
                    else_block: next,
                },
                span.clone(),
            );
            self.switch_to(match_bb);
            self.lower_goto_target(element, span.clone())?;
            self.switch_to(next);
        }
        self.switch_to(oob);
        Ok(())
    }
}
