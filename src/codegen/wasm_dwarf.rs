//! Minimal DWARF-in-wasm for `-g` wasm builds.
//!
//! Reuses the same wasm PC↔span markers collected for Source Map v3 (see
//! [`crate::codegen::wasm`]) to build a single DWARF compilation unit with one
//! `DW_TAG_subprogram` + line-program sequence per MIR function, then emits
//! the resulting gimli sections (`.debug_info`, `.debug_abbrev`,
//! `.debug_line`, `.debug_str`, ...) as wasm custom sections.
//!
//! Named parameters and locals (excluding synthetic `%tN` temps) get
//! `DW_TAG_formal_parameter` / `DW_TAG_variable` DIEs with coarse base types
//! (and `DW_TAG_pointer_type` to a class `DW_TAG_structure_type` when the
//! local's MIR `class_qual` is known) and
//! `DW_AT_location = DW_OP_WASM_location 0x00 <local_index>` (wasm local
//! index matches [`crate::mir::LocalId`], see `wasm::local_index`).
//! Class layouts from MIR become CU-level structure DIEs (header + members),
//! mirroring the native `-g` path in [`crate::codegen::dwarf`].
//!
//! WebAssembly DWARF code addresses (`DW_AT_low_pc`/`DW_AT_high_pc`, and the
//! `.debug_line` instruction pointers) are *Code section-relative* byte
//! offsets, not absolute file offsets or linear-memory addresses — see the
//! tool-conventions spec:
//! <https://github.com/WebAssembly/tool-conventions/blob/main/Dwarf.md>.
//! Callers are responsible for converting the absolute file offsets used by
//! the Source Map v3 side (see `wasm::code_section_layout`) into Code
//! section-relative offsets before building [`WasmFunctionDebug`] values.
//!
//! This is intentionally a small, dedicated encoder rather than a reuse of
//! `codegen::dwarf::DebugContext`: the native path relocates addresses
//! against object-file symbols (via `gimli::write::Address::Symbol`) and
//! uses frame-pointer locations that don't apply here, whereas every
//! address on the wasm side is already a known constant by the time we build
//! debug sections (the module bytes are final), so a plain `Address::Constant`
//! + `DW_OP_WASM_location` suffices.

use std::collections::HashMap;

use gimli::write::{
    Address, AttributeValue, DwarfUnit, EndianVec, Expression, LineProgram, LineString, Sections,
    UnitEntryId,
};
use gimli::{Encoding, Format, LineEncoding, RunTimeEndian};

use crate::layout::{ClassLayout, FieldType, OBJECT_HEADER_SIZE};
use crate::mir::MirType;
use crate::source::SourceFile;

/// One MIR function's DWARF-relevant data: a Code section-relative `[low_pc,
/// high_pc)` range plus per-instruction `(offset, line, column)` rows for the
/// line program, sorted by (non-decreasing) offset.
pub struct WasmFunctionDebug {
    pub name: String,
    pub low_pc: u32,
    pub high_pc: u32,
    /// `DW_AT_decl_line` for the subprogram DIE (first mapped source line,
    /// or `1` if the function has no markers).
    pub default_line: u64,
    /// `(code_section_relative_offset, line, column)`, one per body marker.
    pub rows: Vec<(u32, u64, u64)>,
    /// User-visible params/locals (synthetic `%…` names omitted by the caller).
    pub locals: Vec<WasmLocalDebug>,
}

/// One named MIR local/parameter for wasm DWARF.
pub struct WasmLocalDebug {
    pub name: String,
    pub ty: MirType,
    /// Declared class name for [`MirType::ObjectRef`] locals (`ref(C)`).
    pub class_qual: Option<String>,
    /// Wasm local index (`LocalId.0`).
    pub wasm_local: u32,
    pub is_param: bool,
}

