# Change Log

- Resolve overloaded calls in hover, signature help, and Go to Definition so editor information
  follows the declaration selected from the call's argument count and types.
- Navigate and document `ResourceLocation`, typed `Path` and `Uri` locations, and `File` resource
  capabilities from the bundled Foster standard library.
- Highlight and complete the `not` keyword alias and the `!`, `&&`, and `||` logical operators.
- Advance source-language compatibility to version 7 because `not` is now reserved; ownership and
  bytecode compatibility remain unchanged.
- Navigate, hover, and show signatures for enum instance methods, including the fluent `Option`,
  `Result`, and `Ordering` APIs.
- Move natural scalar, list, string, option, result, ordering, TOML, TCP, byte, and code-point
  operations from module functions to their nominal type or instance.
- Keep inlay hints active in unchanged functions when another function has a syntax or semantic
  failure, with current-buffer position remapping to prevent hints appearing inside source text.
- Use `::` only for module qualification; keep `.` for type accessors, enum cases, runtime fields,
  and instance members.
- Advance source-language compatibility to version 6 for the qualification syntax migration;
  ownership and bytecode compatibility remain unchanged.
- Add highlighting and completion support for the `try` Result-propagation expression.
- Advance source-language compatibility to version 5 because `try` is now reserved; ownership and
  bytecode compatibility remain unchanged.

## 0.1.0

- Add `.fos` syntax highlighting and editing configuration.
- Add package-aware diagnostics, hover documentation, navigation, references, rename, completion,
  signature help, document symbols, and inlay hints through the Foster language server.
- Bundle the Foster compiler and core-library sources in platform-specific extension packages.
- Add **Foster: Run Current File**, **Foster: Run Current Package**, and an editor-title run button.
- Surface language-server startup, communication, and crash errors in VS Code and add a command to
  open the Foster language-server output.
- Complete language-server restarts cleanly when VS Code sends cancellation notifications between
  the LSP `shutdown` request and `exit` notification.
- Navigate `CodePoint` type annotations to the bundled `core.code_point` source module.
- Suppress inlay hints while the current document does not compile so stale source positions never
  place parameter or inferred-type labels inside edited text.
- Add `Arguments` completion with automatic `std.process` import, and keep bundled-library
  diagnostics out of application workspaces unless those sources are explicitly opened.
- Debounce package diagnostics while typing so rapid edits trigger one compilation after the
  latest change instead of recompiling synchronously for every keystroke. Preserve unrelated
  compilation snapshots across edits, reuse failed snapshots until their source changes, and reuse
  parsed modules when rebuilding a package so only changed source is reparsed.
- Widen `Byte` and `CodePoint` losslessly to `Int` in expected-type contexts while retaining
  checked reverse conversions, exact generic inference, and invariant containers.
- Discover `foster.toml` project roots for package runs while retaining legacy `main.fos`
  package discovery.
- Bundle the Foster-written TOML 1.1 parser, renderer, table lookup API, and source-positioned errors.
