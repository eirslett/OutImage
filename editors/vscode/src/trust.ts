import { workspace } from "vscode";

/** True when the workspace is trusted enough to spawn `sim`. */
export function isWorkspaceTrustedForLsp(): boolean {
  return workspace.isTrusted;
}

export function onWorkspaceTrustChanged(
  listener: (trusted: boolean) => void,
): { dispose: () => void } {
  return workspace.onDidGrantWorkspaceTrust(() => {
    listener(workspace.isTrusted);
  });
}
