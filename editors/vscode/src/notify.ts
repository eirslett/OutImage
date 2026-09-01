import { commands, window } from "vscode";

import { showLanguageServerOutput } from "./client";
import { bootLanguageServer } from "./boot";
import type { SimLaunch } from "./launch";

export function notifyUnexpectedStop(): void {
  void window
    .showWarningMessage(
      "Simula language server stopped unexpectedly.",
      "Restart",
      "Show output",
    )
    .then((choice) => {
      if (choice === "Restart") {
        void bootLanguageServer();
      } else if (choice === "Show output") {
        showLanguageServerOutput();
      }
    });
}

export function warnIfBinaryMissing(launch: SimLaunch): void {
  if (launch.present) {
    return;
  }
  const setting = "simula.languageServerPath";
  const message = `Simula executable not found at "${launch.display}". Install sim, then Retry, or set simula.languageServerPath.`;
  void window
    .showWarningMessage(message, "Retry", "Open Settings", "Open Documentation")
    .then((choice) => {
      if (choice === "Retry") {
        void bootLanguageServer();
      } else if (choice === "Open Settings") {
        void commands.executeCommand("workbench.action.openSettings", setting);
      } else if (choice === "Open Documentation") {
        void commands.executeCommand("simula.openDocumentation");
      }
    });
}
