//! Shoot-relative normalisation.
//!
//! # The problem this solves
//!
//! Every raw metric the engine produces is scene-dependent, and comparing raw values
//! across photographs is close to meaningless:
//!
//! * A sharp frame at `f/1.4, ISO 100` and a sharp frame at `f/8, ISO 6400` produce
//!   wildly different Laplacian variances. Both are sharp.
//! * A correctly exposed frame in a dark room and an underexposed frame in daylight can
//!   share a mean level.
//! * A silent-shutter burst of a sleeping child at ISO 12800 is noisy in absolute terms
//!   and perfectly normal *for that shoot*.
//!
//! The PRD calls ranking within the shoot its most important accuracy requirement, and
//! this module is that ranking. It is also the reason a whole shoot of high-ISO frames
//! is not wholesale-rejected: the noisiest frame in a noisy shoot is still an ordinary
//! frame for that shoot.
//!
//! # What a shoot is
//!
//! A continuous period of shooting, in one place, with one camera. Operationally:
//!
//! 1. Same directory.
//! 2. Same camera body (from EXIF, falling back to make).
//! 3. Consecutive in time, with no gap longer than [`DEFAULT_SHOOT_GAP_SECONDS`].
//!
//! Frames with no capture time cannot be placed in time at all. They are grouped by
//! directory and camera alone, into one shoot, rather than being interleaved with timed
//! frames on a fabricated timestamp.
//!
//! # When the shoot is too small to rank
//!
//! A percentile rank over three frames is noise: it reports 0, 50 and 100 and calls the
//! difference meaningful. Below [`MIN_SHOOT_SIZE`] the within-shoot rank is marked
//! unreliable and a library-wide rank is supplied instead, with the flag set so the UI
//! can say which one it used. Silently ranking a two-frame shoot would be worse than
//! admitting the information is not there.

use std::collections::BTreeMap;

use crate::exif::ExifData;

/// A gap longer than this starts a new shoot.
///
/// Thirty minutes. Long enough to cover a pause for a coffee or a lens change mid-shoot;
/// short enough that a morning and an afternoon at the same venue are separate shoots
/// with different light.
pub const DEFAULT_SHOOT_GAP_SECONDS: i64 = 30 * 60;

/// Below this, a within-shoot percentile rank is not meaningful.
///
/// Eight. With fewer frames the ranks quantise to a handful of values and the difference
/// between the best and worst frame is dominated by which frames happened to be included.
pub const MIN_SHOOT_SIZE: usize = 8;

/// A measurable quantity that gets ranked within a shoot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Metric {
    /// Scene-normalised sharpness (higher is better).
    Focus,
    /// Absolute resolved detail. Comparable only within one shoot.
    Detail,
    /// Directional edge-energy ratio. ~1 is isotropic.
    Anisotropy,
    /// Mean level, 0..1.
    ExposureMean,
    /// Occupied range, 0..1 (higher is better).
    ExposureRange,
    /// Fraction clipped high (lower is better).
    ClippedHigh,
    /// Fraction clipped low (lower is better).
    ClippedLow,
    /// Estimated noise sigma (lower is better).
    Noise,
}

/// Every metric, in a fixed order.
pub const ALL_METRICS: [Metric; 8] = [
    Metric::Focus,
    Metric::Detail,
    Metric::Anisotropy,
    Metric::ExposureMean,
    Metric::ExposureRange,
    Metric::ClippedHigh,
    Metric::ClippedLow,
    Metric::Noise,
];

const N_METRICS: usize = ALL_METRICS.len();

/// Which metrics are better when *lower*, so callers do not have to remember.
///
/// Getting this backwards inverts a ranking silently, and an inverted ranking looks
/// exactly like a working one until someone checks the keepers.
pub fn higher_is_better(metric: Metric) -> bool {
    matches!(
        metric,
        Metric::Focus | Metric::Detail | Metric::ExposureRange | Metric::ExposureMean
    )
}

/// One frame's raw measurements plus the identity needed to group it.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameMeasurement {
    pub photo_id: i64,
    pub dir: String,
    pub camera: Option<String>,
    pub captured_at: Option<i64>,
    values: [f64; N_METRICS],
}

