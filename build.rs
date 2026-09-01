use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-env-changed=SIM_RT_SANITIZE");
    println!("cargo:rerun-if-changed=runtime/runtime.c");
    println!("cargo:rerun-if-changed=runtime/runtime.h");
    println!("cargo:rerun-if-changed=runtime/internal.h");
    println!("cargo:rerun-if-changed=runtime/annot.h");
    println!("cargo:rerun-if-changed=runtime/host.h");
    println!("cargo:rerun-if-changed=runtime/safety.c");
    println!("cargo:rerun-if-changed=runtime/safety.h");
    println!("cargo:rerun-if-changed=runtime/env.c");
    println!("cargo:rerun-if-changed=runtime/array.c");
    println!("cargo:rerun-if-changed=runtime/text.c");
    println!("cargo:rerun-if-changed=runtime/object.c");
    println!("cargo:rerun-if-changed=runtime/io.c");
    println!("cargo:rerun-if-changed=runtime/sim.c");
    println!("cargo:rerun-if-changed=runtime/coro.c");
    println!("cargo:rerun-if-changed=runtime/coro.h");
    println!("cargo:rerun-if-changed=runtime/sequencing.c");
    println!("cargo:rerun-if-changed=runtime/sequencing.h");
    println!("cargo:rerun-if-changed=runtime/gc.c");
    println!("cargo:rerun-if-changed=runtime/embed.c");
    println!("cargo:rerun-if-changed=runtime/embed.h");
    println!("cargo:rerun-if-changed=runtime/wasm-rt/Cargo.toml");
    println!("cargo:rerun-if-changed=runtime/wasm-rt/src/math.rs");
    println!("cargo:rerun-if-changed=runtime/wasm-rt/src/abi.rs");
    println!("cargo:rerun-if-changed=runtime/wasm-rt/src/arena.rs");
    println!("cargo:rerun-if-changed=runtime/wasm-rt/src/layout.rs");
    println!("cargo:rerun-if-changed=runtime/wasm-rt/src/runtime/mod.rs");
    println!("cargo:rerun-if-changed=src/runtime/text.rs");
    println!("cargo:rerun-if-changed=src/runtime/environment.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let wasm_aot = feature_enabled("CARGO_FEATURE_WASM_AOT");
    let native_aot = feature_enabled("CARGO_FEATURE_NATIVE_AOT") && !target_is_wasm32();
    if !wasm_aot && !native_aot {
        return;
    }

    let wasm_runtime = compile_wasm_runtime(&out_dir, &manifest_dir, false);
    let wasm_runtime_math = compile_wasm_runtime(&out_dir, &manifest_dir, true);

    let bundled_rs = out_dir.join("bundled_assets.rs");
    let mut file = fs::File::create(&bundled_rs).expect("create bundled_assets.rs");
    let wasm_lit = escape_path_for_include(&wasm_runtime);
    let wasm_math_lit = escape_path_for_include(&wasm_runtime_math);

    if native_aot {
        let (runtime_archive, sanitized) = compile_native_runtime(&out_dir, &manifest_dir);
        let runtime_file_name = runtime_archive
            .file_name()
            .and_then(|n| n.to_str())
            .expect("runtime archive file name")
            .to_string();
        let runtime_lit = escape_path_for_include(&runtime_archive);
        writeln!(
            file,
            r#"pub const RUNTIME_ARCHIVE_NAME: &str = "{runtime_file_name}";
pub const RUNTIME_ARCHIVE: &[u8] = include_bytes!({runtime_lit});
pub const RUNTIME_SANITIZED: bool = {sanitized};
pub const WASM_RUNTIME: &[u8] = include_bytes!({wasm_lit});
pub const WASM_RUNTIME_MATH: &[u8] = include_bytes!({wasm_math_lit});
"#
        )
        .expect("write bundled_assets.rs");
    } else {
        writeln!(
            file,
            r#"pub const RUNTIME_ARCHIVE_NAME: &str = "";
pub const RUNTIME_ARCHIVE: &[u8] = &[];
pub const RUNTIME_SANITIZED: bool = false;
pub const WASM_RUNTIME: &[u8] = include_bytes!({wasm_lit});
pub const WASM_RUNTIME_MATH: &[u8] = include_bytes!({wasm_math_lit});
"#
        )
        .expect("write bundled_assets.rs");
    }
}

fn feature_enabled(name: &str) -> bool {
    env::var(name).is_ok()
}

fn target_is_wasm32() -> bool {
    env::var("CARGO_CFG_TARGET_ARCH").ok().as_deref() == Some("wasm32")
}

