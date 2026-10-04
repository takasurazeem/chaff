//! Chaff's engine.
//!
//! Everything in here is free of `tauri::` types and of any GUI dependency, so it can be
//! unit-tested with a plain `cargo test -p chaff-core` on a machine with no display
//! server and no GTK/WebKit development packages.
//!
//! See ADR-0001 for why the engine is a separate crate rather than a module of the app.

pub mod burst;
pub mod catalog;
pub mod exif;
pub mod ext;
pub mod indexer;
pub mod imaging;
pub mod pair;
pub mod preview;
pub mod scoring;
