//! Focus / sharpness scoring.
//!
//! # Why this is not just "variance of Laplacian"
//!
//! Variance of Laplacian is the standard blur metric, it is cheap, and used naively it
//! is **wrong on two of the most common kinds of photograph**:
//!
//! 1. **Shallow depth of field.** A portrait at f/1.4 is sharp on the face and soft
//!    everywhere else. Whole-frame variance marks it blurry. It is not blurry — it is
//!    the photograph the user meant to take.
//! 2. **Noise.** Sensor noise *is* high-frequency energy. A clean sharp frame at ISO 100
//!    and a noisy frame at ISO 12800 can produce the same Laplacian variance, and a
//!    naive metric ranks the noisy one as sharper.
//!
//! A third case, **low contrast**, is milder: a genuinely sharp frame in a hazy or
//! foggy scene has low absolute variance and gets marked soft.
//!
//! # The three corrections
//!
//! * **Subject ROI** — measure on the subject, not the frame. Faces when we have them
//!   (Phase 2), otherwise saliency, otherwise an honest centre crop.
//! * **Noise correction** — estimate the noise floor and subtract its *known* expected
//!   contribution from both the variance and the contrast term, using the kernel
//!   constants rather than a tuned fudge factor.
//! * **Luma-variance normalisation** — divide by the variance of the pixel values
//!   themselves, which makes the metric scale-invariant.
//!
//! ## How the normaliser was arrived at (two wrong answers first)
//!
//! **Attempt 1: divide by the local (ROI) contrast.** Wrong, and measurably so — the
//! subject region has high contrast *precisely because* it is sharp, so dividing local
//! detail by local contrast cancels the signal being measured. On the shallow-DOF
//! fixture it made whole-frame scoring report a *higher* focus than subject-ROI
//! scoring: the exact inversion this module exists to prevent.
//!
//! **Attempt 2: divide by the whole-frame contrast, floored.** This fixed the inversion
//! but produced a 240x score gap between two sharp frames, because a shallow-DOF frame
//! legitimately has low *frame* contrast while having a very sharp subject. The floor
//! then dominated the denominator and inflated the score without bound.
//!
//! **Attempt 3 (this one): divide by the variance of the luma over the ROI.** This is
//! scale-invariant by construction — scaling scene contrast by `k` multiplies both the
//! Laplacian variance and the luma variance by `k^2`, so the ratio is unchanged. It
//! measures *what fraction of the available tonal range is resolved as fine detail*,
//! which is what sharpness actually is. A hazy-but-sharp frame and a punchy-but-sharp
//! frame land in the same place, which is correct, and a blurred frame lands far below
//! both regardless of its contrast.
//!
//! The lesson generalises: normalise by a quantity that is *causally independent* of
//! the thing being measured. Local contrast is not independent of local sharpness.
//! Total tonal range is.
//! The result is a metric that is *scene-relative* — comparable between frames of the
//! same shoot — rather than an absolute number that only means something for one lens,
//! one ISO and one subject. Absolute comparability is supplied later by the shoot-relative
//! percentile normalisation (issue #15); this module deliberately produces the raw,
//! scene-normalised signal that feeds it.

use crate::imaging::{
    self, directional_energy, gradient_magnitude_mean, laplacian_variance, saliency_region, Luma,
    Region,
};

/// Long edge the metric is computed at. Bounded so a 45 MP raw costs the same as a 12 MP
/// one, and so the memory budget is a property of the pipeline rather than of the input.
pub const WORKING_LONG_EDGE: usize = 1024;

/// Sum of squared kernel taps for `[0,1,0; 1,-4,1; 0,1,0]`.
///
/// For additive white noise of variance `s^2`, the convolution response has variance
/// `s^2 * 20`. Subtracting that is an exact correction, not a heuristic.
const LAPLACIAN_NOISE_GAIN: f64 = imaging::LAPLACIAN_KERNEL_SQ_SUM;

/// Expected Sobel magnitude from noise alone, per unit sigma.
///
/// Sobel x has kernel `[1,0,-1; 2,0,-2; 1,0,-1]` with `sum(w^2) = 12`, so each component
/// is `N(0, 12 s^2)` and the magnitude is Rayleigh-distributed with mean
/// `s * sqrt(12) * sqrt(pi/2) ~= 4.34 s`.
const SOBEL_NOISE_GAIN: f64 = 4.3403;

/// Floor on the normaliser, in squared grey levels.
///
/// A frame with essentially no tonal range carries no measurable detail, and dividing by
/// a near-zero variance would manufacture an unbounded score. Below this the metric
/// reports zero rather than inventing a sharpness it cannot substantiate.
const MIN_LUMA_VARIANCE: f64 = 1.0;

/// Where the region of interest came from. Reported so the UI can be honest about how
/// much it actually knew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoiSource {
    /// Union of detected face boxes. The best case.
    Faces,
    /// Found by energy saliency — "the detail is over here".
    Saliency,
    /// Nothing better was available; a centred crop was used.
    Centre,
    /// Explicitly the whole frame, for diagnostics and comparison only.
    WholeFrame,
}

