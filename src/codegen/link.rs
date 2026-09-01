//! Link native object files with the bundled Simula runtime and a host linker.
//!
//! Linker resolution (override with `SIM_LINKER`):
//! - macOS: Apple `ld` via `xcrun --find ld` / `/usr/bin/ld`
//! - Linux: C compiler driver (`cc` / `clang` / `gcc`) so libc search paths work
//! - Windows: MSVC `link.exe` (or `lld-link` if present)

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bundled;
use crate::error::CompileError;
use crate::target::{CompileTarget, CrateType};

/// Extra inputs from `--link` and from C identifications `lib:symbol`.
#[derive(Debug, Clone, Default)]
pub struct ExtraLink {
    pub files: Vec<PathBuf>,
    pub libs: Vec<String>,
}

/// Classifies `--link` items: existing files stay paths; `-lfoo` / `libfoo` /
/// `foo` become `-l` names.
pub fn classify_link_items(items: &[String]) -> ExtraLink {
    let mut extra = ExtraLink::default();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = Path::new(trimmed);
        if path.exists() {
            extra.files.push(path.to_path_buf());
            continue;
        }
        if let Some(name) = trimmed.strip_prefix("-l") {
            extra.libs.push(name.to_string());
            continue;
        }
        if !trimmed.contains('/')
            && !trimmed.contains('\\')
            && Path::new(trimmed).extension().is_none()
        {
            let name = trimmed.strip_prefix("lib").unwrap_or(trimmed);
            extra.libs.push(name.to_string());
            continue;
        }
        extra.files.push(path.to_path_buf());
    }
    extra
}

/// Default macOS deployment target when `MACOSX_DEPLOYMENT_TARGET` is unset.
/// Fixed (not taken from the host `sw_vers`) so linked binaries are reproducible
/// across developer machines.
const DEFAULT_MACOS_MIN_OS: &str = "11.0.0";

/// How to drive the resolved host linker binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkerKind {
    /// Apple `ld` / compatible Darwin linker (raw ld flags).
    DarwinLd,
    /// `cc`/`clang`/`gcc` as the link driver (`-Wl,…` for linker flags).
    GnuCc,
    /// Raw GNU/`ld.lld` linker (ld flags directly).
    GnuLd,
    /// MSVC `link.exe` / `lld-link`.
    MsvcLink,
}

struct HostLinker {
    path: PathBuf,
    kind: LinkerKind,
}

