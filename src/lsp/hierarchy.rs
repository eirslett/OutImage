//! Call hierarchy and type hierarchy feature helpers.

use serde_json::json;
use tower_lsp_server::ls_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Location, Position,
    Range, SymbolKind as LspSymbolKind, TypeHierarchyItem, Uri,
};

use crate::types::Type;

use super::analysis::AnalysisSnapshot;
use super::position::{Encoding, byte_span_to_range, position_to_byte};
use super::symbols::{SymbolId, SymbolIndex, SymbolKind};

/// Encode document URI + symbol id into hierarchy item `data`.
fn item_data(uri: &Uri, id: SymbolId) -> serde_json::Value {
    json!({ "uri": uri.as_str(), "id": id.0 })
}

fn parse_item_data(data: &Option<serde_json::Value>) -> Option<(String, SymbolId)> {
    let obj = data.as_ref()?.as_object()?;
    let uri = obj.get("uri")?.as_str()?.to_owned();
    let id = obj.get("id")?.as_u64()? as usize;
    Some((uri, SymbolId(id)))
}

fn procedure_item(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    id: SymbolId,
    encoding: Encoding,
) -> CallHierarchyItem {
    let symbol = index.symbol(id);
    CallHierarchyItem {
        name: symbol.name.clone(),
        kind: LspSymbolKind::FUNCTION,
        tags: None,
        detail: Some(symbol.detail.clone()),
        uri: uri.clone(),
        range: byte_span_to_range(&snap.text, symbol.full_span.clone(), encoding),
        selection_range: byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
        data: Some(item_data(uri, id)),
    }
}

fn class_item(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    id: SymbolId,
    encoding: Encoding,
) -> TypeHierarchyItem {
    let symbol = index.symbol(id);
    TypeHierarchyItem {
        name: symbol.name.clone(),
        kind: LspSymbolKind::CLASS,
        tags: None,
        detail: Some(symbol.detail.clone()),
        uri: uri.clone(),
        range: byte_span_to_range(&snap.text, symbol.full_span.clone(), encoding),
        selection_range: byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
        data: Some(item_data(uri, id)),
    }
}

/// Innermost procedure whose `full_span` contains `offset`.
pub fn enclosing_procedure(index: &SymbolIndex, offset: usize) -> Option<SymbolId> {
    index
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.kind == SymbolKind::Procedure
                && offset >= s.full_span.start
                && offset < s.full_span.end
        })
        .min_by_key(|(_, s)| s.full_span.end.saturating_sub(s.full_span.start))
        .map(|(i, _)| SymbolId(i))
}

pub fn prepare_call_hierarchy(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
) -> Option<Vec<CallHierarchyItem>> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let id = index.resolve_at_offset(offset)?;
    if index.symbol(id).kind != SymbolKind::Procedure {
        return None;
    }
    Some(vec![procedure_item(snap, index, uri, id, encoding)])
}

pub fn incoming_calls(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    item: &CallHierarchyItem,
    encoding: Encoding,
) -> Option<Vec<CallHierarchyIncomingCall>> {
    let (_, id) = parse_item_data(&item.data)?;
    let mut by_caller: std::collections::HashMap<SymbolId, Vec<Range>> =
        std::collections::HashMap::new();
    for (span, _) in index.references_of(id, false) {
        let Some(caller) = enclosing_procedure(index, span.start) else {
            continue;
        };
        if caller == id {
            continue;
        }
        by_caller
            .entry(caller)
            .or_default()
            .push(byte_span_to_range(&snap.text, span, encoding));
    }
    let mut out = Vec::new();
    for (caller, from_ranges) in by_caller {
        out.push(CallHierarchyIncomingCall {
            from: procedure_item(snap, index, uri, caller, encoding),
            from_ranges,
        });
    }
    out.sort_by(|a, b| a.from.name.cmp(&b.from.name));
    Some(out)
}

pub fn outgoing_calls(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    item: &CallHierarchyItem,
    encoding: Encoding,
) -> Option<Vec<CallHierarchyOutgoingCall>> {
    let (_, id) = parse_item_data(&item.data)?;
    let proc = index.symbol(id);
    let mut by_callee: std::collections::HashMap<SymbolId, Vec<Range>> =
        std::collections::HashMap::new();
    for u in &index.uses {
        if u.span.start < proc.full_span.start || u.span.start >= proc.full_span.end {
            continue;
        }
        let Some(callee) = u.definition else {
            continue;
        };
        if index.symbol(callee).kind != SymbolKind::Procedure || callee == id {
            continue;
        }
        by_callee
            .entry(callee)
            .or_default()
            .push(byte_span_to_range(&snap.text, u.span.clone(), encoding));
    }
    let mut out = Vec::new();
    for (callee, from_ranges) in by_callee {
        out.push(CallHierarchyOutgoingCall {
            to: procedure_item(snap, index, uri, callee, encoding),
            from_ranges,
        });
    }
    out.sort_by(|a, b| a.to.name.cmp(&b.to.name));
    Some(out)
}