/// Focus measurements for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusMetrics {
    pub roi: Region,
    pub roi_source: RoiSource,
    /// Working scale that produced the reported score (1 or 2).
    pub scale_factor: usize,
    /// Raw variance of Laplacian over the ROI, before any correction.
    pub raw_variance: f64,
    /// Estimated noise standard deviation over the frame, in grey levels.
    pub noise_sigma: f64,
    /// The part of `raw_variance` attributable to noise.
    pub noise_variance: f64,
    /// `raw_variance` minus the noise contribution, floored at zero. This is the detail
    /// the sensor actually resolved.
    pub signal_variance: f64,
    /// Mean Sobel gradient magnitude over the **ROI**, noise-corrected.
    ///
    /// Diagnostic only. It is reported because it explains *why* a frame scores as it
    /// does, but it is deliberately not used as the normaliser — see the module docs.
    pub roi_contrast: f64,
    /// Variance of the ROI's pixel values, noise-corrected. This is the normaliser.
    ///
    /// Scale-invariant: it rises and falls with scene contrast in exactly the same
    /// proportion as [`Self::signal_variance`] does, so the ratio between them measures
    /// resolved detail rather than scene contrast.
    pub luma_variance: f64,
    /// How much of the measured variance survives the noise correction, in `[0, 1]`.
    ///
    /// `signal / (signal + noise)`. When this is low the frame is noise-dominated and the
    /// residual after correction is not a trustworthy measurement of detail, so
    /// [`Self::normalized_focus`] is shrunk toward zero proportionally.
    ///
    /// This matters: a nearly-flat frame has a tiny signal *and* a noise floor several
    /// times larger, and without shrinkage the small uncertain residual gets divided by a
    /// tiny luma variance and lands above a genuinely sharp frame. Refusing to claim
    /// detail you cannot substantiate is the whole discipline of this module.
    pub noise_reliability: f64,
    /// `(signal_variance / luma_variance) * noise_reliability`.
    ///
    /// This is the comparable quantity. Higher is sharper.
    pub normalized_focus: f64,
    /// Directional energy ratio `max(Ex,Ey)/min(Ex,Ey)`, floored at 1.
    ///
    /// ~1 means isotropic (defocus). Above ~1.6 suggests directional motion blur.
    pub anisotropy: f64,
    /// Coefficient of variation of the Laplacian response across sub-tiles. Low means the
    /// frame is sharp throughout its subject rather than in one lucky place.
    pub tile_variation: f64,
}

/// How much more directional than its own shoot a frame must be to be a candidate.
const MOTION_ANISOTROPY_RATIO: f64 = 2.5;

/// How much less absolute detail than its own shoot a frame must show to be a candidate.
///
/// Note this gates **absolute detail**, not `normalized_focus`. The ratio is built to be
/// contrast-invariant, which makes it largely blur-invariant too: blurring a frame
/// reduces the Laplacian variance and the luma variance together, so the ratio barely
/// moves. Measured on the broadband fixtures, a 13px smear cost only 13% of the ratio.
/// Absolute detail is not comparable across scenes — which is precisely why it cannot
/// support a single-frame verdict — but it is comparable within a shoot that shares a
/// scene, a lens and a lighting setup.
const MOTION_DETAIL_FRACTION: f64 = 0.6;

/// Reference values from one shoot.
///
/// # Why motion-blur detection cannot be a single-frame decision
///
/// This was learned from real photographs, after the single-frame version had already
/// been written, tested and believed.
///
/// The original design classified motion blur from one frame: an anisotropy threshold of
/// 3.0, calibrated on synthetic fixtures where every non-motion frame measured between
/// 1.00 and 1.74. Run against 50 real photographs, **9 of them — 18% — were flagged**,
/// including a well-focused frame whose normalized focus of 1.14 was well above the
/// corpus median. The real distribution measured p50 = 1.63, p95 = 6.04, max = 9.39:
/// it overlaps the threshold almost completely.
///
/// The cause is not tuning. Anisotropy is `max(Ex,Ey) / min(Ex,Ey)`, and an ordinary
/// photograph of a horizon, a building, a railing or a field contains genuinely
/// directional structure. **A single frame cannot distinguish "this scene is
/// directional" from "this frame was smeared along one axis"** — separating those
/// requires knowing what the same scene looked like unblurred.
///
/// A burst provides exactly that reference: siblings of the same moment, same scene,
/// same lighting. So motion-blur classification belongs in shoot/burst comparison
/// (issues #15 and #18), and the single-frame verdict was **removed rather than left
/// available to be misused**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShootBaseline {
    pub anisotropy_median: f64,
    /// Median `signal_variance` across the shoot — absolute resolved detail.
    pub detail_median: f64,
    /// Median `normalized_focus`. Retained for reporting and for the composite score;
    /// not used by [`FocusMetrics::is_motion_blur_candidate`].
    pub focus_median: f64,
}

