import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import {
  CloseAction,
  ErrorAction,
  LanguageClient,
  LanguageClientOptions,
  RevealOutputChannelOn,
  ServerOptions,
  State,
  Trace,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let serverRestartTimes: number[] = [];

const maxServerRestarts = 4;
const restartWindowMilliseconds = 3 * 60 * 1000;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("foster.restartLanguageServer", () => restart(context)),
    vscode.commands.registerCommand("foster.showLanguageServerOutput", () =>
      client?.outputChannel.show(true),
    ),
    vscode.commands.registerCommand("foster.runCurrentFile", () => runCurrentFile(context)),
    vscode.commands.registerCommand("foster.runCurrentPackage", () => runCurrentPackage(context)),
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("foster.server")) {
        await restart(context);
      }
    }),
  );
  await start(context);
}

async function runCurrentFile(context: vscode.ExtensionContext): Promise<void> {
  const document = await runnableDocument();
  if (document === undefined) {
    return;
  }
  await executeFoster(context, document.uri.fsPath, path.dirname(document.uri.fsPath));
}

async function runCurrentPackage(context: vscode.ExtensionContext): Promise<void> {
  const document = await runnableDocument();
  if (document === undefined) {
    return;
  }

  const workspaceFolder = vscode.workspace.getWorkspaceFolder(document.uri);
  const packageRoot = findPackageRoot(document.uri.fsPath, workspaceFolder?.uri.fsPath);
  if (packageRoot === undefined) {
    await vscode.window.showErrorMessage(
      "Foster could not find a foster.toml project or legacy main.fos package for the active file.",
    );
    return;
  }
  await executeFoster(context, packageRoot, packageRoot, workspaceFolder);
}

async function runnableDocument(): Promise<vscode.TextDocument | undefined> {
  const document = vscode.window.activeTextEditor?.document;
  if (document === undefined || document.languageId !== "foster") {
    await vscode.window.showErrorMessage("Open a Foster .fos file before running Foster code.");
    return undefined;
  }
  if (document.isUntitled || document.uri.scheme !== "file") {
    await vscode.window.showErrorMessage("Save the Foster file before running it.");
    return undefined;
  }
  if (document.isDirty && !(await document.save())) {
    await vscode.window.showErrorMessage("Foster could not save the active file before running it.");
    return undefined;
  }
  return document;
}

function findPackageRoot(file: string, workspaceRoot: string | undefined): string | undefined {
  let directory = path.dirname(path.resolve(file));
  const boundary = path.resolve(workspaceRoot ?? directory);
  if (!pathWithin(directory, boundary)) {
    return undefined;
  }

  while (true) {
    if (fs.existsSync(path.join(directory, "foster.toml"))) {
      return directory;
    }
    if (fs.existsSync(path.join(directory, "main.fos"))) {
      return directory;
    }
    if (samePath(directory, boundary)) {
      return undefined;
    }
    const parent = path.dirname(directory);
    if (parent === directory || !pathWithin(parent, boundary)) {
      return undefined;
    }
    directory = parent;
  }
}

function pathWithin(candidate: string, root: string): boolean {
  const relative = path.relative(root, candidate);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

function samePath(left: string, right: string): boolean {
  return process.platform === "win32"
    ? left.toLowerCase() === right.toLowerCase()
    : left === right;
}

async function executeFoster(
  context: vscode.ExtensionContext,
  target: string,
  cwd: string,
  workspaceFolder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(target)),
): Promise<void> {
  const execution = new vscode.ProcessExecution(resolveServerCommand(context), ["run", target], {
    cwd,
  });
  const task = new vscode.Task(
    { type: "foster", target },
    workspaceFolder ?? vscode.TaskScope.Workspace,
    "Run",
    "Foster",
    execution,
    [],
  );
  task.presentationOptions = {
    clear: true,
    echo: true,
    focus: false,
    panel: vscode.TaskPanelKind.Shared,
    reveal: vscode.TaskRevealKind.Always,
  };
  await vscode.tasks.executeTask(task);
}

export async function deactivate(): Promise<void> {
  await stop();
}

async function restart(context: vscode.ExtensionContext): Promise<void> {
  await stop();
  await start(context);
}

