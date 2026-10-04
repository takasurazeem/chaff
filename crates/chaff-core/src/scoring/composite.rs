//! Composite scoring and explainability.
//!
//! # From percentiles to a decision
//!
//! [`super::shoot`] produces percentile ranks; this turns them into one number, a band,
//! and a sentence a human can check.
//!
//! # The band is a flag, never a deletion
//!
//! `Reject` means "flagged as a reject". It does not mean removed, and nothing in this
//! module can remove anything. Removal is a separate, reversible, user-initiated
//! operation (ADR-0004). The separation is deliberate: it is what makes a tool that is
//! occasionally wrong safe to use, because being wrong costs a glance rather than a
//! photograph.
//!
//! # Explainability is a requirement, not a feature
//!
//! Every score carries the terms that produced it and a sentence for each. A culling
//! tool that says "62" and nothing else is a tool the user has to verify by hand, which
//! costs more time than culling manually would have. The explanation is the difference
//! between trusting the ranking and re-doing it.
//!
//! # Dimensions that are not measurable yet
//!
//! The PRD lists seven scoring dimensions. Three of them are not implemented, and they
//! are *absent* rather than weighted at zero — a zero-weight term would silently
//! renormalise the others and claim a complete score. [`unimplemented_dimensions`]
//! reports them so the UI can say what is missing.

use super::shoot::{Metric, Normalised};

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------
/// How much each measurable dimension contributes.
///
/// Weights need not sum to 100 — they are normalised before use — but every preset does,
/// so that a reader can compare them at a glance. A test enforces it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreWeights {
    pub focus: f64,
    pub detail: f64,
    pub exposure_range: f64,
    pub clipping: f64,
    pub noise: f64,
}

impl ScoreWeights {
    pub fn total(&self) -> f64 {
        self.focus + self.detail + self.exposure_range + self.clipping + self.noise
    }

    /// Weights scaled to sum to 1. Returns `None` when nothing is weighted, which is a
    /// configuration error rather than a score of zero.
    pub fn normalised(&self) -> Option<ScoreWeights> {
        let total = self.total();
        if total <= 0.0 || !total.is_finite() {
            return None;
        }
        Some(ScoreWeights {
            focus: self.focus / total,
            detail: self.detail / total,
            exposure_range: self.exposure_range / total,
            clipping: self.clipping / total,
            noise: self.noise / total,
        })
    }

    /// Reject nonsensical weights rather than producing a wrong score quietly.
    pub fn is_valid(&self) -> bool {
        let w = [
            self.focus,
            self.detail,
            self.exposure_range,
            self.clipping,
            self.noise,
        ];
        w.iter().all(|v| v.is_finite() && *v >= 0.0) && self.total() > 0.0
    }
}

/// A named weighting, chosen by the kind of photography being culled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub weights: ScoreWeights,
}

const fn w(focus: f64, detail: f64, exposure_range: f64, clipping: f64, noise: f64) -> ScoreWeights {
    ScoreWeights { focus, detail, exposure_range, clipping, noise }
}

/// The presets, in the order the UI should offer them.
///
/// The differences are not decorative. A wildlife shoot is mostly motion and reach, so
/// focus dominates and noise barely matters because the alternative to a noisy frame is
/// no frame. A landscape shoot is static, so noise matters little and tonal range matters
/// a great deal. A low-light event is the reverse on both counts.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Balanced",
        description: "No strong assumption about subject or conditions.",
        weights: w(30.0, 20.0, 15.0, 20.0, 15.0),
    },
    Preset {
        name: "Portrait",
        description: "Noise matters: portraits are often shot in poor light, and skin \
                      shows it. Tonal range matters less than a clean frame.",
        weights: w(30.0, 15.0, 10.0, 20.0, 25.0),
    },
    Preset {
        name: "Landscape",
        description: "Static subject, tripod, low ISO. Tonal range is the point; noise \
                      is almost never the limiting factor.",
        weights: w(30.0, 25.0, 25.0, 15.0, 5.0),
    },
    Preset {
        name: "Wildlife",
        description: "Motion and reach dominate. A noisy frame is worth keeping; a \
                      soft one is not.",
        weights: w(40.0, 25.0, 10.0, 10.0, 15.0),
    },
    Preset {
        name: "Event",
        description: "Low light, high ISO, moving people. Noise and clipping both bite.",
        weights: w(25.0, 15.0, 15.0, 20.0, 25.0),
    },
    Preset {
        name: "Street",
        description: "Composition and tonal range carry a frame that is often not \
                      technically perfect.",
        weights: w(25.0, 20.0, 25.0, 15.0, 15.0),
    },
];

pub fn preset_by_name(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

pub fn default_preset() -> &'static Preset {
    &PRESETS[0]
}

/// Dimensions the PRD asks for that this build cannot measure, with the reason.
///
/// Reported rather than silently weighted at zero. A zero weight would renormalise the
/// remaining terms and present a complete-looking score that is missing a third of what
/// the user was promised.
pub fn unimplemented_dimensions() -> &'static [(&'static str, &'static str)] {
    &[
        ("composition", "needs a subject/rule-of-thirds model; not implemented"),
        ("aesthetic", "needs a no-reference aesthetic model (NIMA/CLIP-IQA); not implemented"),
        ("expression", "needs face landmarks; arrives with Phase 2 (#42-#48)"),
        ("eyes_open", "needs face landmarks; arrives with Phase 2 (#42-#48)"),
    ]
}

