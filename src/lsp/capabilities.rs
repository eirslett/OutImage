//! Server capability negotiation.

use tower_lsp_server::ls_types::{
    CallHierarchyServerCapability, ClientCapabilities, CodeActionProviderCapability,
    CodeLensOptions, CompletionOptions, DiagnosticOptions, DiagnosticServerCapabilities,
    HoverProviderCapability, LinkedEditingRangeServerCapabilities, OneOf, PositionEncodingKind,
    RenameOptions, SelectionRangeProviderCapability, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities,
    SignatureHelpOptions, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, WorkDoneProgressOptions,
    WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};

use super::features::semantic_tokens_legend;
use super::position::Encoding;

/// Client preferences captured at initialize.
#[derive(Debug, Clone, Copy)]
pub struct ClientPrefs {
    pub markdown_hover: bool,
    pub completion_snippets: bool,
    pub hierarchical_document_symbol: bool,
}

impl Default for ClientPrefs {
    fn default() -> Self {
        Self {
            markdown_hover: true,
            completion_snippets: true,
            hierarchical_document_symbol: true,
        }
    }
}

impl ClientPrefs {
    pub fn from_capabilities(client: &ClientCapabilities) -> Self {
        let markdown_hover = client
            .text_document
            .as_ref()
            .and_then(|td| td.hover.as_ref())
            .and_then(|h| h.content_format.as_ref())
            .map(|formats| formats.contains(&tower_lsp_server::ls_types::MarkupKind::Markdown))
            .unwrap_or(true);

        let completion_snippets = client
            .text_document
            .as_ref()
            .and_then(|td| td.completion.as_ref())
            .and_then(|c| c.completion_item.as_ref())
            .and_then(|item| item.snippet_support)
            .unwrap_or(true);

        let hierarchical_document_symbol = client
            .text_document
            .as_ref()
            .and_then(|td| td.document_symbol.as_ref())
            .and_then(|ds| ds.hierarchical_document_symbol_support)
            .unwrap_or(true);

        Self {
            markdown_hover,
            completion_snippets,
            hierarchical_document_symbol,
        }
    }
}

/// Picks a position encoding the client supports, preferring UTF-8 then UTF-16.
pub fn negotiate_encoding(client: &ClientCapabilities) -> (Encoding, PositionEncodingKind) {
    let encodings = client
        .general
        .as_ref()
        .and_then(|g| g.position_encodings.as_ref())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    if encodings.iter().any(|e| e == &PositionEncodingKind::UTF8) {
        return (Encoding::Utf8, PositionEncodingKind::UTF8);
    }
    if encodings.iter().any(|e| e == &PositionEncodingKind::UTF16) || encodings.is_empty() {
        return (Encoding::Utf16, PositionEncodingKind::UTF16);
    }
    if encodings.iter().any(|e| e == &PositionEncodingKind::UTF32) {
        return (Encoding::Utf32, PositionEncodingKind::UTF32);
    }
    (Encoding::Utf16, PositionEncodingKind::UTF16)
}

/// Full server capabilities through Phase 8 features.
pub fn server_capabilities(position_encoding: PositionEncodingKind) -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(position_encoding),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                will_save: None,
                will_save_wait_until: None,
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![".".into(), " ".into()]),
            all_commit_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
            completion_item: None,
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        definition_provider: Some(OneOf::Left(true)),
        type_definition_provider: Some(
            tower_lsp_server::ls_types::TypeDefinitionProviderCapability::Simple(true),
        ),
        implementation_provider: Some(
            tower_lsp_server::ls_types::ImplementationProviderCapability::Simple(true),
        ),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(
            tower_lsp_server::ls_types::FoldingRangeProviderCapability::Simple(true),
        ),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: semantic_tokens_legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        document_on_type_formatting_provider: Some(
            tower_lsp_server::ls_types::DocumentOnTypeFormattingOptions {
                first_trigger_character: ";".into(),
                more_trigger_character: Some(vec!["d".into(), "D".into()]),
            },
        ),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        inlay_hint_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        // `ls-types` 0.0.6 omits `typeHierarchyProvider` on ServerCapabilities;
        // advertise via experimental + dynamic registration in `initialized`.
        experimental: Some(serde_json::json!({
            "typeHierarchyProvider": true
        })),
        linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(true)),
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: Some("sim".into()),
            inter_file_dependencies: false,
            workspace_diagnostics: false,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations: None,
        }),
        ..ServerCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp_server::ls_types::GeneralClientCapabilities;

    #[test]
    fn prefers_utf8_when_offered() {
        let caps = ClientCapabilities {
            general: Some(GeneralClientCapabilities {
                position_encodings: Some(vec![
                    PositionEncodingKind::UTF16,
                    PositionEncodingKind::UTF8,
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (enc, kind) = negotiate_encoding(&caps);
        assert_eq!(enc, Encoding::Utf8);
        assert_eq!(kind, PositionEncodingKind::UTF8);
    }

    #[test]
    fn defaults_to_utf16() {
        let (enc, kind) = negotiate_encoding(&ClientCapabilities::default());
        assert_eq!(enc, Encoding::Utf16);
        assert_eq!(kind, PositionEncodingKind::UTF16);
    }

    #[test]
    fn advertises_core_features() {
        let caps = server_capabilities(PositionEncodingKind::UTF16);
        assert!(caps.hover_provider.is_some());
        assert!(caps.completion_provider.is_some());
        assert!(caps.signature_help_provider.is_some());
        assert!(caps.definition_provider.is_some());
        assert!(caps.type_definition_provider.is_some());
        assert!(caps.implementation_provider.is_some());
        assert!(caps.semantic_tokens_provider.is_some());
        assert!(caps.workspace_symbol_provider.is_some());
        assert!(caps.call_hierarchy_provider.is_some());
        assert!(
            caps.experimental
                .as_ref()
                .and_then(|v| v.get("typeHierarchyProvider"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
        assert!(caps.inlay_hint_provider.is_some());
        assert!(caps.document_on_type_formatting_provider.is_some());
        assert!(caps.code_lens_provider.is_some());
        assert!(caps.diagnostic_provider.is_some());
        assert!(caps.document_range_formatting_provider.is_some());
        assert!(caps.linked_editing_range_provider.is_some());
        if let Some(TextDocumentSyncCapability::Options(opts)) = caps.text_document_sync {
            assert_eq!(opts.change, Some(TextDocumentSyncKind::INCREMENTAL));
        } else {
            panic!("expected incremental sync");
        }
        if let Some(SemanticTokensServerCapabilities::SemanticTokensOptions(opts)) =
            caps.semantic_tokens_provider
        {
            assert_eq!(opts.range, Some(true));
            assert!(matches!(
                opts.full,
                Some(SemanticTokensFullOptions::Delta { delta: Some(true) })
            ));
            assert!(opts.legend.token_types.iter().any(|t| t.as_str() == "type"));
            assert!(
                opts.legend
                    .token_types
                    .iter()
                    .any(|t| t.as_str() == "boolean")
            );
            assert!(
                opts.legend
                    .token_types
                    .iter()
                    .any(|t| t.as_str() == "commentDirective")
            );
        } else {
            panic!("expected semantic tokens options");
        }
        assert!(caps.rename_provider.is_some());
        assert!(caps.selection_range_provider.is_some());
        assert!(caps.document_formatting_provider.is_some());
        assert!(caps.code_action_provider.is_some());
    }
}
