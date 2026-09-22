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

import { registerRunIntegration } from "./run";
import { registerDebugIntegration } from "./debug";

let client: LanguageClient | undefined;
let serverRestartTimes: number[] = [];

const maxServerRestarts = 4;
const restartWindowMilliseconds = 3 * 60 * 1000;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  registerRunIntegration(context, () => resolveServerCommand(context));
  registerDebugIntegration(context, () => resolveServerCommand(context));
  context.subscriptions.push(
    vscode.commands.registerCommand("foster.restartLanguageServer", () => restart(context)),
    vscode.commands.registerCommand("foster.showLanguageServerOutput", () =>
      client?.outputChannel.show(true),
    ),
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("foster.server")) {
        await restart(context);
      }
    }),
  );
  await start(context);
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
