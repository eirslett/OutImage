//! Foreign procedure ABI.
//!
//! A [`ForeignAbi`] on a MIR [`super::Function`] means the body is a boundary
//! thunk: Simula calls it like any other procedure, and the backend binds the
//! identification to a host import.

use crate::ast::{FormalParameter, ParamMode, ProcedureDeclaration};
use crate::error::{CompileError, Span};
use crate::types::Type;

/// Recognised `external kind` identifiers (Simula is case-insensitive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignKind {
    C,
    Js,
    Host,
}

impl ForeignKind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind.to_ascii_lowercase().as_str() {
            "c" => Some(Self::C),
            "js" => Some(Self::Js),
            "host" => Some(Self::Host),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::C => "C",
            Self::Js => "JS",
            Self::Host => "Host",
        }
    }
}

impl std::fmt::Display for ForeignKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Types that may cross a v1 foreign boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignType {
    I64,
    F64,
    Bool,
    Char,
    /// Copy-in / copy-out Latin-1 text. Phase 3; backends may still reject it.
    TextCopy,
    /// Opaque GC-managed object (`ref (C)`). Never a stable address.
    ObjectHandle,
}

impl ForeignType {
    pub fn from_simula(ty: &Type) -> Result<Self, String> {
        match ty {
            Type::Integer { .. } => Ok(Self::I64),
            Type::Real { .. } => Ok(Self::F64),
            Type::Boolean => Ok(Self::Bool),
            Type::Character => Ok(Self::Char),
            Type::Text => Ok(Self::TextCopy),
            Type::ObjectRef(_) => Ok(Self::ObjectHandle),
            Type::Array { element, .. } => Err(format!(
                "{element} array parameters cannot cross a foreign procedure boundary"
            )),
        }
    }

    /// Wasm / C valtype for this foreign type (boolean and character are i32).
    pub fn is_i32_abi(self) -> bool {
        matches!(self, Self::Bool | Self::Char)
    }

    pub fn is_text(self) -> bool {
        matches!(self, Self::TextCopy)
    }

    pub fn is_handle(self) -> bool {
        matches!(self, Self::ObjectHandle)
    }
}

/// Calling convention recorded on a foreign MIR stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignConv {
    /// Scalars (and later text copies) at the boundary.
    Scalar,
}

/// Backend-independent import description for one foreign procedure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignAbi {
    pub kind: ForeignKind,
    /// Linker / import-object name after applying identification defaults.
    pub ident: String,
    pub conv: ForeignConv,
    pub params: Vec<ForeignType>,
    pub result: Option<ForeignType>,
}

impl ForeignAbi {
    pub fn from_spec(
        kind: ForeignKind,
        identification: Option<&str>,
        spec: &ProcedureDeclaration,
        span: Span,
    ) -> Result<Self, CompileError> {
        let ident = match identification {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => spec.name.to_ascii_lowercase(),
        };
        let mut params = Vec::new();
        for param in &spec.parameters {
            params.push(foreign_param_type(param, &span)?);
        }
        let result = match &spec.result_type {
            None => None,
            Some(ty) => Some(foreign_result_type(ty, &span)?),
        };
        Ok(Self {
            kind,
            ident,
            conv: ForeignConv::Scalar,
            params,
            result,
        })
    }

    /// `(module, name)` for a core-wasm import.
    pub fn wasm_import(&self) -> (String, String) {
        match self.kind {
            ForeignKind::Host => ("host".to_string(), self.ident.clone()),
            ForeignKind::Js => parse_js_id(&self.ident),
            ForeignKind::C => ("c".to_string(), c_symbol(&self.ident).to_string()),
        }
    }

    /// Native object-file symbol for a `Linkage::Import`.
    pub fn native_symbol(&self) -> Result<String, CompileError> {
        match self.kind {
            ForeignKind::Host => {
                if !is_c_ident(&self.ident) {
                    return Err(CompileError::codegen(format!(
                        "Host identification '{}' is not a valid native symbol",
                        self.ident
                    )));
                }
                Ok(format!("simrt_host_{}", self.ident))
            }
            ForeignKind::C => Ok(c_symbol(&self.ident).to_string()),
            ForeignKind::Js => Err(CompileError::codegen(
                "JS externals require a wasm target (wasm-browser or wasm-node)",
            )),
        }
    }

    /// Linker `-l` name from a C identification `lib:symbol`, if present.
    pub fn native_link_lib(&self) -> Option<&str> {
        if self.kind != ForeignKind::C {
            return None;
        }
        c_lib(&self.ident)
    }