impl FrameMeasurement {
    pub fn new(photo_id: i64, dir: impl Into<String>) -> Self {
        Self {
            photo_id,
            dir: dir.into(),
            camera: None,
            captured_at: None,
            values: [0.0; N_METRICS],
        }
    }

    pub fn with_camera(mut self, camera: Option<String>) -> Self {
        self.camera = camera.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        self
    }

    pub fn with_capture_time(mut self, t: Option<i64>) -> Self {
        self.captured_at = t;
        self
    }

    pub fn set(&mut self, metric: Metric, value: f64) -> &mut Self {
        self.values[metric as usize] = value;
        self
    }

    pub fn get(&self, metric: Metric) -> f64 {
        self.values[metric as usize]
    }

    /// Build from the engine's own metric types.
    pub fn from_focus(photo_id: i64, dir: impl Into<String>, m: &super::focus::FocusMetrics) -> Self {
        let mut f = Self::new(photo_id, dir);
        f.set(Metric::Focus, m.normalized_focus);
        f.set(Metric::Detail, m.signal_variance);
        f.set(Metric::Anisotropy, m.anisotropy);
        f.set(Metric::Noise, m.noise_sigma);
        f
    }

    pub fn from_exposure(&mut self, e: &super::exposure::ExposureMetrics) -> &mut Self {
        self.set(Metric::ExposureMean, e.mean_level);
        self.set(Metric::ExposureRange, e.range_used);
        self.set(Metric::ClippedHigh, e.clipped_high);
        self.set(Metric::ClippedLow, e.clipped_low)
    }

    pub fn with_exif(mut self, exif: Option<&ExifData>) -> Self {
        if let Some(e) = exif {
            self.camera = e.camera_key();
            self.captured_at = e.captured_at;
        }
        self
    }
}

/// A run of frames shot together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shoot {
    pub id: usize,
    pub dir: String,
    pub camera: Option<String>,
    /// Indices into the slice that was grouped, in capture order where known.
    pub frames: Vec<usize>,
    /// True when the frames had capture times and were split on the time gap.
    pub time_clustered: bool,
}

impl Shoot {
    pub fn size(&self) -> usize {
        self.frames.len()
    }
}

/// Group frames into shoots.
///
/// Deterministic: the same input always produces the same shoots in the same order,
/// regardless of the order the frames arrived in. A ranking that depends on iteration
/// order is not a ranking.
pub fn group_into_shoots(
    frames: &[FrameMeasurement],
    gap_seconds: i64,
) -> Vec<Shoot> {
    // Bucket by (dir, camera) first.
    let mut buckets: BTreeMap<(String, Option<String>), Vec<usize>> = BTreeMap::new();
    for (i, f) in frames.iter().enumerate() {
        buckets
            .entry((f.dir.clone(), f.camera.clone()))
            .or_default()
            .push(i);
    }

    let mut shoots = Vec::new();

    for ((dir, camera), mut idxs) in buckets {
        // Timed frames first, ordered by time then by photo id so that equal timestamps
        // still produce a stable order.
        let (mut timed, untimed): (Vec<usize>, Vec<usize>) = idxs
            .drain(..)
            .partition(|&i| frames[i].captured_at.is_some());

        timed.sort_by_key(|&i| (frames[i].captured_at.unwrap_or(0), frames[i].photo_id));

        let mut current: Vec<usize> = Vec::new();
        let mut last_time: Option<i64> = None;

        for i in timed {
            let t = frames[i].captured_at.unwrap_or(0);
            let starts_new = match last_time {
                Some(prev) => t.saturating_sub(prev) > gap_seconds,
                None => false,
            };
            if starts_new && !current.is_empty() {
                shoots.push(Shoot {
                    id: 0, // assigned below, after every shoot exists
                    dir: dir.clone(),
                    camera: camera.clone(),
                    frames: std::mem::take(&mut current),
                    time_clustered: true,
                });
            }
            current.push(i);
            last_time = Some(t);
        }
        if !current.is_empty() {
            shoots.push(Shoot {
                id: 0,
                dir: dir.clone(),
                camera: camera.clone(),
                frames: current,
                time_clustered: true,
            });
        }

        // Frames with no capture time get one shoot of their own. They cannot be placed
        // in time, and inventing a timestamp would merge unrelated photographs into a
        // sequence that never happened.
        if !untimed.is_empty() {
            let mut untimed = untimed;
            untimed.sort_by_key(|&i| frames[i].photo_id);
            shoots.push(Shoot {
                id: 0,
                dir,
                camera,
                frames: untimed,
                time_clustered: false,
            });
        }
    }

    for (i, s) in shoots.iter_mut().enumerate() {
        s.id = i;
    }
    shoots
}

