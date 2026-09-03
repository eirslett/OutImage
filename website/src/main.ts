import "./style.css";
import { createEditor, clearCompileMarkers, setCompileMarkers } from "./editor";
import { EXAMPLES } from "./examples";
import { createTerminal } from "./terminal";
import PlaygroundWorker from "./worker.ts?worker";

const statusEl = document.querySelector("#status") as HTMLSpanElement;
const runBtn = document.querySelector("#run") as HTMLButtonElement;
const cancelBtn = document.querySelector("#cancel") as HTMLButtonElement;
const eofBtn = document.querySelector("#eof") as HTMLButtonElement;
const exampleSel = document.querySelector("#example") as HTMLSelectElement;
const editorHost = document.querySelector("#editor") as HTMLDivElement;
const termHost = document.querySelector("#terminal") as HTMLDivElement;

const { term, write, writeln, focus: focusTerm } = createTerminal(termHost);

function writeFormattedDiagnostic(diag: {
  code?: string;
  title?: string;
  message?: string;
  notes?: string[];
  helps?: string[];
  suggestions?: { message?: string }[];
}): void {
  const code = diag.code ?? "";
  const title = diag.title ?? "";
  const head = [code, title].filter(Boolean).join(" ");
  writeln(`\x1b[31m${head}${head ? ": " : ""}${diag.message ?? ""}\x1b[0m`);
  for (const note of diag.notes ?? []) {
    writeln(`\x1b[33mnote: ${note}\x1b[0m`);
  }
  for (const help of diag.helps ?? []) {
    writeln(`\x1b[36mhelp: ${help}\x1b[0m`);
  }
  for (const suggestion of diag.suggestions ?? []) {
    if (suggestion.message) {
      writeln(`\x1b[36msuggestion: ${suggestion.message}\x1b[0m`);
    }
  }
}

function writeReport(report: string): void {
  write(report.replace(/\n/g, "\r\n"));
}

for (const example of EXAMPLES) {
  const option = document.createElement("option");
  option.value = example.id;
  option.textContent = example.label;
  exampleSel.append(option);
}

const editorPromise = createEditor(editorHost, EXAMPLES[0].source);

