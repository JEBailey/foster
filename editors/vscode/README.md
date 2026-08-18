# Foster Language Support for VS Code

This extension registers `.fos` files, provides Foster syntax highlighting and editing rules,
and launches the Foster language server. Language features include:

- package-wide compiler errors and warnings with open-buffer overlays;
- document symbols;
- go-to-definition across imported modules, instance methods, and repository core-library source;
- find-references across package modules;
- identity-aware local and declaration rename;
- rich Foster signatures and Markdown documentation on hover;
- call signature help with active-parameter tracking;
- inferred local-type and argument-name inlay hints, with clickable parameter hints;
- scope-aware completion for locals, declarations, imports, qualified modules, and keywords;
- automatic diagnostic refresh when Foster files change on disk.

The bundled grammar highlights line comments, nested block-comment delimiters, documentation
comments, code-point literals, effect clauses, sequence types, and structural intersection types.

## Installation

Marketplace and VSIX releases include the Foster compiler, language server, and core-library
sources for the selected platform. No separate compiler installation is required. Set
`foster.server.path` only to override the bundled compiler with a local build.

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

Hover over a declaration or use **Go to Definition** (`F12`, or Ctrl+click on Windows/Linux) to
inspect and navigate the resolved symbol. Signature help appears after `(` and `,`. VS Code shows
type and parameter hints by default; they can be toggled with **View: Toggle Inlay Hints** or the
`editor.inlayHints.enabled` setting.

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

Publish each platform-specific VSIX with the same extension version:

```powershell
npx vsce publish --packagePath foster-language-support-win32-x64-0.1.0.vsix
```

Build Unix VSIX files on Unix so the staged `server/foster` executable retains its executable bit.
