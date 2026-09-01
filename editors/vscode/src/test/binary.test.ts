import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  binaryLooksPresent,
  expandConfigPath,
  findOnPath,
  pathVariablesForRoot,
  resolveSimulaPath,
  shouldPersistResolvedPath,
} from "../binary.js";

describe("expandConfigPath", () => {
  it("substitutes workspace folder variables", () => {
    const vars = pathVariablesForRoot("/repo/outimage");
    assert.equal(
      expandConfigPath("${workspaceFolder}/target/release/sim", vars),
      "/repo/outimage/target/release/sim",
    );
    assert.equal(
      expandConfigPath("${workspaceFolderBasename}/bin/sim", vars),
      "outimage/bin/sim",
    );
  });

  it("leaves value unchanged without variables", () => {
    assert.equal(expandConfigPath("sim"), "sim");
  });
});

describe("findOnPath", () => {
  it("returns the first matching executable", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sim-path-"));
    const binary = path.join(dir, "sim");
    fs.writeFileSync(binary, "");
    fs.chmodSync(binary, 0o755);

    assert.equal(findOnPath("sim", { path: dir }), binary);
  });

  it("returns undefined when the command is missing", () => {
    assert.equal(
      findOnPath("sim", { path: path.join(os.tmpdir(), "no-such-path") }),
      undefined,
    );
  });
});

describe("resolveSimulaPath", () => {
  it("uses an explicit existing path", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sim-ext-"));
    const binary = path.join(dir, "sim");
    fs.writeFileSync(binary, "");
    fs.chmodSync(binary, 0o755);

    assert.equal(resolveSimulaPath(binary), binary);
  });

  it("does not search workspace target/ directories", () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "sim-ws-"));
    const releaseDir = path.join(root, "target", "release");
    fs.mkdirSync(releaseDir, { recursive: true });
    const binary = path.join(releaseDir, "sim");
    fs.writeFileSync(binary, "");
    fs.chmodSync(binary, 0o755);

    const resolved = resolveSimulaPath("sim", pathVariablesForRoot(root));
    assert.notEqual(resolved, binary);
  });

  it("resolves a bare name via PATH", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sim-path-"));
    const binary = path.join(dir, "sim");
    fs.writeFileSync(binary, "");
    fs.chmodSync(binary, 0o755);
    const previous = process.env.PATH;
    process.env.PATH = dir;
    try {
      assert.equal(resolveSimulaPath("sim"), binary);
    } finally {
      process.env.PATH = previous;
    }
  });

  it("returns a missing filesystem path as configured", () => {
    assert.equal(
      resolveSimulaPath("/no/such/sim"),
      "/no/such/sim",
    );
  });
});

describe("shouldPersistResolvedPath", () => {
  it("persists a PATH lookup for a bare command name", () => {
    assert.equal(
      shouldPersistResolvedPath("sim", "/usr/local/bin/sim"),
      true,
    );
  });

  it("does not overwrite an explicit filesystem path", () => {
    assert.equal(
      shouldPersistResolvedPath("/opt/sim", "/usr/local/bin/sim"),
      false,
    );
  });
});

describe("binaryLooksPresent", () => {
  it("accepts an existing file", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sim-present-"));
    const binary = path.join(dir, "sim");
    fs.writeFileSync(binary, "");
    fs.chmodSync(binary, 0o755);
    assert.equal(binaryLooksPresent(binary), true);
  });

  it("rejects a missing filesystem path", () => {
    assert.equal(binaryLooksPresent("/no/such/sim"), false);
  });
});
