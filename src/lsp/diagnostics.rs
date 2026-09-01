//! Convert [`CompileError`] into LSP [`Diagnostic`]s.

use std::str::FromStr;

use tower_lsp_server::ls_types::{
    CodeDescription, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag,
    Location, NumberOrString, Range, Uri,
};

use crate::error::{CompileError, DiagnosticLabel, Span};

use super::lint::LspLint;
use super::position::{Encoding, byte_span_to_range};

const SOURCE: &str = "sim";

/// Long-form help for a [`CompileError::report_code`] value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportCodeHelp {
    pub code: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub detail: &'static str,
}

/// Returns documentation for known diagnostic codes (`E0201`, `E-lex`, …).
pub fn report_code_help(code: &str) -> Option<ReportCodeHelp> {
    if let Some(entry) = crate::diagnostics::lookup(code) {
        return Some(ReportCodeHelp {
            code: entry.code,
            title: entry.title,
            summary: entry.summary,
            detail: entry.explain,
        });
    }
    match code.trim().to_ascii_lowercase().as_str() {
        "e-lex" | "lex" => Some(ReportCodeHelp {
            code: "E-lex",
            title: "Lexical analysis failed",
            summary: "Unexpected character, malformed literal, or missing separator.",
            detail: "See docs/ERROR_CODES.md and run `sim explain E-lex`.",
        }),
        "e-parse" | "parse" => Some(ReportCodeHelp {
            code: "E-parse",
            title: "Syntax analysis failed",
            summary: "Unexpected token or incomplete declaration/statement.",
            detail: "See docs/ERROR_CODES.md and run `sim explain E-parse`.",
        }),
        "e-semantic" | "semantic" => Some(ReportCodeHelp {
            code: "E-semantic",
            title: "Static semantic check failed",
            summary: "Unknown name, type mismatch, or visibility violation.",
            detail: "See docs/ERROR_CODES.md and run `sim explain E-semantic`.",
        }),
        "e-codegen" | "codegen" => Some(ReportCodeHelp {
            code: "E-codegen",
            title: "Lowering or code generation failed",
            summary: "Unsupported construct for the target or runtime preparation error.",
            detail: "See docs/ERROR_CODES.md and run `sim explain E-codegen`.",
        }),
        "e-runtime" | "runtime" => Some(ReportCodeHelp {
            code: "E-runtime",
            title: "Runtime failure",
            summary: "A Standard runtime condition failed.",
            detail: "See docs/ERROR_CODES.md and run `sim explain E-runtime`.",
        }),
        "i-internal" | "ice" | "internal" | "i0001" => Some(ReportCodeHelp {
            code: "I0001",
            title: "Internal compiler error",
            summary: "The compiler hit an unexpected invariant.",
            detail: "This is a sim bug. See `sim explain I0001`.",
        }),
        "w-unused" | "unused" | "w0001" => Some(ReportCodeHelp {
            code: "W0001",
            title: "Unused declaration",
            summary: "A local, parameter, or label is never referenced.",
            detail: "Remove it or reference it; this is a warning (`sim explain W0001`).",
        }),
        _ => None,
    }
}

/// Converts compiler errors into LSP diagnostics for `uri`.
pub fn compile_errors_to_diagnostics(
    errors: &[CompileError],
    text: &str,
    uri: &Uri,
    encoding: Encoding,
) -> Vec<Diagnostic> {
    errors
        .iter()
        .map(|error| compile_error_to_diagnostic(error, text, uri, encoding))
        .collect()
}

/// Merge compiler errors and soft LSP lints into one diagnostic list.
pub fn snapshot_diagnostics(
    errors: &[CompileError],
    lints: &[LspLint],
    text: &str,
    uri: &Uri,
    encoding: Encoding,
) -> Vec<Diagnostic> {
    let mut out = compile_errors_to_diagnostics(errors, text, uri, encoding);
    out.extend(
        lints
            .iter()
            .map(|lint| lint_to_diagnostic(lint, text, uri, encoding)),
    );
    out
}

fn lint_to_diagnostic(lint: &LspLint, text: &str, _uri: &Uri, encoding: Encoding) -> Diagnostic {
    let range = byte_span_to_range(text, lint.span.clone(), encoding);
    let mut tags = Vec::new();
    if lint.unnecessary {
        tags.push(DiagnosticTag::UNNECESSARY);
    }
    if lint.deprecated {
        tags.push(DiagnosticTag::DEPRECATED);
    }
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::WARNING),
        code: Some(NumberOrString::String(lint.code.into())),
        code_description: code_description_for(lint.code),
        source: Some(SOURCE.into()),
        message: lint.message.clone(),
        related_information: None,
        tags: if tags.is_empty() { None } else { Some(tags) },
        data: None,
    }
}

