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
      red: "#ff7b72",
      green: "#3fb950",
      yellow: "#d29922",
      blue: "#58a6ff",
      magenta: "#bc8cff",
      cyan: "#39d4c5",
      white: "#e6edf3",
      brightRed: "#ffa198",
      brightGreen: "#56d364",
      brightYellow: "#e3b341",
      brightBlue: "#79c0ff",
      brightMagenta: "#d2a8ff",
      brightCyan: "#56d4dd",
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
