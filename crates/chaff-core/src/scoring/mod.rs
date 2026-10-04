//! Scoring pipeline.
//!
//! Every metric here is deterministic: the same input and the same weights produce the
//! same numbers, always. A score the user cannot reproduce is a score they cannot trust,
//! and "why did it change?" is the failure mode that kills confidence in a culling tool.

pub mod composite;
pub mod exposure;
pub mod focus;
pub mod shoot;
