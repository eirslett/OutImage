import { type TerminalOptions } from "vscode";
import {
  TransportKind,
  type Executable,
} from "vscode-languageclient/node";

import { pathVariablesForRoot } from "./binary";
import { getSimSettings, workspaceRoots } from "./config";
import { resolveSimLaunch, type SimLaunch } from "./launch";

let resolvedLaunch: SimLaunch | undefined;

export function getResolvedLaunch(): SimLaunch {
  return resolvedLaunch ?? currentLaunch();
}

export function getResolvedBinary(): string {
  return getResolvedLaunch().display;
}

export function launchIsReady(): boolean {
  return getResolvedLaunch().present;
}

export function currentLaunch(): SimLaunch {
  const settings = getSimSettings();
  const root = workspaceRoots()[0];
  const vars = root ? pathVariablesForRoot(root) : {};
  return resolveSimLaunch({
    settings,
    vars,
  });
}

export function rememberLaunch(launch: SimLaunch): void {
  resolvedLaunch = launch;
}

export function executableForSim(simArgs: string[]): Executable {
  const launch = getResolvedLaunch();
  return {
    command: launch.command,
    args: simArgs,
    transport: TransportKind.stdio,
  };
}

export function terminalOptionsForSim(
  name: string,
  cwd: string,
): TerminalOptions {
  return {
    name,
    cwd,
  };
}