pub fn link_native(
    target: CompileTarget,
    object_path: &Path,
    output_path: &Path,
    debug_info: bool,
    crate_type: CrateType,
    extra: &ExtraLink,
) -> Result<PathBuf, CompileError> {
    assert_native_link_supported(target)?;

    let linker = find_host_linker(target)?;
    let runtime = bundled::cached_runtime_archive()?;

    let mut command = Command::new(&linker.path);
    if bundled::RUNTIME_SANITIZED {
        apply_sanitizer_link_args(&mut command, linker.kind)?;
    }
    match linker.kind {
        LinkerKind::MsvcLink => {
            apply_windows_link_args(
                &mut command,
                object_path,
                &runtime,
                output_path,
                debug_info,
                crate_type,
                extra,
            )?;
        }
        LinkerKind::DarwinLd => {
            apply_darwin_args(&mut command, target, crate_type, output_path)?;
            command.arg(object_path);
            for file in &extra.files {
                command.arg(file);
            }
            command.arg(&runtime);
            for lib in filtered_libs(target, &extra.libs) {
                command.arg(format!("-l{lib}"));
            }
            command.arg("-o").arg(output_path);
        }
        LinkerKind::GnuCc => {
            if crate_type == CrateType::Lib {
                command.arg("-shared");
            }
            if debug_info && linker_name_looks_like_lld(&linker.path) {
                command.arg("-Wl,--gdb-index");
            }
            command.arg(object_path);
            for file in &extra.files {
                command.arg(file);
            }
            command.arg(&runtime);
            command.arg("-o").arg(output_path);
            command.arg("-lc");
            command.arg("-lm");
            for lib in filtered_libs(target, &extra.libs) {
                command.arg(format!("-l{lib}"));
            }
            if crate_type == CrateType::Bin {
                // CRT needs `main` from the runtime archive; force the member in.
                command.arg("-Wl,-u,main");
            }
        }
        LinkerKind::GnuLd => {
            apply_gnu_ld_args(&mut command, crate_type);
            if debug_info && linker_name_looks_like_lld(&linker.path) {
                command.arg("--gdb-index");
            }
            command.arg(object_path);
            for file in &extra.files {
                command.arg(file);
            }
            command.arg(&runtime);
            for lib in filtered_libs(target, &extra.libs) {
                command.arg(format!("-l{lib}"));
            }
            command.arg("-o").arg(output_path);
        }
    }

    let command_summary =
        format_command_summary(&linker.path, object_path, &runtime, output_path, target);

    let output = command.output().map_err(|error| {
        CompileError::codegen(format!(
            "failed to invoke linker at {}: {error}",
            linker.path.display()
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
            (true, true) => String::new(),
            (false, true) => stderr.into_owned(),
            (true, false) => stdout.into_owned(),
            (false, false) => format!("{stderr}\n{stdout}"),
        };
        return Err(crate::diagnostics::linker_failed(
            &target.to_string(),
            format_link_failure(target, &detail, &command_summary),
        ));
    }

    Ok(output_path.to_path_buf())
}

fn filtered_libs(target: CompileTarget, libs: &[String]) -> impl Iterator<Item = &str> {
    libs.iter()
        .map(String::as_str)
        .filter(move |lib| !(link_flavor(target) == "darwin" && *lib == "m"))
}

fn apply_sanitizer_link_args(command: &mut Command, kind: LinkerKind) -> Result<(), CompileError> {
    match kind {
        LinkerKind::GnuCc => {
            command.arg("-fsanitize=address,undefined");
            Ok(())
        }
        LinkerKind::DarwinLd => Err(CompileError::codegen(
            "SIM_RT_SANITIZE=1 needs a C compiler as the linker on macOS \
             (Apple ld cannot pull in libclang_rt.asan). Set SIM_LINKER to \
             `clang` or unset SIM_RT_SANITIZE.",
        )),
        LinkerKind::GnuLd => Err(CompileError::codegen(
            "SIM_RT_SANITIZE=1 needs the C compiler driver as the linker \
             (`cc`/`clang`/`gcc`), not raw ld.",
        )),
        LinkerKind::MsvcLink => Err(CompileError::codegen(
            "SIM_RT_SANITIZE is not supported with MSVC",
        )),
    }
}

fn find_host_linker(target: CompileTarget) -> Result<HostLinker, CompileError> {
    if let Ok(override_path) = std::env::var("SIM_LINKER") {
        let trimmed = override_path.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            let kind = linker_kind_from_path(&path, target);
            return Ok(HostLinker { path, kind });
        }
    }

    match link_flavor(target) {
        "darwin" => find_darwin_linker(),
        "gnu" => find_gnu_linker(),
        "link" => find_windows_linker(),
        other => Err(CompileError::codegen(format!(
            "unsupported linker flavor '{other}' for target {target}"
        ))),
    }
}

fn linker_kind_from_path(path: &Path, target: CompileTarget) -> LinkerKind {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match link_flavor(target) {
        "darwin" => LinkerKind::DarwinLd,
        "link" => LinkerKind::MsvcLink,
        _ if name.contains("lld") && !name.contains("clang") => LinkerKind::GnuLd,
        _ if name == "ld" || name.starts_with("ld.") => LinkerKind::GnuLd,
        _ => LinkerKind::GnuCc,
    }
}

fn linker_name_looks_like_lld(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("lld"))
}

fn find_darwin_linker() -> Result<HostLinker, CompileError> {
    if let Some(path) = xcrun_find("ld") {
        return Ok(HostLinker {
            path,
            kind: LinkerKind::DarwinLd,
        });
    }
    let fallback = PathBuf::from("/usr/bin/ld");
    if fallback.exists() {
        return Ok(HostLinker {
            path: fallback,
            kind: LinkerKind::DarwinLd,
        });
    }
    Err(crate::diagnostics::linker_not_found(
        "native macOS linking requires the system linker (`ld`). \
         Install Xcode Command Line Tools (`xcode-select --install`) and ensure \
         `xcrun --find ld` works. Override with SIM_LINKER=/path/to/ld if needed.",
    ))
}

