//! Burst grouping, bracket detection, and keeper selection.
//!
//! # Three related but distinct problems
//!
//! 1. **A burst** is several frames of one moment. Cull it to the best one or two.
//! 2. **A bracket** is deliberate exposure or focus variation. It is *not* redundant, and
//!    culling it to one frame destroys the thing the photographer was making. It must
//!    never be treated as a burst.
//! 3. **Near-duplicates** are the same frame twice — a copy import, a re-export. Also
//!    redundant, but they are not a sequence and do not have a "best" one.
//!
//! Conflating 1 and 2 is the expensive mistake: a photographer brackets because they
//! intend to blend or choose later, and a tool that silently keeps one frame of a
//! three-stop bracket has deleted two thirds of an intentional set.
//!
//! # Why grouping needs more than a timestamp
//!
//! Capture time alone splits a real burst when the clock is wrong, and merges unrelated
//! frames when a photographer fires two different compositions within the same second.
//! A perceptual hash supplies the missing signal: frames of one moment look nearly
//! identical, and frames of different compositions do not, however close together they
//! were taken.
//!
//! So a burst requires **all three**: same camera, close in time, and visually near
//! identical.
//!
//! # The hash is dHash, not the DCT pHash the PRD names
//!
//! A deliberate deviation. The PRD specifies a DCT-based pHash; this implements a
//! difference hash — resize to 9x8, compare each pixel to its right-hand neighbour,
//! yielding 64 bits.
//!
//! DCT pHash is more robust to *editing*: rescaling, watermarking, mild colour grading.
//! None of that is what this is for. Burst detection needs to tell "the same moment,
//! slightly different" from "a different moment", and the frames involved differ only by
//! micro-motion, exposure and noise. dHash separates those cleanly, needs no DCT, and is
//! a few lines instead of sixty. If near-duplicate detection across *edited* copies is
//! ever wanted, that is a different problem and deserves the DCT version.

use std::collections::{HashMap, HashSet};

use crate::imaging::Luma;

// ---------------------------------------------------------------------------
// Perceptual hash
// ---------------------------------------------------------------------------
/// 64-bit difference hash.
///
/// Each bit records whether a pixel is darker than the one to its right, on a 9x8 grid.
/// The result is invariant to overall brightness scaling — which is exactly what a
/// bracket varies — and to small shifts, because the 9x8 grid is produced by area
/// averaging rather than by sampling.
pub fn perceptual_hash(img: &Luma) -> u64 {
    let small = img.resize_area(9, 8);
    let mut bits = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            // `wrapping` is unnecessary: the loop bounds are fixed at 8x8.
            if small.at(x, y) < small.at(x + 1, y) {
                bits |= 1u64 << (y * 8 + x);
            }
        }
    }
    bits
}

/// Number of differing bits between two hashes.
pub fn hash_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BurstConfig {
    /// Frames further apart than this start a new burst.
    ///
    /// Two seconds. A burst is a held shutter or a rapid sequence; anything slower is a
    /// photographer choosing a new moment.
    pub max_gap_seconds: i64,
    /// Hashes further apart than this are different compositions.
    ///
    /// Eight of 64 bits. Tight enough to separate two compositions shot a second apart,
    /// loose enough to keep a burst together across micro-motion and a stop of exposure.
    pub max_hash_distance: u32,
    /// Frames kept per burst.
    ///
    /// **Two, not one.** The safety valve: keeper selection is a guess, and the cost of
    /// the guess being wrong is a photograph the user never sees. Two keepers halves the
    /// damage while still removing most of the redundancy.
    pub keepers_per_burst: usize,
    /// How far an exposure ratio may sit from a whole or third of a stop.
    pub ev_tolerance: f64,
    /// Frames needed before a group can be called a bracket.
    pub min_bracket_frames: usize,
}

impl Default for BurstConfig {
    fn default() -> Self {
        Self {
            max_gap_seconds: 2,
            max_hash_distance: 8,
            keepers_per_burst: 2,
            ev_tolerance: 0.20,
            min_bracket_frames: 3,
        }
    }
}

