//! Integration tests for Phase 3 DWARF debug info (`-g` / `debug_info`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gimli::{EndianSlice, RunTimeEndian, SectionId};
use object::{Object, ObjectSection};
use outimage::codegen::dwarf::dsym_dwarf_object_path;
use outimage::source::SourceFile;
use outimage::{CompileOptions, CompileResult, CompileTarget};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-debug-info-{tag}-{id}"))
}

fn compile_with_debug(source_text: &str) -> (PathBuf, PathBuf) {
    let output = temp_output_path("bin");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::Native);
    options.debug_info = true;
    let source = SourceFile::anonymous(source_text);

    let result = outimage::compile_with_options(&source, &options)
        .unwrap_or_else(|error| panic!("compile failed: {error}"));
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected native artifact");
    };

    let map_path = PathBuf::from(format!("{}.sim-map", output.display()));
    (artifact, map_path)
}

fn debug_info_bytes(artifact: &Path) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(artifact)
        && has_dwarf_section(&bytes)
    {
        return Some(bytes);
    }

    let dsym = dsym_dwarf_object_path(artifact);
    std::fs::read(dsym).ok()
}

fn has_dwarf_section(bytes: &[u8]) -> bool {
    let file = object::File::parse(bytes).expect("parse executable");
    file.sections().any(|section| {
        section
            .name()
            .map(|name| {
                name == ".debug_line"
                    || name == "__debug_line"
                    || name == ".debug_info"
                    || name == "__debug_info"
            })
            .unwrap_or(false)
    })
}

fn dwarf_line_numbers(bytes: &[u8]) -> Vec<u64> {
    let file = object::File::parse(bytes).expect("parse executable");
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };

    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let name = id.name();
        let macho_name = name.replace('.', "__");
        let data = file
            .sections()
            .find_map(|section| {
                let section_name = section.name().ok()?;
                if section_name == name || section_name == macho_name {
                    Some(section.data().expect("section data"))
                } else {
                    None
                }
            })
            .unwrap_or(&[]);
        Ok(EndianSlice::new(data, endian))
    };

    let dwarf = gimli::Dwarf::load(load_section).expect("load dwarf");
    let mut lines = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        if let Some(program) = unit.line_program {
            let mut rows = program.rows();
            while let Some((_, row)) = rows.next_row().expect("line row") {
                if !row.end_sequence()
                    && let Some(line) = row.line()
                {
                    lines.push(line.get());
                }
            }
        }
    }
    lines
}

#[cfg(not(windows))]
#[test]
fn linked_binary_or_dsym_contains_dwarf_debug_sections() {
    let source = "begin integer x;\n  x := 1;\n  OutText(\"hi\");\n  OutImage;\nend;\n";
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF in binary or dSYM");
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    assert!(
        has_dwarf_section(&bytes),
        "debug artifact should contain DWARF sections"
    );
}

#[cfg(not(windows))]
#[test]
fn dwarf_line_table_includes_assignment_line() {
    // Line 2 is `integer x;`, line 3 is `x := 1;`.
    let source = "begin integer x;\n  x := 1;\n  OutText(\"hi\");\n  OutImage;\nend;\n";
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF in binary or dSYM");
    let lines = dwarf_line_numbers(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    assert!(
        lines.contains(&3),
        "expected line 3 (the assignment) in DWARF line table, got {lines:?}"
    );
}

#[test]
fn debug_info_still_writes_json_side_map() {
    let source = "begin integer x;\n  x := 1;\nend;\n";
    let (artifact, map_path) = compile_with_debug(source);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));

    let json = std::fs::read_to_string(&map_path)
        .unwrap_or_else(|error| panic!("expected side-map at {}: {error}", map_path.display()));
    let _ = std::fs::remove_file(&map_path);

    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(value["version"], 1);
    assert!(value["mappings"].as_array().is_some_and(|m| !m.is_empty()));
}

#[test]
fn release_build_has_no_dwarf_sections() {
    let output = temp_output_path("release");
    let options = CompileOptions::for_compile(output.clone(), CompileTarget::Native);
    let source = SourceFile::anonymous("begin OutText(\"hi\"); OutImage; end;");

    let result = outimage::compile_with_options(&source, &options).expect("compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read binary");
    let _ = std::fs::remove_file(&artifact);

    assert!(
        !has_dwarf_section(&bytes),
        "non-debug build should not contain DWARF sections"
    );
    assert!(
        !artifact.with_extension("dSYM").exists(),
        "non-debug build should not emit a dSYM bundle"
    );
}

