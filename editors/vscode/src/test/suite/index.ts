import * as path from "node:path";

import { glob } from "glob";
import Mocha from "mocha";

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: "tdd", color: true, timeout: 60_000 });
  const testsRoot = path.resolve(__dirname, "..");

  return glob("suite/**/*.test.js", { cwd: testsRoot, absolute: true }).then(
    (files) => {
      for (const file of files) {
        mocha.addFile(file);
      }
      return new Promise<void>((resolve, reject) => {
        try {
          mocha.run((failures) => {
            if (failures > 0) {
              reject(new Error(`${failures} tests failed`));
            } else {
              resolve();
            }
          });
        } catch (error) {
          reject(error);
        }
      });
    },
  );
}
