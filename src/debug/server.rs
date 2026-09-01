//! Stdio DAP server driving the interpreter probe.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::{Value, json};

use super::ADAPTER_NAME;
use super::format::{REF_FRAME_BASE, REF_LOCALS, REF_SIMULATION, VarEntry, evaluate_expression};
use super::probe::{DebugProbe, PauseInfo, SourceBreakpoint};
use super::protocol::{event, read_message, response_err, response_ok, write_message};
use super::session::{LaunchConfig, prepare, run_with_probe};

enum Outgoing {
    Message(Value),
}

struct ActiveSession {
    probe: Arc<DebugProbe>,
    _join: JoinHandle<()>,
    /// Latest pause snapshot for variables / stackTrace.
    last_pause: Arc<Mutex<Option<PauseInfo>>>,
    source_path: PathBuf,
}

type SendFn = Arc<dyn Fn(Value) + Send + Sync>;

/// Runs the Debug Adapter Protocol on stdin/stdout until disconnect.
pub fn run_stdio() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    // Own `Stdout` (Send) so the writer thread can flush DAP events while the
    // main thread blocks on `read_message`.
    let writer = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));

    let (out_tx, out_rx) = mpsc::channel::<Outgoing>();
    let writer_thread = {
        let writer = Arc::clone(&writer);
        thread::spawn(move || {
            while let Ok(Outgoing::Message(msg)) = out_rx.recv() {
                let mut w = writer.lock().expect("dap writer");
                if write_message(&mut *w, &msg).is_err() {
                    break;
                }
            }
        })
    };

    let out_tx_for_send = out_tx.clone();
    let send: SendFn = Arc::new(move |msg: Value| {
        let _ = out_tx_for_send.send(Outgoing::Message(msg));
    });

    let mut session: Option<ActiveSession> = None;

    while let Ok(Some(req)) = read_message(&mut reader) {
        let typ = req.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if typ != "request" {
            continue;
        }
        let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
        match command {
            "initialize" => {
                send(response_ok(
                    &req,
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsTerminateRequest": true,
                        "supportTerminateDebuggee": true,
                        "supportsEvaluateForHovers": true,
                        "supportsConditionalBreakpoints": true,
                        "supportsLogPoints": true,
                        "supportsSetVariable": true,
                        "exceptionBreakpointFilters": [
                            {
                                "filter": "runtime",
                                "label": "Runtime errors",
                                "default": true,
                                "description": "Break when the interpreter reports a runtime error"
                            }
                        ],
                    }),
                ));
                send(event("initialized", json!({})));
            }
            "launch" => {
                if let Some(active) = session.take() {
                    active.probe.request_terminate();
                }
                match start_session(&req, Arc::clone(&send)) {
                    Ok(active) => {
                        session = Some(active);
                        send(response_ok(&req, json!({})));
                    }
                    Err(message) => {
                        send(response_err(&req, message));
                        send(event(
                            "output",
                            json!({
                                "category": "stderr",
                                "output": "Failed to launch Simula debug session.\n",
                            }),
                        ));
                        send(event("terminated", json!({})));
                    }
                }
            }
            "setBreakpoints" => {
                let body = handle_set_breakpoints(&req, session.as_ref());
                send(response_ok(&req, body));
            }
            "configurationDone" => {
                send(response_ok(&req, json!({})));
            }
            "threads" => {
                let body = handle_threads(session.as_ref());
                send(response_ok(&req, body));
            }
            "stackTrace" => {
                let body = handle_stack_trace(&req, session.as_ref());
                send(response_ok(&req, body));
            }
            "scopes" => {
                let body = handle_scopes(&req, session.as_ref());
                send(response_ok(&req, body));
            }
            "variables" => {
                let body = handle_variables(&req, session.as_ref());
                send(response_ok(&req, body));
            }
            "evaluate" => {
                let body = handle_evaluate(&req, session.as_ref());
                match body {
                    Ok(body) => send(response_ok(&req, body)),
                    Err(message) => send(response_err(&req, message)),
                }
            }
            "setVariable" => match handle_set_variable(&req, session.as_ref()) {
                Ok(body) => send(response_ok(&req, body)),
                Err(message) => send(response_err(&req, message)),
            },
            "setExceptionBreakpoints" => {
                let body = handle_set_exception_breakpoints(&req, session.as_ref());
                send(response_ok(&req, body));
            }
            "continue" => {
                if let Some(active) = session.as_ref() {
                    active.probe.continue_execution();
                }
                send(response_ok(&req, json!({ "allThreadsContinued": true })));
            }
            "next" => {
                if let Some(active) = session.as_ref() {
                    active.probe.step_over();
                }
                send(response_ok(&req, json!({})));
            }
            "stepIn" => {
                if let Some(active) = session.as_ref() {
                    active.probe.step_in();
                }
                send(response_ok(&req, json!({})));
            }
            "stepOut" => {
                if let Some(active) = session.as_ref() {
                    active.probe.step_out();
                }
                send(response_ok(&req, json!({})));
            }
            "pause" => {
                if let Some(active) = session.as_ref() {
                    active.probe.step_in();
                }
                send(response_ok(&req, json!({})));
            }
            "terminate" | "disconnect" => {
                if let Some(active) = session.take() {
                    active.probe.request_terminate();
                }
                send(response_ok(&req, json!({})));
                if command == "disconnect" {
                    break;
                }
                send(event("terminated", json!({})));
            }
            _ => {
                send(response_err(
                    &req,
                    format!("unsupported DAP request: {command}"),
                ));
            }
        }
    }

    drop(out_tx);
    let _ = writer_thread.join();
}