    pub fn rejects_text(&self) -> bool {
        self.params.iter().any(|ty| ty.is_text()) || self.result.is_some_and(ForeignType::is_text)
    }
}

fn foreign_param_type(param: &FormalParameter, span: &Span) -> Result<ForeignType, CompileError> {
    if param.is_procedure {
        return Err(crate::diagnostics::foreign_boundary(
            "formal procedure parameters cannot cross a foreign procedure boundary",
            span.clone(),
        ));
    }
    if param.is_label {
        return Err(crate::diagnostics::foreign_boundary(
            "label parameters cannot cross a foreign procedure boundary",
            span.clone(),
        ));
    }
    if param.is_switch {
        return Err(crate::diagnostics::foreign_boundary(
            "switch parameters cannot cross a foreign procedure boundary",
            span.clone(),
        ));
    }
    if param.mode == ParamMode::Name {
        return Err(crate::diagnostics::foreign_boundary(
            "name parameters cannot cross a foreign procedure boundary",
            span.clone(),
        ));
    }
    if matches!(param.ty, Type::Text) && param.mode != ParamMode::Value {
        return Err(crate::diagnostics::foreign_boundary(
            "text parameters at a foreign boundary must be transmitted by value",
            span.clone(),
        ));
    }
    ForeignType::from_simula(&param.ty)
        .map_err(|message| crate::diagnostics::foreign_boundary(message, span.clone()))
}

fn foreign_result_type(ty: &Type, span: &Span) -> Result<ForeignType, CompileError> {
    ForeignType::from_simula(ty)
        .map_err(|message| crate::diagnostics::foreign_boundary(message, span.clone()))
}

fn parse_js_id(ident: &str) -> (String, String) {
    match ident.split_once('.') {
        Some((module, name)) if !module.is_empty() && !name.is_empty() => {
            (module.to_string(), name.to_string())
        }
        _ => ("js".to_string(), ident.to_string()),
    }
}

fn c_symbol(ident: &str) -> &str {
    match ident.rsplit_once(':') {
        Some((_, symbol)) if !symbol.is_empty() => symbol,
        _ => ident,
    }
}

fn c_lib(ident: &str) -> Option<&str> {
    match ident.split_once(':') {
        Some((lib, symbol)) if !lib.is_empty() && !symbol.is_empty() => {
            Some(lib.strip_prefix("lib").unwrap_or(lib))
        }
        _ => None,
    }
}

/// Native export wrapper for a public Simula procedure (`sim_add`).
pub fn native_export_symbol(simula_name: &str) -> String {
    format!("sim_{}", simula_name.to_ascii_lowercase())
}

/// `export:name` identification on a defining procedure. Returns the published
/// name, or `None` if this is not an export identification.
pub fn parse_export_identification(identification: &str) -> Option<&str> {
    let (prefix, rest) = identification.trim().split_once(':')?;
    if !prefix.eq_ignore_ascii_case("export") {
        return None;
    }
    let name = rest.trim();
    if name.is_empty() || !is_c_ident(name) {
        return None;
    }
    Some(name)
}

fn is_c_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_parse_is_case_insensitive() {
        assert_eq!(ForeignKind::parse("C"), Some(ForeignKind::C));
        assert_eq!(ForeignKind::parse("host"), Some(ForeignKind::Host));
        assert_eq!(ForeignKind::parse("JS"), Some(ForeignKind::Js));
        assert_eq!(ForeignKind::parse("Fortran"), None);
    }

    #[test]
    fn js_id_splits_on_first_dot() {
        assert_eq!(
            parse_js_id("console.log"),
            ("console".to_string(), "log".to_string())
        );
        assert_eq!(parse_js_id("log"), ("js".to_string(), "log".to_string()));
    }

    #[test]
    fn c_symbol_strips_lib_prefix() {
        assert_eq!(c_symbol("libm:sqrt"), "sqrt");
        assert_eq!(c_symbol("add"), "add");
    }

    #[test]
    fn c_lib_strips_lib_prefix() {
        assert_eq!(c_lib("libm:sqrt"), Some("m"));
        assert_eq!(c_lib("foo:bar"), Some("foo"));
        assert_eq!(c_lib("add"), None);
    }

    #[test]
    fn parse_export_identification_is_case_insensitive() {
        assert_eq!(parse_export_identification("export:step"), Some("step"));
        assert_eq!(parse_export_identification("EXPORT:tick"), Some("tick"));
        assert_eq!(parse_export_identification("utils"), None);
        assert_eq!(parse_export_identification("export:"), None);
        assert_eq!(parse_export_identification("export:1bad"), None);
    }
}
