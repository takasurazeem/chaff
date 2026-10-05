//! The engine trait and the default implementation.
//!
//! # The decode, and why it is written out longhand
//!
//! YuNet is anchor-free: it emits, at three strides, a class score, an objectness score, a
//! box offset and five landmarks per cell. Nothing in the ONNX graph turns those into
//! pixels — the decoding lives in OpenCV's C++, which this cannot call. So it is
//! reimplemented here from `modules/objdetect/src/face_detect.cpp`, and the details that
//! matter are:
//!
//! * the score is `sqrt(cls * obj)`, not either one alone;
//! * width and height are `exp(offset) * stride` — the model predicts a **log** size, and
//!   dropping the `exp` produces boxes of a few pixels that look like a threshold problem;
//! * the channel order is **BGR**, because OpenCV's `blobFromImage` is called without
//!   `swapRB`. Feeding RGB still finds faces, slightly worse, which is the kind of error
//!   that survives testing and quietly costs accuracy.

use std::path::{Path, PathBuf};

use crate::detect::{DetectError, Detection, Detector};

/// The strides the model emits predictions at.
const STRIDES: [usize; 3] = [8, 16, 32];
/// The input the model was exported for. Its shape is symbolic, but a fixed size keeps the
/// tensor allocation simple and matches the reference implementation.
const INPUT: usize = 640;

/// Below this, a detection is noise.
///
/// 0.6 is OpenCV's default and the value the model was tuned against. Lower finds more
/// faces and more false ones; the PRD's face work is a *grouping* aid, where a false
/// positive costs a stray cluster and a false negative costs a missing person.
const SCORE_THRESHOLD: f32 = 0.6;
/// IoU above which two boxes are the same face.
const NMS_THRESHOLD: f32 = 0.3;

/// A face engine: something that can find faces in an image.
///
/// A trait rather than a concrete type because the PRD commits to two — a permissive
/// default and an opt-in, more accurate, non-commercially-licensed one (#44). The product
/// must work without the second, and the caller must not have to know which it has.
pub trait FaceEngine: Send + Sync {
    fn detector(&self) -> &dyn Detector;
    fn name(&self) -> &'static str;
    /// The licence, shown wherever the engine is chosen.
    ///
    /// Not a formality: `buffalo_l` is non-commercial, and a user who picks it deserves to
    /// know that at the moment they pick it rather than in a README.
    fn licence(&self) -> &'static str;
}

/// YuNet, from OpenCV Zoo.
///
/// Chosen as the default for three reasons, in order: it is **MIT** rather than
/// non-commercial; it is **233 KB**, so it ships with the application instead of being
/// downloaded on first use; and it is fast enough to run over a library without a GPU,
/// which matters because the CPU-only tier must still work.
pub struct YuNet {
    session: std::sync::Mutex<ort::session::Session>,
    input_name: String,
}

impl YuNet {
    /// Load the model from a file.
    pub fn from_file(path: &Path) -> Result<Self, DetectError> {
        ort::init().with_name("chaff").commit();

        let session = ort::session::Session::builder()
            .map_err(|e| DetectError::Inference(e.to_string()))?
            // Two threads, not all of them: this runs alongside the thumbnail pipeline and
            // the indexer, and a session that takes every core makes the window stutter.
            .with_intra_threads(2)
            .map_err(|e| DetectError::Inference(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| DetectError::Inference(format!("{}: {e}", path.display())))?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| DetectError::Inference("the model has no inputs".into()))?;

        Ok(Self { session: std::sync::Mutex::new(session), input_name })
    }

    /// The model shipped with the application.
    ///
    /// `None` when it is absent, which is a supported state: face detection is an
    /// enhancement, and the rest of the application works without it.
    pub fn bundled() -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("models/face_detection_yunet_2023mar.onnx");
        p.is_file().then_some(p)
    }
}

impl FaceEngine for YuNet {
    fn detector(&self) -> &dyn Detector {
        self
    }
    fn name(&self) -> &'static str {
        "YuNet (OpenCV Zoo)"
    }
    fn licence(&self) -> &'static str {
        "MIT"
    }
}

