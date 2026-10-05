//! Chaff's engine.
//!
//! Everything in here is free of `tauri::` types and of any GUI dependency, so it can be
//! unit-tested with a plain `cargo test -p chaff-core` on a machine with no display
//! server and no GTK/WebKit development packages.
//!
//! See ADR-0001 for why the engine is a separate crate rather than a module of the app.

pub mod burst;
/// Re-exported so callers of the engine — the Tauri shell, integration tests — do not
/// need their own `rusqlite` dependency. Two crates depending on two versions of a
/// database library is a class of bug with no upside.
pub use rusqlite;

pub mod catalog;
pub mod delete_session;
pub mod egress;
pub mod exif;
pub mod ext;
pub mod indexer;
pub mod hardware;
pub mod imaging;
pub mod pair;
pub mod pipeline;
pub mod preview;
pub mod raw;
pub mod scoring;
pub mod tagging;
pub mod thumb;
pub mod trash;
pub mod vlm;
pub mod watch;
pub mod xmp;
