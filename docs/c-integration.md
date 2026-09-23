# C integration

Foster calls C through explicitly compiled bridges on Windows x86-64. The same
bridge DLL and resource runtime serve VM bytecode and native executables.
`foster bridge` generates a C adapter and ordinary Foster bindings; no new Foster
declaration syntax is needed.

Binding manifests, C sources, headers, and DLLs are trusted native code. The
manifest author must accurately describe allocation, cleanup, and pointer
contracts. Normal Foster callers never receive a pointer or an editable resource
token. C functions must not unwind, use `longjmp` across Foster frames, call back
into Foster, or retain temporary input pointers.

## Build and use

Use a Clang-compatible C compiler configured for Windows x86-64 (default: `clang`).

```powershell
cargo run --bin foster -- bridge tests/fixtures/c_bridge/bindings.json --output target/fixture.dll
```

The command produces:

- `fixture.dll`: the native bridge and supplied C sources/libraries.
- `fixture.bridge.c`: inspectable generated adapter.
- `fixture.fos`: typed Foster functions and private resource wrappers.
- `fixture.schema`: the binding contract identifier.

Copy the generated Foster module into the application's source tree and import
it by its file's module name. Its `c_` functions marshal arguments and return
`Result<T, CError>`. For example, the fixture exports `c_add`, `c_create`, `c_get`,
and `c_set`; its owner type is `NativeCounter`. `close()` attempts explicit
closing. An owner that remains live is destroyed when its Foster scope ends,
including an early return, loop transfer, `try` failure, or runtime assertion.

The generated bindings contain the DLL's absolute path and schema identifier.
Keep the DLL available at that path. Rebuild bindings when relocating it. Both
native executables and VM programs need the DLL at execution time; native builds
do not currently statically link third-party C code. Dependent DLLs resolve beside
the bridge or in System32, never by searching the working directory.

Building always invokes the compiler. There is no bridge cache to become stale
when a transitive header, compiler, source, or linked library changes. Running
source, bytecode, or native executables never invokes the C compiler implicitly.

## Binding manifest

The JSON manifest has `abi: 1`, `headers`, and `operations`. Optional `sources`,
`include_directories`, and `libraries` are filesystem paths relative to the
manifest directory. Libraries are explicit linker input files, such as `.lib`
files. Optional `resources` declares opaque owning pointer types. Unknown fields,
duplicate names, invalid identifiers, and unsupported contracts are errors.

```json
{
  "abi": 1,
  "headers": ["counter.h"],
  "sources": ["counter.c"],
  "resources": [
    {
      "name": "Counter",
      "c_type": "Counter",
      "destroy": "counter_destroy",
      "close": "counter_close",
      "close_consumes_on_error": false
    }
  ],
  "operations": [
    {"name": "create", "symbol": "counter_create", "parameters": ["i64"], "result": "void", "creates": "Counter"},
    {"name": "get", "symbol": "counter_get", "result": "i64", "receiver": "Counter"}
  ]
}
```

`c_type` names a C typedef. A constructor has `creates`, `result: "void"`, and a C
return type of `Counter *`. Null reports an error. A non-null result transfers
exactly one owning allocation to Foster. Constructors with status/out-pointer
APIs need a small C adapter that cleans up failure results and returns null.

A receiver operation takes `Counter *` as its first C argument. It borrows the
object for the duration of the call and must not free or retain ownership of it.
Generated receiver methods conservatively require mutation access. Calls reject
an incompatible resource kind before passing any pointer to C.

`destroy` has signature `void counter_destroy(Counter *)` and must unconditionally
destroy a live object. It is the fallback cleanup used by `deinit`. Optional
`close` has signature `int counter_close(Counter *)`:

| Close outcome | Ownership |
| --- | --- |
| Zero | C has destroyed the object; clear the Foster token. |
| Nonzero, `close_consumes_on_error: false` | The object remains live; it can be retried or destroyed by `deinit`. |
| Nonzero, `close_consumes_on_error: true` | C has destroyed the object; report the error and clear the token. |

Without `close`, explicit closing invokes `destroy` and succeeds. Closing an
already closed wrapper succeeds; calling another operation on it returns an error.
This initial resource model needs an unconditional destructor. APIs whose cleanup
depends on live children need a more specific ownership adapter.

## Values and wire contract

| Manifest type | Foster binding type | C contract |
| --- | --- | --- |
| `void` | `()` result only | No result payload. |
| `bool` | `Bool` | `_Bool`, only 0 or 1 accepted. |
| `i8`, `i16`, `i32`, `i64` | `Int` | Exact-width signed C integers; narrow inputs are range checked. |
| `u8`, `u16`, `u32`, `u64` | `Int` | Exact-width unsigned C integers; narrow inputs are range checked. U64 preserves all bits, so values above INT64_MAX appear negative in Foster. |
| `f32`, `f64` | `Float` | C float/double. Float32 narrows with a finite-overflow check and widens on return. |
| `bytes` parameter | `Bytes` | Two C parameters: `const uint8_t *`, `size_t`. Input is copied and valid only during the call. |
| `c_string` parameter | `String` | One `const char *`. Copied, NUL-terminated UTF-8; interior NUL and invalid encoding are rejected before C runs. |
| `bytes` result | `Bytes` | C returns `uint8_t *` and accepts a final `size_t *` output-length parameter. Required `release` names its deallocator. Foster copies the bytes, then releases the C allocation. |

A copied bytes result can be decoded with `String.from_utf8`. No unbounded
NUL-terminated output scan is performed. Buffers support embedded zero bytes;
an empty bytes result may have a null pointer. Nonempty null output is an error.
Input packets and output buffers are limited to 16 MiB. Oversized allocated
outputs are released before reporting failure.

The generated adapter assigns each C symbol to an exact function-pointer type;
header/signature disagreements fail compilation. The bridge ABI consists of
version/schema queries, operation metadata, call, close, and destroy exports.
Scalar slots are eight little-endian bytes. Signed integers use two's-complement
bits, floats use binary64 bits, and input buffers have an eight-byte length prefix.
C-string lengths include their terminating NUL. No native struct layout is shared.

Foster's private runtime intrinsics transport these packets as hexadecimal text
using the existing managed-string ABI. The C-facing ABI uses byte pointers and
lengths. This prioritizes one verified implementation for both backends over
marshalling speed. Error responses carry copied UTF-8 messages. Close responses
also carry an explicit consumed flag and signed status; ownership never depends
on guessing from the sign of an error code.

The bridge and wrapper schema identifiers must agree. This identifier detects
stale binding contracts; it is not an authenticity signature. Tokens are unique
within the process, never reused, and checked against resource kind and owning
thread. Internal runtime value sharing does not create another native owner.

## Scope and limitations

Use generated bindings for ordinary application code. `std.ffi.CBridge`,
`CArguments`, `CValue`, and `CResource` also support handwritten binding wrappers.
Their fields are private; only the bridge runtime can adopt an allocation.

Calls are synchronous and are rejected inside remote tasks. Concrete resource
transfers are rejected by the type checker; VM transfer checks and native
specialization checks also cover resources hidden behind generic wrappers.

This implementation does not support borrowed foreign memory, parent/child
resource dependencies, callbacks, variadic functions, C structs/unions by value,
multiple resource arguments, transferring resource ownership to arbitrary C
operations, automatic header importing, cross compilation, relocatable package
bundling, or Linux/macOS. These require additional contracts, rather than exposing
unchecked pointers through the initial API.

The executable fixture and VM/native parity tests live in
`tests/fixtures/c_bridge` and `tests/c_integration.rs`.
