#[test]
fn compiles_hello_world_to_wasm_browser() {
    let output = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(r#"begin OutText("hello world"); OutImage; end;"#),
        &outimage::CompileOptions::for_compile(
            std::env::temp_dir().join("sim-test-hello-browser.wasm"),
            outimage::CompileTarget::WasmBrowser,
        ),
    )
    .expect("wasm-browser compile should succeed");

    match output {
        outimage::CompileResult::Artifact(path) => {
            let bytes = std::fs::read(&path).expect("wasm file should exist");
            assert!(bytes.starts_with(b"\0asm"));
            assert!(
                bytes
                    .windows(b"hello world".len())
                    .any(|w| w == b"hello world"),
                "expected MIR string literal in wasm-browser artifact"
            );
            let html = path.with_extension("html");
            let js = path.with_extension("js");
            assert!(
                html.is_file(),
                "wasm-browser should write {}",
                html.display()
            );
            assert!(js.is_file(), "wasm-browser should write {}", js.display());
            let host = path.with_file_name("wasm_host.mjs");
            assert!(
                host.is_file(),
                "wasm compile should write {}",
                host.display()
            );
            assert!(
                std::fs::read_to_string(&host)
                    .expect("wasm_host.mjs")
                    .contains("export async function instantiateSimulaWasm"),
                "wasm_host.mjs should export instantiateSimulaWasm"
            );
            let page = std::fs::read_to_string(&html).expect("html");
            assert!(
                page.contains("import { run }") && page.contains("await run();"),
                "html should import and run the sidecar: {page}"
            );
            let sidecar = std::fs::read_to_string(&js).expect("js");
            assert!(
                sidecar.contains("export async function run"),
                "js sidecar should export run()"
            );
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&html);
            let _ = std::fs::remove_file(&js);
        }
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected wasm artifact")
        }
    }
}

#[test]
fn compiles_hello_world_to_wasm_node() {
    let output = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(r#"begin OutText("hello world"); OutImage; end;"#),
        &outimage::CompileOptions::for_compile(
            std::env::temp_dir().join("sim-test-hello-node.wasm"),
            outimage::CompileTarget::WasmNode,
        ),
    )
    .expect("wasm-node compile should succeed");

    match output {
        outimage::CompileResult::Artifact(path) => {
            let bytes = std::fs::read(&path).expect("wasm file should exist");
            assert!(bytes.starts_with(b"\0asm"));
            let runner = path.with_extension("mjs");
            assert!(
                runner.is_file(),
                "wasm-node should write {}",
                runner.display()
            );
            let host = path.with_file_name("wasm_host.mjs");
            assert!(
                host.is_file(),
                "wasm compile should write {}",
                host.display()
            );
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&runner);
        }
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected wasm artifact")
        }
    }
}

#[test]
fn wasm_compile_no_wasm_host_skips_sidecar() {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sim-test-no-wasm-host-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let output_path = dir.join("hello.wasm");
    let mut options =
        outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::WasmBrowser);
    options.write_wasm_host = false;
    let result = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(r#"begin OutText("hello world"); OutImage; end;"#),
        &options,
    )
    .expect("wasm-browser compile should succeed");
    match result {
        outimage::CompileResult::Artifact(path) => {
            assert!(path.is_file());
            assert!(
                !dir.join("wasm_host.mjs").exists(),
                "--no-wasm-host should skip wasm_host.mjs"
            );
            assert!(
                path.with_extension("js").is_file(),
                "generated runner should still be written"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            let _ = std::fs::remove_dir_all(&dir);
            panic!("expected wasm artifact")
        }
    }
}

#[test]
fn native_compile_produces_executable() {
    let output_path = std::env::temp_dir().join("sim-test-hello-native");
    let output = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(r#"begin OutText("hello world"); OutImage; end;"#),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::Native,
        ),
    )
    .expect("native compile should succeed");

    match output {
        outimage::CompileResult::Artifact(path) => {
            assert!(path.exists());
        }
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected native artifact")
        }
    }
}

#[test]
fn native_compile_runs_hello_world() {
    let output_path = std::env::temp_dir().join("sim-test-hello-native-run");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(r#"begin OutText("hello world"); OutImage; end;"#),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::Native,
        ),
    )
    .expect("native compile should succeed")
    {
        outimage::CompileResult::Artifact(path) => path,
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected native artifact")
        }
    };

    let output = std::process::Command::new(&artifact)
        .output()
        .expect("compiled binary should run");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello world"
    );
}

#[test]
fn windows_native_link_gate() {
    let output_path = std::env::temp_dir().join("sim-test-windows-gate.exe");
    let result = outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(r#"begin OutText("x"); OutImage; end;"#),
        &outimage::CompileOptions::for_compile(
            output_path.clone(),
            outimage::CompileTarget::WindowsX86_64,
        ),
    );

    if cfg!(target_os = "windows") {
        let artifact = match result.expect("windows host should link PE") {
            outimage::CompileResult::Artifact(path) => path,
            outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
                panic!("expected PE artifact")
            }
        };
        let output = std::process::Command::new(&artifact)
            .output()
            .expect("compiled Windows binary should run");
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "x");
        let _ = std::fs::remove_file(&artifact);
        return;
    }

    let err = result.expect_err("non-Windows hosts cannot link PE");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot link Windows PE")
            || msg.contains("cannot link target")
            || msg.contains("unsupported target")
            || msg.contains("disabled"),
        "{msg}"
    );
}
