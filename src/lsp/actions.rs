//! Code actions beyond explain-diagnostic (quick fixes + source actions).

use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, NumberOrString, Position, Range, TextEdit,
    Uri, WorkspaceEdit,
};

use super::analysis::AnalysisSnapshot;
use super::diagnostics::{compile_errors_to_diagnostics, report_code_help};
use super::lint::{keyword_case_suggestion, keyword_near_miss};
use super::position::{Encoding, PositionIndex, byte_span_to_range, position_to_byte};
use super::symbols::{SymbolIndex, SymbolKind, all_keywords, token_at_offset};
use crate::lex::TokenKind;

/// All code actions for the given selection range.
pub fn code_actions(
    snap: &AnalysisSnapshot,
    index: Option<&SymbolIndex>,
    uri: &Uri,
    range: Range,
    encoding: Encoding,
    workspace_exports: &[WorkspaceExport],
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    explain_actions(snap, uri, range, encoding, &mut actions);
    suggestion_actions(snap, uri, range, encoding, &mut actions);
    keyword_fix_actions(snap, uri, range, encoding, &mut actions);
    missing_terminator_actions(snap, uri, range, encoding, &mut actions);
    if let Some(index) = index {
        external_import_actions(
            snap,
            index,
            uri,
            range,
            encoding,
            workspace_exports,
            &mut actions,
        );
    }
    actions
}

/// Exported top-level procedure/class from another workspace file.
#[derive(Debug, Clone)]
pub struct WorkspaceExport {
    pub name: String,
    pub kind: SymbolKind,
    pub detail: String,
    pub uri: String,
}

fn explain_actions(
    snap: &AnalysisSnapshot,
    uri: &Uri,
    range: Range,
    encoding: Encoding,
    actions: &mut Vec<CodeActionOrCommand>,
) {
    let diagnostics = compile_errors_to_diagnostics(&snap.diagnostics, &snap.text, uri, encoding);
    for diag in diagnostics {
        if !ranges_overlap(&diag.range, &range) {
            continue;
        }
        let Some(NumberOrString::String(code)) = &diag.code else {
            continue;
        };
        let Some(help) = report_code_help(code) else {
            continue;
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Explain {}: {}", help.code, help.summary),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag]),
            is_preferred: Some(true),
            edit: None,
            command: None,
            data: None,
            disabled: None,
        }));
    }
}

fn suggestion_actions(
    snap: &AnalysisSnapshot,
    uri: &Uri,
    range: Range,
    encoding: Encoding,
    actions: &mut Vec<CodeActionOrCommand>,
) {
    for error in &snap.diagnostics {
        for suggestion in &error.suggestions {
            let Some(span) = &suggestion.span else {
                continue;
            };
            let Some(replacement) = &suggestion.replacement else {
                continue;
            };
            let edit_range = byte_span_to_range(&snap.text, span.clone(), encoding);
            if !ranges_overlap(&edit_range, &range) {
                continue;
            }
            let preferred =
                suggestion.applicability == crate::diagnostics::Applicability::MachineApplicable;
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: suggestion.message.clone(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                is_preferred: Some(preferred),
                edit: Some(WorkspaceEdit {
                    changes: Some(std::collections::HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: edit_range,
                            new_text: replacement.clone(),
                        }],
                    )])),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                data: None,
                disabled: None,
            }));
        }
    }
}

fn keyword_fix_actions(
    snap: &AnalysisSnapshot,
    uri: &Uri,
    range: Range,
    encoding: Encoding,
    actions: &mut Vec<CodeActionOrCommand>,
) {
    let Some(tokens) = &snap.tokens else {
        return;
    };
    let start = position_to_byte(&snap.text, range.start, encoding);
    let end = position_to_byte(&snap.text, range.end, encoding);
    for token in &tokens.tokens {
        let overlaps = token.span.end > start && token.span.start < end.max(start + 1);
        let caret_inside = token.span.start <= start && start <= token.span.end;
        if !overlaps && !caret_inside {
            continue;
        }
        let TokenKind::Identifier(name) = &token.kind else {
            continue;
        };
        let suggestion = keyword_case_suggestion(name).or_else(|| keyword_near_miss(name));
        let Some(kw) = suggestion else {
            continue;
        };
        if all_keywords().contains(&name.as_str()) {
            continue;
        }
        let edit_range = byte_span_to_range(&snap.text, token.span.clone(), encoding);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Change `{name}` to `{kw}`"),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: None,
            is_preferred: Some(true),
            edit: Some(WorkspaceEdit {
                changes: Some(std::collections::HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range: edit_range,
                        new_text: kw.to_owned(),
                    }],
                )])),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            data: None,
            disabled: None,
        }));
    }
}

