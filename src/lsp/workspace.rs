//! Workspace folders, path sandboxing, and on-disk `.sim` indexing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tower_lsp_server::ls_types::Uri;

use super::analysis::{AnalysisOptions, AnalysisSnapshot, analyze_document};

/// Default maximum document size analyzed by the language server (2 MiB).
pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

/// Soft cap on recursively discovered `.sim` files per workspace root.
const MAX_DISCOVERED_FILES: usize = 2_000;
/// Soft recursion depth when walking workspace folders.
const MAX_WALK_DEPTH: usize = 12;

/// A `.sim` file discovered (or opened from disk) under a workspace folder.
#[derive(Debug, Clone)]
pub struct IndexedFile {
    pub uri: String,
    pub path: PathBuf,
    pub snapshot: AnalysisSnapshot,
}

/// Multi-root workspace state for the language server.
#[derive(Debug, Default, Clone)]
pub struct Workspace {
    /// Absolute folder paths (from `initialize` / `didChangeWorkspaceFolders`).
    pub folders: Vec<PathBuf>,
    /// Closed-file index keyed by URI string.
    pub disk: HashMap<String, IndexedFile>,
}

impl Workspace {
    pub fn set_folders(&mut self, folders: Vec<PathBuf>) {
        self.folders = folders
            .into_iter()
            .filter_map(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
            .collect();
        let folders = self.folders.clone();
        self.disk.retain(|_, file| {
            folders.iter().any(|root| {
                file.path.starts_with(root)
                    || std::fs::canonicalize(&file.path)
                        .map(|c| c.starts_with(root))
                        .unwrap_or(false)
            })
        });
    }

    pub fn add_folder(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if !self.folders.iter().any(|f| f == &path) {
            self.folders.push(path);
        }
    }

    pub fn remove_folder(&mut self, path: &Path) {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.folders.retain(|f| f != &canon && f != path);
        let folders = self.folders.clone();
        self.disk.retain(|_, file| {
            folders.iter().any(|root| {
                file.path.starts_with(root)
                    || std::fs::canonicalize(&file.path)
                        .map(|c| c.starts_with(root))
                        .unwrap_or(false)
            })
        });
    }

    /// True when `path` is under a registered workspace folder (or no folders
    /// are configured — then only absolute paths that look like file URIs we
    /// already track are allowed via explicit open).
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        if self.folders.is_empty() {
            return false;
        }
        let Ok(canon) = std::fs::canonicalize(path) else {
            // Allow non-existent paths that still nest under a folder prefix.
            return self.folders.iter().any(|root| path.starts_with(root));
        };
        self.folders.iter().any(|root| canon.starts_with(root))
    }

    /// Re-scan workspace folders for `.sim` files and analyze them.
    pub fn reindex_all(&mut self, options: &AnalysisOptions, max_bytes: usize) {
        self.disk.clear();
        let mut paths = Vec::new();
        for root in &self.folders {
            discover_sim_files(root, 0, &mut paths);
            if paths.len() >= MAX_DISCOVERED_FILES {
                break;
            }
        }
        paths.truncate(MAX_DISCOVERED_FILES);
        for path in paths {
            let _ = self.index_path(&path, options, max_bytes);
        }
    }

