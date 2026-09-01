//! Golden diagnostics: rendered reports are the product surface.

use outimage::source::SourceFile;
use outimage::{CompileError, compile};

fn render_err(source: &str) -> String {
    let file = SourceFile::anonymous(source);
    let error: CompileError = compile(&file).expect_err("expected a compile error");
    let mut rendered = error.render(&file);
    // Related siblings are included by render().
    rendered.retain(|ch| ch != '\r');
    rendered
}

fn assert_report(source: &str, needles: &[&str]) -> String {
    let rendered = render_err(source);
    for needle in needles {
        assert!(
            rendered.contains(needle),
            "missing `{needle}` in:\n{rendered}"
        );
    }
    rendered
}

#[test]
fn assign_boolean_to_integer() {
    let rendered = assert_report(
        "begin integer x; x := true; end;",
        &[
            "E0201",
            "TYPE MISMATCH",
            "boolean",
            "integer",
            "this is boolean",
        ],
    );
    assert!(
        !rendered.contains("TokenKind"),
        "internal dump leaked:\n{rendered}"
    );
}

#[test]
fn value_assign_to_ref() {
    assert_report(
        "begin class P; begin end; ref(P) p; p := none; end;",
        &["E0202", ":-", ":="],
    );
}

#[test]
fn if_branch_mentions_destination_type() {
    assert_report(
        "begin integer x; x := if true then 1 else true; end;",
        &["E0206", "`else` branch", "should be integer", "boolean"],
    );
}

#[test]
fn if_needs_boolean() {
    assert_report(
        "begin integer x; x := 1; if x then x := 0; end;",
        &["E0204", "if", "boolean", "integer"],
    );
}

#[test]
fn arithmetic_rejects_boolean() {
    assert_report(
        "begin integer x; x := 1 + true; end;",
        &["E0204", "`+`", "boolean"],
    );
}

