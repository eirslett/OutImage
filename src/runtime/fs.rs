//! File system bindings for Simula programs.

use std::fs;
use std::path::Path;

use super::error::RuntimeError;

/// Whole-file BASICIO-ish MVP builtins (interpreter). Not the full
/// `stdlib/filesystem.sim` `open`/`read`/`write`/`close` File API.
const FILESYSTEM_PROCEDURES: &[&str] = &["fileExists", "fileRead", "fileWrite"];

/// Whether `name` is a whole-file filesystem MVP builtin (case-insensitive).
pub fn is_filesystem_procedure(name: &str) -> bool {
    FILESYSTEM_PROCEDURES
        .iter()
        .any(|proc| proc.eq_ignore_ascii_case(name))
}

/// Whether the filesystem MVP procedure returns a value usable as an expression.
pub fn filesystem_procedure_returns_value(name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "filewrite" => false,
        _ => is_filesystem_procedure(name),
    }
}

pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

pub fn read_file(path: &str) -> Result<String, RuntimeError> {
    if !Path::new(path).exists() {
        return Err(RuntimeError::NotFound(path.into()));
    }

    Ok(fs::read_to_string(path)?)
}

pub fn write_file(path: &str, contents: &str) -> Result<(), RuntimeError> {
    fs::write(path, contents)?;
    Ok(())
}

pub fn list_dir(path: &str) -> Result<Vec<String>, RuntimeError> {
    let entries = fs::read_dir(path)?;

    entries
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(RuntimeError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_filesystem_procedures() {
        assert!(is_filesystem_procedure("fileExists"));
        assert!(is_filesystem_procedure("FILEREAD"));
        assert!(is_filesystem_procedure("fileWrite"));
        assert!(!is_filesystem_procedure("exists"));
        assert!(!filesystem_procedure_returns_value("fileWrite"));
        assert!(filesystem_procedure_returns_value("fileRead"));
    }
}