fn find_gnu_linker() -> Result<HostLinker, CompileError> {
    // Prefer a C compiler driver so libc/libm search paths are correct.
    for name in ["cc", "clang", "gcc"] {
        if let Some(path) = look_up_on_path(name) {
            return Ok(HostLinker {
                path,
                kind: LinkerKind::GnuCc,
            });
        }
    }
    // Fall back to a raw linker when no C compiler is on PATH.
    for name in ["ld.lld", "ld"] {
        if let Some(path) = look_up_on_path(name) {
            return Ok(HostLinker {
                path,
                kind: LinkerKind::GnuLd,
            });
        }
    }
    Err(crate::diagnostics::linker_not_found(
        "native Linux/ELF linking requires a C compiler (`cc`, `clang`, or `gcc`) \
         or linker (`ld.lld` / `ld`) on PATH. Install a toolchain package \
         (e.g. `build-essential` / `clang`) or set SIM_LINKER to the driver you want.",
    ))
}

fn find_windows_linker() -> Result<HostLinker, CompileError> {
    for name in ["link.exe", "link", "lld-link.exe", "lld-link"] {
        if let Some(path) = look_up_on_path(name) {
            return Ok(HostLinker {
                path,
                kind: LinkerKind::MsvcLink,
            });
        }
    }
    Err(crate::diagnostics::linker_not_found(
        "native Windows linking requires MSVC `link.exe` on PATH. \
         Open an \"x64 Native Tools\" Developer Command Prompt, or install \
         Visual Studio Build Tools. Override with SIM_LINKER=…\\link.exe if needed.",
    ))
}

fn look_up_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // Windows: allow extension-less lookup for `.exe`.
        if cfg!(windows) {
            let with_exe = dir.join(format!("{name}.exe"));
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
    }
    None
}

