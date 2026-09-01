//! Compile `outimage-browser-interp` to wasm32 and emit wasm-bindgen JS glue
//! under `target/outimage-browser-interp/` (next to other Cargo artifacts).

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match pack() {
        Ok(out) => {
            eprintln!("browser-interp: wrote {}", out.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("browser-interp: {err}");
            ExitCode::FAILURE
        }
    }
}

fn pack() -> Result<PathBuf, Box<dyn Error>> {
    let cargo = env!("CARGO");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("browser-interp crate should live in the workspace")?
        .to_path_buf();

    let status = Command::new(cargo)
        .current_dir(&workspace)
        .args([
            "build",
            "-p",
            "outimage-browser-interp",
            "--lib",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()?;
    if !status.success() {
        return Err(
            "cargo build -p outimage-browser-interp --target wasm32-unknown-unknown failed".into(),
        );
    }

    let target_dir = cargo_target_dir(cargo, &workspace)?;
    let wasm = target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("outimage_browser_interp.wasm");
    if !wasm.is_file() {
        return Err(format!("missing wasm artifact {}", wasm.display()).into());
    }

    let out = target_dir.join("outimage-browser-interp");
    std::fs::create_dir_all(&out)?;
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm)
        .web(true)?
        .typescript(false)
        .omit_default_module_path(false)
        .generate(&out)?;
    Ok(out)
}

fn cargo_target_dir(cargo: &str, workspace: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()?;
    if !output.status.success() {
        return Err("cargo metadata failed".into());
    }
    let text = String::from_utf8(output.stdout)?;
    const KEY: &str = "\"target_directory\":\"";
    let rest = text
        .split_once(KEY)
        .ok_or("cargo metadata missing target_directory")?
        .1;
    let dir = rest
        .split_once('"')
        .ok_or("cargo metadata: unterminated target_directory")?
        .0;
    Ok(PathBuf::from(dir.replace("\\\\", "\\")))
}
