//! Chaff core — all logic that is not UI and not Tauri.
//!
//! Everything in this module tree is deliberately free of `tauri::` types so that it
//! can be unit-tested with a plain `cargo test`, with no GUI runtime, no display
//! server, and no webview. See ADR-0001: "keep the Rust core free of Tauri types
//! behind plain traits, so a shell swap is possible without rewriting the engine."

pub mod ext;
pub mod pair;
