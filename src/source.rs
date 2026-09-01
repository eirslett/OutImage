//! Source files fed to the compiler front-end.
//!
//! [`CompositeSource`] concatenates multiple files into one virtual buffer for
//! compilation while retaining an origin table so diagnostics can be remapped
//! back to the original file and local span.

use crate::error::{SourceCache, SourceId, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
}

impl SourceFile {
    pub fn anonymous(text: impl Into<String>) -> Self {
        Self {
            name: "<input>".into(),
            text: text.into(),
        }
    }

    pub fn from_path(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Ok(Self {
            name: path.display().to_string(),
            text,
        })
    }
}

/// Converts a byte `offset` into `source` to a `(line, column)` pair, both
/// 1-based, counting *characters* (not bytes) since the start of the line.
///
/// `offset` is clamped to `source.len()` if it runs past the end of the
/// text. A `\r\n` pair is treated the same as a lone `\n`.
pub fn span_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(source.len());
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut line = 1usize;
    let mut column = 1usize;
    for ch in source[..offset].chars() {
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Byte range of one origin file inside a [`CompositeSource`] buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OriginRange {
    name: SourceId,
    /// Inclusive-exclusive byte range of this file's text in the combined buffer
    /// (joining `\n` separators are not part of any origin range).
    range: Span,
}

/// Multiple [`SourceFile`]s joined into one virtual text with an origin table.
///
/// Spans produced while compiling [`Self::as_source_file`] are composite
/// offsets; use [`Self::localize`] / [`CompileError::remap_to_origins`] to map
/// them back to `(file name, local span)` for multi-file ariadne reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeSource {
    combined: SourceFile,
    origins: Vec<SourceFile>,
    ranges: Vec<OriginRange>,
}

impl CompositeSource {
    /// Concatenates `files` in order, joining consecutive texts with `\n`.
    ///
    /// An empty iterator yields an anonymous empty source named `"<composite>"`.
    pub fn concat(files: impl IntoIterator<Item = SourceFile>) -> Self {
        let origins: Vec<SourceFile> = files.into_iter().collect();
        if origins.is_empty() {
            return Self {
                combined: SourceFile {
                    name: "<composite>".into(),
                    text: String::new(),
                },
                origins: Vec::new(),
                ranges: Vec::new(),
            };
        }

        let mut text = String::new();
        let mut ranges = Vec::with_capacity(origins.len());
        for (i, file) in origins.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let start = text.len();
            text.push_str(&file.text);
            let end = text.len();
            ranges.push(OriginRange {
                name: file.name.clone(),
                range: start..end,
            });
        }

