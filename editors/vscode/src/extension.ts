import { workspace, type ExtensionContext } from "vscode";
import { State } from "vscode-languageclient/node";

import { bootLanguageServer, scheduleLanguageServerBoot } from "./boot";
import {
  getResolvedBinary,
  getServerVersion,
  setOnClientStateChange,
  setOnUnexpectedStop,
} from "./client";
import { shouldReloadLanguageServer } from "./config";
import { registerCommands } from "./commands";
import { registerDebugger } from "./debug";
import { notifyUnexpectedStop } from "./notify";
import { initStatusBar, updateStatusBar } from "./statusBar";
import { registerSimTasks } from "./tasks";
import { isWorkspaceTrustedForLsp, onWorkspaceTrustChanged } from "./trust";

export function activate(context: ExtensionContext): void {
  registerCommands(context);
  registerDebugger(context);
  registerSimTasks(context);
  initStatusBar(context.subscriptions);

  setOnClientStateChange((state) => {
    updateStatusBar(state, getResolvedBinary(), getServerVersion());
  });
  setOnUnexpectedStop(() => {
    notifyUnexpectedStop();
  });

  if (isWorkspaceTrustedForLsp()) {
    void bootLanguageServer();
  } else {
    updateStatusBar(State.Stopped, "(workspace untrusted)");
  }

  context.subscriptions.push(
    onWorkspaceTrustChanged((trusted) => {
      if (trusted) {
        void bootLanguageServer();
      }
    }),
    workspace.onDidChangeConfiguration((event) => {
      if (shouldReloadLanguageServer(event)) {
        scheduleLanguageServerBoot();
      }
    }),
  );
}

export function deactivate(): Promise<void> | undefined {
  return import("./client").then(({ stopLanguageClient }) => stopLanguageClient());
}