fn xcrun_find(tool: &str) -> Option<PathBuf> {
    let output = Command::new("xcrun").args(["--find", tool]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    let path = PathBuf::from(path);
    path.exists().then_some(path)
}

/// Refuse cross-OS links (host-built runtime). Windows PE is allowed on Windows hosts.
fn assert_native_link_supported(target: CompileTarget) -> Result<(), CompileError> {
    let want = link_flavor(target);
    let host = link_flavor(CompileTarget::Native);

    if want == "link" && host != "link" {
        return Err(CompileError::codegen(
            "cannot link Windows PE on this host: the bundled C runtime is not an MSVC library.\n\
             Build on Windows, or use `sim run` / `--target wasm-node` / `wasm-browser`.\n\
             Cross-OS native linking is not supported.",
        ));
    }

    if want != host {
        return Err(CompileError::codegen(format!(
            "cannot link target {target} on this host: the bundled C runtime was built \
             for linker flavor '{host}', not '{want}'.\n\
             Cross-OS native linking is not supported.\n\
             Use matching host OS, or compile to wasm (`--target wasm-node` / `wasm-browser`)."
        )));
    }

    Ok(())
}

fn apply_windows_link_args(
    command: &mut Command,
    object_path: &Path,
    runtime: &Path,
    output_path: &Path,
    debug_info: bool,
    crate_type: CrateType,
    extra: &ExtraLink,
) -> Result<(), CompileError> {
    // CRT entry: runtime.c provides `main` → `sim_main` under `_WIN32`.
    command.arg("/NOLOGO");
    if crate_type == CrateType::Lib {
        command.arg("/DLL");
    } else {
        command.arg("/SUBSYSTEM:CONSOLE");
    }
    command.arg(format!("/OUT:{}", output_path.display()));
    // Match `build.rs` `static_crt(true)` on MSVC. COFF defaultlibs from the
    // runtime archive pull kernel32 / UCRT once LIB is set.
    command.arg("/DEFAULTLIB:libcmt");
    command.arg("/DEFAULTLIB:oldnames");
    // Cranelift DWARF uses 32-bit section-relative relocs. MSVC `link.exe`
    // rejects those in a LARGEADDRESSAWARE image (LNK2017 / LNK1165).
    if debug_info {
        command.arg("/LARGEADDRESSAWARE:NO");
    }

    if let Ok(lib) = std::env::var("LIB") {
        for path in std::env::split_paths(&lib) {
            if !path.as_os_str().is_empty() {
                command.arg(format!("/LIBPATH:{}", path.display()));
            }
        }
    } else if let Some(paths) = discover_msvc_lib_paths() {
        for path in paths {
            command.arg(format!("/LIBPATH:{}", path.display()));
        }
    } else {
        return Err(CompileError::codegen(
            "native Windows linking requires the MSVC library path (LIB).\n\
             Open an \"x64 Native Tools\" Developer Command Prompt, or install \
             Visual Studio Build Tools and ensure `vcvars64.bat` has been run.\n\
             CI: use ilammy/msvc-dev-cmd before `sim compile`.",
        ));
    }

    command.arg(object_path);
    for file in &extra.files {
        command.arg(file);
    }
    command.arg(runtime);
    for lib in &extra.libs {
        command.arg(format!("{lib}.lib"));
    }
    Ok(())
}

/// Best-effort MSVC / Windows SDK lib directories when `LIB` is unset.
fn discover_msvc_lib_paths() -> Option<Vec<PathBuf>> {
    let vswhere =
        PathBuf::from(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    if !vswhere.exists() {
        return None;
    }
    let output = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let install = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if install.is_empty() {
        return None;
    }
    let msvc_root = PathBuf::from(&install).join(r"VC\Tools\MSVC");
    let msvc_ver = newest_subdir(&msvc_root)?;
    let mut paths = vec![msvc_ver.join(r"lib\x64")];

    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\Lib");
    if let Some(sdk_ver) = newest_subdir(&kits) {
        paths.push(sdk_ver.join(r"ucrt\x64"));
        paths.push(sdk_ver.join(r"um\x64"));
    }
    Some(paths)
}

fn newest_subdir(parent: &Path) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.pop()
}

/// Turns raw linker stderr into a short, actionable compile error.
pub(crate) fn format_link_failure(
    target: CompileTarget,
    stderr: &str,
    command_summary: &str,
) -> String {
    let trimmed = stderr.trim();
    let mut hints = Vec::new();

    let undefined = collect_undefined_symbols(trimmed);
    if !undefined.is_empty() {
        let list = undefined
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = if undefined.len() > 8 {
            format!(" (+{} more)", undefined.len() - 8)
        } else {
            String::new()
        };
        hints.push(format!("undefined symbol(s): {list}{more}"));
        if undefined.iter().any(|name| name.starts_with("simrt_")) {
            hints.push(
                "a Simula runtime helper is missing from the bundled archive — rebuild \
                 with `cargo clean` / check `runtime/runtime.c`"
                    .into(),
            );
        } else if undefined
            .iter()
            .any(|name| name == "sim_main" || name == "_sim_main" || name.starts_with("simrt_"))
        {
            hints.push("the object file may be empty or for the wrong target triple".into());
        }
    }

    if trimmed.contains("SDK")
        || trimmed.contains("syslibroot")
        || trimmed.contains("library not found for -lSystem")
    {
        hints.push(
            "macOS SDK lookup failed — install Xcode Command Line Tools \
             (`xcode-select --install`) and ensure `xcrun --show-sdk-path` works"
                .into(),
        );
    }

    if trimmed.contains("unknown architecture") || trimmed.contains("wrong architecture") {
        hints.push(format!(
            "object / runtime architecture mismatch for target {target}"
        ));
    }

    if link_flavor(target) == "link"
        && (trimmed.contains("LNK1104")
            || trimmed.contains("cannot open")
            || trimmed.contains("libcmt")
            || trimmed.contains("LIBPATH")
            || trimmed.contains("unresolved external"))
    {
        hints.push(
            "MSVC / Windows SDK libraries missing — set LIB (Developer Command Prompt) \
             or install Visual Studio Build Tools"
                .into(),
        );
    }

    let mut message = format!("linker failed for target {target}");
    if !hints.is_empty() {
        message.push('\n');
        for hint in &hints {
            message.push_str("  • ");
            message.push_str(hint);
            message.push('\n');
        }
    }
    if !trimmed.is_empty() {
        message.push_str("linker stderr:\n");
        message.push_str(trimmed);
        message.push('\n');
    }
    message.push_str("link command: ");
    message.push_str(command_summary);
    message
}

fn collect_undefined_symbols(stderr: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    for line in stderr.lines() {
        let line = line.trim();
        // lld-style: "error: undefined symbol: foo" (possibly prefixed with `ld64.lld:`)
        if let Some(idx) = line.find("undefined symbol:") {
            let rest = line[idx + "undefined symbol:".len()..].trim();
            if !rest.is_empty() {
                push_unique(&mut symbols, rest.to_string());
            }
            continue;
        }
        // ld64-style: "  \"_foo\", referenced from:"
        if let Some(rest) = line.strip_prefix('"')
            && let Some((name, _)) = rest.split_once('"')
            && !name.is_empty()
            && (line.contains("referenced from") || line.ends_with('"') || line.contains(','))
        {
            // Prefer names that look like symbols, not paths.
            if !name.contains('/') && !name.contains(' ') {
                push_unique(&mut symbols, name.to_string());
            }
        }
        // GNU ld: "undefined reference to `foo'"
        if let Some(start) = line.find("undefined reference to `") {
            let rest = &line[start + "undefined reference to `".len()..];
            if let Some(end) = rest.find('\'') {
                push_unique(&mut symbols, rest[..end].to_string());
            }
        }
        // lld-link / MSVC: "unresolved external symbol foo"
        if let Some(idx) = line.find("unresolved external symbol ") {
            let rest = line[idx + "unresolved external symbol ".len()..].trim();
            let name = rest
                .split_whitespace()
                .next()
                .unwrap_or(rest)
                .trim_matches(|c| c == '\'' || c == '"');
            if !name.is_empty() {
                push_unique(&mut symbols, name.to_string());
            }
        }
    }
    symbols
}

fn push_unique(symbols: &mut Vec<String>, name: String) {
    if !symbols.iter().any(|existing| existing == &name) {
        symbols.push(name);
    }
}

fn format_command_summary(
    lld: &Path,
    object_path: &Path,
    runtime: &Path,
    output_path: &Path,
    target: CompileTarget,
) -> String {
    let flavor = link_flavor(target);
    if flavor == "link" {
        format!(
            "{} … /OUT:{}  (object={}, runtime={}, flavor={})",
            lld.display(),
            output_path.display(),
            object_path.display(),
            runtime.display(),
            flavor,
        )
    } else {
        format!(
            "{} … -o {}  (object={}, runtime={}, flavor={})",
            lld.display(),
            output_path.display(),
            object_path.display(),
            runtime.display(),
            flavor,
        )
    }
}

fn apply_darwin_args(
    command: &mut Command,
    target: CompileTarget,
    crate_type: CrateType,
    output_path: &Path,
) -> Result<(), CompileError> {
    let arch = darwin_arch(target)?;
    let sdk = darwin_sdk_root()?;
    let min_os = darwin_min_os_version();
    command
        .arg("-arch")
        .arg(arch)
        .arg("-platform_version")
        .arg("macos")
        .arg(&min_os)
        .arg(&min_os)
        .arg("-syslibroot")
        .arg(sdk);
    if crate_type == CrateType::Lib {
        command.arg("-dylib");
        command.arg("-install_name").arg(output_path);
        command.arg("-undefined").arg("dynamic_lookup");
    } else {
        command.arg("-e").arg("_sim_main");
    }
    command.arg("-lSystem");
    Ok(())
}

fn apply_gnu_ld_args(command: &mut Command, crate_type: CrateType) {
    // `-lm` for math symbols from `runtime/runtime.c` (`pow`, `sqrt`, …).
    // glibc often pulls libm via libc; musl and stricter linkers do not.
    if crate_type == CrateType::Lib {
        command.arg("-shared");
    } else {
        command.arg("-e").arg("sim_main");
    }
    command.arg("-lc").arg("-lm");
}

fn link_flavor(target: CompileTarget) -> &'static str {
    match target {
        CompileTarget::MacOsX86_64 | CompileTarget::MacOsAarch64 => "darwin",
        CompileTarget::WindowsX86_64 => "link",
        CompileTarget::Native if cfg!(target_os = "macos") => "darwin",
        CompileTarget::Native if cfg!(target_os = "windows") => "link",
        _ => "gnu",
    }
}