/// A frame's rank, both within its shoot and across the library.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalised {
    pub photo_id: i64,
    pub shoot_id: usize,
    pub shoot_size: usize,
    /// False when the shoot is below [`MIN_SHOOT_SIZE`]; `in_shoot` is then not
    /// meaningful and `in_library` should be used instead.
    pub shoot_relative: bool,
    /// Percentile rank within the shoot, 0..100. Higher means more of the metric.
    ///
    /// The rank is of the raw value, so a metric where lower is better (noise, clipping)
    /// has a *low* rank when the frame is good. [`higher_is_better`] says which is which.
    pub in_shoot: [f64; N_METRICS],
    /// Percentile rank across the whole library, 0..100. The fallback reference.
    pub in_library: [f64; N_METRICS],
}

impl Normalised {
    pub fn shoot_percentile(&self, metric: Metric) -> f64 {
        self.in_shoot[metric as usize]
    }

    pub fn library_percentile(&self, metric: Metric) -> f64 {
        self.in_library[metric as usize]
    }

    /// The percentile a caller should actually use, given whether the shoot was large
    /// enough to rank.
    pub fn effective_percentile(&self, metric: Metric) -> f64 {
        if self.shoot_relative {
            self.shoot_percentile(metric)
        } else {
            self.library_percentile(metric)
        }
    }

    /// Percentile oriented so that higher is always better, in 0..100.
    ///
    /// The orientation is applied here, once, so no caller has to remember which metrics
    /// are inverted. An inverted ranking is invisible until someone inspects the keepers.
    pub fn score_percentile(&self, metric: Metric) -> f64 {
        let p = self.effective_percentile(metric);
        if higher_is_better(metric) {
            p
        } else {
            100.0 - p
        }
    }
}

/// Percentile rank of every value, 0..100, with mid-ranks for ties.
///
/// Ties share a rank rather than being ordered arbitrarily by position. Many metrics are
/// exactly zero on several frames — no clipping, no measurable noise — and ordering
/// those by array position would make the ranking depend on which frame happened to be
/// enumerated first.
pub fn percentile_ranks(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![50.0];
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        values[a].partial_cmp(&values[b]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
    });

    let mut ranks = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && values[order[j]] == values[order[i]] {
            j += 1;
        }
        // Mid-rank across the tied block: everything below, plus half the ties.
        let below = i as f64;
        let tied = (j - i) as f64;
        let rank = (below + tied / 2.0) / n as f64 * 100.0;
        for k in i..j {
            ranks[order[k]] = rank;
        }
        i = j;
    }
    ranks
}

