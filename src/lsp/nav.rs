//! Cross-file navigation helpers and ENVIRONMENT virtual definitions.

use std::path::PathBuf;
use std::str::FromStr;

use tower_lsp_server::ls_types::{Location, Position, Uri};

use crate::environment::{is_environment_constant, is_environment_procedure};

use super::analysis::AnalysisSnapshot;
use super::position::{Encoding, byte_span_to_range, position_to_byte};
use super::symbols::{SymbolIndex, SymbolKind};
use super::workspace;

/// Go to definition with workspace / ENVIRONMENT fallbacks.
pub fn goto_definition_extended(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
    workspace_docs: &[(Uri, &AnalysisSnapshot, &SymbolIndex)],
) -> Option<Location> {
    let offset = position_to_byte(&snap.text, position, encoding);

    if let Some(id) = index.resolve_at_offset(offset) {
        let symbol = index.symbol(id);
        if symbol.is_external
            && let Some(loc) =
                find_workspace_definition(&symbol.name, symbol.kind, workspace_docs, encoding)
        {
            return Some(loc);
        }
        return Some(Location::new(
            uri.clone(),
            byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
        ));
    }

    // Unresolved identifier that matches an ENVIRONMENT builtin → stdlib virtual def.
    if let Some(tokens) = &snap.tokens
        && let Some(token) = super::symbols::token_at_offset(&tokens.tokens, offset)
        && let crate::lex::TokenKind::Identifier(name) = &token.kind
        && (is_environment_procedure(name) || is_environment_constant(name))
    {
        return environment_definition_location(name);
    }

    // Unresolved name that exists as a concrete export elsewhere.
    if let Some(tokens) = &snap.tokens
        && let Some(token) = super::symbols::token_at_offset(&tokens.tokens, offset)
        && let crate::lex::TokenKind::Identifier(name) = &token.kind
    {
        return find_workspace_definition(name, SymbolKind::Procedure, workspace_docs, encoding)
            .or_else(|| {
                find_workspace_definition(name, SymbolKind::Class, workspace_docs, encoding)
            });
    }

    None
}

fn find_workspace_definition(
    name: &str,
    prefer_kind: SymbolKind,
    workspace_docs: &[(Uri, &AnalysisSnapshot, &SymbolIndex)],
    encoding: Encoding,
) -> Option<Location> {
    let mut fallback = None;
    for (uri, snap, index) in workspace_docs {
        for symbol in &index.symbols {
            if !symbol.name.eq_ignore_ascii_case(name) || symbol.is_external {
                continue;
            }
            if !matches!(
                symbol.kind,
                SymbolKind::Procedure | SymbolKind::Class | SymbolKind::Variable
            ) {
                continue;
            }
            let loc = Location::new(
                uri.clone(),
                byte_span_to_range(&snap.text, symbol.name_span.clone(), encoding),
            );
            if symbol.kind == prefer_kind {
                return Some(loc);
            }
            fallback.get_or_insert(loc);
        }
    }
    fallback
}

/// ENVIRONMENT builtin → location in bundled `stdlib/environment.sim` when present.
pub fn environment_definition_location(name: &str) -> Option<Location> {
    let path = find_environment_sim()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let uri_str = workspace::path_to_uri(&path).ok()?;
    let uri = Uri::from_str(&uri_str).ok()?;
    let lower = name.to_ascii_lowercase();
    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start().to_ascii_lowercase();
        if trimmed.contains(&lower)
            && (trimmed.contains("procedure")
                || trimmed.contains("boolean")
                || trimmed.contains("integer")
                || trimmed.contains("real")
                || trimmed.contains("text")
                || trimmed.contains("character"))
        {
            // Point at the identifier occurrence on the line.
            if let Some(col) = line.to_ascii_lowercase().find(&lower) {
                let line_u = line_idx as u32;
                let col_u = col as u32;
                return Some(Location::new(
                    uri,
                    tower_lsp_server::ls_types::Range::new(
                        Position::new(line_u, col_u),
                        Position::new(line_u, col_u + name.len() as u32),
                    ),
                ));
            }
        }
    }
    None
}

fn find_environment_sim() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SIMRT_STDLIB") {
        let path = PathBuf::from(dir).join("environment.sim");
        if path.exists() {
            return Some(path);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("stdlib/environment.sim");
    if manifest.exists() {
        return Some(manifest);
    }
    None
}

/// References in the current doc plus same-name top-level symbols across the workspace.
pub fn find_references_extended(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    position: Position,
    encoding: Encoding,
    include_declaration: bool,
    workspace_docs: &[(Uri, &AnalysisSnapshot, &SymbolIndex)],
) -> Vec<Location> {
    let offset = position_to_byte(&snap.text, position, encoding);
    let Some(id) = index.resolve_at_offset(offset) else {
        return Vec::new();
    };
    let symbol = index.symbol(id);
    let mut out: Vec<Location> = index
        .references_of(id, include_declaration)
        .into_iter()
        .map(|(span, _)| Location::new(uri.clone(), byte_span_to_range(&snap.text, span, encoding)))
        .collect();

    // Cross-file: matching top-level declarations / uses of the same name.
    for (other_uri, other_snap, other_index) in workspace_docs {
        if other_uri.as_str() == uri.as_str() {
            continue;
        }
        for (i, other) in other_index.symbols.iter().enumerate() {
            if !other.name.eq_ignore_ascii_case(&symbol.name) {
                continue;
            }
            if other.kind != symbol.kind {
                continue;
            }
            let other_id = super::symbols::SymbolId(i);
            for (span, _) in other_index.references_of(other_id, include_declaration) {
                out.push(Location::new(
                    other_uri.clone(),
                    byte_span_to_range(&other_snap.text, span, encoding),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};

    #[test]
    fn environment_builtin_resolves_to_stdlib() {
        let loc = environment_definition_location("sqrt").expect("stdlib present");
        assert!(loc.uri.as_str().contains("environment.sim"));
    }

    #[test]
    fn external_resolves_across_workspace() {
        let def = analyze_document(
            "begin procedure helper; begin end; end",
            &AnalysisOptions::default(),
        );
        let use_doc = analyze_document(
            "external procedure helper;\nbegin helper; end",
            &AnalysisOptions::default(),
        );
        let def_uri = Uri::from_str("file:///tmp/def.sim").unwrap();
        let use_uri = Uri::from_str("file:///tmp/use.sim").unwrap();
        let def_index = def.symbols.as_ref().unwrap();
        let use_index = use_doc.symbols.as_ref().unwrap();
        let workspace = vec![(def_uri.clone(), &def, def_index)];
        // Cursor on `helper` in the call.
        let pos = Position::new(1, 7);
        let loc = goto_definition_extended(
            &use_doc,
            use_index,
            &use_uri,
            pos,
            Encoding::Utf16,
            &workspace,
        )
        .expect("cross-file def");
        assert_eq!(loc.uri.as_str(), def_uri.as_str());
    }
}
