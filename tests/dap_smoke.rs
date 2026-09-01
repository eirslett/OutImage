//! Headless smoke for interpreter DAP (`sim dap`).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_msg(stdin: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    stdin.write_all(&body).unwrap();
    stdin.flush().unwrap();
}

fn read_one(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':')
            && key.eq_ignore_ascii_case("Content-Length")
        {
            content_length = value.trim().parse().ok();
        }
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[test]
fn dap_stop_on_entry_then_continue() {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("sim-dap-{id}.sim"));
    std::fs::write(
        &path,
        "begin\ninteger x;\nx := 1;\nOutText(\"done\"); OutImage;\nend\n",
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_sim"))
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sim dap");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut r = BufReader::new(stderr);
        let mut line = String::new();
        while r.read_line(&mut line).unwrap_or(0) > 0 {
            eprint!("dap-stderr: {line}");
            line.clear();
        }
    });

    let (tx, rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(msg) = read_one(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let pid = child.id();
    let recv = |pred: &dyn Fn(&Value) -> bool| -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(msg) if pred(&msg) => return msg,
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        terminate_pid(pid);
        panic!("timed out waiting for DAP message");
    };

    write_msg(
        &mut stdin,
        &json!({
            "seq": 1,
            "type": "request",
            "command": "initialize",
            "arguments": { "adapterID": "simula" }
        }),
    );
    recv(&|m| m.get("command").and_then(|c| c.as_str()) == Some("initialize"));
    recv(&|m| m.get("event").and_then(|e| e.as_str()) == Some("initialized"));

    write_msg(
        &mut stdin,
        &json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": path.to_string_lossy(),
                "stopOnEntry": true
            }
        }),
    );
    recv(&|m| {
        m.get("command").and_then(|c| c.as_str()) == Some("launch")
            && m.get("success").and_then(|s| s.as_bool()) == Some(true)
    });

    write_msg(
        &mut stdin,
        &json!({
            "seq": 3,
            "type": "request",
            "command": "configurationDone"
        }),
    );

    let stopped = recv(&|m| m.get("event").and_then(|e| e.as_str()) == Some("stopped"));
    assert_eq!(stopped["body"]["reason"].as_str(), Some("entry"));

    write_msg(
        &mut stdin,
        &json!({
            "seq": 4,
            "type": "request",
            "command": "continue",
            "arguments": { "threadId": 1 }
        }),
    );

    recv(&|m| m.get("event").and_then(|e| e.as_str()) == Some("terminated"));

    write_msg(
        &mut stdin,
        &json!({
            "seq": 5,
            "type": "request",
            "command": "disconnect"
        }),
    );
    child.wait_timeout_friendly();
    let _ = std::fs::remove_file(&path);
}

trait WaitTimeout {
    fn wait_timeout_friendly(&mut self);
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout_friendly(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match self.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => return,
            }
        }
        let _ = self.kill();
        let _ = self.wait();
    }
}

/// Best-effort kill of a child by PID (portable; used when `Child` is borrowed elsewhere).
fn terminate_pid(pid: u32) {
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let _ = Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
