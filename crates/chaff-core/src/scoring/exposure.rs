//! Exposure and tonal-range measurement.
//!
//! # What this measures, and what it deliberately does not
//!
//! It produces **raw measurements**, not a score. Converting "3.1% of pixels are
//! clipped" into a 0–100 judgement requires knowing what the rest of the shoot looked
//! like, and that happens in [`super::shoot`]. Keeping the two apart is what makes the
//! ranking shoot-relative rather than absolute, which the PRD calls its most important
//! accuracy requirement.
//!
//! # Black and white points are parameters, not constants
//!
//! A raw file's useful range is not 0–255. The sensor has a black level (often a few
//! hundred counts, and different per channel) and a white point well below the
//! container's maximum. Measuring clipping at 255 on raw data would report that nothing
//! is ever clipped, because raw highlights saturate at the white point and then the
//! remaining headroom is never used.
//!
//! So [`Levels`] is passed in. Until the LibRaw path lands (issue #8) the only caller is
//! the 8-bit decoder and it passes [`Levels::eight_bit`], but the RAW path will pass the
//! values `rawprepare` reads from the file, and nothing here has to change.

use crate::imaging::{Luma, Region};

/// The black and white points of the data being measured, in the same units as the
/// pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub black: f64,
    pub white: f64,
}

impl Levels {
    /// An 8-bit JPEG or PNG, where the container's range is the sensor's range.
    pub fn eight_bit() -> Self {
        Self { black: 0.0, white: 255.0 }
    }

    /// Levels for raw sensor data.
    ///
    /// Rejects a nonsensical range rather than producing silently wrong numbers: a white
    /// point at or below the black level would make every fraction divide by zero or go
    /// negative, and a metric that reports "0% clipped" for a broken range is worse than
    /// one that refuses.
    pub fn raw(black: f64, white: f64) -> Option<Self> {
        if !black.is_finite() || !white.is_finite() || white <= black {
            return None;
        }
        Some(Self { black, white })
    }

    pub fn range(&self) -> f64 {
        self.white - self.black
    }

    /// Normalise a pixel value into 0..1 across the range.
    pub fn normalise(&self, value: f64) -> f64 {
        (value - self.black) / self.range()
    }
}

impl Default for Levels {
    fn default() -> Self {
        Self::eight_bit()
    }
}

/// Histogram resolution. 1024 bins over 8 bits gives sub-level precision, which matters
/// because a clipping tolerance of one 8-bit level is 1/255 and a coarser histogram
/// could not express it.
const BINS: usize = 1024;

/// How close to an extreme a pixel must be to count as clipped, as a fraction of range.
///
/// One 8-bit level is `1/255 ~= 0.0039`. Pixels rarely land on the exact endpoint even
/// when the highlight is blown, so an exact-equality test would under-report clipping
/// badly on real files — and under-reporting is the dangerous direction, because the
/// user is then not warned about a photograph they cannot recover.
const CLIP_TOLERANCE: f64 = 0.004;

/// Measurements for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ExposureMetrics {
    /// Fraction of pixels at or within tolerance of the white point, 0..1.
    pub clipped_high: f64,
    /// Fraction at or within tolerance of the black point, 0..1.
    pub clipped_low: f64,
    /// Mean level across the range, 0..1.
    pub mean_level: f64,
    /// 1st percentile, 0..1.
    pub p01: f64,
    /// 50th percentile, 0..1.
    pub p50: f64,
    /// 99th percentile, 0..1.
    pub p99: f64,
    /// `p99 - p01`: how much of the available range the frame actually occupies.
    ///
    /// A low value means a flat, low-contrast exposure — haze, underexposure, or a
    /// photograph taken through a window. It is the signal that separates "dark because
    /// the scene is dark" from "dark because the exposure was wrong", which a mean level
    /// alone cannot do.
    pub range_used: f64,
    /// The levels the measurement was taken against.
    pub levels: Levels,
}

impl ExposureMetrics {
    /// Fraction of the range that is neither clipped high nor low.
    pub fn usable_range_fraction(&self) -> f64 {
        (1.0 - self.clipped_high - self.clipped_low).clamp(0.0, 1.0)
    }
}

