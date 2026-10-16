# Editor integrations

[VS Code](vscode/README.md) owns extension commands, grammar, language-client
startup, debugger integration, and packaging. The server and debug adapter are
implemented by the Foster command under `src/lsp/` and `src/debugger/`.

Run from `editors/vscode/` with Node.js and npm:

```text
npm ci
npm run check
npm run test:run-debug
```

Run `cargo build --profile test --bin foster` from the repository root before
the run/debug tests, or set `FOSTER_SERVER_PATH` to a current compiler executable.
Those tests drive real compiler/debugger processes. CI builds the compiler before
running them.

CI runs these checks for editor changes. Server changes use the Rust tooling
suite documented in [tests](../tests/README.md).
