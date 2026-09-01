//! `tower-lsp-server` backend and stdio entry point.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::{Error as LspError, Result as LspResult};
use tower_lsp_server::ls_types::request::{
    GotoImplementationParams, GotoImplementationResponse, GotoTypeDefinitionParams,
    GotoTypeDefinitionResponse,
};
use tower_lsp_server::ls_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CodeActionOrCommand, CodeActionParams, CodeLens, CodeLensParams, CompletionList,
    CompletionParams, CompletionResponse, Diagnostic, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightParams,
    DocumentOnTypeFormattingParams, DocumentRangeFormattingParams, DocumentSymbolParams,
    DocumentSymbolResponse, FoldingRange, FoldingRangeParams, FullDocumentDiagnosticReport,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, InitializeParams,
    InitializeResult, InitializedParams, InlayHint, InlayHintParams, LinkedEditingRangeParams,
    LinkedEditingRanges, MessageType, PrepareRenameResponse, ReferenceParams,
    RelatedFullDocumentDiagnosticReport, RenameParams, SelectionRange, SelectionRangeParams,
    SemanticToken, SemanticTokensDeltaParams, SemanticTokensFullDeltaResult, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensRangeResult, SemanticTokensResult, ServerInfo,
    SignatureHelp, SignatureHelpParams, TextDocumentPositionParams, TextEdit, TypeHierarchyItem,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams, Uri,
    WorkspaceEdit, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

use super::actions;
use super::analysis::{AnalysisOptions, AnalysisSnapshot, analyze_document};
use super::capabilities::{ClientPrefs, negotiate_encoding, server_capabilities};
use super::config::{CheckOn, LspConfig};
use super::diagnostics::snapshot_diagnostics;
use super::document::DocumentStore;
use super::features;
use super::hierarchy;
use super::hints;
use super::nav;
use super::position::Encoding;
use super::symbols::{SymbolIndex, SymbolKind};
use super::workspace::{self, Workspace};
use super::{LANGUAGE_ID, SERVER_NAME};

struct TokenCacheEntry {
    result_id: String,
    data: Vec<SemanticToken>,
}

struct State {
    documents: DocumentStore,
    /// Cached analysis keyed by URI string.
    snapshots: HashMap<String, AnalysisSnapshot>,
    encoding: Encoding,
    config: LspConfig,
    /// Per-document generation counter for debounced reanalysis.
    debounce_gen: HashMap<String, u64>,
    /// Last full semantic-token response per URI (for delta).
    token_cache: HashMap<String, TokenCacheEntry>,
    /// Monotonic id for semantic token `resultId` values.
    next_token_result_id: u64,
    /// Workspace folders + on-disk `.sim` index.
    workspace: Workspace,
    /// Negotiated client preferences.
    client_prefs: ClientPrefs,
}

impl Default for State {
    fn default() -> Self {
        Self {
            documents: DocumentStore::new(),
            snapshots: HashMap::new(),
            encoding: Encoding::Utf16,
            config: LspConfig::default(),
            debounce_gen: HashMap::new(),
            token_cache: HashMap::new(),
            next_token_result_id: 1,
            workspace: Workspace::default(),
            client_prefs: ClientPrefs::default(),
        }
    }
}

impl State {
    fn bump_generation(&mut self, uri: &str) -> u64 {
        let entry = self.debounce_gen.entry(uri.to_owned()).or_insert(0);
        *entry = entry.saturating_add(1);
        *entry
    }

    /// True when this analysis still matches the live buffer.
    ///
    /// rust-analyzer drops salsa snapshots after `apply_change` (Cancelled);
    /// clangd discards obsolete ASTWorker jobs. We do the same with a
    /// per-document generation plus the LSP document version: a slow pass
    /// on older text must not publish or cache diagnostics.
    fn is_current_analysis(&self, uri: &Uri, version: i32, generation: u64) -> bool {
        if self.debounce_gen.get(uri.as_str()).copied() != Some(generation) {
            return false;
        }
        self.documents
            .get(uri)
            .is_some_and(|doc| doc.version == version)
    }
}

/// True when an editor buffer is open for `uri_str` (string match, parsed URI,
/// or same filesystem path with a different URI spelling).
fn is_open_document(state: &State, uri_str: &str) -> bool {
    if state.documents.iter().any(|doc| doc.uri == uri_str) {
        return true;
    }
    let Ok(uri) = uri_str.parse::<Uri>() else {
        return false;
    };
    if state.documents.get(&uri).is_some() {
        return true;
    }
    let Some(path) = workspace::uri_to_path(&uri) else {
        return false;
    };
    state.documents.iter().any(|doc| {
        doc.uri
            .parse::<Uri>()
            .ok()
            .and_then(|open_uri| workspace::uri_to_path(&open_uri))
            .is_some_and(|open_path| open_path == path)
    })
}

