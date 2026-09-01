//! Pluggable stdio for the MIR interpreter.
//!
//! `sim run` and tests historically captured SysOut into a `String` and
//! read SYSIN from the process stdin. The playground needs **streaming**
//! stdout/stderr and the ability to **pause** on `InImage` / `InLine`.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

/// One SYSIN / `InLine` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinRecord {
    Line(String),
    Eof,
}

/// Result of [`IoHost::read_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadLine {
    Ready(StdinRecord),
    /// No record available yet; the interpreter must not advance past the
    /// current stdin opcode. Used by the playground; native stdio never
    /// returns this (it blocks).
    NeedStdin,
}

/// Host stdio for one interpreter run.
pub trait IoHost {
    fn write_stdout(&mut self, text: &str);
    fn write_stderr(&mut self, text: &str);
    fn read_line(&mut self) -> Result<ReadLine, String>;
    /// Capturing hosts return the SysOut accumulated so far.
    fn captured_stdout(&self) -> Option<&str> {
        None
    }
}

/// Collects stdout/stderr; empty stdin is immediate EOF (unit-test behaviour).
#[derive(Debug, Default)]
pub struct CapturingHost {
    pub stdout: String,
    pub stderr: String,
    stdin: VecDeque<StdinRecord>,
}

impl CapturingHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_stdin_lines<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut host = Self::new();
        host.stdin
            .extend(lines.into_iter().map(|line| StdinRecord::Line(line.into())));
        host
    }
}

impl IoHost for CapturingHost {
    fn write_stdout(&mut self, text: &str) {
        self.stdout.push_str(text);
    }

    fn write_stderr(&mut self, text: &str) {
        self.stderr.push_str(text);
    }

    fn read_line(&mut self) -> Result<ReadLine, String> {
        Ok(ReadLine::Ready(
            self.stdin.pop_front().unwrap_or(StdinRecord::Eof),
        ))
    }

    fn captured_stdout(&self) -> Option<&str> {
        Some(&self.stdout)
    }
}

/// Process stdin / stdout / stderr. `read_line` blocks.
#[derive(Debug, Default)]
pub struct StdioHost;

impl IoHost for StdioHost {
    fn write_stdout(&mut self, text: &str) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    fn write_stderr(&mut self, text: &str) {
        let mut err = io::stderr().lock();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }

    fn read_line(&mut self) -> Result<ReadLine, String> {
        let mut line = String::new();
        let n = io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|error| format!("failed to read stdin: {error}"))?;
        if n == 0 {
            return Ok(ReadLine::Ready(StdinRecord::Eof));
        }
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        Ok(ReadLine::Ready(StdinRecord::Line(line)))
    }
}