impl ShootBaseline {
    /// Compute a baseline from the frames of one shoot.
    ///
    /// Medians rather than means: a shoot containing a few badly blurred frames should
    /// not drag the reference toward them and then fail to notice them.
    pub fn from_metrics(metrics: &[FocusMetrics]) -> Option<Self> {
        if metrics.is_empty() {
            return None;
        }
        Some(Self {
            anisotropy_median: median(metrics.iter().map(|m| m.anisotropy))?,
            detail_median: median(metrics.iter().map(|m| m.signal_variance))?,
            focus_median: median(metrics.iter().map(|m| m.normalized_focus))?,
        })
    }
}

fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

impl FocusMetrics {
    /// A candidate for motion blur: markedly more directional **and** markedly softer
    /// than the rest of its own shoot.
    ///
    /// Both conditions are required, and each excludes a different false positive:
    ///
    /// * **Directionality alone** fires on any scene with strong directional structure —
    ///   a horizon, a fence, architecture. That produced the 18% false-positive rate.
    /// * **Softness alone** fires on defocus, which is a different defect with a
    ///   different fix and deserves a different label.
    ///
    /// The combination — unusually directional *for this scene* while also unusually
    /// soft *for this scene* — is what motion blur looks like from a single frame.
    ///
    /// "Soft" is measured as absolute detail against the shoot, not as the
    /// contrast-invariant ratio; see [`MOTION_DETAIL_FRACTION`].
    ///
    /// This is a flag for review, never a verdict. The UI should present it as "possibly
    /// motion-blurred — compare with its burst siblings", because the frames that
    /// actually resolve the ambiguity are the siblings.
    pub fn is_motion_blur_candidate(&self, baseline: &ShootBaseline) -> bool {
        self.anisotropy > baseline.anisotropy_median * MOTION_ANISOTROPY_RATIO
            && self.signal_variance < baseline.detail_median * MOTION_DETAIL_FRACTION
    }
}

/// Downscale to the working resolution. Call this once per image and reuse.
pub fn prepare(img: &Luma) -> Luma {
    img.downscale_to_long_edge(WORKING_LONG_EDGE)
}

/// Choose the region to measure.
///
/// Face boxes win when present because they are the only signal that actually knows what
/// the photograph is *of*. Saliency is a decent proxy for "where the detail is". The
/// centre crop is the honest last resort.
pub fn choose_roi(work: &Luma, faces: Option<&[Region]>) -> (Region, RoiSource) {
    if let Some(faces) = faces {
        if !faces.is_empty() {
            let mut x0 = usize::MAX;
            let mut y0 = usize::MAX;
            let mut x1 = 0usize;
            let mut y1 = 0usize;
            for f in faces {
                x0 = x0.min(f.x);
                y0 = y0.min(f.y);
                x1 = x1.max(f.x + f.w);
                y1 = y1.max(f.y + f.h);
            }
            // Expand generously: focus is judged on the whole head and shoulders, not
            // the bounding box of the face, and a face crop excludes the ears, hair and
            // clothing that carry most of the high-frequency detail.
            let mx = ((x1 - x0) * 3) / 10;
            let my = ((y1 - y0) * 3) / 10;
            let rx = x0.saturating_sub(mx);
            let ry = y0.saturating_sub(my);
            let rw = (x1 + mx).min(work.w).saturating_sub(rx).max(1);
            let rh = (y1 + my).min(work.h).saturating_sub(ry).max(1);
            return (Region::new(rx, ry, rw, rh), RoiSource::Faces);
        }
    }

    if let Some(roi) = saliency_region(work, 16, 12, 0.25) {
        return (roi, RoiSource::Saliency);
    }

    (Region::centre(work.w, work.h, 0.6), RoiSource::Centre)
}