    /// Analyze a single on-disk path if sandbox allows it.
    pub fn index_path(
        &mut self,
        path: &Path,
        options: &AnalysisOptions,
        max_bytes: usize,
    ) -> Result<&IndexedFile, String> {
        if !self.is_path_allowed(path) {
            return Err(format!(
                "refusing to read path outside workspace folders: {}",
                path.display()
            ));
        }
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if meta.len() as usize > max_bytes {
            return Err(format!(
                "file exceeds maxDocumentBytes ({max_bytes}): {}",
                path.display()
            ));
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if text.len() > max_bytes {
            return Err(format!(
                "file exceeds maxDocumentBytes ({max_bytes}): {}",
                path.display()
            ));
        }
        let uri = path_to_uri(path)?;
        let snapshot = analyze_document(&text, options);
        let key = uri.clone();
        self.disk.insert(
            key.clone(),
            IndexedFile {
                uri,
                path: path.to_path_buf(),
                snapshot,
            },
        );
        Ok(self.disk.get(&key).expect("just inserted"))
    }

    /// Drop a disk entry (e.g. after the editor opens it, or on delete).
    pub fn forget_uri(&mut self, uri: &str) {
        self.disk.remove(uri);
    }

    /// Resolve a file URI to a sandboxed absolute path, if allowed.
    pub fn resolve_uri_path(&self, uri: &Uri) -> Option<PathBuf> {
        let path = uri_to_path(uri)?;
        if self.is_path_allowed(&path) {
            Some(path)
        } else {
            None
        }
    }
}

fn discover_sim_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_DISCOVERED_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_DISCOVERED_FILES {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip common noise directories.
            if name.starts_with('.')
                || name == "target"
                || name == "node_modules"
                || name == "editors"
            {
                continue;
            }
            discover_sim_files(&path, depth + 1, out);
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("sim"))
        {
            out.push(path);
        }
    }
}

/// Convert a `file://` URI to a filesystem path.
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let path = s.strip_prefix("file://")?;
    // Windows: file:///C:/...
    #[cfg(windows)]
    {
        let path = path.strip_prefix('/').unwrap_or(path);
        let decoded = percent_decode(path);
        Some(PathBuf::from(decoded))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from(percent_decode(path)))
    }
}

/// Build a `file://` URI from an absolute path.
pub fn path_to_uri(path: &Path) -> Result<String, String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    let abs = strip_windows_extended_path(abs);
    let mut s = abs.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && !s.starts_with('/') {
        s = format!("/{s}");
    }
    // Minimal percent-encoding for spaces.
    let s = s.replace(' ', "%20");
    Ok(format!("file://{s}"))
}

/// `std::fs::canonicalize` on Windows returns `\\?\C:\…`. A `file://` URI
/// built from that is not a valid path for `Uri` / `uri_to_path`.
fn strip_windows_extended_path(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &input[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Folders from LSP `WorkspaceFolder` URIs.
pub fn folders_from_uris<'a>(uris: impl IntoIterator<Item = &'a Uri>) -> Vec<PathBuf> {
    uris.into_iter().filter_map(uri_to_path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::str::FromStr;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sim-lsp-ws-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sandbox_rejects_outside_folder() {
        let dir = unique_temp_dir();
        let mut ws = Workspace::default();
        ws.set_folders(vec![dir.clone()]);
        let outside = std::env::temp_dir().join("sim-lsp-outside.sim");
        assert!(!ws.is_path_allowed(&outside));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovers_and_indexes_sim_files() {
        let dir = unique_temp_dir();
        let sim = dir.join("hello.sim");
        fs::write(&sim, "begin integer x; x := 1; end").unwrap();
        let mut ws = Workspace::default();
        ws.set_folders(vec![dir.clone()]);
        ws.reindex_all(&AnalysisOptions::default(), DEFAULT_MAX_DOCUMENT_BYTES);
        assert_eq!(ws.disk.len(), 1);
        let file = ws.disk.values().next().unwrap();
        assert!(file.snapshot.ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_uri_path() {
        let dir = unique_temp_dir();
        let path = dir.join("a.sim");
        fs::write(&path, "begin end").unwrap();
        let uri_str = path_to_uri(&path).unwrap();
        let uri = Uri::from_str(&uri_str).unwrap();
        let back = uri_to_path(&uri).unwrap();
        assert_eq!(
            std::fs::canonicalize(&path).unwrap(),
            std::fs::canonicalize(&back).unwrap()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_windows_extended_path_prefix() {
        assert_eq!(
            strip_windows_extended_path(PathBuf::from(r"\\?\C:\Users\me\a.sim")),
            PathBuf::from(r"C:\Users\me\a.sim")
        );
        assert_eq!(
            strip_windows_extended_path(PathBuf::from(r"\\?\UNC\server\share\a.sim")),
            PathBuf::from(r"\\server\share\a.sim")
        );
        let unix = PathBuf::from("/tmp/a.sim");
        assert_eq!(strip_windows_extended_path(unix.clone()), unix);
    }
}
