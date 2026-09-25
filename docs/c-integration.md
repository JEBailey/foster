# C integration

Foster calls C through explicitly compiled bridges on Windows x86-64. The same
bridge DLL and resource runtime serve VM bytecode and native executables.
`foster bridge` generates a C adapter and ordinary Foster bindings; no new Foster
declaration syntax is needed.

The generator is written in [Foster](../tools/cbind/src/bridge.fos): it validates
the manifest, computes its schema identifier, and emits both source files.
JSON decoding, filesystem operations, and Clang invocation are also Foster code.
The Rust compiler command only embeds and invokes that program. A separately
compiled `cbind.exe` runs without the Foster compiler. Native DLL loading, calls,
and resource storage remain in the runtime. JSON decoding rejects duplicate
object keys; the Foster validator rejects unknown fields and invalid contracts.

For existing headers, [tools/cbind](../tools/cbind/README.md) uses Clang and a
Foster generator to import supported scalar and value-struct functions and build the `.fos` module
and bridge in one command. It reports declarations that require explicit pointer
or resource contracts rather than inferring ownership from C types.

Binding manifests, C sources, headers, and DLLs are trusted native code. The
manifest author must accurately describe allocation, cleanup, and pointer
contracts. Normal Foster callers never receive a pointer or an editable resource
token. C functions must not unwind, use `longjmp` across Foster frames, invoke
unregistered callbacks, or retain temporary input pointers.

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

The generated bindings contain the bridge's module path and schema identifier.
Relative module paths resolve at run time against `FOSTER_BRIDGE_DIR`, the
current directory, and the executable directory, trying the full name before
the bare file name; absolute paths load as given. Regenerate bindings when the
bridge moves to a location the resolver cannot reach. Both native executables
and VM programs need the DLL at execution time; native builds do not currently
statically link third-party C code. Dependent DLLs resolve beside the bridge or
in System32, never by searching the working directory.

Building always invokes the compiler. There is no bridge cache to become stale
when a transitive header, compiler, source, or linked library changes. Running
source, bytecode, or native executables never invokes the C compiler implicitly.

## Binding manifest

The JSON manifest has `abi: 1`, `headers`, and `operations`. Optional `sources`,
`include_directories`, and `libraries` are filesystem paths relative to the
manifest directory. Libraries are explicit linker input files, such as `.lib`
files. Optional `resources` declares opaque owning pointer types. Unknown fields,
duplicate names, invalid identifiers, and unsupported contracts are errors.

Operations optionally carry `parameter_names`, an array matching `parameters`
(empty strings retain generated names). Operations, records, fields, and constants
may carry a `documentation` string. The generated source preserves documentation
and disambiguates parameter names without changing the C call signature.

The optional `constants` array contains `{ "name", "type", "value" }` entries.
Types use the scalar/record/array/enum notation below, plus `"string"` for literal strings.
Values are JSON numbers, booleans, strings, or objects matching the record's
fields. Primitive entries generate `pub const C_NAME`; record entries generate
`pub func C_NAME() -> CRecord` factories. Enum members use integer values so aliases
and flags remain usable. Constants are captured during header conversion and
embedded in the Foster module, with no DLL call on access. Full header discovery
also builds and executes a Clang constant exporter, including in manifest-only mode.
The minimum Int value is emitted as a factory as well, because Foster's literal
grammar cannot represent its positive magnitude in a constant initializer.

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
| `{"record":"Color"}` | `CColor` | Struct by value, copied field by field. Nested value records are supported. |
| `{"array":"f32","length":4}` | `List<Float>` | Fixed-size record field; exact length checked before C runs. May nest arrays or contain value records. |
| `{"enum":"Mode"}` | `CMode` (alias of `Int`) | Named C enum, preserving its C type and checking its integer range. |
| `char` | `Int` | Plain C char storage as a byte value 0–255, including standalone parameters/results and array fields. |
| `long`, `ulong` | `Int` | Exact C `long` / `unsigned long`, range checked using C target limits; 32-bit on Windows x64. |
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

An optional `records` array declares value structs, for example:

```json
{"name":"Color", "c_type":"struct Color", "fields":[
  {"name":"r", "type":"u8"}, {"name":"g", "type":"u8"},
  {"name":"b", "type":"u8"}, {"name":"a", "type":"u8"}
]}
```

