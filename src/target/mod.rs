//! Cross-compilation target configuration.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use clap::ValueEnum;

use crate::error::CompileError;

/// How C pointer+length and JS `String` text copies encode Simula character ranks.
/// Internal texts stay one rank per character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Charset {
    /// ISO 8859-1: one byte per rank 0–255 (default).
    #[default]
    Latin1,
    /// UTF-8 of ranks 0–255 at the FFI edge (`U+0000`..`U+00FF`).
    Utf8,
}

impl Charset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latin1 => "latin1",
            Self::Utf8 => "utf8",
        }
    }
}

impl fmt::Display for Charset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Native artifact shape for `sim compile --crate-type`.
/// On wasm the flag is ignored: every module exports `_start` plus public procedures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CrateType {
    /// Native program: `sim_main` is the process entry.
    #[default]
    Bin,
    /// Native shared library: no process entry; public scalar procedures are exported.
    Lib,
}

/// Preset compilation targets for native OSes and WebAssembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompileTarget {
    /// The host platform's default triple.
    Native,
    LinuxX86_64,
    LinuxAarch64,
    MacOsX86_64,
    MacOsAarch64,
    WindowsX86_64,
    /// WebAssembly with WASI — run the generated `*.mjs` (`node hello.mjs`).
    WasmNode,
    /// WebAssembly without WASI — open the generated `*.html` in a browser.
    WasmBrowser,
}

impl CompileTarget {
    pub fn all() -> &'static [Self] {
        &[
            Self::Native,
            Self::LinuxX86_64,
            Self::LinuxAarch64,
            Self::MacOsX86_64,
            Self::MacOsAarch64,
            Self::WindowsX86_64,
            Self::WasmNode,
            Self::WasmBrowser,
        ]
    }

    pub fn triple(&self) -> String {
        match self {
            Self::Native => host_triple(),
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu".into(),
            Self::LinuxAarch64 => "aarch64-unknown-linux-gnu".into(),
            Self::MacOsX86_64 => "x86_64-apple-darwin".into(),
            Self::MacOsAarch64 => "aarch64-apple-darwin".into(),
            Self::WindowsX86_64 => "x86_64-pc-windows-msvc".into(),
            Self::WasmNode => "wasm32-wasi".into(),
            Self::WasmBrowser => "wasm32-unknown-unknown".into(),
        }
    }

    pub fn is_wasm(&self) -> bool {
        matches!(self, Self::WasmNode | Self::WasmBrowser)
    }

    /// How to run a compiled artifact when it is not a native executable.
    pub fn run_hint(&self, artifact: &Path) -> Option<String> {
        match self {
            Self::WasmNode => {
                let runner = artifact.with_extension("mjs");
                Some(format!("Run with: node {}", runner.display()))
            }
            Self::WasmBrowser => {
                let page = artifact.with_extension("html");
                Some(format!("Open {} in a browser", page.display()))
            }
            _ => None,
        }
    }

    pub fn default_output_extension(&self) -> &'static str {
        self.default_output_extension_for(CrateType::Bin)
    }

    pub fn default_output_extension_for(&self, crate_type: CrateType) -> &'static str {
        if self.is_wasm() {
            return "wasm";
        }
        match crate_type {
            CrateType::Lib => {
                if matches!(self, Self::WindowsX86_64)
                    || (matches!(self, Self::Native) && cfg!(target_os = "windows"))
                {
                    "dll"
                } else if matches!(self, Self::MacOsX86_64 | Self::MacOsAarch64)
                    || (matches!(self, Self::Native) && cfg!(target_os = "macos"))
                {
                    "dylib"
                } else {
                    "so"
                }
            }
            CrateType::Bin => {
                if matches!(self, Self::WindowsX86_64)
                    || (matches!(self, Self::Native) && cfg!(target_os = "windows"))
                {
                    "exe"
                } else {
                    ""
                }
            }
        }
    }
}

impl fmt::Display for CompileTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Native => "native",
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::MacOsX86_64 => "macos-x86_64",
            Self::MacOsAarch64 => "macos-aarch64",
            Self::WindowsX86_64 => "windows-x86_64",
            Self::WasmNode => "wasm-node",
            Self::WasmBrowser => "wasm-browser",
        })
    }
}

impl FromStr for CompileTarget {
    type Err = CompileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "native" => Ok(Self::Native),
            "linux-x86_64" => Ok(Self::LinuxX86_64),
            "linux-aarch64" => Ok(Self::LinuxAarch64),
            "macos-x86_64" => Ok(Self::MacOsX86_64),
            "macos-aarch64" => Ok(Self::MacOsAarch64),
            "windows-x86_64" => Ok(Self::WindowsX86_64),
            "wasm-node" => Ok(Self::WasmNode),
            "wasm-browser" => Ok(Self::WasmBrowser),
            other => Err(CompileError::codegen(format!(
                "unknown target '{other}'. Run `sim targets` for supported values"
            ))),
        }
    }
}

fn host_triple() -> String {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "macos") => "x86_64-apple-darwin".into(),
        ("aarch64", "macos") => "aarch64-apple-darwin".into(),
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu".into(),
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu".into(),
        ("x86_64", "windows") => "x86_64-pc-windows-msvc".into(),
        (arch, os) => format!("{arch}-unknown-{os}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_targets_use_wasm_triples() {
        assert_eq!(CompileTarget::WasmNode.triple(), "wasm32-wasi");
        assert_eq!(
            CompileTarget::WasmBrowser.triple(),
            "wasm32-unknown-unknown"
        );
    }

    #[test]
    fn wasm_run_hints_name_the_wrapper() {
        assert_eq!(
            CompileTarget::WasmNode
                .run_hint(Path::new("hello.wasm"))
                .as_deref(),
            Some("Run with: node hello.mjs")
        );
        assert_eq!(
            CompileTarget::WasmBrowser
                .run_hint(Path::new("hello.wasm"))
                .as_deref(),
            Some("Open hello.html in a browser")
        );
        assert!(CompileTarget::Native.run_hint(Path::new("hello")).is_none());
    }

    #[test]
    fn parses_target_names() {
        assert_eq!(
            "wasm-node".parse::<CompileTarget>().unwrap(),
            CompileTarget::WasmNode
        );
    }
}
