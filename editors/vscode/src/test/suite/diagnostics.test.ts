import * as assert from "node:assert";
import * as path from "node:path";
import * as vscode from "vscode";
import { State } from "vscode-languageclient/node";

import { bootLanguageServer } from "../../boot";
import {
  getLanguageClient,
  getResolvedBinary,
  showLanguageServerOutput,
  startLanguageClient,
  stopLanguageClient,
} from "../../client";

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitFor(
  predicate: () => boolean,
  timeoutMs = 15_000,
): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) {
      return true;
    }
    await sleep(100);
  }
  return predicate();
}

suite("Simula diagnostics (mock LSP)", () => {
  const target = vscode.ConfigurationTarget.Global;

  suiteTeardown(async () => {
    const config = vscode.workspace.getConfiguration("simula");
    await config.update("languageServerPath", undefined, target);
    await config.update("languageServerArgs", undefined, target);
    await stopLanguageClient();
  });

  test("mock server publishes diagnostics on open", async () => {
    const ext = vscode.extensions.getExtension("eirslett.vscode-simula");
    assert.ok(ext);
    await ext!.activate();

    const nodePath = process.env.SIM_E2E_NODE;
    assert.ok(nodePath, "SIM_E2E_NODE must be set by runTest.ts");
    const mockLsp = path.resolve(
      __dirname,
      "../../../test/fixtures/mock-lsp.js",
    );
    const config = vscode.workspace.getConfiguration("simula");
    await config.update("languageServerPath", nodePath, target);
    await config.update("languageServerArgs", [mockLsp], target);

    assert.equal(
      vscode.workspace.getConfiguration("simula").get("languageServerPath"),
      nodePath,
    );

    await stopLanguageClient();
    try {
      await startLanguageClient();
    } catch (error) {
      showLanguageServerOutput();
      const message = error instanceof Error ? error.message : String(error);
      assert.fail(
        `startLanguageClient failed: ${message} (binary=${getResolvedBinary()})`,
      );
    }

    assert.equal(
      getLanguageClient()?.state,
      State.Running,
      `expected Running after start (binary=${getResolvedBinary()})`,
    );

    await vscode.commands.executeCommand("workbench.action.closeAllEditors");
    const fixture = path.resolve(
      __dirname,
      "../../../test/fixtures/hello.sim",
    );
    const doc = await vscode.workspace.openTextDocument(fixture);
    assert.equal(doc.languageId, "simula");
    await vscode.window.showTextDocument(doc);

    const gotDiags = await waitFor(
      () => vscode.languages.getDiagnostics(doc.uri).length > 0,
    );
    const diags = vscode.languages.getDiagnostics(doc.uri);
    assert.ok(
      gotDiags && diags.length > 0,
      `expected mock LSP diagnostics; count=${diags.length}`,
    );
    assert.equal(diags[0]!.source, "sim");
    assert.equal(String(diags[0]!.code), "E-parse");

    // Keep boot path exercised too.
    assert.equal(await bootLanguageServer(), true);
  });
});
