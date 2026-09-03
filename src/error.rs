//! Compiler diagnostics, rendered with [ariadne](https://codeberg.org/zesterer/ariadne).
//!
//! [`CompileError`] is the structured error type used across phases. Call
//! [`CompileError::eprint`] / [`CompileError::write`] / [`CompileError::render`]
//! to produce labelled source snippets (including any [`CompileError::related`]
//! siblings). [`CompileErrors`] collects multiple failures from analysis;
//! rendering is controlled by [`DiagnosticConfig`] (colour, charset, compact mode).

use crate::diagnostics::Severity;
use crate::source::{CompositeSource, SourceFile};
use ariadne::{
    Cache, CharSet, Color, ColorGenerator, Config, IndexType, Label, Report, ReportKind, Source,
    sources,
};
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io::{self, IsTerminal, Write};

pub type Span = std::ops::Range<usize>;

/// Identifier for a source file in multi-file reports (usually a path or `"<input>"`).
pub type SourceId = String;

/// Colour policy for diagnostic rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Enable colour when writing to a TTY and neither `NO_COLOR` nor
    /// `CLICOLOR=0` is set; honour `FORCE_COLOR` / `CLICOLOR_FORCE`.
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    /// Resolves whether ANSI colour should be used for the given stream.
    pub fn enabled(self, stream_is_tty: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                if env_flag_set("NO_COLOR") {
                    return false;
                }
                if env_flag_set("FORCE_COLOR") || env_flag_set("CLICOLOR_FORCE") {
                    return true;
                }
                if env::var_os("CLICOLOR").is_some_and(|v| v == "0") {
                    return false;
                }
                if env::var_os("TERM").is_some_and(|v| v == "dumb") {
                    return false;
                }
                stream_is_tty
            }
        }
    }
}

fn env_flag_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn is_toolchain_message(message: &str) -> bool {
    message.contains("failed to write")
        || message.contains("object builder failed")
        || message.contains("failed to emit object")
        || message.contains("invalid target triple")
        || message.contains("unsupported target triple")
        || message.contains("failed to build ISA")
        || message.contains("failed to invoke")
        || message.contains("invalid Cranelift flag")
        || message.starts_with("failed to read ")
        || message.contains("has no file stem")
        || message.contains("duplicate --with")
}

/// How much tutorial text (`Note` / `Help` / suggestions) to include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExplainLevel {
    /// Full Elm-style body (default).
    #[default]
    Full,
    /// Snippet and labels only — skip notes, helps, and suggestions.
    Short,
}

/// Rendering options for ariadne reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticConfig {
    pub color: ColorChoice,
    /// Prefer Unicode box-drawing when true; ASCII otherwise (better for CI logs).
    pub unicode: bool,
    pub compact: bool,
    pub explain: ExplainLevel,
}

impl Default for DiagnosticConfig {
    fn default() -> Self {
        Self::for_stderr()
    }
}

impl DiagnosticConfig {
    /// Colour auto-detected for stderr; Unicode when colour would be on.
    pub fn for_stderr() -> Self {
        let color = ColorChoice::Auto;
        let unicode = color.enabled(io::stderr().is_terminal());
        Self {
            color,
            unicode,
            compact: false,
            explain: ExplainLevel::Full,
        }
    }

    /// Stable, colourless output for tests and tooling.
    pub fn colorless() -> Self {
        Self {
            color: ColorChoice::Never,
            unicode: true,
            compact: false,
            explain: ExplainLevel::Full,
        }
    }

    /// Unicode boxes plus ANSI colour — xterm / the in-browser playground.
    pub fn ansi() -> Self {
        Self {
            color: ColorChoice::Always,
            unicode: true,
            compact: false,
            explain: ExplainLevel::Full,
        }
    }

    /// Compact, ASCII, no colour — suitable for constrained terminals / CI.
    pub fn plain() -> Self {
        Self {
            color: ColorChoice::Never,
            unicode: false,
            compact: true,
            explain: ExplainLevel::Short,
        }
    }

    pub fn with_color(mut self, color: ColorChoice) -> Self {
        self.color = color;
        self
    }

    pub fn with_unicode(mut self, unicode: bool) -> Self {
        self.unicode = unicode;
        self
    }