impl BurstConfig {
    pub fn is_valid(&self) -> bool {
        self.max_gap_seconds > 0
            && self.max_hash_distance <= 64
            && self.keepers_per_burst >= 1
            && self.ev_tolerance >= 0.0
            && self.min_bracket_frames >= 2
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------
/// One photograph's identity for grouping purposes.
#[derive(Debug, Clone, PartialEq)]
pub struct BurstFrame {
    pub photo_id: i64,
    pub camera: Option<String>,
    pub captured_at: Option<i64>,
    pub hash: Option<u64>,
    pub exposure_time: Option<f64>,
    pub iso: Option<u32>,
}

impl BurstFrame {
    pub fn new(photo_id: i64) -> Self {
        Self {
            photo_id,
            camera: None,
            captured_at: None,
            hash: None,
            exposure_time: None,
            iso: None,
        }
    }

    pub fn with_camera(mut self, camera: Option<String>) -> Self {
        self.camera = camera.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        self
    }

    pub fn with_time(mut self, t: Option<i64>) -> Self {
        self.captured_at = t;
        self
    }

    pub fn with_hash(mut self, h: Option<u64>) -> Self {
        self.hash = h;
        self
    }

    pub fn with_exposure(mut self, seconds: Option<f64>, iso: Option<u32>) -> Self {
        self.exposure_time = seconds.filter(|t| *t > 0.0);
        self.iso = iso;
        self
    }

    pub fn with_exif(mut self, exif: Option<&crate::exif::ExifData>) -> Self {
        if let Some(e) = exif {
            self.camera = e.camera_key();
            self.captured_at = e.captured_at;
            self.exposure_time = e.exposure_time.filter(|t| *t > 0.0);
            self.iso = e.iso;
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------
/// What kind of group this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstKind {
    /// Redundant frames of one moment. Cull to the keepers.
    Burst,
    /// Deliberate exposure variation. **Never culled** — every frame is intentional.
    Bracket,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Burst {
    pub id: usize,
    pub kind: BurstKind,
    /// Indices into the slice that was grouped, in capture order.
    pub frames: Vec<usize>,
    pub camera: Option<String>,
}

impl Burst {
    pub fn size(&self) -> usize {
        self.frames.len()
    }

    /// A group of one is not a burst and needs no decision.
    pub fn is_redundant_group(&self) -> bool {
        self.size() > 1
    }
}

/// Which frames of a burst to keep.
#[derive(Debug, Clone, PartialEq)]
pub struct BurstSelection {
    pub burst_id: usize,
    pub kind: BurstKind,
    /// Photo ids to keep. For a bracket, this is every frame.
    pub keepers: Vec<i64>,
    /// Photo ids that are redundant with a keeper. Never deleted by this module.
    pub redundant: Vec<i64>,
    /// True when a manual promotion forced a frame into the keepers.
    pub had_manual_override: bool,
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------
/// Group frames into bursts and brackets.
///
/// Deterministic: the same input always produces the same groups in the same order.
///
/// Frames with no capture time are **not** burst-grouped. A burst is defined by being a
/// sequence in time, and grouping on hash alone would merge two deliberate near-identical
/// compositions taken minutes apart — which is a duplicate problem, not a burst problem,
/// and deserves to be surfaced differently.
pub fn group_into_bursts(frames: &[BurstFrame], config: &BurstConfig) -> Vec<Burst> {
    // Bucket by camera, then order by time.
    let mut buckets: std::collections::BTreeMap<Option<String>, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, f) in frames.iter().enumerate() {
        if f.captured_at.is_none() {
            continue;
        }
        buckets.entry(f.camera.clone()).or_default().push(i);
    }

    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();

    for (camera, mut idxs) in buckets {
        idxs.sort_by_key(|&i| (frames[i].captured_at.unwrap_or(0), frames[i].photo_id));

        let mut current: Vec<usize> = Vec::new();
        let mut prev_time: Option<i64> = None;
        let mut prev_hash: Option<u64> = None;

        for i in idxs {
            let t = frames[i].captured_at.unwrap_or(0);
            let h = frames[i].hash;

            let split = if current.is_empty() {
                false
            } else {
                let time_split =
                    prev_time.map(|p| t.saturating_sub(p) > config.max_gap_seconds).unwrap_or(false);
                // A hash comparison is only meaningful when both frames have one. Missing
                // hashes must not split a burst, because absence of evidence is not
                // evidence of a different composition.
                let hash_split = match (prev_hash, h) {
                    (Some(a), Some(b)) => hash_distance(a, b) > config.max_hash_distance,
                    _ => false,
                };
                time_split || hash_split
            };

            if split {
                groups.push((camera.clone(), std::mem::take(&mut current)));
            }
            current.push(i);
            prev_time = Some(t);
            prev_hash = h;
        }
        if !current.is_empty() {
            groups.push((camera, current));
        }
    }

    // Deterministic order regardless of how the buckets iterated.
    groups.sort_by_key(|(_, idxs)| idxs.iter().map(|&i| frames[i].photo_id).min().unwrap_or(0));

    groups
        .into_iter()
        .enumerate()
        .map(|(id, (camera, idxs))| {
            let kind = if is_bracket(frames, &idxs, config) {
                BurstKind::Bracket
            } else {
                BurstKind::Burst
            };
            Burst { id, kind, frames: idxs, camera }
        })
        .collect()
}

/// True when a group looks like a deliberate exposure bracket.
///
/// Four conditions, all required:
///
/// 1. At least `min_bracket_frames` frames carry an exposure time.
/// 2. At least two distinct exposures — an unvarying sequence is a plain burst.
/// 3. ISO is constant. Auto-ISO varies with exposure, so a group where both move together
///    is a photographer chasing the light, not bracketing it.
/// 4. **Every step is the same size.** This is the discriminator, and the first version
///    of this function lacked it. Requiring only that each step land near a third of a
///    stop accepted `1/500, 1/300, 1/125` — steps of 0.74 and 1.26 EV, which are each
///    *close to* some multiple of a third but are not a sequence anyone brackets with.
///
///    A bracket steps by identical increments because the camera was told to. A burst
///    where the meter happened to move produces irregular steps. Uniformity is what
///    separates the two, and it is the only signal available without maker-note access.
///
/// The error is deliberately asymmetric. Calling a burst a bracket means the user culls
/// it by hand — mild. Calling a bracket a burst means frames the photographer intended to
/// blend get culled to two — severe. So where the evidence is ambiguous, this says
/// "burst" only when the steps genuinely disagree with each other.
fn is_bracket(frames: &[BurstFrame], idxs: &[usize], config: &BurstConfig) -> bool {
    let exposures: Vec<f64> = idxs
        .iter()
        .filter_map(|&i| frames[i].exposure_time)
        .filter(|t| *t > 0.0)
        .collect();

    if exposures.len() < config.min_bracket_frames {
        return false;
    }

    // ISO must be constant, when known.
    let isos: HashSet<u32> = idxs.iter().filter_map(|&i| frames[i].iso).collect();
    if isos.len() > 1 {
        return false;
    }

    let mut sorted = exposures.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // All the same exposure is a burst, not a bracket. This is the case that matters:
    // a plain burst of five frames at 1/250 must not be mistaken for a bracket and
    // thereby protected from culling.
    if let (Some(first), Some(last)) = (sorted.first(), sorted.last()) {
        if (last - first).abs() < f64::EPSILON {
            return false;
        }
    }

    let mut steps: Vec<f64> = Vec::with_capacity(sorted.len() - 1);
    for w in sorted.windows(2) {
        if w[0] <= 0.0 {
            return false;
        }
        let ev = (w[1] / w[0]).log2();
        if ev <= 0.0 {
            return false;
        }
        // Each step must land near a whole or third of a stop.
        let nearest = (ev * 3.0).round() / 3.0;
        if (ev - nearest).abs() > config.ev_tolerance {
            return false;
        }
        steps.push(ev);
    }

    // And every step must be the same size.
    if steps.is_empty() {
        return false;
    }
    let mean = steps.iter().sum::<f64>() / steps.len() as f64;
    if mean < 1.0 / 3.0 - config.ev_tolerance {
        return false; // a drift smaller than a third of a stop is not a deliberate bracket
    }
    steps.iter().all(|s| (s - mean).abs() <= config.ev_tolerance)
}

// ---------------------------------------------------------------------------
// Keeper selection
// ---------------------------------------------------------------------------
/// Choose keepers for every burst.
///
/// `scores` maps photo id to composite score. A frame with no score is treated as
/// unranked and is never chosen as a keeper in preference to a scored one — but it is
/// also never marked redundant on the strength of a missing number.
pub fn select_all(
    bursts: &[Burst],
    frames: &[BurstFrame],
    scores: &HashMap<i64, f64>,
    config: &BurstConfig,
    manual_keepers: &HashSet<i64>,
) -> Vec<BurstSelection> {
    bursts.iter().map(|b| select_keepers(b, frames, scores, config, manual_keepers)).collect()
}

/// Choose keepers for one burst.
pub fn select_keepers(
    burst: &Burst,
    frames: &[BurstFrame],
    scores: &HashMap<i64, f64>,
    config: &BurstConfig,
    manual_keepers: &HashSet<i64>,
) -> BurstSelection {
    let ids: Vec<i64> = burst.frames.iter().map(|&i| frames[i].photo_id).collect();

    // A bracket is never culled. Every frame was deliberate, and keeping one of a
    // three-stop bracket deletes two thirds of an intentional set.
    if burst.kind == BurstKind::Bracket {
        return BurstSelection {
            burst_id: burst.id,
            kind: burst.kind,
            keepers: ids,
            redundant: Vec::new(),
            had_manual_override: false,
        };
    }

    // Rank by score, descending. Frames without a score sort last but keep a stable
    // order, so the result does not depend on map iteration.
    let mut ranked: Vec<(i64, Option<f64>)> =
        ids.iter().map(|&id| (id, scores.get(&id).copied())).collect();
    ranked.sort_by(|a, b| {
        let av = a.1.unwrap_or(f64::NEG_INFINITY);
        let bv = b.1.unwrap_or(f64::NEG_INFINITY);
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0))
    });

    // Manual promotions first, then the best of the rest, up to the limit. A promoted
    // frame is never demoted by a later re-score, so it is inserted rather than merged.
    let mut keepers: Vec<i64> = Vec::new();
    let mut had_manual_override = false;
    for &id in &ids {
        if manual_keepers.contains(&id) {
            keepers.push(id);
            had_manual_override = true;
        }
    }
    for (id, _) in &ranked {
        if keepers.len() >= config.keepers_per_burst.max(keepers.len()) {
            break;
        }
        if !keepers.contains(id) {
            keepers.push(*id);
        }
    }

    // Order keepers by capture order for a stable, readable result.
    keepers.sort_by_key(|id| ids.iter().position(|x| x == id).unwrap_or(usize::MAX));

    let redundant: Vec<i64> = ids.iter().copied().filter(|id| !keepers.contains(id)).collect();

    BurstSelection { burst_id: burst.id, kind: burst.kind, keepers, redundant, had_manual_override }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: i64, time: i64, hash: u64) -> BurstFrame {
        BurstFrame::new(id)
            .with_camera(Some("X".into()))
            .with_time(Some(time))
            .with_hash(Some(hash))
    }

    fn scored(pairs: &[(i64, f64)]) -> HashMap<i64, f64> {
        pairs.iter().copied().collect()
    }

    // ---------------------------------------------------------------------
    // Perceptual hash
    // ---------------------------------------------------------------------
    #[test]
    fn an_identical_image_has_zero_distance() {
        let img = Luma::new(64, 64, (0..64 * 64).map(|i| (i % 251) as f32).collect());
        assert_eq!(hash_distance(perceptual_hash(&img), perceptual_hash(&img)), 0);
    }

    /// Smooth, photographic content with a wide tonal range.
    ///
    /// Not a pixel-level sawtooth. An earlier version of these tests used
    /// `(i * 7) % 200`, which oscillates every few pixels; area-averaging a 160-pixel row
    /// into 9 cells aliases it badly, and clipping then changes which samples survive.
    /// The test failed and the hash was fine — the pattern was not a photograph.
    fn textured(w: usize, h: usize) -> Vec<f32> {
        (0..w * h)
            .map(|i| {
                let x = (i % w) as f32;
                let y = (i / w) as f32;
                (60.0 + 120.0 * ((x / 17.0).sin() * (y / 13.0).cos()).abs()).clamp(0.0, 255.0)
            })
            .collect()
    }

    #[test]
    fn a_brightness_scale_leaves_the_hash_alone() {
        // The property that matters most here: a bracket varies exposure by one or two
        // stops, so a hash that moved with brightness would split every bracket into
        // separate groups and the bracket protection would never engage.
        let base = textured(160, 120);
        let a = Luma::new(160, 120, base.clone());

        for scale in [0.25f32, 0.5, 0.75, 1.25, 1.5] {
            let scaled: Vec<f32> = base.iter().map(|v| (v * scale).min(255.0)).collect();
            let d = hash_distance(perceptual_hash(&a), perceptual_hash(&Luma::new(160, 120, scaled)));
            assert!(d <= 8, "x{scale} moved the hash by {d} bits, which would split a bracket");
        }
    }

    #[test]
    fn heavy_clipping_barely_moves_the_hash() {
        // The adversarial end of a bracket: two stops over, with most of the frame blown.
        // Adjacent-pixel comparison on an area-averaged 9x8 grid survives it, because
        // clipping changes the absolute values without reordering the cell averages.
        let base = textured(160, 120);
        let a = Luma::new(160, 120, base.clone());
        let blown: Vec<f32> = base.iter().map(|v| (v * 3.0).min(255.0)).collect();
        let clipped_fraction =
            blown.iter().filter(|v| **v >= 255.0).count() as f64 / blown.len() as f64;
        assert!(clipped_fraction > 0.5, "this test needs a genuinely blown frame");

        let d = hash_distance(perceptual_hash(&a), perceptual_hash(&Luma::new(160, 120, blown)));
        assert!(
            d <= 8,
            "a two-stop-over frame moved the hash by {d} bits despite {:.0}% clipping, \
             which would break bracket grouping",
            clipped_fraction * 100.0
        );
    }

    #[test]
    fn a_tiny_shift_leaves_the_hash_nearly_alone() {
        // Area averaging means a one-pixel shift moves the 9x8 grid very little.
        let (w, h) = (80, 60);
        let base: Vec<f32> = (0..w * h).map(|i| ((i * 13) % 220) as f32 + 15.0).collect();
        let a = Luma::new(w, h, base.clone());

        let mut shifted = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                shifted[y * w + x] = base[y * w + ((x + 1) % w)];
            }
        }
        let b = Luma::new(w, h, shifted);

        let d = hash_distance(perceptual_hash(&a), perceptual_hash(&b));
        assert!(d <= 8, "a one-pixel shift moved the hash by {d} bits");
    }

