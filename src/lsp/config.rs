//! Runtime LSP configuration (init options + `workspace/didChangeConfiguration`).

use serde_json::Value;

use super::workspace::DEFAULT_MAX_DOCUMENT_BYTES;

/// When the server should re-run diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckOn {
    /// Analyze on open only.
    Open,
    /// Analyze on open and (debounced) change. Default.
    #[default]
    Change,
    /// Analyze on open and save.
    Save,
}

impl CheckOn {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "change" => Some(Self::Change),
            "save" => Some(Self::Save),
            _ => None,
        }
    }
}

/// Mutable server options that clients may push after initialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspConfig {
    pub check_on: CheckOn,
    pub debounce_ms: u64,
    pub allow_square_bracket_subscripts: bool,
    /// Treat `--` as a line comment (default). When false, `--` is two minuses.
    pub allow_double_dash_comments: bool,
    /// Run MIR lowering after a clean semantic pass and surface codegen errors.
    pub enable_mir_check: bool,
    /// Refuse to analyze buffers larger than this many UTF-8 bytes.
    pub max_document_bytes: usize,
    /// Emit unused-local warnings (`W-unused`) after a clean semantic pass.
    pub enable_unused_lints: bool,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            check_on: CheckOn::Change,
            debounce_ms: 200,
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
            enable_mir_check: false,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            enable_unused_lints: true,
        }
    }
}

impl LspConfig {
    /// Apply fields from `initializationOptions` / settings JSON object.
    pub fn apply_json(&mut self, value: &Value) {
        let Some(obj) = value.as_object() else {
            return;
        };
        // Nested `simula` / `simula.lsp` bags from workspace/didChangeConfiguration.
        if let Some(nested) = obj.get("simula").or_else(|| obj.get("simula.lsp")) {
            self.apply_json(nested);
        }
        if let Some(v) = obj
            .get("allowSquareBracketSubscripts")
            .and_then(Value::as_bool)
        {
            self.allow_square_bracket_subscripts = v;
        }
        if let Some(v) = obj.get("allowDoubleDashComments").and_then(Value::as_bool) {
            self.allow_double_dash_comments = v;
        }
        if let Some(v) = obj
            .get("checkOn")
            .and_then(Value::as_str)
            .and_then(CheckOn::parse)
        {
            self.check_on = v;
        }
        if let Some(v) = obj.get("debounceMs").and_then(Value::as_u64) {
            self.debounce_ms = v.clamp(0, 10_000);
        }
        if let Some(v) = obj.get("enableMirCheck").and_then(Value::as_bool) {
            self.enable_mir_check = v;
        }
        if let Some(v) = obj.get("enableUnusedLints").and_then(Value::as_bool) {
            self.enable_unused_lints = v;
        }
        if let Some(v) = obj.get("maxDocumentBytes").and_then(Value::as_u64) {
            self.max_document_bytes = (v as usize).clamp(16 * 1024, 32 * 1024 * 1024);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn applies_nested_simula_settings() {
        let mut cfg = LspConfig::default();
        cfg.apply_json(&json!({
            "simula": {
                "checkOn": "save",
                "debounceMs": 500,
                "allowSquareBracketSubscripts": false,
                "allowDoubleDashComments": false,
                "enableMirCheck": true,
                "enableUnusedLints": false,
                "maxDocumentBytes": 100_000
            }
        }));
        assert_eq!(cfg.check_on, CheckOn::Save);
        assert_eq!(cfg.debounce_ms, 500);
        assert!(!cfg.allow_square_bracket_subscripts);
        assert!(!cfg.allow_double_dash_comments);
        assert!(cfg.enable_mir_check);
        assert!(!cfg.enable_unused_lints);
        assert_eq!(cfg.max_document_bytes, 100_000);
    }

    #[test]
    fn clamps_debounce() {
        let mut cfg = LspConfig::default();
        cfg.apply_json(&json!({ "debounceMs": 99_999 }));
        assert_eq!(cfg.debounce_ms, 10_000);
    }
}
