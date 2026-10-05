# chaff-ffi

The engine, for Swift.

## Building the bindings

```bash
cargo build -p chaff-ffi
cargo run -p chaff-ffi --bin uniffi-bindgen -- \
  generate --library target/debug/libchaff_ffi.dylib \
  --language swift --out-dir build/swift
```

Produces `chaff_ffi.swift`, `chaff_ffiFFI.h` and `chaff_ffiFFI.modulemap`. The generator is a
**binary in this crate**, not a `cargo install`, so the version generating the Swift cannot
drift from the version the library was built with — a mismatch produces bindings that compile
and then misbehave at the boundary.

## The rule

**No logic in this crate.** A wrapper converts types, calls the engine, converts the answer.
If a wrapper needs an `if` about what something *means*, that belongs in `chaff-core`, where
both shells get it and where it can be tested without Swift.

## What it must never do

Annotate anything in `chaff-core` with `#[uniffi::export]`. The engine stays a plain library:
no FFI dependency, no scaffolding in its build, and `chaff-cli` keeps working on a headless
box.

## The one piece of duplication, recorded

`band_of` exists here **and** in `src-tauri/src/commands.rs`. Two copies of a threshold is how
two UIs start disagreeing about what "Keep" means. It should move into the engine — issue #63
is the natural place, since that work is already moving shell logic down.