// ---------------------------------------------------------------------------
// Terms and scores
// ---------------------------------------------------------------------------
/// Which dimension a term represents.
///
/// Distinct from [`Metric`] because `Clipping` is derived from *two* metrics — the worse
/// of high and low — and pretending it is one of them would misreport the raw value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TermKind {
    Focus,
    Detail,
    ExposureRange,
    Clipping,
    Noise,
}

impl TermKind {
    pub fn label(&self) -> &'static str {
        match self {
            TermKind::Focus => "focus",
            TermKind::Detail => "detail",
            TermKind::ExposureRange => "tonal range",
            TermKind::Clipping => "clipping",
            TermKind::Noise => "noise",
        }
    }
}

/// One dimension's contribution to a frame's score.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub kind: TermKind,
    /// Share of the composite, 0..1, after normalisation.
    pub weight: f64,
    /// Oriented percentile, 0..100. Higher is always better.
    pub percentile: f64,
    /// The underlying measurement, for the explanation and for the UI to display.
    pub raw: f64,
    /// Points this term added to the composite, 0..100.
    pub contribution: f64,
    /// Points this term cost, 0..100. The ranking for detractors uses this, not the
    /// contribution: a heavily weighted term at the 30th percentile costs more than a
    /// lightly weighted one at the 0th.
    pub loss: f64,
}

/// `1st`, `2nd`, `3rd`, `4th`, `11th`, `21st`, `93rd` — the suffix is not just the last
/// digit, and "21th" in a user-facing explanation reads as carelessness.
fn ordinal(value: f64) -> String {
    let v = value.round() as i64;
    let suffix = match (v.unsigned_abs() % 10, v.unsigned_abs() % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{v}{suffix}")
}

impl Term {
    fn new(kind: TermKind, weight: f64, percentile: f64, raw: f64) -> Self {
        let percentile = percentile.clamp(0.0, 100.0);
        Self {
            kind,
            weight,
            percentile,
            raw,
            contribution: weight * percentile,
            loss: weight * (100.0 - percentile),
        }
    }

    /// True when the raw measurement is at the best achievable value, whatever the
    /// percentile says.
    ///
    /// Half a shoot can be tied at "no clipping at all". Mid-ranks then place every one
    /// of them mid-pack — which is correct for *ranking*, because a dimension on which
    /// everyone is equally good does not separate anyone — but absurd for *explanation*,
    /// where it produced the line "− no clipping". Nothing is wrong with a frame that
    /// has no clipping, so it is never a complaint.
    pub fn is_at_ideal(&self) -> bool {
        match self.kind {
            TermKind::Clipping => self.raw <= 0.001,
            TermKind::Noise => self.raw <= 0.0,
            _ => false,
        }
    }

    /// A phrase describing this term, specific enough to be checkable.
    fn describe(&self) -> String {
        let grade = match self.percentile {
            p if p >= 90.0 => "excellent",
            p if p >= 70.0 => "good",
            p if p >= 40.0 => "average",
            p if p >= 15.0 => "weak",
            _ => "poor",
        };
        match self.kind {
            TermKind::Focus => format!("{} {grade}", self.kind.label()),
            TermKind::Detail => format!("{} {grade}", self.kind.label()),
            TermKind::ExposureRange => {
                format!("{} {grade} — {:.0}% of the range used", self.kind.label(), self.raw * 100.0)
            }
            TermKind::Clipping => {
                if self.raw <= 0.001 {
                    "no clipping".to_string()
                } else {
                    format!("{} {grade} — {:.1}% of pixels at an extreme", self.kind.label(), self.raw * 100.0)
                }
            }
            TermKind::Noise => {
                format!("{} {grade} (sigma {:.1})", self.kind.label(), self.raw)
            }
        }
    }
}

/// The three bands. A flag, never an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// Confidently good.
    Keep,
    /// Ambiguous — the human should look. The band exists because a binary decision
    /// forces a guess on exactly the frames where a guess is most expensive.
    Review,
    /// Flagged as a reject. Not deleted, not moved, not hidden without being asked.
    Reject,
}

impl Band {
    pub fn label(&self) -> &'static str {
        match self {
            Band::Keep => "Keep",
            Band::Review => "Review",
            Band::Reject => "Reject",
        }
    }
}

/// A term must beat this percentile to be presented as a strength.
///
/// The sign in an explanation has to mean something. Taking the top two terms by
/// contribution regardless of value produced lines like
/// "− tonal range good — 88% of the range used (75th percentile)": a good result
/// presented as a complaint, purely because it was fourth of five terms. Above average
/// is a plus, below average is a minus, and a frame with neither says so.
const STRENGTH_PERCENTILE: f64 = 50.0;

/// Where the band boundaries sit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandThresholds {
    /// At or above this composite, the frame is a Keep.
    pub keep: f64,
    /// Below this, a Reject. Between the two, Review.
    pub reject: f64,
}

