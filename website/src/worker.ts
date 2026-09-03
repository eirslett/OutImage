/// <reference lib="webworker" />

import init, { Session } from "outimage-browser-interp";

type InMsg =
  | { type: "run"; source: string }
  | { type: "diagnose"; source: string; seq: number }
  | { type: "stdin"; line: string }
  | { type: "stdin-eof" };

type DiagnosticJson = {
  code?: string;
  title?: string;
  message?: string;
  severity?: string;
  span?: { start?: number; end?: number } | null;
  notes?: string[];
  helps?: string[];
  suggestions?: { message?: string }[];
};

type OutMsg =
  | { type: "ready" }
  | { type: "init-error"; message: string }
  | { type: "stdout"; chunk: string }
  | { type: "stderr"; chunk: string }
  | {
      type: "diagnostic";
      report?: string;
      diagnostics: DiagnosticJson[];
    }
  | {
      type: "markers";
      seq: number;
      source: string;
      diagnostics: DiagnosticJson[];
    }
  | { type: "need-stdin" }
  | { type: "exit"; code: number };

const post = (msg: OutMsg) => postMessage(msg);

const DIAG_PREFIX = "SIMULA_DIAGNOSTIC:";

function parseDiagnostics(raw: unknown): DiagnosticJson[] {
  if (Array.isArray(raw)) {
    return raw as DiagnosticJson[];
  }
  return [];
}

function onStderr(chunk: string): void {
  if (chunk.startsWith(DIAG_PREFIX)) {
    try {
      const parsed = JSON.parse(chunk.slice(DIAG_PREFIX.length)) as unknown;
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        const obj = parsed as { report?: string; diagnostics?: DiagnosticJson[] };
        post({
          type: "diagnostic",
          report: obj.report,
          diagnostics: Array.isArray(obj.diagnostics) ? obj.diagnostics : [],
        });
        return;
      }
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
let latestDiagnoseSeq = 0;

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

function runDiagnose(seq: number, source: string): void {
  if (!session) return;
  const json = session.diagnose(source);
  let diagnostics: DiagnosticJson[] = [];
  try {
    diagnostics = parseDiagnostics(JSON.parse(json) as unknown);
  } catch {
    diagnostics = [];
  }
  post({ type: "markers", seq, source, diagnostics });
}

onmessage = (event: MessageEvent<InMsg>) => {
  const data = event.data;
  if (!session) return;
  if (data.type === "run") {
    session.start(data.source);
    pump();
    return;
  }
  if (data.type === "diagnose") {
    latestDiagnoseSeq = Math.max(latestDiagnoseSeq, data.seq);
    // Skip work for superseded requests, but still ack so the UI can flush.
    if (data.seq !== latestDiagnoseSeq) {
      post({ type: "markers", seq: data.seq, source: data.source, diagnostics: [] });
      return;
    }
    runDiagnose(data.seq, data.source);
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