#[test]
fn text_plus_suggests_ampersand() {
    assert_report(
        r#"begin text t; t := "a" + "b"; end;"#,
        &["E0204", "`&`", r#""a" & "b""#],
    );
}

#[test]
fn outtext_arity() {
    assert_report(
        r#"begin OutText("a", "b"); end;"#,
        &["E0205", "OutText", "1"],
    );
}

#[test]
fn unknown_procedure_typo() {
    assert_report(r#"begin Outtxt("hi"); end;"#, &["E0302", "Outtxt"]);
}

#[test]
fn unexpected_character_has_code() {
    assert_report("begin @@@", &["E0001", "UNEXPECTED CHARACTER"]);
}

#[test]
fn json_includes_title_and_code() {
    let file = SourceFile::anonymous("begin integer x; x := true; end;");
    let error = compile(&file).expect_err("type mismatch");
    let json = error.to_json_value();
    assert_eq!(json["code"], "E0201");
    assert_eq!(json["title"], "TYPE MISMATCH");
    assert_eq!(json["severity"], "error");
    assert!(
        json["params"]["found"]
            .as_str()
            .unwrap()
            .contains("boolean")
    );
}

#[test]
fn explain_e0201() {
    let text = outimage::diagnostics::explain("E0201").expect("explain");
    assert!(text.contains("E0201"));
    assert!(text.contains("TYPE MISMATCH"));
    assert!(text.to_ascii_lowercase().contains("value assignment"));
}

#[test]
fn incomplete_ref_prefix() {
    let file = SourceFile::anonymous("begin ref x; end;");
    let error = outimage::lex::tokenize(&file)
        .and_then(|tokens| outimage::parse::parse(&tokens))
        .expect_err("incomplete ref");
    let rendered = error.render(&file);
    assert!(
        rendered.contains("E0105") && rendered.contains("`ref`"),
        "{rendered}"
    );
}

#[test]
fn no_debug_tokenkind_in_parse_error() {
    let file = SourceFile::anonymous("begin integer");
    let error = outimage::lex::tokenize(&file)
        .and_then(|tokens| outimage::parse::parse(&tokens))
        .expect_err("parse should fail");
    let rendered = error.render(&file);
    assert!(
        !rendered.contains("TokenKind") && !rendered.contains("Keyword("),
        "parse error leaked Debug:\n{rendered}"
    );
}

#[test]
fn missing_token_separator_english() {
    let rendered = assert_report(r#""hello"world"#, &["E0003", "separator"]);
    assert!(
        !rendered.contains("TokenKind"),
        "internal dump leaked:\n{rendered}"
    );
}

#[test]
fn unused_binding_warning() {
    let file = SourceFile::anonymous("begin integer x; integer y; y := 1; end;");
    let options = outimage::CompileOptions::for_check();
    let warnings = outimage::unused_diagnostics(&file, &options);
    assert!(
        warnings
            .iter()
            .any(|w| w.report_code() == "W0001" && w.message.contains("`x`")),
        "{warnings:?}"
    );
    let rendered = warnings[0].render(&file);
    assert!(rendered.contains("W0001"), "{rendered}");
    assert!(rendered.contains("UNUSED"), "{rendered}");
    assert!(
        warnings[0].to_string().contains("warning"),
        "{}",
        warnings[0]
    );
}

#[test]
fn array_extent_is_runtime_not_codegen() {
    let file = SourceFile::anonymous(
        "begin integer array a(1:70000, 1:70000); OutText(\"x\"); OutImage; end;",
    );
    let error = compile(&file).expect_err("huge array should fail at runtime");
    assert_eq!(error.report_code(), "E0901");
    assert_eq!(error.phase, outimage::Phase::Runtime);
    let rendered = error.render(&file);
    assert!(rendered.contains("ARRAY TOO LARGE"), "{rendered}");
    assert!(
        !rendered.contains("codegen error"),
        "runtime failure labelled codegen:\n{rendered}"
    );
}

#[test]
fn undeclared_name_in_array_bound() {
    assert_report(
        "begin integer array a(1:n); end;",
        &["E0301", "UNKNOWN NAME", "`n`"],
    );
}

#[test]
fn undeclared_name_in_constant_initializer() {
    assert_report("begin integer k = n; end;", &["E0301", "`n`"]);
}

#[test]
fn duplicate_declaration_labels_second() {
    let rendered = assert_report(
        "begin integer i; integer i; end;",
        &["E0306", "already declared", "second declaration"],
    );
    assert!(
        rendered.contains("first declared") || rendered.contains("DUPLICATE"),
        "{rendered}"
    );
}

#[test]
fn missing_end_points_at_begin() {
    let file = SourceFile::anonymous("begin integer x; x := 1;");
    let error = outimage::lex::tokenize(&file)
        .and_then(|tokens| outimage::parse::parse(&tokens))
        .expect_err("missing end");
    let rendered = error.render(&file);
    assert!(
        rendered.contains("E0103") || rendered.contains("end"),
        "{rendered}"
    );
    assert!(!rendered.contains("TokenKind"), "{rendered}");
}

#[test]
fn unterminated_string() {
    assert_report("begin OutText(\"hi); end;", &["E0002"]);
}

#[test]
fn class_value_on_object_ref_is_illegal_mode() {
    assert_report(
        "begin class Point; begin end; class C(p); value p; ref(Point) p; begin end; end;",
        &["E0501", "value"],
    );
}

#[test]
fn ice_is_not_a_codegen_title() {
    let file = SourceFile::anonymous("begin end;");
    let error = CompileError::ice("local 12 out of range");
    let rendered = error.render(&file);
    assert!(rendered.contains("I0001"), "{rendered}");
    assert!(rendered.contains("INTERNAL ERROR"), "{rendered}");
    assert!(
        !rendered.contains("codegen error"),
        "ICE leaked codegen title:\n{rendered}"
    );
    assert!(
        rendered.contains("local 12") || rendered.contains("internal"),
        "{rendered}"
    );
}

#[test]
fn division_by_zero_is_runtime() {
    let file = SourceFile::anonymous("begin integer x; x := 1 // 0; OutInt(x, 1); OutImage; end;");
    let error = compile(&file).expect_err("div by zero");
    assert_eq!(error.report_code(), "E0905");
    assert_eq!(error.phase, outimage::Phase::Runtime);
    assert!(
        error
            .span
            .as_ref()
            .is_some_and(|span| span.start < span.end),
        "runtime diagnostic should underline the source: {:?}",
        error.span
    );
}

#[test]
fn undeclared_name_in_expression() {
    assert_report(
        "begin integer x; x := n; end;",
        &["E0301", "UNKNOWN NAME", "`n`"],
    );
}

#[test]
fn class_name_parameter_is_e0501() {
    assert_report(
        "begin class C(x); name x; integer x; begin end; end;",
        &["E0501"],
    );
}

#[test]
fn explain_title_search() {
    let text = outimage::diagnostics::explain("type-mismatch").expect("title search");
    assert!(
        text.contains("E0201") || text.contains("TYPE MISMATCH"),
        "{text}"
    );
}

#[test]
fn parse_if_reports_context() {
    let file = SourceFile::anonymous("begin if true x := 1; end;");
    let error = outimage::lex::tokenize(&file)
        .and_then(|tokens| outimage::parse::parse(&tokens))
        .expect_err("parse should fail");
    let rendered = error.render(&file);
    assert!(
        !rendered.contains("TokenKind") && !rendered.contains("Keyword("),
        "{rendered}"
    );
    assert!(
        rendered.to_ascii_lowercase().contains("if")
            || rendered.contains("E0101")
            || rendered.contains("UNEXPECTED"),
        "{rendered}"
    );
}

#[test]
fn lex_recovers_multiple_unexpected_characters() {
    let file = SourceFile::anonymous("begin integer x; x := @1; x := #2; end;");
    let error = outimage::lex::tokenize(&file).expect_err("stray characters");
    assert_eq!(error.report_code(), "E0001");
    assert!(
        error
            .related
            .iter()
            .any(|related| related.report_code() == "E0001"),
        "expected bundled extra lex errors, got related={:?}",
        error.related
    );
}

#[test]
fn file_goldens() {
    use std::fs;
    use std::path::PathBuf;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/diagnostics");
    let update = std::env::var("UPDATE_DIAGNOSTICS").as_deref() == Ok("1");
    let mut sims = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("sim") {
                out.push(path);
            }
        }
    }
    walk(&root, &mut sims);
    sims.sort();
    assert!(
        sims.len() >= 40,
        "plan calls for ≥40 golden files, found {}",
        sims.len()
    );

    let mut failures = Vec::new();
    for sim in &sims {
        let text = fs::read_to_string(sim)
            .unwrap()
            .replace('\r', "");
        let file = SourceFile {
            name: sim.file_name().unwrap().to_string_lossy().into_owned(),
            text,
        };
        let rendered = diagnose_file(&file);
        let stderr_path = sim.with_extension("stderr");
        if update {
            fs::write(&stderr_path, &rendered).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&stderr_path).unwrap_or_else(|_| {
            failures.push(format!(
                "{}: missing {}. Run UPDATE_DIAGNOSTICS=1",
                sim.display(),
                stderr_path.display()
            ));
            String::new()
        });
        if expected.replace('\r', "") != rendered {
            failures.push(format!(
                "{}:\n--- expected ---\n{expected}\n--- actual ---\n{rendered}",
                sim.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} golden failure(s):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

fn diagnose_file(file: &SourceFile) -> String {
    match compile(file) {
        Err(error) => {
            let mut rendered = error.render(file);
            rendered.retain(|ch| ch != '\r');
            rendered
        }
        Ok(_) => {
            let warnings =
                outimage::unused_diagnostics(file, &outimage::CompileOptions::for_check());
            assert!(
                !warnings.is_empty(),
                "{} compiled cleanly; expected a diagnostic or unused warning",
                file.name
            );
            let mut rendered = warnings
                .iter()
                .map(|warning| warning.render(file))
                .collect::<Vec<_>>()
                .join("");
            rendered.retain(|ch| ch != '\r');
            rendered
        }
    }
}

#[test]
fn missing_end_is_e0103() {
    let rendered = assert_report(
        "begin integer x; x := 1;",
        &["E0103", "MISSING END", "begin"],
    );
    assert!(!rendered.contains("E-parse: expected"), "{rendered}");
}

#[test]
fn check_reports_parse_and_type_errors_together() {
    let file = SourceFile::anonymous("begin integer x; x := true;");
    let error = outimage::compile_with_options(&file, &outimage::CompileOptions::for_check())
        .expect_err("missing end plus type mismatch");
    let rendered = error.render(&file);
    assert!(
        rendered.contains("E0103") && rendered.contains("E0201"),
        "check should surface both parse and type errors:\n{rendered}"
    );
}

#[test]
fn compact_is_one_line_with_code() {
    let file = SourceFile::anonymous("begin integer x; x := true; end;");
    let error = compile(&file).expect_err("type mismatch");
    let rendered = error.render_with_config(&file, &outimage::DiagnosticConfig::plain());
    assert!(rendered.contains("E0201"), "{rendered}");
    assert!(rendered.contains("TYPE MISMATCH"), "{rendered}");
    assert!(
        !rendered.contains("╭") && !rendered.contains("Help:"),
        "compact leaked a full report:\n{rendered}"
    );
}

#[test]
fn explain_errors_short_omits_helps() {
    let file = SourceFile::anonymous("begin integer x; x := true; end;");
    let error = compile(&file).expect_err("type mismatch");
    let short = error.render_with_config(
        &file,
        &outimage::DiagnosticConfig::colorless().with_explain(outimage::ExplainLevel::Short),
    );
    let full = error.render(&file);
    assert!(
        full.contains("Help:") || full.to_ascii_lowercase().contains("help"),
        "{full}"
    );
    assert!(
        !short.contains("Help:") && !short.contains("help:"),
        "short mode still has tutor text:\n{short}"
    );
    assert!(short.contains("E0201"), "{short}");
}

#[test]
fn codegen_mir_lowering_is_e0702() {
    let file = SourceFile::anonymous("begin end;");
    let error = CompileError::codegen("MIR lowering: 'activate' is not supported yet");
    assert_eq!(error.report_code(), "E0702");
    let rendered = error.render(&file);
    assert!(rendered.contains("NOT LOWERED"), "{rendered}");
    assert!(
        !rendered.contains("codegen error"),
        "lowering leaked codegen title:\n{rendered}"
    );
}

#[test]
fn codegen_mir_wasm_is_ice() {
    let file = SourceFile::anonymous("begin end;");
    let error = CompileError::codegen_at("MIR wasm: missing main function", 0..5);
    assert_eq!(error.report_code(), "I0001");
    let rendered = error.render(&file);
    assert!(rendered.contains("INTERNAL ERROR"), "{rendered}");
    assert!(!rendered.contains("codegen error"), "{rendered}");
}

#[test]
fn toolchain_write_failure_is_e0803() {
    let file = SourceFile::anonymous("begin end;");
    let error = CompileError::codegen("failed to write /tmp/out: permission denied");
    assert_eq!(error.report_code(), "E0803");
    let rendered = error.render(&file);
    assert!(rendered.contains("TOOLCHAIN"), "{rendered}");
}

#[test]
fn ref_prefix_diff_note() {
    let rendered = assert_report(
        "begin class Car; begin end; class Student; begin end; ref(Car) c; ref(Student) s; c :- s; end;",
        &["ref(Student)", "ref(Car)", "share no prefix"],
    );
    assert!(
        rendered.contains("E0201") || rendered.contains("TYPE MISMATCH"),
        "{rendered}"
    );
}

#[test]
fn hidden_requires_protected_is_single_e0409_with_spec_span() {
    let rendered = assert_report(
        "begin class C; hidden x; begin integer x; end; end;",
        &["E0409", "this `hidden` specification"],
    );
    assert_eq!(
        rendered.matches("[E0409]").count(),
        1,
        "concatenation must not duplicate E0409:\n{rendered}"
    );
}

#[test]
fn protected_access_labels_the_specification() {
    assert_report(
        "begin class C; protected x; begin integer x; end; ref(C) r; r :- new C; integer n; n := r.x; end;",
        &["E0401", "this access", "this `protected` specification"],
    );
}

#[test]
fn hidden_from_subclass_labels_the_hidden_specification() {
    assert_report(
        r#"begin
class a;
protected i;
begin integer i; end;
a class b;
hidden i;
begin end;
b class c;
begin
ref(b) r;
r :- new b;
OutInt(r.i, 2);
end;
end;"#,
        &["E0402", "this access", "this `hidden` specification"],
    );
}

#[test]
fn every_catalog_code_has_explain_page() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/diagnostics");
    for id in outimage::DiagId::ALL {
        let path = root.join(format!("{}.md", id.code()));
        assert!(
            path.is_file(),
            "missing explain page {} for {}",
            path.display(),
            id.code()
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(id.code()),
            "{} does not mention {}",
            path.display(),
            id.code()
        );
    }
}

#[test]
fn error_codes_doc_contains_generated_table() {
    let table = outimage::diagnostics::catalog_index_markdown();
    let doc = include_str!("../docs/ERROR_CODES.md").replace('\r', "");
    assert!(
        doc.contains(table.trim()),
        "docs/ERROR_CODES.md is stale; paste catalog_index_markdown() into the catalogued-codes table"
    );
}