Operation parameters/results refer to these with `{"record":"Color"}`. Record
`c_type` accepts a typedef or `struct Tag`. Each field must be a supported scalar
or another value record, a named enum, or a fixed-size array. Pointer fields,
unions, and bitfields are not imported. Generated C checks field types and uses the C compiler's native ABI,
including packed structs. Generated Foster records expose copied fields and
`copy()`. Keyword fields and fields starting `c_` gain a `c_` prefix. Nesting is
limited to 32 record/array levels and a flattened record to 8191 scalar slots (65528 bytes).
Metadata bits 16..31 carry the fixed result byte count, so small returned structs
do not allocate the 16 MiB buffer used for variable-length byte results.

An optional `enums` array declares `{ "name":"Mode", "c_type":"enum Mode" }`;
`c_type` may also name an enum typedef. These produce transparent Foster aliases,
allowing flag combinations and unnamed values within the C representation's range.
Array dimensions must be positive integers. Every dimension contributes to the
nesting and slot limits. Array fields use owned lists and are deeply copied by
record `copy()` methods; character arrays preserve bytes, including embedded NULs.
Arrays are not valid top-level operation parameters or results: C array parameters
decay to pointers and require explicit buffer contracts. Constant record values may
contain JSON arrays matching the declared dimensions.

The [foster-raylib](../../foster-raylib) repository (sibling of this one) exercises these bindings in a
graphics UI with a click counter, color slider, and animated rectangle.

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

Schema IDs use FNV-1a over the decoded JSON tree with sorted object keys and
ordered arrays. Whitespace and object-key order do not affect the ID. Explicit
optional fields remain part of the contract, so adding a default-valued field
can change it. Regenerate the bridge and bindings together after contract edits.

## Reviewed pointer and resource contracts

Header imports accept `--contracts FILE.json`. The file contains `operations`,
`resources`, optional `callbacks`, and an optional `exclude` array of function names. Operations use the
same schema as a manifest and replace discovered operations by name/symbol.
Ownership and retention must be reviewed; the importer cannot infer them from a
pointer type. All pointer parameters below are valid only during the C call and
must not be retained by C.

| Contract | Foster interface | C interface |
| --- | --- | --- |
| `{"out":"i32"}` | No input; returned output value | Zero-initialized `int32_t *` |
| `{"inout":{"record":"Point"}}` | Copied `CPoint` input and updated output | `Point *` to a temporary copy |
| `{"buffer":"u8","length":"i32"}` | `Bytes` input | `const uint8_t *`, `int32_t` element count |
| `{"buffer":"f32","length":"size"}` | `List<Float>` input | `const float *`, `size_t` element count |
| Buffer with `"mutable":true` | Input copy plus updated output | Writable temporary buffer |
| Buffer with `"output":true` | `Int` capacity input plus output | Zero-initialized writable buffer |

Buffer length types are `i32`, `u32`, or `size`. Byte, char, and void buffers use
`Bytes`; other supported scalar, enum, and value-record elements use `List<T>`.
Output-only buffers return the full capacity, including untouched zeroed elements;
use an explicit output parameter for a library's actual written count. Negative,
oversized, or unrepresentable inputs fail before calling C. All temporary native
allocations are freed on success and failure. Mutating a copy leaves the caller's
original Foster value unchanged. Array elements must be wrapped in a value record.

One output is returned directly. Multiple outputs produce `CResult_<name>` with
`result` for a non-void C return and `outN` for each output parameter, where N is
its zero-based logical manifest parameter index. Components are copied and framed
with checked lengths; no native pointer is exposed.

A `c_string` result requires `max_length` (1 through 16777216, including the
terminator) and exactly one of `borrowed_result:true` or `release:"deallocator"`.
It copies through the first NUL within that bound, then validates UTF-8. Null and
unterminated strings are errors. Owned strings are released even on failure.
The bound limits scanning; the reviewed C contract must guarantee readable memory
through a terminator or that bound. A `bytes` result can select `length_type` for
its final output-length pointer and can declare `borrowed_result:true` instead of
a deallocator. Output allocation sizes remain bounded by the bridge packet limit.

Resource declarations can set `by_value:true` for owning C structs such as raylib
`Image`. The bridge boxes the returned struct and calls its by-value `destroy`
function exactly once before freeing the box. An optional `valid` predicate
rejects invalid constructors after calling their destructor. Such a destructor
must accept invalid/empty values. These owners cannot also specify a custom
fallible `close` contract. Operations normally pass the boxed value by value;
`receiver_pointer:true` passes its address for native mutation. Pointer owners
continue to pass their native pointer. Owners support explicit idempotent close
and scope cleanup through `CResource`; copying a value record never creates an owner.

