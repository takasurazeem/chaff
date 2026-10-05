//! One detected face.
//!
//! Deliberately not a "person". A detection is a rectangle with five landmarks and a
//! confidence — it says nothing about *who*, and keeping that distinction in the type
//! system is what stops a name from being attached to a face by accident.

use serde::{Deserialize, Serialize};

/// A face found in an image, in the coordinates of the image it was found in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    /// Left edge, in pixels.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The detector's own confidence, 0..1. Not a probability that this is a person — only
    /// that it looks like the pattern the model was trained on.
    pub confidence: f32,
    /// Right eye, left eye, nose, right mouth corner, left mouth corner.
    ///
    /// Kept because they are free — the model emits them alongside the box — and because
    /// they are what an alignment step needs to compare two faces properly. Discarding them
    /// would mean running the detector again later.
    pub landmarks: [[f32; 2]; 5],
}

impl Detection {
    pub fn area(&self) -> f32 {
        self.width * self.height
    }

    /// The centre, which is what a crop is built around.
    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// A face detector.
pub trait Detector: Send + Sync {
    /// Every face in an image.
    ///
    /// `pixels` is RGB8, `width * height * 3` bytes. Taking raw pixels rather than a path
    /// keeps the detector free of any opinion about where images come from — the caller
    /// already knows how to decode, and duplicating that here would duplicate the preview
    /// fallback logic with it.
    fn detect(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Detection>, DetectError>;

    /// A name for logs and the capability report.
    fn name(&self) -> &'static str;
}

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("the image is {width}x{height} but {expected} bytes of RGB were supplied")]
    BufferSize { width: u32, height: u32, expected: usize },
    #[error("the detector rejected the input: {0}")]
    Input(String),
    #[error("inference failed: {0}")]
    Inference(String),
}
