//! DWARF 5 line-table emission for native `-g` builds.
//!
//! Cranelift records opaque [`SourceLoc`] values on each machine instruction;
//! we encode Simula `(line, column)` into those bits during codegen (see
//! [`encode_srcloc`] / [`decode_srcloc`]) and turn the resulting machine-code
//! source ranges into a gimli line program after the object module is
//! finalized.
//!
//! Named locals/params get `DW_TAG_variable` / `DW_TAG_formal_parameter` DIEs
//! with Cranelift value-label locations. Class layouts become
//! `DW_TAG_structure_type` DIEs (header + members); `ref(C)` locals are typed
//! as `DW_TAG_pointer_type` to that structure. Text / array locals use
//! pointer-to-`SimrtTextFrame` / `SimrtArray*` DIEs matching `runtime.c`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cranelift_codegen::ir::SourceLoc;
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::FuncId;
use cranelift_object::ObjectProduct;
use gimli::write::{
    Address, AttributeValue, DwarfUnit, EndianVec, Expression, FileId, LineProgram, LineString,
    Location, LocationList, Range, RangeList, Result as GimliResult, Sections, UnitEntryId, Writer,
};
use gimli::{Encoding, Format, LineEncoding, Register, RunTimeEndian, SectionId};
use object::write::{Object as WriteObject, Relocation, StandardSegment};
use object::{
    Architecture, BinaryFormat, Object, ObjectSymbol, RelocationEncoding, RelocationFlags,
    RelocationKind, SectionKind, SymbolKind,
};

use crate::codegen::sourcemap::span_to_line_col;
use crate::error::CompileError;
use crate::error::Span;
use crate::layout::{ClassLayout, FieldType, OBJECT_HEADER_SIZE};
use crate::source::SourceFile;
use crate::target::CompileTarget;

/// Packs a 1-based `(line, column)` pair into a Cranelift [`SourceLoc`].
pub fn encode_srcloc(line: usize, column: usize) -> SourceLoc {
    let line = u32::try_from(line).unwrap_or(u32::MAX);
    let column = u32::try_from(column).unwrap_or(0xFFFF) & 0xFFFF;
    SourceLoc::new((line << 16) | column)
}

/// Decodes a Cranelift [`SourceLoc`] back into a 1-based `(line, column)`.
pub fn decode_srcloc(loc: SourceLoc) -> (u64, u64) {
    let bits = loc.bits();
    ((bits >> 16) as u64, (bits & 0xFFFF) as u64)
}

/// A machine-code source range with an attached Simula location.
#[derive(Debug, Clone, PartialEq)]
pub struct SrcLocRange {
    pub start: u32,
    pub end: u32,
    pub loc: SourceLoc,
}

/// Coarse DWARF type for a named MIR local / parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugValueType {
    I64,
    Bool,
    F64,
    /// `SimrtTextFrame*` (`runtime/runtime.c`).
    Text,
    /// `SimrtArrayI64*` descriptor.
    ArrayI64,
    /// `SimrtArrayText*` descriptor (same header; text-frame elements).
    ArrayText,
    /// Opaque pointer-sized value (unqualified object / funcref).
    Pointer,
}

/// One contiguous machine-code range where a local lives in a fixed place.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalLocRange {
    pub start: u32,
    pub end: u32,
    pub loc: LocalLocation,
}

/// Where a named local lives for a code range (Cranelift value-label output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalLocation {
    /// DWARF register number (`DW_OP_reg`).
    Reg(u16),
    /// Byte offset from the CFA (`DW_OP_fbreg` after converting CFA→FP).
    CfaOffset(i64),
}

/// A user-visible MIR local or parameter to emit as a DWARF DIE.
#[derive(Debug, Clone)]
pub struct DebugLocal {
    pub name: String,
    pub is_param: bool,
    pub ty: DebugValueType,
    /// Declared class name for [`DebugValueType::Pointer`] object refs.
    pub class_qual: Option<String>,
    pub locations: Vec<LocalLocRange>,
}

/// Per-function machine-code source map collected after Cranelift codegen.
pub struct FunctionDebugInfo {
    pub func_id: FuncId,
    pub symbol_name: String,
    pub srclocs: Vec<SrcLocRange>,
    pub default_line: u64,
    pub default_column: u64,
    pub locals: Vec<DebugLocal>,
}

/// Native `-g` payload returned from Cranelift emission (functions + layouts
/// for structure DIEs in the object file and macOS dSYM rewrite).
pub struct NativeDebugInfo {
    pub functions: Vec<FunctionDebugInfo>,
    pub class_layouts: Vec<ClassLayout>,
}