/// Language server backend shared across concurrent requests.
pub struct Backend {
    client: Client,
    state: Arc<RwLock<State>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(State::default())),
        }
    }

    async fn reanalyze(&self, uri: &Uri, version: i32, text: &str) {
        let generation = {
            let mut state = self.state.write().await;
            state.bump_generation(uri.as_str())
        };
        Self::reanalyze_inner(&self.client, &self.state, uri, version, text, generation).await;
    }

    async fn schedule_reanalyze(&self, uri: Uri, version: i32, text: String) {
        let (key, generation, debounce_ms) = {
            let mut state = self.state.write().await;
            let key = uri.as_str().to_owned();
            let generation = state.bump_generation(&key);
            (key, generation, state.config.debounce_ms)
        };
        let state = Arc::clone(&self.state);
        let client = self.client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(debounce_ms)).await;
            let still_current = {
                let state = state.read().await;
                state.debounce_gen.get(&key).copied() == Some(generation)
            };
            if !still_current {
                return;
            }
            Self::reanalyze_inner(&client, &state, &uri, version, &text, generation).await;
        });
    }

    async fn reanalyze_inner(
        client: &Client,
        state: &Arc<RwLock<State>>,
        uri: &Uri,
        version: i32,
        text: &str,
        generation: u64,
    ) {
        let (encoding, options, max_bytes) = {
            let state = state.read().await;
            if !state.is_current_analysis(uri, version, generation) {
                return;
            }
            (
                state.encoding,
                AnalysisOptions::from(&state.config),
                state.config.max_document_bytes,
            )
        };
        if text.len() > max_bytes {
            client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "document {} exceeds maxDocumentBytes ({max_bytes}); skipping analysis",
                        uri.as_str()
                    ),
                )
                .await;
            let publish = {
                let state = state.read().await;
                state.is_current_analysis(uri, version, generation)
            };
            if publish {
                client
                    .publish_diagnostics(uri.clone(), Vec::new(), Some(version))
                    .await;
            }
            return;
        }
        tracing_log(format!("analyzing {} ({} bytes)", uri.as_str(), text.len()));
        // Analyze on the blocking pool so a slow pass cannot stall didChange.
        // Overlapping jobs are allowed (like rust-analyzer's threadpool); the
        // generation check below is what drops a late result. clangd instead
        // serializes per file — also correct, but would delay the newer pass.
        let text_owned = text.to_owned();
        let snapshot = match spawn_analysis(move || {
            catch_unwind(AssertUnwindSafe(|| analyze_document(&text_owned, &options)))
        })
        .await
        {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(_)) => {
                client
                    .log_message(
                        MessageType::ERROR,
                        "analysis panicked; publishing empty diagnostics",
                    )
                    .await;
                AnalysisSnapshot {
                    text: text.to_owned(),
                    tokens: None,
                    trivia: Vec::new(),
                    program: None,
                    diagnostics: Vec::new(),
                    symbols: None,
                    lints: Vec::new(),
                }
            }
            Err(_) => return,
        };
        let diagnostics = snapshot_diagnostics(
            &snapshot.diagnostics,
            &snapshot.lints,
            &snapshot.text,
            uri,
            encoding,
        );
        let commit = {
            let mut state = state.write().await;
            if !state.is_current_analysis(uri, version, generation) {
                false
            } else {
                state.snapshots.insert(uri.as_str().to_owned(), snapshot);
                // Token deltas are invalidated when the document is reanalyzed.
                state.token_cache.remove(uri.as_str());
                // Prefer live buffer over disk index for the same URI.
                state.workspace.forget_uri(uri.as_str());
                true
            }
        };
        if commit {
            client
                .publish_diagnostics(uri.clone(), diagnostics, Some(version))
                .await;
        }
    }

    async fn with_snapshot<R>(
        &self,
        uri: &Uri,
        f: impl FnOnce(&AnalysisSnapshot, &SymbolIndex, Encoding) -> R + std::panic::UnwindSafe,
    ) -> LspResult<Option<R>> {
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(symbols) = snap.symbols.as_ref() else {
            return Ok(None);
        };
        let encoding = state.encoding;
        match catch_unwind(AssertUnwindSafe(|| f(snap, symbols, encoding))) {
            Ok(value) => Ok(Some(value)),
            Err(_) => {
                drop(state);
                self.client
                    .log_message(
                        MessageType::ERROR,
                        "request handler panicked; returning internal error",
                    )
                    .await;
                Err(LspError::internal_error())
            }
        }
    }
}

fn tracing_log(message: impl AsRef<str>) {
    // Prefer `RUST_LOG=sim_lsp=debug` when the host wires `env_logger` /
    // `tracing-subscriber`. Without a subscriber this is a no-op stderr hint
    // gated on an explicit env flag so CI stays quiet.
    if std::env::var_os("SIM_LSP_TRACE").is_some() {
        eprintln!("[sim_lsp] {}", message.as_ref());
    }
}

