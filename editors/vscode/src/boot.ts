import { State } from "vscode-languageclient/node";

import {
  binaryIsReady,
  getResolvedBinary,
  getResolvedLaunch,
  getServerVersion,
  startLanguageClient,
  stopLanguageClient,
} from "./client";
import { warnIfBinaryMissing } from "./notify";
import { isWorkspaceTrustedForLsp } from "./trust";
import { markBinaryMissing, updateStatusBar } from "./statusBar";

const RESTART_DEBOUNCE_MS = 300;

let bootChain: Promise<boolean> = Promise.resolve(false);
let restartTimer: ReturnType<typeof setTimeout> | undefined;

export function scheduleLanguageServerBoot(): void {
  if (restartTimer) {
    clearTimeout(restartTimer);
  }
  restartTimer = setTimeout(() => {
    restartTimer = undefined;
    void bootLanguageServer();
  }, RESTART_DEBOUNCE_MS);
}

export async function bootLanguageServer(): Promise<boolean> {
  if (restartTimer) {
    clearTimeout(restartTimer);
    restartTimer = undefined;
  }
  // Always run after any in-flight boot so a config change mid-start is applied.
  bootChain = bootChain.then(
    () => doBoot(),
    () => doBoot(),
  );
  return bootChain;
}

async function doBoot(): Promise<boolean> {
  if (!isWorkspaceTrustedForLsp()) {
    updateStatusBar(State.Stopped, "(workspace untrusted)");
    return false;
  }

  try {
    await stopLanguageClient();
    await startLanguageClient();
    const ready = binaryIsReady();
    const binary = getResolvedBinary();
    if (!ready) {
      markBinaryMissing(binary);
      warnIfBinaryMissing(getResolvedLaunch());
      return false;
    }
    const version = getServerVersion();
    updateStatusBar(State.Running, binary, version);
    return true;
  } catch (error) {
    const binary = getResolvedBinary();
    markBinaryMissing(binary);
    if (!binaryIsReady()) {
      warnIfBinaryMissing(getResolvedLaunch());
      return false;
    }
    const message = error instanceof Error ? error.message : String(error);
    const { window } = await import("vscode");
    void window.showErrorMessage(
      `Simula language server failed to start: ${message}`,
    );
    return false;
  }
}