fn missing_terminator_actions(
    snap: &AnalysisSnapshot,
    uri: &Uri,
    range: Range,
    encoding: Encoding,
    actions: &mut Vec<CodeActionOrCommand>,
) {
    let has_parse = snap.diagnostics.iter().any(|d| d.phase.as_str() == "parse");
    if !has_parse {
        return;
    }
    let message = snap
        .diagnostics
        .iter()
        .find(|d| d.phase.as_str() == "parse")
        .map(|d| d.message.to_ascii_lowercase())
        .unwrap_or_default();

    let insert_at = snap
        .diagnostics
        .iter()
        .find(|d| d.phase.as_str() == "parse")
        .and_then(|d| d.span.clone())
        .map(|span| byte_span_to_range(&snap.text, span, encoding).end)
        .unwrap_or(range.end);

    if message.contains("end") || message.contains("expected") && snap.text.contains("begin") {
        let trimmed = snap.text.trim_end();
        if !trimmed.to_ascii_lowercase().ends_with("end")
            && !trimmed.to_ascii_lowercase().ends_with("end;")
        {
            let pos = PositionIndex::new(&snap.text).offset_to_position(
                &snap.text,
                snap.text.len(),
                encoding,
            );
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Insert missing `end;`".into(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                is_preferred: Some(false),
                edit: Some(WorkspaceEdit {
                    changes: Some(std::collections::HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: Range::new(pos, pos),
                            new_text: if snap.text.ends_with('\n') {
                                "end;\n".into()
                            } else {
                                "\nend;\n".into()
                            },
                        }],
                    )])),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                data: None,
                disabled: None,
            }));
        }
    }

    if message.contains(';') || message.contains("semicolon") || message.contains("expected") {
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Insert `;`".into(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: None,
            is_preferred: Some(false),
            edit: Some(WorkspaceEdit {
                changes: Some(std::collections::HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range: Range::new(insert_at, insert_at),
                        new_text: ";".into(),
                    }],
                )])),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            data: None,
            disabled: None,
        }));
    }
}

fn external_import_actions(
    snap: &AnalysisSnapshot,
    index: &SymbolIndex,
    uri: &Uri,
    range: Range,
    encoding: Encoding,
    workspace_exports: &[WorkspaceExport],
    actions: &mut Vec<CodeActionOrCommand>,
) {
    let offset = position_to_byte(&snap.text, range.start, encoding);
    let Some(tokens) = &snap.tokens else {
        return;
    };
    let Some(token) = token_at_offset(&tokens.tokens, offset) else {
        return;
    };
    let TokenKind::Identifier(name) = &token.kind else {
        return;
    };
    if index.resolve_at_offset(offset).is_some() {
        return;
    }
    for export in workspace_exports {
        if !export.name.eq_ignore_ascii_case(name) {
            continue;
        }
        if export.uri == uri.as_str() {
            continue;
        }
        let kind_word = match export.kind {
            SymbolKind::Class => "class",
            _ => "procedure",
        };
        let insert = format!("external {kind_word} {};\n", export.name);
        let title = if export.detail.is_empty() {
            format!("Add `external {kind_word} {}`", export.name)
        } else {
            format!(
                "Add `external {kind_word} {}` — {}",
                export.name, export.detail
            )
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title,
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: None,
            is_preferred: Some(true),
            edit: Some(WorkspaceEdit {
                changes: Some(std::collections::HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                        new_text: insert,
                    }],
                )])),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            data: None,
            disabled: None,
        }));
    }
}

