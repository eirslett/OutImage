import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { resolveSimLaunch, simShellCommand } from "../launch.js";
import type { SimSettings } from "../settings.js";

function settings(overrides: Partial<SimSettings> = {}): SimSettings {
  return {
    languageServerPath: "sim",
    languageServerArgs: ["lsp"],
    traceServer: "off",
    allowSquareBracketSubscripts: true,
    allowDoubleDashComments: true,
    checkOn: "change",
    debounceMs: 200,
    enableMirCheck: false,
    enableUnusedLints: true,
    enableHeadingTypeInlayHints: true,
    maxDocumentBytes: 2_097_152,
    ...overrides,
  };
}

describe("resolveSimLaunch", () => {
  it("resolves a native executable on PATH", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sim-launch-"));
    const binary = path.join(dir, "sim");
    fs.writeFileSync(binary, "");
    fs.chmodSync(binary, 0o755);
    const launch = resolveSimLaunch({
      settings: settings(),
      envPath: dir,
    });
    assert.equal(launch.present, true);
    assert.equal(launch.command, binary);
    assert.match(simShellCommand(launch, ["run", "hello.sim"]), /run/);
  });

  it("is missing when no executable is configured", () => {
    const launch = resolveSimLaunch({
      settings: settings(),
      envPath: path.join(os.tmpdir(), "no-such-path"),
    });
    assert.equal(launch.present, false);
  });
});