/// Measure focus over an explicit ROI of an already-[`prepare`]d image.
pub fn analyse_prepared(work: &Luma, roi: Region, source: RoiSource) -> FocusMetrics {
    // Noise is a property of the sensor and exposure, not of the ROI, so it is estimated
    // over the whole frame where there are far more samples to be robust with.
    let sigma = imaging::estimate_noise_sigma(work, Region::full(work.w, work.h));
    let noise_variance = LAPLACIAN_NOISE_GAIN * sigma * sigma;

    let mut best: Option<FocusMetrics> = None;

    for factor in [1usize, 2] {
        let (img, region) = if factor == 1 {
            (work.clone(), roi)
        } else {
            let d = work.box_downscale(factor);
            let r = Region::new(
                roi.x / factor,
                roi.y / factor,
                (roi.w / factor).max(3),
                (roi.h / factor).max(3),
            );
            (d, r)
        };

        if region.w < 3 || region.h < 3 {
            continue;
        }

        // Noise halves as the box filter averages four samples, so it is re-estimated at
        // each scale rather than carried over. Reusing the full-resolution sigma here
        // would over-correct the coarse scale and flatten real detail out of the metric.
        let scale_sigma = if factor == 1 {
            sigma
        } else {
            imaging::estimate_noise_sigma(&img, Region::full(img.w, img.h))
        };

        let raw_variance = laplacian_variance(&img, region);
        let signal_variance =
            (raw_variance - LAPLACIAN_NOISE_GAIN * scale_sigma * scale_sigma).max(0.0);

        // Detail from the subject.
        let roi_contrast =
            (gradient_magnitude_mean(&img, region) - SOBEL_NOISE_GAIN * scale_sigma).max(0.0);

        // The normaliser: total tonal range of the subject region, noise-corrected.
        let luma_variance =
            (imaging::luma_variance(&img, region) - scale_sigma * scale_sigma).max(0.0);

        let normalized = if luma_variance <= MIN_LUMA_VARIANCE {
            0.0
        } else {
            let raw = signal_variance / luma_variance;
            // Shrink toward zero when the measurement is noise-dominated. Note this uses
            // the *unfloored* noise contribution, so a frame whose detail is real is not
            // penalised for a little sensor noise.
            let reliability = signal_variance / (signal_variance + noise_variance.max(1e-9));
            raw * reliability
        };

        let (ex, ey) = directional_energy(&img, region);
        let (hi, lo) = if ex >= ey { (ex, ey) } else { (ey, ex) };
        let anisotropy = if lo <= f64::EPSILON { 1.0 } else { (hi / lo).max(1.0) };

        let metrics = FocusMetrics {
            roi: region,
            roi_source: source,
            scale_factor: factor,
            raw_variance,
            noise_sigma: scale_sigma,
            noise_variance: LAPLACIAN_NOISE_GAIN * scale_sigma * scale_sigma,
            signal_variance,
            roi_contrast,
            luma_variance,
            noise_reliability: signal_variance
                / (signal_variance + (LAPLACIAN_NOISE_GAIN * scale_sigma * scale_sigma).max(1e-9)),
            normalized_focus: normalized,
            anisotropy,
            tile_variation: imaging::response_variation_across_tiles(&img, region, 4),
        };

        // Multi-scale takes the best scale rather than a fixed one: downsampling averages
        // noise away faster than it averages structured detail, so the coarse scale often
        // has the better signal-to-noise ratio on a noisy frame, while the fine scale
        // wins on a clean one. Taking the max agrees with whichever scale can actually
        // see the detail.
        let better = match &best {
            None => true,
            Some(b) => metrics.normalized_focus > b.normalized_focus,
        };
        if better {
            best = Some(metrics);
        }
    }

    best.unwrap_or(FocusMetrics {
        roi,
        roi_source: source,
        scale_factor: 1,
        raw_variance: 0.0,
        noise_sigma: sigma,
        noise_variance,
        signal_variance: 0.0,
        roi_contrast: 0.0,
        luma_variance: 0.0,
        noise_reliability: 0.0,
        normalized_focus: 0.0,
        anisotropy: 1.0,
        tile_variation: 0.0,
    })
}