    pub fn with_compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    pub fn with_explain(mut self, explain: ExplainLevel) -> Self {
        self.explain = explain;
        self
    }

    fn color_enabled(&self, stream_is_tty: bool) -> bool {
        self.color.enabled(stream_is_tty)
    }

    fn ariadne_config(&self, stream_is_tty: bool) -> Config {
        Config::default()
            .with_color(self.color_enabled(stream_is_tty))
            .with_index_type(IndexType::Byte)
            .with_char_set(if self.unicode {
                CharSet::Unicode
            } else {
                CharSet::Ascii
            })
            .with_compact(self.compact)
    }
}

/// An additional labelled span on a diagnostic (secondary / related location).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticLabel {
    pub span: Span,
    pub message: String,
    /// When set, the label refers to a different file than the primary source.
    pub source: Option<SourceId>,
}

impl DiagnosticLabel {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            source: None,
        }
    }

    pub fn in_source(mut self, source: impl Into<SourceId>) -> Self {
        self.source = Some(source.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Lex,
    Parse,
    Semantic,
    Codegen,
    /// Standard runtime failure (bounds, none-ref, …) — not a compiler bug.
    Runtime,
    /// Broken compiler invariant (ICE).
    Internal,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lex => "lex",
            Self::Parse => "parse",
            Self::Semantic => "semantic",
            Self::Codegen => "codegen",
            Self::Runtime => "runtime",
            Self::Internal => "internal",
        }
    }

    /// Stable diagnostic code family for this phase (`E-lex`, `E-parse`, …).
    pub fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Lex => "E-lex",
            Self::Parse => "E-parse",
            Self::Semantic => "E-semantic",
            Self::Codegen => "E-codegen",
            Self::Runtime => "E-runtime",
            Self::Internal => "I-internal",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structured compiler error. Display is a short one-liner; use
/// [`Self::render`] / [`Self::eprint`] for labelled source context.
///
/// When semantic analysis collects multiple independent failures, the first
/// error may carry siblings in [`Self::related`]; rendering prints them all.
#[derive(Debug, Clone)]
pub struct CompileError {
    pub phase: Phase,
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
    /// When set (e.g. after [`Self::remap_to_origins`]), the primary report
    /// location uses this source id instead of the `primary` argument's name.
    pub primary_source: Option<SourceId>,
    /// Override for the primary underline text (default: `"here"`).
    pub primary_message: Option<String>,
    pub labels: Vec<DiagnosticLabel>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
    /// Override the report code (default: [`Phase::diagnostic_code`]).
    pub code: Option<String>,
    /// Catalog title (`TYPE MISMATCH`) when this error came from a [`crate::diagnostics::DiagId`].
    pub title: Option<String>,
    /// Machine-applicable or advisory edits (CLI help + LSP code actions).
    pub suggestions: Vec<crate::diagnostics::Suggestion>,
    /// Typed parameters for `--json` (`found`, `expected`, …).
    pub params: Vec<(String, String)>,
    /// Additional independent errors reported alongside this one (siblings).
    pub related: Vec<CompileError>,
}