/// Measure exposure over `roi`.
///
/// Uses a histogram rather than sorting pixels: a 45 MP frame is 45 million floats, and
/// percentiles from a 1024-bin histogram are exact to within one bin at a fraction of the
/// cost and with no allocation proportional to the image.
pub fn analyse(img: &Luma, roi: Region, levels: Levels) -> ExposureMetrics {
    let r = roi.clamped_with_border(img.w, img.h);
    let mut hist = vec![0u32; BINS];
    let mut total: u64 = 0;
    let mut sum = 0.0f64;

    if r.w == 0 || r.h == 0 || levels.range() <= 0.0 {
        return ExposureMetrics {
            clipped_high: 0.0,
            clipped_low: 0.0,
            mean_level: 0.0,
            p01: 0.0,
            p50: 0.0,
            p99: 0.0,
            range_used: 0.0,
            levels,
        };
    }

    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let v = levels.normalise(img.at(x, y) as f64).clamp(0.0, 1.0);
            // `v == 1.0` must land in the last bin, not one past it.
            let bin = ((v * (BINS - 1) as f64).round() as usize).min(BINS - 1);
            hist[bin] += 1;
            total += 1;
            sum += v;
        }
    }

    if total == 0 {
        return ExposureMetrics {
            clipped_high: 0.0,
            clipped_low: 0.0,
            mean_level: 0.0,
            p01: 0.0,
            p50: 0.0,
            p99: 0.0,
            range_used: 0.0,
            levels,
        };
    }

    let bin_width = 1.0 / (BINS - 1) as f64;
    let clipped_high_bins = (CLIP_TOLERANCE / bin_width).ceil() as usize;
    let clipped_low_bins = clipped_high_bins;

    let clipped_high: u64 =
        hist[BINS.saturating_sub(clipped_high_bins)..].iter().map(|c| *c as u64).sum();
    let clipped_low: u64 = hist[..clipped_low_bins.min(BINS)].iter().map(|c| *c as u64).sum();

    let total_f = total as f64;
    let p01 = percentile(&hist, total, 0.01);
    let p50 = percentile(&hist, total, 0.50);
    let p99 = percentile(&hist, total, 0.99);

    ExposureMetrics {
        clipped_high: clipped_high as f64 / total_f,
        clipped_low: clipped_low as f64 / total_f,
        mean_level: sum / total_f,
        p01,
        p50,
        p99,
        range_used: (p99 - p01).max(0.0),
        levels,
    }
}

