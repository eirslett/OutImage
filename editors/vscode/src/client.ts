import { workspace } from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  type ServerOptions,
  State,
  Trace,
} from "vscode-languageclient/node";

import { shouldPersistResolvedPath } from "./binary";
import {
  getSimSettings,
  initializationOptions,
  persistLanguageServerPath,
} from "./config";
import {
  currentLaunch,
  executableForSim,
  getResolvedBinary,
  getResolvedLaunch,
  launchIsReady,
  rememberLaunch,
} from "./runtime";

let client: LanguageClient | undefined;
let serverVersion: string | undefined;
let onStateChange: ((state: State) => void) | undefined;
let onUnexpectedStop: (() => void) | undefined;
let starting = false;
let clientLock: Promise<unknown> = Promise.resolve();

function withClientLock<T>(fn: () => Promise<T>): Promise<T> {
  const next = clientLock.then(fn, fn);
  clientLock = next.then(
    () => undefined,
    () => undefined,
  );
  return next;
}

export function getLanguageClient(): LanguageClient | undefined {
  return client;
}

export { getResolvedBinary, getResolvedLaunch, launchIsReady };

export function getServerVersion(): string | undefined {
  return serverVersion;
}

export function setOnClientStateChange(
  listener: ((state: State) => void) | undefined,
): void {
  onStateChange = listener;
}

export function setOnUnexpectedStop(listener: (() => void) | undefined): void {
  onUnexpectedStop = listener;
}

function traceFromConfig(level: "off" | "messages" | "verbose"): Trace {
  switch (level) {
    case "verbose":
      return Trace.Verbose;
    case "messages":
      return Trace.Messages;
    default:
      return Trace.Off;
  }
}

async function resolveAndPersistLaunch(): Promise<void> {
  const config = getSimSettings();
  const launch = currentLaunch();
  rememberLaunch(launch);
  if (
    launch.present &&
    shouldPersistResolvedPath(config.languageServerPath, launch.command)
  ) {
    await persistLanguageServerPath(launch.command);
  }
}

export function createLanguageClient(): LanguageClient {
  const config = getSimSettings();
  const run = executableForSim(config.languageServerArgs);
  const serverOptions: ServerOptions = {
    run,
    debug: { ...run },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "simula" }],
    synchronize: {
      configurationSection: "simula",
      fileEvents: workspace.createFileSystemWatcher("**/*.sim"),
    },
    outputChannelName: "Simula Language Server",
    initializationOptions: initializationOptions(config),
  };

  const languageClient = new LanguageClient(
    "simula",
    "Simula",
    serverOptions,
    clientOptions,
  );

  void languageClient.setTrace(traceFromConfig(config.traceServer));

  languageClient.onDidChangeState((event) => {
    if (event.oldState === State.Running && event.newState === State.Stopped) {
      onUnexpectedStop?.();
    }
    onStateChange?.(event.newState);
  });

  return languageClient;
}

export function binaryIsReady(): boolean {
  return launchIsReady();
}

export async function startLanguageClient(): Promise<LanguageClient> {
  return withClientLock(async () => {
    if (starting) {
      throw new Error("language server is already starting");
    }
    starting = true;
    try {
      if (client) {
        await client.stop();
        client.dispose();
        client = undefined;
      }
      await resolveAndPersistLaunch();
      const launch = getResolvedLaunch();
      if (!launch.present) {
        throw new Error(missingLaunchMessage(launch));
      }
      const languageClient = createLanguageClient();
      client = languageClient;
      await languageClient.start();
      serverVersion = languageClient.initializeResult?.serverInfo?.version;
      return languageClient;
    } finally {
      starting = false;
    }
  });
}

export function missingLaunchMessage(launch: ReturnType<typeof getResolvedLaunch>): string {
  return `Simula executable not found at "${launch.display}"`;
}

export async function stopLanguageClient(): Promise<void> {
  return withClientLock(async () => {
    if (!client) {
      return;
    }
    const current = client;
    client = undefined;
    serverVersion = undefined;
    try {
      if (current.state === State.Running || current.state === State.Starting) {
        await current.stop();
      }
    } catch {
      // startFailed / already stopped — dispose below
    }
    try {
      current.dispose();
    } catch {
      // ignore
    }
  });
}

export async function restartLanguageClient(): Promise<LanguageClient> {
  await stopLanguageClient();
  return startLanguageClient();
}

export function showLanguageServerOutput(): void {
  client?.outputChannel.show(true);
}