impl Default for BandThresholds {
    /// **Uncalibrated defaults.** Treat these as a starting point, not a tuned setting.
    ///
    /// The composite is a weighted mean of five percentiles, so it concentrates near 50:
    /// five roughly independent uniform ranks average to something with a standard
    /// deviation near 13, not 29. At 70/35 that puts roughly 6% of a shoot in Keep and
    /// 12% in Reject — a reasonable starting shape, but the real distribution depends
    /// entirely on how correlated the dimensions are for a given photographer.
    ///
    /// Measured on the 50-photograph corpus treated as one shoot: 0 Keep, 41 Review,
    /// 9 Reject. Zero Keeps is within sampling noise of the expected 6%, but it is also
    /// exactly the kind of thing that needs calibrating against real culling decisions
    /// rather than against a corpus of unrelated stock photographs.
    ///
    /// The Review band is deliberately wide. The cost of putting a good frame in Review
    /// is one glance; the cost of putting it in Reject is that the user never sees it.
    fn default() -> Self {
        Self { keep: 70.0, reject: 35.0 }
    }
}

impl BandThresholds {
    pub fn is_valid(&self) -> bool {
        self.keep.is_finite()
            && self.reject.is_finite()
            && self.reject < self.keep
            && (0.0..=100.0).contains(&self.keep)
            && (0.0..=100.0).contains(&self.reject)
    }

    pub fn band_of(&self, composite: f64) -> Band {
        if composite >= self.keep {
            Band::Keep
        } else if composite < self.reject {
            Band::Reject
        } else {
            Band::Review
        }
    }
}

/// A frame's score, with everything needed to justify it.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub photo_id: i64,
    /// 0..100.
    pub composite: f64,
    pub band: Band,
    /// Fixed order, so the explanation is stable between runs.
    pub terms: Vec<Term>,
    pub shoot_relative: bool,
    pub shoot_size: usize,
    pub preset: &'static str,
}

impl Score {
    /// The terms that added the most, highest first.
    pub fn top_contributors(&self, n: usize) -> Vec<&Term> {
        let mut v: Vec<&Term> =
            self.terms.iter().filter(|t| t.percentile > STRENGTH_PERCENTILE).collect();
        v.sort_by(|a, b| {
            b.contribution.partial_cmp(&a.contribution).unwrap_or(std::cmp::Ordering::Equal)
        });
        v.truncate(n);
        v
    }

