# Foster Language Support for VS Code

This extension registers `.fos` files, provides Foster syntax highlighting and editing rules,
and launches the Foster language server. Language features include:

- package-wide compiler errors and warnings with open-buffer overlays;
- clickable related locations for secondary compiler diagnostic labels, including error fallback paths;
- document symbols from the current parse, without waiting for semantic checking;
- go-to-definition across imported modules, selected function and concrete method overloads,
  instance methods, and repository core-library source;
- find-references across package modules;
- identity-aware local and declaration rename;
- rich Foster signatures and Markdown documentation on hover;
- call signature help with active-parameter tracking and the resolved overload's signature;
- inferred local-type and argument-name inlay hints, with clickable parameter hints;
- scope-aware completion for locals, declarations, imports, qualified modules, and keywords,
  including current parameters and local bindings in unfinished functions, plus automatic
  `std.process` import when completing `Arguments`;
- quick fixes for missing imports and spelling mistakes in unresolved names and types;
- automatic diagnostic refresh when Foster files change on disk;
- background semantic compilation that keeps the protocol loop responsive, tags work with the
  current document versions and workspace generation, and cooperatively cancels stale work;
- interactive requests run on a separate worker using published semantic snapshots;
- cached package snapshots and parsed modules, so edits reparse changed sources while preserving
  unaffected compilation work;
- reusable function-body type checks guarded by source, declarations, and callee contracts,
  plus batched recovery for independent function-body errors;
- error-tolerant module parsing that reports independent syntax errors together, skips damaged
  declarations, and continues semantic analysis for complete declarations later in the file;
- commands to run the active file or its nearest `foster.toml` project (with legacy `main.fos`
  package fallback) in a shared task terminal.

The bundled grammar highlights line comments, nested block-comment delimiters, documentation
comments, control keywords such as `try`, logical operators including `not`, module `::`
qualification, code-point literals,
effect clauses, sequence types, structural intersections, and union/variant type members.

## Installation

Marketplace and VSIX releases include the Foster compiler, language server, and core-library
sources for the selected platform. No separate compiler installation is required. Set
`foster.server.path` only to override the bundled compiler with a local build.

## Running Foster

Open a saved `.fos` file and use one of these commands from the Command Palette:

- **Foster: Run Current File** executes the active file as a standalone program.
- **Foster: Run Current Package** searches upward, within the current workspace folder, for the
  nearest directory containing `foster.toml` and executes that project. Packages without a
  manifest continue to fall back to the nearest directory containing `main.fos`.

The ▶ button in the editor title runs the current file. Foster saves modified Foster files and
manifests before launching and shows compiler output in the shared Foster task terminal. Use the
package command when the program imports sibling filesystem modules. Configure `foster.run.args`,
`foster.run.cwd`, and `foster.run.env` for these Run commands.

The extension contributes **Run**, **Check**, **Build**, and **Test** tasks for workspace folders
containing a Foster project. **Tasks: Run Build Task** and **Tasks: Run Test Task** use the build
and test groups. Custom `.vscode/tasks.json` entries can use `"type": "foster"`, for example:

```json
{
  "version": "2.0.0",
  "tasks": [{
    "type": "foster",
    "label": "Check Foster",
    "command": "check",
    "program": "${workspaceFolder}",
    "group": "build",
    "problemMatcher": []
  }]
}
```

Task commands accept `program`, `cwd`, and `env`. Run tasks also accept an `args` array;
Run and Build accept `optimize: false`. Arguments are passed directly to the process.

## Debugging Foster

Press **F5** to debug the active Foster package (or standalone file when no package is found).
Use **Ctrl+F5** to run the selected launch configuration without debugging. The editor's debug
button and **Foster: Debug Current File/Package** commands provide explicit targets.

Set breakpoints in the gutter, then use Continue, Pause, Step Over, Step Into, and Step Out.
The Call Stack and Locals views show the paused program. Hover and Watch support visible local
and parameter names. Values are read-only; values released by normal lifetime handling appear as
`<unavailable>`. Output appears in the Debug Console. Stop terminates the launched process.

To persist arguments and environment settings, add a Foster configuration to `.vscode/launch.json`:

```json
{
  "version": "0.2.0",
  "configurations": [{
    "type": "foster",
    "request": "launch",
    "name": "Debug Foster package",
    "program": "${workspaceFolder}",
    "cwd": "${workspaceFolder}",
    "args": [],
    "env": {},
    "stopOnEntry": true
  }]
}
```

