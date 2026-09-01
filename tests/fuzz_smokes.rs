//! Lightweight fuzz smokes for lex/parse and differential interp vs MIR.
//!
//! These are not a full fuzzing harness — they exercise a fixed corpus of
//! mutated / random-ish inputs so CI catches obvious crashes and silent
//! miscompiles on small programs.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use outimage::source::SourceFile;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-fuzz-{tag}-{id}"))
}

#[test]
fn lex_parse_fuzz_smokes_do_not_panic() {
    let long_begin = "begin ".repeat(50);
    let long_string = format!("begin OutText(\"{}\"); end;", "x".repeat(200));
    let corpus = [
        "",
        "begin end;",
        "begin integer x; x := 1; end;",
        "@@@@",
        "begin /* unclosed",
        "begin OutText(\"hi\"); OutImage; end;",
        "Simulation begin hold(0); end;",
        "begin class C; begin end; ref(C) r; r :- new C; end;",
        "begin integer array a(1:3); a(2) := 9; end;",
        "begin integer i; for i := 1 step 1 until 3 do OutInt(i, 0); end;",
        "begin integer i; for i := 1 while false do i := i; end;",
        long_begin.as_str(),
        long_string.as_str(),
        "begin goto L; L: OutText(\"ok\"); OutImage; end;",
        "external \"foo\";",
    ];
    for (index, source) in corpus.iter().enumerate() {
        let source: &str = source;
        let result = std::panic::catch_unwind(|| {
            let _ = outimage::lex::tokenize(&SourceFile::anonymous(source));
            let _ = outimage::compile_str(source);
            let _ = outimage::compile_with_options(
                &SourceFile::anonymous(source),
                &outimage::CompileOptions::for_compile(
                    temp_path(&format!("lex{index}")),
                    outimage::CompileTarget::Native,
                ),
            );
        });
        assert!(result.is_ok(), "corpus[{index}] panicked");
    }
}

#[test]
fn differential_small_programs_interp_eq_native() {
    let programs = [
        r#"begin integer n; n := 1 + 2 * 3; OutInt(n, 0); OutImage; end;"#,
        r#"begin boolean b; b := 1 < 2; if b then OutText("t"); OutImage; end;"#,
        r#"begin integer i, s; s := 0; for i := 1 step 1 until 4 do s := s + i; OutInt(s, 0); OutImage; end;"#,
        r#"begin integer i, n; n := 0; for i := 1 while n < 2 do n := n + 1; OutInt(n, 0); OutImage; end;"#,
        r#"begin text t; t :- "ab"; OutText(t); OutImage; end;"#,
        r#"begin
            class C; begin integer x; x := 5; end;
            ref(C) r; r :- new C; OutInt(r.x, 0); OutImage;
           end;"#,
    ];
    for (index, source) in programs.iter().enumerate() {
        let source: &str = source;
        let interpreted = outimage::compile_str(source)
            .unwrap_or_else(|error| panic!("interp[{index}] failed: {error}"));
        let output_path = temp_path(&format!("n{index}"));
        let artifact = match outimage::compile_with_options(
            &SourceFile::anonymous(source),
            &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
        )
        .unwrap_or_else(|error| panic!("native compile[{index}] failed: {error}"))
        {
            outimage::CompileResult::Artifact(path) => path,
            outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
                panic!("expected native artifact")
            }
        };
        let result = std::process::Command::new(&artifact)
            .output()
            .unwrap_or_else(|error| panic!("native run[{index}] failed: {error}"));
        let _ = std::fs::remove_file(&artifact);
        assert!(
            result.status.success(),
            "native[{index}] exited {:?}; stderr={}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
        let native = String::from_utf8_lossy(&result.stdout);
        assert_eq!(
            native, interpreted,
            "program[{index}] diverge\nnative={native:?}\ninterp={interpreted:?}"
        );
    }
}
