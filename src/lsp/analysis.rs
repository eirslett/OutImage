//! Synchronous front-end analysis for open documents.

use crate::ast::Program;
use crate::error::CompileError;
use crate::lex::{LexOptions, TokenStream, Trivia, tokenize_recovering};
use crate::mir;
use crate::parse;
use crate::semantic;
use crate::source::SourceFile;

use super::config::LspConfig;
use super::lint::{LspLint, unused_symbol_lints};
use super::symbols::SymbolIndex;

/// Options controlling how a document is analyzed for LSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisOptions {
    pub allow_square_bracket_subscripts: bool,
    pub allow_double_dash_comments: bool,
    pub enable_mir_check: bool,
    pub enable_unused_lints: bool,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
            enable_mir_check: false,
            enable_unused_lints: true,
        }
    }
}

impl From<&LspConfig> for AnalysisOptions {
    fn from(config: &LspConfig) -> Self {
        Self {
            allow_square_bracket_subscripts: config.allow_square_bracket_subscripts,
            allow_double_dash_comments: config.allow_double_dash_comments,
            enable_mir_check: config.enable_mir_check,
            enable_unused_lints: config.enable_unused_lints,
        }
    }
}

/// Result of running the compiler front-end on a single buffer.
#[derive(Debug, Clone)]
pub struct AnalysisSnapshot {
    pub text: String,
    pub tokens: Option<TokenStream>,
    /// Comments, directives, and end-comments retained for semantic tokens.
    pub trivia: Vec<Trivia>,
    pub program: Option<Program>,
    pub symbols: Option<SymbolIndex>,
    pub diagnostics: Vec<CompileError>,
    pub lints: Vec<LspLint>,
}

impl AnalysisSnapshot {
    pub fn ok(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Lexes, parses, and semantically analyzes `text`.
pub fn analyze_document(text: &str, options: &AnalysisOptions) -> AnalysisSnapshot {
    let source = SourceFile {
        name: "<lsp>".into(),
        text: text.to_owned(),
    };
    let lex_options = LexOptions {
        allow_square_bracket_subscripts: options.allow_square_bracket_subscripts,
        allow_double_dash_comments: options.allow_double_dash_comments,
    };

    let (tokens, trivia, lex_errors) = tokenize_recovering(&source, &lex_options, true);
    let mut diagnostics = Vec::new();
    for error in lex_errors {
        diagnostics.extend(flatten_error(error));
    }

    let (program, parse_errors) = parse::parse_lenient(&tokens);
    for error in parse_errors {
        diagnostics.extend(flatten_error(error));
    }

    let symbols = SymbolIndex::build(&program, Some(&tokens));

    // Semantic analysis on a partial AST still surfaces useful name/type errors.
    match semantic::analyze_all(&program) {
        Ok(()) => {}
        Err(errors) => diagnostics.extend(flatten_error(errors.into_bundled())),
    }

    if diagnostics.is_empty() && options.enable_mir_check {
        let (_module, mir_errors) = mir::lower_program_lenient(&program);
        for error in mir_errors {
            diagnostics.extend(flatten_error(error));
        }
    }

    let lints = if diagnostics.is_empty() && options.enable_unused_lints {
        unused_symbol_lints(&symbols)
    } else {
        Vec::new()
    };

    AnalysisSnapshot {
        text: text.to_owned(),
        tokens: Some(tokens),
        trivia,
        program: Some(program),
        symbols: Some(symbols),
        diagnostics,
        lints,
    }
}

fn flatten_error(error: CompileError) -> Vec<CompileError> {
    let mut out = Vec::with_capacity(1 + error.related.len());
    let mut related = error.related.clone();
    let mut primary = error;
    primary.related.clear();
    primary
        .notes
        .retain(|note| !note.contains("more error(s) reported"));
    out.push(primary);
    for mut sibling in related.drain(..) {
        let nested = std::mem::take(&mut sibling.related);
        sibling.related.clear();
        out.push(sibling);
        for mut nested_err in nested {
            nested_err.related.clear();
            out.push(nested_err);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_program_has_no_diagnostics() {
        let snap = analyze_document("begin integer x; x := 1; end", &AnalysisOptions::default());
        assert!(snap.ok(), "{:?}", snap.diagnostics);
        assert!(snap.symbols.is_some());
    }

    #[test]
    fn double_dash_line_comments_are_skipped() {
        let snap = analyze_document(
            "begin integer x; x := 1; -- trailing\nend;",
            &AnalysisOptions::default(),
        );
        assert!(snap.ok(), "{:?}", snap.diagnostics);
        assert!(
            snap.trivia
                .iter()
                .any(|item| item.kind == crate::lex::TriviaKind::Comment),
            "{:?}",
            snap.trivia
        );
    }

    #[test]
    fn lex_error_collected() {
        let snap = analyze_document("begin @@@", &AnalysisOptions::default());
        assert!(!snap.ok());
        assert_eq!(snap.diagnostics[0].phase.as_str(), "lex");
    }

    #[test]
    fn parse_error_collected() {
        let snap = analyze_document("begin integer", &AnalysisOptions::default());
        assert!(!snap.ok());
        assert_eq!(snap.diagnostics[0].phase.as_str(), "parse");
        assert!(snap.tokens.is_some());
        // Lenient parse still yields a partial program / symbol index.
        assert!(snap.program.is_some());
        assert!(snap.symbols.is_some());
    }

    #[test]
    fn missing_end_keeps_symbols_for_navigation() {
        let snap = analyze_document("begin integer x; x := 1;", &AnalysisOptions::default());
        assert!(!snap.ok());
        assert!(
            snap.diagnostics
                .iter()
                .any(|d| d.phase.as_str() == "parse" && d.message.contains("end")),
            "{:?}",
            snap.diagnostics
        );
        let symbols = snap.symbols.as_ref().expect("symbols");
        assert!(
            symbols.symbols.iter().any(|s| s.name == "x"),
            "{:?}",
            symbols.symbols
        );
    }

    #[test]
    fn recovered_statement_keeps_later_calls() {
        let snap = analyze_document(
            "begin integer x; x := ; OutImage; end;",
            &AnalysisOptions::default(),
        );
        assert!(!snap.ok());
        assert!(snap.program.is_some());
        let symbols = snap.symbols.as_ref().expect("symbols");
        assert!(symbols.symbols.iter().any(|s| s.name == "x"));
    }

    #[test]
    fn semantic_error_collected() {
        let snap = analyze_document(
            "begin integer x; x := true; end",
            &AnalysisOptions::default(),
        );
        assert!(!snap.ok());
        assert_eq!(snap.diagnostics[0].phase.as_str(), "semantic");
    }

    #[test]
    fn mir_check_runs_when_enabled() {
        let opts = AnalysisOptions {
            enable_mir_check: true,
            ..Default::default()
        };
        let snap = analyze_document("begin integer x; x := 1; end", &opts);
        assert!(snap.ok(), "{:?}", snap.diagnostics);
    }

    #[test]
    fn unused_lints_on_clean_program() {
        let snap = analyze_document(
            "begin integer x; integer y; y := 1; end",
            &AnalysisOptions::default(),
        );
        assert!(snap.ok());
        assert!(
            snap.lints.iter().any(|l| l.message.contains("`x`")),
            "{:?}",
            snap.lints
        );
    }
}
