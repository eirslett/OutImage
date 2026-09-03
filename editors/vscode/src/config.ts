import {
  ConfigurationTarget,
  workspace,
  type WorkspaceConfiguration,
} from "vscode";

import type { SimSettings } from "./settings";
export type { SimSettings } from "./settings";
export { explainReportCode, initializationOptions } from "./settings";

const SECTION = "simula";

/** Path written by PATH discovery; used to ignore the matching config event. */
let pendingPersistedPath: string | undefined;

export function getSimSettings(
  configuration: WorkspaceConfiguration = workspace.getConfiguration(SECTION),
): SimSettings {
  return {
    languageServerPath: configuration.get<string>("languageServerPath", "sim"),
    languageServerArgs: configuration.get<string[]>("languageServerArgs", ["lsp"]),
    traceServer: configuration.get<"off" | "messages" | "verbose">(
      "trace.server",
      "off",
    ),
    allowSquareBracketSubscripts: configuration.get<boolean>(
      "allowSquareBracketSubscripts",
      true,
    ),
    allowDoubleDashComments: configuration.get<boolean>(
      "allowDoubleDashComments",
      true,
    ),
    checkOn: configuration.get<"open" | "change" | "save">("checkOn", "change"),
    debounceMs: configuration.get<number>("debounceMs", 200),
    enableMirCheck: configuration.get<boolean>("enableMirCheck", false),
    enableUnusedLints: configuration.get<boolean>("enableUnusedLints", true),
    enableHeadingTypeInlayHints: configuration.get<boolean>(
      "enableHeadingTypeInlayHints",
      true,
    ),
    maxDocumentBytes: configuration.get<number>("maxDocumentBytes", 2_097_152),
  };
}

export function workspaceRoots(): string[] {
  return (
    workspace.workspaceFolders?.map((folder) => folder.uri.fsPath) ?? []
  );
}

export function configAffectsServer(event: {
  affectsConfiguration: (section: string) => boolean;
}): boolean {
  return (
    event.affectsConfiguration(`${SECTION}.languageServerPath`) ||
    event.affectsConfiguration(`${SECTION}.languageServerArgs`) ||
    event.affectsConfiguration(`${SECTION}.trace.server`)
  );
}

/**
 * Persist a discovered binary path to the winning settings scope (user
 * settings when the value is still the default).
 */
export async function persistLanguageServerPath(
  absolutePath: string,
): Promise<void> {
  pendingPersistedPath = absolutePath;
  const configuration = workspace.getConfiguration(SECTION);
  const inspect = configuration.inspect<string>("languageServerPath");
  let target = ConfigurationTarget.Global;
  if (inspect?.workspaceFolderValue !== undefined) {
    target = ConfigurationTarget.WorkspaceFolder;
  } else if (inspect?.workspaceValue !== undefined) {
    target = ConfigurationTarget.Workspace;
  }
  try {
    await configuration.update("languageServerPath", absolutePath, target);
  } catch (error) {
    pendingPersistedPath = undefined;
    throw error;
  }
}

/** True when a configuration change should restart the language server. */
export function shouldReloadLanguageServer(event: {
  affectsConfiguration: (section: string) => boolean;
}): boolean {
  if (!configAffectsServer(event)) {
    return false;
  }
  if (
    event.affectsConfiguration(`${SECTION}.languageServerPath`) &&
    pendingPersistedPath !== undefined
  ) {
    const current = workspace
      .getConfiguration(SECTION)
      .get<string>("languageServerPath");
    if (current === pendingPersistedPath) {
      pendingPersistedPath = undefined;
      return (
        event.affectsConfiguration(`${SECTION}.languageServerArgs`) ||
        event.affectsConfiguration(`${SECTION}.trace.server`)
      );
    }
  }
  return true;
}
