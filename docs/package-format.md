# Foster executable package format

Status: container version 1, implemented by `foster::archive`.

A Foster package (`.fpk`) is a deterministic ZIP archive containing an executable `.fbc` program
and optional application resources. It is analogous to an executable JAR: the Foster runtime is
required, but source files and a separate resource directory are not.

## Layout

```text
application.fpk
├── META-INF/
│   └── foster.json
├── app/
│   └── main.fbc
└── resources/
    └── ...
```

The UTF-8 manifest is JSON:

```json
{
  "format": 1,
  "entrypoint": "app/main.fbc",
  "resources": "resources/"
}
```

Version 1 requires these exact values. Unknown archive entries and additional manifest properties
are reserved for future extensions and ignored.

## Building and running

```powershell
foster pack path/to/package -o application.fpk
foster pack main.fos --resources assets -o application.fpk
foster run application.fpk
```

When the input is a directory and contains `resources/`, that directory is included automatically.
`--resources` overrides the default. Resource paths retain their hierarchy beneath the archive's
`resources/` directory.

During `foster run`, bytecode is decoded directly from the archive. Resources are safely written to
an isolated temporary working directory and are available to Foster code at relative paths such as
`resources/config/default.json` through `std.fs`. The previous working directory is restored and
the temporary directory is removed after the program exits.

## Determinism and validation

The canonical writer:

- orders entries by their portable `/`-separated names;
- uses fixed ZIP timestamps and permissions;
- emits a canonical manifest layout; and
- rejects symbolic links, non-UTF-8 paths, non-portable components, and case-only path collisions.

The reader rejects absolute paths, backslashes, `.` and `..` components, duplicate or case-only
duplicate entries, unsupported manifests, missing bytecode, malformed ZIP data, and invalid `.fbc`
programs. It accepts at most 100,000 entries, a 1 MiB manifest, 256 MiB of bytecode, 128 MiB per
resource, and 512 MiB of resources in total. These limits constrain decompression bombs and memory
use before extraction.

The `.fpk` container version and the embedded `.fbc` bytecode version are independent. A container
format change does not by itself require a bytecode format change, or vice versa.
