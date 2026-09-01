//! Phase 3 MVP debug tooling: a JSON MIR side-map plus Source Map v3.
//!
//! - `.sim-map` — custom JSON keyed by MIR function/block/op (tests/tooling).
//! - `.map` (Source Map v3) — for native: one generated line per MIR op; for
//!   wasm (`-g`): a single generated line whose columns are **byte offsets into
//!   the `.wasm` file** (WebAssembly tooling convention), plus a
//!   `sourceMappingURL` custom section in the module.

use serde::{Deserialize, Serialize};

use crate::mir::Module;
use crate::source::SourceFile;

pub use crate::source::span_to_line_col;

/// One MIR op's source location, keyed by function/block/op-index so a
/// consumer can cross-reference it against a `Module::dump()` listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    /// Index of the op within its basic block (matches dump order).
    pub op: usize,
    #[serde(rename = "fn")]
    pub function: String,
    pub block: usize,
    /// `[start, end)` byte offsets into the source file.
    pub span: [usize; 2],
    pub line: usize,
    pub column: usize,
    /// Absolute byte offset into the `.wasm` module for this MIR op's first
    /// emitted instruction (wasm `-g` only).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub wasm_offset: Option<u32>,
}

/// A whole module's worth of mappings, serialized as simple JSON (custom
/// format, not Source Map v3 — see module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMap {
    pub version: u32,
    pub file: String,
    pub mappings: Vec<Mapping>,
}

/// Source Map v3 document (https://sourcemaps.info/spec.html).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMapV3 {
    pub version: u32,
    pub file: String,
    pub sources: Vec<String>,
    #[serde(rename = "sourcesContent", skip_serializing_if = "Option::is_none")]
    pub sources_content: Option<Vec<String>>,
    pub names: Vec<String>,
    pub mappings: String,
}

