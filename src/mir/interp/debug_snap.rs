//! Build DAP [`VariableSnapshot`]s from MIR interpreter state (Phase 8).

use crate::debug::{
    DebugLiteral, InlineFrameSnap, REF_ARRAY_BASE, REF_LOCALS, REF_OBJECT_BASE, REF_SIMULATION,
    REF_SQS, ThreadInfo, VarEntry, VariableSnapshot, parse_debug_value,
};
use crate::error::Span;
use crate::layout::{FieldLayout, FieldType, SIMSET_PRED_FIELD, SIMSET_SUC_FIELD};
use crate::mir::{DebugScope, DebugScopeKind, Function, Local, MirType};
use crate::runtime::text::TextFrame;

use super::{ArrayStorage, Value, Vm};

fn object_ref_key(identity: u64) -> i64 {
    REF_OBJECT_BASE + identity as i64
}

fn identity_for(index: usize) -> u64 {
    index as u64 + 1
}

fn format_text(text: &TextFrame) -> String {
    if text.is_notext() {
        return "notext".into();
    }
    let escaped = text
        .content()
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            '\t' => vec!['\\', 't'],
            other => vec![other],
        })
        .collect::<String>();
    format!("\"{escaped}\"")
}

fn typed_uninit_display(ty: MirType, value: &Value) -> Option<(String, i64)> {
    match (ty, value) {
        (MirType::Bool, Value::Bool(b)) => Some(((if *b { "true" } else { "false" }).into(), 0)),
        (MirType::Bool, Value::I64(n)) => {
            Some(((if *n != 0 { "true" } else { "false" }).into(), 0))
        }
        (MirType::ObjectRef, Value::I64(0)) => Some(("none".into(), 0)),
        (MirType::Text, Value::I64(0)) => Some(("notext".into(), 0)),
        (MirType::F64 | MirType::LongF64, Value::I64(0)) => Some(("0.0".into(), 0)),
        (MirType::ArrayI64 | MirType::ArrayF64 | MirType::ArrayText, Value::I64(0)) => {
            Some(("none".into(), 0))
        }
        _ => None,
    }
}

fn format_mir_value(
    vm: &Vm<'_>,
    value: &Value,
    ty: MirType,
    class_qual: Option<&str>,
) -> (String, i64) {
    if let Some(display) = typed_uninit_display(ty, value) {
        return display;
    }
    match value {
        Value::I64(n) => (n.to_string(), 0),
        Value::Bool(b) => ((if *b { "true" } else { "false" }).into(), 0),
        Value::F64(n) => {
            let s = format!("{n}");
            if s.contains('.') || s.contains('e') || s.contains('E') {
                (s, 0)
            } else {
                (format!("{n}.0"), 0)
            }
        }
        Value::None => ("none".into(), 0),
        Value::ObjectRef(index) => {
            let id = identity_for(*index);
            let qual = class_qual
                .map(str::to_string)
                .or_else(|| vm.class_name_for_object(*index))
                .unwrap_or_else(|| "?".into());
            (format!("ref({qual})#{id}"), object_ref_key(id))
        }
        Value::Text(frame) => (format_text(frame), 0),
        Value::Array(index) if *index == usize::MAX => ("none".into(), 0),
        Value::Array(index) => {
            let ordinal = *index as i64;
            let summary = vm
                .arrays
                .get(*index)
                .map(format_array_summary)
                .unwrap_or_else(|| "array".into());
            (summary, REF_ARRAY_BASE + ordinal)
        }
        Value::RefI64(_) => ("<ref>".into(), 0),
        Value::FuncRef(name) => (format!("fn {name}"), 0),
        Value::SeqSystem(id) => (format!("seq.system#{id}"), 0),
        Value::SeqComponentHandle(id) => (format!("seq.component#{id}"), 0),
    }
}

fn format_array_summary(array: &ArrayStorage) -> String {
    let (bounds, n) = match array {
        ArrayStorage::I64 { bounds, cells, .. } => (bounds, cells.len()),
        ArrayStorage::F64 { bounds, cells } => (bounds, cells.len()),
        ArrayStorage::Text { bounds, cells } => (bounds, cells.len()),
        ArrayStorage::Free => return "array <collected>".into(),
    };
    let bounds: Vec<String> = bounds.iter().map(|(lo, hi)| format!("{lo}:{hi}")).collect();
    format!("array[{}] ({n} elem)", bounds.join(", "))
}

