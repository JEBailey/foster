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

## Development

Build the compiler and extension from the repository root:

```powershell
cargo build
cd editors/vscode
npm install
npm run compile
```

Open `editors/vscode` in VS Code and press `F5` to launch an Extension Development Host. When the
extension is used from this repository it automatically finds `target/debug/foster.exe` (or
`target/debug/foster` on Unix). Otherwise, install `foster` on `PATH` or set
`foster.server.path` to the executable's absolute path.

Use **Foster: Restart Language Server** after rebuilding the compiler. Set
`foster.server.trace` to `messages` or `verbose` when diagnosing protocol traffic.

Hover over a declaration or use **Go to Definition** (`F12`, or Ctrl+click on Windows/Linux) to
inspect and navigate the resolved symbol. Signature help appears after `(` and `,`. VS Code shows
type and parameter hints by default; they can be toggled with **View: Toggle Inlay Hints** or the
`editor.inlayHints.enabled` setting.
