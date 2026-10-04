//! Image primitives for the scoring pipeline.
//!
//! Pure numeric operations over a single-channel luma buffer. No Tauri, no I/O,
//! no decoder — decoding happens at the edge and hands a [`Luma`] in.
//!
//! ## The one rule this module exists to enforce
//!
//! **Never measure at full resolution.** A 45 MP raw is 45 million floats (180 MB) per
//! channel. Every metric here is scale-robust, so the pipeline normalises to a bounded
//! long edge first. This is where the memory budget is actually won or lost, not in the
//! choice of UI framework.

/// A single-channel float luma image. Values are nominally 0–255 but nothing here
/// clamps, so callers may pass linear or scaled data.
#[derive(Debug, Clone, PartialEq)]
pub struct Luma {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Luma {
    pub fn new(w: usize, h: usize, px: Vec<f32>) -> Self {
        assert_eq!(px.len(), w * h, "buffer length must equal w*h");
        Self { w, h, px }
    }

    pub fn zeros(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0.0; w * h] }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.px[y * self.w + x]
    }

    /// Build from an RGB byte buffer using Rec. 709 luma weights.
    pub fn from_rgb8(w: usize, h: usize, rgb: &[u8]) -> Self {
        assert_eq!(rgb.len(), w * h * 3, "rgb buffer length must equal w*h*3");
        let mut px = Vec::with_capacity(w * h);
        for c in rgb.chunks_exact(3) {
            let r = c[0] as f32;
            let g = c[1] as f32;
            let b = c[2] as f32;
            px.push(0.2126 * r + 0.7152 * g + 0.0722 * b);
        }
        Self { w, h, px }
    }

    pub fn from_gray8(w: usize, h: usize, gray: &[u8]) -> Self {
        assert_eq!(gray.len(), w * h, "gray buffer length must equal w*h");
        Self { w, h, px: gray.iter().map(|&v| v as f32).collect() }
    }

    /// Box-filter downscale to a bounded long edge.
    ///
    /// Box rather than Lanczos on purpose: a ringing resampler *invents* high-frequency
    /// energy, which a sharpness metric would then measure as real detail. That would be
    /// a metric that reports its own resampling artefacts as focus.
    pub fn downscale_to_long_edge(&self, target: usize) -> Luma {
        let long = self.w.max(self.h);
        if long <= target || target == 0 {
            return self.clone();
        }
        let factor = (long as f64 / target as f64).ceil() as usize;
        if factor < 2 {
            return self.clone();
        }
        self.box_downscale(factor)
    }

    /// Integer-factor box downscale by averaging `factor x factor` blocks.
    pub fn box_downscale(&self, factor: usize) -> Luma {
        let w = (self.w / factor).max(1);
        let h = (self.h / factor).max(1);
        let mut px = Vec::with_capacity(w * h);
        let inv = 1.0 / (factor * factor) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0f32;
                for dy in 0..factor {
                    let sy = y * factor + dy;
                    if sy >= self.h {
                        continue;
                    }
                    for dx in 0..factor {
                        let sx = x * factor + dx;
                        if sx >= self.w {
                            continue;
                        }
                        acc += self.at(sx, sy);
                    }
                }
                px.push(acc * inv);
            }
        }
        Luma { w, h, px }
    }
}

