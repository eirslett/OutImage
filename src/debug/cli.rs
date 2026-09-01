//! Human-facing CLI sugar over the interpreter debug probe (`sim debug`).

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::CompileError;

use super::format::evaluate_expression;
use super::probe::{DebugProbe, PauseInfo, SourceBreakpoint};
use super::session::{LaunchConfig, prepare, run_with_probe};

/// Options for [`run_cli_debug`].
#[derive(Debug, Clone)]
pub struct CliDebugOptions {
    pub program: PathBuf,
    pub breakpoints: Vec<u32>,
    pub stop_on_entry: bool,
    pub allow_square_bracket_subscripts: bool,
    pub allow_double_dash_comments: bool,
    /// Scripted debugger commands (`continue`, `next`, `step`, …). When empty,
    /// commands are read from stdin (one per line).
    pub commands: Vec<String>,
    /// Print a short pause summary on every stop (useful with scripted runs).
    pub trace: bool,
}

enum CommandEffect {
    /// Resume execution (continue / step*).
    Resume,
    /// Stay paused and accept another command.
    Stay,
    /// Terminate the session.
    Quit,
}

/// Run an interpreted debug session driven by CLI commands.
///
/// Returns the program's collected `OutText` / `OutImage` output.
pub fn run_cli_debug(opts: CliDebugOptions) -> Result<String, CompileError> {
    let config = LaunchConfig {
        program: opts.program.clone(),
        stop_on_entry: opts.stop_on_entry,
        allow_square_bracket_subscripts: opts.allow_square_bracket_subscripts,
        allow_double_dash_comments: opts.allow_double_dash_comments,
    };
    let prepared = prepare(&config)?;
    let probe = DebugProbe::new(
        opts.program.clone(),
        prepared.source.text.clone(),
        opts.stop_on_entry,
    );
    if !opts.breakpoints.is_empty() {
        let bps: Vec<_> = opts
            .breakpoints
            .iter()
            .map(|&line| SourceBreakpoint::line(line))
            .collect();
        probe.set_breakpoints(Path::new(&opts.program), bps);
    }

    let (tx, rx) = mpsc::channel::<PauseInfo>();
    let trace = opts.trace;
    probe.set_on_stopped(move |info| {
        if trace {
            let _ = writeln!(
                io::stderr(),
                "stop reason={} line={} col={} frames={}",
                info.reason,
                info.line,
                info.column,
                info.frames.len()
            );
        }
        let _ = tx.send(info);
    });

    let probe_run = probe.clone();
    let handle = thread::spawn(move || run_with_probe(&prepared, probe_run));

    let scripted = !opts.commands.is_empty();
    let mut cmd_iter = opts.commands.into_iter();
    // Only touch stdin when the session is interactive. Holding `stdin.lock()`
    // for the whole session deadlocks parallel tests that also read stdin.
    let stdin = io::stdin();
    let quiet = opts.trace; // when tracing, skip the verbose pause banner

    'session: loop {
        if handle.is_finished() {
            break;
        }
        let info = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(info) => info,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        print_pause(&info, quiet)?;

        // Inspect commands may re-prompt while still paused.
        loop {
            let command = if scripted {
                match cmd_iter.next() {
                    Some(cmd) => cmd,
                    None => {
                        // Script exhausted while still paused — continue to completion.
                        probe.continue_execution();
                        drain_until_done(&probe, &rx, &handle);
                        break 'session;
                    }
                }
            } else {
                let mut stdin_lines = stdin.lock().lines();
                match next_stdin_command(&mut stdin_lines) {
                    CommandSource::Line(cmd) => cmd,
                    CommandSource::Eof => {
                        probe.continue_execution();
                        drain_until_done(&probe, &rx, &handle);
                        break 'session;
                    }
                    CommandSource::IoError(err) => {
                        probe.request_terminate();
                        let _ = handle.join();
                        return Err(CompileError::codegen(format!("debug stdin: {err}")));
                    }
                }
            };

            match apply_command(&probe, &info, command.trim())? {
                CommandEffect::Resume => break,
                CommandEffect::Stay => continue,
                CommandEffect::Quit => {
                    probe.request_terminate();
                    let _ = handle.join();
                    return Ok(String::new());
                }
            }
        }
    }

    drain_until_done(&probe, &rx, &handle);

    match handle.join() {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(CompileError::codegen("debug eval thread panicked")),
    }
}

enum CommandSource {
    Line(String),
    Eof,
    IoError(io::Error),
}

fn next_stdin_command(stdin_lines: &mut impl Iterator<Item = io::Result<String>>) -> CommandSource {
    eprint!("(sim) ");
    let _ = io::stderr().flush();
    match stdin_lines.next() {
        Some(Ok(line)) => CommandSource::Line(line),
        Some(Err(err)) => CommandSource::IoError(err),
        None => CommandSource::Eof,
    }
}

