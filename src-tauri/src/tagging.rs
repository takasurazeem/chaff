//! The tagging pass, now in the engine so the headless CLI can run it too (#57).
//!
//! Moved rather than duplicated. `chaff-core` already had the VLM client, the thumbnail
//! decoder and the catalog; the pass was the only part in the shell, and it was there for no
//! reason other than where it was first written.

pub use chaff_core::tagging::{diagnose, run, EndpointReport, TagPassReport};