/// How an image was fitted into the model's square input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub src_width: u32,
    pub src_height: u32,
}

impl Letterbox {
    /// Fit `w`x`h` into a square of `INPUT`, preserving aspect ratio.
    ///
    /// Letterboxed rather than stretched. YuNet is anchor-free and tolerates distortion, but
    /// a 3:2 photograph squashed into a square changes every face's aspect, and the box the
    /// model predicts is then wrong by the same factor — an error that looks like a
    /// slightly loose box rather than a bug.
    pub fn fit(w: u32, h: u32) -> Self {
        let scale = (INPUT as f32 / w as f32).min(INPUT as f32 / h as f32);
        let scaled_w = w as f32 * scale;
        let scaled_h = h as f32 * scale;
        Self {
            scale,
            pad_x: (INPUT as f32 - scaled_w) / 2.0,
            pad_y: (INPUT as f32 - scaled_h) / 2.0,
            src_width: w,
            src_height: h,
        }
    }

    /// Model coordinates back to image coordinates.
    fn to_source(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.pad_x) / self.scale, (y - self.pad_y) / self.scale)
    }
}

impl Detector for YuNet {
    fn name(&self) -> &'static str {
        "YuNet"
    }

    fn detect(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Detection>, DetectError> {
        let expected = width as usize * height as usize * 3;
        if pixels.len() != expected {
            return Err(DetectError::BufferSize { width, height, expected });
        }
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }

        let lb = Letterbox::fit(width, height);
        let input = letterbox_tensor(pixels, width, height, &lb);

        let tensor = ort::value::Tensor::from_array(([1usize, 3, INPUT, INPUT], input))
            .map_err(|e| DetectError::Input(e.to_string()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| DetectError::Inference("the session lock is poisoned".into()))?;

        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| DetectError::Inference(e.to_string()))?;

        let mut raw: Vec<Detection> = Vec::new();

        for stride in STRIDES.iter() {
            let cols = INPUT / stride;
            let rows = INPUT / stride;

            // Addressed by **name**, not position. The model happens to emit cls, obj,
            // bbox, kps in that order, but relying on output order is how a model update
            // silently swaps two tensors and produces boxes that are confidently wrong.
            let cls = extract(&outputs, &format!("cls_{stride}"))?;
            let obj = extract(&outputs, &format!("obj_{stride}"))?;
            let bbox = extract(&outputs, &format!("bbox_{stride}"))?;
            let kps = extract(&outputs, &format!("kps_{stride}"))?;

            for r in 0..rows {
                for c in 0..cols {
                    let idx = r * cols + c;

                    let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                    if score < SCORE_THRESHOLD {
                        continue;
                    }

                    let s = *stride as f32;
                    let cx = (c as f32 + bbox[idx * 4]) * s;
                    let cy = (r as f32 + bbox[idx * 4 + 1]) * s;
                    // `exp`, because the model predicts a log size. Dropping it gives boxes
                    // a few pixels across that read as "the threshold is too high".
                    let w = bbox[idx * 4 + 2].exp() * s;
                    let h = bbox[idx * 4 + 3].exp() * s;

                    let (x1, y1) = lb.to_source(cx - w / 2.0, cy - h / 2.0);
                    let (x2, y2) = lb.to_source(cx + w / 2.0, cy + h / 2.0);

                    let mut landmarks = [[0f32; 2]; 5];
                    for n in 0..5 {
                        let (lx, ly) = lb.to_source(
                            (kps[idx * 10 + 2 * n] + c as f32) * s,
                            (kps[idx * 10 + 2 * n + 1] + r as f32) * s,
                        );
                        landmarks[n] = [lx, ly];
                    }

                    raw.push(Detection {
                        x: x1,
                        y: y1,
                        width: x2 - x1,
                        height: y2 - y1,
                        confidence: score,
                        landmarks,
                    });
                }
            }
        }

        Ok(non_maximum_suppression(raw, NMS_THRESHOLD))
    }
}