/// Builds DWARF line tables from collected function debug info.
pub struct DebugContext {
    dwarf: DwarfUnit,
    unit_range_list: RangeList,
    file_id: FileId,
    functions: Vec<(UnitEntryId, FuncId)>,
    type_i64: UnitEntryId,
    type_bool: UnitEntryId,
    type_f64: UnitEntryId,
    type_pointer: UnitEntryId,
    /// 8-byte boolean slot matching object field layout (vs 1-byte locals).
    type_bool_slot: UnitEntryId,
    /// `SimrtTextFrame*`.
    type_text: UnitEntryId,
    /// `SimrtArrayI64*`.
    type_array_i64: UnitEntryId,
    /// `SimrtArrayText*`.
    type_array_text: UnitEntryId,
    /// Class name (ASCII-lower) → `DW_TAG_pointer_type` to the class struct.
    class_pointer_types: HashMap<String, UnitEntryId>,
    /// DWARF register number for the frame pointer (for `DW_AT_frame_base`).
    fp_dwarf_reg: u16,
    /// CFA = FP + this many bytes after the prologue (System V / Apple).
    fp_to_cfa: i64,
}

impl DebugContext {
    pub fn new(isa: &dyn TargetIsa, source: &SourceFile, class_layouts: &[ClassLayout]) -> Self {
        let pointer_bytes = isa.frontend_config().pointer_bytes();
        let encoding = Encoding {
            format: Format::Dwarf32,
            // macOS lldb is happiest with DWARF ≤ 4; line tables work fine.
            version: if cfg!(target_os = "macos") { 4 } else { 5 },
            address_size: pointer_bytes,
        };

        let mut dwarf = DwarfUnit::new(encoding);

        let file_name = source_file_name(source);
        // Prefer a path derived from the source name so DWARF does not embed
        // the host's absolute cwd (helps reproducible `-g` artifacts).
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
        let file_id = if encoding.version >= 5 {
            line_program.files().next().expect("line program file").0
        } else {
            let dir_id = line_program.default_directory();
            line_program.add_file(
                LineString::new(file_name.as_bytes(), encoding, &mut dwarf.line_strings),
                dir_id,
                None,
            )
        };
        dwarf.unit.line_program = line_program;

        let producer = format!("sim {}", env!("CARGO_PKG_VERSION"));
        let root = dwarf.unit.root();
        let root_entry = dwarf.unit.get_mut(root);
        root_entry.set(
            gimli::DW_AT_producer,
            AttributeValue::StringRef(dwarf.strings.add(producer)),
        );
        root_entry.set(
            gimli::DW_AT_language,
            AttributeValue::Language(gimli::DW_LANG_C),
        );
        root_entry.set(
            gimli::DW_AT_name,
            AttributeValue::StringRef(dwarf.strings.add(file_name)),
        );
        root_entry.set(gimli::DW_AT_stmt_list, AttributeValue::Udata(0));
        root_entry.set(
            gimli::DW_AT_comp_dir,
            AttributeValue::StringRef(dwarf.strings.add(comp_dir)),
        );
        root_entry.set(
            gimli::DW_AT_low_pc,
            AttributeValue::Address(Address::Constant(0)),
        );

        let type_i64 = add_base_type(&mut dwarf, "i64", gimli::DW_ATE_signed, 8);
        let type_bool = add_base_type(&mut dwarf, "boolean", gimli::DW_ATE_boolean, 1);
        let type_f64 = add_base_type(&mut dwarf, "real", gimli::DW_ATE_float, 8);
        let type_pointer =
            add_base_type(&mut dwarf, "pointer", gimli::DW_ATE_address, pointer_bytes);
        let type_bool_slot = add_base_type(&mut dwarf, "boolean8", gimli::DW_ATE_boolean, 8);

        let mut context = Self {
            dwarf,
            unit_range_list: RangeList(Vec::new()),
            file_id,
            functions: Vec::new(),
            type_i64,
            type_bool,
            type_f64,
            type_pointer,
            type_bool_slot,
            // Filled by emit_runtime_types below.
            type_text: type_pointer,
            type_array_i64: type_pointer,
            type_array_text: type_pointer,
            class_pointer_types: HashMap::new(),
            fp_dwarf_reg: frame_pointer_dwarf_reg(isa),
            fp_to_cfa: i64::from(pointer_bytes) * 2,
        };
        context.emit_runtime_types(pointer_bytes);
        context.emit_class_types(class_layouts);
        context
    }

    /// Runtime heap shapes from `runtime/runtime.c` (text frames + array descriptors).
    fn emit_runtime_types(&mut self, pointer_bytes: u8) {
        let root = self.dwarf.unit.root();
        let ptr_sz = u64::from(pointer_bytes);

        // SimrtTextObject { main*, main_len, constant }
        let text_obj = self.dwarf.unit.add(root, gimli::DW_TAG_structure_type);
        {
            let name_id = self.dwarf.strings.add("SimrtTextObject");
            let entry = self.dwarf.unit.get_mut(text_obj);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
            entry.set(
                gimli::DW_AT_byte_size,
                AttributeValue::Udata(ptr_sz * 2 + 8),
            );
        }
        self.emit_struct_member(text_obj, "main", 0, self.type_pointer);
        self.emit_struct_member(text_obj, "main_len", ptr_sz, self.type_i64);
        let type_i32 = add_base_type(&mut self.dwarf, "i32", gimli::DW_ATE_signed, 4);
        self.emit_struct_member(text_obj, "constant", ptr_sz * 2, type_i32);

        let text_obj_ptr = self.dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
        {
            let entry = self.dwarf.unit.get_mut(text_obj_ptr);
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(text_obj));
        }

