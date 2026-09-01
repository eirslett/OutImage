import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import * as onigurumaModule from "vscode-oniguruma";
import {
  INITIAL,
  Registry,
  type IGrammar,
  type StateStack,
} from "vscode-textmate";
import onigWasmUrl from "vscode-oniguruma/release/onig.wasm?url";
import grammarJson from "../../editors/vscode/syntaxes/simula.tmLanguage.json";

// `vscode-oniguruma` ships a webpack UMD file. Vite's ESM interop sometimes
// exposes named exports, sometimes only `default`.
const oniguruma =
  typeof onigurumaModule.OnigScanner === "function"
    ? onigurumaModule
    : (onigurumaModule as unknown as { default: typeof onigurumaModule }).default;
const { loadWASM, OnigScanner, OnigString } = oniguruma;

self.MonacoEnvironment = {
  getWorker() {
    return new editorWorker();
  },
};

class TmState implements monaco.languages.IState {
  constructor(readonly ruleStack: StateStack) {}
  clone(): monaco.languages.IState {
    return new TmState(this.ruleStack);
  }
  equals(other: monaco.languages.IState): boolean {
    return other instanceof TmState && other.ruleStack === this.ruleStack;
  }
}

let onigReady: Promise<void> | undefined;

function ensureOnig(): Promise<void> {
  if (!onigReady) {
    onigReady = fetch(onigWasmUrl)
      .then((response) => response.arrayBuffer())
      .then((bytes) => loadWASM({ data: bytes }));
  }
  return onigReady;
}

async function loadSimulaGrammar(): Promise<IGrammar> {
  await ensureOnig();
  const registry = new Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (patterns: string[]) => new OnigScanner(patterns),
      createOnigString: (str: string) => new OnigString(str),
    }),
    loadGrammar: async (scopeName) => {
      if (scopeName === "source.simula") {
        return grammarJson as never;
      }
      return null;
    },
  });
  const grammar = await registry.loadGrammar("source.simula");
  if (!grammar) {
    throw new Error("failed to load source.simula TextMate grammar");
  }
  return grammar;
}

export async function createEditor(
  element: HTMLElement,
  value: string,
): Promise<monaco.editor.IStandaloneCodeEditor> {
  const grammar = await loadSimulaGrammar();

  monaco.languages.register({ id: "simula", extensions: [".sim"] });
  monaco.languages.setLanguageConfiguration("simula", {
    comments: {
      lineComment: "--",
      blockComment: ["!", ";"],
    },
  });
  monaco.languages.setTokensProvider("simula", {
    getInitialState: () => new TmState(INITIAL),
    tokenize(line, state) {
      const tmState = state as TmState;
      const result = grammar.tokenizeLine(line, tmState.ruleStack);
      return {
        tokens: result.tokens.map((token) => ({
          startIndex: token.startIndex,
          scopes: token.scopes[token.scopes.length - 1] ?? "",
        })),
        endState: new TmState(result.ruleStack),
      };
    },
  });

  monaco.editor.defineTheme("outimage-dark", {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: "6A9955" },
      { token: "string", foreground: "CE9178" },
      { token: "keyword", foreground: "569CD6" },
      { token: "storage", foreground: "569CD6" },
      { token: "entity.name.function", foreground: "DCDCAA" },
      { token: "entity.name.type", foreground: "4EC9B0" },
      { token: "constant.numeric", foreground: "B5CEA8" },
    ],
    colors: {
      "editor.background": "#161b22",
    },
  });

  return monaco.editor.create(element, {
    value,
    language: "simula",
    theme: "outimage-dark",
    automaticLayout: true,
    minimap: { enabled: false },
    fontSize: 14,
    lineNumbers: "on",
    scrollBeyondLastLine: false,
    wordWrap: "on",
    renderLineHighlight: "line",
    tabSize: 3,
    insertSpaces: true,
    padding: { top: 8 },
  });
}
