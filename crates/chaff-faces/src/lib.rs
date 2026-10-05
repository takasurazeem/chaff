//! Face detection and grouping.
//!
//! # Why this is a separate crate
//!
//! ONNX Runtime is a large native dependency that downloads platform binaries at build
//! time. In `chaff-core` it would make every engine test depend on it and make the CI
//! matrix fetch ONNX Runtime three times over. ADR-0002 chose in-process ONNX over a
//! Python sidecar for the *product*; this is the same choice expressed in the build.
//!
//! # The licence question, answered before the model was added
//!
//! Faces are biometric data and the models that read them carry different licences. The
//! default engine here is **YuNet** (OpenCV Zoo, MIT) — permissive, small, and good enough
//! to find faces. The more accurate InsightFace `buffalo_l` pack is **non-commercial** and
//! is issue #44: opt-in, with the licence shown at the point of download rather than buried.
//!
//! # What leaves this machine
//!
//! Nothing. Detection runs locally, and embeddings live in the catalog.

pub mod align;
pub mod cluster;
pub mod detect;
pub mod embed;
pub mod engine;
pub mod eval;
pub mod models;
pub mod pass;

pub use align::{similarity_transform, warp, Affine, ALIGNED};
pub use cluster::{cluster, Cluster, ClusteringConfig};
pub use detect::{Detection, Detector};
pub use embed::{cosine, EmbedError, Recogniser, DIMENSIONS};
pub use engine::{FaceEngine, YuNet};
pub use eval::{score, sweep, Corpus, Score, Source};
pub use models::{ModelError, ModelSpec, ModelStore, SFACE};