fn is_user_local_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('%') {
        return false;
    }
    if name.starts_with("__simrt_") {
        return false;
    }
    if name.contains('$') {
        // Keep `__this` (handled separately); skip thunks / captures.
        return name == "__this";
    }
    true
}

fn display_local_name(name: &str) -> String {
    if name == "__this" {
        "this".into()
    } else {
        name.to_string()
    }
}

fn span_covers(scope: &Span, pc: &Span) -> bool {
    pc.start < pc.end && pc.start >= scope.start && pc.end <= scope.end
}

fn span_contains(outer: &Span, inner: &Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

fn covering_procedures<'a>(function: &'a Function, pc: &Span) -> Vec<&'a DebugScope> {
    let mut scopes: Vec<_> = function
        .debug_scopes
        .iter()
        .filter(|scope| scope.kind == DebugScopeKind::Procedure && span_covers(&scope.span, pc))
        .collect();
    scopes.sort_by_key(|scope| std::cmp::Reverse(scope.span.end.saturating_sub(scope.span.start)));
    scopes
}

fn owner_procedure<'a>(
    function: &'a Function,
    local_scope: Option<&Span>,
) -> Option<&'a DebugScope> {
    let local_scope = local_scope?;
    function
        .debug_scopes
        .iter()
        .filter(|scope| {
            scope.kind == DebugScopeKind::Procedure && span_contains(&scope.span, local_scope)
        })
        .min_by_key(|scope| scope.span.end.saturating_sub(scope.span.start))
}

/// Inlined procedure / nested-block locals are tagged with [`Local::debug_scope`].
/// Show them only while the current statement sits inside that span.
fn local_visible_in_debug(local: &Local, pc: Option<&Span>) -> bool {
    match (local.debug_scope.as_ref(), pc) {
        (None, _) => true,
        (Some(scope), _) if scope.start >= scope.end => true,
        (Some(_), None) => true,
        (Some(scope), Some(pc)) => span_covers(scope, pc),
    }
}

/// Short DAP frame label: `Box$leaf` → `leaf`, `main` → `main`.
pub(super) fn debug_frame_name(mangled: &str) -> String {
    mangled
        .rsplit('$')
        .next()
        .unwrap_or(mangled)
        .trim_start_matches('_')
        .to_string()
}

fn fill_debug_frames(
    snap: &mut VariableSnapshot,
    function: &Function,
    locals: &[Value],
    pc: Option<&Span>,
) {
    let covering = pc
        .map(|pc| covering_procedures(function, pc))
        .unwrap_or_default();
    snap.innermost_procedure = covering
        .last()
        .map(|scope| (scope.name.clone(), scope.span.start, scope.span.end));
    let mut inactive = Vec::new();
    for scope in &function.debug_scopes {
        if scope.kind != DebugScopeKind::Procedure {
            continue;
        }
        let covers = pc.is_some_and(|pc| span_covers(&scope.span, pc));
        if !covers
            && !inactive
                .iter()
                .any(|name: &String| name.eq_ignore_ascii_case(&scope.name))
        {
            inactive.push(scope.name.clone());
        }
    }
    snap.inactive_procedures = inactive;

    let mut function_locals = Vec::new();
    let mut inline_locals: Vec<Vec<VarEntry>> = covering.iter().map(|_| Vec::new()).collect();
    for (id, value) in locals.iter().enumerate() {
        let local = function.local(crate::mir::LocalId(id));
        if !is_user_local_name(&local.name) || !local_visible_in_debug(local, pc) {
            continue;
        }
        if matches!(value, Value::None) && local.name != "__this" {
            continue;
        }
        let Some(entry) = snap
            .locals
            .iter()
            .find(|e| {
                e.name
                    .eq_ignore_ascii_case(&display_local_name(&local.name))
            })
            .cloned()
        else {
            continue;
        };
        match owner_procedure(function, local.debug_scope.as_ref()) {
            Some(owner) => {
                if let Some(index) = covering
                    .iter()
                    .position(|scope| std::ptr::eq(*scope, owner))
                {
                    upsert_local(&mut inline_locals[index], entry);
                }
            }
            None => upsert_local(&mut function_locals, entry),
        }
    }
    for entry in &snap.locals {
        let named = |list: &[VarEntry]| {
            list.iter()
                .any(|e| e.name.eq_ignore_ascii_case(&entry.name))
        };
        if named(&function_locals) || inline_locals.iter().any(|list| named(list)) {
            continue;
        }
        function_locals.push(entry.clone());
    }
    snap.function_locals = function_locals;
    snap.inline_frames = covering
        .iter()
        .zip(inline_locals)
        .map(|(scope, locals)| InlineFrameSnap {
            name: scope.name.clone(),
            locals,
        })
        .collect();
}