/// A rectangular region of interest, in pixels, with the origin at the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Region {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub fn full(w: usize, h: usize) -> Self {
        Self { x: 0, y: 0, w, h }
    }

    /// A centred crop of the given fraction, used as the last-resort ROI.
    pub fn centre(w: usize, h: usize, frac: f64) -> Self {
        let cw = ((w as f64) * frac) as usize;
        let ch = ((h as f64) * frac) as usize;
        Self { x: (w - cw) / 2, y: (h - ch) / 2, w: cw.max(1), h: ch.max(1) }
    }

    /// Shrink so the region plus a 1px border fits inside `w x h`.
    ///
    /// Every convolution here reads a 3x3 neighbourhood, so a region touching the image
    /// edge would read out of bounds. Insetting by one is simpler and safer than
    /// replicating edge pixels, and one pixel of a 1024px edge is irrelevant.
    pub fn clamped_with_border(&self, w: usize, h: usize) -> Region {
        let x0 = self.x.min(w.saturating_sub(1));
        let y0 = self.y.min(h.saturating_sub(1));
        let x1 = (self.x + self.w).min(w);
        let y1 = (self.y + self.h).min(h);

        let x0b = (x0 + 1).min(w.saturating_sub(1));
        let y0b = (y0 + 1).min(h.saturating_sub(1));
        let x1b = x1.saturating_sub(1).max(x0b + 1).min(w.saturating_sub(1));
        let y1b = y1.saturating_sub(1).max(y0b + 1).min(h.saturating_sub(1));

        Region { x: x0b, y: y0b, w: x1b.saturating_sub(x0b).max(1), h: y1b.saturating_sub(y0b).max(1) }
    }

    pub fn area(&self) -> usize {
        self.w * self.h
    }
}

// ---------------------------------------------------------------------------
// Convolution responses
// ---------------------------------------------------------------------------

/// Variance of the 3x3 Laplacian response over `roi`.
///
/// The classic blur metric. Cheap, well understood, and — used alone — wrong on any
/// scene with intentional shallow depth of field. See [`super::scoring::focus`] for how
/// it is made trustworthy.
pub fn laplacian_variance(img: &Luma, roi: Region) -> f64 {
    let r = roi.clamped_with_border(img.w, img.h);
    if r.w < 3 || r.h < 3 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut n = 0u64;

    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let c = img.at(x, y) as f64;
            let up = img.at(x, y - 1) as f64;
            let down = img.at(x, y + 1) as f64;
            let left = img.at(x - 1, y) as f64;
            let right = img.at(x + 1, y) as f64;
            let lap = up + down + left + right - 4.0 * c;
            sum += lap;
            sum_sq += lap * lap;
            n += 1;
        }
    }

    if n == 0 {
        return 0.0;
    }
    let nf = n as f64;
    let mean = sum / nf;
    (sum_sq / nf - mean * mean).max(0.0)
}

/// Number of taps squared-sum for the 3x3 Laplacian kernel `[0,1,0; 1,-4,1; 0,1,0]`.
///
/// For additive white noise of variance `s^2`, the variance of the convolution response
/// is `s^2 * LAPLACIAN_KERNEL_SQ_SUM`. This is the constant that makes noise correction
/// possible instead of heuristic.
pub const LAPLACIAN_KERNEL_SQ_SUM: f64 = 20.0; // 1+1+16+1+1

/// Mean Sobel gradient magnitude over `roi` — the local-contrast measure.
pub fn gradient_magnitude_mean(img: &Luma, roi: Region) -> f64 {
    let r = roi.clamped_with_border(img.w, img.h);
    if r.w < 3 || r.h < 3 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let (gx, gy) = sobel_at(img, x, y);
            sum += (gx * gx + gy * gy).sqrt();
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f64
    }
}

/// Directional edge energy over `roi`, as `(energy_x, energy_y)`.
///
/// `energy_x` responds to *vertical* edges (detail varying along x), `energy_y` to
/// horizontal edges. Motion blur along one axis selectively destroys the edges
/// perpendicular to it, which is what makes motion separable from defocus.
pub fn directional_energy(img: &Luma, roi: Region) -> (f64, f64) {
    let r = roi.clamped_with_border(img.w, img.h);
    if r.w < 3 || r.h < 3 {
        return (0.0, 0.0);
    }
    let mut ex = 0.0f64;
    let mut ey = 0.0f64;
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let (gx, gy) = sobel_at(img, x, y);
            ex += gx * gx;
            ey += gy * gy;
        }
    }
    (ex, ey)
}