fn compile_native_runtime(out_dir: &Path, manifest_dir: &Path) -> (PathBuf, bool) {
    let sanitize = runtime_sanitize_requested();

    // Unsanitized copy is what the compiler binary itself links. AOT programs
    // use the bundled archive below, which may be a second, sanitized build.
    let mut host = cc::Build::new();
    add_runtime_files(&mut host, manifest_dir);
    if cfg!(target_env = "msvc") {
        host.static_crt(true);
    }
    host.compile("simrt_rt");

    if sanitize {
        let mut san = cc::Build::new();
        add_runtime_files(&mut san, manifest_dir);
        san.flag("-fsanitize=address,undefined");
        san.flag("-fno-omit-frame-pointer");
        san.flag("-g");
        san.cargo_metadata(false);
        san.compile("simrt_san");
        let archive = find_runtime_archive(out_dir, "simrt_san")
            .or_else(|| find_runtime_archive(out_dir, "simrt_rt"))
            .unwrap_or_else(|| {
                panic!(
                    "cc produced no sanitized simrt_rt archive under {}",
                    out_dir.display()
                )
            });
        (archive, true)
    } else {
        let archive = find_runtime_archive(out_dir, "simrt_rt").unwrap_or_else(|| {
            panic!(
                "cc produced no simrt_rt archive under {} \
                 (expected libsimrt_rt.a or simrt_rt.lib)",
                out_dir.display()
            )
        });
        (archive, false)
    }
}

fn compile_wasm_runtime(out_dir: &Path, manifest_dir: &Path, math_only: bool) -> PathBuf {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = manifest_dir.join("runtime/wasm-rt/Cargo.toml");
    let target_dir = out_dir.join(if math_only {
        "wasm-rt-math-target"
    } else {
        "wasm-rt-target"
    });
    let rustflags = [
        "-C",
        "panic=abort",
        "-C",
        "link-arg=--import-memory",
        "-C",
        "link-arg=--no-entry",
        "-C",
        "link-arg=--initial-memory=2097152",
    ]
    .join(" ");

    let mut command = std::process::Command::new(&cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("--release")
        .arg("--target-dir")
        .arg(&target_dir);
    if math_only {
        command
            .arg("--no-default-features")
            .arg("--features")
            .arg("math-only");
    }
    let status = command
        .env("RUSTFLAGS", rustflags)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .unwrap_or_else(|error| panic!("failed to spawn cargo for outimage-wasm-rt: {error}"));
    if !status.success() {
        panic!(
            "outimage-wasm-rt wasm32 {} build failed with {status}",
            if math_only { "math-only" } else { "full" }
        );
    }

    let wasm = target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("outimage_wasm_rt.wasm");
    if !wasm.is_file() {
        panic!("outimage-wasm-rt did not produce {}", wasm.display());
    }
    wasm
}

fn runtime_sanitize_requested() -> bool {
    if cfg!(target_env = "msvc") {
        return false;
    }
    match env::var("SIM_RT_SANITIZE") {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    }
}

fn add_runtime_files(build: &mut cc::Build, manifest_dir: &Path) {
    build.file(manifest_dir.join("runtime/runtime.c"));
    build.file(manifest_dir.join("runtime/env.c"));
    build.file(manifest_dir.join("runtime/array.c"));
    build.file(manifest_dir.join("runtime/text.c"));
    build.file(manifest_dir.join("runtime/object.c"));
    build.file(manifest_dir.join("runtime/io.c"));
    build.file(manifest_dir.join("runtime/sim.c"));
    build.file(manifest_dir.join("runtime/safety.c"));
    build.file(manifest_dir.join("runtime/coro.c"));
    build.file(manifest_dir.join("runtime/sequencing.c"));
    build.file(manifest_dir.join("runtime/gc.c"));
    build.file(manifest_dir.join("runtime/embed.c"));
    if cfg!(target_env = "msvc") {
        build.static_crt(true);
    }
}

fn escape_path_for_include(path: &Path) -> String {
    format!("r#\"{}\"#", path.display())
}

fn find_runtime_archive(out_dir: &Path, stem: &str) -> Option<PathBuf> {
    let candidates = [
        format!("lib{stem}.a"),
        format!("{stem}.lib"),
        format!("lib{stem}.lib"),
        format!("{stem}.a"),
    ];
    for name in candidates {
        let path = out_dir.join(&name);
        if path.exists() {
            return Some(path);
        }
    }
    if let Ok(entries) = fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && (name == format!("lib{stem}.a") || name == format!("{stem}.lib"))
            {
                return Some(path);
            }
        }
    }
    None
}