/// Builds minimal DWARF sections (one CU, class structure DIEs, one
/// `DW_TAG_subprogram` + line sequence per function) and returns them as
/// `(section_name, payload)` pairs — e.g. `(".debug_line", ...)` — ready to
/// append as wasm custom sections. Returns an empty `Vec` if gimli fails to
/// encode (never expected for this minimal shape, but callers should treat
/// DWARF as best-effort).
pub fn build_debug_sections(
    source: &SourceFile,
    functions: &[WasmFunctionDebug],
    class_layouts: &[ClassLayout],
) -> Vec<(String, Vec<u8>)> {
    // DWARF 4 keeps the line-program / file-table encoding simple (DWARF 5
    // reshuffles the file/directory tables); wasm consumers accept either.
    // wasm32 code offsets fit comfortably in 4 bytes.
    let encoding = Encoding {
        format: Format::Dwarf32,
        version: 4,
        address_size: 4,
    };

    let mut dwarf = DwarfUnit::new(encoding);

    let file_name = source_file_name(source);
    let comp_dir = source_comp_dir(source);

    let mut line_program = LineProgram::new(
        encoding,
        LineEncoding::default(),
        LineString::new(comp_dir.as_bytes(), encoding, &mut dwarf.line_strings),
        None,
        LineString::new(file_name.as_bytes(), encoding, &mut dwarf.line_strings),
        None,
    );
    line_program.file_has_md5 = false;
    let dir_id = line_program.default_directory();
    let file_id = line_program.add_file(
        LineString::new(file_name.as_bytes(), encoding, &mut dwarf.line_strings),
        dir_id,
        None,
    );
    dwarf.unit.line_program = line_program;

    let producer = format!("sim {}", env!("CARGO_PKG_VERSION"));
    let root = dwarf.unit.root();
    let module_high_pc = functions.iter().map(|f| f.high_pc).max().unwrap_or(0);
    {
        let name_ref = dwarf.strings.add(file_name.as_str());
        let comp_dir_ref = dwarf.strings.add(comp_dir.as_str());
        let producer_ref = dwarf.strings.add(producer);
        let root_entry = dwarf.unit.get_mut(root);
        root_entry.set(
            gimli::DW_AT_producer,
            AttributeValue::StringRef(producer_ref),
        );
        root_entry.set(
            gimli::DW_AT_language,
            AttributeValue::Language(gimli::DW_LANG_C),
        );
        root_entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_ref));
        root_entry.set(
            gimli::DW_AT_comp_dir,
            AttributeValue::StringRef(comp_dir_ref),
        );
        root_entry.set(
            gimli::DW_AT_low_pc,
            AttributeValue::Address(Address::Constant(0)),
        );
        root_entry.set(
            gimli::DW_AT_high_pc,
            AttributeValue::Udata(u64::from(module_high_pc)),
        );
    }

    let mut types = BaseTypes::emit(&mut dwarf);
    emit_class_types(&mut dwarf, &mut types, class_layouts);

    for function in functions {
        add_subprogram(&mut dwarf, root, file_id, function, &types);
    }

    let mut sections = Sections::new(EndianVec::new(RunTimeEndian::Little));
    if dwarf.write(&mut sections).is_err() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let _ = sections.for_each_mut(|id, section| {
        if !section.slice().is_empty() {
            out.push((id.name().to_string(), section.take()));
        }
        Ok::<(), gimli::write::Error>(())
    });
    out
}

struct BaseTypes {
    i64: UnitEntryId,
    i32: UnitEntryId,
    bool_slot: UnitEntryId,
    f64: UnitEntryId,
    pointer: UnitEntryId,
    /// Wasm `SimrtTextFrame*` (16-byte frame: ptr/len/pos/pad).
    text: UnitEntryId,
    array_i64: UnitEntryId,
    array_text: UnitEntryId,
    /// Class name (ASCII-lower) → `DW_TAG_pointer_type` to the class struct.
    class_pointer_types: HashMap<String, UnitEntryId>,
}

impl BaseTypes {
    fn emit(dwarf: &mut DwarfUnit) -> Self {
        let pointer = add_base_type(dwarf, "pointer", gimli::DW_ATE_address, 4);
        let mut types = Self {
            i64: add_base_type(dwarf, "i64", gimli::DW_ATE_signed, 8),
            i32: add_base_type(dwarf, "i32", gimli::DW_ATE_signed, 4),
            // Wasm stores booleans in i64 slots.
            bool_slot: add_base_type(dwarf, "boolean8", gimli::DW_ATE_boolean, 8),
            f64: add_base_type(dwarf, "real", gimli::DW_ATE_float, 8),
            // Wasm object refs / pointers are i64 locals holding i32 addresses.
            pointer,
            text: pointer,
            array_i64: pointer,
            array_text: pointer,
            class_pointer_types: HashMap::new(),
        };
        types.emit_runtime_types(dwarf);
        types
    }

