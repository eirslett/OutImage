use std::path::PathBuf;

use outimage::lex::tokenize;
use outimage::mir::lower_program_with_source;
use outimage::parse::parse;
use outimage::semantic::analyze;
use outimage::source::SourceFile;
use outimage::{Interpreter, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("model.sim");
    let source = SourceFile::from_path(&path)?;
    let tokens = tokenize(&source)?;
    let program = parse(&tokens)?;
    analyze(&program)?;
    let module = lower_program_with_source(&program, &source.text)?;

    let mut vm = Interpreter::from_module(&module);
    vm.define_host("plot", |_ctx, args| {
        let x = args[0].as_f64()?;
        let y = args[1].as_f64()?;
        println!("plot({x:.1}, {y:.1})");
        Ok(Value::None)
    });

    let hypot = vm
        .call("hypot", &[Value::F64(3.0), Value::F64(4.0)])?
        .and_then(|value| value.as_f64().ok())
        .expect("hypot result");
    println!("hypot(3, 4) = {hypot:.0}");

    vm.call("tick", &[Value::F64(10.0), Value::F64(20.0)])?;
    Ok(())
}
