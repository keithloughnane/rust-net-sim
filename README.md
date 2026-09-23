# Emergence Engine

A hierarchical packet-routing network simulation, reimplemented in Rust from the SmitherNet
design in *Smithereen Cold Boot Attack*. It ships as a native library with a C ABI, so it can be
used from Unity (C#), Unreal (C++) or any other engine.

## Layout

| Path | Purpose |
|---|---|
| `crates/emergence-engine` | The engine itself. Pure Rust, no UI, no unsafe code. |
| `crates/emergence-ffi` | C ABI over the engine. Builds `libemergence` (`.dylib`/`.dll`/`.so` and a static lib). |
| `crates/emergence-sandbox` | Desktop test UI (egui). Loads the compiled `libemergence` at runtime, exactly like a game engine would. |
| `bindings/c/emergence.h` | Generated C/C++ header (Unreal, custom engines). |
| `bindings/unity/` | Unity package: generated P/Invoke bindings plus a safe C# wrapper. |
| `bindings/csharp-smoke/` | Compiles the Unity bindings with .NET and runs them against the real library. |
| `xtask/` | Build tasks (`cargo xtask help`). |

## Common commands

```sh
cargo build                 # engine + native library (the workspace default)
cargo test                  # engine and ABI tests
cargo doc --open            # API docs

cargo sandbox               # build the native library and run the test UI against it
cargo xtask bindings        # regenerate the C header and C# bindings after changing the ABI
cargo xtask test-csharp     # run the C# bindings against the native library (needs dotnet)
cargo xtask dist            # package a release build into dist/

cargo fmt --all             # format
cargo check-all             # clippy on everything, warnings as errors
```

The sandbox loads the library from next to its own executable. Set `EMERGENCE_LIB` to load a
different build, for example one from `dist/`.

## Using the library

### Unity

1. `cargo xtask dist`
2. In Unity: *Window → Package Manager → + → Install package from disk…* and pick
   `dist/unity/com.keithloughnane.emergence/package.json`.
3. Use it:

   ```csharp
   using Emergence;

   using var world = new World();
   world.Tick();
   Debug.Log(world.TickCount);
   ```

`dist` only contains the native library for the machine that built it. Other platforms need their
own build (or cross-compilation) added to `Runtime/Plugins/`. On iOS the bindings automatically
switch to the statically linked `__Internal` library.

### Unreal / C++

Include `dist/c/include/emergence.h` and link `libemergence` from `dist/c/lib/<platform>/` via a
ThirdParty module. The header is plain C with `extern "C"` guards, so it works from C++ unchanged.

## Changing the C ABI

1. Edit `crates/emergence-ffi/src/lib.rs`. Keep to its conventions: `emergence_` prefix, opaque
   handles, status codes, no panics across the boundary.
2. If you changed or removed anything existing, bump `EMERGENCE_ABI_VERSION`.
3. `cargo xtask bindings` and commit the regenerated files.
4. Update the safe wrappers: `bindings/unity/Runtime/*.cs` and
   `crates/emergence-sandbox/src/native.rs`.
5. `cargo test && cargo xtask test-csharp && cargo check-all`.
