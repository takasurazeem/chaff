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

/// Minimum directional energy ratio for a frame to be called motion-blurred.
///
/// Measured margin on the fixtures is enormous: the motion-blurred frame scores **62.1**
/// while every non-motion frame scores **1.00–1.74**, so this threshold sits roughly 35x
/// below the signal and 1.7x above the highest false positive. It is not a delicate
/// tuning.
///
/// An earlier reading of the data suggested anisotropy could not identify motion blur at
/// all, because a sharp fixture measured 6.8. That was a **fixture bug**, not a metric
/// problem: the pattern drew its vertical grid in white and its horizontal grid in black,
/// giving it unequal energy along x and y, so an isotropic frame measured as strongly
/// anisotropic. With a directionally neutral fixture the separation is decisive.
const MOTION_ANISOTROPY_MIN: f64 = 3.0;

impl FocusMetrics {
    /// True when the frame looks like directional motion blur rather than defocus.
    ///
    /// Defocus destroys structure along every axis equally and measures near-isotropic
    /// (1.00–1.04 on the fixtures). Motion blur smears along one axis only, so the
    /// perpendicular edge energy survives and the ratio explodes.
    ///
    /// **Caveat for real photographs:** a scene with genuinely dominant directional
    /// structure — a picket fence, a horizon, a curtain — can raise anisotropy without any
    /// blur. The 35x margin makes that unlikely, but if false positives appear on real
    /// libraries the right fix is to compare against the shoot's own anisotropy baseline
    /// (issue #12), not to raise this constant until nothing fires.
    pub fn looks_like_motion_blur(&self) -> bool {
        self.anisotropy >= MOTION_ANISOTROPY_MIN
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
    fn motion_blur_is_identified_decisively() {
        let motion = analyse(&fixture("blur_motion"), None);
        assert!(
            motion.looks_like_motion_blur(),
            "the motion fixture should be identified as motion-blurred, got aniso {:.3}",
            motion.anisotropy
        );
    }

    #[test]
    fn nothing_else_is_mistaken_for_motion_blur() {
        // The false-positive guard, and the test that would have caught the fixture bug
        // that originally made this signal look unreliable.
        for n in [
            "sharp_a",
            "sharp_b",
            "bokeh_portrait",
            "blur_defocus_mild",
            "blur_defocus_heavy",
            "noise_high_iso",
            "flat_low_contrast",
            "exposure_over",
            "exposure_under",
        ] {
            let m = analyse(&fixture(n), None);
            assert!(
                !m.looks_like_motion_blur(),
                "{n} must not be flagged as motion-blurred, got aniso {:.3}",
                m.anisotropy
            );
        }
    }

    #[test]
    fn the_motion_signal_has_a_wide_margin_over_every_false_positive() {
        // If this margin ever narrows, the fixed threshold stops being defensible and the
        // shoot-relative comparison in issue #12 becomes mandatory.
        let motion = analyse(&fixture("blur_motion"), None).anisotropy;
        let worst_other = [
            "sharp_a",
            "sharp_b",
            "bokeh_portrait",
            "blur_defocus_mild",
            "blur_defocus_heavy",
            "noise_high_iso",
            "flat_low_contrast",
            "exposure_over",
            "exposure_under",
        ]
        .iter()
        .map(|n| analyse(&fixture(n), None).anisotropy)
        .fold(0.0f64, f64::max);

        assert!(
            motion > worst_other * 10.0,
            "the motion signal ({motion:.2}) must dominate every false positive \
             ({worst_other:.2}) by at least an order of magnitude, or a fixed threshold is \
             not defensible"
        );
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