fn darwin_arch(target: CompileTarget) -> Result<&'static str, CompileError> {
    match target {
        CompileTarget::MacOsX86_64 => Ok("x86_64"),
        CompileTarget::MacOsAarch64 => Ok("arm64"),
        CompileTarget::Native if cfg!(target_os = "macos") => {
            if cfg!(target_arch = "aarch64") {
                Ok("arm64")
            } else {
                Ok("x86_64")
            }
        }
        other => Err(CompileError::codegen(format!(
            "target {other} is not a macOS target"
        ))),
    }
}

fn darwin_sdk_root() -> Result<String, CompileError> {
    let output = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .map_err(|error| CompileError::codegen(format!("failed to invoke xcrun: {error}")))?;

    if !output.status.success() {
        return Err(CompileError::codegen(
            "xcrun --show-sdk-path failed.\n\
             Install Xcode command line tools for native macOS linking.",
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// macOS deployment target for `-platform_version`.
///
/// Prefer `MACOSX_DEPLOYMENT_TARGET` when set; otherwise use
/// [`DEFAULT_MACOS_MIN_OS`] so builds do not depend on the host OS version.
fn darwin_min_os_version() -> String {
    match std::env::var("MACOSX_DEPLOYMENT_TARGET") {
        Ok(value) if !value.trim().is_empty() => normalize_macos_version(value.trim()),
        _ => DEFAULT_MACOS_MIN_OS.to_string(),
    }
}

fn normalize_macos_version(version: &str) -> String {
    // ld64 expects three components for `-platform_version`.
    let parts: Vec<&str> = version.split('.').collect();
    match parts.as_slice() {
        [major] => format!("{major}.0.0"),
        [major, minor] => format!("{major}.{minor}.0"),
        [major, minor, patch, ..] => format!("{major}.{minor}.{patch}"),
        [] => DEFAULT_MACOS_MIN_OS.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_link_items_splits_files_and_libs() {
        let extra = classify_link_items(&["-lm".into(), "libfoo".into(), "bar".into()]);
        assert_eq!(extra.libs, vec!["m", "foo", "bar"]);
        assert!(extra.files.is_empty());
    }

    #[test]
    fn formats_lld_undefined_symbol() {
        let stderr =
            "ld64.lld: error: undefined symbol: simrt_out_text\n>>> referenced by main.o\n";
        let message = format_link_failure(CompileTarget::Native, stderr, "lld … -o prog");
        assert!(
            message.contains("undefined symbol(s): simrt_out_text"),
            "{message}"
        );
        assert!(
            message.contains("Simula runtime helper is missing"),
            "{message}"
        );
        assert!(message.contains("link command: lld"), "{message}");
    }

    #[test]
    fn formats_gnu_undefined_reference() {
        let stderr = "ld: main.o: undefined reference to `sim_main'\n";
        let message = format_link_failure(CompileTarget::LinuxX86_64, stderr, "ld.lld …");
        assert!(
            message.contains("undefined symbol(s): sim_main"),
            "{message}"
        );
        assert!(message.contains("object file may be empty"), "{message}");
    }

    #[test]
    fn formats_sdk_failure_hint() {
        let stderr = "ld: library not found for -lSystem\n";
        let message = format_link_failure(CompileTarget::Native, stderr, "lld …");
        assert!(message.contains("macOS SDK lookup failed"), "{message}");
    }

    #[test]
    fn normalizes_deployment_target_versions() {
        assert_eq!(normalize_macos_version("11"), "11.0.0");
        assert_eq!(normalize_macos_version("12.3"), "12.3.0");
        assert_eq!(normalize_macos_version("13.0.1"), "13.0.1");
    }

    #[test]
    fn default_min_os_is_stable() {
        // Ensure we are not accidentally reading sw_vers in the default path.
        assert_eq!(DEFAULT_MACOS_MIN_OS, "11.0.0");
    }

    #[test]
    fn refuses_windows_native_link_off_windows() {
        if cfg!(target_os = "windows") {
            assert!(assert_native_link_supported(CompileTarget::WindowsX86_64).is_ok());
            assert!(assert_native_link_supported(CompileTarget::Native).is_ok());
            return;
        }
        let err = assert_native_link_supported(CompileTarget::WindowsX86_64).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot link Windows PE") || msg.contains("cannot link target"),
            "{msg}"
        );
    }

    #[test]
    fn refuses_cross_os_native_link() {
        // Pick a target whose linker flavor differs from the host.
        let foreign = if cfg!(target_os = "macos") {
            CompileTarget::LinuxX86_64
        } else if cfg!(target_os = "windows") {
            CompileTarget::LinuxX86_64
        } else {
            CompileTarget::MacOsAarch64
        };
        let err = assert_native_link_supported(foreign).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot link target") || msg.contains("cannot link Windows"),
            "{msg}"
        );
    }

    #[test]
    fn accepts_host_native_flavor() {
        assert!(assert_native_link_supported(CompileTarget::Native).is_ok());
    }
}