See `contracts.json` and `resources.fos` in the [foster-raylib](../../foster-raylib) repository for an
Image creation, resize, PNG export/import, and cleanup example.

## Callbacks

A reviewed `callbacks` entry describes copied argument values, the handler method,
and delivery. Each corresponding operation parameter consumes two adjacent C
parameters: the function pointer and `void *context`. The callback itself takes
that context as its last parameter. For example:

```json
"callbacks": [
  {"name":"Visitor", "method":"visit", "parameters":["i32"],
   "result":"i32", "delivery":"direct", "failure":"zero"}
]
```

When importing a named C callback typedef, add `"c_type":"Visitor"` to its
callback definition. The importer verifies that the typedef was discovered and
the generated C checks its complete function-pointer type. This resolves the
typedef's unsupported-report entry as well as reviewed operations using it.

An operation parameter `{"callback":"Visitor"}` generates a generic Foster
handler parameter. The handler supplies `visit(self, value: Int) -> Int`; it can
mutate its own state. The wrapper borrows it until the C call returns. C must not
retain either the callback or its context after that call.

`{"callback":"Visitor", "retained":true}` requires an operation with `creates`
and an owning resource contract. Call the generated constructor with
`move handler`. The returned resource owns the handler, rejects borrowed captures,
and cannot cross a remote boundary. Its destructor must unregister and quiesce
native callbacks before returning. Explicit close and scope cleanup destroy the
native registration first, then release the handler. Failed registration releases
the handler too.

`"delivery":"direct"` invokes Foster only on the registering thread. Wrong-thread
and recursive calls fail without executing the handler. A non-void result requires
`"failure":"zero"`: C receives an all-zero value if the handler fails; the enclosing
wrapper or `check_callbacks()` reports the failure as `CError`. Foster failures do
not unwind across the C boundary.

`"delivery":"queued"` requires a retained, void-returning callback. C threads copy
events into a bounded queue (4096 events, 16 MiB total). Call
`registration.poll_callbacks()` on the owner thread to deliver the snapshot pending
at entry. Handler failures are reported after processing that snapshot; later events
remain for the next poll. Queue overflow is reported and the overflowing event is
not enqueued. This is explicit polling, not an implicit event loop.

Callbacks currently accept scalars, enums, and copied value records (including
array fields). Pointer arguments, borrowed callback buffers, context-free callbacks,
multiple callback pairs in one operation, and nested callable fields in retained
handlers are not supported. In particular, raylib's context-free audio callbacks
need an additional adapter contract; importing their declaration alone does not
make them safe to retain.

Generated DLLs export `foster_c_callbacks_init` only when needed. Runtime callback
ABI version 1 installs a packet dispatcher; opaque monotonic tokens identify
handlers. Neither a Foster object address nor a C function address enters Foster
source. Stale tokens fail instead of dereferencing released handler memory.

The complete handler and registration example is in
[`tests/fixtures/c_bridge/callbacks.fos`](../tests/fixtures/c_bridge/callbacks.fos),
with its matching header and manifest alongside it. `tests/c_callbacks.rs` runs
it on the VM and both native optimization modes.

## Scope and limitations

Use generated bindings for ordinary application code. `std.ffi.CBridge`,
`CArguments`, `CValue`, and `CResource` also support handwritten binding wrappers.
Their fields are private; only the bridge runtime can adopt an allocation.

Calls are synchronous and are rejected inside remote tasks. Concrete resource
transfers are rejected by the type checker; VM transfer checks and native
specialization checks also cover resources hidden behind generic wrappers.

This implementation does not support borrowed foreign memory, parent/child
resource dependencies, arbitrary callback signatures, variadic functions, unions, bitfields or buffers of pointers,
multiple resource arguments, transferring resource ownership to arbitrary C
operations, unrestricted header importing, cross compilation, relocatable package
bundling, or Linux/macOS. These require additional contracts, rather than exposing
unchecked pointers through the initial API.

The executable fixture and VM/native parity tests live in
`tests/fixtures/c_bridge` and `tests/c_integration.rs`. Struct import, nested and
packed layouts, range checks, and both native optimization modes are covered by
`tests/c_header_tool.rs`.
