//! BASICIO Chapter 10: SysOut/SysIn images, Standard file APIs.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NATIVE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_output_path(tag: &str) -> PathBuf {
    let id = NATIVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-basicio-native-{tag}-{id}"))
}

fn run_native(source: &str) -> String {
    let output_path = temp_output_path("bin");
    let artifact = match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(output_path, outimage::CompileTarget::Native),
    )
    .unwrap_or_else(|error| panic!("native compile failed: {error}"))
    {
        outimage::CompileResult::Artifact(path) => path,
        _ => panic!("expected native artifact"),
    };
    let result = std::process::Command::new(&artifact)
        .output()
        .unwrap_or_else(|error| panic!("native binary failed to run: {error}"));
    let _ = std::fs::remove_file(&artifact);
    assert!(
        result.status.success(),
        "native binary exited {:?}; stderr: {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

#[test]
fn outchar_and_break_out_image_interpreter() {
    let output = outimage::compile_str(
        r#"begin
            OutChar('H');
            OutChar('i');
            BreakOutImage;
            OutText("there");
            OutImage;
        end;"#,
    )
    .expect("OutChar program");
    assert_eq!(output, "Hi\nthere\n");
}

#[test]
fn outchar_break_fixture() {
    let source = common::fixture("basicio/outchar_break.sim");
    let output = outimage::compile_str(&source).expect("fixture");
    assert_eq!(output, "Hi\nthere\n");
}

#[test]
fn outchar_native_parity() {
    let source = r#"begin
        OutChar('A');
        OutChar('B');
        BreakOutImage;
    end;"#;
    let interpreted = outimage::compile_str(source).expect("interp");
    let native = run_native(source);
    assert_eq!(native, interpreted);
    assert_eq!(native, "AB\n");
}

#[test]
fn outfile_infile_round_trip_interpreter() {
    let path = common::temp_path("basicio_file_rt.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutFile) outf;
            ref (InFile) inf;
            character c;
            outf :- new OutFile("{path_lit}");
            if outf.open(blanks(80)) then begin
                outf.outtext("xy");
                outf.outimage;
                outf.close;
            end;
            inf :- new InFile("{path_lit}");
            if inf.open(blanks(80)) then begin
                inf.inimage;
                c := inf.inchar;
                OutChar(c);
                c := inf.inchar;
                OutChar(c);
                BreakOutImage;
                if inf.endfile then OutText("eof-early") else OutText("ok");
                OutImage;
                inf.inimage;
                if inf.endfile then OutText("eof") else OutText("more");
                OutImage;
                inf.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("file round-trip");
    assert_eq!(output, "xy\nok\neof\n");
    let written = std::fs::read_to_string(&path).expect("file");
    assert!(written.starts_with("xy"), "written was {written:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn outfile_infile_native_round_trip() {
    let path = common::temp_path("basicio_file_native_rt.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutFile) outf;
            ref (InFile) inf;
            outf :- new OutFile("{path_lit}");
            if outf.open(blanks(8)) then begin
                outf.outtext("Z");
                outf.outimage;
                outf.close;
            end;
            inf :- new InFile("{path_lit}");
            if inf.open(blanks(8)) then begin
                inf.inimage;
                OutChar(inf.inchar);
                BreakOutImage;
                inf.close;
            end;
        end;"#
    );
    let interpreted = outimage::compile_str(&source).expect("interp file");
    let native = run_native(&source);
    assert_eq!(native, interpreted);
    assert_eq!(native, "Z\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_returns_false_for_missing_infile() {
    let output = outimage::compile_str(
        r#"begin
            ref (InFile) inf;
            inf :- new InFile("/no/such/sim-basicio-missing-file");
            if inf.open(blanks(10)) then OutText("opened") else OutText("failed");
            OutImage;
        end;"#,
    )
    .expect("missing open");
    assert_eq!(output, "failed\n");
}

#[test]
fn close_flushes_partial_outfile_image() {
    let path = common::temp_path("basicio_close_flush.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutFile) outf;
            outf :- new OutFile("{path_lit}");
            if outf.open(blanks(20)) then begin
                outf.outtext("hi");
                outf.close;
            end;
        end;"#
    );
    let _ = outimage::compile_str(&source).expect("close flush");
    let written = std::fs::read_to_string(&path).expect("file");
    assert!(written.starts_with("hi"), "written was {written:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn setaccess_append_mode() {
    let path = common::temp_path("basicio_append.txt");
    let _ = std::fs::remove_file(&path);
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutFile) outf;
            boolean ok;
            outf :- new OutFile("{path_lit}");
            ok := outf.setaccess("append");
            if not ok then begin OutText("bad-mode"); OutImage; end;
            if outf.open(blanks(20)) then begin
                outf.outtext("A");
                outf.outimage;
                outf.close;
            end;
            outf :- new OutFile("{path_lit}");
            outf.setaccess("append");
            if outf.open(blanks(20)) then begin
                outf.outtext("B");
                outf.outimage;
                outf.close;
            end;
        end;"#
    );
    let _ = outimage::compile_str(&source).expect("append");
    let written = std::fs::read_to_string(&path).expect("file");
    assert!(
        written.contains('A') && written.contains('B'),
        "written was {written:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn sysout_sysin_accessors_interpreter() {
    let output = outimage::compile_str(
        r#"begin
            ref (PrintFile) so;
            so :- sysout;
            so.outtext("via");
            so.outimage;
            if so.isopen then OutText("open") else OutText("closed");
            OutImage;
        end;"#,
    )
    .expect("sysout");
    assert_eq!(output, "via\nopen\n");
}

#[test]
fn existing_outtext_outimage_unchanged() {
    let output = outimage::compile_str(
        r#"begin
            OutText("hello");
            OutImage;
        end;"#,
    )
    .expect("compat");
    assert_eq!(output, "hello\n");
}

#[test]
fn filename_procedure_returns_ctor_name() {
    let output = outimage::compile_str(
        r#"begin
            ref (OutFile) outf;
            text t;
            outf :- new OutFile("demo.dat");
            t :- outf.filename;
            OutText(t);
            OutImage;
        end;"#,
    )
    .expect("filename");
    assert_eq!(output, "demo.dat\n");
}

#[test]
fn outfile_outint_field_widths() {
    let path = common::temp_path("basicio_outint.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutFile) outf;
            outf :- new OutFile("{path_lit}");
            if outf.open(blanks(20)) then begin
                outf.outint(42, 5);
                outf.outimage;
                outf.close;
            end;
        end;"#
    );
    let _ = outimage::compile_str(&source).expect("outint");
    let written = std::fs::read_to_string(&path).expect("file");
    assert!(written.contains("   42"), "written was {written:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn infile_inint_item_oriented() {
    let path = common::temp_path("basicio_inint.txt");
    std::fs::write(&path, "  17 23\n").expect("write");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (InFile) inf;
            integer a, b;
            inf :- new InFile("{path_lit}");
            if inf.open(blanks(40)) then begin
                inf.inimage;
                a := inf.inint;
                b := inf.inint;
                OutInt(a, 0); OutText(" "); OutInt(b, 0); OutImage;
                inf.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("inint");
    assert_eq!(output, "17 23\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn printfile_eject_and_line() {
    let path = common::temp_path("basicio_print.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (PrintFile) pf;
            integer ln;
            pf :- new PrintFile("{path_lit}");
            if pf.open(blanks(40)) then begin
                pf.linesperpage(10);
                pf.outtext("L1");
                pf.outimage;
                ln := pf.line;
                OutInt(ln, 0); OutImage;
                pf.eject(5);
                ln := pf.line;
                OutInt(ln, 0); OutImage;
                pf.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("printfile");
    assert_eq!(output, "2\n5\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn directfile_locate_outimage_inimage() {
    let path = common::temp_path("basicio_direct.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (DirectFile) df;
            character c;
            df :- new DirectFile("{path_lit}");
            if df.open(blanks(8)) then begin
                df.outtext("AB");
                df.outimage;
                df.locate(1);
                df.inimage;
                c := df.inchar;
                OutChar(c);
                BreakOutImage;
                df.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("directfile");
    assert_eq!(output, "A\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bytefile_round_trip() {
    let path = common::temp_path("basicio_bytes.bin");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutByteFile) outb;
            ref (InByteFile) inb;
            short integer b;
            outb :- new OutByteFile("{path_lit}");
            if outb.open then begin
                outb.outbyte(65);
                outb.outbyte(66);
                outb.close;
            end;
            inb :- new InByteFile("{path_lit}");
            if inb.open then begin
                b := inb.inbyte;
                OutInt(b, 0); OutImage;
                b := inb.inbyte;
                OutInt(b, 0); OutImage;
                inb.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("bytefile");
    assert_eq!(output, "65\n66\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn terminate_program_stops_execution() {
    let output = outimage::compile_str(
        r#"begin
            OutText("before");
            OutImage;
            terminate_program;
            OutText("after");
            OutImage;
        end;"#,
    )
    .expect("terminate");
    assert_eq!(output, "before\n");
}

#[test]
fn free_outint_two_arg_width() {
    let output = outimage::compile_str(
        r#"begin
            OutInt(7, 4);
            OutImage;
        end;"#,
    )
    .expect("OutInt(i,w)");
    assert_eq!(output, "   7\n");
}

#[test]
fn free_outint_requires_width_argument() {
    let err = outimage::compile_str(
        r#"begin
            OutInt(7);
            OutImage;
        end;"#,
    )
    .expect_err("1-arg OutInt must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("2 arguments") || msg.contains("expects 2"),
        "unexpected error: {msg}"
    );
}

#[test]
fn infile_inrecord_and_lastitem() {
    let path = common::temp_path("basicio_inrecord.txt");
    std::fs::write(&path, "hello world\n").expect("write");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (InFile) inf;
            boolean trunc;
            boolean last;
            character c;
            inf :- new InFile("{path_lit}");
            if inf.open(blanks(5)) then begin
                trunc := inf.inrecord;
                if trunc then OutText("trunc") else OutText("full");
                OutImage;
                inf.close;
            end;
            inf :- new InFile("{path_lit}");
            if inf.open(blanks(40)) then begin
                last := inf.lastitem;
                if last then OutText("empty") else begin
                    c := inf.inchar;
                    OutChar(c);
                    BreakOutImage;
                end;
                inf.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("inrecord/lastitem");
    assert_eq!(output, "trunc\nh\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn outfile_outfix_and_create_mode() {
    let path = common::temp_path("basicio_outfix.txt");
    // CREATE must see a missing file on the first open; prior failed runs can
    // leave an empty path at the same counter id.
    let _ = std::fs::remove_file(&path);
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutFile) outf;
            boolean ok;
            outf :- new OutFile("{path_lit}");
            outf.setaccess("create");
            ok := outf.open(blanks(20));
            if ok then begin
                outf.outfix(3.14, 2, 8);
                outf.outimage;
                outf.close;
            end else begin
                OutText("open1-fail");
                OutImage;
            end;
            outf :- new OutFile("{path_lit}");
            outf.setaccess("create");
            if outf.open(blanks(20)) then OutText("recreate-ok") else OutText("recreate-fail");
            OutImage;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("outfix/create");
    assert!(
        !output.contains("open1-fail"),
        "first CREATE open should succeed: {output:?}"
    );
    assert!(output.contains("recreate-fail"), "output was {output:?}");
    let written = std::fs::read_to_string(&path).expect("file");
    assert!(
        written.contains('3') && written.contains('1'),
        "written was {written:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn directfile_create_mode_ignores_existing_file() {
    // DosTestBatch simtst81/simtst85: direct files are read+write and created
    // on demand, so `setaccess("CREATE")` must not make `open` fail when the
    // external file already exists (matches the native and wasm runtimes).
    let path = common::temp_path("basicio_direct_create.txt");
    std::fs::write(&path, "old record\n").expect("seed");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (DirectFile) df;
            df :- new DirectFile("{path_lit}");
            df.setaccess("create");
            if df.open(blanks(12)) then begin
                OutInt(df.location, 2);
                df.close;
            end else OutText("open-fail");
            OutImage;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("directfile create");
    assert_eq!(output, " 1\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn directbytefile_locate_round_trip() {
    let path = common::temp_path("basicio_directbyte.bin");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (DirectByteFile) df;
            short integer b;
            df :- new DirectByteFile("{path_lit}");
            if df.open then begin
                df.outbyte(90);
                df.outbyte(91);
                df.locate(1);
                b := df.inbyte;
                OutInt(b, 0); OutImage;
                b := df.inbyte;
                OutInt(b, 0); OutImage;
                df.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("directbyte");
    assert_eq!(output, "90\n91\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn directbytefile_parameterless_open_close_native() {
    // DosTestBatch simtst81: DirectByteFile.open (0-arg) must not abort with
    // "image file requires fileimage" on the native runtime path.
    let path = common::temp_path("basicio_directbyte_open.bin");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (File) xf;
            xf :- new DirectByteFile("{path_lit}");
            xf.SetAccess("CREATE");
            if xf qua DirectByteFile.Close then OutText("bad-close0")
            else if not xf qua DirectByteFile.Open then OutText("bad-open")
            else if xf qua DirectByteFile.Open then OutText("bad-reopen")
            else if not xf qua DirectByteFile.Close then OutText("bad-close1")
            else if xf qua DirectByteFile.Close then OutText("bad-close2")
            else OutText("ok");
            OutImage;
        end;"#
    );
    let out = run_native(&source);
    assert_eq!(out, "ok\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn infile_em_on_eof_endfile() {
    let path = common::temp_path("basicio_eof_em.txt");
    std::fs::write(&path, "X\n").expect("write");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (InFile) inf;
            character c;
            inf :- new InFile("{path_lit}");
            if inf.open(blanks(10)) then begin
                inf.inimage;
                c := inf.inchar;
                OutChar(c); BreakOutImage;
                inf.inimage;
                if inf.endfile then OutText("eof") else OutText("more");
                OutImage;
                inf.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&source).expect("eof em");
    assert_eq!(output, "X\neof\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bytefile_native_round_trip() {
    let path = common::temp_path("basicio_bytes_native.bin");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (OutByteFile) outb;
            ref (InByteFile) inb;
            short integer b;
            outb :- new OutByteFile("{path_lit}");
            if outb.open then begin
                outb.outbyte(65);
                outb.outbyte(66);
                outb.close;
            end;
            inb :- new InByteFile("{path_lit}");
            if inb.open then begin
                b := inb.inbyte;
                OutInt(b, 0); OutImage;
                b := inb.inbyte;
                OutInt(b, 0); OutImage;
                inb.close;
            end;
        end;"#
    );
    let interpreted = outimage::compile_str(&source).expect("interp byte");
    let native = run_native(&source);
    assert_eq!(native, interpreted);
    assert_eq!(native, "65\n66\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn terminate_program_native_exits_cleanly() {
    let source = r#"begin
        OutText("bye");
        OutImage;
        terminate_program;
        OutText("unreachable");
        OutImage;
    end;"#;
    let interpreted = outimage::compile_str(source).expect("interp terminate");
    assert_eq!(interpreted, "bye\n");
    let native = run_native(source);
    assert_eq!(native, "bye\n");
}

#[test]
fn sysout_has_printfile_line_after_open_embedding() {
    let output = outimage::compile_str(
        r#"begin
            ref (PrintFile) so;
            integer ln;
            so :- sysout;
            ln := so.line;
            OutInt(ln, 0); OutImage;
        end;"#,
    )
    .expect("sysout line");
    assert_eq!(output, "1\n");
}

#[test]
fn free_line_and_eject_via_basicio_embedding() {
    // §10: program acts as `inspect SYSIN do inspect SYSOUT do …`
    // so free `line` / `eject` resolve to SYSOUT PrintFile attributes.
    let output = outimage::compile_str(
        r#"begin
            integer ln;
            ln := line;
            OutInt(ln, 0); OutImage;
            eject(3);
            ln := line;
            OutInt(ln, 0); OutImage;
        end;"#,
    )
    .expect("free line/eject");
    assert_eq!(output, "1\n3\n");
}

#[test]
fn free_line_shadowed_by_local_declaration() {
    let output = outimage::compile_str(
        r#"begin
            integer line;
            line := 42;
            OutInt(line, 0); OutImage;
        end;"#,
    )
    .expect("shadowed line");
    assert_eq!(output, "42\n");
}

#[test]
fn directfile_native_parity() {
    let path = common::temp_path("basicio_direct_native.txt");
    let path_lit = path.replace('\\', "\\\\");
    let source = format!(
        r#"begin
            ref (DirectFile) df;
            character c;
            integer last;
            df :- new DirectFile("{path_lit}");
            if df.open(blanks(8)) then begin
                df.outtext("AB");
                df.outimage;
                last := df.lastloc;
                OutInt(last, 0); OutImage;
                df.locate(1);
                df.inimage;
                c := df.inchar;
                OutChar(c);
                BreakOutImage;
                df.close;
            end;
        end;"#
    );
    let interpreted = outimage::compile_str(&source).expect("directfile interp");
    assert_eq!(interpreted, "1\nA\n");
    let native = run_native(&source);
    assert_eq!(native, interpreted);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn directfile_locate_round_trip_persist() {
    let path = common::temp_path("basicio_direct_persist.txt");
    let path_lit = path.replace('\\', "\\\\");
    let write = format!(
        r#"begin
            ref (DirectFile) df;
            df :- new DirectFile("{path_lit}");
            if df.open(blanks(4)) then begin
                df.locate(2);
                df.outtext("XY");
                df.outimage;
                df.close;
            end;
        end;"#
    );
    let _ = outimage::compile_str(&write).expect("direct write");
    let read = format!(
        r#"begin
            ref (DirectFile) df;
            character c;
            df :- new DirectFile("{path_lit}");
            if df.open(blanks(4)) then begin
                df.locate(2);
                df.inimage;
                c := df.inchar;
                OutChar(c); OutChar(df.inchar);
                BreakOutImage;
                df.close;
            end;
        end;"#
    );
    let output = outimage::compile_str(&read).expect("direct read");
    assert_eq!(output, "XY\n");
    let _ = std::fs::remove_file(&path);
}