fn collect_workspace_docs<'a>(
    state: &'a State,
    skip_uri: &str,
) -> Vec<(Uri, &'a AnalysisSnapshot, &'a SymbolIndex)> {
    let mut docs = Vec::new();
    for (uri_str, snap) in &state.snapshots {
        if uri_str == skip_uri {
            continue;
        }
        let Some(symbols) = snap.symbols.as_ref() else {
            continue;
        };
        let Ok(uri) = uri_str.parse::<Uri>() else {
            continue;
        };
        docs.push((uri, snap, symbols));
    }
    for file in state.workspace.disk.values() {
        if file.uri == skip_uri || state.snapshots.contains_key(&file.uri) {
            continue;
        }
        let Some(symbols) = file.snapshot.symbols.as_ref() else {
            continue;
        };
        let Ok(uri) = file.uri.parse::<Uri>() else {
            continue;
        };
        docs.push((uri, &file.snapshot, symbols));
    }
    docs
}

fn workspace_exports(state: &State) -> Vec<actions::WorkspaceExport> {
    let mut out = Vec::new();
    for (uri, snap, index) in collect_workspace_docs(state, "") {
        let _ = snap;
        for symbol in &index.symbols {
            if symbol.is_external || symbol.container.is_some() {
                continue;
            }
            if !matches!(symbol.kind, SymbolKind::Procedure | SymbolKind::Class) {
                continue;
            }
            out.push(actions::WorkspaceExport {
                name: symbol.name.clone(),
                kind: symbol.kind,
                detail: symbol.detail.clone(),
                uri: uri.as_str().to_owned(),
            });
        }
    }
    out
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        let (encoding, position_encoding) = negotiate_encoding(&params.capabilities);
        let prefs = ClientPrefs::from_capabilities(&params.capabilities);
        {
            let mut state = self.state.write().await;
            state.encoding = encoding;
            state.client_prefs = prefs;
            if let Some(opts) = params.initialization_options {
                state.config.apply_json(&opts);
            }
            if let Some(folders) = params.workspace_folders {
                let paths = workspace::folders_from_uris(folders.iter().map(|f| &f.uri));
                state.workspace.set_folders(paths);
            } else {
                #[allow(deprecated)]
                if let Some(root) = params.root_uri.as_ref().and_then(workspace::uri_to_path) {
                    state.workspace.set_folders(vec![root]);
                }
            }
        }
        Ok(InitializeResult {
            capabilities: server_capabilities(position_encoding),
            server_info: Some(ServerInfo {
                name: SERVER_NAME.into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            offset_encoding: None,
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "sim language server ready")
            .await;
        // Dynamic registration: type hierarchy (missing from ServerCapabilities in ls-types 0.0.6).
        let _ = self
            .client
            .register_capability(vec![tower_lsp_server::ls_types::Registration {
                id: "sim-type-hierarchy".into(),
                method: "textDocument/prepareTypeHierarchy".into(),
                register_options: Some(serde_json::json!({
                    "documentSelector": [{ "language": "simula" }, { "pattern": "**/*.sim" }]
                })),
            }])
            .await;
        // Pull configuration when the client supports it.
        if let Ok(values) = self
            .client
            .configuration(vec![tower_lsp_server::ls_types::ConfigurationItem {
                scope_uri: None,
                section: Some("simula".into()),
            }])
            .await
            && let Some(value) = values.into_iter().next()
        {
            let mut state = self.state.write().await;
            state.config.apply_json(&value);
        }
        // Best-effort workspace index (sandboxed to workspace folders).
        let (options, max_bytes, folder_count) = {
            let state = self.state.read().await;
            (
                AnalysisOptions::from(&state.config),
                state.config.max_document_bytes,
                state.workspace.folders.len(),
            )
        };
        if folder_count > 0 {
            let state = Arc::clone(&self.state);
            let client = self.client.clone();
            tokio::spawn(async move {
                tracing_log(format!("indexing {folder_count} workspace folder(s)"));
                let token = tower_lsp_server::ls_types::NumberOrString::String(
                    "sim-workspace-index".into(),
                );
                let _ = client.create_work_done_progress(token.clone()).await;
                let progress = client
                    .progress(token, "Indexing Simula workspace")
                    .with_percentage(0);
                let progress = progress.begin().await;
                {
                    let mut state = state.write().await;
                    state.workspace.reindex_all(&options, max_bytes);
                    // Live buffers own diagnostics for open files; drop the
                    // on-disk copies so a later publish cannot clobber them.
                    let open_uris: Vec<String> =
                        state.documents.iter().map(|d| d.uri.clone()).collect();
                    for uri in open_uris {
                        state.workspace.forget_uri(&uri);
                    }
                }
                // Publish diagnostics for indexed (closed) files.
                let encoding = {
                    let state = state.read().await;
                    state.encoding
                };
                let files: Vec<_> = {
                    let state = state.read().await;
                    state
                        .workspace
                        .disk
                        .values()
                        .map(|f| {
                            (
                                f.uri.clone(),
                                f.snapshot.diagnostics.clone(),
                                f.snapshot.lints.clone(),
                                f.snapshot.text.clone(),
                            )
                        })
                        .collect()
                };
                let total = files.len().max(1);
                for (i, (uri_str, diags, lints, text)) in files.into_iter().enumerate() {
                    progress
                        .report_with_message(
                            format!("analyzed {uri_str}"),
                            ((i + 1) * 100 / total) as u32,
                        )
                        .await;
                    if let Ok(uri) = uri_str.parse::<Uri>() {
                        let skip = {
                            let state = state.read().await;
                            is_open_document(&state, &uri_str)
                        };
                        if skip {
                            continue;
                        }
                        let diagnostics =
                            snapshot_diagnostics(&diags, &lints, &text, &uri, encoding);
                        // No version: these are closed files. Never publish
                        // unversioned diagnostics for an open buffer.
                        client.publish_diagnostics(uri, diagnostics, None).await;
                    }
                }
                progress
                    .finish_with_message(format!(
                        "workspace index ready ({folder_count} folder(s))"
                    ))
                    .await;
                client
                    .log_message(
                        MessageType::INFO,
                        format!("workspace index ready ({folder_count} folder(s))"),
                    )
                    .await;
            });
        }
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let language_id = if doc.language_id.is_empty() {
            LANGUAGE_ID.to_owned()
        } else {
            doc.language_id
        };
        {
            let mut state = self.state.write().await;
            state
                .documents
                .open(&doc.uri, language_id, doc.version, doc.text.clone());
        }
        self.reanalyze(&doc.uri, doc.version, &doc.text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let text = {
            let mut state = self.state.write().await;
            let encoding = state.encoding;
            match state
                .documents
                .apply_changes(&uri, version, &params.content_changes, encoding)
            {
                Ok(doc) => doc.text.clone(),
                Err(err) => {
                    drop(state);
                    self.client
                        .log_message(MessageType::ERROR, format!("didChange failed: {err}"))
                        .await;
                    return;
                }
            }
        };
        let check_on = self.state.read().await.config.check_on;
        if check_on == CheckOn::Change {
            self.schedule_reanalyze(uri, version, text).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        let check_on = {
            let state = self.state.read().await;
            state.config.check_on
        };
        if check_on != CheckOn::Save {
            return;
        }
        let (version, text) = {
            let state = self.state.read().await;
            let Some(doc) = state.documents.get(&uri) else {
                return;
            };
            (doc.version, doc.text.clone())
        };
        self.reanalyze(&uri, version, &text).await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let message = {
            let mut state = self.state.write().await;
            state.config.apply_json(&params.settings);
            format!(
                "configuration updated: checkOn={:?} debounceMs={} brackets={} doubleDashComments={}",
                state.config.check_on,
                state.config.debounce_ms,
                state.config.allow_square_bracket_subscripts,
                state.config.allow_double_dash_comments
            )
        };
        self.client.log_message(MessageType::INFO, message).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        {
            let mut state = self.state.write().await;
            state.documents.close(&uri);
            state.snapshots.remove(uri.as_str());
            state.debounce_gen.remove(uri.as_str());
            state.token_cache.remove(uri.as_str());
        }
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let markdown = self.state.read().await.client_prefs.markdown_hover;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                features::hover_with_markup(snap, idx, pos, enc, markdown)
            })
            .await?
            .flatten())
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let prefs = self.state.read().await.client_prefs;
        self.with_snapshot(uri, |snap, idx, enc| {
            let options = features::CompletionOptions {
                include_snippets: prefs.completion_snippets,
                max_items: 500,
            };
            let (items, incomplete) = features::completions_list(snap, idx, pos, enc, &options);
            CompletionResponse::List(CompletionList {
                is_incomplete: incomplete,
                items,
            })
        })
        .await
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                features::signature_help(snap, idx, pos, enc)
            })
            .await?
            .flatten())
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(symbols) = snap.symbols.as_ref() else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let workspace_docs = collect_workspace_docs(&state, uri.as_str());
        match catch_unwind(AssertUnwindSafe(|| {
            nav::goto_definition_extended(snap, symbols, uri, pos, encoding, &workspace_docs)
                .map(GotoDefinitionResponse::Scalar)
        })) {
            Ok(value) => Ok(value),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> LspResult<Option<GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                features::goto_type_definition(snap, idx, uri, pos, enc)
                    .map(GotoTypeDefinitionResponse::Scalar)
            })
            .await?
            .flatten())
    }

    async fn references(
        &self,
        params: ReferenceParams,
    ) -> LspResult<Option<Vec<tower_lsp_server::ls_types::Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_decl = params.context.include_declaration;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(symbols) = snap.symbols.as_ref() else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let workspace_docs = collect_workspace_docs(&state, "");
        match catch_unwind(AssertUnwindSafe(|| {
            nav::find_references_extended(
                snap,
                symbols,
                uri,
                pos,
                encoding,
                include_decl,
                &workspace_docs,
            )
        })) {
            Ok(locs) => Ok(Some(locs)),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> LspResult<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.with_snapshot(uri, |snap, idx, enc| {
            features::document_highlight(snap, idx, pos, enc)
        })
        .await
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let hierarchical = self
            .state
            .read()
            .await
            .client_prefs
            .hierarchical_document_symbol;
        self.with_snapshot(uri, |snap, idx, enc| {
            if hierarchical {
                DocumentSymbolResponse::Nested(features::document_symbols(snap, idx, enc))
            } else {
                DocumentSymbolResponse::Flat(features::document_symbols_flat(snap, idx, uri, enc))
            }
        })
        .await
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> LspResult<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let encoding = state.encoding;
        match catch_unwind(AssertUnwindSafe(|| {
            features::folding_ranges(snap, encoding)
        })) {
            Ok(ranges) => Ok(Some(ranges)),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                features::rename(snap, idx, uri, pos, enc, &new_name)
            })
            .await?
            .flatten())
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> LspResult<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let pos = params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                features::prepare_rename(snap, idx, pos, enc).map(PrepareRenameResponse::Range)
            })
            .await?
            .flatten())
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> LspResult<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let mut state = self.state.write().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(symbols) = snap.symbols.as_ref() else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let mut tokens = match catch_unwind(AssertUnwindSafe(|| {
            features::semantic_tokens_full(snap, symbols, encoding)
        })) {
            Ok(tokens) => tokens,
            Err(_) => return Err(LspError::internal_error()),
        };
        let result_id = format!("tok-{}", state.next_token_result_id);
        state.next_token_result_id = state.next_token_result_id.saturating_add(1);
        tokens.result_id = Some(result_id.clone());
        state.token_cache.insert(
            uri.as_str().to_owned(),
            TokenCacheEntry {
                result_id,
                data: tokens.data.clone(),
            },
        );
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn semantic_tokens_full_delta(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> LspResult<Option<SemanticTokensFullDeltaResult>> {
        let uri = &params.text_document.uri;
        let previous_id = params.previous_result_id;
        let mut state = self.state.write().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(symbols) = snap.symbols.as_ref() else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let mut tokens = match catch_unwind(AssertUnwindSafe(|| {
            features::semantic_tokens_full(snap, symbols, encoding)
        })) {
            Ok(tokens) => tokens,
            Err(_) => return Err(LspError::internal_error()),
        };
        let result_id = format!("tok-{}", state.next_token_result_id);
        state.next_token_result_id = state.next_token_result_id.saturating_add(1);
        tokens.result_id = Some(result_id.clone());

        let previous = state
            .token_cache
            .get(uri.as_str())
            .filter(|entry| entry.result_id == previous_id)
            .map(|entry| entry.data.clone());

        let response = match catch_unwind(AssertUnwindSafe(|| {
            features::semantic_tokens_delta(previous.as_deref(), tokens.clone())
        })) {
            Ok(Ok(delta)) => SemanticTokensFullDeltaResult::TokensDelta(delta),
            Ok(Err(full)) => SemanticTokensFullDeltaResult::Tokens(full),
            Err(_) => return Err(LspError::internal_error()),
        };

        state.token_cache.insert(
            uri.as_str().to_owned(),
            TokenCacheEntry {
                result_id,
                data: tokens.data,
            },
        );
        Ok(Some(response))
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> LspResult<Option<SemanticTokensRangeResult>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        self.with_snapshot(uri, |snap, idx, enc| {
            SemanticTokensRangeResult::Tokens(features::semantic_tokens_range(
                snap, idx, enc, range,
            ))
        })
        .await
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> LspResult<Option<Vec<SelectionRange>>> {
        let uri = &params.text_document.uri;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let positions = params.positions.clone();
        match catch_unwind(AssertUnwindSafe(|| {
            features::selection_ranges(snap, &positions, encoding)
        })) {
            Ok(ranges) => Ok(Some(ranges)),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let tab_size = params.options.tab_size;
        let insert_spaces = params.options.insert_spaces;
        match catch_unwind(AssertUnwindSafe(|| {
            features::format_edits(&snap.text, tab_size, insert_spaces, encoding)
        })) {
            Ok(edits) => Ok(edits),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let range = params.range;
        let tab_size = params.options.tab_size;
        let insert_spaces = params.options.insert_spaces;
        match catch_unwind(AssertUnwindSafe(|| {
            crate::lsp::format::format_range_edits(
                &snap.text,
                range,
                tab_size,
                insert_spaces,
                encoding,
            )
        })) {
            Ok(edits) => Ok(edits),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> LspResult<Option<Vec<CodeActionOrCommand>>> {
        let uri = &params.text_document.uri;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let range = params.range;
        let exports = workspace_exports(&state);
        let symbols = snap.symbols.as_ref();
        match catch_unwind(AssertUnwindSafe(|| {
            actions::code_actions(snap, symbols, uri, range, encoding, &exports)
        })) {
            Ok(result) => Ok(Some(result)),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn code_lens(&self, params: CodeLensParams) -> LspResult<Option<Vec<CodeLens>>> {
        let uri = &params.text_document.uri;
        self.with_snapshot(uri, |snap, idx, enc| {
            features::code_lenses(snap, idx, uri, enc)
        })
        .await
    }

    async fn linked_editing_range(
        &self,
        params: LinkedEditingRangeParams,
    ) -> LspResult<Option<LinkedEditingRanges>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                features::linked_editing_ranges(snap, idx, pos, enc)
            })
            .await?
            .flatten())
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> LspResult<Option<GotoImplementationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                let locs = hierarchy::goto_implementations(snap, idx, uri, pos, enc);
                if locs.is_empty() {
                    None
                } else {
                    Some(GotoImplementationResponse::Array(locs))
                }
            })
            .await?
            .flatten())
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> LspResult<Option<Vec<CallHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                hierarchy::prepare_call_hierarchy(snap, idx, uri, pos, enc)
            })
            .await?
            .flatten())
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyIncomingCall>>> {
        let item = params.item;
        let uri = item.uri.clone();
        Ok(self
            .with_snapshot(&uri, |snap, idx, enc| {
                hierarchy::incoming_calls(snap, idx, &uri, &item, enc)
            })
            .await?
            .flatten())
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;
        let uri = item.uri.clone();
        Ok(self
            .with_snapshot(&uri, |snap, idx, enc| {
                hierarchy::outgoing_calls(snap, idx, &uri, &item, enc)
            })
            .await?
            .flatten())
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> LspResult<Option<Vec<TypeHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self
            .with_snapshot(uri, |snap, idx, enc| {
                hierarchy::prepare_type_hierarchy(snap, idx, uri, pos, enc)
            })
            .await?
            .flatten())
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> LspResult<Option<Vec<TypeHierarchyItem>>> {
        let item = params.item;
        let uri = item.uri.clone();
        Ok(self
            .with_snapshot(&uri, |snap, idx, enc| {
                hierarchy::type_supertypes(snap, idx, &uri, &item, enc)
            })
            .await?
            .flatten())
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> LspResult<Option<Vec<TypeHierarchyItem>>> {
        let item = params.item;
        let uri = item.uri.clone();
        Ok(self
            .with_snapshot(&uri, |snap, idx, enc| {
                hierarchy::type_subtypes(snap, idx, &uri, &item, enc)
            })
            .await?
            .flatten())
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> LspResult<DocumentDiagnosticReportResult> {
        let uri = &params.text_document.uri;
        let state = self.state.read().await;
        let encoding = state.encoding;
        let (diagnostics, result_id) = if let Some(snap) = state.snapshots.get(uri.as_str()) {
            // Pull diagnostics must not replay a snapshot from an older buffer.
            let stale = state
                .documents
                .get(uri)
                .is_some_and(|doc| doc.text != snap.text);
            if stale {
                (Vec::<Diagnostic>::new(), None)
            } else {
                let diags =
                    snapshot_diagnostics(&snap.diagnostics, &snap.lints, &snap.text, uri, encoding);
                (diags, Some(format!("diag-{}", snap.text.len())))
            }
        } else {
            (Vec::<Diagnostic>::new(), None)
        };
        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id,
                    items: diagnostics,
                },
            }),
        ))
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        let mut state = self.state.write().await;
        for removed in params.event.removed {
            if let Some(path) = workspace::uri_to_path(&removed.uri) {
                state.workspace.remove_folder(&path);
            }
        }
        for added in params.event.added {
            if let Some(path) = workspace::uri_to_path(&added.uri) {
                state.workspace.add_folder(path);
            }
        }
        let options = AnalysisOptions::from(&state.config);
        let max_bytes = state.config.max_document_bytes;
        state.workspace.reindex_all(&options, max_bytes);
        let count = state.workspace.disk.len();
        drop(state);
        self.client
            .log_message(
                MessageType::INFO,
                format!("workspace folders updated; indexed {count} .sim file(s)"),
            )
            .await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut state = self.state.write().await;
        let options = AnalysisOptions::from(&state.config);
        let max_bytes = state.config.max_document_bytes;
        for change in params.changes {
            let Some(path) = state.workspace.resolve_uri_path(&change.uri) else {
                continue;
            };
            // Skip files currently open in the editor.
            if state.documents.get(&change.uri).is_some() {
                continue;
            }
            match change.typ {
                tower_lsp_server::ls_types::FileChangeType::DELETED => {
                    state.workspace.forget_uri(change.uri.as_str());
                }
                _ => {
                    let _ = state.workspace.index_path(&path, &options, max_bytes);
                }
            }
        }
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        self.with_snapshot(uri, |snap, idx, enc| {
            hints::inlay_hints(snap, idx, Some(range), enc)
        })
        .await
    }

    async fn on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document_position.text_document.uri;
        let state = self.state.read().await;
        let Some(snap) = state.snapshots.get(uri.as_str()) else {
            return Ok(None);
        };
        let encoding = state.encoding;
        let ch = params.ch;
        let position = params.text_document_position.position;
        let tab_size = params.options.tab_size;
        let insert_spaces = params.options.insert_spaces;
        match catch_unwind(AssertUnwindSafe(|| {
            actions::on_type_formatting(
                &snap.text,
                position,
                &ch,
                tab_size,
                insert_spaces,
                encoding,
            )
        })) {
            Ok(edits) => Ok(edits),
            Err(_) => Err(LspError::internal_error()),
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<WorkspaceSymbolResponse>> {
        let state = self.state.read().await;
        let encoding = state.encoding;
        let mut docs = Vec::new();
        for (uri_str, snap) in &state.snapshots {
            let Some(symbols) = snap.symbols.as_ref() else {
                continue;
            };
            let Ok(uri) = uri_str.parse::<Uri>() else {
                continue;
            };
            docs.push((uri, snap, symbols));
        }
        for file in state.workspace.disk.values() {
            if state.snapshots.contains_key(&file.uri) {
                continue;
            }
            let Some(symbols) = file.snapshot.symbols.as_ref() else {
                continue;
            };
            let Ok(uri) = file.uri.parse::<Uri>() else {
                continue;
            };
            docs.push((uri, &file.snapshot, symbols));
        }
        let query = params.query.clone();
        match catch_unwind(AssertUnwindSafe(|| {
            features::workspace_symbols(&docs, &query, encoding)
        })) {
            Ok(symbols) => Ok(Some(WorkspaceSymbolResponse::Flat(symbols))),
            Err(_) => Err(LspError::internal_error()),
        }
    }
}

