//! Fitting a face onto the template the recogniser expects.
//!
//! # Why this is not just a crop
//!
//! A face recogniser compares embeddings, and an embedding is a function of the pixels it
//! was given. Two photographs of one person taken at slightly different angles produce
//! *aligned* crops that are nearly identical and *cropped* ones that differ by a rotation
//! and a translation — and the model reads those differences as a different face.
//!
//! So the five landmarks are used to fit a **similarity transform** (rotation, uniform
//! scale, translation — no shear, no independent axis scaling, because a face is not
//! stretched) onto the reference template the model was trained against.
//!
//! # Reimplemented from the reference
//!
//! The transform lives in OpenCV's C++, which this cannot call, so it is reimplemented from
//! `modules/objdetect/src/face_recognize.cpp`: the Umeyama estimate, and the template
//! below. The template is not a choice — it is what the model was trained on, and a
//! different one degrades every embedding in a way that looks like a worse model.

/// The reference landmark positions, in a 112x112 frame.
///
/// Right eye, left eye, nose, right mouth corner, left mouth corner — the same order the
/// detector emits them in.
const TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// The size the recogniser takes.
pub const ALIGNED: usize = 112;

/// A 2x3 affine matrix, row-major: `[a, b, tx, c, d, ty]`.
///
/// Maps a source point `(x, y)` to `(a*x + b*y + tx, c*x + d*y + ty)`.
pub type Affine = [f32; 6];

/// Fit the similarity transform taking `landmarks` onto the template.
///
/// # The algebra, and why it is this short
///
/// A similarity transform has four degrees of freedom — rotation, uniform scale, and a
/// translation in each axis. The least-squares fit is the classic **Procrustes** solution,
/// and in two dimensions it has a closed form that needs no SVD:
///
/// ```text
/// a = Σ (q · p)        b = Σ (q × p)
/// θ = atan2(b, a)      s = √(a² + b²) / Σ|p|²
/// ```
///
/// where `p` are source points about their mean and `q` the destination points about
/// theirs. The first version of this used a closed-form 2x2 SVD from the literature, got a
/// sign convention wrong, and failed every test that checked the transform actually maps
/// one set of points onto the other. This is fewer lines and harder to get wrong.
///
/// **Reflection is impossible by construction.** `atan2` returns a rotation, and a rotation
/// has determinant +1 — so the mirrored case that a general fit would solve by flipping the
/// face cannot arise. That matters: a reflected fit makes two different people produce the
/// same embedding.
pub fn similarity_transform(landmarks: &[[f32; 2]; 5]) -> Affine {
    let src_mean = mean(landmarks);
    let dst_mean = mean(&TEMPLATE);

    let mut a = 0f32;
    let mut b = 0f32;
    let mut norm = 0f32;
    for i in 0..5 {
        let px = landmarks[i][0] - src_mean[0];
        let py = landmarks[i][1] - src_mean[1];
        let qx = TEMPLATE[i][0] - dst_mean[0];
        let qy = TEMPLATE[i][1] - dst_mean[1];

        a += qx * px + qy * py; // dot
        b += qx * py - qy * px; // cross
        norm += px * px + py * py;
    }

    if norm <= f32::EPSILON {
        // All five landmarks are the same point. There is no scale to recover, and dividing
        // by this is how a NaN reaches every downstream embedding — and a NaN embedding
        // compares as "identical" to every other NaN, so every face would cluster together.
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    }

    let theta = b.atan2(a);
    let scale = (a * a + b * b).sqrt() / norm;
    let (sin, cos) = theta.sin_cos();

    // s * R, where R = [[cos, -sin], [sin, cos]]
    let m00 = scale * cos;
    let m01 = -scale * sin;
    let m10 = scale * sin;
    let m11 = scale * cos;

    [
        m00,
        m01,
        dst_mean[0] - (m00 * src_mean[0] + m01 * src_mean[1]),
        m10,
        m11,
        dst_mean[1] - (m10 * src_mean[0] + m11 * src_mean[1]),
    ]
}

fn mean(p: &[[f32; 2]; 5]) -> [f32; 2] {
    let mut m = [0f32; 2];
    for v in p {
        m[0] += v[0];
        m[1] += v[1];
    }
    [m[0] / 5.0, m[1] / 5.0]
}

