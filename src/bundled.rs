//! Native AOT links the host C archive; wasm AOT embeds `outimage-wasm-rt`
//! (the interpreter runtime compiled to wasm32) as a `simrt` custom section.

include!(concat!(env!("OUT_DIR"), "/bundled_assets.rs"));

#[cfg(feature = "native-aot")]
mod native {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::error::CompileError;

    use super::{RUNTIME_ARCHIVE, RUNTIME_ARCHIVE_NAME};

    pub fn cached_runtime_archive() -> Result<PathBuf, CompileError> {
        materialize_asset(RUNTIME_ARCHIVE_NAME, RUNTIME_ARCHIVE)
    }

    fn materialize_asset(name: &str, bytes: &[u8]) -> Result<PathBuf, CompileError> {
        let path = cache_dir()?.join(name);
        materialize_asset_at(&path, bytes)?;
        Ok(path)
    }

    fn materialize_asset_at(path: &Path, bytes: &[u8]) -> Result<(), CompileError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CompileError::codegen(format!(
                    "failed to create cache directory {}: {error}",
                    parent.display()
                ))
            })?;
        }

        let needs_write = match fs::read(path) {
            Ok(existing) => existing != bytes,
            Err(_) => true,
        };

        if needs_write {
            fs::write(path, bytes).map_err(|error| {
                CompileError::codegen(format!(
                    "failed to write bundled asset {}: {error}",
                    path.display()
                ))
            })?;
        }

        Ok(())
    }

    fn cache_dir() -> Result<PathBuf, CompileError> {
        if let Ok(dir) = std::env::var("SIM_CACHE") {
            return Ok(PathBuf::from(dir));
        }

        // Windows
        if let Ok(local) = std::env::var("LOCALAPPDATA")
            && !local.is_empty()
        {
            return Ok(PathBuf::from(local)
                .join("sim")
                .join(env!("CARGO_PKG_VERSION")));
        }

        // XDG (Linux / BSD)
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
            && !xdg.is_empty()
        {
            return Ok(PathBuf::from(xdg)
                .join("sim")
                .join(env!("CARGO_PKG_VERSION")));
        }

        if let Ok(home) = std::env::var("HOME") {
            return Ok(PathBuf::from(home)
                .join(".cache")
                .join("sim")
                .join(env!("CARGO_PKG_VERSION")));
        }

        // USERPROFILE on Windows when LOCALAPPDATA is unset
        if let Ok(profile) = std::env::var("USERPROFILE")
            && !profile.is_empty()
        {
            return Ok(PathBuf::from(profile)
                .join(".cache")
                .join("sim")
                .join(env!("CARGO_PKG_VERSION")));
        }

        Ok(std::env::temp_dir()
            .join("outimage-cache")
            .join(env!("CARGO_PKG_VERSION")))
    }
}

#[cfg(feature = "native-aot")]
pub use native::cached_runtime_archive;
