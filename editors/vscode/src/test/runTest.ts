import * as path from "node:path";

import { runTests } from "@vscode/test-electron";

async function main(): Promise<void> {
  try {
    const extensionDevelopmentPath = path.resolve(__dirname, "../../");
    const extensionTestsPath = path.resolve(__dirname, "./suite/index");
    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [
        path.resolve(extensionDevelopmentPath, "simula.code-workspace"),
      ],
      extensionTestsEnv: {
        // Real Node binary for mock-lsp.js (extension host process.execPath is Electron).
        SIM_E2E_NODE: process.execPath,
      },
    });
  } catch (error) {
    console.error(error);
    process.exit(1);
  }
}

void main();
