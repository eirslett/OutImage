import * as fs from "node:fs";
import * as path from "node:path";

/** Variable substitution for user-provided paths. */
export interface PathVariables {
  workspaceFolder?: string;
  workspaceFolderBasename?: string;
}

/** Options for locating a command on PATH (injectable for tests). */
export interface FindOnPathOptions {
  path?: string;
  pathext?: string;
  platform?: NodeJS.Platform;
  delimiter?: string;
}

/** Expands `${workspaceFolder}` and `${workspaceFolderBasename}` in a path. */
export function expandConfigPath(
  value: string,
  vars: PathVariables = {},
): string {
  let out = value;
  if (vars.workspaceFolder) {
    out = out.replace(/\$\{workspaceFolder\}/g, vars.workspaceFolder);
  }
  if (vars.workspaceFolderBasename && vars.workspaceFolder) {
    out = out.replace(
      /\$\{workspaceFolderBasename\}/g,
      vars.workspaceFolderBasename,
    );
  }
  return out;
}

export function pathVariablesForRoot(root: string): PathVariables {
  return {
    workspaceFolder: root,
    workspaceFolderBasename: path.basename(root),
  };
}

export function isPathLike(value: string): boolean {
  return (
    path.isAbsolute(value) ||
    value.includes("/") ||
    value.includes("\\") ||
    value.startsWith(".") ||
    value.includes("${workspaceFolder}") ||
    value.includes("${workspaceFolderBasename}")
  );
}

function candidateExists(candidate: string): boolean {
  try {
    fs.accessSync(candidate, fs.constants.X_OK);
    return true;
  } catch {
    try {
      return fs.existsSync(candidate);
    } catch {
      return false;
    }
  }
}

function executableNames(
  command: string,
  platform: NodeJS.Platform,
  pathext: string,
): string[] {
  if (platform !== "win32") {
    return [command];
  }
  if (path.extname(command)) {
    return [command];
  }
  const exts = pathext
    .split(";")
    .map((ext) => ext.trim())
    .filter((ext) => ext.length > 0);
  const names = [command];
  for (const ext of exts) {
    const suffix = ext.startsWith(".") ? ext : `.${ext}`;
    names.push(`${command}${suffix}`);
  }
  return names;
}

/** Returns the first matching executable for `command` on PATH. */
export function findOnPath(
  command: string,
  options: FindOnPathOptions = {},
): string | undefined {
  const trimmed = command.trim();
  if (!trimmed || isPathLike(trimmed)) {
    return undefined;
  }

  const platform = options.platform ?? process.platform;
  const envPath =
    options.path ?? process.env.PATH ?? process.env.Path ?? "";
  const delimiter = options.delimiter ?? path.delimiter;
  const pathext =
    options.pathext ??
    process.env.PATHEXT ??
    (platform === "win32" ? ".EXE;.CMD;.BAT;.COM" : "");
  const names = executableNames(trimmed, platform, pathext);

  for (const dir of envPath.split(delimiter)) {
    if (!dir) {
      continue;
    }
    for (const name of names) {
      const candidate = path.join(dir, name);
      if (candidateExists(candidate)) {
        return candidate;
      }
    }
  }
  return undefined;
}

/**
 * Resolves `simula.languageServerPath`.
 *
 * Bare command names (the default `sim`) are resolved via PATH. Workspace
 * folders are not searched for a locally built binary.
 */
export function resolveSimulaPath(
  configuredPath: string,
  vars: PathVariables = {},
): string {
  const expanded = expandConfigPath(configuredPath.trim(), vars);
  if (!expanded) {
    return findOnPath("sim") ?? expanded;
  }
  if (isPathLike(expanded)) {
    return expanded;
  }
  return findOnPath(expanded) ?? expanded;
}

/**
 * True when a PATH lookup should be written back to `languageServerPath`.
 *
 * Only bare command names (not user-supplied filesystem paths) are persisted.
 */
export function shouldPersistResolvedPath(
  configured: string,
  resolved: string,
): boolean {
  return (
    path.isAbsolute(resolved) &&
    configured.trim() !== resolved &&
    !isPathLike(configured.trim())
  );
}

/** Returns true when `candidate` is an existing file or a command on PATH. */
export function binaryLooksPresent(candidate: string): boolean {
  const trimmed = candidate.trim();
  if (!trimmed) {
    return false;
  }
  if (isPathLike(trimmed)) {
    return candidateExists(trimmed);
  }
  return findOnPath(trimmed) !== undefined;
}