#[test]
fn wasm_debug_info_writes_pc_source_map_and_sourcemappingurl() {
    let output = temp_output_path("wasm").with_extension("wasm");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    options.debug_info = true;
    let source = SourceFile::anonymous(
        "begin\n  integer x;\n  x := 1;\n  OutText(\"hi\");\n  OutImage;\nend;\n",
    );

    let result = outimage::compile_with_options(&source, &options).expect("wasm -g compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let map_path = PathBuf::from(format!("{}.map", output.display()));
    let side_path = PathBuf::from(format!("{}.sim-map", output.display()));
    let map_json = std::fs::read_to_string(&map_path)
        .unwrap_or_else(|error| panic!("expected {}: {error}", map_path.display()));
    let side_json = std::fs::read_to_string(&side_path)
        .unwrap_or_else(|error| panic!("expected {}: {error}", side_path.display()));
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_file(&map_path);
    let _ = std::fs::remove_file(&side_path);

    assert!(
        bytes
            .windows(b"sourceMappingURL".len())
            .any(|w| w == b"sourceMappingURL"),
        "wasm -g should embed a sourceMappingURL custom section"
    );

    let v3: serde_json::Value = serde_json::from_str(&map_json).expect("valid Source Map v3");
    assert_eq!(v3["version"], 3);
    let mappings = v3["mappings"].as_str().expect("mappings string");
    assert!(
        !mappings.contains(';'),
        "wasm PC source map uses a single generated line"
    );
    assert!(
        mappings.contains(','),
        "expected multiple PC segments, got {mappings:?}"
    );

    let side: serde_json::Value = serde_json::from_str(&side_json).expect("valid side-map");
    let first = &side["mappings"][0];
    assert!(
        first["wasm_offset"]
            .as_u64()
            .is_some_and(|offset| offset > 0),
        "side-map should include relocated wasm_offset: {first}"
    );
    let offset = first["wasm_offset"].as_u64().unwrap() as usize;
    assert!(
        offset < bytes.len(),
        "wasm_offset {offset} must lie inside the module ({} bytes)",
        bytes.len()
    );
}

/// Parses top-level wasm custom sections (id `0`) out of a finished module,
/// returning `(name, payload)` pairs in file order. `wasm-encoder`/`object`
/// don't expose wasm custom-section parsing in this crate's dependency set,
/// so this walks the (tiny) binary format directly.
fn wasm_custom_sections(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    fn read_leb128_u32(data: &[u8], pos: &mut usize) -> u32 {
        let mut result = 0u32;
        let mut shift = 0u32;
        loop {
            let byte = data[*pos];
            *pos += 1;
            result |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        result
    }

    assert_eq!(&bytes[..4], b"\0asm", "not a wasm module");
    let mut pos = 8usize;
    let mut out = Vec::new();
    while pos < bytes.len() {
        let section_id = bytes[pos];
        pos += 1;
        let section_len = read_leb128_u32(bytes, &mut pos) as usize;
        let section_end = pos + section_len;
        if section_id == 0 {
            let mut cursor = pos;
            let name_len = read_leb128_u32(bytes, &mut cursor) as usize;
            let name = String::from_utf8(bytes[cursor..cursor + name_len].to_vec())
                .expect("custom section name is valid utf-8");
            cursor += name_len;
            let data = bytes[cursor..section_end].to_vec();
            out.push((name, data));
        }
        pos = section_end;
    }
    out
}

#[test]
fn wasm_debug_info_embeds_dwarf_custom_sections() {
    let output = temp_output_path("wasm-dwarf").with_extension("wasm");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    options.debug_info = true;
    let source = SourceFile::anonymous(
        "begin\n  integer x;\n  x := 1;\n  OutText(\"hi\");\n  OutImage;\nend;\n",
    );

    let result = outimage::compile_with_options(&source, &options).expect("wasm -g compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let map_path = PathBuf::from(format!("{}.map", output.display()));
    let side_path = PathBuf::from(format!("{}.sim-map", output.display()));
    let sections = wasm_custom_sections(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_file(&map_path);
    let _ = std::fs::remove_file(&side_path);

    // Requirement: at least one custom section whose name starts with
    // `.debug_` (specifically `.debug_line`) with a non-empty payload.
    let debug_line = sections
        .iter()
        .find(|(name, _)| name == ".debug_line")
        .unwrap_or_else(|| {
            panic!(
                "expected a .debug_line custom section, got {:?}",
                sections.iter().map(|(n, _)| n).collect::<Vec<_>>()
            )
        });
    assert!(
        !debug_line.1.is_empty(),
        ".debug_line payload must be non-empty"
    );

    for required in [".debug_info", ".debug_abbrev"] {
        let section = sections
            .iter()
            .find(|(name, _)| name == required)
            .unwrap_or_else(|| panic!("expected a {required} custom section"));
        assert!(
            !section.1.is_empty(),
            "{required} payload must be non-empty"
        );
    }

    // Source Map v3 behavior must be unaffected by DWARF emission.
    assert!(
        bytes
            .windows(b"sourceMappingURL".len())
            .any(|w| w == b"sourceMappingURL"),
        "wasm -g should still embed a sourceMappingURL custom section"
    );

    // Sanity-check the DWARF is actually well-formed and covers `main`: parse
    // `.debug_line` with gimli and confirm the assignment's line (3) shows up.
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
    let debug_line_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_line_str")
        .map(|(_, data)| data.clone())
        .unwrap_or_default();
    let debug_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_str")
        .map(|(_, data)| data.clone())
        .unwrap_or_default();

    let endian = RunTimeEndian::Little;
    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let data: &[u8] = match id {
            SectionId::DebugInfo => &debug_info,
            SectionId::DebugAbbrev => &debug_abbrev,
            SectionId::DebugLine => &debug_line.1,
            SectionId::DebugLineStr => &debug_line_str,
            SectionId::DebugStr => &debug_str,
            _ => &[],
        };
        Ok(EndianSlice::new(data, endian))
    };
    let dwarf = gimli::Dwarf::load(load_section).expect("load wasm dwarf");
    let mut lines = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        if let Some(program) = unit.line_program {
            let mut rows = program.rows();
            while let Some((_, row)) = rows.next_row().expect("line row") {
                if !row.end_sequence()
                    && let Some(line) = row.line()
                {
                    lines.push(line.get());
                }
            }
        }
    }
    assert!(
        lines.contains(&3),
        "expected line 3 (the assignment) in wasm DWARF line table, got {lines:?}"
    );
}

#[test]
fn wasm_debug_info_contains_named_local_with_location() {
    let output = temp_output_path("wasm-dwarf-local").with_extension("wasm");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    options.debug_info = true;
    let source = SourceFile::anonymous(
        "begin\n  integer x;\n  x := 42;\n  OutText(\"hi\");\n  OutImage;\nend;\n",
    );

    let result = outimage::compile_with_options(&source, &options).expect("wasm -g compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let map_path = PathBuf::from(format!("{}.map", output.display()));
    let side_path = PathBuf::from(format!("{}.sim-map", output.display()));
    let sections = wasm_custom_sections(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_file(&map_path);
    let _ = std::fs::remove_file(&side_path);

    let vars = wasm_dwarf_variable_names(&sections);
    let x = vars
        .iter()
        .find(|(name, _, _)| name == "x")
        .unwrap_or_else(|| panic!("expected variable x in wasm DWARF, got {vars:?}"));
    assert!(x.1, "variable x should have a DW_AT_location: {vars:?}");
    assert!(x.2, "variable x should have a DW_AT_type: {vars:?}");
}

#[test]
fn wasm_debug_info_contains_procedure_parameter_name() {
    let output = temp_output_path("wasm-dwarf-param").with_extension("wasm");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    options.debug_info = true;
    let source = SourceFile::anonymous(
        r#"begin
  integer procedure inc(n); value n; integer n;
  begin
    inc := n + 1;
  end;
  integer y;
  y := inc(1);
  if y = 2 then OutText("ok") else OutText("bad");
  OutImage;
end;
"#,
    );

    let result = outimage::compile_with_options(&source, &options).expect("wasm -g compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let map_path = PathBuf::from(format!("{}.map", output.display()));
    let side_path = PathBuf::from(format!("{}.sim-map", output.display()));
    let sections = wasm_custom_sections(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_file(&map_path);
    let _ = std::fs::remove_file(&side_path);

    let vars = wasm_dwarf_variable_names(&sections);
    let n = vars
        .iter()
        .find(|(name, _, _)| name == "n")
        .unwrap_or_else(|| panic!("expected parameter n in wasm DWARF, got {vars:?}"));
    assert!(n.1, "parameter n should have a DW_AT_location: {vars:?}");
    assert!(n.2, "parameter n should have a DW_AT_type: {vars:?}");
}

#[test]
fn wasm_debug_info_contains_class_structure_and_field_members() {
    let output = temp_output_path("wasm-dwarf-struct").with_extension("wasm");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    options.debug_info = true;
    let source = SourceFile::anonymous(
        r#"begin
  class Point(x, y); integer x, y;
  begin end;
  ref(Point) p;
  p :- new Point(3, 4);
  integer s; s := p.x + p.y;
end;
"#,
    );

    let result = outimage::compile_with_options(&source, &options).expect("wasm -g compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let map_path = PathBuf::from(format!("{}.map", output.display()));
    let side_path = PathBuf::from(format!("{}.sim-map", output.display()));
    let sections = wasm_custom_sections(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_file(&map_path);
    let _ = std::fs::remove_file(&side_path);

    let structs = wasm_dwarf_struct_members(&sections);
    let point = structs
        .iter()
        .find(|(name, _)| name == "Point")
        .unwrap_or_else(|| panic!("expected DW_TAG_structure_type Point, got {structs:?}"));
    let members: std::collections::HashMap<&str, u64> =
        point.1.iter().map(|(n, o)| (n.as_str(), *o)).collect();
    assert_eq!(members.get("__class_id"), Some(&0), "members: {members:?}");
    assert_eq!(members.get("x"), Some(&8), "members: {members:?}");
    assert_eq!(members.get("y"), Some(&16), "members: {members:?}");
}

/// Collects `DW_TAG_structure_type` names and their member `(name, offset)` pairs
/// from wasm custom-section DWARF.
fn wasm_dwarf_struct_members(sections: &[(String, Vec<u8>)]) -> Vec<(String, Vec<(String, u64)>)> {
    let debug_info = sections
        .iter()
        .find(|(name, _)| name == ".debug_info")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_abbrev = sections
        .iter()
        .find(|(name, _)| name == ".debug_abbrev")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_str")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_line = sections
        .iter()
        .find(|(name, _)| name == ".debug_line")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_line_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_line_str")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);

    let endian = RunTimeEndian::Little;
    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let data: &[u8] = match id {
            SectionId::DebugInfo => debug_info,
            SectionId::DebugAbbrev => debug_abbrev,
            SectionId::DebugStr => debug_str,
            SectionId::DebugLine => debug_line,
            SectionId::DebugLineStr => debug_line_str,
            _ => &[],
        };
        Ok(EndianSlice::new(data, endian))
    };
    let dwarf = gimli::Dwarf::load(load_section).expect("load wasm dwarf");
    let mut structs = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        let mut entries = unit.entries();
        let mut current: Option<(String, Vec<(String, u64)>)> = None;
        while let Some(entry) = entries.next_dfs().expect("entry") {
            match entry.tag() {
                gimli::DW_TAG_structure_type => {
                    if let Some(finished) = current.take() {
                        structs.push(finished);
                    }
                    let name = entry
                        .attr_value(gimli::DW_AT_name)
                        .and_then(|attr| dwarf.attr_string(&unit, attr).ok())
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    current = Some((name, Vec::new()));
                }
                gimli::DW_TAG_member => {
                    if let Some((_, members)) = current.as_mut() {
                        let name = entry
                            .attr_value(gimli::DW_AT_name)
                            .and_then(|attr| dwarf.attr_string(&unit, attr).ok())
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let offset = entry
                            .attr_value(gimli::DW_AT_data_member_location)
                            .and_then(|attr| match attr {
                                gimli::AttributeValue::Udata(v) => Some(v),
                                gimli::AttributeValue::Data1(v) => Some(u64::from(v)),
                                gimli::AttributeValue::Data2(v) => Some(u64::from(v)),
                                gimli::AttributeValue::Data4(v) => Some(u64::from(v)),
                                gimli::AttributeValue::Data8(v) => Some(v),
                                gimli::AttributeValue::Sdata(v) if v >= 0 => Some(v as u64),
                                _ => None,
                            })
                            .unwrap_or(0);
                        members.push((name, offset));
                    }
                }
                _ => {
                    if let Some(finished) = current.take() {
                        structs.push(finished);
                    }
                }
            }
        }
        if let Some(finished) = current {
            structs.push(finished);
        }
    }
    structs
}

/// `(name, has_location, has_type)` for wasm DWARF variables/params.
fn wasm_dwarf_variable_names(sections: &[(String, Vec<u8>)]) -> Vec<(String, bool, bool)> {
    let debug_info = sections
        .iter()
        .find(|(name, _)| name == ".debug_info")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_abbrev = sections
        .iter()
        .find(|(name, _)| name == ".debug_abbrev")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_str")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_line = sections
        .iter()
        .find(|(name, _)| name == ".debug_line")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    let debug_line_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_line_str")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);

    let endian = RunTimeEndian::Little;
    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let data: &[u8] = match id {
            SectionId::DebugInfo => debug_info,
            SectionId::DebugAbbrev => debug_abbrev,
            SectionId::DebugStr => debug_str,
            SectionId::DebugLine => debug_line,
            SectionId::DebugLineStr => debug_line_str,
            _ => &[],
        };
        Ok(EndianSlice::new(data, endian))
    };
    let dwarf = gimli::Dwarf::load(load_section).expect("load wasm dwarf");
    let mut names = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs().expect("entry") {
            let is_var = entry.tag() == gimli::DW_TAG_variable;
            let is_param = entry.tag() == gimli::DW_TAG_formal_parameter;
            if !is_var && !is_param {
                continue;
            }
            let Some(name_attr) = entry.attr_value(gimli::DW_AT_name) else {
                continue;
            };
            let name = dwarf
                .attr_string(&unit, name_attr)
                .expect("attr string")
                .to_string_lossy()
                .into_owned();
            let has_location = entry.attr_value(gimli::DW_AT_location).is_some();
            let has_type = entry.attr_value(gimli::DW_AT_type).is_some();
            names.push((name, has_location, has_type));
        }
    }
    names
}

#[test]
fn wasm_without_debug_flag_has_no_dwarf_custom_sections() {
    let output = temp_output_path("wasm-nodwarf").with_extension("wasm");
    let options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    let source = SourceFile::anonymous("begin OutText(\"hi\"); OutImage; end;");

    let result = outimage::compile_with_options(&source, &options).expect("wasm compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let sections = wasm_custom_sections(&bytes);
    let _ = std::fs::remove_file(&artifact);

    assert!(
        !sections.iter().any(|(name, _)| name.starts_with(".debug_")),
        "non-debug wasm build should not contain DWARF custom sections: {:?}",
        sections.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    assert!(
        !sections.iter().any(|(name, _)| name == "sourceMappingURL"),
        "non-debug wasm build should not contain a sourceMappingURL section"
    );
}

fn dwarf_variable_names(bytes: &[u8]) -> Vec<(String, bool)> {
    let file = object::File::parse(bytes).expect("parse executable");
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };

    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let name = id.name();
        let macho_name = name.replace('.', "__");
        let data = file
            .sections()
            .find_map(|section| {
                let section_name = section.name().ok()?;
                if section_name == name || section_name == macho_name {
                    Some(section.data().expect("section data"))
                } else {
                    None
                }
            })
            .unwrap_or(&[]);
        Ok(EndianSlice::new(data, endian))
    };

    let dwarf = gimli::Dwarf::load(load_section).expect("load dwarf");
    let mut names = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs().expect("entry") {
            let is_var = entry.tag() == gimli::DW_TAG_variable;
            let is_param = entry.tag() == gimli::DW_TAG_formal_parameter;
            if !is_var && !is_param {
                continue;
            }
            let Some(name_attr) = entry.attr_value(gimli::DW_AT_name) else {
                continue;
            };
            let name = dwarf
                .attr_string(&unit, name_attr)
                .expect("attr string")
                .to_string_lossy()
                .into_owned();
            let has_location = entry.attr_value(gimli::DW_AT_location).is_some();
            names.push((name, has_location));
        }
    }
    names
}

#[cfg(not(windows))]
#[test]
fn dwarf_contains_named_local_with_location() {
    // Unused after assignment still locates: `-g` keeps named locals live via
    // stack homes + end-of-function keep-alive stores.
    let source = "begin\n  integer x;\n  x := 42;\n  OutText(\"hi\");\n  OutImage;\nend;\n";
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF in binary or dSYM");
    let vars = dwarf_variable_names(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    let has_x = vars.iter().any(|(name, _)| name == "x");
    assert!(has_x, "expected variable x in DWARF, got {vars:?}");
    // GNU ld + CRT can drop DW_AT_location on unused Simula locals while still
    // emitting the name (and extra C `x` locals from libc). Darwin ld keeps it.
    if !cfg!(target_os = "linux") {
        assert!(
            vars.iter().any(|(name, has_loc)| name == "x" && *has_loc),
            "expected variable x with DW_AT_location, got {vars:?}"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn dwarf_contains_procedure_parameter_name() {
    let source = r#"begin
  integer procedure inc(n); value n; integer n;
  begin
    inc := n + 1;
  end;
  integer y;
  y := inc(1);
end;
"#;
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF in binary or dSYM");
    let vars = dwarf_variable_names(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    let n = vars
        .iter()
        .find(|(name, _)| name == "n")
        .unwrap_or_else(|| panic!("expected parameter n in DWARF, got {vars:?}"));
    assert!(n.1, "parameter n should have a DW_AT_location: {vars:?}");
}

/// Collects `DW_TAG_structure_type` names and their member `(name, offset)` pairs.
fn dwarf_struct_members(bytes: &[u8]) -> Vec<(String, Vec<(String, u64)>)> {
    let file = object::File::parse(bytes).expect("parse object");
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };

    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let name = id.name();
        let macho_name = name.replace('.', "__");
        let data = file
            .sections()
            .find_map(|section| {
                let section_name = section.name().ok()?;
                if section_name == name || section_name == macho_name {
                    Some(section.data().expect("section data"))
                } else {
                    None
                }
            })
            .unwrap_or(&[]);
        Ok(EndianSlice::new(data, endian))
    };

    let dwarf = gimli::Dwarf::load(load_section).expect("load dwarf");
    let mut structs = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        let mut entries = unit.entries();
        let mut current: Option<(String, Vec<(String, u64)>)> = None;
        while let Some(entry) = entries.next_dfs().expect("entry") {
            match entry.tag() {
                gimli::DW_TAG_structure_type => {
                    if let Some(finished) = current.take() {
                        structs.push(finished);
                    }
                    let name = entry
                        .attr_value(gimli::DW_AT_name)
                        .and_then(|attr| dwarf.attr_string(&unit, attr).ok())
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    current = Some((name, Vec::new()));
                }
                gimli::DW_TAG_member => {
                    if let Some((_, members)) = current.as_mut() {
                        let name = entry
                            .attr_value(gimli::DW_AT_name)
                            .and_then(|attr| dwarf.attr_string(&unit, attr).ok())
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let offset = entry
                            .attr_value(gimli::DW_AT_data_member_location)
                            .and_then(|attr| match attr {
                                gimli::AttributeValue::Udata(v) => Some(v),
                                gimli::AttributeValue::Data1(v) => Some(u64::from(v)),
                                gimli::AttributeValue::Data2(v) => Some(u64::from(v)),
                                gimli::AttributeValue::Data4(v) => Some(u64::from(v)),
                                gimli::AttributeValue::Data8(v) => Some(v),
                                gimli::AttributeValue::Sdata(v) if v >= 0 => Some(v as u64),
                                _ => None,
                            })
                            .unwrap_or(0);
                        members.push((name, offset));
                    }
                }
                _ => {
                    if let Some(finished) = current.take() {
                        structs.push(finished);
                    }
                }
            }
        }
        if let Some(finished) = current {
            structs.push(finished);
        }
    }
    structs
}

#[cfg(not(windows))]
#[test]
fn dwarf_contains_class_structure_and_field_members() {
    let source = r#"begin
  class Point(x, y); integer x, y;
  begin end;
  ref(Point) p;
  p :- new Point(3, 4);
  integer s; s := p.x + p.y;
end;
"#;
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF in binary or dSYM");
    let structs = dwarf_struct_members(&bytes);
    let vars = dwarf_variable_names(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    let point = structs
        .iter()
        .find(|(name, _)| name == "Point")
        .unwrap_or_else(|| panic!("expected DW_TAG_structure_type Point, got {structs:?}"));
    let members: std::collections::HashMap<&str, u64> =
        point.1.iter().map(|(n, o)| (n.as_str(), *o)).collect();
    assert_eq!(members.get("__class_id"), Some(&0), "members: {members:?}");
    assert_eq!(members.get("x"), Some(&8), "members: {members:?}");
    assert_eq!(members.get("y"), Some(&16), "members: {members:?}");

    let p = vars
        .iter()
        .find(|(name, _)| name == "p")
        .unwrap_or_else(|| panic!("expected object local p in DWARF, got {vars:?}"));
    assert!(p.1, "object local p should have a DW_AT_location: {vars:?}");
}

#[cfg(not(windows))]
#[test]
fn dwarf_prefixed_class_includes_prefix_fields() {
    let source = r#"begin
  class Point(x); integer x; begin end;
  Point class Polar(r); real r; begin end;
  ref(Polar) p;
  p :- new Polar(1, 2.0);
end;
"#;
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF");
    let structs = dwarf_struct_members(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    let polar = structs
        .iter()
        .find(|(name, _)| name == "Polar")
        .unwrap_or_else(|| panic!("expected Polar struct, got {structs:?}"));
    let members: std::collections::HashMap<&str, u64> =
        polar.1.iter().map(|(n, o)| (n.as_str(), *o)).collect();
    assert!(
        members.contains_key("x"),
        "Polar should include prefix field x: {members:?}"
    );
    assert!(
        members.contains_key("r"),
        "Polar should include field r: {members:?}"
    );
    assert!(
        members["x"] < members["r"],
        "prefix field x should precede r: {members:?}"
    );
}

/// A component's continuation lives on its own stack, not in a synthetic
/// attribute, so a debugger sees the object's declared attributes and nothing
/// else. The statement-index splitter this replaced kept a resume PC in
/// `__simrt_coro_pc`, which showed up here.
#[cfg(not(windows))]
#[test]
fn dwarf_shows_no_synthetic_continuation_field_on_a_process() {
    let source = r#"Simulation begin
  Process class Worker;
  begin
    integer count;
    detach;
    count := 1;
  end;
  ref(Worker) w;
  w :- new Worker;
  activate w;
end;"#;
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF in binary or dSYM");
    let structs = dwarf_struct_members(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    let worker = structs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Worker"))
        .unwrap_or_else(|| panic!("expected DW_TAG_structure_type Worker, got {structs:?}"));
    let members: std::collections::HashMap<&str, u64> =
        worker.1.iter().map(|(n, o)| (n.as_str(), *o)).collect();
    assert!(
        members.contains_key("count"),
        "Worker should expose its declared attributes: {members:?}"
    );
    assert!(
        !members.keys().any(|name| name.contains("coro_pc")),
        "a component keeps no continuation in a field: {members:?}"
    );
}

/// Resolves `DW_AT_type` chains to the innermost named type (follows pointers).
fn dwarf_named_type<'a>(
    dwarf: &gimli::Dwarf<EndianSlice<'a, RunTimeEndian>>,
    unit: &gimli::Unit<EndianSlice<'a, RunTimeEndian>>,
    mut attr: gimli::AttributeValue<EndianSlice<'a, RunTimeEndian>>,
) -> Option<String> {
    for _ in 0..8 {
        let gimli::AttributeValue::UnitRef(offset) = attr else {
            return None;
        };
        let entry = unit.entry(offset).ok()?;
        if let Some(name) = entry
            .attr_value(gimli::DW_AT_name)
            .and_then(|a| dwarf.attr_string(unit, a).ok())
            .map(|s| s.to_string_lossy().into_owned())
        {
            return Some(name);
        }
        // Follow pointer / typedef / const wrappers without a name.
        attr = entry.attr_value(gimli::DW_AT_type)?;
    }
    None
}

fn dwarf_variable_pointee_types(bytes: &[u8]) -> Vec<(String, String)> {
    let file = object::File::parse(bytes).expect("parse object");
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let load_section = |id: SectionId| -> Result<EndianSlice<RunTimeEndian>, gimli::Error> {
        let name = id.name();
        let macho_name = name.replace('.', "__");
        let data = file
            .sections()
            .find_map(|section| {
                let section_name = section.name().ok()?;
                if section_name == name || section_name == macho_name {
                    Some(section.data().expect("section data"))
                } else {
                    None
                }
            })
            .unwrap_or(&[]);
        Ok(EndianSlice::new(data, endian))
    };
    let dwarf = gimli::Dwarf::load(load_section).expect("load dwarf");
    let mut out = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs().expect("entry") {
            if entry.tag() != gimli::DW_TAG_variable
                && entry.tag() != gimli::DW_TAG_formal_parameter
            {
                continue;
            }
            let Some(name) = entry
                .attr_value(gimli::DW_AT_name)
                .and_then(|attr| dwarf.attr_string(&unit, attr).ok())
                .map(|s| s.to_string_lossy().into_owned())
            else {
                continue;
            };
            let Some(ty_attr) = entry.attr_value(gimli::DW_AT_type) else {
                continue;
            };
            if let Some(ty_name) = dwarf_named_type(&dwarf, &unit, ty_attr) {
                out.push((name, ty_name));
            }
        }
    }
    out
}

#[cfg(not(windows))]
#[test]
fn dwarf_text_and_array_locals_use_runtime_struct_types() {
    let source = r#"begin
  text t;
  t :- Copy("hi");
  integer array a(1:3);
  a(1) := 11;
end;
"#;
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF");
    let structs = dwarf_struct_members(&bytes);
    let vars = dwarf_variable_pointee_types(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    assert!(
        structs.iter().any(|(n, m)| n == "SimrtTextFrame"
            && m.iter().any(|(mn, _)| mn == "obj")
            && m.iter().any(|(mn, _)| mn == "length")),
        "expected SimrtTextFrame members, got {structs:?}"
    );
    assert!(
        structs
            .iter()
            .any(|(n, m)| n == "SimrtArrayI64" && m.iter().any(|(mn, _)| mn == "ndims")),
        "expected SimrtArrayI64.ndims, got {structs:?}"
    );

    let t = vars
        .iter()
        .find(|(n, _)| n == "t")
        .unwrap_or_else(|| panic!("expected text local t, got {vars:?}"));
    assert!(
        t.1.contains("SimrtTextFrame"),
        "text local should point at SimrtTextFrame, got {}",
        t.1
    );
    let a = vars
        .iter()
        .find(|(n, _)| n == "a")
        .unwrap_or_else(|| panic!("expected array local a, got {vars:?}"));
    assert!(
        a.1.contains("SimrtArrayI64"),
        "array local should point at SimrtArrayI64, got {}",
        a.1
    );
}

#[cfg(not(windows))]
#[test]
fn dwarf_object_text_and_ref_fields_are_typed() {
    let source = r#"begin
  class Point; begin integer x; end;
  class Box;
  begin
    text caption;
    ref(Point) p;
  end;
  ref(Box) b;
  b :- new Box;
end;
"#;
    let (artifact, map_path) = compile_with_debug(source);
    let bytes = debug_info_bytes(&artifact).expect("expected DWARF");
    let structs = dwarf_struct_members(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_dir_all(artifact.with_extension("dSYM"));
    let _ = std::fs::remove_file(&map_path);

    let box_struct = structs
        .iter()
        .find(|(n, _)| n == "Box")
        .unwrap_or_else(|| panic!("expected Box struct, got {structs:?}"));
    assert!(
        box_struct.1.iter().any(|(n, _)| n == "caption"),
        "Box should have caption: {box_struct:?}"
    );
    assert!(
        box_struct.1.iter().any(|(n, _)| n == "p"),
        "Box should have p: {box_struct:?}"
    );
    // Presence of SimrtTextFrame + Point proves typed emission for those fields.
    assert!(
        structs.iter().any(|(n, _)| n == "SimrtTextFrame"),
        "expected text frame DIE for caption field typing"
    );
    assert!(
        structs.iter().any(|(n, _)| n == "Point"),
        "expected Point DIE for ref field typing"
    );
}

#[test]
fn wasm_dwarf_embeds_text_and_array_runtime_types() {
    let output = temp_output_path("wasm-text-array").with_extension("wasm");
    let mut options = CompileOptions::for_compile(output.clone(), CompileTarget::WasmNode);
    options.debug_info = true;
    let source = SourceFile::anonymous(
        "begin\n  text t;\n  t :- Copy(\"hi\");\n  integer array a(1:2);\n  a(1) := 1;\nend;\n",
    );
    let result = outimage::compile_with_options(&source, &options).expect("wasm -g compile");
    let CompileResult::Artifact(artifact) = result else {
        panic!("expected wasm artifact");
    };
    let bytes = std::fs::read(&artifact).expect("read wasm");
    let map_path = PathBuf::from(format!("{}.map", output.display()));
    let side_path = PathBuf::from(format!("{}.sim-map", output.display()));
    let sections = wasm_custom_sections(&bytes);
    let _ = std::fs::remove_file(&artifact);
    let _ = std::fs::remove_file(&map_path);
    let _ = std::fs::remove_file(&side_path);

    let debug_str = sections
        .iter()
        .find(|(name, _)| name == ".debug_str")
        .map(|(_, data)| data.as_slice())
        .unwrap_or(&[]);
    assert!(
        debug_str
            .windows(b"SimrtTextFrame".len())
            .any(|w| w == b"SimrtTextFrame"),
        "wasm DWARF should name SimrtTextFrame"
    );
    assert!(
        debug_str
            .windows(b"SimrtArrayI64".len())
            .any(|w| w == b"SimrtArrayI64"),
        "wasm DWARF should name SimrtArrayI64"
    );
}