        // SimrtTextFrame { obj*, start, length, pos }
        let text_frame = self.dwarf.unit.add(root, gimli::DW_TAG_structure_type);
        {
            let name_id = self.dwarf.strings.add("SimrtTextFrame");
            let entry = self.dwarf.unit.get_mut(text_frame);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
            entry.set(gimli::DW_AT_byte_size, AttributeValue::Udata(ptr_sz + 24));
        }
        self.emit_struct_member(text_frame, "obj", 0, text_obj_ptr);
        self.emit_struct_member(text_frame, "start", ptr_sz, self.type_i64);
        self.emit_struct_member(text_frame, "length", ptr_sz + 8, self.type_i64);
        self.emit_struct_member(text_frame, "pos", ptr_sz + 16, self.type_i64);

        let text_ptr = self.dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
        {
            let entry = self.dwarf.unit.get_mut(text_ptr);
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(text_frame));
        }
        self.type_text = text_ptr;

        // Array descriptors: { ndims:i64, bounds_and_data[] … }
        self.type_array_i64 = self.emit_array_descriptor("SimrtArrayI64");
        self.type_array_text = self.emit_array_descriptor("SimrtArrayText");
    }

    fn emit_array_descriptor(&mut self, name: &str) -> UnitEntryId {
        let root = self.dwarf.unit.root();
        let struct_id = self.dwarf.unit.add(root, gimli::DW_TAG_structure_type);
        {
            let name_id = self.dwarf.strings.add(name);
            let entry = self.dwarf.unit.get_mut(struct_id);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
            // Flexible trailing bounds/data not fully described; ndims is enough for lldb.
            entry.set(gimli::DW_AT_byte_size, AttributeValue::Udata(8));
        }
        self.emit_struct_member(struct_id, "ndims", 0, self.type_i64);
        let ptr_id = self.dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
        {
            let entry = self.dwarf.unit.get_mut(ptr_id);
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(struct_id));
        }
        ptr_id
    }

    /// Emits one `DW_TAG_structure_type` (+ pointer type) per class layout.
    fn emit_class_types(&mut self, layouts: &[ClassLayout]) {
        let root = self.dwarf.unit.root();
        // Pass 1: declare structures + pointer types so `ref(C)` fields can resolve.
        let mut struct_ids = Vec::with_capacity(layouts.len());
        for layout in layouts {
            let struct_id = self.dwarf.unit.add(root, gimli::DW_TAG_structure_type);
            let name_id = self.dwarf.strings.add(layout.name.as_str());
            {
                let entry = self.dwarf.unit.get_mut(struct_id);
                entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
                entry.set(
                    gimli::DW_AT_byte_size,
                    AttributeValue::Udata(layout.size as u64),
                );
            }
            let ptr_id = self.dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
            {
                let entry = self.dwarf.unit.get_mut(ptr_id);
                entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(struct_id));
            }
            self.class_pointer_types
                .insert(layout.name.to_ascii_lowercase(), ptr_id);
            struct_ids.push(struct_id);
        }
        // Pass 2: members (may reference other class pointer types).
        for (layout, &struct_id) in layouts.iter().zip(struct_ids.iter()) {
            self.emit_struct_member(struct_id, "__class_id", 0, self.type_i64);
            for field in &layout.fields {
                let field_ty = self.type_for_field(field);
                debug_assert!(
                    field.offset >= OBJECT_HEADER_SIZE,
                    "field offsets start after the object header"
                );
                self.emit_struct_member(struct_id, &field.name, field.offset as u64, field_ty);
            }
        }
    }

    fn emit_struct_member(
        &mut self,
        struct_id: UnitEntryId,
        name: &str,
        offset: u64,
        ty: UnitEntryId,
    ) {
        let member_id = self.dwarf.unit.add(struct_id, gimli::DW_TAG_member);
        let name_id = self.dwarf.strings.add(name);
        let entry = self.dwarf.unit.get_mut(member_id);
        entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
        entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(ty));
        entry.set(
            gimli::DW_AT_data_member_location,
            AttributeValue::Udata(offset),
        );
    }

    fn type_for_field(&self, field: &crate::layout::FieldLayout) -> UnitEntryId {
        match field.ty {
            FieldType::I64 => self.type_i64,
            FieldType::Bool => self.type_bool_slot,
            FieldType::F64 => self.type_f64,
            FieldType::Text => self.type_text,
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
                self.type_pointer
            }
        }
    }

    fn type_for_local(&self, local: &DebugLocal) -> UnitEntryId {
        match local.ty {
            DebugValueType::I64 => self.type_i64,
            DebugValueType::Bool => self.type_bool,
            DebugValueType::F64 => self.type_f64,
            DebugValueType::Text => self.type_text,
            DebugValueType::ArrayI64 => self.type_array_i64,
            DebugValueType::ArrayText => self.type_array_text,
            DebugValueType::Pointer => {
                if let Some(qual) = &local.class_qual
                    && let Some(&id) = self.class_pointer_types.get(&qual.to_ascii_lowercase())
                {
                    return id;
                }
                self.type_pointer
            }
        }
    }

    pub fn add_function(&mut self, info: &FunctionDebugInfo) {
        let entry_id = self.emit_subprogram(info, address_for_func(info.func_id), None);
        self.functions.push((entry_id, info.func_id));
        self.emit_line_sequence(info, address_for_func(info.func_id));
        self.emit_locals(entry_id, info, address_for_func(info.func_id));
    }

    /// Like [`Self::add_function`], but binds line-table addresses to a
    /// concrete load address (used when patching a linked executable / dSYM).
    pub fn add_function_at_base(&mut self, info: &FunctionDebugInfo, base_addr: u64) {
        let entry_id = self.emit_subprogram(
            info,
            Address::Constant(base_addr),
            Some(u64::from(
                info.srclocs.last().map(|loc| loc.end).unwrap_or(0),
            )),
        );
        self.functions.push((entry_id, info.func_id));
        self.emit_line_sequence(info, Address::Constant(base_addr));
        self.emit_locals(entry_id, info, Address::Constant(base_addr));
    }

    fn emit_subprogram(
        &mut self,
        info: &FunctionDebugInfo,
        low_pc: Address,
        high_pc: Option<u64>,
    ) -> UnitEntryId {
        let entry_id = self
            .dwarf
            .unit
            .add(self.dwarf.unit.root(), gimli::DW_TAG_subprogram);
        let entry = self.dwarf.unit.get_mut(entry_id);
        let name_id = self.dwarf.strings.add(info.symbol_name.as_str());
        entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
        entry.set(
            gimli::DW_AT_linkage_name,
            AttributeValue::StringRef(name_id),
        );
        match low_pc {
            Address::Constant(0) if high_pc.is_none() => {
                entry.set(gimli::DW_AT_low_pc, AttributeValue::Udata(0));
                entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(0));
            }
            _ => {
                entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(low_pc));
                if let Some(high) = high_pc {
                    entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(high));
                } else {
                    entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(0));
                }
            }
        }
        entry.set(
            gimli::DW_AT_decl_file,
            AttributeValue::FileIndex(Some(self.file_id)),
        );
        entry.set(
            gimli::DW_AT_decl_line,
            AttributeValue::Udata(info.default_line),
        );
        if info.symbol_name == "sim_main" {
            entry.set(gimli::DW_AT_external, AttributeValue::FlagPresent);
        }

        // Locals use DW_OP_fbreg relative to the frame pointer. Preserve FP in
        // debug builds (see `create_isa`) so this stays valid on macOS without
        // `.eh_frame`.
        let mut frame_base = Expression::new();
        frame_base.op_reg(Register(self.fp_dwarf_reg));
        entry.set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(frame_base));

        entry_id
    }

    fn emit_locals(
        &mut self,
        subprogram: UnitEntryId,
        info: &FunctionDebugInfo,
        func_addr: Address,
    ) {
        for local in &info.locals {
            let tag = if local.is_param {
                gimli::DW_TAG_formal_parameter
            } else {
                gimli::DW_TAG_variable
            };
            let entry_id = self.dwarf.unit.add(subprogram, tag);
            let type_id = self.type_for_local(local);
            let name_id = self.dwarf.strings.add(local.name.as_str());
            let entry = self.dwarf.unit.get_mut(entry_id);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name_id));
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(type_id));
            entry.set(
                gimli::DW_AT_decl_file,
                AttributeValue::FileIndex(Some(self.file_id)),
            );
            entry.set(
                gimli::DW_AT_decl_line,
                AttributeValue::Udata(info.default_line),
            );

            if let Some(expr) = self.simple_location_expr(local) {
                let entry = self.dwarf.unit.get_mut(entry_id);
                entry.set(gimli::DW_AT_location, AttributeValue::Exprloc(expr));
            } else if !local.locations.is_empty() {
                let loc_list = self.location_list_for(local, func_addr);
                let loc_id = self.dwarf.unit.locations.add(loc_list);
                let entry = self.dwarf.unit.get_mut(entry_id);
                entry.set(
                    gimli::DW_AT_location,
                    AttributeValue::LocationListRef(loc_id),
                );
            }
        }
    }

    /// When every range shares one location, emit a single `DW_AT_location`
    /// expression covering the whole function (simpler for lldb).
    fn simple_location_expr(&self, local: &DebugLocal) -> Option<Expression> {
        let first = local.locations.first()?;
        if local.locations.iter().all(|range| range.loc == first.loc) {
            Some(self.location_expression(first.loc))
        } else {
            None
        }
    }

    fn location_list_for(&self, local: &DebugLocal, func_addr: Address) -> LocationList {
        let mut entries = Vec::with_capacity(local.locations.len());
        for range in &local.locations {
            let begin = relocate_address(func_addr, range.start);
            let end = relocate_address(func_addr, range.end);
            entries.push(Location::StartEnd {
                begin,
                end,
                data: self.location_expression(range.loc),
            });
        }
        LocationList(entries)
    }

    fn location_expression(&self, loc: LocalLocation) -> Expression {
        let mut expr = Expression::new();
        match loc {
            LocalLocation::Reg(reg) => {
                expr.op_reg(Register(reg));
            }
            LocalLocation::CfaOffset(offset) => {
                // CFA = FP + fp_to_cfa ⇒ address = FP + (fp_to_cfa + offset).
                expr.op_fbreg(self.fp_to_cfa + offset);
            }
        }
        expr
    }

    fn emit_line_sequence(&mut self, info: &FunctionDebugInfo, func_addr: Address) {
        self.dwarf.unit.line_program.begin_sequence(Some(func_addr));

        let mut end_offset = 0u64;
        for srcloc in &info.srclocs {
            self.dwarf.unit.line_program.row().address_offset = u64::from(srcloc.start);
            let (line, column) = if srcloc.loc.is_default() {
                (info.default_line, info.default_column)
            } else {
                decode_srcloc(srcloc.loc)
            };
            self.dwarf.unit.line_program.row().file = self.file_id;
            self.dwarf.unit.line_program.row().line = line;
            self.dwarf.unit.line_program.row().column = column;
            self.dwarf.unit.line_program.generate_row();
            end_offset = u64::from(srcloc.end);
        }

        self.dwarf.unit.line_program.end_sequence(end_offset);

        let func_end = info.srclocs.last().map(|loc| loc.end).unwrap_or(0);
        self.unit_range_list.0.push(Range::StartLength {
            begin: func_addr,
            length: u64::from(func_end),
        });

        if let Some((entry_id, _)) = self.functions.iter().find(|(_, id)| *id == info.func_id) {
            let entry = self.dwarf.unit.get_mut(*entry_id);
            entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(func_addr));
            entry.set(
                gimli::DW_AT_high_pc,
                AttributeValue::Udata(u64::from(func_end)),
            );
        }
    }

    pub fn write_into_product(
        &mut self,
        product: &mut ObjectProduct,
        endian: RunTimeEndian,
    ) -> Result<(), CompileError> {
        let unit_range_list_id = self.dwarf.unit.ranges.add(self.unit_range_list.clone());
        let root = self.dwarf.unit.root();
        let root_entry = self.dwarf.unit.get_mut(root);
        root_entry.set(
            gimli::DW_AT_ranges,
            AttributeValue::RangeListRef(unit_range_list_id),
        );

        let mut sections = Sections::new(WriterRelocate::new(endian));
        self.dwarf
            .write(&mut sections)
            .map_err(|error| CompileError::codegen(format!("failed to write DWARF: {error}")))?;

        let mut section_map = HashMap::new();
        sections
            .for_each_mut(|id, section| {
                if !section.writer.slice().is_empty() {
                    let section_id = add_debug_section(product, id, section.writer.take());
                    section_map.insert(id, section_id);
                }
                Ok::<(), gimli::write::Error>(())
            })
            .map_err(|error| CompileError::codegen(format!("failed to finalize DWARF: {error}")))?;

        sections
            .for_each(|id, section| {
                if let Some(section_id) = section_map.get(&id) {
                    for reloc in &section.relocs {
                        add_debug_reloc(product, &section_map, section_id, reloc);
                    }
                }
                Ok::<(), gimli::write::Error>(())
            })
            .map_err(|error| CompileError::codegen(format!("failed to relocate DWARF: {error}")))?;

        Ok(())
    }
}

