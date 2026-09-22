import * as vscode from "vscode";
import * as net from "node:net";
import { randomBytes } from "node:crypto";
import { ChildProcess, spawn } from "node:child_process";
import { activeTarget, errorText, saveSources } from "./run";
import { commandArguments, launchOptions, LaunchOptions } from "./launch";
type Message = {
  seq: number;
  type: string;
  command?: string;
  arguments?: Record<string, unknown>;
  [key: string]: unknown;
};
export function registerDebugIntegration(context: vscode.ExtensionContext, compiler: () => string): void {
  context.subscriptions.push(vscode.debug.registerDebugConfigurationProvider("foster", {
    provideDebugConfigurations: () => [{ type: "foster", request: "launch", name: "Debug Foster package", program: "${workspaceFolder}", stopOnEntry: true }, { type: "foster", request: "launch", name: "Debug Foster file", program: "${file}", stopOnEntry: true }],
    resolveDebugConfiguration: (_folder, config) => {
      if (!config.type && !config.request && !config.name) {
        try {
          const target = activeTarget(true);
          return { type: "foster", request: "launch", name: "Debug Foster", program: target.program, stopOnEntry: true };
        }
        catch {
          try {
            return { type: "foster", request: "launch", name: "Debug Foster", program: activeTarget(false).program, stopOnEntry: true };
          }
          catch (error) {
            void vscode.window.showErrorMessage(errorText(error));
            return undefined;
          }
        }
      }
      return config;
    },
    resolveDebugConfigurationWithSubstitutedVariables: async (folder, config) => {
      try {
        if (!(await saveSources()))
          return undefined;
        const options = launchOptions(config as unknown as LaunchOptions, folder?.uri.fsPath ?? process.cwd());
        if (config.request !== "launch")
          throw new Error("Foster supports launch requests; attaching to running processes is not supported.");
        if (config.stopOnEntry !== undefined && typeof config.stopOnEntry !== "boolean") {
          throw new Error("stopOnEntry must be true or false.");
        }
        return { stopOnEntry: true, ...config, ...options };
      }
      catch (error) {
        await vscode.window.showErrorMessage(`Cannot launch Foster: ${errorText(error)}`);
        return undefined;
      }
    },
  }), vscode.debug.registerDebugAdapterDescriptorFactory("foster", {
    createDebugAdapterDescriptor: session => new vscode.DebugAdapterInlineImplementation(new FosterDebugAdapter(compiler(), session.configuration)),
  }), vscode.commands.registerCommand("foster.debugCurrentPackage", async () => {
    try {
      const target = activeTarget(true);
      await vscode.debug.startDebugging(target.folder, { type: "foster", request: "launch", name: "Debug Foster package", program: target.program, stopOnEntry: true });
    }
    catch (error) {
      await vscode.window.showErrorMessage(errorText(error));
    }
  }), vscode.commands.registerCommand("foster.debugCurrentFile", async () => {
    try {
      const target = activeTarget(false);
      await vscode.debug.startDebugging(target.folder, { type: "foster", request: "launch", name: "Debug Foster file", program: target.program, stopOnEntry: true });
    }
    catch (error) {
      await vscode.window.showErrorMessage(errorText(error));
    }
  }));
}
export class FosterDebugAdapter implements vscode.DebugAdapter {
  private readonly events = new vscode.EventEmitter<vscode.DebugProtocolMessage>();
  readonly onDidSendMessage = this.events.event;
  private process?: ChildProcess;
  private server?: net.Server;
  private socket?: net.Socket;
  private incoming = Buffer.alloc(0);
  private queue: Message[] = [];
  private pending = new Map<number, Message>();
  private sequence = 0;
  private started = false;
  private ended = false;
  private backendExit?: number;
  private launched = false;
  private configured = false;
  private timeout?: NodeJS.Timeout;
  constructor(private readonly compiler: string, private readonly configuration: vscode.DebugConfiguration) { }
  private emit(message: Record<string, unknown>): void { this.events.fire({ ...message, seq: ++this.sequence } as vscode.DebugProtocolMessage); }
  private response(request: Message, body: unknown = {}, error?: string): void { this.pending.delete(request.seq); this.emit({ type: "response", request_seq: request.seq, command: request.command, success: error === undefined, ...(error ? { message: error } : { body }) }); }
  private event(event: string, body: unknown = {}): void { this.emit({ type: "event", event, body }); }
  handleMessage(message: vscode.DebugProtocolMessage): void {
    const request = message as Message;
    if (request.type !== "request")
      return;
    if (this.ended) {
      this.response(request, {}, "The Foster process has ended.");
      return;
    }
    if (request.command === "disconnect" || request.command === "terminate") {
      this.response(request);
      this.stop();
      this.finish();
      return;
    }
    if (this.configuration.noDebug) {
      this.handleRun(request);
      return;
    }
    this.pending.set(request.seq, request);
    if (this.socket)
      this.write(request);
    else
      this.queue.push(request);
    if (!this.started) {
      this.started = true;
      this.startDebugger();
    }
  }
  private handleRun(request: Message): void {
    switch (request.command) {
      case "initialize":
        this.response(request, { supportsConfigurationDoneRequest: true, supportsTerminateRequest: true });
        this.event("initialized");
        break;
      case "launch":
        this.launched = true;
        this.response(request);
        break;
      case "configurationDone":
        this.configured = true;
        this.response(request);
        break;
      case "setBreakpoints":
        this.response(request, { breakpoints: ((request.arguments?.breakpoints as unknown[]) ?? []).map(() => ({ verified: false, message: "Running without debugging." })) });
        break;
      case "setExceptionBreakpoints":
        this.response(request);
        break;
      case "threads":
        this.response(request, { threads: [] });
        break;
      default: this.response(request, {}, "This operation requires a debug session.");
    }
    if (this.launched && this.configured && !this.started) {
      this.started = true;
      try {
        const options = launchOptions(this.configuration as unknown as LaunchOptions, process.cwd());
        this.spawnProcess(commandArguments("run", options));
      }
      catch (error) {
        this.fail(errorText(error));
      }
    }
  }
  private startDebugger(): void {
    const token = randomBytes(32).toString("hex");
    const server = net.createServer(socket => {
      let handshake = Buffer.alloc(0);
      socket.setTimeout(5000, () => socket.destroy());
      const authenticate = (chunk: Buffer) => {
        handshake = Buffer.concat([handshake, chunk]);
        const newline = handshake.indexOf(10);
        if (newline < 0) {
          if (handshake.length > 128)
            socket.destroy();
          return;
        }
        if (handshake.subarray(0, newline).toString().trim() !== token || this.socket) {
          socket.destroy();
          return;
        }
        socket.off("data", authenticate);
        socket.setTimeout(0);
        this.socket = socket;
        server.close(() => { });
        clearTimeout(this.timeout);
        socket.on("data", chunk => this.read(chunk));
        socket.on("error", error => {
          if (this.backendExit === undefined)
            this.fail(errorText(error));
        });
        socket.on("close", () => {
          if (!this.ended && this.backendExit === undefined && this.process?.exitCode === null)
            this.fail("Foster debugger disconnected unexpectedly.");
        });
        const remaining = handshake.subarray(newline + 1);
        if (remaining.length)
          this.read(remaining);
        for (const request of this.queue)
          this.write(request);
        this.queue = [];
      };
      socket.on("data", authenticate);
      socket.on("error", () => socket.destroy());
    });
    this.server = server;
    server.on("error", error => this.fail(errorText(error)));
    server.listen(0, "127.0.0.1", () => {
      if (this.ended)
        return;
      try {
        const port = (server.address() as net.AddressInfo).port;
        const options = launchOptions(this.configuration as unknown as LaunchOptions, process.cwd());
        this.spawnProcess(["debug", options.program, "--port", String(port), "--token", token, ...(options.args.length ? ["--", ...options.args] : [])]);
      }
      catch (error) {
        this.fail(errorText(error));
      }
    });
    this.timeout = setTimeout(() => this.fail("Foster debugger did not connect within 30 seconds."), 30000);
  }
  private spawnProcess(args: string[]): void {
    try {
      const options = launchOptions(this.configuration as unknown as LaunchOptions, process.cwd());
      this.process = spawn(this.compiler, args, { cwd: options.cwd, env: { ...process.env, ...options.env }, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
      this.process.stdout?.setEncoding("utf8").on("data", output => this.event("output", { category: "stdout", output }));
      this.process.stderr?.setEncoding("utf8").on("data", output => this.event("output", { category: "stderr", output }));
      this.process.on("error", error => this.fail(errorText(error)));
      this.process.on("close", code => {
        for (const request of this.pending.values())
          this.response(request, {}, "Foster exited before completing the request. See the Debug Console.");
        this.finish(this.backendExit ?? code ?? 1);
        this.stop();
      });
    }
    catch (error) {
      this.fail(errorText(error));
    }
  }
  private write(message: Message): void {
    const bytes = Buffer.from(JSON.stringify(message));
    this.socket?.write(`Content-Length: ${bytes.length}\r\n\r\n`);
    this.socket?.write(bytes);
  }
  private read(chunk: Buffer): void {
    this.incoming = Buffer.concat([this.incoming, chunk]);
    while (true) {
      const end = this.incoming.indexOf("\r\n\r\n");
      if (end < 0) {
        if (this.incoming.length > 8192)
          this.fail("Invalid debugger protocol header.");
        return;
      }
      const match = /Content-Length:\s*(\d+)/i.exec(this.incoming.subarray(0, end).toString());
      const length = match ? Number(match[1]) : NaN;
      if (!Number.isSafeInteger(length) || length < 0 || length > 16 * 1024 * 1024) {
        this.fail("Invalid debugger protocol length.");
        return;
      }
      if (this.incoming.length < end + 4 + length)
        return;
      try {
        const message = JSON.parse(this.incoming.subarray(end + 4, end + 4 + length).toString()) as Record<string, unknown>;
        this.incoming = this.incoming.subarray(end + 4 + length);
        if (message.type === "response")
          this.pending.delete(message.request_seq as number);
        // Process close is authoritative: drain stdout/stderr before ending the UI session.
        if (message.type === "event" && message.event === "exited") {
          this.backendExit = (message.body as {
            exitCode: number;
          }).exitCode;
          continue;
        }
        if (message.type === "event" && message.event === "terminated")
          continue;
        this.emit(message);
      }
      catch {
        this.fail("Invalid debugger protocol message.");
        return;
      }
    }
  }
  private fail(message: string): void {
    if (this.ended)
      return;
    this.event("output", { category: "stderr", output: `${message}\n` });
    for (const request of this.pending.values())
      this.response(request, {}, message);
    this.stop();
    this.finish(1);
  }
  private finish(code = 0): void {
    if (this.ended)
      return;
    this.ended = true;
    this.event("exited", { exitCode: code });
    this.event("terminated");
  }
  private stop(): void {
    clearTimeout(this.timeout);
    this.server?.close(() => {});
    this.socket?.destroy();
    this.process?.kill();
  }
  dispose(): void {
    this.ended = true;
    this.stop();
    this.events.dispose();
  }
}