impl SourceMap {
    /// Builds a source map for `module`, resolving spans against `source`'s
    /// text. Ops with an empty `0..0` span are synthetic control-flow
    /// scaffolding the lowerer inserted itself (e.g. the jump back to a
    /// `while` header) — they have no real source location, so they are
    /// **omitted** from `mappings` rather than mapped to line 1, column 1.
    pub fn build(module: &Module, source: &SourceFile) -> Self {
        let mut mappings = Vec::new();
        for function in &module.functions {
            for block in &function.blocks {
                for (op_index, spanned) in block.ops.iter().enumerate() {
                    if spanned.span.start == 0 && spanned.span.end == 0 {
                        continue;
                    }
                    let (line, column) = span_to_line_col(&source.text, spanned.span.start);
                    mappings.push(Mapping {
                        op: op_index,
                        function: function.name.clone(),
                        block: block.id.0,
                        span: [spanned.span.start, spanned.span.end],
                        line,
                        column,
                        wasm_offset: None,
                    });
                }
            }
        }
        Self {
            version: 1,
            file: source.name.clone(),
            mappings,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Converts this MIR side-map into Source Map v3 (one generated line per
    /// MIR mapping). Used for native `-g` tooling.
    pub fn to_source_map_v3(&self, generated_file: &str, source_text: Option<&str>) -> SourceMapV3 {
        SourceMapV3 {
            version: 3,
            file: generated_file.to_string(),
            sources: vec![self.file.clone()],
            sources_content: source_text.map(|text| vec![text.to_string()]),
            names: Vec::new(),
            mappings: encode_v3_mappings(&self.mappings),
        }
    }

    /// Source Map v3 for wasm: a single generated line whose columns are
    /// absolute byte offsets into the `.wasm` file (tooling convention).
    pub fn to_wasm_pc_source_map_v3(
        &self,
        generated_file: &str,
        source_text: Option<&str>,
    ) -> SourceMapV3 {
        SourceMapV3 {
            version: 3,
            file: generated_file.to_string(),
            sources: vec![self.file.clone()],
            sources_content: source_text.map(|text| vec![text.to_string()]),
            names: Vec::new(),
            mappings: encode_v3_wasm_pc_mappings(&self.mappings),
        }
    }
}

impl SourceMapV3 {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_vlq(value: i32) -> String {
    let mut vlq = if value < 0 {
        ((-value) << 1) | 1
    } else {
        value << 1
    } as u32;
    let mut out = String::new();
    loop {
        let mut digit = vlq & 0b11111;
        vlq >>= 5;
        if vlq != 0 {
            digit |= 0b100000;
        }
        out.push(BASE64[digit as usize] as char);
        if vlq == 0 {
            break;
        }
    }
    out
}

fn encode_v3_mappings(mappings: &[Mapping]) -> String {
    let mut out = String::new();
    let mut prev_gen_col = 0i32;
    let mut prev_src = 0i32;
    let mut prev_src_line = 0i32;
    let mut prev_src_col = 0i32;
    for (index, mapping) in mappings.iter().enumerate() {
        if index > 0 {
            out.push(';');
            prev_gen_col = 0;
        }
        let gen_col = 0i32;
        let src = 0i32;
        let src_line = mapping.line.saturating_sub(1) as i32;
        let src_col = mapping.column.saturating_sub(1) as i32;
        out.push_str(&encode_vlq(gen_col - prev_gen_col));
        out.push_str(&encode_vlq(src - prev_src));
        out.push_str(&encode_vlq(src_line - prev_src_line));
        out.push_str(&encode_vlq(src_col - prev_src_col));
        prev_gen_col = gen_col;
        prev_src = src;
        prev_src_line = src_line;
        prev_src_col = src_col;
    }
    out
}

/// One generated line; each segment's column is a wasm file byte offset.
fn encode_v3_wasm_pc_mappings(mappings: &[Mapping]) -> String {
    let mut items: Vec<(u32, usize, usize)> = mappings
        .iter()
        .filter_map(|mapping| {
            mapping
                .wasm_offset
                .map(|offset| (offset, mapping.line, mapping.column))
        })
        .collect();
    items.sort_by_key(|(offset, _, _)| *offset);

    let mut out = String::new();
    let mut prev_gen_col = 0i32;
    let mut prev_src = 0i32;
    let mut prev_src_line = 0i32;
    let mut prev_src_col = 0i32;
    for (index, (offset, line, column)) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let gen_col = *offset as i32;
        let src = 0i32;
        let src_line = (*line as i32).saturating_sub(1);
        let src_col = (*column as i32).saturating_sub(1);
        out.push_str(&encode_vlq(gen_col - prev_gen_col));
        out.push_str(&encode_vlq(src - prev_src));
        out.push_str(&encode_vlq(src_line - prev_src_line));
        out.push_str(&encode_vlq(src_col - prev_src_col));
        prev_gen_col = gen_col;
        prev_src = src;
        prev_src_line = src_line;
        prev_src_col = src_col;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::lower_program;
    use crate::parse::test_support::parse_program;

    // --- span_to_line_col -----------------------------------------------

    #[test]
    fn empty_source_is_line_one_column_one() {
        assert_eq!(span_to_line_col("", 0), (1, 1));
    }

    #[test]
    fn offset_zero_is_line_one_column_one() {
        assert_eq!(span_to_line_col("hello", 0), (1, 1));
    }

    #[test]
    fn first_line_counts_columns() {
        // "hello" — offset 3 is right before the second 'l' (index 3).
        assert_eq!(span_to_line_col("hello", 3), (1, 4));
    }

    #[test]
    fn mid_line_on_a_later_line() {
        let source = "line one\nline two\nline three";
        // Offset of the 't' in "two" (line 2, 6th character).
        let offset = source.find("two").unwrap();
        assert_eq!(span_to_line_col(source, offset), (2, 6));
    }

    #[test]
    fn start_of_each_line_is_column_one() {
        let source = "aaa\nbbb\nccc";
        let second_line_start = source.find("bbb").unwrap();
        let third_line_start = source.find("ccc").unwrap();
        assert_eq!(span_to_line_col(source, 0), (1, 1));
        assert_eq!(span_to_line_col(source, second_line_start), (2, 1));
        assert_eq!(span_to_line_col(source, third_line_start), (3, 1));
    }

    #[test]
    fn windows_line_endings_advance_the_line_after_the_lf() {
        let source = "aaa\r\nbbb\r\nccc";
        let second_line_start = source.find("bbb").unwrap();
        let third_line_start = source.find("ccc").unwrap();
        assert_eq!(span_to_line_col(source, second_line_start), (2, 1));
        assert_eq!(span_to_line_col(source, third_line_start), (3, 1));
        // The '\r' itself is still on the first line, just before the '\n'.
        let cr_offset = source.find('\r').unwrap();
        assert_eq!(span_to_line_col(source, cr_offset), (1, 4));
    }

    #[test]
    fn multi_byte_characters_count_as_one_column() {
        // "héllo": 'h' (1 byte), 'é' (2 bytes), then "llo".
        let source = "héllo\nworld";
        let l_offset = source.find('l').unwrap();
        // h=col1, é=col2, so the first 'l' is column 3.
        assert_eq!(span_to_line_col(source, l_offset), (1, 3));

        let world_offset = source.find("world").unwrap();
        assert_eq!(span_to_line_col(source, world_offset), (2, 1));
    }

    #[test]
    fn offset_past_end_clamps_to_the_end_of_the_source() {
        let source = "abc\ndef";
        let past_end = source.len() + 50;
        assert_eq!(
            span_to_line_col(source, past_end),
            span_to_line_col(source, source.len())
        );
        assert_eq!(span_to_line_col(source, past_end), (2, 4));
    }

    // --- SourceMap::build -------------------------------------------------

    fn build(source_text: &str) -> (SourceMap, Module) {
        let source = SourceFile::anonymous(source_text);
        let program = parse_program(source_text);
        let module = lower_program(&program).expect("expected lowering to succeed");
        let map = SourceMap::build(&module, &source);
        (map, module)
    }

    #[test]
    fn maps_assignment_and_out_text_to_their_lines() {
        let source_text = "begin\n  integer x;\n  x := 1;\n  OutText(\"hi\");\nend;\n";
        let (map, _module) = build(source_text);

        let assignment_line = map
            .mappings
            .iter()
            .find(|m| m.function == "main" && m.line == 3);
        assert!(
            assignment_line.is_some(),
            "expected a mapping on line 3 (the assignment): {map:#?}"
        );

        let out_text_line = map.mappings.iter().find(|m| m.line == 4);
        assert!(
            out_text_line.is_some(),
            "expected a mapping on line 4 (OutText): {map:#?}"
        );
    }

    #[test]
    fn every_mapping_references_the_main_function() {
        let (map, _module) = build("begin integer x; x := 1; end;");
        assert!(!map.mappings.is_empty());
        assert!(map.mappings.iter().all(|m| m.function == "main"));
    }

    #[test]
    fn synthetic_zero_zero_spans_are_omitted() {
        // The empty `begin end;` program lowers to a single `Return { value: None }`
        // op pushed with the synthetic `0..0` span (see `lower_program`).
        let (map, module) = build("begin end;");
        assert_eq!(
            module.functions[0].blocks[0].ops.len(),
            1,
            "sanity: only the synthetic return"
        );
        assert!(
            map.mappings.is_empty(),
            "the only op has an empty span and must be omitted: {map:#?}"
        );
    }

    #[test]
    fn if_else_omits_synthetic_jumps_but_keeps_branch_and_stores() {
        let (map, _module) = build("begin integer x; if x = 0 then x := 1 else x := 2; end;");
        // The `if`/`else` lowering inserts synthetic `Jump`s back to the merge
        // block with a `0..0` span; none of those may appear in the map.
        assert!(!map.mappings.iter().any(|m| m.span == [0, 0]));
        assert!(
            !map.mappings.is_empty(),
            "the branch/stores should still be mapped"
        );
    }

    #[test]
    fn serializes_to_the_documented_json_shape() {
        let (map, _module) = build("begin integer x; x := 1; end;");
        let json = map.to_json().expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["version"], 1);
        assert!(value["mappings"].is_array());
        let first = &value["mappings"][0];
        assert!(first["op"].is_number());
        assert!(first["fn"].is_string());
        assert!(first["block"].is_number());
        assert!(first["span"].is_array());
        assert!(first["line"].is_number());
        assert!(first["column"].is_number());
    }

    #[test]
    fn encode_vlq_matches_sourcemap_spec_examples() {
        assert_eq!(encode_vlq(0), "A");
        assert_eq!(encode_vlq(1), "C");
        assert_eq!(encode_vlq(-1), "D");
        assert_eq!(encode_vlq(15), "e");
        assert_eq!(encode_vlq(16), "gB");
    }

    #[test]
    fn source_map_v3_has_one_generated_line_per_mir_mapping() {
        let (map, _module) = build("begin integer x; x := 1; end;");
        assert!(!map.mappings.is_empty());
        let v3 = map.to_source_map_v3("out.wasm", Some("begin integer x; x := 1; end;"));
        assert_eq!(v3.version, 3);
        assert_eq!(v3.file, "out.wasm");
        assert_eq!(v3.sources, vec![map.file.clone()]);
        assert_eq!(
            v3.mappings.matches(';').count() + 1,
            map.mappings.len(),
            "mappings={:?}",
            v3.mappings
        );
        let json = v3.to_json().expect("serialize v3");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["version"], 3);
        assert!(value["mappings"].is_string());
        assert!(value["sourcesContent"].is_array());
    }

    #[test]
    fn wasm_pc_source_map_uses_one_line_and_offset_columns() {
        let mut map = build("begin integer x; x := 1; OutText(\"a\"); OutImage; end;").0;
        assert!(map.mappings.len() >= 2);
        map.mappings[0].wasm_offset = Some(100);
        map.mappings[1].wasm_offset = Some(150);
        let v3 = map.to_wasm_pc_source_map_v3("prog.wasm", None);
        assert!(
            !v3.mappings.contains(';'),
            "wasm PC map is a single generated line"
        );
        assert!(
            v3.mappings.contains(','),
            "multiple PC segments are comma-separated"
        );
    }
}