pub fn prepare_type_hierarchy(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let id = index.resolve_at_offset(offset)?;
    if index.symbol(id).kind != SymbolKind::Class {
        // From a `ref(C)` variable, jump to the class hierarchy root.
        if let Some(Type::ObjectRef(class)) = index.symbol(id).ty.as_ref() {
            let class_id = index.find_class(class)?;
            return Some(vec![class_item(snap, index, uri, class_id, encoding)]);
        }
        return None;
    }
    Some(vec![class_item(snap, index, uri, id, encoding)])
}

pub fn type_supertypes(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    item: &TypeHierarchyItem,
    encoding: Encoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let (_, id) = parse_item_data(&item.data)?;
    let name = &index.symbol(id).name;
    let Some(prefix) = index.class_prefix(name) else {
        return Some(Vec::new());
    };
    let prefix_id = index.find_class(&prefix)?;
    Some(vec![class_item(snap, index, uri, prefix_id, encoding)])
}

pub fn type_subtypes(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    item: &TypeHierarchyItem,
    encoding: Encoding,
) -> Option<Vec<TypeHierarchyItem>> {
    let (_, id) = parse_item_data(&item.data)?;
    let name = index.symbol(id).name.clone();
    let mut out = Vec::new();
    for child_id in index.class_subtypes(&name) {
        out.push(class_item(snap, index, uri, child_id, encoding));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(out)
}

/// Matching procedure definitions for a virtual (same name along prefix / subclass chain).
pub fn goto_implementations(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
) -> Vec<Location> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let Some(id) = index.resolve_at_offset(offset) else {
        return Vec::new();
    };
    let symbol = index.symbol(id);
    if symbol.kind != SymbolKind::Procedure {
        return Vec::new();
    }
    let Some(container) = symbol.container else {
        return Vec::new();
    };
    if index.symbol(container).kind != SymbolKind::Class {
        return Vec::new();
    }
    let class_name = index.symbol(container).name.clone();
    let mut out = Vec::new();
    // Prefix chain upward + subclasses downward: any procedure with the same name.
    for related in index.related_class_procedures(&class_name, &symbol.name) {
        if related == id {
            continue;
        }
        let other = index.symbol(related);
        out.push(Location::new(
            uri.clone(),
            byte_span_to_range(&snap.text, other.name_span.clone(), encoding),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};
    use std::str::FromStr;

    fn uri() -> Uri {
        Uri::from_str("file:///tmp/h.sim").unwrap()
    }

    #[test]
    fn call_hierarchy_outgoing() {
        let src = r#"
begin
  procedure leaf; begin end;
  procedure root; begin leaf; end;
  root;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let root = index
            .symbols
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case("root"))
            .map(SymbolId)
            .unwrap();
        let item = procedure_item(&snap, index, &uri(), root, Encoding::Utf16);
        let out = outgoing_calls(&snap, index, &uri(), &item, Encoding::Utf16).unwrap();
        assert!(
            out.iter().any(|c| c.to.name.eq_ignore_ascii_case("leaf")),
            "{out:?}"
        );
    }

    #[test]
    fn type_hierarchy_prefix() {
        let src = r#"
begin
  class base; begin end;
  base class derived; begin end;
end
"#;
        let snap = analyze_document(src, &AnalysisOptions::default());
        let index = snap.symbols.as_ref().unwrap();
        let derived = index.find_class("derived").unwrap();
        let item = class_item(&snap, index, &uri(), derived, Encoding::Utf16);
        let supers = type_supertypes(&snap, index, &uri(), &item, Encoding::Utf16).unwrap();
        assert_eq!(supers.len(), 1);
        assert!(supers[0].name.eq_ignore_ascii_case("base"));
        let base = index.find_class("base").unwrap();
        let base_item = class_item(&snap, index, &uri(), base, Encoding::Utf16);
        let subs = type_subtypes(&snap, index, &uri(), &base_item, Encoding::Utf16).unwrap();
        assert!(subs.iter().any(|s| s.name.eq_ignore_ascii_case("derived")));
    }
}
