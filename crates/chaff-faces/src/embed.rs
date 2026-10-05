//! Turning an aligned face into a vector that can be compared.
//!
//! # What an embedding is, and is not
//!
//! 128 floats. Two embeddings being close means "the recogniser thinks these are the same
//! face" — not that they are the same person. The distinction is the whole reason identity
//! lives in a separate table confirmed by a human (#46) rather than being written here.
//!
//! # Preprocessing, from the reference
//!
//! OpenCV calls `blobFromImage(aligned, 1, Size(112,112), Scalar(0,0,0), swapRB=true,
//! crop=false)` — scale 1, **no mean subtraction**, and a **channel swap to RGB**. Feeding
//! BGR, or dividing by 255, both produce embeddings that are stable and wrong: the same
//! face still lands near itself, so the mistake survives a casual test and quietly costs
//! accuracy on everyone.

use std::path::Path;

use crate::align::{similarity_transform, warp, ALIGNED};
use crate::detect::{DetectError, Detection};

/// Dimensions in an SFace embedding.
pub const DIMENSIONS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error(transparent)]
    Detect(#[from] DetectError),
    #[error("the model returned {found} values, expected {DIMENSIONS}")]
    WrongShape { found: usize },
}

/// A face recogniser.
pub struct Recogniser {
    session: std::sync::Mutex<ort::session::Session>,
    input_name: String,
    output_name: String,
}

impl Recogniser {
    /// Load from a file. The caller is expected to have verified it — see
    /// [`crate::models::ModelStore`].
    pub fn from_file(path: &Path) -> Result<Self, DetectError> {
        ort::init().with_name("chaff").commit();

        let session = ort::session::Session::builder()
            .map_err(|e| DetectError::Inference(e.to_string()))?
            .with_intra_threads(2)
            .map_err(|e| DetectError::Inference(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| DetectError::Inference(format!("{}: {e}", path.display())))?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| DetectError::Inference("the model has no inputs".into()))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| DetectError::Inference("the model has no outputs".into()))?;

        Ok(Self { session: std::sync::Mutex::new(session), input_name, output_name })
    }

    /// The embedding of one face, given the image it was found in.
    ///
    /// The landmarks are used to align it onto the template first. Without that, two
    /// photographs of one person at slightly different angles produce crops that differ by
    /// a rotation, and the model reads the rotation as a different face.
    pub fn embed(
        &self,
        pixels: &[u8],
        width: u32,
        height: u32,
        face: &Detection,
    ) -> Result<Vec<f32>, EmbedError> {
        let m = similarity_transform(&face.landmarks);
        let aligned = warp(pixels, width, height, m);
        self.embed_aligned(&aligned)
    }

    /// The embedding of an already-aligned 112x112 RGB buffer.
    pub fn embed_aligned(&self, aligned: &[u8]) -> Result<Vec<f32>, EmbedError> {
        let expected = ALIGNED * ALIGNED * 3;
        if aligned.len() != expected {
            return Err(EmbedError::Detect(DetectError::BufferSize {
                width: ALIGNED as u32,
                height: ALIGNED as u32,
                expected,
            }));
        }

        // NCHW, RGB, raw 0..255 — no mean subtraction, no scaling. See the module docs.
        let plane = ALIGNED * ALIGNED;
        let mut input = vec![0f32; 3 * plane];
        for i in 0..plane {
            input[i] = aligned[i * 3] as f32;
            input[plane + i] = aligned[i * 3 + 1] as f32;
            input[2 * plane + i] = aligned[i * 3 + 2] as f32;
        }

        let tensor = ort::value::Tensor::from_array(([1usize, 3, ALIGNED, ALIGNED], input))
            .map_err(|e| DetectError::Input(e.to_string()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| DetectError::Inference("the session lock is poisoned".into()))?;

        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| DetectError::Inference(e.to_string()))?;

        let value = outputs
            .get(self.output_name.as_str())
            .ok_or_else(|| DetectError::Inference(format!("no output {}", self.output_name)))?;
        let array = value
            .try_extract_array::<f32>()
            .map_err(|e| DetectError::Inference(e.to_string()))?;

        let embedding: Vec<f32> = array.iter().copied().collect();
        if embedding.len() != DIMENSIONS {
            return Err(EmbedError::WrongShape { found: embedding.len() });
        }
        Ok(embedding)
    }
}

/// Cosine similarity, in `-1..=1`.
///
/// Cosine rather than Euclidean because an embedding's *direction* is what carries the
/// identity; its magnitude varies with lighting and exposure. Two photographs of one face
/// under different light point the same way and are different lengths, and Euclidean
/// distance would call that a different person.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= f32::EPSILON {
        // A zero vector has no direction, so it is similar to nothing. Returning 1.0 here
        // — which a naive implementation does, since 0/0 is treated as a match by some
        // guards — would make every failed embedding cluster with every other.
        return 0.0;
    }
    dot / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_a_vector_with_itself_is_one() {
        let v: Vec<f32> = (0..DIMENSIONS).map(|i| i as f32 - 64.0).collect();
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_ignores_magnitude() {
        // The property that makes it the right measure: brightness changes the length of an
        // embedding and not its direction, and brightness is not identity.
        let a: Vec<f32> = (0..DIMENSIONS).map(|i| (i % 7) as f32).collect();
        let b: Vec<f32> = a.iter().map(|v| v * 3.5).collect();
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_of_opposite_vectors_is_minus_one() {
        let a: Vec<f32> = (0..DIMENSIONS).map(|i| (i % 5) as f32 + 1.0).collect();
        let b: Vec<f32> = a.iter().map(|v| -v).collect();
        assert!((cosine(&a, &b) + 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_zero_embedding_is_similar_to_nothing() {
        // **The failure this prevents.** A zero vector has no direction. A guard that
        // returns 1.0 for 0/0 — which several implementations do — makes every failed
        // embedding cluster with every other, and the symptom is "all faces are one person".
        let zero = vec![0f32; DIMENSIONS];
        let real: Vec<f32> = (0..DIMENSIONS).map(|i| i as f32 + 1.0).collect();
        assert_eq!(cosine(&zero, &real), 0.0);
        assert_eq!(cosine(&zero, &zero), 0.0);
    }

    #[test]
    fn mismatched_or_empty_input_is_zero_not_a_panic() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn a_wrongly_sized_aligned_buffer_is_an_error() {
        // The recogniser is reached from a caller that may have produced the crop itself.
        // Silently reshaping a short buffer would run the model on whatever followed it.
        let Some(path) = std::env::var("CHAFF_SFACE").ok().map(std::path::PathBuf::from) else {
            return;
        };
        let Ok(r) = Recogniser::from_file(&path) else { return };
        assert!(matches!(
            r.embed_aligned(&[0u8; 100]),
            Err(EmbedError::Detect(DetectError::BufferSize { .. }))
        ));
    }
}