fn start_session(req: &Value, send: SendFn) -> Result<ActiveSession, String> {
    let args = req.get("arguments").cloned().unwrap_or(json!({}));
    let program = args
        .get("program")
        .and_then(|p| p.as_str())
        .ok_or_else(|| "launch requires arguments.program".to_string())?;
    let stop_on_entry = args
        .get("stopOnEntry")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let allow_square = args
        .get("allowSquareBracketSubscripts")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let allow_double_dash = args
        .get("allowDoubleDashComments")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let config = LaunchConfig {
        program: PathBuf::from(program),
        stop_on_entry,
        allow_square_bracket_subscripts: allow_square,
        allow_double_dash_comments: allow_double_dash,
    };
    let prepared = prepare(&config).map_err(|e| e.to_string())?;
    let probe = DebugProbe::new(
        config.program.clone(),
        prepared.source.text.clone(),
        stop_on_entry,
    );
    let last_pause = Arc::new(Mutex::new(None));
    {
        let last_pause = Arc::clone(&last_pause);
        let send = Arc::clone(&send);
        let source_path = config.program.clone();
        probe.set_on_stopped(move |info| {
            *last_pause.lock().expect("pause") = Some(info.clone());
            send(event(
                "stopped",
                json!({
                    "reason": info.reason,
                    "threadId": 1,
                    "allThreadsStopped": true,
                    "source": {
                        "name": source_path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("program.sim"),
                        "path": source_path.to_string_lossy(),
                    },
                    "line": info.line,
                    "column": info.column,
                }),
            ));
        });
    }
    {
        let send = Arc::clone(&send);
        probe.set_on_output(move |output| {
            send(event(
                "output",
                json!({
                    "category": "console",
                    "output": output,
                }),
            ));
        });
    }

    let probe_for_thread = Arc::clone(&probe);
    let send_done = Arc::clone(&send);
    let join = thread::spawn(move || {
        let result = run_with_probe(&prepared, probe_for_thread.clone());
        match result {
            Ok(output) => {
                if !output.is_empty() {
                    send_done(event(
                        "output",
                        json!({
                            "category": "stdout",
                            "output": output,
                        }),
                    ));
                }
                send_done(event(
                    "output",
                    json!({
                        "category": "console",
                        "output": format!("{ADAPTER_NAME}: program finished\n"),
                    }),
                ));
            }
            Err(error) => {
                send_done(event(
                    "output",
                    json!({
                        "category": "stderr",
                        "output": format!("{ADAPTER_NAME}: {error}\n"),
                    }),
                ));
            }
        }
        probe_for_thread.request_terminate();
        send_done(event("terminated", json!({})));
        send_done(event("exited", json!({ "exitCode": 0 })));
    });

    Ok(ActiveSession {
        probe,
        _join: join,
        last_pause,
        source_path: config.program,
    })
}