/// Sample the source image through the transform, into an aligned 112x112 RGB buffer.
///
/// Bilinear, unlike the detector's letterbox. This output is compared against other
/// embeddings rather than searched for a pattern, so resampling artefacts become part of
/// the identity — and nearest-neighbour at this scale is visible in the embedding.
pub fn warp(pixels: &[u8], width: u32, height: u32, m: Affine) -> Vec<u8> {
    let mut out = vec![0u8; ALIGNED * ALIGNED * 3];

    for ty in 0..ALIGNED {
        for tx in 0..ALIGNED {
            // The inverse map, so the output is filled rather than the input scattered.
            let (sx, sy) = inverse_point(m, tx as f32, ty as f32);
            let dst = (ty * ALIGNED + tx) * 3;
            if sx < 0.0 || sy < 0.0 || sx >= width as f32 - 1.0 || sy >= height as f32 - 1.0 {
                continue; // outside the source: leave black, as `warpAffine` does
            }

            let x0 = sx.floor() as usize;
            let y0 = sy.floor() as usize;
            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;

            for c in 0..3 {
                let p = |x: usize, y: usize| pixels[(y * width as usize + x) * 3 + c] as f32;
                let top = p(x0, y0) * (1.0 - fx) + p(x0 + 1, y0) * fx;
                let bottom = p(x0, y0 + 1) * (1.0 - fx) + p(x0 + 1, y0 + 1) * fx;
                out[dst + c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Where an output pixel came from.
fn inverse_point(m: Affine, x: f32, y: f32) -> (f32, f32) {
    let det = m[0] * m[4] - m[1] * m[3];
    if det.abs() < f32::EPSILON {
        return (-1.0, -1.0);
    }
    let dx = x - m[2];
    let dy = y - m[5];
    (
        (m[4] * dx - m[1] * dy) / det,
        (-m[3] * dx + m[0] * dy) / det,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_transforms_onto_itself() {
        // The identity case, and the strongest check that the algebra is right: fitting the
        // template to itself must give a transform that maps every template point to where
        // it already is.
        let m = similarity_transform(&TEMPLATE);
        for p in TEMPLATE {
            let x = m[0] * p[0] + m[1] * p[1] + m[2];
            let y = m[3] * p[0] + m[4] * p[1] + m[5];
            assert!((x - p[0]).abs() < 0.05, "x: {x} vs {}", p[0]);
            assert!((y - p[1]).abs() < 0.05, "y: {y} vs {}", p[1]);
        }
    }

    #[test]
    fn a_translated_face_is_mapped_back_onto_the_template() {
        // What alignment is for: the same face, moved, must land in the same place.
        let shifted: [[f32; 2]; 5] = std::array::from_fn(|i| [TEMPLATE[i][0] + 40.0, TEMPLATE[i][1] + 25.0]);
        let m = similarity_transform(&shifted);
        for p in TEMPLATE {
            let sx = p[0] + 40.0;
            let sy = p[1] + 25.0;
            let x = m[0] * sx + m[1] * sy + m[2];
            let y = m[3] * sx + m[4] * sy + m[5];
            assert!((x - p[0]).abs() < 0.1, "x: {x} vs {}", p[0]);
            assert!((y - p[1]).abs() < 0.1, "y: {y} vs {}", p[1]);
        }
    }

    #[test]
    fn a_scaled_face_is_mapped_back_onto_the_template() {
        // A face twice as large must be shrunk onto the template, which is what makes two
        // photographs taken at different distances comparable.
        let centre = [56.0f32, 72.0];
        let big: [[f32; 2]; 5] = std::array::from_fn(|i| {
            [
                centre[0] + (TEMPLATE[i][0] - centre[0]) * 2.0,
                centre[1] + (TEMPLATE[i][1] - centre[1]) * 2.0,
            ]
        });
        let m = similarity_transform(&big);
        for p in TEMPLATE {
            let sx = centre[0] + (p[0] - centre[0]) * 2.0;
            let sy = centre[1] + (p[1] - centre[1]) * 2.0;
            let x = m[0] * sx + m[1] * sy + m[2];
            let y = m[3] * sx + m[4] * sy + m[5];
            assert!((x - p[0]).abs() < 0.2, "x: {x} vs {}", p[0]);
            assert!((y - p[1]).abs() < 0.2, "y: {y} vs {}", p[1]);
        }
    }

    #[test]
    fn degenerate_landmarks_produce_a_usable_matrix_not_a_nan() {
        // Five identical points have no scale to recover. Dividing by that variance is how
        // a NaN reaches every downstream embedding, and a NaN embedding compares as
        // "identical" to every other NaN — so every face would cluster together.
        let m = similarity_transform(&[[10.0, 10.0]; 5]);
        assert!(m.iter().all(|v| v.is_finite()), "got {m:?}");
    }

    #[test]
    fn the_transform_forbids_reflection() {
        // A mirrored set of landmarks must not be fitted by mirroring the template: that
        // would make two different people produce the same embedding.
        let mirrored: [[f32; 2]; 5] = std::array::from_fn(|i| [112.0 - TEMPLATE[i][0], TEMPLATE[i][1]]);
        let m = similarity_transform(&mirrored);
        let determinant = m[0] * m[4] - m[1] * m[3];
        assert!(determinant > 0.0, "the fit reflected: determinant {determinant}");
    }

    #[test]
    fn warping_a_solid_image_gives_a_solid_image() {
        // A sanity check on the sampler: every output pixel must be filled when the
        // transform maps inside the source, and none when it maps outside.
        let pixels = vec![200u8; 400 * 400 * 3];
        let m = similarity_transform(&TEMPLATE); // ~identity, so the source covers the output
        let out = warp(&pixels, 400, 400, m);
        assert_eq!(out.len(), ALIGNED * ALIGNED * 3);
        assert!(out.iter().all(|v| *v > 190), "the warp lost the source");
    }

    #[test]
    fn a_transform_pointing_off_the_image_yields_black_not_garbage() {
        // Outside the source is black, as `warpAffine` does. Reading out of bounds instead
        // would be a panic, or worse, whatever the allocator had.
        let pixels = vec![255u8; 10 * 10 * 3];
        let off = [1.0, 0.0, 10_000.0, 0.0, 1.0, 10_000.0];
        let out = warp(&pixels, 10, 10, off);
        assert!(out.iter().all(|v| *v == 0), "out-of-frame pixels must be black");
    }
}