/// Percentile from a cumulative histogram, returned in 0..1.
///
/// Interpolates within the containing bin so that a frame with a few distinct values does
/// not quantise every percentile to the same number — which would make the low-contrast
/// case indistinguishable from the flat case.
fn percentile(hist: &[u32], total: u64, p: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let target = (p * total as f64).round().max(1.0) as u64;
    let mut acc: u64 = 0;
    for (i, &count) in hist.iter().enumerate() {
        let count = count as u64;
        if acc + count >= target {
            let before = acc;
            let frac = if count == 0 {
                0.0
            } else {
                (target.saturating_sub(before)) as f64 / count as f64
            };
            return ((i as f64 + frac) / (hist.len() - 1) as f64).clamp(0.0, 1.0);
        }
        acc += count;
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> Luma {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images")
            .join(format!("{name}.jpg"));
        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("could not open fixture {}: {e}", path.display()))
            .to_luma8();
        Luma::from_gray8(img.width() as usize, img.height() as usize, img.as_raw())
    }

    fn full(img: &Luma) -> ExposureMetrics {
        analyse(img, Region::full(img.w, img.h), Levels::eight_bit())
    }

    /// Flat image of one value.
    fn flat(value: u8) -> Luma {
        Luma::new(32, 32, vec![value as f32; 32 * 32])
    }

    // ---------------------------------------------------------------------
    // Calibration
    // ---------------------------------------------------------------------
    #[test]
    #[ignore]
    fn calibration_report() {
        println!(
            "\n{:<26} {:>10} {:>10} {:>8} {:>8} {:>8} {:>8}",
            "fixture", "clip_high", "clip_low", "mean", "p01", "p99", "range"
        );
        println!("{}", "-".repeat(84));
        for n in [
            "exp_normal",
            "exp_over",
            "exp_under",
            "sharp_broadband",
            "sharp_a",
            "blur_defocus_heavy",
        ] {
            let m = full(&fixture(n));
            println!(
                "{:<26} {:>10.4} {:>10.4} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
                n, m.clipped_high, m.clipped_low, m.mean_level, m.p01, m.p99, m.range_used
            );
        }
        println!();
    }

    // ---------------------------------------------------------------------
    // Levels
    // ---------------------------------------------------------------------
    #[test]
    fn a_raw_range_that_is_nonsense_is_refused() {
        assert!(Levels::raw(512.0, 4095.0).is_some());
        assert!(Levels::raw(4095.0, 512.0).is_none(), "white below black");
        assert!(Levels::raw(100.0, 100.0).is_none(), "empty range");
        assert!(Levels::raw(f64::NAN, 100.0).is_none());
    }

    #[test]
    fn raw_levels_shift_what_counts_as_clipping() {
        // The same pixels, judged against an 8-bit range and against a raw range whose
        // white point is 4095. One level below 8-bit white counts as clipped there;
        // on a 0..4095 scale the same value is deep shadow.
        //
        // Note 250 does NOT count as clipped on 8-bit levels, and should not: it is 98%
        // of the range, which is bright but recoverable. The tolerance is one level, not
        // a "looks bright" heuristic.
        let img = flat(254);

        let eight = analyse(&img, Region::full(img.w, img.h), Levels::eight_bit());
        assert!(
            eight.clipped_high > 0.9,
            "254 of 255 should read as clipped on an 8-bit range, got {}",
            eight.clipped_high
        );

        let nearly = analyse(&flat(250), Region::full(img.w, img.h), Levels::eight_bit());
        assert_eq!(
            nearly.clipped_high, 0.0,
            "250 is 98% of the range — bright, not blown, and must not be reported as clipped"
        );

        let raw = Levels::raw(512.0, 4095.0).unwrap();
        let raw_m = analyse(&img, Region::full(img.w, img.h), raw);
        assert_eq!(
            raw_m.clipped_high, 0.0,
            "250 on a 0..4095 raw scale is deep shadow, and must not read as clipped"
        );
        assert!(raw_m.clipped_low > 0.0, "it is below the raw black level, so it is crushed");
    }

    // ---------------------------------------------------------------------
    // Clipping
    // ---------------------------------------------------------------------
    #[test]
    fn a_mid_grey_frame_clips_at_neither_end() {
        let m = full(&flat(128));
        assert_eq!(m.clipped_high, 0.0);
        assert_eq!(m.clipped_low, 0.0);
        assert!(m.range_used < 0.02, "a flat frame occupies almost none of the range");
    }

    #[test]
    fn a_black_frame_reads_as_fully_clipped_low() {
        let m = full(&flat(0));
        assert!(m.clipped_low > 0.99, "got {}", m.clipped_low);
        assert_eq!(m.clipped_high, 0.0);
    }

    #[test]
    fn a_white_frame_reads_as_fully_clipped_high() {
        let m = full(&flat(255));
        assert!(m.clipped_high > 0.99, "got {}", m.clipped_high);
        assert_eq!(m.clipped_low, 0.0);
    }

    #[test]
    fn overexposure_shows_up_as_high_clipping() {
        let normal = full(&fixture("exp_normal"));
        let over = full(&fixture("exp_over"));

        assert!(
            normal.clipped_high < 0.01,
            "the correctly exposed reference should not be clipped, got {}",
            normal.clipped_high
        );
        assert!(
            over.clipped_high > normal.clipped_high + 0.05,
            "overexposure must raise high clipping: normal {:.4} vs over {:.4}",
            normal.clipped_high,
            over.clipped_high
        );
    }

    #[test]
    fn underexposure_shows_up_as_a_dark_frame_with_a_narrow_range() {
        // Underexposure and crushed shadows are different faults and this fixture is the
        // former. It measures no low clipping at all — correctly, because its darkest
        // pixel is still several levels above black. What it does show is a low mean and
        // a narrow occupied range, which is what an underexposed frame actually looks
        // like. Crushing is covered by `a_black_frame_reads_as_fully_clipped_low` and
        // `negative_values_are_clamped_at_the_bottom`.
        let normal = full(&fixture("exp_normal"));
        let under = full(&fixture("exp_under"));

        assert!(
            under.mean_level < normal.mean_level * 0.5,
            "an underexposed frame is much darker: {:.3} vs {:.3}",
            under.mean_level,
            normal.mean_level
        );
        assert!(
            under.range_used < normal.range_used * 0.5,
            "and it occupies far less of the range: {:.3} vs {:.3}",
            under.range_used,
            normal.range_used
        );
        assert_eq!(
            under.clipped_low, 0.0,
            "this fixture is dark, not crushed — reporting low clipping here would be a \
             false alarm about unrecoverable shadows"
        );
    }

    #[test]
    fn a_frame_crushed_to_black_is_reported_as_low_clipped() {
        // The complement of the above, so the two faults are provably distinguishable.
        let mut px = vec![0f32; 64 * 64];
        for (i, v) in px.iter_mut().enumerate() {
            // Half the frame at true black, half in the shadows.
            *v = if i % 2 == 0 { 0.0 } else { 8.0 };
        }
        let m = full(&Luma::new(64, 64, px));
        assert!(
            m.clipped_low > 0.4,
            "half the frame at black must register as crushed, got {:.3}",
            m.clipped_low
        );
    }

    #[test]
    fn mean_level_does_not_determine_range_used() {
        // The distinction a mean cannot make, which is why `range_used` exists. Two
        // frames with an identical mean can occupy wildly different amounts of the
        // available range, and it is the range that separates a moody low-key photograph
        // from a flat, hazy one.
        //
        // The frames are constructed to share a mean exactly, so the assertion is about
        // the range and nothing else.
        let mut full_contrast = vec![0f32; 64 * 64];
        let mut low_contrast = vec![0f32; 64 * 64];
        for y in 0..64 {
            for x in 0..64 {
                let i = y * 64 + x;
                let t = x as f32 / 63.0;
                full_contrast[i] = 20.0 + t * 90.0; // 20..110, mean 65
                low_contrast[i] = 57.5 + t * 15.0; // 57.5..72.5, mean 65
            }
        }
        let a = full(&Luma::new(64, 64, full_contrast));
        let b = full(&Luma::new(64, 64, low_contrast));

        assert!(
            (a.mean_level - b.mean_level).abs() < 0.02,
            "the frames are constructed to share a mean: {:.4} vs {:.4}",
            a.mean_level,
            b.mean_level
        );
        assert!(
            a.range_used > b.range_used * 3.0,
            "yet one uses far more of the range: {:.3} vs {:.3}",
            a.range_used,
            b.range_used
        );
    }

    // ---------------------------------------------------------------------
    // Robustness
    // ---------------------------------------------------------------------
    #[test]
    fn a_perfectly_flat_frame_does_not_produce_nan() {
        let m = full(&flat(77));
        assert!(m.mean_level.is_finite());
        assert!(m.p01.is_finite() && m.p50.is_finite() && m.p99.is_finite());
        assert!(m.range_used.is_finite() && m.range_used >= 0.0);
    }

    #[test]
    fn degenerate_inputs_are_survivable() {
        for (w, h) in [(1usize, 1usize), (2, 2), (3, 3)] {
            let img = Luma::new(w, h, vec![100.0; w * h]);
            let m = analyse(&img, Region::full(w, h), Levels::eight_bit());
            assert!(m.mean_level.is_finite());
        }
        // An ROI entirely outside the image.
        let img = flat(100);
        let m = analyse(&img, Region::new(10_000, 10_000, 5, 5), Levels::eight_bit());
        assert!(m.mean_level.is_finite());
    }

    #[test]
    fn values_beyond_the_range_are_clamped_not_wrapped() {
        // Raw data can exceed the declared white point (a hot pixel, a slightly wrong
        // level). Wrapping would put a blown highlight at the bottom of the histogram and
        // report it as a crushed shadow — the exact opposite of the truth.
        let img = Luma::new(16, 16, vec![400.0; 16 * 16]);
        let m = analyse(&img, Region::full(16, 16), Levels::eight_bit());
        assert!(m.clipped_high > 0.99, "400 is far above white and must read as clipped high");
        assert_eq!(m.clipped_low, 0.0);
    }

    #[test]
    fn negative_values_are_clamped_at_the_bottom() {
        let img = Luma::new(16, 16, vec![-50.0; 16 * 16]);
        let m = analyse(&img, Region::full(16, 16), Levels::eight_bit());
        assert!(m.clipped_low > 0.99);
        assert_eq!(m.clipped_high, 0.0);
    }

    #[test]
    fn measurement_is_deterministic() {
        let img = fixture("exp_over");
        assert_eq!(full(&img), full(&img));
    }

    #[test]
    fn percentiles_are_ordered() {
        let m = full(&fixture("exp_normal"));
        assert!(m.p01 <= m.p50, "p01 {} > p50 {}", m.p01, m.p50);
        assert!(m.p50 <= m.p99, "p50 {} > p99 {}", m.p50, m.p99);
    }

    #[test]
    fn a_measurement_over_part_of_the_frame_differs_from_the_whole() {
        // The ROI plumbing must actually be used. A bright patch in one corner should
        // raise clipping when measured there and barely register over the whole frame.
        let (w, h) = (128, 128);
        let mut px = vec![64.0f32; w * h];
        for y in 0..32 {
            for x in 0..32 {
                px[y * w + x] = 255.0;
            }
        }
        let img = Luma::new(w, h, px);

        let whole = analyse(&img, Region::full(w, h), Levels::eight_bit());
        let corner = analyse(&img, Region::new(0, 0, 32, 32), Levels::eight_bit());

        assert!(whole.clipped_high < 0.10, "the patch is 1/16 of the frame");
        assert!(corner.clipped_high > 0.99, "measured on the patch it is everything");
    }
}