fn handle_set_breakpoints(req: &Value, session: Option<&ActiveSession>) -> Value {
    let args = req.get("arguments").cloned().unwrap_or(json!({}));
    let source = args.get("source").cloned().unwrap_or(json!({}));
    let path = source
        .get("path")
        .and_then(|p| p.as_str())
        .map(PathBuf::from)
        .or_else(|| session.map(|s| s.source_path.clone()))
        .unwrap_or_else(|| PathBuf::from("program.sim"));
    let breakpoints: Vec<SourceBreakpoint> = args
        .get("breakpoints")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|bp| {
                    let line = bp.get("line").and_then(|l| l.as_u64()).map(|l| l as u32)?;
                    Some(SourceBreakpoint {
                        line,
                        condition: bp
                            .get("condition")
                            .and_then(|c| c.as_str())
                            .map(str::to_string),
                        log_message: bp
                            .get("logMessage")
                            .and_then(|c| c.as_str())
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(active) = session {
        active.probe.set_breakpoints(&path, breakpoints.clone());
    }

    let verified: Vec<Value> = breakpoints
        .iter()
        .enumerate()
        .map(|(i, bp)| {
            json!({
                "id": i + 1,
                "verified": true,
                "line": bp.line,
            })
        })
        .collect();
    json!({ "breakpoints": verified })
}

fn handle_scopes(req: &Value, session: Option<&ActiveSession>) -> Value {
    let pause = session
        .and_then(|s| s.last_pause.lock().ok())
        .and_then(|g| g.clone());
    let has_sim = pause.as_ref().is_some_and(|p| p.variables.has_simulation);
    let frame_id = req
        .get("arguments")
        .and_then(|a| a.get("frameId"))
        .and_then(|v| v.as_i64());
    let variables_reference = match (&pause, frame_id) {
        (Some(pause), Some(id))
            if pause
                .variables
                .children
                .contains_key(&(REF_FRAME_BASE + id)) =>
        {
            REF_FRAME_BASE + id
        }
        _ => REF_LOCALS,
    };
    let mut scopes = vec![json!({
        "name": "Locals",
        "variablesReference": variables_reference,
        "expensive": false
    })];
    if has_sim {
        scopes.push(json!({
            "name": "Simulation",
            "variablesReference": REF_SIMULATION,
            "expensive": false
        }));
    }
    json!({ "scopes": scopes })
}

fn handle_threads(session: Option<&ActiveSession>) -> Value {
    let threads = session
        .and_then(|s| s.last_pause.lock().ok())
        .and_then(|g| g.clone())
        .map(|p| p.variables.threads)
        .unwrap_or_else(|| {
            vec![super::format::ThreadInfo {
                id: 1,
                name: "main".into(),
                resume_summary: None,
            }]
        });
    let threads: Vec<Value> = threads
        .into_iter()
        .map(|t| json!({ "id": t.id, "name": t.name }))
        .collect();
    json!({ "threads": threads })
}

fn handle_stack_trace(req: &Value, session: Option<&ActiveSession>) -> Value {
    let Some(active) = session else {
        return json!({ "stackFrames": [], "totalFrames": 0 });
    };
    let thread_id = req
        .get("arguments")
        .and_then(|a| a.get("threadId"))
        .and_then(|t| t.as_i64())
        .unwrap_or(1);
    let pause = active.last_pause.lock().expect("pause").clone();
    let path = active.source_path.to_string_lossy().to_string();
    let name = active
        .source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("program.sim");

    // Detached object threads get a single synthetic frame with resume hint.
    if thread_id != 1 {
        let (label, resume) = pause
            .as_ref()
            .and_then(|p| {
                p.variables
                    .threads
                    .iter()
                    .find(|t| t.id == thread_id)
                    .map(|t| (t.name.clone(), t.resume_summary.clone()))
            })
            .unwrap_or_else(|| (format!("thread {thread_id}"), None));
        let frame_name = match resume {
            Some(summary) => format!("{label} ({summary})"),
            None => label,
        };
        return json!({
            "stackFrames": [{
                "id": thread_id * 100,
                "name": frame_name,
                "source": { "name": name, "path": path },
                "line": 1,
                "column": 1,
            }],
            "totalFrames": 1
        });
    }

    let frames = pause
        .as_ref()
        .map(|p| p.frames.clone())
        .unwrap_or_else(|| active.probe.frames());
    let line = pause.as_ref().map(|p| p.line).unwrap_or(1);
    let column = pause.as_ref().map(|p| p.column).unwrap_or(1);

    // DAP wants leaf frame first.
    let mut stack_frames = Vec::new();
    for (i, frame) in frames.iter().rev().enumerate() {
        stack_frames.push(json!({
            "id": frame.id,
            "name": frame.name,
            "source": { "name": name, "path": path },
            "line": if i == 0 { line } else { 1 },
            "column": if i == 0 { column } else { 1 },
        }));
    }
    let total = stack_frames.len();
    json!({ "stackFrames": stack_frames, "totalFrames": total })
}

fn dap_var(entry: &VarEntry) -> Value {
    json!({
        "name": entry.name,
        "value": entry.value,
        "variablesReference": entry.variables_reference,
    })
}

fn handle_variables(req: &Value, session: Option<&ActiveSession>) -> Value {
    let Some(active) = session else {
        return json!({ "variables": [] });
    };
    let pause = active.last_pause.lock().expect("pause").clone();
    let Some(pause) = pause else {
        return json!({ "variables": [] });
    };
    let reference = req
        .get("arguments")
        .and_then(|a| a.get("variablesReference"))
        .and_then(|r| r.as_i64())
        .unwrap_or(REF_LOCALS);
    let entries = if reference == REF_LOCALS {
        &pause.variables.locals
    } else {
        pause
            .variables
            .children
            .get(&reference)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    };
    let variables: Vec<Value> = entries.iter().map(dap_var).collect();
    json!({ "variables": variables })
}

fn handle_evaluate(req: &Value, session: Option<&ActiveSession>) -> Result<Value, String> {
    let expression = req
        .get("arguments")
        .and_then(|a| a.get("expression"))
        .and_then(|e| e.as_str())
        .unwrap_or("")
        .to_string();
    let Some(active) = session else {
        return Err("no active debug session".into());
    };
    let pause = active
        .last_pause
        .lock()
        .expect("pause")
        .clone()
        .ok_or_else(|| "not paused".to_string())?;
    let entry = evaluate_expression(&pause.variables, &expression)?;
    Ok(json!({
        "result": entry.value,
        "variablesReference": entry.variables_reference,
    }))
}

fn handle_set_variable(req: &Value, session: Option<&ActiveSession>) -> Result<Value, String> {
    let args = req.get("arguments").cloned().unwrap_or(json!({}));
    let name = args
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| "setVariable requires name".to_string())?
        .to_string();
    let value_text = args
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "setVariable requires value".to_string())?
        .to_string();
    let variables_reference = args
        .get("variablesReference")
        .and_then(|r| r.as_i64())
        .unwrap_or(REF_LOCALS);
    let Some(active) = session else {
        return Err("no active debug session".into());
    };
    let entry = active
        .probe
        .request_set_variable(name, variables_reference, value_text)?;
    if let Some(info) = active.probe.pause_info() {
        *active.last_pause.lock().expect("pause") = Some(info);
    }
    Ok(json!({
        "value": entry.value,
        "variablesReference": entry.variables_reference,
    }))
}

fn handle_set_exception_breakpoints(req: &Value, session: Option<&ActiveSession>) -> Value {
    let filters: Vec<String> = req
        .get("arguments")
        .and_then(|a| a.get("filters"))
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let enabled = filters.iter().any(|f| f == "runtime");
    if let Some(active) = session {
        active.probe.set_break_on_exceptions(enabled);
    }
    json!({ "breakpoints": [] })
}
