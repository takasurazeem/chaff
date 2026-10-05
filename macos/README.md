# Chaff for macOS, natively

A SwiftUI shell around the same Rust engine the web app uses.

## Why two shells

`crates/chaff-core` (21,602 lines) and `crates/chaff-faces` (3,639) have **zero** GUI
dependencies — verified three ways by a reviewer: `grep -rn tauri crates/` returns two comments
and no code; `cargo tree` shows no webview crate; no uniffi annotation anywhere in the engine.
`chaff-cli` already runs the whole thing on a headless Linux box.

So the engine is the shared part and the shell is the replaceable part. macOS gets a native one;
Windows and Linux keep the web one.

## Building

```bash
cd macos
make build     # cargo builds the FFI library, then swift builds the shell
make run
make test
```

`make build` does both in order, because `uniffi-bindgen` reads the built library's metadata
rather than re-parsing the source.

## What is native here, and what is not

**Native:** the window, the sidebar, the grid, the menu bar (⌘O is discoverable in a way a web
app's shortcut cannot be), `NavigationSplitView`, `ContentUnavailableView`, `NSSOpenPanel`, and
`NSCache` for thumbnails.

**Not yet:** culling, the inspector, faces and tags. Tracked as #65, #66, #67.

## Four things that were wrong first, and why each is written down

Every one of these produced an error naming something other than the cause.

1. **`systemLibrary` cannot hold Swift.** The generated bindings went into the C module target
   and every FFI type became "cannot find type in scope".
2. **The module must be named `chaff_ffiFFI`.** The bindings do `#if canImport(chaff_ffiFFI)`,
   so a target named anything else silently skips the import.
3. **A C target with only headers produces no object file** — SwiftPM fails at link time with
   `Build input file cannot be found: .../chaff_ffiFFI.o`, naming a file the target was never
   going to produce. Hence `shim.c`.
4. **`-lchaff_ffi` picks the dylib.** Cargo produces both, and the linker rejected the dylib
   with `mis-aligned LINKEDIT string pool` — a message about internal file layout that says
   nothing about having chosen the wrong one of two. The `.a` is named by path.

Plus `-lc++`: LibRaw is C++, and the failure reads as a Rust problem and is not one. The Linux
build needs `-lstdc++` for the same reason.

## Measured

```
binary:  5 MB     (the Tauri build is 33 MB)
memory:  94 MB
WebKit:  none — otool shows no webview library
```
