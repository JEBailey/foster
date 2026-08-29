# Foster Language Support for VS Code

This extension registers `.fos` files, provides Foster syntax highlighting and editing rules,
and launches the Foster language server. Language features include:

- package-wide compiler errors and warnings with open-buffer overlays;
- document symbols;
- go-to-definition across imported modules, selected function and concrete method overloads,
  instance methods, and repository core-library source;
- find-references across package modules;
- identity-aware local and declaration rename;
- rich Foster signatures and Markdown documentation on hover;
- call signature help with active-parameter tracking and the resolved overload's signature;
- inferred local-type and argument-name inlay hints, with clickable parameter hints;
- scope-aware completion for locals, declarations, imports, qualified modules, and keywords,
  including `try`, `not`, and automatic `std.process` import when completing `Arguments`;
- automatic diagnostic refresh when Foster files change on disk;
- cached package snapshots and parsed modules, so edits reparse changed sources while preserving
  unaffected compilation work;
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

The ▶ button in the editor title runs the current file. Foster saves a modified file before
starting it and shows compiler output in the shared Foster task terminal. Use the package command
when the program imports sibling filesystem modules.

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

Use **Foster: Restart Language Server** after rebuilding the compiler. Set
`foster.server.trace` to `messages` or `verbose` when diagnosing protocol traffic.
Use **Foster: Show Language Server Output** to inspect compiler output and protocol failures. The
extension also reports language-server startup, communication, and unexpected-exit errors as VS
Code notifications instead of leaving them only in the extension-host log.

Hover over a declaration or use **Go to Definition** (`F12`, or Ctrl+click on Windows/Linux) to
inspect and navigate the resolved symbol. Signature help appears after `(` and `,`. VS Code shows
type and parameter hints by default; they can be toggled with **View: Toggle Inlay Hints** or the
`editor.inlayHints.enabled` setting. When a document does not compile, the server safely reuses
hints only for functions whose complete source is unchanged and remaps them to the current buffer.
The edited function receives no stale hints, while failures there do not suppress hints elsewhere.

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
