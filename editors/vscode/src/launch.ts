import * as fs from "node:fs";
import * as path from "node:path";
export const taskCommands = ["run", "check", "build", "test"] as const;
export type TaskCommand = typeof taskCommands[number];
export interface LaunchOptions {
  program: string;
  cwd?: string;
  args?: string[];
  env?: Record<string, string>;
  optimize?: boolean;
}
export function findPackageRoot(file: string, boundary?: string): string | undefined {
  let directory = path.dirname(path.resolve(file));
  if (boundary !== undefined) {
    const relative = path.relative(path.resolve(boundary), directory);
    if (relative === ".." || relative.startsWith(".." + path.sep) || path.isAbsolute(relative)) return undefined;
  }
  // A manifest takes precedence over an incidental main.fos in src/.
  let plainPackage: string | undefined;
  while (true) {
    if (fs.existsSync(path.join(directory, "foster.toml")))
      return directory;
    if (plainPackage === undefined && fs.existsSync(path.join(directory, "main.fos")))
      plainPackage = directory;
    const parent = path.dirname(directory);
    if (parent === directory || (boundary !== undefined && path.relative(directory, path.resolve(boundary)) === ""))
      return plainPackage;
    directory = parent;
  }
}
export function launchOptions(value: LaunchOptions, base: string): Required<LaunchOptions> {
  if (typeof value.program !== "string" || !value.program.trim())
    throw new Error("A Foster program file or package directory is required.");
  if (value.cwd !== undefined && typeof value.cwd !== "string")
    throw new Error("cwd must be a directory path.");
  if (value.args !== undefined && (!Array.isArray(value.args) || value.args.some(arg => typeof arg !== "string")))
    throw new Error("args must be an array of strings.");
  if (value.env !== undefined && (value.env === null || Array.isArray(value.env) || typeof value.env !== "object" || Object.values(value.env).some(item => typeof item !== "string")))
    throw new Error("env must map names to string values.");
  if (value.optimize !== undefined && typeof value.optimize !== "boolean")
    throw new Error("optimize must be true or false.");
  const program = path.resolve(base, value.program);
  const stat = fs.statSync(program);
  const cwd = path.resolve(base, value.cwd ?? (stat.isDirectory() ? program : path.dirname(program)));
  if (!fs.statSync(cwd).isDirectory())
    throw new Error("cwd must identify an existing directory.");
  return { program, cwd, args: value.args ?? [], env: value.env ?? {}, optimize: value.optimize ?? true };
}
export function commandArguments(command: TaskCommand, options: Required<LaunchOptions>): string[] {
  if (!taskCommands.includes(command))
    throw new Error(`Unsupported Foster task: ${command}`);
  if (command !== "run" && options.args.length)
    throw new Error("Program arguments are supported only by run tasks.");
  return [command, options.program, ...((command === "run" || command === "build") && !options.optimize ? ["--no-optimize"] : []), ...(command === "run" && options.args.length ? ["--", ...options.args] : [])];
}
export function taskPath(value: string, workspace?: string, file?: string): string {
  return value.replace(/\$\{([^}]+)\}/g, (_match, name: string) => {
    const values: Record<string, string | undefined> = { workspaceFolder: workspace, file, fileDirname: file ? path.dirname(file) : undefined, fileBasename: file ? path.basename(file) : undefined, relativeFile: file && workspace ? path.relative(workspace, file) : undefined };
    const resolved = name.startsWith("env:") ? process.env[name.slice(4)] : values[name];
    if (resolved === undefined)
      throw new Error(`Cannot resolve task path variable: \${${name}}`);
    return resolved;
  });
}