async function start(context: vscode.ExtensionContext): Promise<void> {
  if (client !== undefined) {
    return;
  }

  const command = resolveServerCommand(context);
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  const serverOptions: ServerOptions = {
    command,
    args: ["lsp"],
    options: workspaceFolder === undefined ? undefined : { cwd: workspaceFolder.uri.fsPath },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ language: "foster", scheme: "file" }],
    diagnosticCollectionName: "foster",
    outputChannelName: "Foster Language Server",
    revealOutputChannelOn: RevealOutputChannelOn.Error,
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.fos"),
    },
    initializationFailedHandler: (error) => {
      void showServerError("Foster language server failed to start", error);
      return false;
    },
    errorHandler: {
      error: (error, _message, count) => ({
        action: count !== undefined && count > 3 ? ErrorAction.Shutdown : ErrorAction.Continue,
        message: serverErrorMessage("Foster language server communication error", error),
      }),
      closed: unexpectedServerClose,
    },
  };

  client = new LanguageClient(
    "fosterLanguageServer",
    "Foster Language Server",
    serverOptions,
    clientOptions,
  );
  client.setTrace(configuredTrace());
  await client.start();
}

function unexpectedServerClose(): { action: CloseAction; message: string } {
  const now = Date.now();
  serverRestartTimes = serverRestartTimes.filter(
    (restart) => now - restart <= restartWindowMilliseconds,
  );
  if (serverRestartTimes.length < maxServerRestarts) {
    serverRestartTimes.push(now);
    return {
      action: CloseAction.Restart,
      message: "The Foster language server stopped unexpectedly and will be restarted. " +
        "Run 'Foster: Show Language Server Output' for details.",
    };
  }
  return {
    action: CloseAction.DoNotRestart,
    message: `The Foster language server stopped more than ${maxServerRestarts} times in three ` +
      "minutes and will not be restarted. Run 'Foster: Show Language Server Output' for details.",
  };
}

async function showServerError(summary: string, error: unknown): Promise<void> {
  const selection = await vscode.window.showErrorMessage(
    serverErrorMessage(summary, error),
    "Show Language Server Output",
  );
  if (selection === "Show Language Server Output") {
    client?.outputChannel.show(true);
  }
}

function serverErrorMessage(summary: string, error: unknown): string {
  const detail = error instanceof Error ? error.message : String(error);
  return `${summary}: ${detail}. Run 'Foster: Show Language Server Output' for details.`;
}

async function stop(): Promise<void> {
  const running = client;
  client = undefined;
  if (running !== undefined && running.state !== State.Stopped) {
    try {
      await running.stop();
    } catch (error) {
      if (!isClosedStreamError(error)) {
        throw error;
      }
    }
  }
}

function isClosedStreamError(error: unknown): boolean {
  return error instanceof Error &&
    (error.message.includes("EPIPE") || error.message.includes("stream was destroyed"));
}

function resolveServerCommand(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace
    .getConfiguration("foster.server")
    .get<string>("path", "")
    .trim();
  if (configured.length > 0) {
    return configured;
  }

  const executable = process.platform === "win32" ? "foster.exe" : "foster";
  const bundled = context.asAbsolutePath(path.join("server", executable));
  if (fs.existsSync(bundled)) {
    return bundled;
  }

  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const compiler = findWorkspaceCompiler(folder.uri.fsPath);
    if (compiler !== undefined) {
      return compiler;
    }
  }
  return "foster";
}

function findWorkspaceCompiler(start: string): string | undefined {
  const executable = process.platform === "win32" ? "foster.exe" : "foster";
  let directory = path.resolve(start);
  while (true) {
    const candidate = path.join(directory, "target", "debug", executable);
    if (fs.existsSync(path.join(directory, "Cargo.toml")) && fs.existsSync(candidate)) {
      return candidate;
    }
    const parent = path.dirname(directory);
    if (parent === directory) {
      return undefined;
    }
    directory = parent;
  }
}

function configuredTrace(): Trace {
  switch (vscode.workspace.getConfiguration("foster.server").get<string>("trace", "off")) {
    case "messages":
      return Trace.Messages;
    case "verbose":
      return Trace.Verbose;
    default:
      return Trace.Off;
  }
}
