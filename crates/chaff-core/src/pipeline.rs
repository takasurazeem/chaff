//! The culling pipeline: index a folder, score what it found, record both.
//!
//! This is the engine end to end — walk, pair, measure, rank within the shoot, weight into
//! a composite, and persist. It lives in `chaff-core` rather than in the Tauri shell so
//! that it can be run, and tested, on a machine with no webview.
//!
//! # The order is not arbitrary
//!
//! Indexing has to happen before scoring, because scoring needs to know which files belong
//! to which photograph. Shoot-relative ranking has to happen after *every* frame is
//! measured, because a percentile is a property of the set. And the composite has to
//! happen after normalisation, because it consumes percentiles rather than raw values.
//!
//! Getting that order wrong produces numbers that look plausible and rank nothing — which
//! is why it is asserted rather than assumed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::Connection;
use thiserror::Error;

use crate::catalog::{store, CatalogError};
use crate::exif;
use crate::imaging::{Luma, Region};
use crate::indexer::{self, IndexError};
use crate::scoring::{
    composite::{self, BandThresholds, Score},
    exposure::{self, Levels},
    focus::{self, FocusMetrics},
    shoot::{self, FrameMeasurement, DEFAULT_SHOOT_GAP_SECONDS},
};
use crate::thumb;

/// Bumped whenever a metric changes in a way that makes old scores incomparable.
///
/// Scores are keyed by this, so bumping it writes new rows instead of overwriting the old
/// ones — which keeps a regression diagnosable rather than destructive.
pub const SCORER_VERSION: i64 = 1;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Normalise(#[from] shoot::NormaliseError),
}

/// What one pipeline run did.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineReport {
    pub library_id: i64,
    pub root: PathBuf,
    pub scanned_files: usize,
    pub photos: usize,
    pub pairs: usize,
    pub needs_review: usize,
    /// Photographs that produced measurements.
    pub scored: usize,
    /// Photographs whose image data this build cannot read. Needs a raw decoder (#8).
    pub unscoreable: usize,
    pub shoots: usize,
    pub bands: BandCounts,
    pub elapsed_ms: u128,
}

