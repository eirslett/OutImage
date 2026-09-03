//! Native compile+run regression for TestBatch units that are known-good.
//! Expand the allowlist as more corpus tests pass.
//!
//! Full corpus sweep: `./tests/run_testbatch.py native` (also `wasm` / `interp`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unit that stops making progress must fail the test rather than wedge the
/// whole suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

fn temp_bin(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-result-plan-{tag}-{id}"))
}

fn corpus_root() -> &'static Path {
    Path::new("tests/testbatch")
}

fn corpus_unit(name: &str) -> Vec<PathBuf> {
    let root = corpus_root();
    let extras = Path::new("tests/fixtures/dostestbatch_externals");
    let overrides = Path::new("tests/fixtures/dostestbatch_overrides");
    if name == "simtst59" {
        return vec![extras.join("c59.sim"), root.join("simtst59.sim")];
    }
    if name == "simtst40" {
        return vec![
            extras.join("pa.sim"),
            extras.join("pb.sim"),
            root.join("simtst40.sim"),
        ];
    }
    if name == "simtst41" {
        return vec![extras.join("p41.sim"), root.join("simtst41.sim")];
    }
    let override_src = overrides.join(format!("{name}.sim"));
    if override_src.is_file() {
        return vec![override_src];
    }
    vec![root.join(format!("{name}.sim"))]
}

fn compile_and_run(sources: &[PathBuf], stdin: &[u8]) -> Result<String, String> {
    let bin = temp_bin("unit");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sim"));
    cmd.arg("compile").arg("--target").arg("native");
    for src in sources {
        cmd.arg(src);
    }
    cmd.arg("-o").arg(&bin);
    let compile = cmd.output().map_err(|e| format!("spawn compile: {e}"))?;
    if !compile.status.success() {
        let _ = std::fs::remove_file(&bin);
        return Err(format!(
            "compile failed:\n{}{}",
            String::from_utf8_lossy(&compile.stdout),
            String::from_utf8_lossy(&compile.stderr)
        ));
    }
    // TestBatch units that open external files expect fixtures in cwd.
    let run_dir = std::env::temp_dir().join(format!(
        "sim-result-plan-cwd-{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&run_dir);
    let fixtures = Path::new("tests/fixtures/dostestbatch_data");
    if fixtures.is_dir()
        && let Ok(entries) = std::fs::read_dir(fixtures)
    {
        for entry in entries.flatten() {
            let dest = run_dir.join(entry.file_name());
            let _ = std::fs::copy(entry.path(), dest);
        }
    }
    // Redirect to files rather than pipes: a unit that outruns the pipe buffer
    // would otherwise block forever while we are polling for the deadline.
    let stdout_path = run_dir.join("__stdout");
    let stderr_path = run_dir.join("__stderr");
    let mut run = Command::new(&bin);
    run.current_dir(&run_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&stdout_path).map_err(|e| format!("stdout file: {e}"))?)
        .stderr(std::fs::File::create(&stderr_path).map_err(|e| format!("stderr file: {e}"))?);
    let mut child = run.spawn().map_err(|e| format!("spawn run: {e}"))?;
    if let Some(mut child_stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = child_stdin.write_all(stdin);
    }

    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| format!("wait run: {e}"))? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_dir_all(&run_dir);

    let Some(status) = status else {
        return Err(format!(
            "run timed out after {}s:\nstdout:\n{stdout}\nstderr:\n{stderr}",
            RUN_TIMEOUT.as_secs()
        ));
    };
    if !status.success() {
        return Err(format!(
            "run failed (exit {:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    Ok(stdout)
}

fn assert_corpus_success(name: &str) {
    let sources = corpus_unit(name);
    for src in &sources {
        assert!(src.is_file(), "missing {}", src.display());
    }
    let stdin: &[u8] = match name {
        "simtst86" => b"E\n",
        "simtst88" => b"!\n",
        "simtst89" => b"any8189\nout89.bin\n",
        _ => b"",
    };
    let stdout = compile_and_run(&sources, stdin).unwrap_or_else(|e| panic!("{name}: {e}"));
    let upper = stdout.to_ascii_uppercase();
    let has_hard_error = upper.contains("*** ERROR")
        || upper
            .lines()
            .any(|l| l.contains("ERROR:") || l.contains("--- ERROR"));
    let ok = !has_hard_error
        && (upper.contains("NO ERRORS")
            || upper.contains("END SIMULA")
            || upper.contains("..END SIMULA"));
    assert!(
        ok,
        "{name}: unexpected stdout (missing success marker or has ERROR):\n{stdout}"
    );
}

#[test]
fn dostestbatch_simtst96_native_success() {
    assert_corpus_success("simtst96");
}

#[test]
fn dostestbatch_simulation_units_native_success() {
    // Simulation / nested-class units.
    for name in ["simtst85", "simtst87", "simtst95"] {
        assert_corpus_success(name);
    }
}

#[test]
fn dostestbatch_allowlist_native_success() {
    // Curated from TestBatch passes + units fixed by nested-if / bool-array /
    // pow / text-semantics / sysout.image / paren-putchar regressions.
    // Grow this list as more corpus units go green. simtst68 is still off the
    // native allowlist; simtst96 has its own test above.
    for name in [
        "simtst00", "simtst01", "simtst02", "simtst03", "simtst04", "simtst05", "simtst06",
        "simtst07", "simtst08", "simtst09", "simtst10", "simtst11", "simtst12", "simtst13",
        "simtst14", "simtst15", "simtst16", "simtst17", "simtst18", "simtst19", "simtst20",
        "simtst21", "simtst22", "simtst23", "simtst24", "simtst25", "simtst26", "simtst27",
        "simtst28", "simtst29", "simtst30", "simtst31", "simtst32", "simtst33", "simtst34",
        "simtst35", "simtst36", "simtst37", "simtst38", "simtst39", "simtst40", "simtst41",
        "simtst42", "simtst43", "simtst44", "simtst45", "simtst46", "simtst47", "simtst48",
        "simtst49", "simtst50", "simtst51", "simtst52", "simtst53", "simtst54", "simtst55",
        "simtst56", "simtst57", "simtst58", "simtst59", "simtst60", "simtst61", "simtst62",
        "simtst63", "simtst64", "simtst65", "simtst69", "simtst66", "simtst67", "simtst70",
        "simtst71", "simtst72", "simtst73", "simtst74", "simtst75", "simtst76", "simtst77",
        "simtst78", "simtst79", "simtst80", "simtst81", "simtst82", "simtst83", "simtst84",
        "simtst85", "simtst86", "simtst87", "simtst88", "simtst89", "simtst90", "simtst91",
        "simtst92", "simtst93", "simtst94", "simtst95", "simtst99",
    ] {
        assert_corpus_success(name);
    }
}
