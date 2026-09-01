#!/usr/bin/env node
"use strict";

/** Minimal JSON-RPC LSP stub for extension smoke tests. */

function send(obj) {
  const body = JSON.stringify(obj);
  process.stdout.write(
    `Content-Length: ${Buffer.byteLength(body, "utf8")}\r\n\r\n${body}`,
  );
}

function log(...args) {
  process.stderr.write(`[mock-lsp] ${args.join(" ")}\n`);
}

let buffer = Buffer.alloc(0);
/** @type {string | undefined} */
let lastUri;

function publishMockDiagnostic(uri, version) {
  send({
    jsonrpc: "2.0",
    method: "textDocument/publishDiagnostics",
    params: {
      uri,
      ...(version === undefined ? {} : { version }),
      diagnostics: [
        {
          range: {
            start: { line: 0, character: 0 },
            end: { line: 0, character: 5 },
          },
          severity: 1,
          source: "sim",
          code: "E-parse",
          message: "mock parse error",
        },
      ],
    },
  });
}

process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const headerEnd = buffer.indexOf("\r\n\r\n");
    if (headerEnd === -1) {
      break;
    }
    const header = buffer.slice(0, headerEnd).toString("utf8");
    const match = header.match(/Content-Length: (\d+)/i);
    if (!match) {
      buffer = buffer.slice(headerEnd + 4);
      continue;
    }
    const len = Number.parseInt(match[1], 10);
    const start = headerEnd + 4;
    if (buffer.length < start + len) {
      break;
    }
    const body = buffer.slice(start, start + len).toString("utf8");
    buffer = buffer.slice(start + len);
    const msg = JSON.parse(body);
    log("recv", msg.method || `id=${msg.id}`);

    if (msg.method === "initialize") {
      send({
        jsonrpc: "2.0",
        id: msg.id,
        result: {
          capabilities: {
            textDocumentSync: {
              openClose: true,
              change: 1,
            },
          },
          serverInfo: { name: "mock-sim", version: "test" },
        },
      });
    } else if (msg.method === "initialized") {
      if (lastUri) {
        publishMockDiagnostic(lastUri);
      }
    } else if (msg.method === "textDocument/didOpen") {
      const doc = msg.params.textDocument;
      lastUri = doc.uri;
      log("didOpen", doc.uri);
      publishMockDiagnostic(doc.uri, doc.version);
    } else if (msg.method === "textDocument/didChange") {
      const uri = msg.params.textDocument.uri;
      lastUri = uri;
      publishMockDiagnostic(uri, msg.params.textDocument.version);
    } else if (msg.method === "textDocument/didClose") {
      const uri = msg.params.textDocument.uri;
      send({
        jsonrpc: "2.0",
        method: "textDocument/publishDiagnostics",
        params: { uri, diagnostics: [] },
      });
    } else if (msg.method === "shutdown") {
      send({ jsonrpc: "2.0", id: msg.id, result: null });
    } else if (msg.method === "exit") {
      process.exit(0);
    } else if (msg.id !== undefined) {
      send({ jsonrpc: "2.0", id: msg.id, result: null });
    }
  }
});
