//! FunctionBuilder methods for [`crate::mir::lower`].

use super::super::*;

impl FunctionBuilder<'_> {
    /// Whether objects of `class_name` get their own stack, i.e. the class body
    /// can suspend and the per-component sequencing runtime is in use.
    pub(in crate::mir::lower) fn class_runs_on_own_stack(&self, class_name: &str) -> bool {
        self.find_layout(class_name)
            .is_some_and(|layout| layout.runs_on_own_stack)
    }

    /// A Process is a component like any other; chapter 12 only adds the
    /// sequencing set that decides which of them is operative.
    pub(in crate::mir::lower) fn class_is_scheduled_process(&self, class_name: &str) -> bool {
        crate::simulation::is_process_class(class_name)
            || is_subclass_of(class_name, "process", self.classes)
    }

    /// Creates the component for a freshly generated object and runs its body.
    pub(in crate::mir::lower) fn emit_seq_object_generation(
        &mut self,
        object: LocalId,
        class_name: &str,
        span: Span,
    ) -> Result<(), CompileError> {
        // The system is the instance of the declaring block this generator is
        // running inside, which only the runtime can know; naming the block is
        // all the compiler can do (7.2).
        let declaring_block = self
            .find_layout(class_name)
            .map_or(0, |layout| layout.system_block);

        let entry = self.temp(MirType::FuncRef);
        self.push(
            Op::FuncAddr {
                dest: entry,
                name: mangle_coro_entry_name(class_name),
            },
            span.clone(),
        );

        let component = self.temp(MirType::RefI64);
        self.push(
            Op::SeqObjectCreate {
                dest: component,
                declaring_block,
                entry,
                object,
            },
            span.clone(),
        );
        let refreshed = self.refresh_enclosing_captures(object, class_name, span.clone())?;
        self.push(Op::SeqObjectStart { component }, span.clone());
        self.writeback_enclosing_captures(object, class_name, &refreshed, span)
    }

    /// Enclosing variables live in the frame of the block instance that
    /// declares the class, which the component's own stack cannot reach, so a
    /// transfer of control carries them across on the object: the side giving
    /// up control pushes its locals onto the object first and takes back
    /// whatever the component left there when it returns.
    pub(in crate::mir::lower) fn around_seq_transfer(
        &mut self,
        object: LocalId,
        span: Span,
        transfer: impl Fn(&mut Self, Span) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        let Some(class_name) = self.ref_qual.get(&object).cloned() else {
            return transfer(self, span);
        };
        let targets = self.seq_capture_families(&class_name);
        // A reference is qualified by a prefix (`ref(Coroutine) r` naming a
        // `Reader`), and each class in the family lays its captures out after
        // its own attributes, so the slots to carry across are only known from
        // the object's runtime class.
        if targets.len() == 1 && targets[0].1.eq_ignore_ascii_case(&class_name) {
            let refreshed = self.refresh_enclosing_captures(object, &class_name, span.clone())?;
            transfer(self, span.clone())?;
            return self.writeback_enclosing_captures(object, &class_name, &refreshed, span);
        }
        if targets.is_empty() {
            return transfer(self, span);
        }

        let class_id = self.temp(MirType::I64);
        self.push(
            Op::ObjectClassIdSafe {
                dest: class_id,
                object,
            },
            span.clone(),
        );
        let merge = self.new_block();
        for (target_id, target_class) in &targets {
            let matched = self.new_block();
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
                    then_block: matched,
                    else_block: next,
                },
                span.clone(),
            );
            self.switch_to(matched);
            let refreshed = self.refresh_enclosing_captures(object, target_class, span.clone())?;
            transfer(self, span.clone())?;
            self.writeback_enclosing_captures(object, target_class, &refreshed, span.clone())?;
            self.push(Op::Jump { target: merge }, 0..0);
            self.switch_to(next);
        }
        // A class in the family with no captures of its own needs no carrying.
        transfer(self, span)?;
        self.push(Op::Jump { target: merge }, 0..0);
        self.switch_to(merge);
        Ok(())
    }

    /// The static qualification and its subclasses that carry enclosing
    /// captures, by runtime class id.
    pub(in crate::mir::lower) fn seq_capture_families(
        &self,
        class_name: &str,
    ) -> Vec<(i64, String)> {
        let mut targets: Vec<(i64, String)> = self
            .layouts
            .values()
            .filter(|layout| {
                !layout.enclosing_captures.is_empty()
                    && (layout.name.eq_ignore_ascii_case(class_name)
                        || is_subclass_of(&layout.name, class_name, self.classes))
            })
            .map(|layout| (layout.class_id, layout.name.clone()))
            .collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));
        targets
    }

    /// 7.3.4 for the object whose body this function is.
    ///
    /// A process leaves the sequencing set as it terminates, and control goes to
    /// whichever process now heads it — not to the main component, which is
    /// where an ordinary component's final end leads (12.3).
    pub(in crate::mir::lower) fn emit_seq_terminate(
        &mut self,
        object: LocalId,
        is_process: bool,
        span: Span,
    ) -> Result<(), CompileError> {
        if is_process {
            self.push(Op::SimTerminateCurrent { process: object }, span.clone());
        } else {
            self.push(Op::SeqTerminate { object }, span.clone());
        }
        // `SeqTerminate` never returns, but the function still needs a
        // terminator for the block to be well formed.
        self.push(Op::Return { value: None }, span);
        Ok(())
    }

    /// After an SQS transfer returns in a Process body, `this` may still hold a
    /// stale pointer (Windows fiber register / TLS). Reload from SQS `current`,
    /// which is this process once it is operative again.
    pub(in crate::mir::lower) fn reload_process_this_after_transfer(&mut self, span: Span) {
        if self.method_this_is_connection {
            return;
        }
        let Some(this_id) = self.method_this else {
            return;
        };
        let Some(qual) = self.ref_qual.get(&this_id).cloned() else {
            return;
        };
        if !self.class_is_scheduled_process(&qual) {
            return;
        }
        self.push(Op::SimCurrent { dest: this_id }, span);
    }
}