impl CompileError {
    fn new(phase: Phase, message: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            phase,
            severity: Severity::Error,
            message: message.into(),
            span,
            primary_source: None,
            primary_message: None,
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            code: None,
            title: None,
            suggestions: Vec::new(),
            params: Vec::new(),
            related: Vec::new(),
        }
    }

    /// Builds a [`CompileError`] from a catalogued [`crate::diagnostics::Diagnostic`].
    pub fn from_diagnostic(diag: crate::diagnostics::Diagnostic) -> Self {
        let mut error = Self::new(diag.id.phase(), diag.message, diag.span);
        error.code = Some(diag.id.code().to_string());
        error.title = Some(diag.id.title().to_string());
        error.primary_message = diag.primary_message;
        error.labels = diag.labels;
        error.notes = diag.notes;
        error.helps = diag.helps;
        error.suggestions = diag.suggestions;
        error.params = diag.params;
        error.severity = diag.id.severity();
        error
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(Phase::Runtime, message, None)
    }

    pub fn runtime_at(message: impl Into<String>, span: Span) -> Self {
        Self::new(Phase::Runtime, message, Some(span))
    }

    pub fn lex(message: impl Into<String>, span: Span) -> Self {
        Self::new(Phase::Lex, message, Some(span))
    }

    pub fn lex_at(message: impl Into<String>, offset: usize) -> Self {
        Self::lex(message, offset..offset)
    }

    pub fn parse(message: impl Into<String>, span: Option<Span>) -> Self {
        Self::new(Phase::Parse, message, span)
    }

    pub fn semantic(message: impl Into<String>) -> Self {
        Self::new(Phase::Semantic, message, None)
    }

    pub fn semantic_at(message: impl Into<String>, span: Span) -> Self {
        Self::new(Phase::Semantic, message, Some(span))
    }

    pub fn codegen(message: impl Into<String>) -> Self {
        Self::from_codegen_message(message.into(), None)
    }

    pub fn ice(detail: impl Into<String>) -> Self {
        crate::diagnostics::ice(detail)
    }

    pub fn codegen_at(message: impl Into<String>, span: Span) -> Self {
        Self::from_codegen_message(message.into(), Some(span))
    }

    fn from_codegen_message(message: String, span: Option<Span>) -> Self {
        let ice_like = message.starts_with("MIR interp:")
            || message.starts_with("MIR wasm:")
            || message.starts_with("internal error:")
            || message.contains("MIR interp:");
        if ice_like {
            let mut err = crate::diagnostics::ice(message);
            if let Some(span) = span {
                err = err.with_span(span);
            }
            return err;
        }
        if let Some(rest) = message.strip_prefix("MIR lowering:") {
            return crate::diagnostics::not_lowered(rest.trim(), span);
        }
        if is_toolchain_message(&message) {
            return crate::diagnostics::toolchain(message, span);
        }
        Self::new(Phase::Codegen, message, span)
    }

    pub fn with_primary_message(mut self, message: impl Into<String>) -> Self {
        self.primary_message = Some(message.into());
        self
    }

    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(DiagnosticLabel::new(span, message));
        self
    }

    pub fn with_diagnostic_label(mut self, label: DiagnosticLabel) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_related(mut self, related: impl Into<Vec<CompileError>>) -> Self {
        self.related = related.into();
        self
    }

    /// Attach a source span when this error does not already have one.
    ///
    /// Empty / synthetic spans (`start >= end`) are ignored so MIR glue does
    /// not underline the start of the file for compiler-generated ops.
    pub fn with_span(mut self, span: Span) -> Self {
        if self.span.is_none() && span.start < span.end {
            self.span = Some(span);
        }
        self
    }

    pub fn with_primary_source(mut self, source: impl Into<SourceId>) -> Self {
        self.primary_source = Some(source.into());
        self
    }

    /// Remaps composite-buffer spans onto origin files via `map`.
    ///
    /// Updates the primary [`Self::span`], sets [`Self::primary_source`], and
    /// remaps each label span. Labels whose origin differs from the primary
    /// file get [`DiagnosticLabel::source`] set so multi-file ariadne reports
    /// resolve correctly through a [`SourceCache`] from
    /// [`CompositeSource::to_cache`].
    pub fn remap_to_origins(&self, map: &CompositeSource) -> CompileError {
        let mut out = self.clone();
        let primary_id = if let Some(span) = &self.span {
            let (id, local) = map.localize(span.clone());
            out.span = Some(local);
            out.primary_source = Some(id.clone());
            Some(id)
        } else {
            out.primary_source = self.primary_source.clone();
            self.primary_source.clone()
        };

        out.labels = self
            .labels
            .iter()
            .map(|label| {
                // Labels that already name an origin keep that id; otherwise
                // treat the span as a composite offset.
                if let Some(existing) = &label.source {
                    let mut remapped = label.clone();
                    remapped.source = Some(existing.clone());
                    remapped
                } else {
                    let (id, local) = map.localize(label.span.clone());
                    let mut remapped = DiagnosticLabel::new(local, label.message.clone());
                    if primary_id.as_ref() != Some(&id) {
                        remapped.source = Some(id);
                    }
                    remapped
                }
            })
            .collect();

        out.suggestions = self
            .suggestions
            .iter()
            .map(|suggestion| {
                let mut remapped = suggestion.clone();
                if let Some(span) = &suggestion.span {
                    let (_id, local) = map.localize(span.clone());
                    remapped.span = Some(local);
                }
                remapped
            })
            .collect();

        out.related = self
            .related
            .iter()
            .map(|related| related.remap_to_origins(map))
            .collect();

        out
    }

    /// Stable diagnostic code (`E-lex`, …), honouring any explicit override.
    pub fn report_code(&self) -> &str {
        self.code
            .as_deref()
            .unwrap_or_else(|| self.phase.diagnostic_code())
    }

    /// Machine-readable diagnostic for `--json` / tooling (one object per error).
    pub fn to_json_value(&self) -> serde_json::Value {
        let mut related = Vec::new();
        for sibling in &self.related {
            related.push(sibling.to_json_value());
        }
        let params: serde_json::Map<String, serde_json::Value> = self
            .params
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect();
        let suggestions: Vec<serde_json::Value> = self
            .suggestions
            .iter()
            .map(|suggestion| {
                serde_json::json!({
                    "message": suggestion.message,
                    "replacement": suggestion.replacement,
                    "span": suggestion.span.as_ref().map(|span| {
                        serde_json::json!({ "start": span.start, "end": span.end })
                    }),
                })
            })
            .collect();
        serde_json::json!({
            "code": self.report_code(),
            "title": self.title,
            "phase": self.phase.as_str(),
            "message": self.message,
            "span": self.span.as_ref().map(|span| {
                serde_json::json!({ "start": span.start, "end": span.end })
            }),
            "source": self.primary_source,
            "notes": self.notes,
            "helps": self.helps,
            "suggestions": suggestions,
            "params": params,
            "severity": match self.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Ice => "ice",
            },
            "related": related,
        })
    }

    /// This diagnostic and each related sibling as JSON objects.
    pub fn to_json_values(&self) -> Vec<serde_json::Value> {
        let mut items = vec![self.to_json_value()];
        fn walk(err: &CompileError, items: &mut Vec<serde_json::Value>) {
            for related in &err.related {
                items.push(related.to_json_value());
                walk(related, items);
            }
        }
        walk(self, &mut items);
        items
    }

    /// JSON array of this diagnostic and each related sibling — playground / wasm.
    pub fn to_json_bundle(&self) -> String {
        serde_json::Value::Array(self.to_json_values()).to_string()
    }

    /// Playground payload: Ariadne text plus machine-readable spans.
    pub fn to_playground_payload(&self, source: &SourceFile) -> String {
        serde_json::json!({
            "report": self.render_with_config(source, &DiagnosticConfig::ansi()),
            "diagnostics": self.to_json_values(),
        })
        .to_string()
    }

    /// Builds an ariadne [`Report`] for this error against `primary` source.
    ///
    /// Labels that name another source id must also appear in `cache`.
    /// When [`Self::primary_source`] is set, that id is used for the report
    /// location (text is taken from `cache`).
    pub fn build_report<'a>(
        &self,
        primary: &SourceFile,
        cache: &SourceCache,
        config: &DiagnosticConfig,
        stream_is_tty: bool,
    ) -> Report<'a, (SourceId, Span)> {
        let primary_id = self
            .primary_source
            .clone()
            .unwrap_or_else(|| primary.name.clone());
        let primary_span = self.span.clone().unwrap_or(0..0);
        let headline = match &self.title {
            Some(title) => format!("{title}: {}", self.message),
            None => format!("{} error: {}", self.phase, self.message),
        };

        let kind = match self.severity {
            Severity::Error | Severity::Ice => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
        };
        let mut builder = Report::build(kind, (primary_id.clone(), primary_span.clone()))
            .with_code(self.report_code())
            .with_message(headline)
            .with_config(config.ariadne_config(stream_is_tty));

        let mut colors = ColorGenerator::new();
        let primary_color = match self.severity {
            Severity::Error | Severity::Ice => Color::Red,
            Severity::Warning => Color::Yellow,
        };

        if self.span.is_some() {
            let msg = self
                .primary_message
                .clone()
                .unwrap_or_else(|| "here".to_string());
            builder = builder.with_label(
                Label::new((primary_id.clone(), primary_span))
                    .with_message(msg)
                    .with_color(primary_color)
                    .with_priority(1),
            );
        }

        for label in &self.labels {
            let id = label.source.clone().unwrap_or_else(|| primary_id.clone());
            // Ensure the id is resolvable when the report is written.
            debug_assert!(
                cache.contains(&id) || id == primary_id,
                "diagnostic label source '{id}' is missing from SourceCache"
            );
            let color = colors.next();
            builder = builder.with_label(
                Label::new((id, label.span.clone()))
                    .with_message(label.message.clone())
                    .with_color(color),
            );
        }

        if config.explain == ExplainLevel::Full {
            for note in &self.notes {
                builder = builder.with_note(note.clone());
            }
            for help in &self.helps {
                builder = builder.with_help(help.clone());
            }
            for suggestion in &self.suggestions {
                builder = builder.with_help(format!("suggestion: {}", suggestion.message));
            }
        }

        builder.finish()
    }

    /// Writes this diagnostic using `config`. `stream_is_tty` affects Auto colour.
    /// Related sibling errors (including nested ones) are written after the primary.
    pub fn write_with_config<W: Write>(
        &self,
        source: &SourceFile,
        mut w: W,
        config: &DiagnosticConfig,
        stream_is_tty: bool,
    ) -> io::Result<()> {
        let cache = SourceCache::from_file(source);
        self.write_primary_and_related(&cache, source, &mut w, config, stream_is_tty)
    }

    fn write_primary_and_related<W: Write>(
        &self,
        cache: &SourceCache,
        primary: &SourceFile,
        w: &mut W,
        config: &DiagnosticConfig,
        stream_is_tty: bool,
    ) -> io::Result<()> {
        self.write_cached(cache, primary, &mut *w, config, stream_is_tty)?;
        for related in &self.related {
            related.write_primary_and_related(cache, primary, w, config, stream_is_tty)?;
        }
        Ok(())
    }

    /// Writes using an explicit multi-file [`SourceCache`].
    ///
    /// Does not recurse into [`Self::related`]; use [`Self::write_with_config`]
    /// when sibling errors should be included.
    pub fn write_cached<W: Write>(
        &self,
        cache: &SourceCache,
        primary: &SourceFile,
        mut w: W,
        config: &DiagnosticConfig,
        stream_is_tty: bool,
    ) -> io::Result<()> {
        if config.compact {
            return w.write_all(self.compact_line(primary).as_bytes());
        }
        let report = self.build_report(primary, cache, config, stream_is_tty);
        report.write(cache.ariadne_cache(), w)
    }

    fn compact_line(&self, primary: &SourceFile) -> String {
        let code = self.report_code();
        let title = self.title.as_deref().unwrap_or(self.phase.as_str());
        let offset = self.span.as_ref().map(|span| span.start).unwrap_or(0);
        let (line, col) = crate::source::span_to_line_col(&primary.text, offset);
        let file = self
            .primary_source
            .as_deref()
            .unwrap_or(primary.name.as_str());
        format!("{code} {title}: {file}:{line}:{col}: {}\n", self.message)
    }

    /// Writes with an explicit colour flag (tests / simple callers).
    pub fn write<W: Write>(&self, source: &SourceFile, w: W, color: bool) -> io::Result<()> {
        let config = if color {
            DiagnosticConfig::for_stderr().with_color(ColorChoice::Always)
        } else {
            DiagnosticConfig::colorless()
        };
        self.write_with_config(source, w, &config, color)
    }

    /// Prints to stderr using [`DiagnosticConfig::for_stderr`], overridden by `config` when given.
    pub fn eprint(&self, source: &SourceFile) -> io::Result<()> {
        self.eprint_with_config(source, &DiagnosticConfig::for_stderr())
    }

    pub fn eprint_with_config(
        &self,
        source: &SourceFile,
        config: &DiagnosticConfig,
    ) -> io::Result<()> {
        let tty = io::stderr().is_terminal();
        let cache = SourceCache::from_file(source);
        self.eprint_primary_and_related(source, &cache, config, tty)
    }

    fn eprint_primary_and_related(
        &self,
        source: &SourceFile,
        cache: &SourceCache,
        config: &DiagnosticConfig,
        tty: bool,
    ) -> io::Result<()> {
        if config.compact {
            eprint!("{}", self.compact_line(source));
            for related in &self.related {
                related.eprint_primary_and_related(source, cache, config, tty)?;
            }
            return Ok(());
        }
        let report = self.build_report(source, cache, config, tty);
        report.eprint(cache.ariadne_cache())?;
        for related in &self.related {
            related.eprint_primary_and_related(source, cache, config, tty)?;
        }
        Ok(())
    }

    /// Colourless rendering for tests and tooling (includes related errors).
    pub fn render(&self, source: &SourceFile) -> String {
        self.render_with_config(source, &DiagnosticConfig::colorless())
    }

    pub fn render_with_config(&self, source: &SourceFile, config: &DiagnosticConfig) -> String {
        let want_color = config.color_enabled(false);
        // Ariadne paints through yansi, which starts off on Windows when
        // CONOUT$ has no VT mode. Enable for Always colour so Vec renders
        // actually contain escapes. Do not disable: colourless output already
        // omits styles via Config::with_color(false), and disable() is
        // process-global so it races with parallel Always-colour writes.
        if want_color {
            yansi::enable();
        }
        let mut buf = Vec::new();
        self.write_with_config(source, &mut buf, config, want_color)
            .expect("writing an ariadne report to a Vec should not fail");
        String::from_utf8(buf).expect("ariadne output is UTF-8")
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.severity {
            Severity::Error | Severity::Ice => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{} {}: {}", self.phase, kind, self.message)
    }
}