fn upsert_local(list: &mut Vec<VarEntry>, entry: VarEntry) {
    if let Some(existing) = list
        .iter_mut()
        .find(|e| e.name.eq_ignore_ascii_case(&entry.name))
    {
        *existing = entry;
    } else {
        list.push(entry);
    }
}

impl<'a> Vm<'a> {
    pub(super) fn debug_snapshot(&self) -> VariableSnapshot {
        let mut snap = VariableSnapshot::default();
        if self.frames.is_empty() {
            return snap;
        }
        let frame_index = self.frames.len() - 1;
        let function = &self.module.functions[self.frames[frame_index].function_index];
        let frame = &self.frames[frame_index];

        #[cfg(feature = "dap")]
        let pc = self.last_debug_span.as_ref();
        #[cfg(not(feature = "dap"))]
        let pc: Option<&Span> = None;

        let mut this_index: Option<usize> = None;
        for id in 0..frame.locals.len() {
            let local = function.local(crate::mir::LocalId(id));
            if !is_user_local_name(&local.name) || !local_visible_in_debug(local, pc) {
                continue;
            }
            let value = &frame.locals[id];
            if matches!(value, Value::None) && local.name != "__this" {
                continue;
            }
            if let Value::ObjectRef(index) = value {
                if local.name == "__this" {
                    this_index = Some(*index);
                }
                self.ensure_object_children(&mut snap, *index);
            }
            if let Value::Array(index) = value
                && *index != usize::MAX
            {
                self.ensure_array_children(&mut snap, *index);
            }
            let (text, variables_reference) =
                format_mir_value(self, value, local.ty, local.class_qual.as_deref());
            let name = display_local_name(&local.name);
            if let Some(existing) = snap
                .locals
                .iter_mut()
                .find(|e| e.name.eq_ignore_ascii_case(&name))
            {
                // Several inlined activations share a name; keep the later
                // initialized slot (I64(0) is still the frame zero-fill).
                if !matches!(value, Value::I64(0)) {
                    *existing = VarEntry {
                        name,
                        value: text,
                        variables_reference,
                    };
                }
                continue;
            }
            snap.locals.push(VarEntry {
                name,
                value: text,
                variables_reference,
            });
        }

        // Surface current-object attributes as locals (matches AST DAP).
        if let Some(index) = this_index {
            self.push_this_fields_as_locals(&mut snap, index);
        }

        snap.locals
            .sort_by_key(|a| (a.name != "this", a.name.to_ascii_lowercase()));

        fill_debug_frames(&mut snap, function, &frame.locals, pc);

        snap.threads.push(ThreadInfo {
            id: 1,
            name: "main".into(),
            resume_summary: None,
        });
        for (comp_id, comp) in self.seq_components.iter().enumerate() {
            if !comp.is_detached() || comp.block_instance {
                continue;
            }
            let index = comp.object;
            let id = identity_for(index);
            let class = self
                .class_name_for_object(index)
                .unwrap_or_else(|| "object".into());
            snap.threads.push(ThreadInfo {
                id: 10 + comp_id as i64,
                name: format!("detached {class}#{id}"),
                resume_summary: Some(format!("parked component #{comp_id}")),
            });
            self.ensure_object_children(&mut snap, index);
        }

        if self.sim.active {
            self.fill_simulation(&mut snap);
        }

        snap
    }

    fn class_name_for_object(&self, index: usize) -> Option<String> {
        let class_id = self.load_object_i64(index, 0).ok()?;
        self.module
            .class_layouts
            .iter()
            .find(|layout| layout.class_id == class_id)
            .map(|layout| layout.declared_name.clone())
    }