/// One output tensor as a flat vector.
fn extract(outputs: &ort::session::SessionOutputs<'_>, name: &str) -> Result<Vec<f32>, DetectError> {
    let value = outputs
        .get(name)
        .ok_or_else(|| DetectError::Inference(format!("the model has no output {name}")))?;
    let array = value
        .try_extract_array::<f32>()
        .map_err(|e| DetectError::Inference(e.to_string()))?;
    Ok(array.iter().copied().collect())
}

/// Fit the image into the model's square input, letterboxed and in **BGR**.
///
/// Nearest-neighbour on purpose: this is a detector input, not a deliverable. A smoother
/// filter costs time and changes nothing the model cares about.
fn letterbox_tensor(pixels: &[u8], width: u32, height: u32, lb: &Letterbox) -> Vec<f32> {
    let mut out = vec![0f32; 3 * INPUT * INPUT];
    let plane = INPUT * INPUT;

    for ty in 0..INPUT {
        for tx in 0..INPUT {
            let sx = ((tx as f32 - lb.pad_x) / lb.scale).round();
            let sy = ((ty as f32 - lb.pad_y) / lb.scale).round();
            if sx < 0.0 || sy < 0.0 || sx >= width as f32 || sy >= height as f32 {
                continue; // the padding band stays black
            }
            let (sx, sy) = (sx as usize, sy as usize);
            let src = (sy * width as usize + sx) * 3;
            let dst = ty * INPUT + tx;
            // BGR: the model was exported for OpenCV's channel order, which is BGR. Feeding
            // RGB still finds faces, slightly worse — an error that survives testing.
            out[dst] = pixels[src + 2] as f32;
            out[plane + dst] = pixels[src + 1] as f32;
            out[2 * plane + dst] = pixels[src] as f32;
        }
    }
    out
}

/// Keep the best box where several overlap.
///
/// YuNet fires on neighbouring cells for one face, so this is not optional. Greedy
/// suppression by descending score, which is what OpenCV's `NMSBoxes` does.
pub fn non_maximum_suppression(mut boxes: Vec<Detection>, iou_threshold: f32) -> Vec<Detection> {
    boxes.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut kept: Vec<Detection> = Vec::with_capacity(boxes.len());
    for candidate in boxes {
        if kept.iter().all(|k| iou(k, &candidate) <= iou_threshold) {
            kept.push(candidate);
        }
    }
    kept
}