fn drain_until_done(
    probe: &DebugProbe,
    rx: &mpsc::Receiver<PauseInfo>,
    handle: &thread::JoinHandle<Result<String, CompileError>>,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if handle.is_finished() {
            return;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(_) => probe.continue_execution(),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
    probe.request_terminate();
}

fn print_pause(info: &PauseInfo, quiet: bool) -> Result<(), CompileError> {
    if quiet {
        return Ok(());
    }
    let mut out = io::stderr();
    writeln!(
        out,
        "Paused ({}) at {}:{}",
        info.reason, info.line, info.column
    )
    .map_err(|e| CompileError::codegen(e.to_string()))?;
    for (i, frame) in info.frames.iter().enumerate() {
        writeln!(out, "  #{i} {}", frame.name).map_err(|e| CompileError::codegen(e.to_string()))?;
    }
    Ok(())
}

fn apply_command(
    probe: &DebugProbe,
    info: &PauseInfo,
    command: &str,
) -> Result<CommandEffect, CompileError> {
    let mut parts = command.split_whitespace();
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    match verb.as_str() {
        "" => Ok(CommandEffect::Stay),
        "c" | "continue" | "cont" => {
            probe.continue_execution();
            Ok(CommandEffect::Resume)
        }
        "n" | "next" | "over" => {
            probe.step_over();
            Ok(CommandEffect::Resume)
        }
        "s" | "step" | "stepin" | "into" => {
            probe.step_in();
            Ok(CommandEffect::Resume)
        }
        "o" | "out" | "stepout" | "finish" => {
            probe.step_out();
            Ok(CommandEffect::Resume)
        }
        "l" | "locals" => {
            for entry in &info.variables.locals {
                let _ = writeln!(io::stderr(), "  {} = {}", entry.name, entry.value);
            }
            Ok(CommandEffect::Stay)
        }
        "p" | "print" | "eval" => {
            let expr: String = parts.collect::<Vec<_>>().join(" ");
            if expr.is_empty() {
                let _ = writeln!(io::stderr(), "usage: print <expression>");
            } else {
                match evaluate_expression(&info.variables, &expr) {
                    Ok(entry) => {
                        let _ = writeln!(io::stderr(), "{} = {}", entry.name, entry.value);
                    }
                    Err(err) => {
                        let _ = writeln!(io::stderr(), "error: {err}");
                    }
                }
            }
            Ok(CommandEffect::Stay)
        }
        "bt" | "backtrace" | "where" => {
            for (i, frame) in info.frames.iter().enumerate() {
                let _ = writeln!(io::stderr(), "  #{i} {}", frame.name);
            }
            Ok(CommandEffect::Stay)
        }
        "h" | "help" | "?" => {
            print_help();
            Ok(CommandEffect::Stay)
        }
        "q" | "quit" | "exit" => Ok(CommandEffect::Quit),
        other => {
            let _ = writeln!(io::stderr(), "unknown command {other:?} (try help)");
            Ok(CommandEffect::Stay)
        }
    }
}

fn print_help() {
    let _ = writeln!(
        io::stderr(),
        "Commands: continue (c), next (n), step (s), out (o),\n\
         \tlocals (l), print <expr> (p), backtrace (bt), quit (q), help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_sim(body: &str) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sim-debug-cli-{id}.sim"));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn cli_break_then_continue_script() {
        let path = temp_sim("begin\ninteger x;\nx := 1;\nOutText(\"hi\");\nOutImage;\nend\n");
        let output = run_cli_debug(CliDebugOptions {
            program: path.clone(),
            breakpoints: vec![3],
            stop_on_entry: false,
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
            commands: vec!["print x".into(), "continue".into()],
            trace: false,
        })
        .expect("cli debug");
        let _ = std::fs::remove_file(&path);
        assert!(output.contains("hi"), "got {output:?}");
    }

    #[test]
    fn cli_stop_on_entry_step_over() {
        let path = temp_sim("begin\ninteger x;\nx := 2;\nOutText(\"ok\");\nOutImage;\nend\n");
        let output = run_cli_debug(CliDebugOptions {
            program: path.clone(),
            breakpoints: vec![],
            stop_on_entry: true,
            allow_square_bracket_subscripts: true,
            allow_double_dash_comments: true,
            commands: vec!["next".into(), "continue".into()],
            trace: true,
        })
        .expect("cli debug");
        let _ = std::fs::remove_file(&path);
        assert!(output.contains("ok"), "got {output:?}");
    }
}