    fn layout_for_object(&self, index: usize) -> Option<&crate::layout::ClassLayout> {
        let class_id = self.load_object_i64(index, 0).ok()?;
        self.module
            .class_layouts
            .iter()
            .find(|layout| layout.class_id == class_id)
    }

    fn field_type_to_mir(ty: FieldType) -> MirType {
        match ty {
            FieldType::I64 => MirType::I64,
            FieldType::Bool => MirType::Bool,
            FieldType::F64 => MirType::F64,
            FieldType::Text => MirType::Text,
            FieldType::ObjectRef => MirType::ObjectRef,
            FieldType::ArrayI64 | FieldType::ArrayBool => MirType::ArrayI64,
            FieldType::ArrayF64 => MirType::ArrayF64,
            FieldType::ArrayText => MirType::ArrayText,
        }
    }

    fn decode_field(&self, object: usize, field: &FieldLayout) -> Option<Value> {
        let raw = self.load_object_i64(object, field.offset).ok()?;
        let ty = Self::field_type_to_mir(field.ty);
        self.i64_to_value(ty, raw).ok()
    }

    fn ensure_object_children(&self, snap: &mut VariableSnapshot, index: usize) {
        let id = identity_for(index);
        let key = object_ref_key(id);
        if snap.children.contains_key(&key) {
            return;
        }
        let Some(layout) = self.layout_for_object(index) else {
            snap.children.insert(key, Vec::new());
            return;
        };
        let mut nested = Vec::new();
        let mut entries = Vec::new();
        for field in &layout.fields {
            if field.name.eq_ignore_ascii_case(SIMSET_SUC_FIELD)
                || field.name.eq_ignore_ascii_case(SIMSET_PRED_FIELD)
                || field.name.starts_with("__simrt_")
            {
                continue;
            }
            let Some(value) = self.decode_field(index, field) else {
                continue;
            };
            if let Value::ObjectRef(child) = &value {
                nested.push(*child);
            }
            if let Value::Array(arr) = &value
                && *arr != usize::MAX
            {
                self.ensure_array_children(snap, *arr);
            }
            let (text, variables_reference) = format_mir_value(
                self,
                &value,
                Self::field_type_to_mir(field.ty),
                field.class_qual.as_deref(),
            );
            entries.push(VarEntry {
                name: field.name.clone(),
                value: text,
                variables_reference,
            });
        }
        snap.children.insert(key, entries);
        for child in nested {
            self.ensure_object_children(snap, child);
        }
    }

