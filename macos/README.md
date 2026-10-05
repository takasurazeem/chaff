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
make app       # cargo builds the FFI library, swift builds the shell, then bundles a .app
make run       # builds and opens it
make test
```

`make app` matters: `swift build` produces a **bare executable**, which is a developer artefact —
no Dock icon, no window activation, nothing to double-click. `bundle.sh` turns it into
`build/Chaff.app`.

It is **ad-hoc signed**, which is enough for this machine and not enough for Gatekeeper
anywhere else. That is issue #68.

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

## The layout, inspired by Xcode

| Xcode | Chaff |
|---|---|
| Navigator on the left | Folders / Tags / People |
| **Segmented bar** switching navigators | the same — one at a time, not stacked |
| **Filter field pinned to the bottom** | the same |
| Editor in the centre | the grid |
| Inspector on the right | EXIF, score, files |
| **Status line** under the editor | "2,956 photographs" / "412 of 2,956" |

The segmented bar is the part that matters. The web app **stacks** folders, tags and people in
one narrow column — three unrelated lists competing for 200 points of width, with the one you
want usually scrolled off. Xcode's answer has been the same for twenty years: a bar saying what
is available, one navigator visible, and a filter that narrows whatever is showing.

The filter sits at the **bottom** because it is a control *over the list above it*; at the top it
pushes the content down a row for something used occasionally.

## What exists now

| | |
|---|---|
| **#64** library, grid, thumbnails | done, unverified |
| **#65** folders and the inspector | done, unverified |
| **#66** culling: rating, reject, delete, undo | done, unverified |
| **#67** faces and tags | not started |

`⌘O` opens a library. `1`–`5` rate, `X` rejects, `⌫` opens the delete confirmation, `⌘Z` undoes.
All of them are in the **Cull menu**, which is what a native shell buys that a webview cannot:
shortcuts a user can find.

## The delete flow, and why the sheet is the guarantee

`⌫` asks the engine what the selection would move. The engine **hashes every file at that
moment**, while the sheet is on screen, and verifies those hashes when the user confirms. A file
whose contents changed in between aborts the whole operation rather than being moved unexamined.

`commitDelete(root:)` takes **no file list**. The sheet cannot name a file, cannot supply a hash,
and cannot widen the operation — the invariant lives in the engine's `DeleteSession`, shared by
both shells, and it is a type-level test there so relaxing it stops compiling.

## Undo covers two kinds of action

A rating is reversed by **writing the previous value back**. A move is not: the files are in the
trash and undoing it means **restoring the operation**. `Culling`'s stack holds a tagged union
for that reason — one shape would mean one of the two being a lie.