        let combined_name = origins[0].name.clone();
        Self {
            combined: SourceFile {
                name: combined_name,
                text,
            },
            origins,
            ranges,
        }
    }

    /// Same as [`Self::concat`].
    pub fn from_files(files: Vec<SourceFile>) -> Self {
        Self::concat(files)
    }

    /// The virtual combined source used for lex/parse/compile.
    pub fn as_source_file(&self) -> &SourceFile {
        &self.combined
    }

    /// Alias for [`Self::as_source_file`] (the virtual combined buffer).
    pub fn primary(&self) -> &SourceFile {
        self.as_source_file()
    }

    /// Origin files in concatenation order.
    pub fn origins(&self) -> &[SourceFile] {
        &self.origins
    }

    /// Looks up an origin by [`SourceId`] (file name).
    pub fn origin(&self, id: &str) -> Option<&SourceFile> {
        self.origins.iter().find(|f| f.name == id)
    }

    /// Builds a [`SourceCache`] containing every origin file (not the virtual
    /// combined buffer).
    pub fn to_cache(&self) -> SourceCache {
        let mut cache = SourceCache::new();
        for file in &self.origins {
            cache.insert(file);
        }
        if self.origins.is_empty() {
            cache.insert(&self.combined);
        }
        cache
    }

    /// Alias for [`Self::to_cache`].
    pub fn cache(&self) -> SourceCache {
        self.to_cache()
    }

    /// Maps a composite byte span to `(origin id, local span)`.
    ///
    /// If the span starts on a joining newline, it is attributed to the
    /// preceding file. Spans that extend past an origin are clamped to that
    /// file. Falls back to the first origin (or the combined file) when the
    /// offset cannot be resolved.
    pub fn localize(&self, span: Span) -> (SourceId, Span) {
        self.resolve(span.clone()).unwrap_or_else(|| {
            if let Some(first) = self.origins.first() {
                let len = first.text.len();
                let start = span.start.min(len);
                let end = span.end.min(len).max(start);
                (first.name.clone(), start..end)
            } else {
                (self.combined.name.clone(), span)
            }
        })
    }

    /// Like [`Self::localize`], but returns `None` when `span.start` does not
    /// fall in any origin (including empty composites).
    pub fn resolve(&self, span: Span) -> Option<(SourceId, Span)> {
        let index = self.file_index_for_offset(span.start)?;
        let origin = &self.origins[index];
        let range = &self.ranges[index].range;
        let local_start = span.start.saturating_sub(range.start);
        let local_end = span
            .end
            .saturating_sub(range.start)
            .min(origin.text.len())
            .max(local_start);
        Some((origin.name.clone(), local_start..local_end))
    }

    fn file_index_for_offset(&self, offset: usize) -> Option<usize> {
        for (i, fr) in self.ranges.iter().enumerate() {
            if offset >= fr.range.start && offset < fr.range.end {
                return Some(i);
            }
        }
        // Empty span at file EOF, or a joining `\n` right after a file.
        for (i, fr) in self.ranges.iter().enumerate() {
            if offset == fr.range.end {
                return Some(i);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CompileError, DiagnosticConfig};

    fn file(name: &str, text: &str) -> SourceFile {
        SourceFile {
            name: name.into(),
            text: text.into(),
        }
    }

    #[test]
    fn concat_joins_with_newline_and_records_ranges() {
        let composite = CompositeSource::concat([file("a.sim", "begin"), file("b.sim", "end;")]);
        assert_eq!(composite.as_source_file().text, "begin\nend;");
        assert_eq!(composite.origins().len(), 2);

        let (id, local) = composite.localize(0..5);
        assert_eq!(id, "a.sim");
        assert_eq!(local, 0..5);

        let (id, local) = composite.localize(6..10);
        assert_eq!(id, "b.sim");
        assert_eq!(local, 0..4);
    }

    #[test]
    fn resolve_maps_second_file_span() {
        let composite =
            CompositeSource::from_files(vec![file("first.sim", "aaa"), file("second.sim", "bbb")]);
        // "aaa\nbbb" — second file starts at offset 4
        let resolved = composite.resolve(4..7).expect("in second file");
        assert_eq!(resolved.0, "second.sim");
        assert_eq!(resolved.1, 0..3);
    }

    #[test]
    fn to_cache_contains_both_origin_ids() {
        let composite = CompositeSource::concat([file("a.sim", "begin"), file("b.sim", "end;")]);
        let cache = composite.to_cache();
        assert!(cache.contains("a.sim"));
        assert!(cache.contains("b.sim"));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("b.sim"), Some("end;"));
    }

    #[test]
    fn remap_error_span_in_second_file_to_origin() {
        let composite = CompositeSource::concat([file("a.sim", "begin "), file("b.sim", "oops")]);
        // Combined: "begin \noops" — "oops" is at 7..11
        let error = CompileError::parse("unexpected", Some(7..11));
        let remapped = error.remap_to_origins(&composite);
        assert_eq!(remapped.span, Some(0..4));
        assert_eq!(remapped.primary_source.as_deref(), Some("b.sim"));

        let mut buf = Vec::new();
        remapped
            .write_cached(
                &composite.to_cache(),
                composite.as_source_file(),
                &mut buf,
                &DiagnosticConfig::colorless(),
                false,
            )
            .unwrap();
        let rendered = String::from_utf8(buf).unwrap();
        assert!(
            rendered.contains("b.sim"),
            "expected second file name in report: {rendered}"
        );
        assert!(rendered.contains("oops"), "rendered: {rendered}");
    }

    #[test]
    fn single_file_composite_is_identity() {
        let composite = CompositeSource::concat([file("only.sim", "begin end;")]);
        assert_eq!(composite.as_source_file().text, "begin end;");
        assert_eq!(composite.localize(6..9), ("only.sim".into(), 6..9));
        assert_eq!(composite.to_cache().len(), 1);
    }
}
