import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  State,
  Trace,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("foster.restartLanguageServer", () => restart(context)),
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
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.fos"),
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

async function stop(): Promise<void> {
  const running = client;
  client = undefined;
  if (running !== undefined && running.state !== State.Stopped) {
    await running.stop();
  }
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
