import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

export function createTerminal(element: HTMLElement): {
  term: Terminal;
  writeln: (text: string) => void;
  write: (text: string) => void;
  focus: () => void;
} {
  const term = new Terminal({
    convertEol: true,
    cursorBlink: true,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    fontSize: 13,
    theme: {
      background: "#0d1117",
      foreground: "#e6edf3",
      cursor: "#6cb6ff",
    },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(element);
  fit.fit();
  new ResizeObserver(() => fit.fit()).observe(element);

  return {
    term,
    writeln: (text) => term.writeln(text),
    write: (text) => term.write(text),
    focus: () => term.focus(),
  };
}
