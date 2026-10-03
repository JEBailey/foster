# Foster-written driver tools

The Rust build compiles these Foster sources and bundles them in `foster`:

- `init.fos` creates projects, validates names, and writes starter files.
- `project.fos` loads and validates manifests, selects discovered projects,
  discovers compiled libraries, and resolves transitive dependencies.
- `format.fos` applies indentation, whitespace, and declaration formatting.
- `documentation.fos` builds documentation pages, type summaries, navigation,
  and the module index, and writes the static site.

The formatter is compiled to native code and linked into host builds, so editor
formatting does not interpret the formatting policy on every request. Cross builds
use its embedded bytecode. The other tools are decoded and verified once per
process and execute in the VM. Both paths stop cooperatively on editor cancellation.
Tools use the normal Foster filesystem, path, string, and TOML APIs.
They do not launch another `foster` process or require source files beside the
installed executable. The build script uses the compiler's standalone source API,
so loading the project tool does not recursively load a project manifest.

Rust retains command-line parsing, compiler diagnostics, conversion of tool
results into compiler-facing project records, and platform path ancestry and
boundary comparison. The formatter's Rust entry point validates source with the
production parser before invoking the Foster formatting policy. These adapters
also serve editor requests; there is no second Rust formatting implementation.

Documentation uses a count-prefixed string stream from the Rust adapter:
modules contain type cards and declaration groups, and groups contain overloads.
Optional values have a presence flag, preserving missing versus empty comments.
Rust supplies resolved signatures, type links, and Markdown HTML. Foster escapes
plain labels and summaries and assembles the pages. Styles and browser behavior
live in `documentation/style.css` and `documentation/script.html`; the local
preview server remains in Rust.

Each source is an independent embedded program, rather than modules of one
package. Check and test them individually from the repository root:

```powershell
cargo run --bin foster -- check tools/driver/init.fos
cargo run --bin foster -- check tools/driver/project.fos
cargo run --bin foster -- test tools/driver/project.fos
cargo run --bin foster -- test tools/driver/format.fos
cargo run --bin foster -- test tools/driver/documentation.fos
cargo test --lib project::tests
cargo test --lib formatter::tests
cargo test --lib documentation::
cargo test --test cli
```

Project and CLI integration tests exercise real filesystem operations, including
dependency cycles, aliases, discovery conflicts, and initialization. Formatting
tests retain parser validation and check stable output through the Rust adapter.