#[inline]
fn sobel_at(img: &Luma, x: usize, y: usize) -> (f64, f64) {
    let tl = img.at(x - 1, y - 1) as f64;
    let t = img.at(x, y - 1) as f64;
    let tr = img.at(x + 1, y - 1) as f64;
    let l = img.at(x - 1, y) as f64;
    let r = img.at(x + 1, y) as f64;
    let bl = img.at(x - 1, y + 1) as f64;
    let b = img.at(x, y + 1) as f64;
    let br = img.at(x + 1, y + 1) as f64;

    let gx = (tr + 2.0 * r + br) - (tl + 2.0 * l + bl);
    let gy = (bl + 2.0 * b + br) - (tl + 2.0 * t + tr);
    (gx, gy)
}

/// Variance of the luma values themselves over `roi`.
///
/// This is the normaliser for the focus metric, and it is the right one because it makes
/// the ratio scale-invariant: scaling a scene's contrast by any factor `k` multiplies
/// both the Laplacian variance and this by `k^2`, so their ratio is unchanged. The metric
/// therefore measures *how much of the available tonal range is resolved as fine detail*
/// rather than how contrasty the scene happens to be.
pub fn luma_variance(img: &Luma, roi: Region) -> f64 {
    let r = roi.clamped_with_border(img.w, img.h);
    if r.area() == 0 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut n = 0u64;
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let v = img.at(x, y) as f64;
            sum += v;
            sum_sq += v * v;
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    let nf = n as f64;
    (sum_sq / nf - (sum / nf).powi(2)).max(0.0)
}

/// Immerkær's noise estimate: standard deviation of additive white noise.
///
/// Convolves with the mask `[1,-2,1; -2,4,-2; 1,-2,1]` — which is zero-mean and therefore
/// blind to smooth image content — and converts the mean absolute response to a sigma
/// via the Gaussian MAD identity. Robust in practice and, unlike "look at the flattest
/// patch", it does not need to find a flat patch in an image that has none.
pub fn estimate_noise_sigma(img: &Luma, roi: Region) -> f64 {
    let r = roi.clamped_with_border(img.w, img.h);
    if r.w < 3 || r.h < 3 {
        return 0.0;
    }
    let mut sum_abs = 0.0f64;
    let mut n = 0u64;
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let tl = img.at(x - 1, y - 1) as f64;
            let t = img.at(x, y - 1) as f64;
            let tr = img.at(x + 1, y - 1) as f64;
            let l = img.at(x - 1, y) as f64;
            let c = img.at(x, y) as f64;
            let rr = img.at(x + 1, y) as f64;
            let bl = img.at(x - 1, y + 1) as f64;
            let b = img.at(x, y + 1) as f64;
            let br = img.at(x + 1, y + 1) as f64;

            let m = (tl + tr + bl + br) - 2.0 * (t + l + rr + b) + 4.0 * c;
            sum_abs += m.abs();
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    let mean_abs = sum_abs / n as f64;
    // sigma = sqrt(pi/2) * mean(|response|) / 6   (Immerkaer 1996)
    (std::f64::consts::FRAC_PI_2.sqrt() * mean_abs / 6.0).max(0.0)
}

/// Coefficient of variation of the Laplacian response across `tiles x tiles` sub-tiles.
///
/// A reliable frame is sharp *throughout* its subject; a frame with one lucky edge is
/// not. High variation means the sharpness is concentrated, which is a weaker signal.
pub fn response_variation_across_tiles(img: &Luma, roi: Region, tiles: usize) -> f64 {
    let r = roi.clamped_with_border(img.w, img.h);
    if r.w < tiles * 3 || r.h < tiles * 3 || tiles < 2 {
        return 0.0;
    }
    let tw = r.w / tiles;
    let th = r.h / tiles;
    let mut vals = Vec::with_capacity(tiles * tiles);
    for ty in 0..tiles {
        for tx in 0..tiles {
            let sub = Region::new(r.x + tx * tw, r.y + ty * th, tw, th);
            vals.push(laplacian_variance(img, sub));
        }
    }
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    if mean <= f64::EPSILON {
        return 0.0;
    }
    let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    var.sqrt() / mean
}

