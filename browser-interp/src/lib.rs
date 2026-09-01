//! wasm32 façade: compile Simula and interpret it with streaming stdio.
//!
//! Build with `cargo browser-interp` (see `.cargo/config.toml`). Artifacts land
//! in `target/outimage-browser-interp/` for the website to import.

use std::cell::RefCell;

use js_sys::Function;
use outimage::error::CompileError;
use outimage::lex::LexOptions;
use outimage::mir::interp::{InterpretPoll, Interpreter};
use outimage::runtime::{IoHost, ReadLine, StdinRecord};
use outimage::source::SourceFile;
use outimage::{lex, mir, parse, semantic};
use wasm_bindgen::prelude::*;

struct CallbackHost {
    on_stdout: Function,
    on_stderr: Function,
}

impl IoHost for CallbackHost {
    fn write_stdout(&mut self, text: &str) {
        let _ = self
            .on_stdout
            .call1(&JsValue::NULL, &JsValue::from_str(text));
    }

    fn write_stderr(&mut self, text: &str) {
        let _ = self
            .on_stderr
            .call1(&JsValue::NULL, &JsValue::from_str(text));
    }

    fn read_line(&mut self) -> Result<ReadLine, String> {
        Ok(ReadLine::NeedStdin)
    }
}

struct Running {
    /// `interp` borrows `module`; dropped first (field order).
    interp: Interpreter<'static>,
    _module: Box<outimage::mir::Module>,
}

impl Running {
    fn new(module: outimage::mir::Module, host: Box<dyn IoHost>) -> Self {
        let module = Box::new(module);
        // SAFETY: `interp` is declared before `_module`, so it is dropped first.
        // The `'static` borrow never outlives the boxed module.
        let module_ref: &'static outimage::mir::Module =
            unsafe { &*(&*module as *const outimage::mir::Module) };
        Self {
            interp: Interpreter::new(module_ref, host),
            _module: module,
        }
    }
}

/// One playground process: compile, stream stdio, exit once.
#[wasm_bindgen]
pub struct Session {
    on_stdout: Function,
    on_stderr: Function,
    on_exit: Function,
    running: RefCell<Option<Running>>,
    exited: RefCell<bool>,
    source: RefCell<String>,
}

#[wasm_bindgen]
impl Session {
    #[wasm_bindgen(constructor)]
    pub fn new(on_stdout: Function, on_stderr: Function, on_exit: Function) -> Session {
        console_error_panic_hook::set_once();
        Session {
            on_stdout,
            on_stderr,
            on_exit,
            running: RefCell::new(None),
            exited: RefCell::new(false),
            source: RefCell::new(String::new()),
        }
    }

    /// Compile `source` and start interpreting. Compile errors go to stderr
    /// and `on_exit(1)` without running.
    pub fn start(&self, source: &str) {
        *self.exited.borrow_mut() = false;
        *self.running.borrow_mut() = None;
        *self.source.borrow_mut() = source.to_string();
        match compile_module(source) {
            Ok(module) => {
                let host = Box::new(CallbackHost {
                    on_stdout: self.on_stdout.clone(),
                    on_stderr: self.on_stderr.clone(),
                });
                *self.running.borrow_mut() = Some(Running::new(module, host));
            }
            Err(error) => {
                self.write_compile_error(source, &error);
                self.exit(1);
            }
        }
    }

    pub fn stdin_line(&self, line: &str) {
        if let Some(running) = self.running.borrow_mut().as_mut() {
            running
                .interp
                .provide_stdin(StdinRecord::Line(line.to_string()));
        }
    }

    pub fn stdin_eof(&self) {
        if let Some(running) = self.running.borrow_mut().as_mut() {
            running.interp.provide_stdin(StdinRecord::Eof);
        }
    }

    /// Run until stdin is needed or the process exits.
    /// Returns `"need-stdin"`, `"exited"`, or `"idle"`.
    pub fn poll(&self) -> String {
        if *self.exited.borrow() {
            return "exited".into();
        }
        let mut running = self.running.borrow_mut();
        let Some(session) = running.as_mut() else {
            return "idle".into();
        };
        match session.interp.poll() {
            Ok(InterpretPoll::NeedStdin) => "need-stdin".into(),
            Ok(InterpretPoll::Exited) => {
                drop(running);
                self.exit(0);
                "exited".into()
            }
            Err(error) => {
                self.write_compile_error(&self.source.borrow(), &error);
                drop(running);
                self.exit(1);
                "exited".into()
            }
        }
    }
}

impl Session {
    fn write_compile_error(&self, _source: &str, error: &CompileError) {
        let payload = format!("SIMULA_DIAGNOSTIC:{}", error.to_json_bundle());
        let _ = self
            .on_stderr
            .call1(&JsValue::NULL, &JsValue::from_str(&payload));
    }

    fn exit(&self, code: i32) {
        *self.running.borrow_mut() = None;
        *self.exited.borrow_mut() = true;
        let _ = self.on_exit.call1(&JsValue::NULL, &JsValue::from(code));
    }
}

fn compile_module(source: &str) -> Result<outimage::mir::Module, CompileError> {
    let file = SourceFile::anonymous(source);
    let tokens = lex::tokenize_with_options(
        &file,
        &LexOptions {
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
        },
    )?;
    let program = parse::parse(&tokens)?;
    if let Err(errors) = semantic::analyze_all(&program) {
        return Err(errors.into_bundled());
    }
    mir::lower_program_with_source(&program, &file.text)
}
