//! In-memory text document store keyed by URI.

use std::collections::HashMap;

use tower_lsp_server::ls_types::{Range, TextDocumentContentChangeEvent, Uri};

use super::position::{Encoding, PositionIndex};

/// An open text document tracked by the language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub uri: String,
    pub language_id: String,
    pub version: i32,
    pub text: String,
}

impl Document {
    pub fn new(
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i32,
        text: impl Into<String>,
    ) -> Self {
        Self {
            uri: uri.into(),
            language_id: language_id.into(),
            version,
            text: text.into(),
        }
    }

    /// Applies a full-document or incremental content change.
    pub fn apply_change(
        &mut self,
        change: &TextDocumentContentChangeEvent,
        encoding: Encoding,
    ) -> Result<(), String> {
        match &change.range {
            None => {
                self.text = change.text.clone();
                Ok(())
            }
            Some(range) => apply_range_edit(&mut self.text, *range, &change.text, encoding),
        }
    }
}

/// Open documents keyed by URI string (`Uri::as_str()`).
#[derive(Debug, Default, Clone)]
pub struct DocumentStore {
    docs: HashMap<String, Document>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self, uri: &Uri, language_id: String, version: i32, text: String) {
        let key = uri.as_str().to_owned();
        self.docs
            .insert(key.clone(), Document::new(key, language_id, version, text));
    }

    pub fn close(&mut self, uri: &Uri) -> Option<Document> {
        self.docs.remove(uri.as_str())
    }

    pub fn get(&self, uri: &Uri) -> Option<&Document> {
        self.docs.get(uri.as_str())
    }

    pub fn get_mut(&mut self, uri: &Uri) -> Option<&mut Document> {
        self.docs.get_mut(uri.as_str())
    }

    /// Applies content changes and updates the document version.
    pub fn apply_changes(
        &mut self,
        uri: &Uri,
        version: i32,
        changes: &[TextDocumentContentChangeEvent],
        encoding: Encoding,
    ) -> Result<&Document, String> {
        let doc = self
            .docs
            .get_mut(uri.as_str())
            .ok_or_else(|| format!("document not open: {}", uri.as_str()))?;
        for change in changes {
            doc.apply_change(change, encoding)?;
        }
        doc.version = version;
        Ok(doc)
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }
}

fn apply_range_edit(
    text: &mut String,
    range: Range,
    replacement: &str,
    encoding: Encoding,
) -> Result<(), String> {
    let index = PositionIndex::new(text);
    let start = index.position_to_offset(text, range.start, encoding);
    let end = index.position_to_offset(text, range.end, encoding);
    if start > end || end > text.len() {
        return Err(format!(
            "invalid edit range {}..{} for document of length {}",
            start,
            end,
            text.len()
        ));
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return Err("edit range is not on a UTF-8 character boundary".into());
    }
    text.replace_range(start..end, replacement);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn uri(s: &str) -> Uri {
        Uri::from_str(s).expect("uri")
    }

    #[test]
    fn open_get_close() {
        let mut store = DocumentStore::new();
        let u = uri("file:///tmp/a.sim");
        store.open(&u, "simula".into(), 1, "begin end".into());
        assert_eq!(store.get(&u).unwrap().text, "begin end");
        assert_eq!(store.get(&u).unwrap().version, 1);
        store.close(&u);
        assert!(store.get(&u).is_none());
    }

    #[test]
    fn full_document_change() {
        let mut store = DocumentStore::new();
        let u = uri("file:///tmp/a.sim");
        store.open(&u, "simula".into(), 1, "old".into());
        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "new text".into(),
        }];
        let doc = store
            .apply_changes(&u, 2, &changes, Encoding::Utf16)
            .unwrap();
        assert_eq!(doc.text, "new text");
        assert_eq!(doc.version, 2);
    }

    #[test]
    fn incremental_range_edit() {
        let mut store = DocumentStore::new();
        let u = uri("file:///tmp/a.sim");
        store.open(&u, "simula".into(), 1, "begin x end".into());
        let changes = vec![TextDocumentContentChangeEvent {
            range: Some(Range::new(
                tower_lsp_server::ls_types::Position::new(0, 6),
                tower_lsp_server::ls_types::Position::new(0, 7),
            )),
            range_length: None,
            text: "y".into(),
        }];
        let doc = store
            .apply_changes(&u, 2, &changes, Encoding::Utf16)
            .unwrap();
        assert_eq!(doc.text, "begin y end");
    }
}