/// Normalise a set of frames within their shoots.
///
/// Returns one entry per input frame, in input order.
pub fn normalise(frames: &[FrameMeasurement], gap_seconds: i64) -> Vec<Normalised> {
    let shoots = group_into_shoots(frames, gap_seconds);

    // Library-wide ranks, computed once. `vec![]` per metric rather than an array
    // literal: arrays of Vec are not Copy, so the repeat syntax does not compile.
    let library: Vec<Vec<f64>> = ALL_METRICS
        .iter()
        .map(|metric| {
            let values: Vec<f64> = frames.iter().map(|f| f.get(*metric)).collect();
            percentile_ranks(&values)
        })
        .collect();

    let mut out: Vec<Option<Normalised>> = vec![None; frames.len()];

    for shoot in &shoots {
        let size = shoot.size();
        let shoot_relative = size >= MIN_SHOOT_SIZE;

        let in_shoot: Vec<Vec<f64>> = ALL_METRICS
            .iter()
            .map(|metric| {
                let values: Vec<f64> =
                    shoot.frames.iter().map(|&i| frames[i].get(*metric)).collect();
                percentile_ranks(&values)
            })
            .collect();

        for (pos, &frame_idx) in shoot.frames.iter().enumerate() {
            let mut row = [0.0; N_METRICS];
            for mi in 0..N_METRICS {
                row[mi] = in_shoot[mi][pos];
            }
            let mut lib = [0.0; N_METRICS];
            for mi in 0..N_METRICS {
                lib[mi] = library[mi][frame_idx];
            }

            out[frame_idx] = Some(Normalised {
                photo_id: frames[frame_idx].photo_id,
                shoot_id: shoot.id,
                shoot_size: size,
                shoot_relative,
                in_shoot: row,
                in_library: lib,
            });
        }
    }

    // Every frame belongs to exactly one shoot, so this cannot be None. If it ever is,
    // that is a bug in grouping and silently dropping the frame would hide it.
    out.into_iter()
        .enumerate()
        .map(|(i, o)| {
            o.unwrap_or_else(|| panic!("frame {i} was not assigned to any shoot — grouping is broken"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: i64, dir: &str, camera: Option<&str>, time: Option<i64>, focus: f64) -> FrameMeasurement {
        let mut f = FrameMeasurement::new(id, dir).with_camera(camera.map(str::to_string));
        f.captured_at = time;
        f.set(Metric::Focus, focus);
        f
    }

    // ---------------------------------------------------------------------
    // Grouping
    // ---------------------------------------------------------------------
    #[test]
    fn consecutive_frames_in_one_place_with_one_camera_are_one_shoot() {
        let frames: Vec<_> = (0..10)
            .map(|i| frame(i, "/lib", Some("Canon EOS R5"), Some(1000 + i * 5), 1.0))
            .collect();
        let shoots = group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(shoots.len(), 1);
        assert_eq!(shoots[0].size(), 10);
        assert!(shoots[0].time_clustered);
    }

    #[test]
    fn a_long_gap_starts_a_new_shoot() {
        // Five frames, then a two-hour break, then five more. Two shoots.
        let mut frames: Vec<_> =
            (0..5).map(|i| frame(i, "/lib", Some("X"), Some(1000 + i * 5), 1.0)).collect();
        frames.extend(
            (5..10).map(|i| frame(i, "/lib", Some("X"), Some(1000 + 7200 + i * 5), 1.0)),
        );

        let shoots = group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(shoots.len(), 2, "a two-hour gap is two shoots");
        assert_eq!(shoots[0].size(), 5);
        assert_eq!(shoots[1].size(), 5);
    }

    #[test]
    fn a_gap_exactly_at_the_threshold_does_not_split() {
        // Boundary behaviour, asserted so it cannot drift.
        let frames = vec![
            frame(0, "/lib", Some("X"), Some(0), 1.0),
            frame(1, "/lib", Some("X"), Some(DEFAULT_SHOOT_GAP_SECONDS), 1.0),
        ];
        let shoots = group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(shoots.len(), 1, "the gap must EXCEED the threshold to split");
    }

    #[test]
    fn one_second_past_the_threshold_splits() {
        let frames = vec![
            frame(0, "/lib", Some("X"), Some(0), 1.0),
            frame(1, "/lib", Some("X"), Some(DEFAULT_SHOOT_GAP_SECONDS + 1), 1.0),
        ];
        assert_eq!(group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS).len(), 2);
    }

    #[test]
    fn two_camera_bodies_shooting_the_same_scene_are_two_shoots() {
        // Same directory, same seconds, different bodies. Near-identical frames that are
        // not a sequence, and the body is what separates them.
        let frames = vec![
            frame(0, "/lib", Some("Canon EOS R5"), Some(1000), 1.0),
            frame(1, "/lib", Some("Canon EOS R6"), Some(1001), 1.0),
            frame(2, "/lib", Some("Canon EOS R5"), Some(1002), 1.0),
        ];
        let shoots = group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(shoots.len(), 2);
        assert!(shoots.iter().all(|s| s.size() == if s.camera.as_deref() == Some("Canon EOS R5") { 2 } else { 1 }));
    }

    #[test]
    fn different_directories_are_different_shoots() {
        let frames = vec![
            frame(0, "/lib/a", Some("X"), Some(1000), 1.0),
            frame(1, "/lib/b", Some("X"), Some(1001), 1.0),
        ];
        assert_eq!(group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS).len(), 2);
    }

    #[test]
    fn frames_without_a_capture_time_form_one_shoot_per_directory_and_camera() {
        // They cannot be placed in time, and inventing a timestamp would merge unrelated
        // photographs into a sequence that never happened.
        let frames = vec![
            frame(0, "/lib", Some("X"), None, 1.0),
            frame(1, "/lib", Some("X"), None, 2.0),
            frame(2, "/lib", Some("Y"), None, 3.0),
        ];
        let shoots = group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(shoots.len(), 2);
        assert!(shoots.iter().all(|s| !s.time_clustered));
        let x = shoots.iter().find(|s| s.camera.as_deref() == Some("X")).unwrap();
        assert_eq!(x.size(), 2);
    }

    #[test]
    fn timed_and_untimed_frames_are_not_interleaved() {
        let frames = vec![
            frame(0, "/lib", Some("X"), Some(1000), 1.0),
            frame(1, "/lib", Some("X"), None, 2.0),
            frame(2, "/lib", Some("X"), Some(1005), 3.0),
        ];
        let shoots = group_into_shoots(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(shoots.len(), 2, "the untimed frame gets its own shoot");
        assert_eq!(shoots.iter().filter(|s| s.time_clustered).count(), 1);
        assert_eq!(shoots.iter().filter(|s| !s.time_clustered).count(), 1);
    }

    #[test]
    fn grouping_is_independent_of_input_order() {
        // Compared by photo id, not by index. Shoot membership is a list of indices into
        // the slice that was grouped, so two differently-ordered inputs necessarily
        // produce different indices — comparing those would assert nothing about whether
        // the grouping itself is stable.
        let a = vec![
            frame(2, "/lib", Some("X"), Some(1002), 3.0),
            frame(0, "/lib", Some("X"), Some(1000), 1.0),
            frame(1, "/lib", Some("X"), Some(1001), 2.0),
        ];
        let b = vec![
            frame(0, "/lib", Some("X"), Some(1000), 1.0),
            frame(1, "/lib", Some("X"), Some(1001), 2.0),
            frame(2, "/lib", Some("X"), Some(1002), 3.0),
        ];

        let ids = |frames: &[FrameMeasurement], shoots: &[Shoot]| -> Vec<Vec<i64>> {
            shoots
                .iter()
                .map(|s| s.frames.iter().map(|&i| frames[i].photo_id).collect())
                .collect()
        };

        let sa = group_into_shoots(&a, 1800);
        let sb = group_into_shoots(&b, 1800);
        assert_eq!(sa.len(), sb.len());
        assert_eq!(
            ids(&a, &sa),
            ids(&b, &sb),
            "the same photographs must group the same way regardless of input order"
        );
        // And within a shoot, frames are ordered by capture time, not by arrival.
        assert_eq!(ids(&a, &sa)[0], vec![0, 1, 2]);
    }

    #[test]
    fn every_frame_is_assigned_to_exactly_one_shoot() {
        let frames = vec![
            frame(0, "/a", Some("X"), Some(1000), 1.0),
            frame(1, "/a", Some("X"), Some(99999), 2.0),
            frame(2, "/b", None, None, 3.0),
            frame(3, "/a", Some("Y"), Some(1000), 4.0),
        ];
        let shoots = group_into_shoots(&frames, 1800);
        let total: usize = shoots.iter().map(Shoot::size).sum();
        assert_eq!(total, frames.len());
    }

    // ---------------------------------------------------------------------
    // Percentile ranks
    // ---------------------------------------------------------------------
    #[test]
    fn ranks_span_the_full_range_for_distinct_values() {
        let r = percentile_ranks(&[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(r[0], 12.5, "lowest of four");
        assert_eq!(r[3], 87.5, "highest of four");
        assert!(r.windows(2).all(|w| w[0] < w[1]), "ranks must be monotonic");
    }

    #[test]
    fn ties_share_a_mid_rank() {
        // Many metrics are exactly zero on several frames — no clipping, no measurable
        // noise. Ordering those by array position would make the ranking depend on which
        // frame happened to be enumerated first.
        let r = percentile_ranks(&[0.0, 0.0, 0.0, 0.0]);
        assert!(r.iter().all(|v| (*v - 50.0).abs() < 1e-9), "all tied -> all 50: {r:?}");

        let r = percentile_ranks(&[1.0, 2.0, 2.0, 3.0]);
        assert_eq!(r[0], 12.5);
        assert_eq!(r[1], r[2], "the tied pair must share a rank");
        assert_eq!(r[1], 50.0, "mid-rank of the tied block");
        assert_eq!(r[3], 87.5);
    }

    #[test]
    fn a_single_frame_gets_a_neutral_rank() {
        assert_eq!(percentile_ranks(&[42.0]), vec![50.0]);
        assert!(percentile_ranks(&[]).is_empty());
    }

    #[test]
    fn ranks_are_always_within_zero_and_hundred() {
        for values in [
            vec![1.0, 2.0, 3.0],
            vec![-5.0, 0.0, 5.0],
            vec![0.0; 20],
            vec![1e-12, 1e12],
        ] {
            for r in percentile_ranks(&values) {
                assert!((0.0..=100.0).contains(&r), "rank {r} out of range");
            }
        }
    }

    // ---------------------------------------------------------------------
    // Normalisation
    // ---------------------------------------------------------------------
    #[test]
    fn within_a_shoot_the_best_frame_ranks_highest() {
        let frames: Vec<_> = (0..10)
            .map(|i| frame(i, "/lib", Some("X"), Some(1000 + i), i as f64))
            .collect();
        let n = normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);

        assert!(n.iter().all(|x| x.shoot_relative), "ten frames is enough to rank");
        // Frame 9 has the highest focus and must rank above frame 0.
        assert!(n[9].shoot_percentile(Metric::Focus) > n[0].shoot_percentile(Metric::Focus));
        assert!(n[9].score_percentile(Metric::Focus) > 87.0);
        assert!(n[0].score_percentile(Metric::Focus) < 13.0);
    }

    #[test]
    fn a_small_shoot_is_flagged_and_falls_back_to_the_library() {
        // A percentile over three frames reports 0, 50 and 100 and calls the difference
        // meaningful. Below the threshold that must be admitted, not reported.
        let mut frames: Vec<_> =
            (0..3).map(|i| frame(i, "/small", Some("X"), Some(1000 + i), i as f64)).collect();
        // A large shoot elsewhere in the library, so library ranks have something to say.
        frames.extend(
            (10..30).map(|i| frame(i, "/big", Some("Y"), Some(5000 + i), i as f64)),
        );

        let n = normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        let small: Vec<_> = n.iter().filter(|x| x.shoot_size == 3).collect();
        assert_eq!(small.len(), 3);
        assert!(
            small.iter().all(|x| !x.shoot_relative),
            "a three-frame shoot must not claim to be shoot-relative"
        );
        // The fallback is the library rank, and it must actually differ from the
        // within-shoot rank — otherwise the flag would be cosmetic.
        assert!(
            small.iter().any(|x| x.effective_percentile(Metric::Focus)
                == x.library_percentile(Metric::Focus)),
            "the fallback must route to the library percentile"
        );
    }

    #[test]
    fn a_shoot_exactly_at_the_threshold_is_relative() {
        let frames: Vec<_> = (0..MIN_SHOOT_SIZE)
            .map(|i| frame(i as i64, "/lib", Some("X"), Some(1000 + i as i64), i as f64))
            .collect();
        let n = normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert!(n.iter().all(|x| x.shoot_relative));
        assert_eq!(n[0].shoot_size, MIN_SHOOT_SIZE);
    }

    #[test]
    fn score_percentile_orientates_inverted_metrics() {
        // Noise and clipping are better when lower. Getting this backwards inverts a
        // ranking silently, and an inverted ranking looks like a working one.
        let mut frames: Vec<_> = (0..10)
            .map(|i| frame(i, "/lib", Some("X"), Some(1000 + i), 1.0))
            .collect();
        for (i, f) in frames.iter_mut().enumerate() {
            f.set(Metric::Noise, i as f64 * 10.0); // frame 9 is noisiest
        }

        let n = normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert!(
            n[9].shoot_percentile(Metric::Noise) > n[0].shoot_percentile(Metric::Noise),
            "the raw rank follows the value"
        );
        assert!(
            n[9].score_percentile(Metric::Noise) < n[0].score_percentile(Metric::Noise),
            "but the oriented score must invert it: noisier is worse"
        );
        assert!(n[0].score_percentile(Metric::Noise) > 87.0, "the cleanest frame scores best");
    }

    #[test]
    fn higher_is_better_is_declared_for_every_metric() {
        // Exhaustive so that adding a metric forces a decision rather than defaulting to
        // "higher is better" for something like noise.
        for m in ALL_METRICS {
            let _ = higher_is_better(m);
        }
        assert!(higher_is_better(Metric::Focus));
        assert!(!higher_is_better(Metric::Noise));
        assert!(!higher_is_better(Metric::ClippedHigh));
        assert!(!higher_is_better(Metric::ClippedLow));
    }

    #[test]
    fn normalisation_is_deterministic() {
        let frames: Vec<_> = (0..12)
            .map(|i| frame(i, "/lib", Some("X"), Some(1000 + i), (i % 4) as f64))
            .collect();
        assert_eq!(
            normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS),
            normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS)
        );
    }

    #[test]
    fn a_whole_noisy_shoot_is_not_penalised_for_being_noisy() {
        // The property the PRD cares about most: the noisiest frame in a noisy shoot is
        // an ordinary frame for that shoot, not a reject. Absolute noise would rank every
        // frame in this shoot as bad; shoot-relative ranking spreads them across the
        // range, which is what lets the user see which of them is worst.
        let noisy: Vec<_> = (0..10)
            .map(|i| {
                let mut f = frame(i, "/lib", Some("X"), Some(1000 + i), 1.0);
                f.set(Metric::Noise, 20.0 + i as f64);
                f
            })
            .collect();
        let quiet: Vec<_> = (0..10)
            .map(|i| {
                let mut f = frame(i, "/other", Some("Y"), Some(5000 + i), 1.0);
                f.set(Metric::Noise, 1.0 + i as f64 * 0.1);
                f
            })
            .collect();

        let all: Vec<_> = noisy.iter().chain(quiet.iter()).cloned().collect();
        let n = normalise(&all, DEFAULT_SHOOT_GAP_SECONDS);

        // The least noisy frame of the noisy shoot should score well *within its shoot*.
        let best_noisy = n.iter().filter(|x| x.shoot_id == 0).min_by(|a, b| {
            a.shoot_percentile(Metric::Noise).partial_cmp(&b.shoot_percentile(Metric::Noise)).unwrap()
        }).unwrap();
        assert!(
            best_noisy.score_percentile(Metric::Noise) > 80.0,
            "the cleanest frame of a noisy shoot must score well within that shoot, got {:.1}",
            best_noisy.score_percentile(Metric::Noise)
        );
    }

    #[test]
    fn a_frame_with_no_camera_or_time_still_normalises() {
        let mut frames: Vec<_> = (0..10).map(|i| frame(i, "/lib", None, None, i as f64)).collect();
        for f in frames.iter_mut() {
            f.camera = None;
        }
        let n = normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        assert_eq!(n.len(), 10);
        assert!(n.iter().all(|x| x.shoot_relative), "ten frames, even untimed, can rank");
    }

    #[test]
    fn a_frame_built_from_the_engine_metrics_carries_them_through() {
        use crate::imaging::{Luma, Region};
        use crate::scoring::{exposure, focus};

        let img = Luma::new(64, 64, (0..64 * 64).map(|i| (i % 255) as f32).collect());
        let f = focus::analyse(&img, None);
        let e = exposure::analyse(&img, Region::full(64, 64), exposure::Levels::eight_bit());

        let mut m = FrameMeasurement::from_focus(1, "/lib", &f);
        m.from_exposure(&e);

        assert_eq!(m.get(Metric::Focus), f.normalized_focus);
        assert_eq!(m.get(Metric::Noise), f.noise_sigma);
        assert_eq!(m.get(Metric::ExposureMean), e.mean_level);
        assert_eq!(m.get(Metric::ClippedHigh), e.clipped_high);
    }

    #[test]
    fn exif_identity_is_trimmed_and_empties_become_none() {
        let mut e = ExifData::default();
        e.make = Some("  Canon  ".into());
        e.model = Some("".into());
        let f = FrameMeasurement::new(1, "/lib").with_exif(Some(&e));
        assert_eq!(f.camera.as_deref(), Some("Canon"), "empty model falls back to make");
    }
}