/// Find the region of significant detail by tiling the frame and thresholding per-tile
/// detail energy.
///
/// Returns `None` when nothing stands out — either because there is no detail at all, or
/// because detail is spread evenly across the whole frame. Both are honest answers, and
/// the caller falls back to a centre crop.
///
/// ## Why this scores tiles with the Laplacian, not with Sobel
///
/// The region chosen here is the region the *focus metric* will measure, so it must be
/// chosen by the same operator that metric uses. Scoring tiles by Sobel gradient energy
/// instead produced a concrete, reproducible bug: **the Sobel operator is identically
/// zero on a one-pixel checkerboard.** Every term cancels — `gx = (tr + 2r + br) -
/// (tl + 2l + bl)` has identical terms on both sides when the pattern alternates every
/// pixel at the Nyquist frequency. So a frame whose only detail was Nyquist-frequency
/// texture was reported as having no salient region at all, while the Laplacian measured
/// that same texture as very strong detail.
///
/// Matching the operator to the metric removes the disagreement by construction.
///
/// ## Why this thresholds on energy rather than taking the top-N tiles
///
/// An earlier version sorted tiles by energy and took a fixed top fraction. That is
/// wrong in two ways, both of which produced measurable false results:
///
/// * **Ties break by insertion order.** On a frame that is flat everywhere except one
///   patch, every background tile has exactly zero energy. The fixed quota then had to
///   fill its remaining slots with zero-energy tiles, and because iteration starts at the
///   top-left, the bounding box stretched up to row 0 — so the "salient region" included
///   an arbitrary strip of the empty background and its centre landed nowhere near the
///   subject.
/// * **Uniform detail looks salient.** On a frame where every tile has *equal* energy,
///   the top quartile is still a quarter of the tiles, so a confident-looking box was
///   returned around whichever tiles happened to be enumerated first.
///
/// Thresholding at a fraction of peak energy fixes both: background tiles at zero cannot
/// qualify, and if everything qualifies then nothing is distinctive and we say so.
///
/// `significant_fraction` is the share of peak tile energy a tile must reach to count.
pub fn saliency_region(
    img: &Luma,
    tiles_x: usize,
    tiles_y: usize,
    significant_fraction: f64,
) -> Option<Region> {
    if tiles_x < 2 || tiles_y < 2 {
        return None;
    }
    let tw = img.w / tiles_x;
    let th = img.h / tiles_y;
    if tw < 3 || th < 3 {
        return None;
    }

    let mut scored: Vec<(f64, usize, usize)> = Vec::with_capacity(tiles_x * tiles_y);
    let mut peak = 0.0f64;
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let sub = Region::new(tx * tw, ty * th, tw, th);
            let e = laplacian_variance(img, sub);
            peak = peak.max(e);
            scored.push((e, tx, ty));
        }
    }

    if peak <= 0.0 {
        return None;
    }

    let threshold = peak * significant_fraction;
    let salient: Vec<&(f64, usize, usize)> =
        scored.iter().filter(|(e, _, _)| *e >= threshold).collect();

    // If essentially everything is salient, nothing is distinctive.
    if salient.len() * 10 >= scored.len() * 9 {
        return None;
    }

    let (mut min_tx, mut min_ty) = (usize::MAX, usize::MAX);
    let (mut max_tx, mut max_ty) = (0usize, 0usize);
    for &&(_, tx, ty) in &salient {
        min_tx = min_tx.min(tx);
        min_ty = min_ty.min(ty);
        max_tx = max_tx.max(tx);
        max_ty = max_ty.max(ty);
    }

    // Expand by one tile of margin and clamp: a subject's edges matter as much as its
    // centre, and a box tight to the energy peak clips them.
    let x0 = min_tx.saturating_sub(1) * tw;
    let y0 = min_ty.saturating_sub(1) * th;
    let x1 = ((max_tx + 2) * tw).min(img.w);
    let y1 = ((max_ty + 2) * th).min(img.h);

    let region =
        Region::new(x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));

    let full = Region::full(img.w, img.h);
    if region.area() * 10 >= full.area() * 9 {
        None
    } else {
        Some(region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_respects_the_target_long_edge() {
        let img = Luma::zeros(4000, 3000);
        let small = img.downscale_to_long_edge(1024);
        assert!(small.w.max(small.h) <= 1024, "long edge was {}", small.w.max(small.h));
    }

    #[test]
    fn downscale_is_a_no_op_below_the_target() {
        let img = Luma::zeros(100, 80);
        assert_eq!(img.downscale_to_long_edge(1024), img);
    }

    #[test]
    fn box_downscale_averages_rather_than_samples() {
        // A checkerboard averaged 2x2 becomes flat mid-grey. Point sampling would keep
        // the extremes and invent detail that is not in the downscaled image.
        let mut px = vec![0.0f32; 4 * 4];
        for y in 0..4 {
            for x in 0..4 {
                px[y * 4 + x] = if (x + y) % 2 == 0 { 0.0 } else { 100.0 };
            }
        }
        let img = Luma::new(4, 4, px);
        let d = img.box_downscale(2);
        assert_eq!(d.w, 2);
        for &v in &d.px {
            assert!((v - 50.0).abs() < 1e-4, "expected flat 50, got {v}");
        }
    }

    #[test]
    fn laplacian_variance_is_zero_on_a_constant_field() {
        let img = Luma::new(16, 16, vec![42.0; 256]);
        assert_eq!(laplacian_variance(&img, Region::full(16, 16)), 0.0);
    }

    #[test]
    fn laplacian_variance_rises_with_edge_contrast() {
        let mut low = vec![0.0f32; 32 * 32];
        let mut high = vec![0.0f32; 32 * 32];
        for y in 0..32 {
            for x in 0..32 {
                let step = if x < 16 { 0.0 } else { 1.0 };
                low[y * 32 + x] = 100.0 + step * 10.0;
                high[y * 32 + x] = 100.0 + step * 100.0;
            }
        }
        let a = laplacian_variance(&Luma::new(32, 32, low), Region::full(32, 32));
        let b = laplacian_variance(&Luma::new(32, 32, high), Region::full(32, 32));
        assert!(b > a * 10.0, "contrast should dominate: {a} vs {b}");
    }

    #[test]
    fn noise_estimate_is_near_zero_on_a_clean_gradient() {
        let mut px = vec![0.0f32; 64 * 64];
        for y in 0..64 {
            for x in 0..64 {
                px[y * 64 + x] = x as f32;
            }
        }
        let sigma = estimate_noise_sigma(&Luma::new(64, 64, px), Region::full(64, 64));
        assert!(sigma < 0.5, "a clean ramp should report ~0 noise, got {sigma}");
    }

    #[test]
    fn sobel_is_blind_at_nyquist_while_the_laplacian_is_not() {
        // The finding that forced saliency to switch operators.
        //
        // On a one-pixel checkerboard the Sobel response is identically zero: at (x,y),
        // `gx = (tr + 2r + br) - (tl + 2l + bl)` has the same value on both sides because
        // every diagonal neighbour equals the centre and every axial neighbour equals its
        // opposite. The pattern sits exactly at the Nyquist frequency and the symmetric
        // kernel cancels it perfectly.
        //
        // The Laplacian has no such blind spot. So a gradient-based region selector
        // genuinely cannot see detail that the Laplacian-based metric will then measure
        // as very strong — which is how a frame full of fine texture ended up being
        // reported as having no salient region at all.
        let (w, h) = (32, 32);
        let mut px = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                px[y * w + x] = if (x + y) % 2 == 0 { 0.0 } else { 255.0 };
            }
        }
        let img = Luma::new(w, h, px);
        let full = Region::full(w, h);

        let (ex, ey) = directional_energy(&img, full);
        assert!(
            ex < 1e-6 && ey < 1e-6,
            "Sobel should vanish exactly on a Nyquist checkerboard, got ({ex}, {ey})"
        );

        let lap = laplacian_variance(&img, full);
        assert!(
            lap > 1e5,
            "the Laplacian should respond strongly to the same pattern, got {lap}"
        );
    }

    #[test]
    fn noise_estimate_recovers_a_known_sigma() {
        // Deterministic pseudo-noise: a fixed LCG, so this test cannot flake.
        //
        // The divisor matters. `state >> 8` spans 24 bits, so dividing by 2^24 maps it to
        // [0,1). An earlier version divided by 2^20, which produced a range of about
        // [-0.5, 15.5) — a ramp with a small wiggle on it, not noise — and the estimator
        // dutifully reported sigma 111.
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / 16_777_216.0) - 0.5 // uniform in [-0.5, 0.5)
        };
        let amplitude = 12.0f32; // uniform(-6, 6) has sd = 12/sqrt(12) ~= 3.464
        let px: Vec<f32> = (0..128 * 128).map(|_| 128.0 + next() * amplitude).collect();
        let sigma = estimate_noise_sigma(&Luma::new(128, 128, px), Region::full(128, 128));
        assert!(
            (2.5..4.5).contains(&sigma),
            "expected ~3.46 noise, got {sigma} — the estimator should recover a known sigma"
        );
    }

    #[test]
    fn region_insets_by_one_pixel_at_the_border() {
        let r = Region::full(10, 10).clamped_with_border(10, 10);
        assert_eq!(r, Region::new(1, 1, 8, 8));
    }

    #[test]
    fn region_clamps_when_asked_for_more_than_exists() {
        let r = Region::new(5, 5, 1000, 1000).clamped_with_border(20, 20);
        assert!(r.x + r.w <= 20 && r.y + r.h <= 20, "region escaped the image: {r:?}");
    }

    #[test]
    fn centre_region_is_centred() {
        let r = Region::centre(100, 50, 0.5);
        assert_eq!(r.w, 50);
        assert_eq!(r.h, 25);
        assert_eq!(r.x, 25);
    }

    #[test]
    fn saliency_finds_a_bright_detail_patch_over_a_flat_field() {
        // Flat background with one high-detail square. Saliency should land on it.
        let (w, h) = (160, 120);
        let mut px = vec![128.0f32; w * h];
        for y in 30..90 {
            for x in 60..100 {
                px[y * w + x] = if (x + y) % 2 == 0 { 20.0 } else { 235.0 };
            }
        }
        let img = Luma::new(w, h, px);
        let r = saliency_region(&img, 16, 12, 0.15).expect("detail patch should be salient");
        let cx = r.x + r.w / 2;
        let cy = r.y + r.h / 2;
        assert!((55..105).contains(&cx), "saliency centre x was {cx}, expected in 55..105");
        assert!((25..95).contains(&cy), "saliency centre y was {cy}, expected in 25..95");
    }

    #[test]
    fn saliency_returns_none_when_detail_is_everywhere() {
        // Uniform high-frequency content: no region is more salient than any other, so
        // an honest implementation declines rather than inventing a box.
        let (w, h) = (160, 120);
        let mut px = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                px[y * w + x] = if (x + y) % 2 == 0 { 0.0 } else { 255.0 };
            }
        }
        let img = Luma::new(w, h, px);
        assert_eq!(saliency_region(&img, 16, 12, 0.25), None);
    }
}