    /// The terms that cost the most, highest first.
    ///
    /// Ranked by loss rather than by lowest percentile: a heavily weighted dimension at
    /// the 30th percentile costs more than a lightly weighted one at the 0th, and the
    /// user wants to know what is actually holding the frame back.
    pub fn top_detractors(&self, n: usize) -> Vec<&Term> {
        let mut v: Vec<&Term> = self
            .terms
            .iter()
            .filter(|t| t.percentile < STRENGTH_PERCENTILE && !t.is_at_ideal())
            .collect();
        v.sort_by(|a, b| b.loss.partial_cmp(&a.loss).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(n);
        v
    }

    /// The terms that cost the most, excluding any whose kind is in `exclude`.
    pub fn top_detractors_excluding(&self, n: usize, exclude: &[TermKind]) -> Vec<&Term> {
        let mut v: Vec<&Term> = self
            .terms
            .iter()
            .filter(|t| {
                t.percentile < STRENGTH_PERCENTILE
                    && !t.is_at_ideal()
                    && !exclude.contains(&t.kind)
            })
            .collect();
        v.sort_by(|a, b| b.loss.partial_cmp(&a.loss).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(n);
        v
    }

    /// A sentence a human can check against the photograph.
    ///
    /// Names the reference for the percentile, because "40th percentile" means nothing
    /// without knowing whether it is within the shoot or across the library — and the
    /// module falls back to the library when a shoot is too small to rank.
    pub fn explain(&self) -> Vec<String> {
        let reference = if self.shoot_relative {
            format!("of {} in this shoot", self.shoot_size)
        } else {
            format!("across the library; this shoot has only {} frames", self.shoot_size)
        };

        let mut out = Vec::new();
        // The reference is named in the verdict, not only in the term lines. A frame with
        // nothing above or below average has no term lines, and the explanation would
        // otherwise never say whether the ranking was within the shoot or across the
        // library — which is the single most important caveat about the number.
        out.push(format!(
            "{} — {:.0}/100 ({}, ranked {})",
            self.band.label(),
            self.composite,
            self.preset,
            reference
        ));

        let contributors = self.top_contributors(2);
        for t in &contributors {
            out.push(format!(
                "  + {} ({} percentile {reference})",
                t.describe(),
                ordinal(t.percentile)
            ));
        }
        // Detractors exclude anything already shown as a contributor. With five terms
        // and two of each, an overlapping pair produced the same line twice — once with
        // a plus and once with a minus — which reads as a contradiction.
        let shown: Vec<TermKind> = contributors.iter().map(|t| t.kind).collect();
        for t in self.top_detractors_excluding(2, &shown) {
            out.push(format!(
                "  − {} ({} percentile {reference})",
                t.describe(),
                ordinal(t.percentile)
            ));
        }

        if contributors.is_empty() {
            out.push("  nothing above average for this shoot".to_string());
        }
        if shown.len() == contributors.len() && self.top_detractors(2).is_empty() {
            out.push("  nothing below average for this shoot".to_string());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------
/// Score one already-normalised frame.
pub fn score_one(
    n: &Normalised,
    weights: &ScoreWeights,
    thresholds: &BandThresholds,
    preset: &'static str,
) -> Score {
    let Some(w) = weights.normalised() else {
        // No usable weights is a configuration error. Returning a zero score with no
        // terms makes it visible rather than producing a confident number from nothing.
        return Score {
            photo_id: n.photo_id,
            composite: 0.0,
            band: Band::Review,
            terms: Vec::new(),
            shoot_relative: n.shoot_relative,
            shoot_size: n.shoot_size,
            preset,
        };
    };

    let mut terms = Vec::with_capacity(5);

    // Every term carries the RAW measurement, not its percentile. The percentile ranks
    // the term; the raw value is what the explanation quotes. Passing a percentile as a
    // raw value is how "2100% of the range used" reaches a user.
    if let Some(p) = n.score_percentile(Metric::Focus) {
        terms.push(Term::new(TermKind::Focus, w.focus, p, n.raw_value(Metric::Focus)));
    }
    if let Some(p) = n.score_percentile(Metric::Detail) {
        terms.push(Term::new(TermKind::Detail, w.detail, p, n.raw_value(Metric::Detail)));
    }
    if let Some(p) = n.score_percentile(Metric::ExposureRange) {
        terms.push(Term::new(
            TermKind::ExposureRange,
            w.exposure_range,
            p,
            n.raw_value(Metric::ExposureRange),
        ));
    }
    if let (Some(hi), Some(lo)) = (
        n.score_percentile(Metric::ClippedHigh),
        n.score_percentile(Metric::ClippedLow),
    ) {
        // The worse of the two governs: a frame with blown highlights OR crushed shadows
        // is limited by whichever it is, and averaging would let a clean end hide a
        // destroyed one.
        let percentile = hi.min(lo);
        let raw = n.raw_value(Metric::ClippedHigh).max(n.raw_value(Metric::ClippedLow));
        terms.push(Term::new(TermKind::Clipping, w.clipping, percentile, raw));
    }
    if let Some(p) = n.score_percentile(Metric::Noise) {
        terms.push(Term::new(TermKind::Noise, w.noise, p, n.raw_value(Metric::Noise)));
    }

    // Terms already carry normalised weights, so the composite is a weighted mean in
    // 0..100 with no further scaling.
    let composite = terms.iter().map(|t| t.contribution).sum::<f64>().clamp(0.0, 100.0);

    Score {
        photo_id: n.photo_id,
        composite,
        band: thresholds.band_of(composite),
        terms,
        shoot_relative: n.shoot_relative,
        shoot_size: n.shoot_size,
        preset,
    }
}

/// Score a whole normalised set with one preset.
pub fn score_all(
    normalised: &[Normalised],
    weights: &ScoreWeights,
    thresholds: &BandThresholds,
    preset: &'static str,
) -> Vec<Score> {
    normalised.iter().map(|n| score_one(n, weights, thresholds, preset)).collect()
}

/// Convenience: score with a named preset.
pub fn score_with_preset(
    normalised: &[Normalised],
    preset_name: &str,
    thresholds: &BandThresholds,
) -> Option<Vec<Score>> {
    let p = preset_by_name(preset_name)?;
    Some(score_all(normalised, &p.weights, thresholds, p.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoring::shoot::{
        self, direction, Direction, FrameMeasurement, DEFAULT_SHOOT_GAP_SECONDS,
    };

    /// A shoot of `n` frames where every metric improves with index, so the ordering of
    /// the resulting scores is known by construction.
    fn improving_shoot(n: usize) -> Vec<Normalised> {
        let frames: Vec<FrameMeasurement> = (0..n)
            .map(|i| {
                let mut f = FrameMeasurement::new(i as i64, "/lib")
                    .with_camera(Some("X".into()))
                    .with_capture_time(Some(1000 + i as i64));
                let t = i as f64 / (n - 1) as f64;
                f.set(Metric::Focus, t * 100.0);
                f.set(Metric::Detail, t * 1000.0);
                f.set(Metric::ExposureRange, t);
                f.set(Metric::ClippedHigh, 1.0 - t);
                f.set(Metric::ClippedLow, 1.0 - t);
                f.set(Metric::Noise, (1.0 - t) * 50.0);
                f
            })
            .collect();
        shoot::normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS)
    }

    // ---------------------------------------------------------------------
    // Weights
    // ---------------------------------------------------------------------
    #[test]
    fn every_preset_sums_to_one_hundred() {
        // Weights are normalised before use, so a preset that sums to 90 still works —
        // which is exactly why this needs a test. A typo would be invisible in the score
        // and visible only to someone comparing the presets by eye.
        for p in PRESETS {
            let total = p.weights.total();
            assert!(
                (total - 100.0).abs() < 1e-9,
                "preset {} sums to {total}, not 100",
                p.name
            );
            assert!(p.weights.is_valid(), "preset {} has invalid weights", p.name);
        }
    }

    #[test]
    fn preset_names_are_unique_and_lookup_works() {
        let mut names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate preset name");

        assert!(preset_by_name("portrait").is_some(), "lookup is case-insensitive");
        assert!(preset_by_name("PORTRAIT").is_some());
        assert!(preset_by_name("nope").is_none());
        assert_eq!(default_preset().name, "Balanced");
    }

    #[test]
    fn normalisation_makes_weights_share_one() {
        let w = ScoreWeights { focus: 2.0, detail: 1.0, exposure_range: 1.0, clipping: 0.0, noise: 0.0 };
        let n = w.normalised().unwrap();
        assert!((n.total() - 1.0).abs() < 1e-12);
        assert!((n.focus - 0.5).abs() < 1e-12);
    }

    #[test]
    fn all_zero_weights_are_refused_rather_than_scored() {
        let w = ScoreWeights { focus: 0.0, detail: 0.0, exposure_range: 0.0, clipping: 0.0, noise: 0.0 };
        assert!(!w.is_valid());
        assert!(w.normalised().is_none());
    }

    #[test]
    fn negative_or_nan_weights_are_refused() {
        let mut w = default_preset().weights;
        w.focus = -1.0;
        assert!(!w.is_valid());
        let mut w = default_preset().weights;
        w.noise = f64::NAN;
        assert!(!w.is_valid());
    }

    #[test]
    fn a_score_with_no_usable_weights_is_flagged_not_faked() {
        let n = &improving_shoot(10)[5];
        let empty = ScoreWeights { focus: 0.0, detail: 0.0, exposure_range: 0.0, clipping: 0.0, noise: 0.0 };
        let s = score_one(n, &empty, &BandThresholds::default(), "none");
        assert_eq!(s.composite, 0.0);
        assert!(s.terms.is_empty());
        assert_eq!(s.band, Band::Review, "an unscoreable frame must not be silently rejected");
    }

    // ---------------------------------------------------------------------
    // Composite behaviour
    // ---------------------------------------------------------------------
    #[test]
    fn scores_are_ordered_the_way_the_metrics_are() {
        let n = improving_shoot(12);
        let scores = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");

        for w in scores.windows(2) {
            assert!(
                w[1].composite > w[0].composite,
                "every metric improves with index, so scores must too: {} then {}",
                w[0].composite,
                w[1].composite
            );
        }
        assert!(scores[0].composite < 15.0, "the worst frame should score low");
        assert!(scores[11].composite > 85.0, "the best frame should score high");
    }

    #[test]
    fn the_composite_stays_within_zero_and_one_hundred() {
        for n in [2usize, 5, 8, 20] {
            for s in score_all(
                &improving_shoot(n),
                &default_preset().weights,
                &BandThresholds::default(),
                "Balanced",
            ) {
                assert!((0.0..=100.0).contains(&s.composite), "composite {}", s.composite);
            }
        }
    }

    #[test]
    fn a_perfect_frame_scores_near_one_hundred_and_a_terrible_one_near_zero() {
        // A spread, not a two-valued set. An earlier version made frames 0-8 identical
        // and expected frame 0 to score near zero; mid-ranks correctly tied them all at
        // 45, and the test was asserting that ties do not exist. Percentile ranking is
        // *relative*, so a floor only appears when there is something below it.
        let frames: Vec<FrameMeasurement> = (0..10)
            .map(|i| {
                let t = i as f64 / 9.0;
                let mut f = FrameMeasurement::new(i as i64, "/lib")
                    .with_camera(Some("X".into()))
                    .with_capture_time(Some(1000 + i as i64));
                f.set(Metric::Focus, 1.0 + t * 99.0);
                f.set(Metric::Detail, 1.0 + t * 999.0);
                f.set(Metric::ExposureRange, 0.01 + t * 0.99);
                f.set(Metric::ClippedHigh, 0.5 * (1.0 - t));
                f.set(Metric::ClippedLow, 0.5 * (1.0 - t));
                f.set(Metric::Noise, 50.0 * (1.0 - t) + 0.1);
                f
            })
            .collect();
        let n = shoot::normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        let s = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        assert!(s[9].composite > 90.0, "best frame scored {}", s[9].composite);
        assert!(s[0].composite < 10.0, "worst frame scored {}", s[0].composite);
    }

    #[test]
    fn identical_frames_share_a_score_rather_than_being_ordered_arbitrarily() {
        // The complement of the above, and the reason that test had to change: ten
        // identical frames have no ranking, and pretending otherwise would order them by
        // whichever happened to be enumerated first.
        let frames: Vec<FrameMeasurement> = (0..10)
            .map(|i| {
                let mut f = FrameMeasurement::new(i as i64, "/lib")
                    .with_camera(Some("X".into()))
                    .with_capture_time(Some(1000 + i as i64));
                f.set(Metric::Focus, 50.0);
                f.set(Metric::Detail, 500.0);
                f.set(Metric::ExposureRange, 0.5);
                f.set(Metric::ClippedHigh, 0.0);
                f.set(Metric::ClippedLow, 0.0);
                f.set(Metric::Noise, 5.0);
                f
            })
            .collect();
        let n = shoot::normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        let s = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        let first = s[0].composite;
        assert!(
            s.iter().all(|x| (x.composite - first).abs() < 1e-9),
            "identical frames must score identically: {:?}",
            s.iter().map(|x| x.composite).collect::<Vec<_>>()
        );
        assert!(
            (first - 50.0).abs() < 1.0,
            "and land mid-range, not at an extreme: {first}"
        );
    }

    #[test]
    fn the_worse_clipping_end_governs() {
        // A frame with blown highlights and clean shadows is limited by the highlights.
        // Averaging the two would let a clean end hide a destroyed one.
        let frames: Vec<FrameMeasurement> = (0..10)
            .map(|i| {
                let mut f = FrameMeasurement::new(i as i64, "/lib")
                    .with_camera(Some("X".into()))
                    .with_capture_time(Some(1000 + i as i64));
                f.set(Metric::Focus, 50.0);
                f.set(Metric::Detail, 500.0);
                f.set(Metric::ExposureRange, 0.5);
                // Frame 9 has the worst highlights of all but the cleanest shadows.
                f.set(Metric::ClippedHigh, if i == 9 { 1.0 } else { i as f64 / 20.0 });
                f.set(Metric::ClippedLow, if i == 9 { 0.0 } else { 0.5 });
                f.set(Metric::Noise, 5.0);
                f
            })
            .collect();
        let n = shoot::normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        let s = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");

        let clip = s[9].terms.iter().find(|t| t.kind == TermKind::Clipping).unwrap();
        assert!(
            clip.percentile < 20.0,
            "the worst end must govern, got percentile {:.1}",
            clip.percentile
        );
        assert!(clip.raw > 0.9, "and the reported raw value is the bad end's: {}", clip.raw);
    }

    #[test]
    fn scoring_is_deterministic() {
        let n = improving_shoot(12);
        let a = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        let b = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        assert_eq!(a, b);
    }

    #[test]
    fn presets_actually_produce_different_rankings() {
        // A preset that changes nothing is decoration. A noisy, low-contrast but sharp
        // frame should rank better under Wildlife (which barely weights noise) than under
        // Portrait (which weights it heavily).
        let frames: Vec<FrameMeasurement> = (0..10)
            .map(|i| {
                let mut f = FrameMeasurement::new(i as i64, "/lib")
                    .with_camera(Some("X".into()))
                    .with_capture_time(Some(1000 + i as i64));
                // Frame 9: sharpest, but noisiest and lowest range.
                f.set(Metric::Focus, if i == 9 { 100.0 } else { i as f64 * 5.0 });
                f.set(Metric::Detail, if i == 9 { 1000.0 } else { i as f64 * 50.0 });
                f.set(Metric::ExposureRange, if i == 9 { 0.05 } else { i as f64 / 10.0 });
                f.set(Metric::ClippedHigh, 0.0);
                f.set(Metric::ClippedLow, 0.0);
                f.set(Metric::Noise, if i == 9 { 50.0 } else { 1.0 });
                f
            })
            .collect();
        let n = shoot::normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);

        let wildlife = score_with_preset(&n, "Wildlife", &BandThresholds::default()).unwrap();
        let portrait = score_with_preset(&n, "Portrait", &BandThresholds::default()).unwrap();

        assert!(
            wildlife[9].composite > portrait[9].composite,
            "the sharp-but-noisy frame must fare better under Wildlife than Portrait: \
             {:.1} vs {:.1}",
            wildlife[9].composite,
            portrait[9].composite
        );
    }

    // ---------------------------------------------------------------------
    // Bands
    // ---------------------------------------------------------------------
    #[test]
    fn bands_follow_the_thresholds() {
        let t = BandThresholds::default();
        assert_eq!(t.band_of(100.0), Band::Keep);
        assert_eq!(t.band_of(70.0), Band::Keep, "the boundary is inclusive");
        assert_eq!(t.band_of(69.9), Band::Review);
        assert_eq!(t.band_of(35.0), Band::Review, "the reject boundary is exclusive");
        assert_eq!(t.band_of(34.9), Band::Reject);
        assert_eq!(t.band_of(0.0), Band::Reject);
    }

    #[test]
    fn nonsensical_thresholds_are_refused() {
        assert!(!BandThresholds { keep: 30.0, reject: 70.0 }.is_valid(), "inverted");
        assert!(!BandThresholds { keep: 50.0, reject: 50.0 }.is_valid(), "empty review band");
        assert!(!BandThresholds { keep: 150.0, reject: 0.0 }.is_valid(), "out of range");
        assert!(BandThresholds::default().is_valid());
    }

    #[test]
    fn a_reject_band_is_a_flag_and_carries_no_action() {
        // The band is a label. Nothing in this module can remove a file, and this test
        // exists so that a future refactor that tries to give Band a side effect has to
        // delete an assertion that says why it must not.
        let s = Score {
            photo_id: 1,
            composite: 0.0,
            band: Band::Reject,
            terms: Vec::new(),
            shoot_relative: true,
            shoot_size: 10,
            preset: "Balanced",
        };
        assert_eq!(s.band.label(), "Reject");
        // The type has no method that touches the filesystem; this is a compile-time
        // property, and the assertion above pins the label.
    }

    // ---------------------------------------------------------------------
    // Explainability
    // ---------------------------------------------------------------------
    #[test]
    fn every_term_explains_itself_specifically() {
        let n = improving_shoot(12);
        let s = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        for score in &s {
            for line in score.explain() {
                assert!(!line.trim().is_empty());
            }
            // No generic filler: every term line must name a dimension.
            for t in &score.terms {
                let text = t.describe();
                assert!(
                    text.contains(t.kind.label()),
                    "the explanation for {:?} does not name it: {text}",
                    t.kind
                );
            }
        }
    }

    #[test]
    fn the_explanation_names_the_reference_for_the_percentile() {
        // "40th percentile" is meaningless without saying whether it is within the shoot
        // or across the library, and the module falls back to the library for small shoots.
        let big = improving_shoot(12);
        let s = score_all(&big, &default_preset().weights, &BandThresholds::default(), "Balanced");
        let text = s[5].explain().join("\n");
        assert!(text.contains("this shoot"), "large shoot should say so:\n{text}");

        let small = improving_shoot(3);
        let s = score_all(&small, &default_preset().weights, &BandThresholds::default(), "Balanced");
        let text = s[1].explain().join("\n");
        assert!(
            text.contains("across the library"),
            "a small shoot must say the reference is the library:\n{text}"
        );
    }

    #[test]
    fn detractors_are_ranked_by_cost_not_by_percentile() {
        // A heavily weighted dimension at the 30th percentile costs more than a lightly
        // weighted one at the 0th, and the user wants to know what actually holds the
        // frame back.
        let heavy_low = Term::new(TermKind::Focus, 0.9, 30.0, 0.0); // loss 63
        let light_zero = Term::new(TermKind::Noise, 0.1, 0.0, 0.0); // loss 10
        assert!(heavy_low.loss > light_zero.loss);

        let s = Score {
            photo_id: 1,
            composite: 50.0,
            band: Band::Review,
            terms: vec![light_zero, heavy_low.clone()],
            shoot_relative: true,
            shoot_size: 10,
            preset: "Balanced",
        };
        assert_eq!(s.top_detractors(1)[0].kind, TermKind::Focus);
    }

    #[test]
    fn a_frame_with_no_clipping_is_never_complained_about() {
        // The bug this replaced: with 49 of 50 frames tied at zero clipping, mid-ranks
        // put them all near the 51st percentile, and the explanation read
        // "- no clipping (51st percentile)". Nothing is wrong with a frame that has no
        // clipping, so it must not appear as a complaint however it ranks.
        let clipped = Term::new(TermKind::Clipping, 0.2, 51.0, 0.0);
        assert!(clipped.is_at_ideal());

        let s = Score {
            photo_id: 1,
            composite: 60.0,
            band: Band::Review,
            terms: vec![
                clipped,
                Term::new(TermKind::Focus, 0.8, 70.0, 50.0),
            ],
            shoot_relative: true,
            shoot_size: 50,
            preset: "Balanced",
        };
        assert!(
            s.top_detractors(5).iter().all(|t| t.kind != TermKind::Clipping),
            "a clean clipping result must not be listed as a detractor"
        );
        let text = s.explain().join("\n");
        assert!(!text.contains("− no clipping"), "explanation reads:\n{text}");
    }

    #[test]
    fn a_frame_that_does_clip_is_complained_about() {
        // The complement, so the guard above cannot be satisfied by never complaining.
        // Equal weights, so the comparison is decided by percentile and nothing else.
        // With clipping at 0.2 weight against focus at 0.8, focus's larger share made it
        // the bigger loss even at a better percentile — which is correct behaviour and
        // made the first version of this test assert the wrong thing.
        let clipped = Term::new(TermKind::Clipping, 0.5, 5.0, 0.08);
        assert!(!clipped.is_at_ideal());
        let s = Score {
            photo_id: 1,
            composite: 50.0,
            band: Band::Review,
            terms: vec![clipped, Term::new(TermKind::Focus, 0.5, 60.0, 50.0)],
            shoot_relative: true,
            shoot_size: 50,
            preset: "Balanced",
        };
        assert_eq!(
            s.top_detractors(1)[0].kind,
            TermKind::Clipping,
            "clipping at the 5th percentile costs more than focus at the 60th at equal weight"
        );
    }

    #[test]
    fn a_good_result_is_never_presented_as_a_complaint() {
        // The bug this replaced: "− tonal range good — 88% of the range used (75th
        // percentile)". A term above average is not a complaint, however it ranks among
        // the frame's own terms.
        let good = Term::new(TermKind::ExposureRange, 0.5, 75.0, 0.88);
        let s = Score {
            photo_id: 1,
            composite: 70.0,
            band: Band::Keep,
            terms: vec![good, Term::new(TermKind::Focus, 0.5, 80.0, 60.0)],
            shoot_relative: true,
            shoot_size: 50,
            preset: "Balanced",
        };
        assert!(
            s.top_detractors(5).is_empty(),
            "nothing is below average, so nothing is a detractor"
        );
        let text = s.explain().join("\n");
        assert!(!text.contains('−'), "no minus lines expected:\n{text}");
        assert!(text.contains("nothing below average"), "and it should say so:\n{text}");
    }

    #[test]
    fn a_below_average_term_is_presented_as_a_complaint() {
        let poor = Term::new(TermKind::Noise, 0.5, 12.0, 40.0);
        let s = Score {
            photo_id: 1,
            composite: 45.0,
            band: Band::Review,
            terms: vec![poor, Term::new(TermKind::Focus, 0.5, 80.0, 60.0)],
            shoot_relative: true,
            shoot_size: 50,
            preset: "Balanced",
        };
        assert_eq!(s.top_detractors(1)[0].kind, TermKind::Noise);
        assert!(s.explain().join("\n").contains('−'));
    }

    #[test]
    fn a_mediocre_frame_says_there_is_nothing_to_report() {
        // Everything at exactly average. The explanation must not invent a strength or a
        // weakness to fill the space.
        let s = Score {
            photo_id: 1,
            composite: 50.0,
            band: Band::Review,
            terms: vec![
                Term::new(TermKind::Focus, 0.5, 50.0, 50.0),
                Term::new(TermKind::Noise, 0.5, 50.0, 10.0),
            ],
            shoot_relative: true,
            shoot_size: 50,
            preset: "Balanced",
        };
        let text = s.explain().join("\n");
        assert!(text.contains("nothing above average"), "reads:\n{text}");
        assert!(text.contains("nothing below average"), "reads:\n{text}");
    }

    #[test]
    fn a_perfect_frame_has_no_detractors() {
        let perfect = Term::new(TermKind::Focus, 1.0, 100.0, 0.0);
        let s = Score {
            photo_id: 1,
            composite: 100.0,
            band: Band::Keep,
            terms: vec![perfect],
            shoot_relative: true,
            shoot_size: 10,
            preset: "Balanced",
        };
        assert!(s.top_detractors(3).is_empty(), "nothing to complain about");
    }

    #[test]
    fn contributions_and_losses_account_for_the_whole_score() {
        // Every point the composite has must be attributable, or the explanation is
        // describing something other than the number.
        let n = improving_shoot(12);
        for s in score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced") {
            let sum: f64 = s.terms.iter().map(|t| t.contribution).sum();
            assert!(
                (sum - s.composite).abs() < 1e-9,
                "contributions sum to {sum} but the composite is {}",
                s.composite
            );
            let total_loss: f64 = s.terms.iter().map(|t| t.loss).sum();
            assert!(
                (sum + total_loss - 100.0).abs() < 1e-9,
                "gained {sum} and lost {total_loss}, which is not 100"
            );
        }
    }

    #[test]
    fn unimplemented_dimensions_are_declared_with_reasons() {
        // Reported rather than silently weighted at zero, which would renormalise the
        // rest and present a complete-looking score missing a third of what was promised.
        let dims = unimplemented_dimensions();
        assert!(!dims.is_empty());
        for (name, reason) in dims {
            assert!(!name.is_empty());
            assert!(reason.len() > 10, "{name} needs a real reason, got {reason:?}");
        }
        let names: Vec<&str> = dims.iter().map(|(n, _)| *n).collect();
        for expected in ["composition", "aesthetic", "expression", "eyes_open"] {
            assert!(names.contains(&expected), "{expected} should be declared as missing");
        }
    }

    #[test]
    fn a_clipping_free_frame_says_so_plainly() {
        let t = Term::new(TermKind::Clipping, 0.2, 100.0, 0.0);
        assert_eq!(t.describe(), "no clipping");
    }

    #[test]
    fn the_composite_of_a_single_frame_shoot_is_still_computed() {
        // Degenerate but reachable: a folder with one photograph.
        let frames = vec![FrameMeasurement::new(1, "/lib")
            .with_camera(Some("X".into()))
            .with_capture_time(Some(1000))];
        let n = shoot::normalise(&frames, DEFAULT_SHOOT_GAP_SECONDS);
        let s = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        assert_eq!(s.len(), 1);
        assert!(s[0].composite.is_finite());
        assert!(!s[0].shoot_relative, "one frame cannot be ranked within a shoot");
    }

    #[test]
    fn every_term_kind_is_reachable_from_a_real_score() {
        // Guards against a term that is computed but never added, which would silently
        // drop a dimension from every score.
        let n = improving_shoot(12);
        let s = score_all(&n, &default_preset().weights, &BandThresholds::default(), "Balanced");
        let kinds: Vec<TermKind> = s[5].terms.iter().map(|t| t.kind).collect();
        for expected in [
            TermKind::Focus,
            TermKind::Detail,
            TermKind::ExposureRange,
            TermKind::Clipping,
            TermKind::Noise,
        ] {
            assert!(kinds.contains(&expected), "{expected:?} missing from a full score");
        }
    }

    #[test]
    fn direction_is_consistent_with_what_the_composite_assumes() {
        // The composite reads oriented percentiles, so every metric it consumes must be
        // rankable. If one became NotRanked the term would silently vanish.
        for m in [
            Metric::Focus,
            Metric::Detail,
            Metric::ExposureRange,
            Metric::ClippedHigh,
            Metric::ClippedLow,
            Metric::Noise,
        ] {
            assert_ne!(
                direction(m),
                Direction::NotRanked,
                "{m:?} is consumed by the composite and must be rankable"
            );
        }
    }
}
