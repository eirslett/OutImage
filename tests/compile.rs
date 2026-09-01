mod common;

#[test]
fn compiles_empty_source() {
    assert!(outimage::compile_str("").is_ok());
}

#[test]
fn compiles_empty_fixture() {
    let source = common::fixture("empty.sim");
    assert!(outimage::compile_str(&source).is_ok());
}

#[test]
fn compiles_begin_end_fixture() {
    let source = common::fixture("begin_end.sim");
    assert!(outimage::compile_str(&source).is_ok());
}

#[test]
fn compiles_nested_end_comment_fixture() {
    let source = common::fixture("end-comment/nested_end_comment_if.sim");
    assert!(outimage::compile_str(&source).is_ok());
}

#[test]
fn reports_semantic_errors_with_source_context() {
    let source = outimage::source::SourceFile::anonymous("begin integer x; x := true; end;");
    let error = outimage::compile(&source).expect_err("type mismatch should fail");
    assert!(error.span.is_some(), "semantic error should carry a span");
    let rendered = error.render(&source);
    assert!(
        rendered.contains("TYPE MISMATCH") || rendered.contains("boolean"),
        "report: {rendered}"
    );
    assert!(
        rendered.contains("boolean"),
        "report should mention the offending type: {rendered}"
    );
    assert!(
        rendered.contains("x := true") || rendered.contains(":="),
        "report should include a source snippet: {rendered}"
    );
    assert!(
        rendered.contains("this is boolean") || rendered.contains("expects integer"),
        "report should include enriched type labels/notes: {rendered}"
    );
    assert!(
        rendered.contains("E0201") || rendered.contains("E-semantic"),
        "report should include a stable diagnostic code: {rendered}"
    );
}

#[test]
fn reports_outtext_arity_error_with_span() {
    let source = outimage::source::SourceFile::anonymous(r#"begin OutText("a", "b"); end;"#);
    let error = outimage::compile(&source).expect_err("OutText arity should fail");
    assert!(error.span.is_some(), "arity error should carry a span");
    assert!(
        error.message.contains("OutText") || error.message.contains("argument"),
        "message: {}",
        error.message
    );
}

#[test]
fn reports_arithmetic_type_error_with_span() {
    let source = outimage::source::SourceFile::anonymous(r#"begin integer x; x := 1 + true; end;"#);
    let error = outimage::compile(&source).expect_err("non-arithmetic operand should fail");
    assert!(error.span.is_some(), "arithmetic error should carry a span");
    assert!(
        error.message.contains("arithmetic") || error.message.contains("`+`"),
        "message: {}",
        error.message
    );
}

#[test]
fn reports_duplicate_declaration_with_span() {
    let source = outimage::source::SourceFile::anonymous("begin integer x; integer x; end;");
    let error = outimage::compile(&source).expect_err("duplicate decl should fail");
    assert!(
        error.span.is_some(),
        "duplicate declaration should carry a span"
    );
    assert!(
        error.message.contains("duplicate") || error.message.contains("already declared"),
        "message: {}",
        error.message
    );
}

#[test]
fn declaration_and_procedure_nodes_carry_nonempty_spans() {
    let stream = outimage::lex::tokenize(&outimage::source::SourceFile::anonymous(
        "begin integer x; procedure p; begin end; end;",
    ))
    .unwrap();
    let program = outimage::parse::parse(&stream).unwrap();
    let block = &program.blocks[0];
    assert!(
        !block.declarations[0].span.is_empty(),
        "declaration span should be non-empty"
    );
    assert!(
        !block.procedures[0].span.is_empty(),
        "procedure span should be non-empty"
    );
}

#[test]
fn emit_obj_writes_object_file_without_linking() {
    let dir = std::env::temp_dir().join(format!("sim-emit-obj-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let object_path = dir.join("hello.o");
    let source = outimage::source::SourceFile::anonymous(r#"begin OutText("hi"); OutImage; end;"#);
    let mut options =
        outimage::CompileOptions::for_compile(object_path.clone(), outimage::CompileTarget::Native);
    options.emit_obj = true;
    match outimage::compile_with_options(&source, &options)
        .expect("emit-obj compile should succeed")
    {
        outimage::CompileResult::Artifact(path) => {
            assert_eq!(path, object_path);
            let bytes = std::fs::read(&path).expect("object file should exist");
            assert!(!bytes.is_empty(), "object file should be non-empty");
            assert!(
                bytes.starts_with(b"\xcf\xfa\xed\xfe") // Mach-O 64 LE
                    || bytes.starts_with(b"\x7fELF")
                    || bytes.starts_with(b"\xfe\xed\xfa\xce")
                    || bytes.starts_with(b"\xfe\xed\xfa\xcf"),
                "unexpected object magic: {:02x?}",
                &bytes[..bytes.len().min(4)]
            );
        }
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected artifact")
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_asm_writes_disassembly() {
    let dir = std::env::temp_dir().join(format!("sim-emit-asm-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let object_path = dir.join("hello.o");
    let asm_path = dir.join("hello.s");
    let source = outimage::source::SourceFile::anonymous(r#"begin OutText("hi"); OutImage; end;"#);
    let mut options =
        outimage::CompileOptions::for_compile(object_path.clone(), outimage::CompileTarget::Native);
    options.emit_obj = true;
    options.emit_asm = true;
    match outimage::compile_with_options(&source, &options)
        .expect("emit-asm compile should succeed")
    {
        outimage::CompileResult::Artifact(_) => {}
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected artifact")
        }
    }
    let asm = std::fs::read_to_string(&asm_path).expect("asm file should exist");
    assert!(
        asm.contains("sim_main"),
        "expected main symbol in asm, got:\n{asm}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
