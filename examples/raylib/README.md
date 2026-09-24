# Foster + raylib

A graphics UI whose application logic is written in Foster. It has a click
counter, a draggable color slider, and an animated rectangle. The generated
bindings use raylib's actual `Color`, `Rectangle`, and `Vector2` definitions;
there is no handwritten C adapter for these structs.

Requires Windows x64, Clang, and the checkout's Foster compiler. From the
repository root:

```powershell
cargo build --bin foster
./examples/raylib/setup.ps1
./target/debug/foster.exe run examples/raylib
```

Setup downloads the official raylib 6.0 MSVC x64 SDK into `target/raylib`, checks
the pinned archive SHA-256, and runs `tools/cbind`. To use an existing SDK,
pass `-Raylib C:/path/to/raylib-6.0_win64_msvc16`. The upstream license ships in
that SDK. Raylib's project and release are available at
[raysan5/raylib](https://github.com/raysan5/raylib/releases/tag/6.0).

Generated artifacts live in `target/raylib-demo`. The generated Foster module
is copied to ignored `src/raylib.fos`; rerun setup when moving the checkout or
changing bridge generation. `raylib.dll` sits beside the bridge DLL so the loader
can resolve it without PATH changes. No binaries or SDK headers are vendored.

`setup.ps1` explicitly declares the borrowed C-string contracts for drawn text
and screenshot paths. Raylib 6.0 retains the window-title pointer in
[`CORE.Window.title`](https://github.com/raysan5/raylib/blob/6.0/src/rcore.c), so
`window.c` supplies a static title whose storage outlives the window. It performs
no struct conversions. Both headers are processed by the importer.
All value structs are discovered and mapped automatically. `:window` and `:frame` scopes own small Foster wrappers
whose destructors pair `CloseWindow` and `EndDrawing`, including error paths.

For native compilation:

```powershell
./target/debug/foster.exe build examples/raylib --native -o target/raylib-demo/demo.exe
./target/raylib-demo/demo.exe
```

For a bounded graphics smoke test, pass `--smoke`. It renders 90 frames, writes
`target/raylib-demo/smoke.png`, closes the window, and verifies that the window
scope actually released raylib. Run from the repository root for the screenshot
path to resolve:

```powershell
./target/debug/foster.exe run examples/raylib -- --smoke
./target/raylib-demo/demo.exe --smoke
```

The screenshot shows rendering; interactive mouse behavior can be exercised by
running without `--smoke`. Press Escape or close the window to exit.