    fn emit_runtime_types(&mut self, dwarf: &mut DwarfUnit) {
        let root = dwarf.unit.root();
        // Wasm text frame: { ptr, len, pos, pad } — 4×i32 = 16 bytes.
        let frame = dwarf.unit.add(root, gimli::DW_TAG_structure_type);
        {
            let name_id = dwarf.strings.add("SimrtTextFrame");
            let entry = dwarf.unit.get_mut(frame);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
            entry.set(gimli::DW_AT_byte_size, AttributeValue::Udata(16));
        }
        emit_struct_member(dwarf, frame, "ptr", 0, self.pointer);
        emit_struct_member(dwarf, frame, "len", 4, self.i32);
        emit_struct_member(dwarf, frame, "pos", 8, self.i32);
        emit_struct_member(dwarf, frame, "pad", 12, self.i32);
        let text_ptr = dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
        {
            let entry = dwarf.unit.get_mut(text_ptr);
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(frame));
        }
        self.text = text_ptr;
        self.array_i64 = emit_array_descriptor(dwarf, "SimrtArrayI64", self.i64);
        self.array_text = emit_array_descriptor(dwarf, "SimrtArrayText", self.i64);
    }

    fn for_local(&self, local: &WasmLocalDebug) -> UnitEntryId {
        match local.ty {
            MirType::I64 => self.i64,
            MirType::Bool => self.bool_slot,
            MirType::F64 | MirType::LongF64 => self.f64,
            MirType::Text => self.text,
            MirType::ArrayI64 | MirType::ArrayF64 => self.array_i64,
            MirType::ArrayText => self.array_text,
            MirType::ObjectRef => {
                if let Some(qual) = &local.class_qual
                    && let Some(&id) = self.class_pointer_types.get(&qual.to_ascii_lowercase())
                {
                    return id;
                }
                self.pointer
            }
            MirType::RefI64 | MirType::FuncRef => self.pointer,
        }
    }

    fn for_field(&self, field: &crate::layout::FieldLayout) -> UnitEntryId {
        match field.ty {
            FieldType::I64 => self.i64,
            FieldType::Bool => self.bool_slot,
            FieldType::F64 => self.f64,
            FieldType::Text => self.text,
            FieldType::ObjectRef
            | FieldType::ArrayI64
            | FieldType::ArrayBool
            | FieldType::ArrayF64
            | FieldType::ArrayText => {
                if let Some(qual) = &field.class_qual
                    && let Some(&id) = self.class_pointer_types.get(&qual.to_ascii_lowercase())
                {
                    return id;
                }
                self.pointer
            }
        }
    }
}

fn emit_array_descriptor(dwarf: &mut DwarfUnit, name: &str, i64_ty: UnitEntryId) -> UnitEntryId {
    let root = dwarf.unit.root();
    let struct_id = dwarf.unit.add(root, gimli::DW_TAG_structure_type);
    {
        let name_id = dwarf.strings.add(name);
        let entry = dwarf.unit.get_mut(struct_id);
        entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
        entry.set(gimli::DW_AT_byte_size, AttributeValue::Udata(8));
    }
    emit_struct_member(dwarf, struct_id, "ndims", 0, i64_ty);
    let ptr_id = dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
    {
        let entry = dwarf.unit.get_mut(ptr_id);
        entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(struct_id));
    }
    ptr_id
}

