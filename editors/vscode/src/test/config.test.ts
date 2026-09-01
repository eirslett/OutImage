import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  explainReportCode,
  initializationOptions,
  type SimSettings,
} from "../settings.js";

describe("initializationOptions", () => {
  it("forwards bracket subscript setting", () => {
    const config: SimSettings = {
      languageServerPath: "sim",
      languageServerArgs: ["lsp"],
      traceServer: "off",
      allowSquareBracketSubscripts: false,
      allowDoubleDashComments: true,
      checkOn: "change",
      debounceMs: 200,
      enableMirCheck: false,
      enableUnusedLints: true,
      maxDocumentBytes: 2_097_152,
    };
    assert.deepEqual(initializationOptions(config), {
      allowSquareBracketSubscripts: false,
      allowDoubleDashComments: true,
      checkOn: "change",
      debounceMs: 200,
      enableMirCheck: false,
      enableUnusedLints: true,
      maxDocumentBytes: 2_097_152,
    });
  });
});

describe("explainReportCode", () => {
  it("recognizes E-parse", () => {
    assert.match(explainReportCode("E-parse") ?? "", /Syntax/i);
  });
});
