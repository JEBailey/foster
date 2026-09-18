# Experimental Foster lexer

This standalone Foster project explores implementing source scanning in Foster.
The production compiler uses the Rust lexer in `src/lexer.rs`; it does not load
or execute this tool. No bytecode seed or token cache is required.

`src/main.fos` accepts one argument containing source text and returns a structured
`Scan`. The project works with both the VM and native backends.

```powershell
cargo run --release --bin foster -- check tools/lexer
cargo run --release --bin foster -- test tools/lexer
cargo run --release --bin foster -- run tools/lexer -- 'func main() { 42 }'
```

The tool's tests cover Unicode positions, malformed tokens, nested comments,
documentation comments, and escape decoding. Keep this experiment separate from
production scanning until self-hosting becomes a priority.

Positions use half-open UTF-8 byte ranges and one-based scalar columns. For an
unescaped string, `raw_start` and `raw_end` identify the payload in the original
source; otherwise they are -1 and `text` holds the decoded payload.