    fn push_this_fields_as_locals(&self, snap: &mut VariableSnapshot, index: usize) {
        let Some(layout) = self.layout_for_object(index) else {
            return;
        };
        for field in &layout.fields {
            if field.name.eq_ignore_ascii_case(SIMSET_SUC_FIELD)
                || field.name.eq_ignore_ascii_case(SIMSET_PRED_FIELD)
                || field.name.starts_with("__simrt_")
            {
                continue;
            }
            if snap
                .locals
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case(&field.name))
            {
                continue;
            }
            let Some(value) = self.decode_field(index, field) else {
                continue;
            };
            if matches!(value, Value::None) {
                continue;
            }
            if let Value::ObjectRef(child) = &value {
                self.ensure_object_children(snap, *child);
            }
            if let Value::Array(arr) = &value
                && *arr != usize::MAX
            {
                self.ensure_array_children(snap, *arr);
            }
            let (text, variables_reference) = format_mir_value(
                self,
                &value,
                Self::field_type_to_mir(field.ty),
                field.class_qual.as_deref(),
            );
            snap.locals.push(VarEntry {
                name: field.name.clone(),
                value: text,
                variables_reference,
            });
        }
    }

    fn ensure_array_children(&self, snap: &mut VariableSnapshot, index: usize) {
        let key = REF_ARRAY_BASE + index as i64;
        if snap.children.contains_key(&key) {
            return;
        }
        let Some(array) = self.arrays.get(index) else {
            snap.children.insert(key, Vec::new());
            return;
        };
        const MAX: usize = 64;
        let mut children = Vec::new();
        match array {
            ArrayStorage::I64 { cells, .. } => {
                let mut elems: Vec<_> = cells.iter().collect();
                elems.sort_by(|a, b| a.0.cmp(b.0));
                let total = elems.len();
                for (idx, raw) in elems.into_iter().take(MAX) {
                    let label = format!(
                        "[{}]",
                        idx.iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    children.push(VarEntry {
                        name: label,
                        value: raw.to_string(),
                        variables_reference: 0,
                    });
                }
                if total > MAX {
                    children.push(VarEntry {
                        name: "…".into(),
                        value: format!("{} more", total - MAX),
                        variables_reference: 0,
                    });
                }
            }
            ArrayStorage::F64 { cells, .. } => {
                let mut elems: Vec<_> = cells.iter().collect();
                elems.sort_by(|a, b| a.0.cmp(b.0));
                for (idx, raw) in elems.into_iter().take(MAX) {
                    let label = format!(
                        "[{}]",
                        idx.iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    children.push(VarEntry {
                        name: label,
                        value: format!("{raw}"),
                        variables_reference: 0,
                    });
                }
            }
            ArrayStorage::Text { cells, .. } => {
                let mut elems: Vec<_> = cells.iter().collect();
                elems.sort_by(|a, b| a.0.cmp(b.0));
                for (idx, frame) in elems.into_iter().take(MAX) {
                    let label = format!(
                        "[{}]",
                        idx.iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    children.push(VarEntry {
                        name: label,
                        value: format_text(frame),
                        variables_reference: 0,
                    });
                }
            }
            ArrayStorage::Free => {}
        }
        snap.children.insert(key, children);
    }

    fn fill_simulation(&self, snap: &mut VariableSnapshot) {
        snap.has_simulation = true;
        let time = self.sim.sqs.first().map(|n| n.evtime).unwrap_or(0.0);
        let current_id = self.sim.current.map(identity_for).unwrap_or(0);
        let current_name = self
            .sim
            .current
            .and_then(|idx| self.class_name_for_object(idx))
            .unwrap_or_else(|| "?".into());
        let main_id = self.sim.main_object.map(identity_for).unwrap_or(0);
        snap.children.insert(
            REF_SIMULATION,
            vec![
                VarEntry {
                    name: "time".into(),
                    value: format!("{time}"),
                    variables_reference: 0,
                },
                VarEntry {
                    name: "current".into(),
                    value: format!("{current_name}#{current_id}"),
                    variables_reference: 0,
                },
                VarEntry {
                    name: "main".into(),
                    value: format!("MAIN#{main_id}"),
                    variables_reference: 0,
                },
                VarEntry {
                    name: "sqs".into(),
                    value: format!("{} event(s)", self.sim.sqs.len()),
                    variables_reference: REF_SQS,
                },
            ],
        );
        let sqs_entries: Vec<VarEntry> = self
            .sim
            .sqs
            .iter()
            .enumerate()
            .map(|(i, event)| {
                let name = self
                    .class_name_for_object(event.process)
                    .unwrap_or_else(|| "process".into());
                let id = identity_for(event.process);
                VarEntry {
                    name: format!("[{i}]"),
                    value: format!("t={} {name}#{id}", event.evtime),
                    variables_reference: 0,
                }
            })
            .collect();
        snap.children.insert(REF_SQS, sqs_entries);
    }

    /// DAP `setVariable` against the current MIR frame / object fields.
    pub(super) fn debug_set_variable(
        &mut self,
        name: &str,
        variables_reference: i64,
        value_text: &str,
    ) -> Result<VarEntry, String> {
        let literal = parse_debug_value(value_text)?;
        let mir_value = literal_to_mir(&literal)?;
        if variables_reference == REF_LOCALS || variables_reference == 0 {
            self.debug_set_local(name, mir_value.clone())?;
        } else if variables_reference >= REF_OBJECT_BASE {
            let identity = (variables_reference - REF_OBJECT_BASE) as u64;
            let index = identity
                .checked_sub(1)
                .ok_or_else(|| format!("invalid object identity #{identity}"))?
                as usize;
            self.debug_set_object_field(index, name, mir_value.clone())?;
        } else {
            return Err("cannot set variables in this scope".into());
        }
        Ok(VarEntry {
            name: name.to_string(),
            value: literal.display(),
            variables_reference: match &mir_value {
                Value::ObjectRef(index) => object_ref_key(identity_for(*index)),
                _ => 0,
            },
        })
    }

    fn debug_set_local(&mut self, name: &str, value: Value) -> Result<(), String> {
        if self.frames.is_empty() {
            return Err("no active frame".into());
        }
        let frame_index = self.frames.len() - 1;
        let function_index = self.frames[frame_index].function_index;
        let function = &self.module.functions[function_index];

        #[cfg(feature = "dap")]
        let pc = self.last_debug_span.as_ref();
        #[cfg(not(feature = "dap"))]
        let pc: Option<&Span> = None;

        // Prefer a matching local / param slot currently in Simula scope.
        let mut chosen: Option<usize> = None;
        for id in 0..self.frames[frame_index].locals.len() {
            let local = function.local(crate::mir::LocalId(id));
            let display = display_local_name(&local.name);
            if display.eq_ignore_ascii_case(name)
                && is_user_local_name(&local.name)
                && local_visible_in_debug(local, pc)
            {
                if local.name == "__this" {
                    return Err("cannot assign to `this`".into());
                }
                chosen = Some(id);
            }
        }
        if let Some(id) = chosen {
            self.frames[frame_index].set_local(crate::mir::LocalId(id), value);
            return Ok(());
        }

        // Attribute of `__this` surfaced as a local.
        if let Some(index) = self.current_this_index() {
            return self.debug_set_object_field(index, name, value);
        }
        Err(format!("unknown local `{name}`"))
    }

    fn current_this_index(&self) -> Option<usize> {
        if self.frames.is_empty() {
            return None;
        }
        let frame_index = self.frames.len() - 1;
        let function = &self.module.functions[self.frames[frame_index].function_index];
        for id in 0..self.frames[frame_index].locals.len() {
            let local = function.local(crate::mir::LocalId(id));
            if local.name == "__this"
                && let Value::ObjectRef(index) = &self.frames[frame_index].locals[id]
            {
                return Some(*index);
            }
        }
        None
    }

    fn debug_set_object_field(
        &mut self,
        index: usize,
        name: &str,
        value: Value,
    ) -> Result<(), String> {
        if name.starts_with("__simrt_") {
            return Err("cannot modify runtime internal fields".into());
        }
        let layout = self
            .layout_for_object(index)
            .ok_or_else(|| format!("unknown object #{}", identity_for(index)))?;
        let field = layout
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("unknown field `{name}` on object #{}", identity_for(index)))?
            .clone();
        if field.name.eq_ignore_ascii_case(SIMSET_SUC_FIELD)
            || field.name.eq_ignore_ascii_case(SIMSET_PRED_FIELD)
        {
            return Err("cannot modify SIMSET link fields".into());
        }
        let ty = Self::field_type_to_mir(field.ty);
        let (raw, tag) = self.value_to_i64(ty, &value).map_err(|e| e.to_string())?;
        self.store_object_i64(index, field.offset, raw, tag)
            .map_err(|e| e.to_string())
    }
}

fn literal_to_mir(value: &DebugLiteral) -> Result<Value, String> {
    match value {
        DebugLiteral::Integer(n) => Ok(Value::I64(*n)),
        DebugLiteral::Real(n) => Ok(Value::F64(*n)),
        DebugLiteral::Boolean(b) => Ok(Value::Bool(*b)),
        DebugLiteral::Character(c) => Ok(Value::I64(*c as i64)),
        DebugLiteral::Text(frame) => Ok(Value::Text(frame.clone())),
        DebugLiteral::None => Ok(Value::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_uninit_i64_zero_displays_simula_defaults() {
        assert_eq!(
            typed_uninit_display(MirType::Bool, &Value::I64(0)).map(|(s, _)| s),
            Some("false".into())
        );
        assert_eq!(
            typed_uninit_display(MirType::ObjectRef, &Value::I64(0)).map(|(s, _)| s),
            Some("none".into())
        );
        assert_eq!(
            typed_uninit_display(MirType::Text, &Value::I64(0)).map(|(s, _)| s),
            Some("notext".into())
        );
        assert_eq!(
            typed_uninit_display(MirType::F64, &Value::I64(0)).map(|(s, _)| s),
            Some("0.0".into())
        );
        assert_eq!(
            typed_uninit_display(MirType::ArrayI64, &Value::I64(0)).map(|(s, _)| s),
            Some("none".into())
        );
    }
}
