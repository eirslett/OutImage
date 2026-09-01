import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const workspaceRoot = fileURLToPath(new URL("..", import.meta.url));
const browserInterpJs = fileURLToPath(
  new URL("../target/outimage-browser-interp/outimage_browser_interp.js", import.meta.url),
);

if (!existsSync(browserInterpJs)) {
  throw new Error(
    "Browser interpreter wasm is missing. From the repo root run:\n  cargo browser-interp",
  );
}

export default defineConfig({
  root: ".",
  base: "./",
  worker: { format: "es" },
  build: {
    target: "es2022",
  },
  resolve: {
    alias: {
      "outimage-browser-interp": browserInterpJs,
    },
  },
  optimizeDeps: {
    // UMD webpack bundles (`module.exports = …`). Vite must prebundle them
    // to ESM or named imports fail in the browser (`OnigScanner` is missing).
    include: ["vscode-oniguruma", "vscode-textmate"],
  },
  server: {
    fs: {
      allow: [workspaceRoot],
    },
  },
});