    #[test]
    fn different_content_produces_a_distant_hash() {
        let (w, h) = (64, 64);
        let a = Luma::new(w, h, (0..w * h).map(|i| ((i * 3) % 256) as f32).collect());
        // A very different structure: a hard vertical split.
        let b = Luma::new(w, h, (0..w * h).map(|i| if (i % w) < w / 2 { 10.0 } else { 240.0 }).collect());

        let d = hash_distance(perceptual_hash(&a), perceptual_hash(&b));
        assert!(d > 8, "unrelated content should be far apart, got {d}");
    }

    // ---------------------------------------------------------------------
    // Grouping
    // ---------------------------------------------------------------------
    #[test]
    fn consecutive_frames_of_one_moment_are_one_burst() {
        let frames: Vec<_> = (0..5).map(|i| frame(i, 1000 + i, 0xAAAA_AAAA_AAAA_AAAA)).collect();
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].size(), 5);
        assert_eq!(bursts[0].kind, BurstKind::Burst);
    }

    #[test]
    fn a_gap_starts_a_new_burst() {
        let mut frames: Vec<_> = (0..3).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        frames.extend((3..6).map(|i| frame(i, 1000 + 60 + i, 0xAAAA)));
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 2);
    }

    #[test]
    fn a_gap_exactly_at_the_threshold_does_not_split() {
        let frames = vec![frame(0, 1000, 0xAAAA), frame(1, 1002, 0xAAAA)];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 1, "the gap must EXCEED the threshold");
    }

    #[test]
    fn two_compositions_a_second_apart_are_two_bursts() {
        // The case a timestamp alone gets wrong: the photographer swung the camera and
        // fired again within the same second. The hash is what separates them.
        let a = 0x0000_0000_0000_0000u64;
        let b = 0xFFFF_FFFF_FFFF_FFFFu64;
        let frames = vec![frame(0, 1000, a), frame(1, 1001, b), frame(2, 1002, b)];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 2, "a different composition is a different burst");
        assert_eq!(bursts[0].size(), 1);
        assert_eq!(bursts[1].size(), 2);
    }

    #[test]
    fn two_cameras_shooting_together_are_two_bursts() {
        let frames = vec![
            frame(0, 1000, 0xAAAA).with_camera(Some("A".into())),
            frame(1, 1001, 0xAAAA).with_camera(Some("B".into())),
            frame(2, 1002, 0xAAAA).with_camera(Some("A".into())),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 2);
    }

    #[test]
    fn frames_without_a_capture_time_are_not_burst_grouped() {
        // A burst is a sequence in time. Grouping on hash alone would merge two
        // deliberate near-identical compositions taken minutes apart, which is a
        // duplicate problem rather than a burst problem.
        let frames = vec![
            BurstFrame::new(0).with_hash(Some(0xAAAA)),
            BurstFrame::new(1).with_hash(Some(0xAAAA)),
        ];
        assert!(group_into_bursts(&frames, &BurstConfig::default()).is_empty());
    }

    #[test]
    fn a_missing_hash_does_not_split_a_burst() {
        // Absence of evidence is not evidence of a different composition.
        let frames = vec![
            frame(0, 1000, 0xAAAA),
            BurstFrame::new(1).with_camera(Some("X".into())).with_time(Some(1001)),
            frame(2, 1002, 0xAAAA),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 1, "a frame with no hash must not break the sequence");
    }

    #[test]
    fn grouping_is_deterministic_and_order_independent() {
        let mk = |order: &[i64]| -> Vec<BurstFrame> {
            order.iter().map(|&i| frame(i, 1000 + i, 0xAAAA_0000_0000_0000)).collect()
        };
        let a = group_into_bursts(&mk(&[0, 1, 2, 3]), &BurstConfig::default());
        let b = group_into_bursts(&mk(&[3, 1, 0, 2]), &BurstConfig::default());
        let ids = |bs: &[Burst]| -> Vec<Vec<i64>> {
            bs.iter()
                .map(|x| x.frames.iter().map(|&i| i as i64).collect())
                .collect()
        };
        // Indices differ by construction, so compare the photo ids each burst covers.
        let ids_of = |bs: &[Burst], frames: &[BurstFrame]| -> Vec<Vec<i64>> {
            bs.iter()
                .map(|x| x.frames.iter().map(|&i| frames[i].photo_id).collect())
                .collect()
        };
        assert_eq!(ids_of(&a, &mk(&[0, 1, 2, 3])), ids_of(&b, &mk(&[3, 1, 0, 2])));
        let _ = ids(&a);
    }

    // ---------------------------------------------------------------------
    // Brackets
    // ---------------------------------------------------------------------
    fn bracket_frame(id: i64, time: i64, exposure: f64, iso: u32) -> BurstFrame {
        BurstFrame::new(id)
            .with_camera(Some("X".into()))
            .with_time(Some(time))
            .with_hash(Some(0xAAAA_AAAA_AAAA_AAAA))
            .with_exposure(Some(exposure), Some(iso))
    }

    #[test]
    fn a_whole_stop_exposure_sequence_is_a_bracket() {
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 500.0, 100),
            bracket_frame(1, 1001, 1.0 / 250.0, 100),
            bracket_frame(2, 1002, 1.0 / 125.0, 100),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].kind, BurstKind::Bracket);
    }

    #[test]
    fn a_third_stop_sequence_is_also_a_bracket() {
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 250.0, 100),
            bracket_frame(1, 1001, 1.0 / 200.0, 100),
            bracket_frame(2, 1002, 1.0 / 160.0, 100),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts[0].kind, BurstKind::Bracket);
    }

    #[test]
    fn a_plain_burst_is_not_mistaken_for_a_bracket() {
        // The case that matters most. Five frames at the same exposure are a burst and
        // must stay cullable; protecting them as a bracket would leave the user with
        // five near-identical frames and no help.
        let frames: Vec<_> =
            (0..5).map(|i| bracket_frame(i, 1000 + i, 1.0 / 250.0, 400)).collect();
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts[0].kind, BurstKind::Burst);
    }

    #[test]
    fn varying_iso_disqualifies_a_bracket() {
        // Auto-ISO varies with exposure. A group where both move together is a
        // photographer chasing the light, not bracketing it.
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 500.0, 100),
            bracket_frame(1, 1001, 1.0 / 250.0, 200),
            bracket_frame(2, 1002, 1.0 / 125.0, 400),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts[0].kind, BurstKind::Burst);
    }

    #[test]
    fn two_frames_are_not_enough_to_call_a_bracket() {
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 500.0, 100),
            bracket_frame(1, 1001, 1.0 / 250.0, 100),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts[0].kind, BurstKind::Burst, "two frames is a coincidence");
    }

    #[test]
    fn non_uniform_steps_are_not_a_bracket() {
        // 1/500 -> 1/300 -> 1/125 is steps of 0.74 and 1.26 EV. Each is close to some
        // multiple of a third of a stop, so a check that only validated each step
        // individually accepted this — and it is not a sequence anyone brackets with.
        // The camera steps by identical increments; a meter reacting to the scene does
        // not.
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 500.0, 100),
            bracket_frame(1, 1001, 1.0 / 300.0, 100),
            bracket_frame(2, 1002, 1.0 / 125.0, 100),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(
            bursts[0].kind,
            BurstKind::Burst,
            "steps of 0.74 and 1.26 EV are not a bracket sequence"
        );
    }

    #[test]
    fn a_tiny_exposure_drift_is_not_a_bracket() {
        // The meter nudging exposure by a sixth of a stop is not a deliberate bracket.
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 250.0, 400),
            bracket_frame(1, 1001, 1.0 / 240.0, 400),
            bracket_frame(2, 1002, 1.0 / 230.0, 400),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts[0].kind, BurstKind::Burst);
    }

    #[test]
    fn a_five_frame_one_stop_bracket_is_detected() {
        // The common shape: -2, -1, 0, +1, +2 EV, uniform one-stop steps.
        let frames: Vec<_> = (0..5)
            .map(|i| {
                let stops = i as f64 - 2.0;
                bracket_frame(i, 1000 + i, (1.0 / 250.0) * 2f64.powf(stops), 100)
            })
            .collect();
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].kind, BurstKind::Bracket);
    }

    #[test]
    fn a_bracket_is_never_culled() {
        // The whole point. Keeping one frame of a three-stop bracket deletes two thirds
        // of an intentional set.
        let frames = vec![
            bracket_frame(0, 1000, 1.0 / 500.0, 100),
            bracket_frame(1, 1001, 1.0 / 250.0, 100),
            bracket_frame(2, 1002, 1.0 / 125.0, 100),
        ];
        let bursts = group_into_bursts(&frames, &BurstConfig::default());
        let scores = scored(&[(0, 90.0), (1, 50.0), (2, 10.0)]);
        let sel = select_keepers(&bursts[0], &frames, &scores, &BurstConfig::default(), &HashSet::new());

        assert_eq!(sel.keepers.len(), 3, "every bracket frame is a keeper");
        assert!(sel.redundant.is_empty(), "nothing in a bracket is redundant");
    }

    // ---------------------------------------------------------------------
    // Keeper selection
    // ---------------------------------------------------------------------
    fn burst_of(frames: &[BurstFrame]) -> Burst {
        group_into_bursts(frames, &BurstConfig::default()).remove(0)
    }

    #[test]
    fn the_best_two_frames_are_kept_by_default() {
        let frames: Vec<_> = (0..5).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 30.0), (2, 90.0), (3, 70.0), (4, 20.0)]);

        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        assert_eq!(sel.keepers.len(), 2, "the safety default");
        assert!(sel.keepers.contains(&2) && sel.keepers.contains(&3), "got {:?}", sel.keepers);
        assert_eq!(sel.redundant.len(), 3);
    }

    #[test]
    fn keepers_are_ordered_by_capture_not_by_score() {
        let frames: Vec<_> = (0..4).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 99.0), (2, 98.0), (3, 1.0)]);
        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        assert_eq!(sel.keepers, vec![1, 2], "in capture order, so the result is readable");
    }

    #[test]
    fn a_single_keeper_can_be_configured() {
        let frames: Vec<_> = (0..4).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 99.0), (2, 98.0), (3, 1.0)]);
        let config = BurstConfig { keepers_per_burst: 1, ..Default::default() };

        let sel = select_keepers(&b, &frames, &scores, &config, &HashSet::new());
        assert_eq!(sel.keepers, vec![1], "the single best frame");
        assert_eq!(sel.redundant.len(), 3);
    }

    #[test]
    fn a_manual_promotion_survives_a_re_score() {
        // The PRD's requirement: a promoted frame is never auto-demoted. Even when its
        // score is the worst in the burst.
        let frames: Vec<_> = (0..5).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 30.0), (2, 90.0), (3, 70.0), (4, 20.0)]);

        let manual: HashSet<i64> = [0].into_iter().collect();
        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &manual);

        assert!(sel.had_manual_override);
        assert!(sel.keepers.contains(&0), "the promoted frame must survive: {:?}", sel.keepers);
        assert!(sel.keepers.contains(&2), "and the best frame still gets a place");
        assert_eq!(sel.keepers.len(), 2, "the limit still applies");
    }

    #[test]
    fn more_promotions_than_the_limit_keeps_them_all() {
        // A manual choice is not subject to the automatic limit. Silently dropping one
        // would demote a frame the user explicitly chose.
        let frames: Vec<_> = (0..5).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 30.0), (2, 90.0), (3, 70.0), (4, 20.0)]);

        let manual: HashSet<i64> = [0, 1, 4].into_iter().collect();
        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &manual);

        assert_eq!(sel.keepers.len(), 3);
        assert!(sel.keepers.contains(&0) && sel.keepers.contains(&1) && sel.keepers.contains(&4));
    }

    #[test]
    fn an_unscored_frame_is_never_chosen_over_a_scored_one() {
        let frames: Vec<_> = (0..3).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        // Only frame 1 has a score.
        let scores = scored(&[(1, 5.0)]);
        let config = BurstConfig { keepers_per_burst: 1, ..Default::default() };

        let sel = select_keepers(&b, &frames, &scores, &config, &HashSet::new());
        assert_eq!(sel.keepers, vec![1], "a known score beats an unknown one");
    }

    #[test]
    fn a_group_of_one_is_not_redundant() {
        let frames = vec![frame(0, 1000, 0xAAAA)];
        let b = burst_of(&frames);
        assert!(!b.is_redundant_group());

        let scores = scored(&[(0, 50.0)]);
        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        assert_eq!(sel.keepers, vec![0]);
        assert!(sel.redundant.is_empty());
    }

    #[test]
    fn keepers_and_redundant_partition_the_burst_exactly() {
        // Every frame is in exactly one list. A frame in neither would be invisible to
        // the UI; a frame in both would be contradictory.
        let frames: Vec<_> = (0..6).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 30.0), (2, 90.0), (3, 70.0), (4, 20.0), (5, 5.0)]);

        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        let mut all: Vec<i64> = sel.keepers.iter().chain(sel.redundant.iter()).copied().collect();
        all.sort_unstable();
        let expected: Vec<i64> = (0..6).collect();
        assert_eq!(all, expected);
    }

    #[test]
    fn selection_never_removes_anything() {
        // A structural property, asserted so a future refactor has to argue with it:
        // the redundant list is a label. Nothing in this module deletes, moves or opens
        // a file.
        let frames: Vec<_> = (0..4).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 99.0), (2, 98.0), (3, 1.0)]);
        let sel = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());

        assert_eq!(sel.keepers.len() + sel.redundant.len(), frames.len());
        assert!(!sel.redundant.is_empty());
    }

    #[test]
    fn selection_is_deterministic() {
        let frames: Vec<_> = (0..5).map(|i| frame(i, 1000 + i, 0xAAAA)).collect();
        let b = burst_of(&frames);
        let scores = scored(&[(0, 10.0), (1, 30.0), (2, 90.0), (3, 70.0), (4, 20.0)]);
        let a = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        let c = select_keepers(&b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        assert_eq!(a, c);
    }

    #[test]
    fn config_validation_rejects_nonsense() {
        assert!(BurstConfig::default().is_valid());
        assert!(!BurstConfig { max_gap_seconds: 0, ..Default::default() }.is_valid());
        assert!(!BurstConfig { keepers_per_burst: 0, ..Default::default() }.is_valid());
        assert!(!BurstConfig { max_hash_distance: 65, ..Default::default() }.is_valid());
        assert!(!BurstConfig { min_bracket_frames: 1, ..Default::default() }.is_valid());
    }
}