impl std::error::Error for CompileError {}

/// Non-empty collection of [`CompileError`]s from multi-error analysis.
#[derive(Debug, Clone)]
pub struct CompileErrors {
    errors: Vec<CompileError>,
}

impl CompileErrors {
    /// Creates a collection. Panics if `errors` is empty.
    pub fn new(errors: Vec<CompileError>) -> Self {
        assert!(
            !errors.is_empty(),
            "CompileErrors must contain at least one error"
        );
        Self { errors }
    }

    pub fn len(&self) -> usize {
        self.errors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn as_slice(&self) -> &[CompileError] {
        &self.errors
    }

    pub fn first(&self) -> &CompileError {
        &self.errors[0]
    }

    /// Returns the first error only (no [`CompileError::related`] packing).
    pub fn into_first(mut self) -> CompileError {
        self.errors.remove(0)
    }

    /// Packs siblings into the first error's [`CompileError::related`] and adds
    /// a note when more than one error was collected. Nested `related` lists are
    /// flattened so rendering prints every diagnostic once.
    pub fn into_bundled(mut self) -> CompileError {
        let mut first = self.errors.remove(0);
        if self.errors.is_empty() && first.related.is_empty() {
            return first;
        }
        let mut related = std::mem::take(&mut first.related);
        for mut error in self.errors {
            related.append(&mut error.related);
            related.push(error);
        }
        let n = related.len();
        first
            .with_related(related)
            .with_note(format!("{n} more error(s) reported"))
    }

    pub fn eprint_all(&self, source: &SourceFile, config: &DiagnosticConfig) -> io::Result<()> {
        for error in &self.errors {
            error.eprint_with_config(source, config)?;
        }
        Ok(())
    }

    /// Colourless rendering of every error, joined with a blank line.
    pub fn render_all(&self, source: &SourceFile) -> String {
        self.errors
            .iter()
            .map(|error| error.render(source))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn iter(&self) -> impl Iterator<Item = &CompileError> {
        self.errors.iter()
    }
}

impl From<CompileError> for CompileErrors {
    fn from(error: CompileError) -> Self {
        Self::new(vec![error])
    }
}

impl From<Vec<CompileError>> for CompileErrors {
    fn from(errors: Vec<CompileError>) -> Self {
        Self::new(errors)
    }
}

impl IntoIterator for CompileErrors {
    type Item = CompileError;
    type IntoIter = std::vec::IntoIter<CompileError>;

    fn into_iter(self) -> Self::IntoIter {
        self.errors.into_iter()
    }
}

impl fmt::Display for CompileErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.first())?;
        let rest = self.len() - 1;
        if rest > 0 {
            write!(f, " ({rest} more error(s))")?;
        }
        Ok(())
    }
}

