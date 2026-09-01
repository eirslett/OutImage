import * as vscode from "vscode";

import { currentLaunch, rememberLaunch } from "./runtime";

interface SimTaskDefinition extends vscode.TaskDefinition {
  command: "check" | "run" | "compile";
  args?: string[];
}

export function registerSimTasks(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.tasks.registerTaskProvider("sim", {
      provideTasks: () => undefined,
      resolveTask(task: vscode.Task): vscode.Task | undefined {
        const definition = task.definition as SimTaskDefinition;
        if (
          definition.command !== "check" &&
          definition.command !== "run" &&
          definition.command !== "compile"
        ) {
          return undefined;
        }
        const launch = currentLaunch();
        rememberLaunch(launch);
        const simArgs = [definition.command, ...(definition.args ?? [])];
        const execution = new vscode.ProcessExecution(
          launch.command,
          simArgs,
        );
        return new vscode.Task(
          definition,
          task.scope ?? vscode.TaskScope.Workspace,
          task.name,
          "sim",
          execution,
          task.problemMatchers,
        );
      },
    }),
  );
}