Use `"program": "${file}"` for a standalone file. Debugging runs checked bytecode with optimization
disabled. It currently observes the main VM thread: remote workers and cleanup callbacks are not
stepped or paused. Source locations require source files; precompiled library bodies without
source metadata remain executable but cannot be stepped through. Compound values have bounded
text previews. Expression evaluation, variable mutation, conditional breakpoints, logpoints,
exception breakpoints, native executable debugging, and attach are not implemented. Pause takes
effect at the next executable source location, so blocking host calls must return first.

## Development

Build the compiler and extension from the repository root:

```powershell
cargo build
cd editors/vscode
npm install
npm run compile
```

Open `editors/vscode` in VS Code and press `F5` to launch an Extension Development Host. A
development session without a staged server automatically finds `target/debug/foster.exe` (or
`target/debug/foster` on Unix), then falls back to `foster` on `PATH`.

For everyday editing, use an optimized compiler: run `cargo build --release --bin foster`, then
set `foster.server.path` to the absolute path of `target/release/foster.exe` (or `foster` on Unix).
Debug builds are useful for compiler debugging but can make semantic editor requests much slower.

Use **Foster: Restart Language Server** after rebuilding the compiler. Set
`foster.server.trace` to `messages` or `verbose` when diagnosing protocol traffic.
Use **Foster: Show Language Server Output** to inspect compiler output and protocol failures. The
extension also reports language-server startup, communication, and unexpected-exit errors as VS
Code notifications instead of leaving them only in the extension-host log.

Hover over a declaration or use **Go to Definition** (`F12`, or Ctrl+click on Windows/Linux) to
inspect and navigate the resolved symbol. Signature help appears after `(` and `,`. VS Code shows
type and parameter hints by default; they can be toggled with **View: Toggle Inlay Hints** or the
`editor.inlayHints.enabled` setting. When a document does not compile, the server safely reuses
semantic snapshots only for functions whose complete source is unchanged and remaps them to the
current buffer. Hover, definition, signature help, references, document symbols, and inlay
hints remain available in unchanged functions. Completion also reads the current parse to
suggest parameters, preceding local bindings, declarations, and imports inside the function
being edited, including common unfinished bodies. Inferred types and member suggestions still
need a usable semantic snapshot. Rename waits for a current package snapshot.

Use **Quick Fix** (`Ctrl+.` on Windows/Linux) on an unresolved name or type to see spelling
suggestions and available imports. Fixes use versioned edits and are withheld for stale
diagnostics. Import suggestions cover public library declarations and modules already present
in a published package snapshot.

Syntax recovery synchronizes at top-level declaration boundaries. A malformed function, constant,
type, or test is omitted from the current semantic snapshot, while complete declarations after it
continue to provide symbols, navigation, hover, completion, and type checking. All independently
recoverable syntax errors are published in the same diagnostic pass.

## Formatting and workspace symbols

Use **Format Document** (`Shift+Alt+F` on Windows) to apply the same formatting as
`foster fmt`. Foster uses four-space indentation and LF line endings. Formatting
uses unsaved editor text, preserves unchanged text where possible, and reports
syntax errors without applying a partial result. VS Code's `editor.formatOnSave`
setting can enable this automatically for Foster files.

Use **Go to Symbol in Workspace** (`Ctrl+T` on Windows) to find functions, types,
constants, methods, and enum cases. Unopened `.fos` files in every workspace folder
are indexed in the background. Search supports case-insensitive prefixes,
substrings, and subsequences; unsaved editor declarations override disk entries.

## Packaging

Build a release compiler and create a VSIX for the current platform:

```powershell
cargo build --release --locked
cd editors/vscode
npm install
npx vsce package --target win32-x64
```

The `vscode:prepublish` hook bundles the TypeScript extension, copies the release compiler, stages
the Foster core sources for navigation, and includes both project licenses. Cross-compiled or CI
builds can set `FOSTER_SERVER_PATH` to the exact compiler binary before invoking `vsce`.

## Local installation

After packaging, install the generated VSIX from the command line:

```powershell
code --install-extension .\foster-language-support-win32-x64-0.1.0.vsix
```

Alternatively, open the Extensions view in VS Code, select the **...** menu, choose
**Install from VSIX...**, and select the generated file. Reload VS Code if prompted. To install a
newly rebuilt VSIX over the existing version, add `--force` to the command above.

Confirm the installation by opening a `.fos` file and checking that its language mode is
**Foster**. The installed extension uses its bundled release compiler unless `foster.server.path`
is configured.

Publish each platform-specific VSIX with the same extension version:

```powershell
npx vsce publish --packagePath foster-language-support-win32-x64-0.1.0.vsix
```

Build Unix VSIX files on Unix so the staged `server/foster` executable retains its executable bit.
