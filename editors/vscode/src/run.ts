import * as path from "node:path";
import * as fs from "node:fs";
import * as vscode from "vscode";
import { commandArguments, findPackageRoot, launchOptions, LaunchOptions, TaskCommand, taskCommands, taskPath } from "./launch";
type Compiler = () => string;
interface FosterTask extends vscode.TaskDefinition, LaunchOptions {
  command: TaskCommand;
}
export function registerRunIntegration(context: vscode.ExtensionContext, compiler: Compiler): void {
  context.subscriptions.push(vscode.commands.registerCommand("foster.runCurrentFile", () => runActive(compiler, false)), vscode.commands.registerCommand("foster.runCurrentPackage", () => runActive(compiler, true)), vscode.tasks.registerTaskProvider("foster", {
    provideTasks: () => (vscode.workspace.workspaceFolders ?? []).flatMap(folder => {
      if (!fs.existsSync(path.join(folder.uri.fsPath, "foster.toml")) && !fs.existsSync(path.join(folder.uri.fsPath, "main.fos")))
        return [];
      return taskCommands.map(command => createTask(compiler, { type: "foster", command, program: folder.uri.fsPath }, folder));
    }),
    resolveTask: task => {
      const folder = typeof task.scope === "object" ? task.scope : undefined;
      try {
        return createTask(compiler, task.definition as FosterTask, folder, task.name, task.problemMatchers, task);
      }
      catch (error) {
        void vscode.window.showErrorMessage(`Cannot resolve Foster task: ${errorText(error)}`);
        return undefined;
      }
    },
  }));
}
export async function saveSources(): Promise<boolean> {
  if (!vscode.workspace.isTrusted) {
    await vscode.window.showErrorMessage("Trust this workspace before running Foster programs.");
    return false;
  }
  for (const document of vscode.workspace.textDocuments) {
    if (document.isDirty && (document.languageId === "foster" || path.basename(document.uri.fsPath) === "foster.toml")) {
      if (document.isUntitled || !(await document.save())) {
        await vscode.window.showErrorMessage("Save the Foster source files and manifest before launching.");
        return false;
      }
    }
  }
  return true;
}
export function activeTarget(packageTarget: boolean): {
  program: string;
  folder?: vscode.WorkspaceFolder;
} {
  const document = vscode.window.activeTextEditor?.document;
  if (document?.languageId === "foster" && !document.isUntitled && document.uri.scheme === "file") {
    const folder = vscode.workspace.getWorkspaceFolder(document.uri);
    const program = packageTarget ? findPackageRoot(document.uri.fsPath, folder?.uri.fsPath) : document.uri.fsPath;
    if (!program)
      throw new Error("No foster.toml project or main.fos package was found for the active file.");
    return { program, folder };
  }
  if (packageTarget && vscode.workspace.workspaceFolders?.length === 1)
    return { program: vscode.workspace.workspaceFolders[0].uri.fsPath, folder: vscode.workspace.workspaceFolders[0] };
  throw new Error("Open a saved Foster file to select the program to run.");
}
async function runActive(compiler: Compiler, packageTarget: boolean): Promise<void> {
  try {
    if (!(await saveSources()))
      return;
    const { program, folder } = activeTarget(packageTarget);
    const settings = vscode.workspace.getConfiguration("foster.run", vscode.Uri.file(program));
    const definition: FosterTask = { type: "foster", command: "run", program, args: settings.get<string[]>("args", []), env: settings.get<Record<string, string>>("env", {}) };
    const cwd = settings.get<string>("cwd", "");
    if (cwd)
      definition.cwd = cwd;
    await vscode.tasks.executeTask(createTask(compiler, definition, folder));
  }
  catch (error) {
    await vscode.window.showErrorMessage(`Cannot run Foster: ${errorText(error)}`);
  }
}
function createTask(compiler: Compiler, definition: FosterTask, folder?: vscode.WorkspaceFolder, name?: string, matchers: string[] = [], original?: vscode.Task): vscode.Task {
  if (!taskCommands.includes(definition.command))
    throw new Error("command must be run, check, build, or test.");
  const file = vscode.window.activeTextEditor?.document.uri.fsPath;
  const resolved = { ...definition, program: taskPath(definition.program, folder?.uri.fsPath, file), cwd: definition.cwd === undefined ? undefined : taskPath(definition.cwd, folder?.uri.fsPath, file) };
  const options = launchOptions(resolved, folder?.uri.fsPath ?? process.cwd());
  const execution = new vscode.ProcessExecution(compiler(), commandArguments(definition.command, options), { cwd: options.cwd, env: options.env });
  const task = new vscode.Task(definition, folder ?? vscode.TaskScope.Workspace, name ?? definition.command, "Foster", execution, matchers);
  task.group = original?.group ?? (definition.command === "build" ? vscode.TaskGroup.Build : definition.command === "test" ? vscode.TaskGroup.Test : undefined);
  task.presentationOptions = original?.presentationOptions ?? { clear: true, echo: true, focus: false, panel: vscode.TaskPanelKind.Shared, reveal: vscode.TaskRevealKind.Always };
  task.runOptions = original?.runOptions ?? {};
  return task;
}
export function errorText(error: unknown): string { return error instanceof Error ? error.message : String(error); }
