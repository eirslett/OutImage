import * as vscode from "vscode";

import { getSimSettings } from "./config";
import { getResolvedLaunch } from "./client";
import { currentLaunch, rememberLaunch } from "./runtime";

/** Registers the Simula interpreter debug adapter (`sim dap`). */
export function registerDebugger(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider(
      "simula",
      new SimulaDebugConfigProvider(),
    ),
    vscode.debug.registerDebugAdapterDescriptorFactory(
      "simula",
      new SimulaDebugAdapterFactory(),
    ),
  );
}

function resolveDebugLaunch() {
  const fromClient = getResolvedLaunch();
  if (fromClient.present) {
    return fromClient;
  }
  const launch = currentLaunch();
  rememberLaunch(launch);
  return launch;
}

class SimulaDebugConfigProvider
  implements vscode.DebugConfigurationProvider
{
  async resolveDebugConfiguration(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): Promise<vscode.DebugConfiguration | undefined> {
    if (!config.type && !config.request && !config.name) {
      const editor = vscode.window.activeTextEditor;
      if (editor?.document.languageId === "simula") {
        config.type = "simula";
        config.name = "Debug Simula (interpreter)";
        config.request = "launch";
        config.program = editor.document.uri.fsPath;
        config.stopOnEntry = true;
      }
    }
    if (!config.program) {
      const editor = vscode.window.activeTextEditor;
      if (editor?.document.languageId === "simula") {
        config.program = editor.document.uri.fsPath;
      }
    }
    if (config.stopOnEntry === undefined) {
      config.stopOnEntry = true;
    }
    if (config.allowDoubleDashComments === undefined) {
      config.allowDoubleDashComments =
        getSimSettings().allowDoubleDashComments;
    }
    if (config.backend === "native") {
      const program =
        typeof config.nativeProgram === "string" && config.nativeProgram.length > 0
          ? config.nativeProgram
          : undefined;
      if (!program) {
        void vscode.window.showErrorMessage(
          'backend "native" requires nativeProgram (path to a sim compile -g binary). Prefer type "lldb" — see docs/DEBUG.md.',
        );
        return undefined;
      }
      const native: vscode.DebugConfiguration = {
        type: "lldb",
        request: "launch",
        name: config.name ?? "Debug Simula (native / CodeLLDB)",
        program,
        cwd: folder?.uri.fsPath ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath,
      };
      await vscode.debug.startDebugging(folder, native);
      return undefined;
    }
    return config;
  }
}

class SimulaDebugAdapterFactory
  implements vscode.DebugAdapterDescriptorFactory
{
  createDebugAdapterDescriptor(): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    const launch = resolveDebugLaunch();
    return new vscode.DebugAdapterExecutable(
      launch.command,
      ["dap"],
      {
        cwd: vscode.workspace.workspaceFolders?.[0]?.uri.fsPath,
      },
    );
  }
}
