import {
  commands,
  StatusBarAlignment,
  window,
  type Disposable,
  type StatusBarItem,
} from "vscode";
import { State } from "vscode-languageclient/node";

import { showLanguageServerOutput } from "./client";

const LABEL: Record<State, string> = {
  [State.Starting]: "$(sync~spin) Simula",
  [State.Running]: "$(check) Simula",
  [State.Stopped]: "$(debug-disconnect) Simula",
};

let statusItem: StatusBarItem | undefined;
let lastError = false;

export function initStatusBar(disposables: Disposable[]): void {
  statusItem = window.createStatusBarItem(StatusBarAlignment.Right, 100);
  statusItem.command = "simula.showLanguageServerOutput";
  statusItem.text = "$(sync~spin) Simula";
  statusItem.tooltip = "Simula language server";
  statusItem.show();
  disposables.push(
    statusItem,
    commands.registerCommand("simula.statusBarClick", () => {
      if (lastError) {
        void import("./boot").then(({ bootLanguageServer }) =>
          bootLanguageServer(),
        );
      } else {
        showLanguageServerOutput();
      }
    }),
  );
  statusItem.command = "simula.statusBarClick";
}

export function updateStatusBar(
  state: State,
  binaryPath: string,
  serverVersion?: string,
): void {
  if (!statusItem) {
    return;
  }
  lastError = false;
  statusItem.text = LABEL[state] ?? "$(question) Simula";
  const versionLine = serverVersion ? `\nServer: v${serverVersion}` : "";
  statusItem.tooltip = `Simula language server (${State[state].toLowerCase()})${versionLine}\nBinary: ${binaryPath}\nClick for output`;
}

export function markBinaryMissing(binaryPath: string): void {
  if (!statusItem) {
    return;
  }
  lastError = true;
  statusItem.text = "$(error) Simula";
  statusItem.tooltip = `sim not found: ${binaryPath}\nClick to retry`;
}
