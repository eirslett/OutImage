import {
  binaryLooksPresent,
  expandConfigPath,
  findOnPath,
  isPathLike,
  type PathVariables,
} from "./binary";
import type { SimSettings } from "./settings";

export interface SimLaunch {
  /** Path shown in the status bar and error messages. */
  display: string;
  present: boolean;
  command: string;
}

export interface ResolveLaunchOptions {
  settings: SimSettings;
  vars?: PathVariables;
  platform?: NodeJS.Platform;
  envPath?: string;
}

export function quoteShellArg(value: string): string {
  if (/^[A-Za-z0-9_./:-]+$/.test(value)) {
    return value;
  }
  return JSON.stringify(value);
}

export function simShellCommand(launch: SimLaunch, simArgs: string[]): string {
  return [launch.command, ...simArgs].map(quoteShellArg).join(" ");
}

export function resolveSimLaunch(options: ResolveLaunchOptions): SimLaunch {
  const configured = options.settings.languageServerPath.trim() || "sim";
  const expanded = expandConfigPath(configured, options.vars ?? {});
  let command = expanded;
  if (!isPathLike(expanded)) {
    command =
      findOnPath(expanded, {
        path: options.envPath,
        platform: options.platform,
      }) ?? expanded;
  }
  return {
    display: command,
    present: binaryLooksPresent(command),
    command,
  };
}