#[cfg(test)]
mod diagnostics {
    use super::*;

    /// Measure how far a brightness scale moves the hash.
    ///
    /// Ignored by default: run with
    /// `cargo test -p chaff-core --lib -- --ignored --nocapture exposure_hash`
    #[test]
    #[ignore]
    fn exposure_hash_report() {
        let (w, h) = (160, 120);
        // Textured content with a wide tonal range, like a real frame.
        let base: Vec<f32> = (0..w * h)
            .map(|i| {
                let x = (i % w) as f32;
                let y = (i / w) as f32;
                (60.0 + 120.0 * ((x / 17.0).sin() * (y / 13.0).cos()).abs()).clamp(0.0, 255.0)
            })
            .collect();
        let a = Luma::new(w, h, base.clone());
        let ha = perceptual_hash(&a);
        println!("\n{:<14} {:>8} {:>8} {:>10}", "scale", "clip_hi", "dist", "note");
        println!("{}", "-".repeat(46));
        for scale in [0.25f32, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0] {
            let scaled: Vec<f32> = base.iter().map(|v| (v * scale).min(255.0)).collect();
            let clipped = scaled.iter().filter(|v| **v >= 255.0).count() as f64 / scaled.len() as f64;
            let d = hash_distance(ha, perceptual_hash(&Luma::new(w, h, scaled)));
            println!(
                "{:<14} {:>8.3} {:>8} {:>10}",
                format!("x{scale}"),
                clipped,
                d,
                if d <= 8 { "groups" } else { "SPLITS" }
            );
        }
        println!();
    }
}