/// Emits one `DW_TAG_structure_type` (+ pointer type) per class layout.
fn emit_class_types(dwarf: &mut DwarfUnit, types: &mut BaseTypes, layouts: &[ClassLayout]) {
    let root = dwarf.unit.root();
    let mut struct_ids = Vec::with_capacity(layouts.len());
    for layout in layouts {
        let struct_id = dwarf.unit.add(root, gimli::DW_TAG_structure_type);
        let name_id = dwarf.strings.add(layout.name.as_str());
        {
            let entry = dwarf.unit.get_mut(struct_id);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
            entry.set(
                gimli::DW_AT_byte_size,
                AttributeValue::Udata(layout.size as u64),
            );
        }
        let ptr_id = dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
        {
            let entry = dwarf.unit.get_mut(ptr_id);
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(struct_id));
        }
        types
            .class_pointer_types
            .insert(layout.name.to_ascii_lowercase(), ptr_id);
        struct_ids.push(struct_id);
    }
    for (layout, &struct_id) in layouts.iter().zip(struct_ids.iter()) {
        emit_struct_member(dwarf, struct_id, "__class_id", 0, types.i64);
        for field in &layout.fields {
            let field_ty = types.for_field(field);
            debug_assert!(
                field.offset >= OBJECT_HEADER_SIZE,
                "field offsets start after the object header"
            );
            emit_struct_member(dwarf, struct_id, &field.name, field.offset as u64, field_ty);
        }
    }
}

fn emit_struct_member(
    dwarf: &mut DwarfUnit,
    struct_id: UnitEntryId,
    name: &str,
    offset: u64,
    ty: UnitEntryId,
) {
    let member_id = dwarf.unit.add(struct_id, gimli::DW_TAG_member);
    let name_id = dwarf.strings.add(name);
    let entry = dwarf.unit.get_mut(member_id);
    entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
    entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(ty));
    entry.set(
        gimli::DW_AT_data_member_location,
        AttributeValue::Udata(offset),
    );
}

fn add_base_type(
    dwarf: &mut DwarfUnit,
    name: &str,
    encoding: gimli::DwAte,
    size: u8,
) -> UnitEntryId {
    let root = dwarf.unit.root();
    let entry_id = dwarf.unit.add(root, gimli::DW_TAG_base_type);
    let name_id = dwarf.strings.add(name);
    let entry = dwarf.unit.get_mut(entry_id);
    entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
    entry.set(gimli::DW_AT_encoding, AttributeValue::Encoding(encoding));
    entry.set(
        gimli::DW_AT_byte_size,
        AttributeValue::Udata(u64::from(size)),
    );
    entry_id
}

fn add_subprogram(
    dwarf: &mut DwarfUnit,
    root: gimli::write::UnitEntryId,
    file_id: gimli::write::FileId,
    function: &WasmFunctionDebug,
    types: &BaseTypes,
) {
    let entry_id = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
    let name_id = dwarf.strings.add(function.name.as_str());
    {
        let entry = dwarf.unit.get_mut(entry_id);
        entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
        entry.set(
            gimli::DW_AT_low_pc,
            AttributeValue::Address(Address::Constant(u64::from(function.low_pc))),
        );
        entry.set(
            gimli::DW_AT_high_pc,
            AttributeValue::Udata(u64::from(function.high_pc.saturating_sub(function.low_pc))),
        );
        entry.set(
            gimli::DW_AT_decl_file,
            AttributeValue::FileIndex(Some(file_id)),
        );
        entry.set(
            gimli::DW_AT_decl_line,
            AttributeValue::Udata(function.default_line),
        );
    }

    for local in &function.locals {
        add_local(
            dwarf,
            entry_id,
            file_id,
            function.default_line,
            local,
            types,
        );
    }

    if function.rows.is_empty() {
        return;
    }

    let low_pc = function.low_pc;
    dwarf
        .unit
        .line_program
        .begin_sequence(Some(Address::Constant(u64::from(low_pc))));
    for &(offset, line, column) in &function.rows {
        let row = dwarf.unit.line_program.row();
        row.address_offset = u64::from(offset.saturating_sub(low_pc));
        row.file = file_id;
        row.line = line;
        row.column = column;
        dwarf.unit.line_program.generate_row();
    }
    dwarf
        .unit
        .line_program
        .end_sequence(u64::from(function.high_pc.saturating_sub(low_pc)));
}

