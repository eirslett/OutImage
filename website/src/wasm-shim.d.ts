declare module "outimage-browser-interp" {
  export class Session {
    constructor(
      on_stdout: (chunk: string) => void,
      on_stderr: (chunk: string) => void,
      on_exit: (code: number) => void,
    );
    start(source: string): void;
    diagnose(source: string): string;
    stdin_line(line: string): void;
    stdin_eof(): void;
    poll(): string;
    free(): void;
  }
  export default function init(
    module?: RequestInfo | URL | Response | BufferSource | WebAssembly.Module,
  ): Promise<unknown>;
}
