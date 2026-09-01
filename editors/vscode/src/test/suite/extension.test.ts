import * as assert from "node:assert";
import * as vscode from "vscode";

suite("Simula extension", () => {
  test("extension is present", async () => {
    const ext = vscode.extensions.getExtension("eirslett.vscode-simula");
    assert.ok(ext, "extension should be installed");
    await ext!.activate();
    assert.ok(ext!.isActive);
  });

  test("simula language is registered", async () => {
    const languages = await vscode.languages.getLanguages();
    assert.ok(languages.includes("simula"));
  });

  test("restart language server command runs", async () => {
    await assert.doesNotReject(async () => {
      await vscode.commands.executeCommand("simula.restartLanguageServer");
    });
  });
});