/// Intersection over union.
fn iou(a: &Detection, b: &Detection) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.width).min(b.x + b.width);
    let y2 = (a.y + a.height).min(b.y + b.height);

    let w = (x2 - x1).max(0.0);
    let h = (y2 - y1).max(0.0);
    let intersection = w * h;
    let union = a.area() + b.area() - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x: f32, y: f32, w: f32, h: f32, c: f32) -> Detection {
        Detection { x, y, width: w, height: h, confidence: c, landmarks: [[0.0; 2]; 5] }
    }

    #[test]
    fn letterbox_preserves_aspect_and_centres() {
        // A 3:2 photograph must not be squashed into a square — the box the model predicts
        // is then wrong by the same factor, which looks like a loose box rather than a bug.
        let lb = Letterbox::fit(1200, 800);
        assert!((lb.scale - INPUT as f32 / 1200.0).abs() < 1e-6);
        // Compared with a tolerance, not `== 0.0`: `1200.0 * (640.0 / 1200.0)` is not
        // exactly 640.0 in f32, so the padding comes out at 3e-5 rather than zero. An
        // exact assertion here tests floating point, not the letterbox.
        assert!(lb.pad_x.abs() < 0.01, "the long edge fills the input, got {}", lb.pad_x);
        assert!(lb.pad_y > 0.0, "the short edge is padded");
        assert!((lb.pad_y * 2.0 + 800.0 * lb.scale - INPUT as f32).abs() < 0.01);
    }

    #[test]
    fn letterbox_round_trips_a_point() {
        let lb = Letterbox::fit(1000, 500);
        // The centre of the image maps to the centre of the input.
        let (x, y) = lb.to_source(INPUT as f32 / 2.0, INPUT as f32 / 2.0);
        assert!((x - 500.0).abs() < 0.5, "got {x}");
        assert!((y - 250.0).abs() < 0.5, "got {y}");
    }

    #[test]
    fn nms_keeps_the_best_of_an_overlapping_pair() {
        // YuNet fires on neighbouring cells for one face, so this is not optional.
        let kept = non_maximum_suppression(
            vec![det(0.0, 0.0, 100.0, 100.0, 0.9), det(5.0, 5.0, 100.0, 100.0, 0.7)],
            0.3,
        );
        assert_eq!(kept.len(), 1);
        assert!((kept[0].confidence - 0.9).abs() < 1e-6, "the higher score must win");
    }

    #[test]
    fn nms_keeps_two_faces_that_merely_touch() {
        let kept = non_maximum_suppression(
            vec![det(0.0, 0.0, 100.0, 100.0, 0.9), det(150.0, 0.0, 100.0, 100.0, 0.8)],
            0.3,
        );
        assert_eq!(kept.len(), 2, "two faces side by side must both survive");
    }

    #[test]
    fn nms_handles_an_empty_list_and_a_single_box() {
        assert!(non_maximum_suppression(vec![], 0.3).is_empty());
        assert_eq!(non_maximum_suppression(vec![det(0.0, 0.0, 10.0, 10.0, 0.5)], 0.3).len(), 1);
    }

    #[test]
    fn nms_is_order_independent() {
        // Sorting by score is what makes it deterministic. Without that, the same three
        // boxes in a different order would suppress different ones.
        let boxes = vec![
            det(0.0, 0.0, 100.0, 100.0, 0.5),
            det(2.0, 2.0, 100.0, 100.0, 0.95),
            det(200.0, 0.0, 50.0, 50.0, 0.8),
        ];
        let mut reversed = boxes.clone();
        reversed.reverse();
        let a = non_maximum_suppression(boxes, 0.3);
        let b = non_maximum_suppression(reversed, 0.3);
        assert_eq!(a.len(), b.len());
        assert!((a[0].confidence - b[0].confidence).abs() < 1e-6);
    }

    #[test]
    fn iou_of_identical_boxes_is_one_and_of_disjoint_boxes_is_zero() {
        let a = det(0.0, 0.0, 10.0, 10.0, 1.0);
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&a, &det(100.0, 100.0, 10.0, 10.0, 1.0)), 0.0);
        // Zero-area boxes must not divide by zero.
        assert_eq!(iou(&det(0.0, 0.0, 0.0, 0.0, 1.0), &det(0.0, 0.0, 0.0, 0.0, 1.0)), 0.0);
    }

    #[test]
    fn a_wrongly_sized_buffer_is_an_error_not_a_panic() {
        // The buffer contract is the part that silently corrupts if it is wrong.
        let Some(model) = YuNet::bundled() else { return };
        let Ok(engine) = YuNet::from_file(&model) else { return };
        match engine.detect(&[0u8; 10], 100, 100) {
            Err(DetectError::BufferSize { expected, .. }) => assert_eq!(expected, 30_000),
            other => panic!("expected BufferSize, got {other:?}"),
        }
    }

    #[test]
    fn the_model_is_present_and_loads() {
        // If the model is missing, face detection is simply off — a supported state, and the
        // rest of the application works. If it is present, it must load.
        let Some(model) = YuNet::bundled() else {
            eprintln!("SKIP: the YuNet model is not present");
            return;
        };
        let engine = YuNet::from_file(&model).expect("the bundled model must load");
        assert_eq!(FaceEngine::name(&engine), "YuNet (OpenCV Zoo)");
        assert_eq!(engine.licence(), "MIT");
    }

    #[test]
    fn a_blank_image_yields_no_faces() {
        let Some(model) = YuNet::bundled() else { return };
        let Ok(engine) = YuNet::from_file(&model) else { return };
        let faces = engine.detect(&vec![0u8; 320 * 240 * 3], 320, 240).expect("inference");
        assert!(faces.is_empty(), "a black rectangle is not a face");
    }
}
