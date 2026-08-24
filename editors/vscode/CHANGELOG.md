# Change Log

## 0.1.0

- Add `.fos` syntax highlighting and editing configuration.
- Add package-aware diagnostics, hover documentation, navigation, references, rename, completion,
  signature help, document symbols, and inlay hints through the Foster language server.
- Bundle the Foster compiler and core-library sources in platform-specific extension packages.
- Add **Foster: Run Current File**, **Foster: Run Current Package**, and an editor-title run button.
- Surface language-server startup, communication, and crash errors in VS Code and add a command to
  open the Foster language-server output.
- Add `Arguments` completion with automatic `std.process` import, and keep bundled-library
  diagnostics out of application workspaces unless those sources are explicitly opened.
- Discover `foster.toml` project roots for package runs while retaining legacy `main.fos`
  package discovery.
- Bundle the Foster-written TOML 1.1 parser, renderer, table lookup API, and source-positioned errors.
