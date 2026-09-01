//! Structured compiler diagnostics: catalog ids, English reports, suggestions.
//!
//! Call sites emit a [`DiagId`] plus typed parameters through the constructors
//! in [`report`]. Rendering (ariadne, JSON, LSP, `explain`) all share that
//! report. Legacy [`crate::error::CompileError::semantic`] strings remain valid
//! during the migration.

mod catalog;
mod report;
mod suggest;
mod typeset;

pub use catalog::{
    CatalogEntry, catalog_index_markdown, explain, explain_doc_url, lookup, lookup_group,
};
pub use report::{
    Applicability, ExpectRole, IfBranch, Suggestion, arity_mismatch, array_bound_name,
    array_extent_overflow, array_subscript, array_subscript_count, assign_to_constant,
    attribute_not_visible, constant_initializer, detach_needs_object, directive_not_at_column_zero,
    division_by_zero, duplicate_declaration, duplicate_formal, duplicate_virtual,
    empty_array_bounds, empty_switch, exponentiation_undefined, external_body_not_empty,
    foreign_boundary, formal_array_arity, formal_not_visible, formal_redeclared, hidden_attribute,
    hidden_requires_protected, ice, if_branch_should_be, illegal_param_mode, illegal_this,
    incompatible_branches, incomplete_type_prefix, invalid_iso_code, invalid_number, linker_failed,
    linker_not_found, missing_end, missing_external_spec, missing_token_separator,
    non_simula_formal_proc, none_dereference, not_lowered, not_prefix_class, plus_on_text,
    prefix_cycle, prefix_not_local, procedure_name_as_formal, protected_attribute,
    ref_assign_to_value, simulation_not_active, statement_as_expression, toolchain, type_mismatch,
    type_mismatch_assign, undefined_class, undefined_label, undefined_label_runtime,
    undefined_switch, unexpected_character, unexpected_eof, unexpected_token, unknown_attribute,
    unknown_external_kind, unknown_name, unknown_procedure, unterminated_string, unused_binding,
    value_assign_to_ref, virtual_mismatch, wrong_assign_operator,
};
pub use suggest::{rank, suggest_one};
pub use typeset::{expected_list_english, ref_prefix_note, token_english, type_english};

use crate::error::{CompileError, Phase};

/// Stable diagnostic identity. Codes are never reused once published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagId {
    UnexpectedCharacter,
    UnterminatedString,
    MissingTokenSeparator,
    DirectiveNotAtColumnZero,
    InvalidNumber,
    InvalidIsoCode,
    UnexpectedToken,
    UnexpectedEof,
    MissingEnd,
    WrongAssignOperator,
    IncompleteTypePrefix,
    TypeMismatchAssign,
    ValueAssignToRef,
    RefAssignToValue,
    TypeMismatch,
    ArityMismatch,
    IncompatibleBranches,
    UnknownName,
    UnknownProcedure,
    UnknownAttribute,
    UndefinedLabel,
    UndefinedSwitch,
    DuplicateDeclaration,
    UnusedBinding,
    ArrayExtentOverflow,
    NoneDereference,
    UndefinedLabelRuntime,
    ArraySubscript,
    DivisionByZero,
    ExponentiationUndefined,
    ProtectedAttribute,
    HiddenAttribute,
    EmptyArrayBounds,
    ArrayBoundName,
    EmptySwitch,
    StatementAsExpression,
    AssignToConstant,
    ConstantInitializer,
    PrefixCycle,
    UndefinedClass,
    PrefixNotLocal,
    VirtualMismatch,
    IllegalThis,
    NotPrefixClass,
    HiddenRequiresProtected,
    DuplicateVirtual,
    DetachNeedsObject,
    AttributeNotVisible,
    IllegalParamMode,
    DuplicateFormal,
    ProcedureNameAsFormal,
    FormalRedeclared,
    FormalArrayArity,
    FormalNotVisible,
    NonSimulaFormalProc,
    UnknownExternalKind,
    MissingExternalSpec,
    ExternalBodyNotEmpty,
    ForeignBoundary,
    SimulationNotActive,
    NotLowered,
    LinkerFailed,
    LinkerNotFound,
    Toolchain,
    InternalError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Ice,
}

