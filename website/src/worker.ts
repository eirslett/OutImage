/// <reference lib="webworker" />

import init, { Session } from "outimage-browser-interp";

type InMsg =
  | { type: "run"; source: string }
  | { type: "stdin"; line: string }
  | { type: "stdin-eof" };

type DiagnosticJson = {
  code?: string;
  title?: string;
  message?: string;
  notes?: string[];
  helps?: string[];
  suggestions?: { message?: string }[];
};

type OutMsg =
  | { type: "ready" }
  | { type: "init-error"; message: string }
  | { type: "stdout"; chunk: string }
  | { type: "stderr"; chunk: string }
  | { type: "diagnostic"; diagnostics: DiagnosticJson[] }
  | { type: "need-stdin" }
  | { type: "exit"; code: number };

const post = (msg: OutMsg) => postMessage(msg);

const DIAG_PREFIX = "SIMULA_DIAGNOSTIC:";

function onStderr(chunk: string): void {
  if (chunk.startsWith(DIAG_PREFIX)) {
    try {
      const parsed = JSON.parse(chunk.slice(DIAG_PREFIX.length)) as unknown;
      const diagnostics = Array.isArray(parsed) ? parsed : [parsed];
      post({ type: "diagnostic", diagnostics: diagnostics as DiagnosticJson[] });
      return;
    } catch {
      // Fall through and show the raw chunk.
    }
  }
  post({ type: "stderr", chunk });
}

let session: Session | undefined;

function pump(): void {
  if (!session) return;
  for (;;) {
    const status = session.poll();
    if (status === "need-stdin") {
      post({ type: "need-stdin" });
      return;
    }
    if (status === "exited" || status === "idle") {
      return;
    }
  }
}

onmessage = (event: MessageEvent<InMsg>) => {
  const data = event.data;
  if (!session) return;
  if (data.type === "run") {
    session.start(data.source);
    pump();
    return;
  }
  if (data.type === "stdin") {
    session.stdin_line(data.line);
    pump();
    return;
  }
  if (data.type === "stdin-eof") {
    session.stdin_eof();
    pump();
  }
};

init()
  .then(() => {
    session = new Session(
      (chunk: string) => post({ type: "stdout", chunk }),
      (chunk: string) => onStderr(chunk),
      (code: number) => post({ type: "exit", code }),
    );
    post({ type: "ready" });
  })
  .catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    post({ type: "init-error", message });
  });