/// Runs the language server on stdin/stdout until the client exits.
pub async fn run_stdio() {
    let (service, socket) = LspService::new(Backend::new);
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    Server::new(stdin, stdout, socket).serve(service).await;
}

async fn spawn_analysis<F, R>(work: F) -> Result<R, tokio::task::JoinError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(work).await
}

/// Builds an [`LspService`] for tests (no stdio).
#[cfg(test)]
pub fn test_service() -> (LspService<Backend>, tower_lsp_server::ClientSocket) {
    LspService::new(Backend::new)
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures::StreamExt;
    use futures::stream::Stream;
    use serde_json::json;
    use tower::{Service, ServiceExt};
    use tower_lsp_server::jsonrpc::{Request, Response};

    use super::*;

    fn initialize_request(id: i64) -> Request {
        Request::build("initialize")
            .params(json!({
                "capabilities": {
                    "general": {
                        "positionEncodings": ["utf-16", "utf-8"]
                    }
                }
            }))
            .id(id)
            .finish()
    }

    async fn drive(
        service: &mut LspService<Backend>,
        socket: &mut tower_lsp_server::ClientSocket,
        request: Request,
    ) -> (Option<Response>, Vec<Request>) {
        let mut notifications = Vec::new();
        let call = async { service.ready().await.unwrap().call(request).await.unwrap() };
        tokio::pin!(call);
        loop {
            tokio::select! {
                result = &mut call => {
                    while let Some(msg) = poll_pending(socket) {
                        notifications.push(msg);
                    }
                    return (result, notifications);
                }
                msg = socket.next() => {
                    notifications.push(msg.expect("client socket closed"));
                }
            }
        }
    }

    fn poll_pending(socket: &mut tower_lsp_server::ClientSocket) -> Option<Request> {
        match Pin::new(socket).poll_next(&mut Context::from_waker(futures::task::noop_waker_ref()))
        {
            Poll::Ready(msg) => msg,
            Poll::Pending => None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_advertises_rich_capabilities() {
        let (mut service, mut socket) = test_service();
        let (response, _) = drive(&mut service, &mut socket, initialize_request(1)).await;
        let response = response.expect("response");
        assert!(response.is_ok());
        let result = response.result().expect("result");
        assert_eq!(result["serverInfo"]["name"], "sim");
        assert!(result["capabilities"]["hoverProvider"].as_bool().unwrap());
        assert!(
            result["capabilities"]["definitionProvider"]
                .as_bool()
                .unwrap()
        );
        assert!(result["capabilities"]["completionProvider"].is_object());
        assert!(result["capabilities"]["semanticTokensProvider"].is_object());
        assert!(result["capabilities"]["renameProvider"].is_object());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hover_request_returns_markdown() {
        let (mut service, mut socket) = test_service();
        let _ = drive(&mut service, &mut socket, initialize_request(1)).await;
        let open = Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": "file:///tmp/hover.sim",
                    "languageId": "simula",
                    "version": 1,
                    "text": "begin integer x; x := 1; end"
                }
            }))
            .finish();
        let _ = drive(&mut service, &mut socket, open).await;

        let x_col = "begin integer ".len();
        let hover = Request::build("textDocument/hover")
            .params(json!({
                "textDocument": { "uri": "file:///tmp/hover.sim" },
                "position": { "line": 0, "character": x_col }
            }))
            .id(2)
            .finish();
        let (response, _) = drive(&mut service, &mut socket, hover).await;
        let result = response.unwrap().result().unwrap().clone();
        assert!(
            result["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("integer"),
            "{result}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn definition_from_use() {
        let (mut service, mut socket) = test_service();
        let _ = drive(&mut service, &mut socket, initialize_request(1)).await;
        let text = "begin integer count; count := 1; end";
        let open = Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": "file:///tmp/def.sim",
                    "languageId": "simula",
                    "version": 1,
                    "text": text
                }
            }))
            .finish();
        let _ = drive(&mut service, &mut socket, open).await;

        let use_col = text.rfind("count").unwrap();
        let def = Request::build("textDocument/definition")
            .params(json!({
                "textDocument": { "uri": "file:///tmp/def.sim" },
                "position": { "line": 0, "character": use_col }
            }))
            .id(3)
            .finish();
        let (response, _) = drive(&mut service, &mut socket, def).await;
        let result = response.unwrap().result().unwrap().clone();
        assert_eq!(
            result["range"]["start"]["character"],
            text.find("count").unwrap()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hover_survives_missing_end() {
        let (mut service, mut socket) = test_service();
        let _ = drive(&mut service, &mut socket, initialize_request(1)).await;
        let text = "begin integer count; count := 1;";
        let open = Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": "file:///tmp/partial.sim",
                    "languageId": "simula",
                    "version": 1,
                    "text": text
                }
            }))
            .finish();
        let _ = drive(&mut service, &mut socket, open).await;

        let use_col = text.rfind("count").unwrap();
        let hover = Request::build("textDocument/hover")
            .params(json!({
                "textDocument": { "uri": "file:///tmp/partial.sim" },
                "position": { "line": 0, "character": use_col }
            }))
            .id(4)
            .finish();
        let (response, _) = drive(&mut service, &mut socket, hover).await;
        let result = response.unwrap().result().unwrap().clone();
        assert!(
            result["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("integer"),
            "{result}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rapid_did_change_does_not_deadlock() {
        let (mut service, mut socket) = test_service();
        let _ = drive(&mut service, &mut socket, initialize_request(1)).await;
        let open = Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": "file:///tmp/soak.sim",
                    "languageId": "simula",
                    "version": 1,
                    "text": "begin end"
                }
            }))
            .finish();
        let _ = drive(&mut service, &mut socket, open).await;

        for version in 2..=1000 {
            let change = Request::build("textDocument/didChange")
                .params(json!({
                    "textDocument": {
                        "uri": "file:///tmp/soak.sim",
                        "version": version
                    },
                    "contentChanges": [{
                        "text": format!("begin integer x; x := {version}; end")
                    }]
                }))
                .finish();
            let _ = drive(&mut service, &mut socket, change).await;
        }

        // Final hover should still succeed after the storm of edits.
        let hover = Request::build("textDocument/hover")
            .params(json!({
                "textDocument": { "uri": "file:///tmp/soak.sim" },
                "position": { "line": 0, "character": 14 }
            }))
            .id(99)
            .finish();
        let (response, _) = drive(&mut service, &mut socket, hover).await;
        assert!(response.unwrap().is_ok());
    }

    fn test_uri(s: &str) -> Uri {
        s.parse().expect("uri")
    }

    #[test]
    fn stale_generation_is_not_current() {
        let mut state = State::default();
        let uri = test_uri("file:///tmp/a.sim");
        state
            .documents
            .open(&uri, LANGUAGE_ID.into(), 3, "begin end".into());
        let generation = state.bump_generation(uri.as_str());
        assert!(state.is_current_analysis(&uri, 3, generation));
        let newer = state.bump_generation(uri.as_str());
        assert!(!state.is_current_analysis(&uri, 3, generation));
        assert!(state.is_current_analysis(&uri, 3, newer));
    }

    #[test]
    fn stale_document_version_is_not_current() {
        let mut state = State::default();
        let uri = test_uri("file:///tmp/a.sim");
        state
            .documents
            .open(&uri, LANGUAGE_ID.into(), 1, "begin end".into());
        let generation = state.bump_generation(uri.as_str());
        state
            .documents
            .apply_changes(
                &uri,
                2,
                &[tower_lsp_server::ls_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "begin integer x; x := 1; end".into(),
                }],
                Encoding::Utf16,
            )
            .unwrap();
        assert!(!state.is_current_analysis(&uri, 1, generation));
        assert!(state.is_current_analysis(&uri, 2, generation));
    }

    #[test]
    fn closed_document_is_not_current() {
        let mut state = State::default();
        let uri = test_uri("file:///tmp/a.sim");
        state
            .documents
            .open(&uri, LANGUAGE_ID.into(), 1, "begin end".into());
        let generation = state.bump_generation(uri.as_str());
        state.documents.close(&uri);
        assert!(!state.is_current_analysis(&uri, 1, generation));
        assert!(!is_open_document(&state, uri.as_str()));
    }

    #[test]
    fn is_open_document_matches_live_buffer() {
        let mut state = State::default();
        let uri = test_uri("file:///tmp/a.sim");
        assert!(!is_open_document(&state, uri.as_str()));
        state
            .documents
            .open(&uri, LANGUAGE_ID.into(), 1, "begin end".into());
        assert!(is_open_document(&state, uri.as_str()));
    }
}