async function main(): Promise<void> {
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

type WorkerOut =
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

let worker: Worker | undefined;
let workerReady = false;
let editorReady = false;
let running = false;
let waitingStdin = false;
let stdinBuffer = "";
let runSource = "";
let debounceTimer: ReturnType<typeof setTimeout> | undefined;
let diagnoseSeq = 0;
let inFlightSeq: number | null = null;
let queuedDiagnose: string | null = null;
let workerGeneration = 0;
let editor: Awaited<typeof editorPromise> | undefined;

const DIAGNOSE_DEBOUNCE_MS = 250;

function setStatus(text: string): void {
  statusEl.textContent = text;
}

function syncButtons(): void {
  runBtn.disabled = running || !workerReady || !editorReady;
  cancelBtn.disabled = !running;
  eofBtn.disabled = !running;
}

function setRunning(value: boolean): void {
  running = value;
  waitingStdin = false;
  stdinBuffer = "";
  syncButtons();
}

function spawnWorker(): Worker {
  workerGeneration += 1;
  const generation = workerGeneration;
  inFlightSeq = null;
  queuedDiagnose = null;
  workerReady = false;
  syncButtons();
  const next = new PlaygroundWorker();
  next.addEventListener("message", (event: MessageEvent<WorkerOut>) => {
    if (generation !== workerGeneration) {
      return;
    }
    const msg = event.data;
    if (msg.type === "ready") {
      workerReady = true;
      syncButtons();
      setStatus("Ready");
      if (editorReady && editor) {
        sendDiagnose(editor.getValue());
      }
      return;
    }
    if (msg.type === "init-error") {
      workerReady = false;
      syncButtons();
      writeln(`\x1b[31mwasm init failed: ${msg.message}\x1b[0m`);
      setStatus("Worker failed");
      return;
    }
    if (msg.type === "stdout") {
      write(msg.chunk.replace(/\n/g, "\r\n"));
      return;
    }
    if (msg.type === "stderr") {
      term.write(`\x1b[31m${msg.chunk.replace(/\n/g, "\r\n")}\x1b[0m`);
      return;
    }
    if (msg.type === "markers") {
      applyMarkerResult(msg.seq, msg.source, msg.diagnostics);
      return;
    }
    if (msg.type === "diagnostic") {
      if (msg.report) {
        writeReport(msg.report);
      } else {
        for (const diag of msg.diagnostics) {
          writeFormattedDiagnostic(diag);
        }
      }
      if (editorReady && editor && editor.getValue() === runSource) {
        setCompileMarkers(editor, runSource, msg.diagnostics);
      }
      return;
    }
    if (msg.type === "need-stdin") {
      waitingStdin = true;
      setStatus("Waiting for stdin");
      focusTerm();
      return;
    }
    if (msg.type === "exit") {
      writeln("");
      writeln(`\x1b[2mexit ${msg.code}\x1b[0m`);
      setRunning(false);
      setStatus("Ready");
      if (msg.code === 0 && editorReady && editor && editor.getValue() === runSource) {
        clearCompileMarkers(editor);
      }
    }
  });
  next.addEventListener("error", (event) => {
    if (generation !== workerGeneration) {
      return;
    }
    writeln(`\x1b[31mworker error: ${event.message}\x1b[0m`);
    setRunning(false);
    setStatus("Worker failed");
  });
  return next;
}

worker = spawnWorker();
editor = await editorPromise;
editorReady = true;
syncButtons();

function sendDiagnose(source: string): void {
  if (!workerReady || !worker) {
    queuedDiagnose = source;
    return;
  }
  if (inFlightSeq !== null) {
    queuedDiagnose = source;
    return;
  }
  diagnoseSeq += 1;
  inFlightSeq = diagnoseSeq;
  worker.postMessage({ type: "diagnose", source, seq: diagnoseSeq });
}

function applyMarkerResult(
  seq: number,
  source: string,
  diagnostics: DiagnosticJson[],
): void {
  if (seq !== inFlightSeq) {
    return;
  }
  inFlightSeq = null;
  const current = editor?.getValue() ?? "";
  if (editor && source === current) {
    setCompileMarkers(editor, current, diagnostics);
  }
  if (queuedDiagnose !== null) {
    const next = queuedDiagnose;
    queuedDiagnose = null;
    sendDiagnose(next);
    return;
  }
  if (source !== current) {
    scheduleDiagnose();
  }
}

function scheduleDiagnose(): void {
  if (debounceTimer !== undefined) {
    clearTimeout(debounceTimer);
  }
  debounceTimer = setTimeout(() => {
    debounceTimer = undefined;
    sendDiagnose(editor?.getValue() ?? "");
  }, DIAGNOSE_DEBOUNCE_MS);
}

if (workerReady && editor) {
  sendDiagnose(editor.getValue());
}

editor?.onDidChangeModelContent(() => {
  scheduleDiagnose();
});

function run(): void {
  if (!workerReady || !worker) return;
  runSource = editor?.getValue() ?? "";
  term.reset();
  setRunning(true);
  setStatus("Running…");
  worker.postMessage({ type: "run", source: runSource });
}

function cancel(): void {
  if (debounceTimer !== undefined) {
    clearTimeout(debounceTimer);
    debounceTimer = undefined;
  }
  queuedDiagnose = null;
  worker?.terminate();
  worker = spawnWorker();
  writeln("");
  writeln("\x1b[2mexit 130\x1b[0m");
  setRunning(false);
  setStatus("Loading wasm…");
}

runBtn.addEventListener("click", run);
cancelBtn.addEventListener("click", cancel);
eofBtn.addEventListener("click", () => {
  if (!running) return;
  worker?.postMessage({ type: "stdin-eof" });
  waitingStdin = false;
});

exampleSel.addEventListener("change", () => {
  const example = EXAMPLES.find((item) => item.id === exampleSel.value);
  if (example) editor?.setValue(example.source);
});

term.onData((data) => {
  if (!running || !waitingStdin) return;
  for (const ch of data) {
    if (ch === "\r" || ch === "\n") {
      term.write("\r\n");
      worker?.postMessage({ type: "stdin", line: stdinBuffer });
      stdinBuffer = "";
      waitingStdin = false;
      setStatus("Running…");
    } else if (ch === "\u0004") {
      worker?.postMessage({ type: "stdin-eof" });
      waitingStdin = false;
    } else if (ch === "\u007f") {
      if (stdinBuffer.length > 0) {
        stdinBuffer = stdinBuffer.slice(0, -1);
        term.write("\b \b");
      }
    } else if (ch >= " ") {
      stdinBuffer += ch;
      term.write(ch);
    }
  }
});
}

void main().catch((error: unknown) => {
  statusEl.textContent = "Failed to start";
  writeln(`\x1b[31m${error instanceof Error ? error.message : String(error)}\x1b[0m`);
});