fn add_local(
    dwarf: &mut DwarfUnit,
    subprogram: UnitEntryId,
    file_id: gimli::write::FileId,
    decl_line: u64,
    local: &WasmLocalDebug,
    types: &BaseTypes,
) {
    let tag = if local.is_param {
        gimli::DW_TAG_formal_parameter
    } else {
        gimli::DW_TAG_variable
    };
    let entry_id = dwarf.unit.add(subprogram, tag);
    let name_id = dwarf.strings.add(local.name.as_str());
    let type_id = types.for_local(local);
    let mut loc = Expression::new();
    loc.op_wasm_local(local.wasm_local);
    {
        let entry = dwarf.unit.get_mut(entry_id);
        entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
        entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(type_id));
        entry.set(
            gimli::DW_AT_decl_file,
            AttributeValue::FileIndex(Some(file_id)),
        );
        entry.set(gimli::DW_AT_decl_line, AttributeValue::Udata(decl_line));
        entry.set(gimli::DW_AT_location, AttributeValue::Exprloc(loc));
    }
}

fn source_file_name(source: &SourceFile) -> String {
    if source.name == "<input>" {
        "input.sim".into()
    } else {
        std::path::Path::new(&source.name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.name.clone())
    }
}

fn source_comp_dir(source: &SourceFile) -> String {
    if source.name == "<input>" {
        return ".".into();
    }
    std::path::Path::new(&source.name)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.display().to_string())
        .unwrap_or_else(|| ".".into())
}

