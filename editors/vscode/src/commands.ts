import * as path from "node:path";

import {
  commands,
  env,
  languages,
  Uri,
  window,
  workspace,
  type ExtensionContext,
} from "vscode";

import {
  getResolvedLaunch,
  showLanguageServerOutput,
} from "./client";
import { explainReportCode } from "./config";
import { simShellCommand } from "./launch";
import { terminalOptionsForSim } from "./runtime";
import { isWorkspaceTrustedForLsp } from "./trust";

function diagnosticCode(diagnostic: {
  code?: string | number | { value: string | number };
}): string | undefined {
  const { code } = diagnostic;
  if (code === undefined) {
    return undefined;
  }
  if (typeof code === "string" || typeof code === "number") {
    return String(code);
  }
  return String(code.value);
}

export function registerCommands(context: ExtensionContext): void {
  context.subscriptions.push(
    window.onDidChangeActiveTextEditor(() => {
      void updateContextHasSimulaDiagnostic();
    }),
    window.onDidChangeTextEditorSelection(() => {
      void updateContextHasSimulaDiagnostic();
    }),
  );

  context.subscriptions.push(
    commands.registerCommand("simula.restartLanguageServer", async () => {
      const { bootLanguageServer } = await import("./boot");
      try {
        const started = await bootLanguageServer();
        if (started) {
          void window.showInformationMessage("Simula language server restarted.");
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void window.showErrorMessage(
          `Failed to restart Simula language server: ${message}`,
        );
      }
    }),

    commands.registerCommand("simula.showLanguageServerOutput", () => {
      showLanguageServerOutput();
    }),

    commands.registerCommand("simula.explainDiagnostic", async (uri?, diag?) => {
      await explainDiagnostic(uri, diag);
    }),

    commands.registerCommand("simula.openDocumentation", async () => {
      await openDocumentation(context);
    }),

    commands.registerCommand("simula.checkCurrentFile", async () => {
      await runSimulaOnActiveFile("check");
    }),

    commands.registerCommand("simula.runCurrentFile", async () => {
      await runSimulaOnActiveFile("run");
    }),

    commands.registerCommand("simula.compileCurrentFile", async () => {
      await runSimulaOnActiveFile("compile");
    }),

    commands.registerCommand("simula.debugCurrentFile", async () => {
      await debugCurrentFile();
    }),
  );
}

async function explainDiagnostic(
  uri?: Uri,
  diagnostic?: { message: string; code?: unknown; range?: unknown },
): Promise<void> {
  if (diagnostic && uri) {
    const code =
      typeof diagnostic.code === "string" || typeof diagnostic.code === "number"
        ? String(diagnostic.code)
        : undefined;
    const explanation =
      (code ? explainReportCode(code) : undefined) ?? diagnostic.message;
    await presentExplanation(explanation);
    return;
  }

  const editor = window.activeTextEditor;
  if (!editor || editor.document.languageId !== "simula") {
    void window.showWarningMessage("Open a Simula (.sim) file first.");
    return;
  }

  const position = editor.selection.active;
  const match = languages
    .getDiagnostics(editor.document.uri)
    .find((diag) => diag.range.contains(position));
  if (!match) {
    void window.showInformationMessage("No diagnostic at the cursor.");
    return;
  }

  const code = diagnosticCode(match);
  const explanation =
    (code ? explainReportCode(code) : undefined) ?? match.message;
  await presentExplanation(explanation);
}

async function presentExplanation(explanation: string): Promise<void> {
  const choice = await window.showInformationMessage(
    explanation,
    "Show LSP output",
    "Copy message",
  );
  if (choice === "Show LSP output") {
    showLanguageServerOutput();
  } else if (choice === "Copy message") {
    await env.clipboard.writeText(explanation);
  }
}

async function openDocumentation(context: ExtensionContext): Promise<void> {
  const roots = workspace.workspaceFolders?.map((f) => f.uri.fsPath) ?? [];
  for (const root of roots) {
    const local = path.join(root, "docs", "ERROR_CODES.md");
    if (await fileExists(local)) {
      const doc = await workspace.openTextDocument(Uri.file(local));
      await window.showTextDocument(doc);
      return;
    }
  }
  await env.openExternal(
    Uri.parse(
      "https://github.com/eirslett/outimage/blob/main/docs/ERROR_CODES.md",
    ),
  );
  void context;
}

async function fileExists(filePath: string): Promise<boolean> {
  try {
    await workspace.fs.stat(Uri.file(filePath));
    return true;
  } catch {
    return false;
  }
}

async function runSimulaOnActiveFile(
  subcommand: "check" | "run" | "compile",
): Promise<void> {
  const editor = window.activeTextEditor;
  if (!editor || editor.document.languageId !== "simula") {
    void window.showWarningMessage("Open a Simula (.sim) file first.");
    return;
  }
  if (editor.document.isUntitled) {
    void window.showWarningMessage("Save the file before running simula.");
    return;
  }

  const launch = getResolvedLaunch();
  if (!launch.present) {
    void window.showWarningMessage(
      `Simula executable not found at "${launch.display}".`,
    );
    return;
  }

  const file = editor.document.uri.fsPath;
  const simArgs =
    subcommand === "compile" ? ["compile", file] : [subcommand, file];
  const line = simShellCommand(launch, simArgs);
  const terminal = window.createTerminal(
    terminalOptionsForSim(`sim ${subcommand}`, path.dirname(file)),
  );
  terminal.show();
  terminal.sendText(line);
}

async function debugCurrentFile(): Promise<void> {
  if (!isWorkspaceTrustedForLsp()) {
    void window.showWarningMessage(
      "Trust this workspace before debugging Simula (executes program code).",
    );
    return;
  }
  const editor = window.activeTextEditor;
  if (!editor || editor.document.languageId !== "simula") {
    void window.showWarningMessage("Open a Simula (.sim) file first.");
    return;
  }
  if (editor.document.isUntitled) {
    void window.showWarningMessage("Save the file before debugging.");
    return;
  }
  await editor.document.save();
  await commands.executeCommand("workbench.action.debug.start");
}

async function updateContextHasSimulaDiagnostic(): Promise<void> {
  const editor = window.activeTextEditor;
  const hasDiagnostic =
    !!editor &&
    editor.document.languageId === "simula" &&
    languages
      .getDiagnostics(editor.document.uri)
      .some((diag) => diag.range.contains(editor.selection.active));
  await commands.executeCommand(
    "setContext",
    "simula.hasDiagnosticAtCursor",
    hasDiagnostic,
  );
}