/// Full analysis: prepare, choose the ROI, measure.
pub fn analyse(img: &Luma, faces: Option<&[Region]>) -> FocusMetrics {
    let work = prepare(img);
    let (roi, source) = choose_roi(&work, faces);
    analyse_prepared(&work, roi, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Load a generated synthetic fixture as luma.
    ///
    /// Unit tests use fixtures with *known* properties (see
    /// `tools/fixtures/generate_synthetic.py`) and assert ordering and discrimination,
    /// never a magic value. A test that asserts `focus == 412.7` is a test that has to be
    /// edited every time a constant is tuned, and it does not actually test anything.
    fn fixture(name: &str) -> Luma {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images")
            .join(format!("{name}.jpg"));
        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("could not open fixture {}: {e}", path.display()))
            .to_luma8();
        Luma::from_gray8(img.width() as usize, img.height() as usize, img.as_raw())
    }

    // ---------------------------------------------------------------------
    // Calibration. Ignored by default; run with:
    //     cargo test --lib -- --ignored --nocapture calibration
    // This exists so the assertions below are set from measured behaviour rather
    // than guessed thresholds. Re-run it whenever a constant changes.
    // ---------------------------------------------------------------------
    #[test]
    #[ignore]
    fn calibration_report() {
        let names = [
            "sharp_a",
            "sharp_b",
            "blur_defocus_mild",
            "blur_defocus_heavy",
            "blur_motion",
            "noise_high_iso",
            "flat_low_contrast",
            "bokeh_portrait",
            "exposure_over",
            "exposure_under",
            "sharp_broadband",
            "blur_motion_broadband",
            "blur_defocus_broadband",
        ];
        println!(
            "\n{:<20} {:>9} {:>6} {:>11} {:>9} {:>9} {:>8} {:>9} {:>8} {:>7} {:>7}",
            "fixture", "source", "scale", "rawvar", "noiseVar", "signalVar", "reliab",
            "lumaVar", "norm", "aniso", "tileCV"
        );
        println!("{}", "-".repeat(112));
        for n in names {
            let img = fixture(n);
            let work = prepare(&img);
            let (roi, src) = choose_roi(&work, None);
            let m = analyse_prepared(&work, roi, src);
            println!(
                "{:<20} {:>9} {:>6} {:>11.1} {:>9.1} {:>9.1} {:>8.3} {:>9.1} {:>8.3} {:>7.3} {:>7.3}",
                n,
                format!("{src:?}"),
                m.scale_factor,
                m.raw_variance,
                m.noise_variance,
                m.signal_variance,
                m.noise_reliability,
                m.luma_variance,
                m.normalized_focus,
                m.anisotropy,
                m.tile_variation
            );
        }

        println!("\nwhole-frame ROI, for the bokeh comparison:");
        println!(
            "{:<20} {:>11} {:>9} {:>9} {:>8}",
            "fixture", "rawvar", "signalVar", "lumaVar", "norm"
        );
        println!("{}", "-".repeat(62));
        for n in ["sharp_a", "bokeh_portrait", "blur_defocus_heavy"] {
            let img = fixture(n);
            let work = prepare(&img);
            let full = Region::full(work.w, work.h);
            let m = analyse_prepared(&work, full, RoiSource::WholeFrame);
            println!(
                "{:<20} {:>11.1} {:>9.1} {:>9.1} {:>8.3}",
                n, m.raw_variance, m.signal_variance, m.luma_variance, m.normalized_focus
            );
        }
        println!();
    }

    /// Convenience used by the ordering assertions.
    fn norm(name: &str) -> f64 {
        let img = fixture(name);
        analyse(&img, None).normalized_focus
    }

    // ---------------------------------------------------------------------
    // The blur ladder
    // ---------------------------------------------------------------------
    #[test]
    fn focus_orders_the_defocus_ladder() {
        let sharp = norm("sharp_a");
        let mild = norm("blur_defocus_mild");
        let heavy = norm("blur_defocus_heavy");
        assert!(
            sharp > mild,
            "sharp ({sharp:.3}) should beat mildly defocused ({mild:.3})"
        );
        assert!(
            mild > heavy,
            "mildly defocused ({mild:.3}) should beat heavily defocused ({heavy:.3})"
        );
    }

    #[test]
    fn two_independent_sharp_frames_are_close() {
        // Sanity against a metric that is secretly a fingerprint of one fixture.
        let a = norm("sharp_a");
        let b = norm("sharp_b");
        let ratio = a.max(b) / a.min(b);
        assert!(ratio < 1.5, "two sharp frames differed by {ratio:.2}x: {a:.3} vs {b:.3}");
    }

    // ---------------------------------------------------------------------
    // The most important test in the project.
    // ---------------------------------------------------------------------
    #[test]
    fn bokeh_scores_sharp_on_the_subject_and_blurry_whole_frame() {
        let roi = analyse(&fixture("bokeh_portrait"), None);
        let whole = {
            let img = fixture("bokeh_portrait");
            let work = prepare(&img);
            let full = Region::full(work.w, work.h);
            analyse_prepared(&work, full, RoiSource::WholeFrame)
        };
        let sharp = analyse(&fixture("sharp_a"), None);
        let heavy = analyse(&fixture("blur_defocus_heavy"), None);

        // The direct, uncontaminated comparison: subject measurement finds far more
        // detail than whole-frame measurement on the same frame. This is the property
        // that ROI selection exists to produce.
        assert!(
            roi.signal_variance > whole.signal_variance * 1.5,
            "subject-ROI detail ({:.1}) must clearly exceed whole-frame detail ({:.1}) on a \
             shallow-depth-of-field frame, or the ROI selection is not doing anything",
            roi.signal_variance,
            whole.signal_variance
        );

        assert!(
            roi.normalized_focus > whole.normalized_focus,
            "subject-ROI scoring ({:.3}) must beat whole-frame scoring ({:.3})",
            roi.normalized_focus,
            whole.normalized_focus
        );

        // The frame that the whole point of this module is not to misjudge: a bokeh
        // portrait is a SHARP photograph, and must land in the same league as one.
        let ratio = roi.normalized_focus / sharp.normalized_focus;
        assert!(
            (0.5..2.0).contains(&ratio),
            "a bokeh portrait measured on its subject ({:.3}) should be comparable to a \
             sharp frame ({:.3}), but was {ratio:.2}x it",
            roi.normalized_focus,
            sharp.normalized_focus
        );

        assert!(
            roi.normalized_focus > heavy.normalized_focus * 100.0,
            "a sharp-subject bokeh frame ({:.3}) must utterly outrank a genuinely blurred \
             frame ({:.3})",
            roi.normalized_focus,
            heavy.normalized_focus
        );
    }

    #[test]
    fn roi_source_is_saliency_for_a_bokeh_frame() {
        let img = fixture("bokeh_portrait");
        let m = analyse(&img, None);
        assert_eq!(
            m.roi_source,
            RoiSource::Saliency,
            "a frame with a clear detail region should not fall back to a centre crop"
        );
    }

    // ---------------------------------------------------------------------
    // Contrast and noise robustness
    // ---------------------------------------------------------------------
    #[test]
    fn low_contrast_is_not_punished_as_blur() {
        let flat = norm("flat_low_contrast");
        let heavy = norm("blur_defocus_heavy");
        assert!(
            flat > heavy,
            "a sharp but low-contrast frame ({flat:.3}) must beat a blurred frame \
             ({heavy:.3}) — this is the whole point of contrast normalisation"
        );
    }

    #[test]
    fn noise_is_not_mistaken_for_detail() {
        // The confound this design exists to remove. Noise is high-frequency energy, so
        // it inflates raw Laplacian variance; the correction must pull it back down.
        let img = fixture("noise_high_iso");
        let work = prepare(&img);
        let (roi, src) = choose_roi(&work, None);
        let m = analyse_prepared(&work, roi, src);

        assert!(
            m.noise_sigma > 1.0,
            "the noisy fixture should be detected as noisy, got sigma {:.3}",
            m.noise_sigma
        );
        assert!(
            m.noise_variance > 0.0,
            "noise should contribute a measurable share of the raw variance"
        );
        assert!(
            m.signal_variance < m.raw_variance,
            "the correction must strictly reduce the variance: raw {:.1} -> signal {:.1}",
            m.raw_variance,
            m.signal_variance
        );
        assert!(
            m.noise_reliability < 1.0,
            "a noisy frame should carry less measurement confidence than a clean one, got \
             {:.3}",
            m.noise_reliability
        );

        // And it must not collapse to zero: the underlying frame IS sharp.
        assert!(
            m.signal_variance > 0.0,
            "a noisy but genuinely sharp frame must not be zeroed out by the correction"
        );

        // A noisy sharp frame should still beat a defocused one — the noise penalty is a
        // haircut, not a disqualification.
        assert!(
            m.normalized_focus > norm("blur_defocus_mild") * 2.0,
            "noisy-but-sharp ({:.3}) must still clearly beat defocused ({:.3})",
            m.normalized_focus,
            norm("blur_defocus_mild")
        );
    }

    #[test]
    fn a_noise_dominated_measurement_is_shrunk_rather_than_trusted() {
        // The low-contrast fixture has a signal barely a fifth of its noise floor. Its
        // raw ratio is the highest of any fixture; without shrinkage it would outrank
        // every genuinely sharp frame. Refusing to claim detail you cannot substantiate
        // is the point.
        let flat = analyse(&fixture("flat_low_contrast"), None);
        let sharp = analyse(&fixture("sharp_a"), None);

        assert!(
            flat.noise_reliability < 0.5,
            "the low-contrast fixture should be recognised as noise-dominated, got {:.3}",
            flat.noise_reliability
        );
        assert!(
            flat.normalized_focus < sharp.normalized_focus,
            "a noise-dominated measurement ({:.3}) must not outrank a clean sharp frame \
             ({:.3})",
            flat.normalized_focus,
            sharp.normalized_focus
        );
    }

    #[test]
    fn a_clean_frame_keeps_its_detail_through_the_noise_correction() {
        // Immerkær's estimator cannot distinguish sensor noise from genuine detail at the
        // Nyquist limit — they are the same thing locally, and no local estimator can
        // separate them. This fixture is a dense 8px grid, so the estimator reads a few
        // grey levels of "noise" that is really pattern.
        //
        // That is acceptable *because the metric does not depend on sigma being accurate
        // in absolute terms*. What matters is that the correction removes noise without
        // removing detail, so the assertion is about the ratio, not the sigma.
        let img = fixture("sharp_a");
        let m = analyse(&img, None);

        assert!(
            m.noise_reliability > 0.9,
            "a clean frame must retain high measurement confidence, got {:.3}",
            m.noise_reliability
        );
        assert!(
            m.noise_variance < m.raw_variance * 0.10,
            "the noise correction must take less than a tenth of a clean frame's variance: \
             {:.1} of {:.1}",
            m.noise_variance,
            m.raw_variance
        );
        assert!(
            m.signal_variance > 0.0,
            "real detail must survive the correction"
        );
    }

    // ---------------------------------------------------------------------
    // Motion vs defocus
    // ---------------------------------------------------------------------
    #[test]
    fn motion_blur_is_more_anisotropic_than_defocus() {
        let motion = analyse(&fixture("blur_motion"), None);
        let defocus = analyse(&fixture("blur_defocus_mild"), None);

        assert!(
            motion.anisotropy > defocus.anisotropy,
            "directional blur (aniso {:.3}) must be more anisotropic than uniform blur \
             (aniso {:.3})",
            motion.anisotropy,
            defocus.anisotropy
        );
    }

    #[test]
    fn heavy_defocus_is_the_most_isotropic_thing_there_is() {
        // Uniform blur destroys structure along every axis equally, so it should measure
        // as near-isotropic. This is the sanity anchor for the whole anisotropy idea.
        let heavy = analyse(&fixture("blur_defocus_heavy"), None);
        assert!(
            heavy.anisotropy < 1.5,
            "uniform defocus should be near-isotropic, got {:.3}",
            heavy.anisotropy
        );
    }

    #[test]
    fn motion_blur_reduces_detail_on_a_broadband_spectrum() {
        // The property a grid fixture cannot demonstrate. Horizontal lines are constant
        // along x, so a horizontal blur leaves them untouched; measured on the grid
        // fixture, a 13px smear retained 83% of the sharp frame's focus. On a
        // direction-neutral broadband spectrum the same smear destroys roughly half.
        //
        // Without this fixture the "is it soft?" half of motion detection was untestable.
        let sharp = analyse(&fixture("sharp_broadband"), None);
        let motion = analyse(&fixture("blur_motion_broadband"), None);

        // Asserted on ABSOLUTE detail, not on normalized_focus. The ratio is built to be
        // contrast-invariant, which also makes it largely blur-invariant: a 13px smear
        // costs only about 13% of the ratio while costing over half the real detail.
        // Using the ratio here would have hidden the very effect being tested.
        assert!(
            motion.signal_variance < sharp.signal_variance * 0.75,
            "a 13px horizontal smear of broadband texture (signal variance {:.1}) must \
             lose clearly more than a quarter of the sharp original's detail ({:.1})",
            motion.signal_variance,
            sharp.signal_variance
        );
        assert!(
            motion.anisotropy > sharp.anisotropy * 2.0,
            "the smear must be markedly more directional ({:.3}) than its original ({:.3})",
            motion.anisotropy,
            sharp.anisotropy
        );
    }

    #[test]
    fn broadband_defocus_is_isotropic_while_broadband_motion_is_not() {
        let defocus = analyse(&fixture("blur_defocus_broadband"), None);
        let motion = analyse(&fixture("blur_motion_broadband"), None);
        assert!(
            motion.anisotropy > defocus.anisotropy * 2.0,
            "on identical texture, directional blur ({:.3}) must be far more anisotropic \
             than isotropic blur ({:.3})",
            motion.anisotropy,
            defocus.anisotropy
        );
    }

    // ---------------------------------------------------------------------
    // The motion-blur retraction, as tests
    // ---------------------------------------------------------------------

    /// A synthetic shoot: the frames a photographer would actually have together.
    fn synthetic_shoot() -> Vec<FocusMetrics> {
        [
            "sharp_a",
            "sharp_b",
            "sharp_broadband",
            "blur_defocus_mild",
            "blur_defocus_broadband",
            "noise_high_iso",
            "bokeh_portrait",
            "exposure_under",
        ]
        .iter()
        .map(|n| analyse(&fixture(n), None))
        .collect()
    }

    #[test]
    fn a_soft_directional_frame_in_a_shoot_is_flagged() {
        let baseline = ShootBaseline::from_metrics(&synthetic_shoot()).expect("baseline");
        let motion = analyse(&fixture("blur_motion_broadband"), None);
        assert!(
            motion.is_motion_blur_candidate(&baseline),
            "a smeared frame that is both more directional ({:.3} vs baseline {:.3}) and \
             softer ({:.3} vs baseline {:.3}) than its shoot should be flagged",
            motion.anisotropy,
            baseline.anisotropy_median,
            motion.normalized_focus,
            baseline.focus_median
        );
    }

    #[test]
    fn a_directional_but_sharp_frame_is_not_flagged() {
        // **The false positive that real photographs exposed.** A scene with strong
        // directional structure — a horizon, a fence, architecture — has high anisotropy
        // and no blur at all. Directionality alone flagged 18% of a 50-photograph corpus
        // this way, including well-focused frames.
        //
        // This constructs that case deliberately: anisotropically stretched texture that
        // is nevertheless perfectly sharp.
        let shoot = synthetic_shoot();
        let baseline = ShootBaseline::from_metrics(&shoot).expect("baseline");

        // Case 1: far more directional than the shoot, but NOT low on detail.
        let mut directional_but_sharp = analyse(&fixture("sharp_broadband"), None);
        directional_but_sharp.anisotropy = baseline.anisotropy_median * 50.0;
        directional_but_sharp.signal_variance = baseline.detail_median * 5.0;
        assert!(
            !directional_but_sharp.is_motion_blur_candidate(&baseline),
            "a sharp frame must never be flagged however directional it is — this is the \
             false positive that flagged 18% of real photographs"
        );

        // Case 2: the converse. Low detail but isotropic is defocus, not motion: a
        // different defect with a different fix, and it deserves a different label.
        let mut soft_but_isotropic = analyse(&fixture("blur_defocus_broadband"), None);
        soft_but_isotropic.anisotropy = baseline.anisotropy_median;
        soft_but_isotropic.signal_variance = baseline.detail_median * 0.1;
        assert!(
            !soft_but_isotropic.is_motion_blur_candidate(&baseline),
            "a soft but isotropic frame is defocus, not motion blur"
        );

        // Case 3: both conditions together, which is the only thing that qualifies.
        let mut both = analyse(&fixture("blur_motion_broadband"), None);
        both.anisotropy = baseline.anisotropy_median * 50.0;
        both.signal_variance = baseline.detail_median * 0.1;
        assert!(
            both.is_motion_blur_candidate(&baseline),
            "directional AND low-detail must be flagged"
        );
    }

    #[test]
    fn a_single_frame_anisotropy_threshold_is_not_sound() {
        // Documents why the single-frame verdict was removed, using synthetic frames
        // whose anisotropy alone cannot separate the cases.
        //
        // The grid-based motion fixture is *less* anisotropic than several frames that
        // are not motion-blurred at all. Any fixed single-frame threshold either misses
        // it or fires on them.
        let grid_motion = analyse(&fixture("blur_motion"), None).anisotropy;
        let broadband_defocus = analyse(&fixture("blur_defocus_broadband"), None).anisotropy;
        let flat = analyse(&fixture("flat_low_contrast"), None).anisotropy;

        assert!(
            flat < grid_motion,
            "sanity: the low-contrast fixture is less directional than the motion fixture"
        );
        // The real point: anisotropy is a property of the SCENE as much as of the blur,
        // so a threshold tuned on one scene family does not transfer. This is asserted
        // in full against real photographs in tests/corpus.rs.
        assert!(broadband_defocus >= 1.0);
    }

    #[test]
    fn a_synthetic_burst_flags_its_smeared_frame_and_only_that_one() {
        // **The real validation of shoot-relative motion detection.**
        //
        // The corpus cannot do this: it is 50 unrelated photographs, so its "baseline" is
        // a baseline over different scenes, which is not what the design assumes. A burst
        // is the substrate the design actually requires — one scene, one lens, one
        // lighting setup, several frames.
        //
        // The baseline here is computed from ALL five frames, including the bad one,
        // because in real use the detector does not know in advance which frame is
        // damaged. That the median survives one bad frame in five is part of what is
        // being tested.
        let names = [
            "burst_0_sharp",
            "burst_1_sharp",
            "burst_2_sharp",
            "burst_3_smeared",
            "burst_4_sharp",
        ];
        let metrics: Vec<FocusMetrics> = names.iter().map(|n| analyse(&fixture(n), None)).collect();
        let baseline = ShootBaseline::from_metrics(&metrics).expect("baseline from a burst");

        let mut flagged = Vec::new();
        for (name, m) in names.iter().zip(&metrics) {
            if m.is_motion_blur_candidate(&baseline) {
                flagged.push(*name);
            }
        }

        assert_eq!(
            flagged,
            vec!["burst_3_smeared"],
            "exactly the smeared frame should be flagged; got {flagged:?}. \
             baseline: aniso {:.3}, detail {:.1}",
            baseline.anisotropy_median,
            baseline.detail_median
        );
    }

    #[test]
    fn baseline_needs_at_least_one_frame() {
        assert!(ShootBaseline::from_metrics(&[]).is_none());
        let one = vec![analyse(&fixture("sharp_a"), None)];
        let b = ShootBaseline::from_metrics(&one).expect("a single frame still yields a baseline");
        assert!(b.anisotropy_median.is_finite() && b.focus_median.is_finite());
    }

    // ---------------------------------------------------------------------
    // Contract
    // ---------------------------------------------------------------------
    #[test]
    fn analysis_is_deterministic() {
        let img = fixture("bokeh_portrait");
        let a = analyse(&img, None);
        let b = analyse(&img, None);
        assert_eq!(a, b, "the same input must always produce the same metrics");
    }

    #[test]
    fn face_boxes_take_priority_over_saliency() {
        let img = fixture("sharp_a");
        let work = prepare(&img);
        let face = Region::new(work.w / 4, work.h / 4, work.w / 4, work.h / 4);
        let m = analyse(&img, Some(&[face]));
        assert_eq!(m.roi_source, RoiSource::Faces);
        // The ROI is expanded beyond the face box, so it should contain it entirely.
        assert!(m.roi.x <= face.x && m.roi.y <= face.y);
        assert!(m.roi.x + m.roi.w >= face.x + face.w);
    }

    #[test]
    fn an_empty_face_list_falls_through_to_saliency() {
        let img = fixture("bokeh_portrait");
        let m = analyse(&img, Some(&[]));
        assert_eq!(m.roi_source, RoiSource::Saliency);
    }

    #[test]
    fn roi_never_escapes_the_image() {
        for n in ["sharp_a", "bokeh_portrait", "blur_motion", "flat_low_contrast"] {
            let img = fixture(n);
            let work = prepare(&img);
            let m = analyse(&img, None);
            assert!(
                m.roi.x + m.roi.w <= work.w && m.roi.y + m.roi.h <= work.h,
                "{n}: ROI {:?} escaped the {}x{} working image",
                m.roi,
                work.w,
                work.h
            );
        }
    }

    #[test]
    fn extreme_regions_do_not_panic_or_produce_nan() {
        // A 1x1 image, a 2x2 image, and a degenerate ROI. None may panic, divide by
        // zero, or produce NaN — all of which would be reachable from a corrupt file.
        for (w, h) in [(1usize, 1usize), (2, 2), (3, 3)] {
            let img = Luma::new(w, h, vec![100.0; w * h]);
            let m = analyse(&img, None);
            assert!(m.normalized_focus.is_finite(), "{w}x{h} produced {}", m.normalized_focus);
            assert!(m.anisotropy.is_finite());
        }

        let img = Luma::zeros(64, 64);
        let m = analyse_prepared(&img, Region::new(0, 0, 1, 1), RoiSource::WholeFrame);
        assert!(m.normalized_focus.is_finite());
    }

    #[test]
    fn a_perfectly_flat_frame_scores_zero_not_infinite() {
        let img = Luma::new(128, 128, vec![128.0; 128 * 128]);
        let m = analyse(&img, None);
        assert_eq!(
            m.normalized_focus, 0.0,
            "a frame with no detail must score zero, not divide by a near-zero contrast"
        );
    }
}
