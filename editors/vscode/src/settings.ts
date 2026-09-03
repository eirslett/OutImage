export interface SimSettings {
  languageServerPath: string;
  languageServerArgs: string[];
  traceServer: "off" | "messages" | "verbose";
  allowSquareBracketSubscripts: boolean;
  allowDoubleDashComments: boolean;
  checkOn: "open" | "change" | "save";
  debounceMs: number;
  enableMirCheck: boolean;
  enableUnusedLints: boolean;
  enableHeadingTypeInlayHints: boolean;
  maxDocumentBytes: number;
}

export function initializationOptions(
  config: SimSettings,
): Record<string, boolean | string | number> {
  return {
    allowSquareBracketSubscripts: config.allowSquareBracketSubscripts,
    allowDoubleDashComments: config.allowDoubleDashComments,
    checkOn: config.checkOn,
    debounceMs: config.debounceMs,
    enableMirCheck: config.enableMirCheck,
    enableUnusedLints: config.enableUnusedLints,
    enableHeadingTypeInlayHints: config.enableHeadingTypeInlayHints,
    maxDocumentBytes: config.maxDocumentBytes,
  };
}

/** Maps diagnostic report codes to short help (mirrors `sim explain`). */
export function explainReportCode(code: string): string | undefined {
  switch (code.trim().toLowerCase()) {
    case "e-lex":
    case "lex":
      return "E-lex — Lexical analysis failed. Unexpected character, malformed literal, or missing separator. See docs/ERROR_CODES.md or run: sim explain E-lex";
    case "e-parse":
    case "parse":
      return "E-parse — Syntax analysis failed. Unexpected token or incomplete declaration. See docs/ERROR_CODES.md or run: sim explain E-parse";
    case "e-semantic":
    case "semantic":
      return "E-semantic — Static semantic check failed. Unknown name, type mismatch, or visibility violation. See docs/ERROR_CODES.md or run: sim explain E-semantic";
    case "e-codegen":
    case "codegen":
      return "E-codegen — Lowering or code generation failed. See docs/ERROR_CODES.md or run: sim explain E-codegen";
    default:
      return undefined;
  }
}
