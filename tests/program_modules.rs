mod common;

use outimage::lex::tokenize;
use outimage::parse::parse;
use outimage::semantic::analyze;
use outimage::source::SourceFile;

fn parse_and_analyze(source: &str) -> Result<(), outimage::CompileError> {
    let stream = tokenize(&SourceFile::anonymous(source))?;
    let program = parse(&stream)?;
    analyze(&program)
}

#[test]
fn parses_module_external_head() {
    let stream = tokenize(&SourceFile::anonymous("external class b, c; begin end;")).unwrap();
    let program = parse(&stream).unwrap();
    assert_eq!(program.external_head.len(), 1);
}

#[test]
fn semantic_accepts_external_class_in_block() {
    parse_and_analyze("begin external class Node; ref(Node) r; end;").unwrap();
}

#[test]
fn semantic_accepts_external_procedure_list() {
    parse_and_analyze("begin external procedure aproc; end;").unwrap();
}

#[test]
fn semantic_accepts_external_procedure_with_specification() {
    parse_and_analyze(
        "external procedure OutText is procedure OutText(text value); begin end; begin end;",
    )
    .unwrap();
}

#[test]
fn semantic_accepts_stdlib_external_shorthand() {
    parse_and_analyze("procedure OutText(text value); external;").unwrap();
    parse_and_analyze("procedure OutImage; external;").unwrap();
}

#[test]
fn semantic_accepts_single_statement_main_program() {
    parse_and_analyze(r#"OutText("hello");"#).unwrap();
}

#[test]
fn semantic_accepts_standalone_procedure_module() {
    parse_and_analyze("procedure helper; begin OutImage; end;").unwrap();
}

#[test]
fn semantic_accepts_standalone_class_module() {
    parse_and_analyze("class Node; begin integer x; end Node;").unwrap();
}

#[test]
fn semantic_accepts_prefixed_class_with_external_head() {
    parse_and_analyze(
        "external class b, c; b class e(f); ref(c) f; begin external class d; external procedure aproc; ref(d) dref; end class e;",
    )
    .unwrap();
}

#[test]
fn semantic_rejects_prefixed_class_module_without_external_head() {
    let err = parse_and_analyze("b class e; begin end class e;").unwrap_err();
    assert!(
        err.to_string().contains("not local") || err.to_string().contains("external"),
        "unexpected error: {err}"
    );
}

#[test]
fn semantic_accepts_external_identification() {
    parse_and_analyze(r#"external procedure foo = "libfoo"; begin end;"#).unwrap();
}

#[test]
fn records_external_identification_metadata() {
    let stream = tokenize(&SourceFile::anonymous(
        r#"external procedure foo = "libfoo", bar = "libbar"; begin end;"#,
    ))
    .unwrap();
    let program = parse(&stream).unwrap();
    let ids = outimage::semantic::external_identifications(&program);
    assert_eq!(ids.get("foo").map(String::as_str), Some("libfoo"));
    assert_eq!(ids.get("bar").map(String::as_str), Some("libbar"));
}

#[test]
fn external_spec_does_not_consume_the_program_block() {
    parse_and_analyze("external procedure p is procedure p; begin integer x; x := 1; end;")
        .unwrap();
}

#[test]
fn parses_typed_external_procedure_shorthand() {
    let stream = tokenize(&SourceFile::anonymous(
        "ref(File) procedure open(text path, text mode); external;",
    ))
    .unwrap();
    let program = parse(&stream).unwrap();
    let proc = &program.blocks[0].procedures[0];
    assert!(proc.is_external);
    assert_eq!(proc.name, "open");
}

#[test]
fn fixture_rejects_non_simula_kind_as_formal_actual() {
    let source = common::fixture("modules/external_kind_formal.sim");
    let err = parse_and_analyze(&source).unwrap_err();
    assert!(
        err.to_string().contains("non-Simula"),
        "unexpected error: {err}"
    );
}

#[test]
fn external_outtext_alias_runs_on_interpreter_and_native() {
    let source = r#"begin
        procedure OutText(t); text t; external;
        OutText("hi");
        OutImage;
    end;"#;
    parse_and_analyze(source).unwrap();
    let interpreted = outimage::compile_str(source).unwrap();
    assert_eq!(interpreted, "hi\n");

    let output = std::env::temp_dir().join(format!(
        "sim-ext-outtext-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let artifact = match outimage::compile_with_options(
        &SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output.clone(), outimage::CompileTarget::Native),
    )
    .unwrap()
    {
        outimage::CompileResult::Artifact(path) => path,
        _ => panic!("expected native artifact"),
    };
    let result = std::process::Command::new(&artifact).output().unwrap();
    let _ = std::fs::remove_file(&artifact);
    assert!(result.status.success());
    assert_eq!(String::from_utf8_lossy(&result.stdout), "hi\n");
}

const UNSUPPLIED_EXTERNAL: &str = r#"begin
    procedure mystery; external;
    mystery;
end;"#;

#[test]
fn external_procedure_without_body_passes_check() {
    // Chapter 6: an external procedure is supplied by a separately compiled
    // module, so checking this module on its own must succeed — the corpus
    // units combined from several sources (simtst40 / simtst41 / simtst59)
    // rely on the real body replacing the stub once compiled together.
    parse_and_analyze(UNSUPPLIED_EXTERNAL).unwrap();
    match outimage::compile_with_options(
        &SourceFile::anonymous(UNSUPPLIED_EXTERNAL),
        &outimage::CompileOptions::for_check(),
    )
    .expect("checking one module in isolation cannot see the other module")
    {
        outimage::CompileResult::Checked => {}
        other => panic!("expected a check result, got {other:?}"),
    }
}

#[test]
fn external_procedure_without_body_cannot_produce_an_artifact() {
    // Chapter 6 makes an external declaration "a substitute for a complete
    // introduction of the corresponding source module". With no module to
    // substitute, emitting a silently-empty procedure would be wrong, so the
    // artifact path refuses.
    let output = std::env::temp_dir().join("sim-mystery-compile");
    let error = outimage::compile_with_options(
        &SourceFile::anonymous(UNSUPPLIED_EXTERNAL),
        &outimage::CompileOptions::for_compile(output, outimage::CompileTarget::Native),
    )
    .expect_err("an unsupplied external body cannot be linked");
    let message = error.to_string();
    assert!(
        message.contains("mystery") && message.contains("no compiled module supplies its body"),
        "unexpected diagnostic: {message}"
    );
}
