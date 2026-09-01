//! Minimal DAP JSON-RPC framing (Content-Length headers).

use std::io::{BufRead, Write};

use serde_json::{Value, json};

pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case("Content-Length") {
            content_length = value.trim().parse().ok();
        }
    }
    let Some(len) = content_length else {
        return Ok(None);
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    let value = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(value))
}

pub fn write_message(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

pub fn response_ok(req: &Value, body: Value) -> Value {
    json!({
        "seq": next_seq(),
        "type": "response",
        "request_seq": req.get("seq").and_then(|s| s.as_i64()).unwrap_or(0),
        "success": true,
        "command": req.get("command").cloned().unwrap_or(Value::Null),
        "body": body,
    })
}

pub fn response_err(req: &Value, message: impl Into<String>) -> Value {
    json!({
        "seq": next_seq(),
        "type": "response",
        "request_seq": req.get("seq").and_then(|s| s.as_i64()).unwrap_or(0),
        "success": false,
        "command": req.get("command").cloned().unwrap_or(Value::Null),
        "message": message.into(),
    })
}

pub fn event(event: &str, body: Value) -> Value {
    json!({
        "seq": next_seq(),
        "type": "event",
        "event": event,
        "body": body,
    })
}

fn next_seq() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static SEQ: AtomicI64 = AtomicI64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}