fn ranges_overlap(a: &Range, b: &Range) -> bool {
    if a.start.line > b.end.line || b.start.line > a.end.line {
        return false;
    }
    if a.start.line == b.end.line && a.start.character > b.end.character {
        return false;
    }
    if b.start.line == a.end.line && b.start.character > a.end.character {
        return false;
    }
    true
}

/// On-type formatting edits when the user types `ch`.
pub fn on_type_formatting(
    text: &str,
    position: Position,
    ch: &str,
    tab_size: u32,
    insert_spaces: bool,
    encoding: Encoding,
) -> Option<Vec<TextEdit>> {
    if ch != ";" && ch != "d" && ch != "D" {
        return None;
    }
    // Only reformat when completing `end` or after `;`.
    let offset = position_to_byte(text, position, encoding);
    if ch.eq_ignore_ascii_case("d") {
        let before = &text[..offset.min(text.len())];
        if !before.to_ascii_lowercase().ends_with("end") {
            return None;
        }
    }
    crate::lsp::format::format_edits(text, tab_size, insert_spaces, encoding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};
    use std::str::FromStr;

    fn uri() -> Uri {
        Uri::from_str("file:///tmp/a.sim").unwrap()
    }

    #[test]
    fn keyword_case_quick_fix() {
        // Lexer folds real keywords; near-miss identifiers still get a fix.
        let snap = analyze_document("begn integer x; end", &AnalysisOptions::default());
        let actions = code_actions(
            &snap,
            snap.symbols.as_ref(),
            &uri(),
            Range::new(Position::new(0, 0), Position::new(0, 4)),
            Encoding::Utf16,
            &[],
        );
        assert!(
            actions.iter().any(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => ca.title.contains("begin"),
                _ => false,
            }),
            "{actions:?}"
        );
    }

    #[test]
    fn missing_end_quick_fix() {
        let snap = analyze_document("begin integer x;", &AnalysisOptions::default());
        let actions = code_actions(
            &snap,
            snap.symbols.as_ref(),
            &uri(),
            Range::new(Position::new(0, 0), Position::new(0, 5)),
            Encoding::Utf16,
            &[],
        );
        assert!(
            actions.iter().any(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => ca.title.contains("end"),
                _ => false,
            }),
            "{actions:?}"
        );
    }

    #[test]
    fn unknown_name_replace_suggestion() {
        let source = r#"begin Outtxt("hi"); end;"#;
        let snap = analyze_document(source, &AnalysisOptions::default());
        let start = source.find("Outtxt").unwrap() as u32;
        let actions = code_actions(
            &snap,
            snap.symbols.as_ref(),
            &uri(),
            Range::new(Position::new(0, start), Position::new(0, start + 6)),
            Encoding::Utf16,
            &[],
        );
        assert!(
            actions.iter().any(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => {
                    ca.edit.is_some()
                        && ca.disabled.is_none()
                        && (ca.title.contains("OutText") || ca.title.contains("replace"))
                }
                _ => false,
            }),
            "{actions:?}"
        );
    }

    #[test]
    fn external_import_suggestion() {
        let snap = analyze_document("begin integer x; foo; end", &AnalysisOptions::default());
        // Position on `foo`
        let pos = Position::new(0, 17);
        let exports = vec![WorkspaceExport {
            name: "foo".into(),
            kind: SymbolKind::Procedure,
            detail: "procedure foo".into(),
            uri: "file:///tmp/other.sim".into(),
        }];
        let actions = code_actions(
            &snap,
            snap.symbols.as_ref(),
            &uri(),
            Range::new(pos, pos),
            Encoding::Utf16,
            &exports,
        );
        assert!(
            actions.iter().any(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => ca.title.contains("external procedure foo"),
                _ => false,
            }),
            "{actions:?}"
        );
    }

    #[test]
    fn on_type_formats_after_semicolon() {
        let text = "begin\ninteger x;\nx:=1;\nend";
        let edits = on_type_formatting(text, Position::new(1, 10), ";", 2, true, Encoding::Utf16);
        assert!(edits.is_some());
    }
}