impl std::error::Error for CompileErrors {}

/// Collection of named sources for multi-file ariadne reports.
#[derive(Debug, Clone, Default)]
pub struct SourceCache {
    files: HashMap<SourceId, String>,
}

impl SourceCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_file(source: &SourceFile) -> Self {
        let mut cache = Self::new();
        cache.insert(source);
        cache
    }

    pub fn insert(&mut self, source: &SourceFile) -> &mut Self {
        self.files.insert(source.name.clone(), source.text.clone());
        self
    }

    pub fn insert_named(
        &mut self,
        name: impl Into<SourceId>,
        text: impl Into<String>,
    ) -> &mut Self {
        self.files.insert(name.into(), text.into());
        self
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        self.files.get(id).map(String::as_str)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.files.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Builds an ariadne [`Cache`] snapshot (owned copies of current contents).
    pub fn ariadne_cache(&self) -> impl Cache<SourceId> {
        sources(
            self.files
                .iter()
                .map(|(id, text)| (id.clone(), text.clone())),
        )
    }

    /// Convenience single-file cache as `(id, Source)` for callers that prefer it.
    pub fn single_pair(source: &SourceFile) -> (SourceId, Source) {
        (source.name.clone(), Source::from(source.text.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_phase_and_message() {
        let error = CompileError::parse("expected begin", None);
        assert_eq!(error.to_string(), "parse error: expected begin");
    }

    #[test]
    fn json_bundle_is_an_array() {
        let error = CompileError::semantic("first failure")
            .with_related(vec![CompileError::semantic("second failure")]);
        let bundle: serde_json::Value =
            serde_json::from_str(&error.to_json_bundle()).expect("json");
        let arr = bundle.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["message"], "first failure");
        assert_eq!(arr[1]["message"], "second failure");
    }

    #[test]
    fn render_includes_phase_message_and_source_line() {
        let source = SourceFile::anonymous("begin integer x; x := true; end;");
        let error = CompileError::semantic_at("type mismatch", 17..26);
        let rendered = error.render(&source);
        assert!(rendered.contains("semantic error"), "rendered: {rendered}");
        assert!(rendered.contains("type mismatch"), "rendered: {rendered}");
        assert!(
            rendered.contains("x := true") || rendered.contains(":="),
            "expected a source snippet in: {rendered}"
        );
        assert!(
            rendered.contains("E-semantic"),
            "expected diagnostic code in: {rendered}"
        );
    }

    #[test]
    fn render_unspanned_still_shows_message() {
        let source = SourceFile::anonymous("begin end;");
        let error = CompileError::parse("expected begin", None);
        let rendered = error.render(&source);
        assert!(rendered.contains("parse error"), "rendered: {rendered}");
        assert!(rendered.contains("expected begin"), "rendered: {rendered}");
    }

    #[test]
    fn render_includes_notes_helps_and_secondary_labels() {
        let source = SourceFile::anonymous("begin integer x; x := true; end;");
        let error = CompileError::semantic_at("cannot assign boolean to integer", 20..24)
            .with_primary_message("this is boolean")
            .with_label(7..16, "destination declared here")
            .with_note("value assignment requires compatible types")
            .with_help("use an integer expression on the right-hand side");
        let rendered = error.render(&source);
        assert!(rendered.contains("this is boolean"), "rendered: {rendered}");
        assert!(
            rendered.contains("destination declared here"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains("compatible types"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains("integer expression"),
            "rendered: {rendered}"
        );
    }

    #[test]
    fn multi_file_cache_resolves_cross_file_label() {
        let primary = SourceFile {
            name: "main.sim".into(),
            text: "begin x := 1; end;".into(),
        };
        let other = SourceFile {
            name: "decl.sim".into(),
            text: "integer x;".into(),
        };
        let mut cache = SourceCache::from_file(&primary);
        cache.insert(&other);
        let error = CompileError::semantic_at("undefined in this scope", 6..7)
            .with_diagnostic_label(
                DiagnosticLabel::new(0..10, "declared in another file").in_source("decl.sim"),
            );
        let mut buf = Vec::new();
        error
            .write_cached(
                &cache,
                &primary,
                &mut buf,
                &DiagnosticConfig::colorless(),
                false,
            )
            .unwrap();
        let rendered = String::from_utf8(buf).unwrap();
        assert!(
            rendered.contains("main.sim") || rendered.contains("undefined"),
            "{rendered}"
        );
        assert!(
            rendered.contains("declared in another file") || rendered.contains("decl.sim"),
            "rendered: {rendered}"
        );
    }

    #[test]
    fn remap_to_origins_sets_primary_source_and_local_span() {
        let composite = crate::source::CompositeSource::concat([
            SourceFile {
                name: "a.sim".into(),
                text: "begin ".into(),
            },
            SourceFile {
                name: "b.sim".into(),
                text: "bad".into(),
            },
        ]);
        let error = CompileError::lex("bad token", 7..10).with_label(0..5, "started here");
        let remapped = error.remap_to_origins(&composite);
        assert_eq!(remapped.span, Some(0..3));
        assert_eq!(remapped.primary_source.as_deref(), Some("b.sim"));
        assert_eq!(remapped.labels[0].span, 0..5);
        assert_eq!(remapped.labels[0].source.as_deref(), Some("a.sim"));

        let cache = composite.to_cache();
        assert!(cache.contains("a.sim"));
        assert!(cache.contains("b.sim"));
    }

    #[test]
    fn color_choice_never_disables() {
        assert!(!ColorChoice::Never.enabled(true));
        assert!(!ColorChoice::Never.enabled(false));
    }

    #[test]
    fn color_choice_always_enables() {
        assert!(ColorChoice::Always.enabled(false));
    }

    #[test]
    fn ansi_config_emits_escape_codes() {
        let source = SourceFile::anonymous("begin end;");
        let error = CompileError::parse("expected begin", Some(0..5));
        let rendered = error.render_with_config(&source, &DiagnosticConfig::ansi());
        assert!(
            rendered.contains('\u{1b}'),
            "expected ANSI colour in:\n{rendered:?}"
        );
        let plain = error.render_with_config(&source, &DiagnosticConfig::colorless());
        assert!(
            !plain.contains('\u{1b}'),
            "colorless should not paint:\n{plain:?}"
        );
    }

    #[test]
    fn plain_config_uses_ascii() {
        let source = SourceFile::anonymous("begin end;");
        let error = CompileError::parse("expected begin", Some(0..5));
        let rendered = error.render_with_config(&source, &DiagnosticConfig::plain());
        assert!(
            rendered.contains("E-parse") && rendered.contains("expected begin"),
            "{rendered}"
        );
        assert!(
            !rendered.contains('╭') && !rendered.contains('│'),
            "plain config should not use Unicode boxes:\n{rendered}"
        );
    }

    #[test]
    fn render_includes_related_errors() {
        let source = SourceFile::anonymous("begin integer i; boolean b; i := b; b := i; end;");
        let primary = CompileError::semantic_at("cannot assign boolean to integer", 27..33)
            .with_related(vec![CompileError::semantic_at(
                "cannot assign integer to boolean",
                35..41,
            )]);
        let rendered = primary.render(&source);
        assert!(
            rendered.contains("cannot assign boolean to integer"),
            "{rendered}"
        );
        assert!(
            rendered.contains("cannot assign integer to boolean"),
            "{rendered}"
        );
    }

    #[test]
    fn compile_errors_render_all_and_bundle() {
        let source = SourceFile::anonymous("begin end;");
        let errors = CompileErrors::new(vec![
            CompileError::semantic("first failure"),
            CompileError::semantic("second failure"),
        ]);
        let all = errors.render_all(&source);
        assert!(all.contains("first failure"), "{all}");
        assert!(all.contains("second failure"), "{all}");
        assert!(errors.to_string().contains("1 more error"));

        let bundled = errors.into_bundled();
        assert_eq!(bundled.message, "first failure");
        assert_eq!(bundled.related.len(), 1);
        assert!(bundled.notes.iter().any(|n| n.contains("1 more error")));
    }
}