impl PipelineReport {
    /// The result, without how long it took.
    ///
    /// `elapsed_ms` is a measurement of the machine, not of the library, so two runs that
    /// produced the same outcome differ in it. Comparing reports — in a test, or in a log
    /// after a re-index — means comparing this.
    pub fn without_timing(&self) -> Self {
        Self { elapsed_ms: 0, ..self.clone() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BandCounts {
    pub keep: usize,
    pub review: usize,
    pub reject: usize,
}

impl BandCounts {
    fn add(&mut self, band: composite::Band) {
        match band {
            composite::Band::Keep => self.keep += 1,
            composite::Band::Review => self.review += 1,
            composite::Band::Reject => self.reject += 1,
        }
    }
}

/// Index a folder and score everything in it.
///
/// `now` is supplied rather than read from the clock so the whole run is reproducible.
pub fn index_and_score(
    conn: &mut Connection,
    root: &Path,
    now: i64,
) -> Result<PipelineReport, PipelineError> {
    let started = Instant::now();

    let outcome = indexer::index(conn, root, now)?;
    let library_id = outcome.library_id;

    let photos = store::photos(conn, library_id)?;
    let mut measured: Vec<FrameMeasurement> = Vec::with_capacity(photos.len());
    let mut unscoreable = 0usize;

    for photo in &photos {
        let files = store::files_for_photo(conn, photo.id)?;

        // Prefer the raw: its embedded preview is the camera's own rendering, and for a
        // paired photograph the raw is the one that carries the full sensor data. Fall
        // back to the rendered file, then to anything readable.
        let mut candidates: Vec<&store::FileRow> =
            files.iter().filter(|f| f.role == "raw").collect();
        candidates.extend(files.iter().filter(|f| f.role == "raster"));

        let mut row = None;
        for f in candidates {
            if let Some(m) = measure(Path::new(&f.path)) {
                row = Some((f.clone(), m));
                break;
            }
        }

        let Some((file, (focus_m, exposure_m))) = row else {
            // No readable image data. A raw format needing LibRaw (#8), or a corrupt file.
            // Counted rather than fatal: one unreadable photograph must not abandon a
            // scoring pass over ten thousand others.
            unscoreable += 1;
            continue;
        };

        let exif_data = exif::read(Path::new(&file.path)).ok().and_then(|r| r.data().cloned());

        // `with_exif` consumes and returns, so the chain has to be reassembled rather
        // than built up with two `&mut self` calls and then moved.
        let mut frame = FrameMeasurement::from_focus(photo.id, &photo.dir, &focus_m);
        frame.from_exposure(&exposure_m);
        let frame = frame.with_exif(exif_data.as_ref());

        measured.push(frame);
    }

    // Ranking is a property of the set, so it can only happen once everything is measured.
    let normalised = shoot::normalise(&measured, DEFAULT_SHOOT_GAP_SECONDS)?;

    let preset = composite::default_preset();
    let thresholds = BandThresholds::default();
    let scores = composite::score_all(&normalised, &preset.weights, &thresholds, preset.name);

    let mut bands = BandCounts::default();
    for score in &scores {
        bands.add(score.band);
        persist_score(conn, score, now)?;
    }

    let shoot_count = normalised.iter().map(|n| n.shoot_id).max().map(|m| m + 1).unwrap_or(0);

    Ok(PipelineReport {
        library_id,
        root: root.to_path_buf(),
        scanned_files: outcome.scanned_files,
        photos: outcome.stats.photos,
        pairs: outcome.stats.pairs,
        needs_review: outcome.stats.needs_review,
        scored: scores.len(),
        unscoreable,
        shoots: shoot_count,
        bands,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Decode one file and measure it.
///
/// Returns `None` rather than an error when the image cannot be read: a photograph this
/// build cannot decode is a gap to count, not a reason to fail a whole run.
fn measure(path: &Path) -> Option<(FocusMetrics, exposure::ExposureMetrics)> {
    let img = thumb::decode_source(path).ok()?;
    let gray = img.to_luma8();
    let luma = Luma::from_gray8(gray.width() as usize, gray.height() as usize, gray.as_raw());

    let focus_m = focus::analyse(&luma, None);
    // Exposure is measured over the whole frame. Unlike focus it is not a property of the
    // subject — a blown highlight in the corner is still a blown highlight.
    let exposure_m = exposure::analyse(&luma, Region::full(luma.w, luma.h), Levels::eight_bit());
    Some((focus_m, exposure_m))
}

/// Write a score's terms and composite to the catalog.
fn persist_score(conn: &Connection, score: &Score, now: i64) -> Result<(), CatalogError> {
    store::upsert_score(
        conn,
        score.photo_id,
        "composite",
        score.composite,
        SCORER_VERSION,
        now,
    )?;
    store::upsert_score(
        conn,
        score.photo_id,
        "band",
        match score.band {
            composite::Band::Keep => 2.0,
            composite::Band::Review => 1.0,
            composite::Band::Reject => 0.0,
        },
        SCORER_VERSION,
        now,
    )?;
    // The shoot context is stored rather than passed in by the caller. It was a
    // parameter, and the CLI passed the *library* total for it — which read correctly
    // only because the folder happened to contain exactly one shoot. A caller cannot be
    // trusted to know something the scorer already knew.
    store::upsert_score(
        conn,
        score.photo_id,
        "shoot_size",
        score.shoot_size as f64,
        SCORER_VERSION,
        now,
    )?;
    store::upsert_score(
        conn,
        score.photo_id,
        "shoot_relative",
        if score.shoot_relative { 1.0 } else { 0.0 },
        SCORER_VERSION,
        now,
    )?;
    for term in &score.terms {
        // The term label is stable and human-readable; the percentile goes in the same
        // row's value so the UI can show either without a second query.
        store::upsert_score(
            conn,
            score.photo_id,
            &format!("term:{}", term.kind.label().replace(' ', "_")),
            term.percentile,
            SCORER_VERSION,
            now,
        )?;
    }
    Ok(())
}

/// Photographs with their scores, for a grid.
///
/// One query for the whole library rather than one per row: a 50,000-cell grid asking
/// per cell is 50,000 SQLite round trips, which is the difference between a grid that
/// appears and one that crawls.
pub fn scored_photos(
    conn: &Connection,
    library_id: i64,
) -> Result<Vec<(store::PhotoRow, Option<f64>)>, CatalogError> {
    let photos = store::photos(conn, library_id)?;
    let scores: HashMap<i64, f64> = store::composites(conn, library_id, SCORER_VERSION)?;
    Ok(photos.into_iter().map(|p| { let s = scores.get(&p.id).copied(); (p, s) }).collect())
}

/// The explanation for one photograph's score, rebuilt from stored terms.
///
/// Rebuilt rather than stored as text, so that changing how an explanation is worded does
/// not require re-scoring the library.
pub fn explain_photo(conn: &Connection, photo_id: i64) -> Result<Vec<String>, CatalogError> {
    let rows = store::scores_for_photo(conn, photo_id, SCORER_VERSION)?;
    if rows.is_empty() {
        return Ok(vec!["not scored".to_string()]);
    }

    let shoot_size = rows
        .iter()
        .find(|r| r.metric == "shoot_size")
        .map(|r| r.value as usize)
        .unwrap_or(0);
    let shoot_relative = rows
        .iter()
        .find(|r| r.metric == "shoot_relative")
        .map(|r| r.value > 0.5)
        .unwrap_or(false);

    let composite_value = rows
        .iter()
        .find(|r| r.metric == "composite")
        .map(|r| r.value)
        .unwrap_or(0.0);
    let band = rows
        .iter()
        .find(|r| r.metric == "band")
        .map(|r| match r.value as i64 {
            2 => "Keep",
            1 => "Review",
            _ => "Reject",
        })
        .unwrap_or("Review");

    let reference = if shoot_relative {
        format!("of {shoot_size} in this shoot")
    } else {
        format!("across the library; this shoot has only {shoot_size} frames")
    };

    let mut out = vec![format!("{band} — {composite_value:.0}/100 (ranked {reference})")];
    for r in rows.iter().filter(|r| r.metric.starts_with("term:")).take(4) {
        let name = r.metric.trim_start_matches("term:").replace('_', " ");
        out.push(format!("  {name}: {}th percentile", r.value.round() as i64));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::open_in_memory;
    use std::fs;
    use tempfile::tempdir;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/images")
    }

    /// Copy generated fixtures into a temp library so the pipeline has real photographs.
    fn build_library(names: &[&str]) -> Option<(tempfile::TempDir, Vec<String>)> {
        let src = fixture_dir();
        if !src.is_dir() {
            eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
            return None;
        }
        let dir = tempdir().unwrap();
        let mut copied = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let from = src.join(format!("{n}.jpg"));
            if !from.is_file() {
                continue;
            }
            let to = dir.path().join(format!("IMG_{i:04}.JPG"));
            fs::copy(&from, &to).unwrap();
            copied.push(to.to_string_lossy().to_string());
        }
        Some((dir, copied))
    }

    #[test]
    fn the_pipeline_indexes_and_scores_a_real_folder() {
        let names = [
            "sharp_a",
            "sharp_b",
            "blur_defocus_mild",
            "blur_defocus_heavy",
            "noise_high_iso",
            "bokeh_portrait",
            "flat_low_contrast",
            "exp_normal",
            "exp_over",
            "burst_0_sharp",
            "burst_1_sharp",
            "burst_2_sharp",
        ];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).expect("pipeline");

        assert_eq!(report.scanned_files, names.len());
        assert_eq!(report.photos, names.len(), "one photograph per file here");
        assert_eq!(report.scored, names.len(), "every fixture is decodable");
        assert_eq!(report.unscoreable, 0);
        assert!(report.shoots >= 1);

        // Every photograph must have a composite recorded.
        let scored = scored_photos(&conn, report.library_id).unwrap();
        assert_eq!(scored.len(), names.len());
        assert!(
            scored.iter().all(|(_, s)| s.is_some()),
            "every photograph must come back with a score"
        );
    }

    #[test]
    fn the_pipeline_ranks_the_sharp_fixtures_above_the_blurred_ones() {
        // End-to-end verification that the whole chain — decode, measure, rank, weight —
        // produces the ordering the individual metrics promise. A pipeline that wired the
        // pieces together in the wrong order would still produce plausible numbers, and
        // this is what catches that.
        let names = [
            "sharp_a",
            "sharp_b",
            "sharp_broadband",
            "blur_defocus_mild",
            "blur_defocus_heavy",
            "blur_defocus_broadband",
            "blur_motion",
            "noise_high_iso",
            "exp_normal",
            "exp_over",
            "flat_low_contrast",
            "bokeh_portrait",
        ];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();
        let scored = scored_photos(&conn, report.library_id).unwrap();

        // Map back from filename to score. The files were renamed IMG_000N in the order
        // they appear in `names`, so index position is the mapping.
        let by_index: Vec<Option<f64>> = {
            let mut v = vec![None; names.len()];
            for (photo, score) in &scored {
                let files = store::files_for_photo(&conn, photo.id).unwrap();
                let path = files[0].path.clone();
                let stem = Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
                if let Some(n) = stem.strip_prefix("IMG_").and_then(|s| s.parse::<usize>().ok()) {
                    v[n] = *score;
                }
            }
            v
        };

        let sharp = by_index[0].expect("sharp_a scored");
        let heavy = by_index[4].expect("blur_defocus_heavy scored");
        assert!(
            sharp > heavy + 20.0,
            "a sharp frame ({sharp:.1}) must clearly outrank a heavily blurred one ({heavy:.1})"
        );

        let motion = by_index[6].expect("blur_motion scored");
        assert!(sharp > motion, "sharp ({sharp:.1}) must outrank motion-blurred ({motion:.1})");
    }

    #[test]
    fn an_unreadable_file_is_counted_not_fatal() {
        // One photograph this build cannot decode must not abandon a scoring pass over
        // everything else.
        let names = ["sharp_a", "sharp_b", "blur_defocus_mild", "noise_high_iso"];
        let Some((dir, _)) = build_library(&names) else { return };

        // A file with an image extension and no image content.
        fs::write(dir.path().join("IMG_9999.JPG"), b"not an image at all").unwrap();

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();

        assert_eq!(report.unscoreable, 1, "the broken file must be counted");
        assert_eq!(report.scored, names.len(), "and everything else still scored");
    }

    #[test]
    fn scores_are_persisted_under_the_scorer_version() {
        let names = ["sharp_a", "sharp_b", "blur_defocus_mild", "noise_high_iso"];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();
        let photos = store::photos(&conn, report.library_id).unwrap();

        let rows = store::scores_for_photo(&conn, photos[0].id, SCORER_VERSION).unwrap();
        assert!(
            rows.iter().any(|r| r.metric == "composite"),
            "a composite must be recorded"
        );
        assert!(rows.iter().any(|r| r.metric == "band"), "and a band");
        assert!(
            rows.iter().any(|r| r.metric.starts_with("term:")),
            "and per-term percentiles, so an explanation can be rebuilt without re-scoring"
        );
    }

    #[test]
    fn a_second_run_at_a_new_version_does_not_destroy_the_first() {
        // Scores are keyed by version. Re-scoring writes new rows; keeping the old ones is
        // what makes a scorer regression diagnosable rather than destructive.
        let names = ["sharp_a", "sharp_b", "blur_defocus_mild", "noise_high_iso"];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();
        let photos = store::photos(&conn, report.library_id).unwrap();
        let photo_id = photos[0].id;

        let before = store::scores_for_photo(&conn, photo_id, SCORER_VERSION).unwrap();
        assert!(!before.is_empty());

        // A hypothetical future scorer.
        store::upsert_score(&conn, photo_id, "composite", 99.0, SCORER_VERSION + 1, 1_700_000_100)
            .unwrap();

        let still_there = store::scores_for_photo(&conn, photo_id, SCORER_VERSION).unwrap();
        assert_eq!(still_there, before, "the old version's scores must survive");
        let newer = store::scores_for_photo(&conn, photo_id, SCORER_VERSION + 1).unwrap();
        assert_eq!(newer.len(), 1);
    }

    #[test]
    fn re_running_the_pipeline_is_idempotent() {
        let names = ["sharp_a", "sharp_b", "blur_defocus_mild", "noise_high_iso"];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let first = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();
        let a = scored_photos(&conn, first.library_id).unwrap();

        let second = index_and_score(&mut conn, dir.path(), 1_700_000_100).unwrap();
        let b = scored_photos(&conn, second.library_id).unwrap();

        assert_eq!(first.library_id, second.library_id);
        assert_eq!(a, b, "a re-run must not change what the grid shows");
        assert_eq!(second.unscoreable, 0);
    }

    #[test]
    fn the_pipeline_is_deterministic() {
        let names = ["sharp_a", "sharp_b", "blur_defocus_mild", "noise_high_iso", "exp_over"];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut c1 = open_in_memory().unwrap();
        let mut c2 = open_in_memory().unwrap();
        let r1 = index_and_score(&mut c1, dir.path(), 1_700_000_000).unwrap();
        let r2 = index_and_score(&mut c2, dir.path(), 1_700_000_000).unwrap();

        // Compared without timing: `elapsed_ms` measures the machine, not the library.
        // The first version of this test compared the whole report and failed on a 146 ms
        // difference between two identical runs.
        assert_eq!(r1.without_timing(), r2.without_timing());
        assert_eq!(
            scored_photos(&c1, r1.library_id).unwrap(),
            scored_photos(&c2, r2.library_id).unwrap()
        );
    }

    #[test]
    fn an_empty_folder_produces_an_empty_report_not_an_error() {
        let dir = tempdir().unwrap();
        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();

        assert_eq!(report.photos, 0);
        assert_eq!(report.scored, 0);
        assert_eq!(report.bands, BandCounts::default());
    }

    #[test]
    fn bands_partition_the_scored_photographs_exactly() {
        let names = [
            "sharp_a",
            "sharp_b",
            "sharp_broadband",
            "blur_defocus_mild",
            "blur_defocus_heavy",
            "blur_defocus_broadband",
            "noise_high_iso",
            "flat_low_contrast",
            "exp_normal",
            "exp_over",
            "bokeh_portrait",
            "burst_0_sharp",
        ];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();

        let total = report.bands.keep + report.bands.review + report.bands.reject;
        assert_eq!(total, report.scored, "every scored photograph lands in exactly one band");
    }

    #[test]
    fn the_explanation_can_be_rebuilt_from_stored_scores() {
        // Rebuilt rather than stored as text, so rewording an explanation does not require
        // re-scoring a library.
        let names = ["sharp_a", "sharp_b", "blur_defocus_mild", "noise_high_iso", "exp_over"];
        let Some((dir, _)) = build_library(&names) else { return };

        let mut conn = open_in_memory().unwrap();
        let report = index_and_score(&mut conn, dir.path(), 1_700_000_000).unwrap();
        let photos = store::photos(&conn, report.library_id).unwrap();

        let lines = explain_photo(&conn, photos[0].id).unwrap();
        assert!(lines.len() >= 2, "an explanation needs a verdict and at least one term");
        assert!(lines[0].contains("Keep") || lines[0].contains("Review") || lines[0].contains("Reject"));
        assert!(
            lines[0].contains("this shoot") || lines[0].contains("across the library"),
            "the reference must be named: {:?}",
            lines[0]
        );
    }

    #[test]
    fn an_unscored_photograph_says_so_rather_than_inventing_a_verdict() {
        let conn = open_in_memory().unwrap();
        let lib = store::upsert_library(&conn, Path::new("/empty"), 1).unwrap();
        conn.execute(
            "INSERT INTO photo (library_id, dir, stem, state, needs_review) VALUES (?1,'/empty','x','pair',0)",
            rusqlite::params![lib],
        )
        .unwrap();
        let id: i64 = conn.query_row("SELECT id FROM photo LIMIT 1", [], |r| r.get(0)).unwrap();

        let lines = explain_photo(&conn, id).unwrap();
        assert_eq!(lines, vec!["not scored".to_string()]);
    }
}