/// Writes a macOS dSYM bundle next to `executable_path` so lldb can resolve
/// Simula source lines after the host Darwin linker drops DWARF from the `.o`.
pub fn write_dsym_bundle(
    executable_path: &Path,
    debug: &NativeDebugInfo,
    source: &SourceFile,
    target: CompileTarget,
    isa: &dyn TargetIsa,
) -> Result<(), CompileError> {
    if !target_is_macho(target) {
        return Ok(());
    }

    let exe_bytes = std::fs::read(executable_path).map_err(|error| {
        CompileError::codegen(format!(
            "failed to read {} for DWARF: {error}",
            executable_path.display()
        ))
    })?;

    let mut context = DebugContext::new(isa, source, &debug.class_layouts);
    for info in &debug.functions {
        let lookup_name = macho_symbol_name(&info.symbol_name);
        let base = symbol_address(&exe_bytes, lookup_name).ok_or_else(|| {
            CompileError::codegen(format!(
                "failed to resolve linked symbol '{lookup_name}' for DWARF"
            ))
        })?;
        context.add_function_at_base(info, base);
    }

    let dwarf_path = dsym_dwarf_object_path(executable_path);
    if let Some(parent) = dwarf_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CompileError::codegen(format!("failed to create {}: {error}", parent.display()))
        })?;
    }
    write_info_plist(executable_path)?;

    let endian = match isa.endianness() {
        cranelift_codegen::ir::Endianness::Little => RunTimeEndian::Little,
        cranelift_codegen::ir::Endianness::Big => RunTimeEndian::Big,
    };
    let sections = context.finish_sections(endian)?;
    let uuid = executable_uuid(&exe_bytes);
    write_standalone_macho(&dwarf_path, target, sections, uuid)?;

    Ok(())
}