impl DiagId {
    pub const ALL: &'static [DiagId] = &[
        Self::UnexpectedCharacter,
        Self::UnterminatedString,
        Self::MissingTokenSeparator,
        Self::DirectiveNotAtColumnZero,
        Self::InvalidNumber,
        Self::InvalidIsoCode,
        Self::UnexpectedToken,
        Self::UnexpectedEof,
        Self::MissingEnd,
        Self::WrongAssignOperator,
        Self::IncompleteTypePrefix,
        Self::TypeMismatchAssign,
        Self::ValueAssignToRef,
        Self::RefAssignToValue,
        Self::TypeMismatch,
        Self::ArityMismatch,
        Self::IncompatibleBranches,
        Self::UnknownName,
        Self::UnknownProcedure,
        Self::UnknownAttribute,
        Self::UndefinedLabel,
        Self::UndefinedSwitch,
        Self::DuplicateDeclaration,
        Self::UnusedBinding,
        Self::ArrayExtentOverflow,
        Self::NoneDereference,
        Self::UndefinedLabelRuntime,
        Self::ArraySubscript,
        Self::DivisionByZero,
        Self::ExponentiationUndefined,
        Self::EmptyArrayBounds,
        Self::ArrayBoundName,
        Self::EmptySwitch,
        Self::StatementAsExpression,
        Self::AssignToConstant,
        Self::ConstantInitializer,
        Self::ProtectedAttribute,
        Self::HiddenAttribute,
        Self::PrefixCycle,
        Self::UndefinedClass,
        Self::PrefixNotLocal,
        Self::VirtualMismatch,
        Self::IllegalThis,
        Self::NotPrefixClass,
        Self::HiddenRequiresProtected,
        Self::DuplicateVirtual,
        Self::DetachNeedsObject,
        Self::AttributeNotVisible,
        Self::IllegalParamMode,
        Self::DuplicateFormal,
        Self::ProcedureNameAsFormal,
        Self::FormalRedeclared,
        Self::FormalArrayArity,
        Self::FormalNotVisible,
        Self::NonSimulaFormalProc,
        Self::UnknownExternalKind,
        Self::MissingExternalSpec,
        Self::ExternalBodyNotEmpty,
        Self::ForeignBoundary,
        Self::SimulationNotActive,
        Self::NotLowered,
        Self::LinkerFailed,
        Self::LinkerNotFound,
        Self::Toolchain,
        Self::InternalError,
    ];

    pub fn code(self) -> &'static str {
        self.entry().code
    }

    pub fn title(self) -> &'static str {
        self.entry().title
    }

    pub fn phase(self) -> Phase {
        self.entry().phase
    }

    pub fn severity(self) -> Severity {
        self.entry().severity
    }

    pub fn group(self) -> &'static str {
        self.phase().diagnostic_code()
    }

    pub fn entry(self) -> &'static CatalogEntry {
        catalog::entry(self)
    }

    pub fn parse_code(code: &str) -> Option<Self> {
        let normalized = code.trim().to_ascii_uppercase();
        Self::ALL.iter().copied().find(|id| id.code() == normalized)
    }
}

/// Finished report ready to store on [`CompileError`].
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub id: DiagId,
    pub message: String,
    pub span: Option<crate::error::Span>,
    pub primary_message: Option<String>,
    pub labels: Vec<crate::error::DiagnosticLabel>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
    pub suggestions: Vec<Suggestion>,
    pub params: Vec<(String, String)>,
}

impl Diagnostic {
    pub fn new(id: DiagId, message: impl Into<String>) -> Self {
        Self {
            id,
            message: message.into(),
            span: None,
            primary_message: None,
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            suggestions: Vec::new(),
            params: Vec::new(),
        }
    }

    pub fn at(mut self, span: crate::error::Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn primary(mut self, message: impl Into<String>) -> Self {
        self.primary_message = Some(message.into());
        self
    }

    pub fn label(mut self, span: crate::error::Span, message: impl Into<String>) -> Self {
        self.labels
            .push(crate::error::DiagnosticLabel::new(span, message));
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    pub fn suggest(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }

    pub fn param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.push((key.into(), value.into()));
        self
    }

    pub fn into_error(self) -> CompileError {
        CompileError::from_diagnostic(self)
    }
}
