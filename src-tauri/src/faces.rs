//! The face pass, now in the engine so the headless CLI can run it too (#57).
//!
//! Moved rather than duplicated: two implementations of "detect, embed, group" would drift,
//! and the one that drifted would be the one nobody was looking at.

pub use chaff_faces::pass::{model_store, recogniser_model, run, similarity, FacePassReport};