fn compile_error_to_diagnostic(
    error: &CompileError,
    text: &str,
    uri: &Uri,
    encoding: Encoding,
) -> Diagnostic {
    let range = error
        .span
        .as_ref()
        .map(|span| byte_span_to_range(text, span.clone(), encoding))
        .unwrap_or_else(|| Range::new(Default::default(), Default::default()));

    let mut related = Vec::new();
    for label in &error.labels {
        related.push(label_to_related(label, text, uri, encoding));
    }
    for note in &error.notes {
        related.push(DiagnosticRelatedInformation {
            location: Location::new(uri.clone(), range),
            message: format!("note: {note}"),
        });
    }
    for help in &error.helps {
        related.push(DiagnosticRelatedInformation {
            location: Location::new(uri.clone(), range),
            message: format!("help: {help}"),
        });
    }

    let mut message = error.message.clone();
    if let Some(primary) = &error.primary_message
        && primary != "here"
    {
        message = format!("{message} ({primary})");
    }

    Diagnostic {
        range,
        severity: Some(match error.severity {
            crate::diagnostics::Severity::Warning => DiagnosticSeverity::WARNING,
            crate::diagnostics::Severity::Error | crate::diagnostics::Severity::Ice => {
                DiagnosticSeverity::ERROR
            }
        }),
        code: Some(NumberOrString::String(error.report_code().to_owned())),
        code_description: code_description_for(error.report_code()),
        source: Some(SOURCE.into()),
        message,
        related_information: if related.is_empty() {
            None
        } else {
            Some(related)
        },
        tags: None,
        data: None,
    }
}

fn label_to_related(
    label: &DiagnosticLabel,
    text: &str,
    uri: &Uri,
    encoding: Encoding,
) -> DiagnosticRelatedInformation {
    let range = span_range(text, &label.span, encoding);
    DiagnosticRelatedInformation {
        location: Location::new(uri.clone(), range),
        message: label.message.clone(),
    }
}

fn span_range(text: &str, span: &Span, encoding: Encoding) -> Range {
    byte_span_to_range(text, span.clone(), encoding)
}

fn code_description_for(code: &str) -> Option<CodeDescription> {
    crate::diagnostics::lookup(code)?;
    let href = crate::diagnostics::explain_doc_url(code);
    Some(CodeDescription {
        href: Uri::from_str(&href).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::analysis::{AnalysisOptions, analyze_document};
    use std::str::FromStr;

    fn file_uri() -> Uri {
        Uri::from_str("file:///tmp/test.sim").unwrap()
    }

    #[test]
    fn lex_diagnostic_has_code_and_range() {
        let snap = analyze_document("begin @@@", &AnalysisOptions::default());
        let diags = compile_errors_to_diagnostics(
            &snap.diagnostics,
            &snap.text,
            &file_uri(),
            Encoding::Utf16,
        );
        assert!(
            diags
                .iter()
                .any(|d| d.code == Some(NumberOrString::String("E0001".into()))),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .filter(|d| d.code == Some(NumberOrString::String("E0001".into())))
                .count()
                >= 3,
            "lex recovery should report each stray character: {diags:?}"
        );
        assert_eq!(diags[0].source.as_deref(), Some("sim"));
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(diags[0].range.start.character > 0 || diags[0].range.start.line == 0);
    }

    #[test]
    fn semantic_diagnostic_maps_span() {
        let source = "begin integer x; x := true; end";
        let snap = analyze_document(source, &AnalysisOptions::default());
        let diags = compile_errors_to_diagnostics(
            &snap.diagnostics,
            &snap.text,
            &file_uri(),
            Encoding::Utf16,
        );
        assert!(!diags.is_empty());
        assert_eq!(diags[0].code, Some(NumberOrString::String("E0201".into())));
        assert!(
            diags[0].code_description.is_some(),
            "catalogued codes should carry a docs href"
        );
        // The assignment / `true` should not be at document start.
        assert!(
            diags[0].range.start.character > 0 || diags[0].range.start.line > 0,
            "expected non-zero range, got {:?}",
            diags[0].range
        );
    }

    #[test]
    fn clean_file_yields_no_diagnostics() {
        let snap = analyze_document("begin integer x; x := 1; end", &AnalysisOptions::default());
        let diags = compile_errors_to_diagnostics(
            &snap.diagnostics,
            &snap.text,
            &file_uri(),
            Encoding::Utf16,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn unused_lint_maps_to_warning_with_tag() {
        let snap = analyze_document(
            "begin integer x; integer y; y := 1; end",
            &AnalysisOptions::default(),
        );
        let diags = snapshot_diagnostics(
            &snap.diagnostics,
            &snap.lints,
            &snap.text,
            &file_uri(),
            Encoding::Utf16,
        );
        let unused = diags
            .iter()
            .find(|d| {
                d.code == Some(NumberOrString::String("W0001".into()))
                    || d.code == Some(NumberOrString::String("W-unused".into()))
            })
            .expect("unused lint");
        assert_eq!(unused.severity, Some(DiagnosticSeverity::WARNING));
        assert!(
            unused.code_description.is_some(),
            "W0001 should link to the explain page"
        );
        assert!(
            unused
                .tags
                .as_ref()
                .is_some_and(|t| t.contains(&DiagnosticTag::UNNECESSARY))
        );
    }

    #[test]
    fn report_code_help_known_codes() {
        let help = report_code_help("E-parse").expect("help");
        assert_eq!(help.code, "E-parse");
        assert!(help.summary.contains("token"));
    }
}