/// Path to `{exe}.dSYM/Contents/Resources/DWARF/{exe_name}`.
pub fn dsym_dwarf_object_path(executable_path: &Path) -> PathBuf {
    let name = executable_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sim".into());
    executable_path
        .with_extension("dSYM")
        .join(format!("Contents/Resources/DWARF/{name}"))
}

impl DebugContext {
    fn finish_sections(
        &mut self,
        endian: RunTimeEndian,
    ) -> Result<Vec<(SectionId, Vec<u8>)>, CompileError> {
        let unit_range_list_id = self.dwarf.unit.ranges.add(self.unit_range_list.clone());
        let root = self.dwarf.unit.root();
        let root_entry = self.dwarf.unit.get_mut(root);
        root_entry.set(
            gimli::DW_AT_ranges,
            AttributeValue::RangeListRef(unit_range_list_id),
        );

        let mut sections = Sections::new(EndianVec::new(endian));
        self.dwarf
            .write(&mut sections)
            .map_err(|error| CompileError::codegen(format!("failed to write DWARF: {error}")))?;

        let mut out = Vec::new();
        sections
            .for_each_mut(|id, section| {
                if !section.slice().is_empty() {
                    out.push((id, section.take()));
                }
                Ok::<(), gimli::write::Error>(())
            })
            .map_err(|error| CompileError::codegen(format!("failed to finalize DWARF: {error}")))?;
        Ok(out)
    }
}