/// Whether a MIR local name should appear in wasm DWARF (matches native).
pub fn is_user_local_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('%')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FieldLayout, FieldType};

    fn source(text: &str) -> SourceFile {
        SourceFile::anonymous(text)
    }

    #[test]
    fn emits_debug_line_info_and_abbrev_with_non_empty_payloads() {
        let src = source("begin OutText(\"hi\"); OutImage; end;");
        let functions = vec![WasmFunctionDebug {
            name: "main".into(),
            low_pc: 2,
            high_pc: 40,
            default_line: 1,
            rows: vec![(2, 1, 7), (10, 1, 20), (30, 1, 33)],
            locals: Vec::new(),
        }];
        let sections = build_debug_sections(&src, &functions, &[]);
        let names: Vec<&str> = sections.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&".debug_line"), "{names:?}");
        assert!(names.contains(&".debug_info"), "{names:?}");
        assert!(names.contains(&".debug_abbrev"), "{names:?}");
        for (name, payload) in &sections {
            assert!(
                !payload.is_empty(),
                "{name} should have a non-empty payload"
            );
        }
    }

    #[test]
    fn functions_without_markers_still_get_a_subprogram_but_no_sequence() {
        let src = source("begin end;");
        let functions = vec![WasmFunctionDebug {
            name: "main".into(),
            low_pc: 2,
            high_pc: 4,
            default_line: 1,
            rows: Vec::new(),
            locals: Vec::new(),
        }];
        let sections = build_debug_sections(&src, &functions, &[]);
        assert!(
            sections
                .iter()
                .any(|(name, payload)| name == ".debug_info" && !payload.is_empty())
        );
    }

    #[test]
    fn emits_named_local_with_wasm_location() {
        let src = source("begin integer x; x := 1; end;");
        let functions = vec![WasmFunctionDebug {
            name: "main".into(),
            low_pc: 2,
            high_pc: 20,
            default_line: 1,
            rows: vec![(2, 1, 1)],
            locals: vec![WasmLocalDebug {
                name: "x".into(),
                ty: MirType::I64,
                class_qual: None,
                wasm_local: 0,
                is_param: false,
            }],
        }];
        let sections = build_debug_sections(&src, &functions, &[]);
        assert!(!sections.is_empty(), "expected non-empty DWARF sections");
        // Smoke: the local name must appear in .debug_str.
        let debug_str = sections
            .iter()
            .find(|(name, _)| name == ".debug_str")
            .map(|(_, data)| data.as_slice())
            .unwrap_or(&[]);
        assert!(
            debug_str.windows(2).any(|w| w == b"x\0") || debug_str.contains(&b'x'),
            "expected 'x' in .debug_str: {debug_str:?}"
        );

        let debug_info = sections
            .iter()
            .find(|(name, _)| name == ".debug_info")
            .map(|(_, data)| data.clone())
            .unwrap_or_default();
        let debug_abbrev = sections
            .iter()
            .find(|(name, _)| name == ".debug_abbrev")
            .map(|(_, data)| data.clone())
            .unwrap_or_default();
        let debug_str = debug_str.to_vec();
        let debug_line = sections
            .iter()
            .find(|(name, _)| name == ".debug_line")
            .map(|(_, data)| data.clone())
            .unwrap_or_default();

        let endian = RunTimeEndian::Little;
        let load_section =
            |id: gimli::SectionId| -> Result<gimli::EndianSlice<RunTimeEndian>, gimli::Error> {
                let data: &[u8] = match id {
                    gimli::SectionId::DebugInfo => &debug_info,
                    gimli::SectionId::DebugAbbrev => &debug_abbrev,
                    gimli::SectionId::DebugStr => &debug_str,
                    gimli::SectionId::DebugLine => &debug_line,
                    _ => &[],
                };
                Ok(gimli::EndianSlice::new(data, endian))
            };
        let dwarf = gimli::Dwarf::load(load_section).expect("load dwarf");
        let mut found = false;
        let mut units = dwarf.units();
        while let Some(header) = units.next().expect("unit header") {
            let unit = match dwarf.unit(header) {
                Ok(unit) => unit,
                Err(error) => panic!(
                    "unit parse failed: {error:?}; info_len={} abbrev_len={} str_len={}",
                    debug_info.len(),
                    debug_abbrev.len(),
                    debug_str.len()
                ),
            };
            let mut entries = unit.entries();
            while let Some(entry) = entries.next_dfs().expect("entry") {
                if entry.tag() != gimli::DW_TAG_variable {
                    continue;
                }
                let Some(name_attr) = entry.attr_value(gimli::DW_AT_name) else {
                    continue;
                };
                let name = dwarf
                    .attr_string(&unit, name_attr)
                    .expect("name")
                    .to_string_lossy();
                if name == "x" {
                    assert!(
                        entry.attr_value(gimli::DW_AT_location).is_some(),
                        "x should have DW_AT_location"
                    );
                    assert!(
                        entry.attr_value(gimli::DW_AT_type).is_some(),
                        "x should have DW_AT_type"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "expected DW_TAG_variable named x");
    }

    #[test]
    fn emits_class_structure_type_with_members() {
        let src = source("begin class Point; begin integer x, y; end; end;");
        let layouts = vec![ClassLayout {
            name: "Point".into(),
            declared_name: "Point".into(),
            decl_span: 0..0,
            system_block: 0,
            fields: vec![
                FieldLayout {
                    name: "x".into(),
                    offset: 8,
                    size: 8,
                    ty: FieldType::I64,
                    class_qual: None,
                },
                FieldLayout {
                    name: "y".into(),
                    offset: 16,
                    size: 8,
                    ty: FieldType::I64,
                    class_qual: None,
                },
            ],
            methods: Vec::new(),
            virtual_methods: Vec::new(),
            constructor_params: Vec::new(),
            needs_init: false,
            runs_on_own_stack: false,
            enclosing_captures: Vec::new(),
            size: 24,
            class_id: 0,
            prefix: None,
        }];
        let functions = vec![WasmFunctionDebug {
            name: "main".into(),
            low_pc: 2,
            high_pc: 10,
            default_line: 1,
            rows: Vec::new(),
            locals: vec![WasmLocalDebug {
                name: "p".into(),
                ty: MirType::ObjectRef,
                class_qual: Some("Point".into()),
                wasm_local: 0,
                is_param: false,
            }],
        }];
        let sections = build_debug_sections(&src, &functions, &layouts);
        let debug_str = sections
            .iter()
            .find(|(name, _)| name == ".debug_str")
            .map(|(_, data)| data.as_slice())
            .unwrap_or(&[]);
        assert!(
            debug_str.windows(6).any(|w| w == b"Point\0"),
            "expected Point in .debug_str"
        );
        assert!(
            debug_str.windows(11).any(|w| w == b"__class_id\0"),
            "expected __class_id member in .debug_str"
        );
    }
}