fn target_is_macho(target: CompileTarget) -> bool {
    matches!(
        target,
        CompileTarget::MacOsX86_64
            | CompileTarget::MacOsAarch64
            | CompileTarget::Native if cfg!(target_os = "macos")
    )
}

fn macho_symbol_name(name: &str) -> &str {
    if name.starts_with('_') {
        name
    } else {
        // Mach-O exports use a leading underscore; nm/lldb accept either form
        // but object crate returns the nlist name including `_`.
        name
    }
}

fn symbol_address(bytes: &[u8], name: &str) -> Option<u64> {
    let file = object::File::parse(bytes).ok()?;
    file.symbols().find_map(|symbol| {
        if symbol.kind() != SymbolKind::Text {
            return None;
        }
        let Ok(symbol_name) = symbol.name() else {
            return None;
        };
        if symbol_name == name
            || symbol_name.trim_start_matches('_') == name.trim_start_matches('_')
        {
            Some(symbol.address())
        } else {
            None
        }
    })
}

fn write_info_plist(executable_path: &Path) -> Result<(), CompileError> {
    let dsym_root = executable_path.with_extension("dSYM");
    let plist_path = dsym_root.join("Contents/Info.plist");
    let bundle_id = executable_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sim");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>English</string>
  <key>CFBundleIdentifier</key>
  <string>com.outimage.dsym.{bundle_id}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundlePackageType</key>
  <string>dSYM</string>
  <key>CFBundleSignature</key>
  <string>????</string>
  <key>CFBundleShortVersionString</key>
  <string>1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
</dict>
</plist>
"#
    );
    std::fs::write(plist_path, plist)
        .map_err(|error| CompileError::codegen(format!("failed to write dSYM Info.plist: {error}")))
}

fn write_standalone_macho(
    path: &Path,
    target: CompileTarget,
    sections: Vec<(SectionId, Vec<u8>)>,
    uuid: Option<[u8; 16]>,
) -> Result<(), CompileError> {
    let (arch, endian) = object_format(target)?;
    let mut object = WriteObject::new(BinaryFormat::MachO, arch, endian);
    for (id, data) in sections {
        let name = id.name().replace('.', "__").into_bytes();
        let section_id = object.add_section(
            object.segment_name(StandardSegment::Debug).to_vec(),
            name,
            SectionKind::Debug,
        );
        object.section_mut(section_id).set_data(data, 1);
    }
    let mut bytes = object
        .write()
        .map_err(|error| CompileError::codegen(format!("failed to emit dSYM Mach-O: {error}")))?;
    if let Some(uuid) = uuid {
        patch_macho_as_dsym(&mut bytes, uuid)?;
    }
    std::fs::write(path, bytes).map_err(|error| {
        CompileError::codegen(format!("failed to write {}: {error}", path.display()))
    })
}

/// Rewrites a relocatable Mach-O as `MH_DSYM` and injects an `LC_UUID` that
/// matches the linked executable so lldb will associate the companion file.
fn patch_macho_as_dsym(bytes: &mut Vec<u8>, uuid: [u8; 16]) -> Result<(), CompileError> {
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_DSYM: u32 = 0xa;
    const LC_UUID: u32 = 0x1b;
    const UUID_CMD_SIZE: u32 = 24;
    const HEADER64_SIZE: usize = 32;

    if bytes.len() < HEADER64_SIZE {
        return Err(CompileError::codegen("dSYM Mach-O is too short"));
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != MH_MAGIC_64 {
        return Err(CompileError::codegen(
            "expected little-endian 64-bit Mach-O for dSYM patching",
        ));
    }

    // filetype
    bytes[12..16].copy_from_slice(&MH_DSYM.to_le_bytes());

    let ncmds = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let sizeofcmds = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    bytes[16..20].copy_from_slice(&(ncmds + 1).to_le_bytes());
    bytes[20..24].copy_from_slice(&(sizeofcmds + UUID_CMD_SIZE).to_le_bytes());

    let insert_at = HEADER64_SIZE;
    let mut uuid_cmd = Vec::with_capacity(UUID_CMD_SIZE as usize);
    uuid_cmd.extend_from_slice(&LC_UUID.to_le_bytes());
    uuid_cmd.extend_from_slice(&UUID_CMD_SIZE.to_le_bytes());
    uuid_cmd.extend_from_slice(&uuid);
    bytes.splice(insert_at..insert_at, uuid_cmd);

    // Section file offsets live inside segment load commands and point past
    // the load-command region; bump every 64-bit fileoff that falls at/after
    // the old load-command end by UUID_CMD_SIZE.
    let old_cmds_end = HEADER64_SIZE + sizeofcmds as usize;
    let new_cmds_end = old_cmds_end + UUID_CMD_SIZE as usize;
    bump_macho_fileoffs(bytes, HEADER64_SIZE, new_cmds_end, UUID_CMD_SIZE as u64)?;
    let _ = old_cmds_end;
    Ok(())
}

fn bump_macho_fileoffs(
    bytes: &mut [u8],
    header_size: usize,
    cmds_end: usize,
    delta: u64,
) -> Result<(), CompileError> {
    const LC_SEGMENT_64: u32 = 0x19;
    let ncmds = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let mut offset = header_size;
    for _ in 0..ncmds {
        if offset + 8 > cmds_end || offset + 8 > bytes.len() {
            break;
        }
        let cmd = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let cmdsize =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if cmd == LC_SEGMENT_64 && cmdsize >= 72 {
            // fileoff at +40 in segment_command_64
            let fileoff_pos = offset + 40;
            if fileoff_pos + 8 <= bytes.len() {
                let fileoff =
                    u64::from_le_bytes(bytes[fileoff_pos..fileoff_pos + 8].try_into().unwrap());
                if fileoff != 0 {
                    bytes[fileoff_pos..fileoff_pos + 8]
                        .copy_from_slice(&(fileoff + delta).to_le_bytes());
                }
            }
            // Each section_64 has offset at +48 relative to section start;
            // first section starts at offset+72.
            let nsects = u32::from_le_bytes(bytes[offset + 64..offset + 68].try_into().unwrap());
            let mut sect = offset + 72;
            for _ in 0..nsects {
                if sect + 56 > bytes.len() {
                    break;
                }
                let sect_offset_pos = sect + 48;
                let sect_off = u32::from_le_bytes(
                    bytes[sect_offset_pos..sect_offset_pos + 4]
                        .try_into()
                        .unwrap(),
                );
                if sect_off != 0 {
                    bytes[sect_offset_pos..sect_offset_pos + 4]
                        .copy_from_slice(&(sect_off + delta as u32).to_le_bytes());
                }
                sect += 80; // sizeof(section_64)
            }
        }
        offset += cmdsize;
    }
    Ok(())
}

fn executable_uuid(bytes: &[u8]) -> Option<[u8; 16]> {
    let file = object::File::parse(bytes).ok()?;
    file.mach_uuid().ok().flatten()
}

fn object_format(
    target: CompileTarget,
) -> Result<(Architecture, object::Endianness), CompileError> {
    match target {
        CompileTarget::MacOsX86_64 => Ok((Architecture::X86_64, object::Endianness::Little)),
        CompileTarget::MacOsAarch64 => Ok((Architecture::Aarch64, object::Endianness::Little)),
        CompileTarget::Native if cfg!(target_os = "macos") => {
            if cfg!(target_arch = "aarch64") {
                Ok((Architecture::Aarch64, object::Endianness::Little))
            } else {
                Ok((Architecture::X86_64, object::Endianness::Little))
            }
        }
        other => Err(CompileError::codegen(format!(
            "unsupported DWARF target {other}"
        ))),
    }
}

/// Returns a default `(line, column)` for a MIR function (first non-synthetic span).
pub fn default_location_for_function(
    source: &str,
    spans: impl IntoIterator<Item = Span>,
) -> (u64, u64) {
    for span in spans {
        if span.start == 0 && span.end == 0 {
            continue;
        }
        let (line, column) = span_to_line_col(source, span.start);
        return (line as u64, column as u64);
    }
    (1, 1)
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

fn address_for_func(func_id: FuncId) -> Address {
    Address::Symbol {
        symbol: func_id.as_u32() as usize,
        addend: 0,
    }
}

fn relocate_address(base: Address, offset: u32) -> Address {
    match base {
        Address::Constant(addr) => Address::Constant(addr + u64::from(offset)),
        Address::Symbol { symbol, addend } => Address::Symbol {
            symbol,
            addend: addend + i64::from(offset),
        },
    }
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

fn frame_pointer_dwarf_reg(isa: &dyn TargetIsa) -> u16 {
    // DWARF register numbers: x86_64 RBP = 6, AArch64 FP (x29) = 29.
    match isa.triple().architecture {
        target_lexicon::Architecture::X86_64 => 6,
        target_lexicon::Architecture::Aarch64(_) => 29,
        _ => 6,
    }
}

type DebugSectionId = (object::write::SectionId, object::write::SymbolId);

fn add_debug_section(product: &mut ObjectProduct, id: SectionId, data: Vec<u8>) -> DebugSectionId {
    let name = if product.object.format() == object::BinaryFormat::MachO {
        id.name().replace('.', "__")
    } else {
        id.name().to_string()
    }
    .into_bytes();

    let segment = product.object.segment_name(StandardSegment::Debug).to_vec();
    let section_id = product
        .object
        .add_section(segment, name, SectionKind::Debug);
    product.object.section_mut(section_id).set_data(data, 1);
    let symbol_id = product.object.section_symbol(section_id);
    (section_id, symbol_id)
}

#[derive(Clone)]
struct DebugReloc {
    offset: u32,
    size: u8,
    name: DebugRelocName,
    addend: i64,
    kind: RelocationKind,
}

#[derive(Clone)]
enum DebugRelocName {
    Section(SectionId),
    Symbol(usize),
}

#[derive(Clone)]
struct WriterRelocate {
    relocs: Vec<DebugReloc>,
    writer: EndianVec<RunTimeEndian>,
}

impl WriterRelocate {
    fn new(endian: RunTimeEndian) -> Self {
        Self {
            relocs: Vec::new(),
            writer: EndianVec::new(endian),
        }
    }
}

impl Writer for WriterRelocate {
    type Endian = RunTimeEndian;

    fn endian(&self) -> Self::Endian {
        self.writer.endian()
    }

    fn len(&self) -> usize {
        self.writer.len()
    }

    fn write(&mut self, bytes: &[u8]) -> GimliResult<()> {
        self.writer.write(bytes)
    }

    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> GimliResult<()> {
        self.writer.write_at(offset, bytes)
    }

    fn write_address(&mut self, address: Address, size: u8) -> GimliResult<()> {
        match address {
            Address::Constant(val) => self.write_udata(val, size),
            Address::Symbol { symbol, addend } => {
                let offset = self.len() as u64;
                self.relocs.push(DebugReloc {
                    offset: offset as u32,
                    size,
                    name: DebugRelocName::Symbol(symbol),
                    addend,
                    kind: RelocationKind::Absolute,
                });
                self.write_udata(0, size)
            }
        }
    }

    fn write_offset(&mut self, val: usize, section: SectionId, size: u8) -> GimliResult<()> {
        let offset = self.len() as u32;
        self.relocs.push(DebugReloc {
            offset,
            size,
            name: DebugRelocName::Section(section),
            addend: val as i64,
            kind: RelocationKind::Absolute,
        });
        self.write_udata(0, size)
    }

    fn write_offset_at(
        &mut self,
        offset: usize,
        val: usize,
        section: SectionId,
        size: u8,
    ) -> GimliResult<()> {
        self.relocs.push(DebugReloc {
            offset: offset as u32,
            size,
            name: DebugRelocName::Section(section),
            addend: val as i64,
            kind: RelocationKind::Absolute,
        });
        self.write_udata_at(offset, 0, size)
    }
}

fn add_debug_reloc(
    product: &mut ObjectProduct,
    section_map: &HashMap<SectionId, DebugSectionId>,
    from: &DebugSectionId,
    reloc: &DebugReloc,
) {
    let (symbol, symbol_offset) = match reloc.name {
        DebugRelocName::Section(id) => (section_map.get(&id).unwrap().1, 0),
        DebugRelocName::Symbol(func_id) => {
            let symbol_id = product.function_symbol(FuncId::from_u32(func_id as u32));
            product
                .object
                .symbol_section_and_offset(symbol_id)
                .unwrap_or((symbol_id, 0))
        }
    };
    product
        .object
        .add_relocation(
            from.0,
            Relocation {
                offset: u64::from(reloc.offset),
                symbol,
                flags: RelocationFlags::Generic {
                    kind: reloc.kind,
                    encoding: RelocationEncoding::Generic,
                    size: reloc.size * 8,
                },
                addend: i64::try_from(symbol_offset).unwrap_or(0) + reloc.addend,
            },
        )
        .expect("failed to add DWARF relocation");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srcloc_roundtrip_preserves_line_and_column() {
        let loc = encode_srcloc(3, 7);
        assert_eq!(decode_srcloc(loc), (3, 7));
    }

    #[test]
    fn default_location_skips_synthetic_spans() {
        let source = "begin\n  x := 1;\nend;";
        let spans = [0..0, 7..12];
        assert_eq!(default_location_for_function(source, spans), (2, 2));
    }
}
