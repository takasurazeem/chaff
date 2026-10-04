//! Integration tests against the real downloaded corpus.
//!
//! # Why this file exists separately from the unit tests
//!
//! The unit tests in `src/` run against `fixtures/synthetic/` — generated, tiny,
//! committed, deterministic, offline. That is the right substrate for testing an
//! *algorithm*, and it is what found every bug during the focus metric's development.
//!
//! But synthetic fixtures cannot answer the question that actually decides whether this
//! product works: **do the thresholds hold on real photographs?** The motion-blur
//! anisotropy threshold has a 35x margin on a synthetic grid. A real picket fence, a
//! horizon, or a curtain could narrow that to nothing. Only real files can say.
//!
//! # Skipping rather than failing
//!
//! `fixtures/corpus/` is gitignored and ~47 MB, so it is deliberately absent from CI and
//! from the Linux build host. Every test here skips cleanly with an explanatory message
//! when the corpus is missing. That is not a weakness — a corpus-dependent test that
//! *fails* on a fresh clone trains people to ignore red builds. Run
//! `python3 tools/fixtures/fetch_corpus.py --raw 12 --jpeg 50` to enable them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chaff_core::ext::{classify, FileKind};
use chaff_core::imaging::Luma;
use chaff_core::pair::{resolve, GroupState};
use chaff_core::scoring::focus::{analyse, prepare, FocusMetrics, ShootBaseline};

// ---------------------------------------------------------------------------
// Corpus discovery
// ---------------------------------------------------------------------------
fn corpus_root() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus");
    p.is_dir().then_some(p)
}

fn files_in(sub: &str, exts: &[&str]) -> Vec<PathBuf> {
    let Some(root) = corpus_root() else { return Vec::new() };
    let dir = root.join(sub);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

fn jpegs() -> Vec<PathBuf> {
    files_in("jpeg", &["jpg", "jpeg"])
}

fn raws() -> Vec<PathBuf> {
    files_in("raw", &["cr2", "cr3", "crw", "nef", "nrw", "arw", "raf", "orf", "rw2", "dng", "pef", "mrw", "kdc", "raw", "tif", "cam", "sr2", "srf", "srw", "3fr", "iiq", "x3f"])
}

/// Announce a skip loudly. A silently passing test that did nothing is worse than no test.
fn skip(what: &str) -> bool {
    if corpus_root().is_none() {
        eprintln!(
            "\nSKIP {what}: fixtures/corpus/ is absent.\n     \
             Enable with: python3 tools/fixtures/fetch_corpus.py --raw 12 --jpeg 50\n"
        );
        return true;
    }
    false
}

fn load(path: &Path) -> Luma {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("could not decode {}: {e}", path.display()))
        .to_luma8();
    Luma::from_gray8(img.width() as usize, img.height() as usize, img.as_raw())
}

fn analyse_file(path: &Path) -> FocusMetrics {
    analyse(&load(path), None)
}

/// Every measurement taken from one decode of one file.
#[derive(Clone, PartialEq)]
struct Measurements {
    focus: FocusMetrics,
    exposure: chaff_core::scoring::exposure::ExposureMetrics,
}

/// Memoised analysis, shared by every test that walks the corpus.
///
/// Decoding and measuring a 1600x1067 JPEG costs ~0.7s. Several tests here walk all 50
/// of them, and without a shared cache the suite re-decodes the corpus once per test —
/// which took it past 80 seconds. One cache holding *both* focus and exposure means a
/// test that needs only one of them still pays for a single decode.
///
/// Safe because every measurement here is deterministic and pure.
fn measurements_for(path: &Path) -> Measurements {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Measurements>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("cache mutex");

    if let Some(hit) = guard.get(path) {
        return hit.clone();
    }

    let luma = load(path);
    let computed = Measurements {
        focus: analyse(&luma, None),
        exposure: chaff_core::scoring::exposure::analyse(
            &luma,
            chaff_core::imaging::Region::full(luma.w, luma.h),
            chaff_core::scoring::exposure::Levels::eight_bit(),
        ),
    };
    guard.insert(path.to_path_buf(), computed.clone());
    computed
}

fn metrics_for(path: &Path) -> FocusMetrics {
    measurements_for(path).focus
}

// ---------------------------------------------------------------------------
// Diagnostics — run with:
//     cargo test -p chaff-core --test corpus -- --ignored --nocapture real_corpus
// ---------------------------------------------------------------------------
#[test]
#[ignore]
fn real_corpus_distribution_report() {
    if skip("distribution report") {
        return;
    }
    let files = jpegs();
    println!("\n{} real photographs\n", files.len());
    println!(
        "{:<22} {:>9} {:>8} {:>8} {:>8} {:>8} {:>7}",
        "file", "source", "noise", "reliab", "norm", "aniso", "tileCV"
    );
    println!("{}", "-".repeat(76));

    let mut norms = Vec::new();
    let mut anisos = Vec::new();
    let flagged: Vec<(PathBuf, f64, f64)> = Vec::new();

    for f in &files {
        let m = analyse_file(f);
        norms.push(m.normalized_focus);
        anisos.push(m.anisotropy);
        println!(
            "{:<22} {:>9} {:>8.2} {:>8.3} {:>8.3} {:>8.3} {:>7.3}",
            f.file_name().unwrap().to_string_lossy(),
            format!("{:?}", m.roi_source),
            m.noise_sigma,
            m.noise_reliability,
            m.normalized_focus,
            m.anisotropy,
            m.tile_variation
        );
    }

    let pct = |v: &mut Vec<f64>, p: f64| -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() {
            return f64::NAN;
        }
        v[((v.len() - 1) as f64 * p).round() as usize]
    };

    println!("\nnormalized_focus  p05={:.3}  p50={:.3}  p95={:.3}", pct(&mut norms.clone(), 0.05), pct(&mut norms.clone(), 0.50), pct(&mut norms.clone(), 0.95));
    println!("anisotropy        p05={:.3}  p50={:.3}  p95={:.3}  max={:.3}", pct(&mut anisos.clone(), 0.05), pct(&mut anisos.clone(), 0.50), pct(&mut anisos.clone(), 0.95), anisos.iter().cloned().fold(0.0, f64::max));
    println!("\nnaive single-frame threshold (3.0) would flag: {} of {}",
        anisos.iter().filter(|a| **a >= 3.0).count(), files.len());
    for (f, a, n) in &flagged {
        println!("   {}  aniso={a:.2} norm={n:.3}", f.file_name().unwrap().to_string_lossy());
    }
    println!();
}

// ---------------------------------------------------------------------------
// The engine must survive real files at all
// ---------------------------------------------------------------------------
#[test]
fn real_photographs_analyse_without_panicking_or_producing_nan() {
    if skip("real-photo robustness") {
        return;
    }
    let files = jpegs();
    assert!(!files.is_empty(), "corpus dir exists but holds no JPEGs");

    for f in &files {
        let m = analyse_file(f);
        assert!(
            m.normalized_focus.is_finite(),
            "{} produced a non-finite focus score",
            f.display()
        );
        assert!(
            m.anisotropy.is_finite() && m.anisotropy >= 1.0,
            "{} produced anisotropy {}",
            f.display(),
            m.anisotropy
        );
        assert!(
            m.noise_reliability.is_finite() && (0.0..=1.0).contains(&m.noise_reliability),
            "{} produced reliability {} outside [0,1]",
            f.display(),
            m.noise_reliability
        );
        assert!(
            m.normalized_focus >= 0.0,
            "{} produced a negative focus score",
            f.display()
        );
    }
}

#[test]
fn real_photograph_analysis_is_deterministic() {
    if skip("real-photo determinism") {
        return;
    }
    // The product promise: the same file always scores the same. A user who re-runs a
    // cull and sees different numbers has lost all reason to trust the tool.
    for f in jpegs().iter().take(10) {
        let a = analyse_file(f);
        let b = analyse_file(f);
        assert_eq!(a, b, "{} scored differently on two runs", f.display());
    }
}

#[test]
fn real_photographs_produce_a_discriminating_distribution() {
    if skip("real-photo distribution") {
        return;
    }
    let files = jpegs();
    let scores: Vec<f64> = files.iter().map(|f| metrics_for(f).normalized_focus).collect();

    let max = scores.iter().cloned().fold(f64::MIN, f64::max);
    let min = scores.iter().cloned().fold(f64::MAX, f64::min);

    // A metric that returns ~0 for everything, or the same value for everything, ranks
    // nothing and is therefore useless regardless of how correct it is frame by frame.
    assert!(max > 0.0, "every real photograph scored zero");
    assert!(
        max > min * 1.5 + 1e-9,
        "real photographs produced a degenerate distribution: min {min:.4}, max {max:.4}"
    );

    let zeros = scores.iter().filter(|s| **s == 0.0).count();
    assert!(
        zeros * 4 < scores.len(),
        "{zeros} of {} real photographs scored exactly zero — far more than the \
         low-contrast/underexposed frames alone would explain",
        scores.len()
    );
}

// ---------------------------------------------------------------------------
// The false-positive check that actually matters
// ---------------------------------------------------------------------------
#[test]
fn a_single_frame_anisotropy_threshold_would_flag_real_photographs() {
    if skip("single-frame threshold counter-example") {
        return;
    }
    // **A regression guard on a design decision, not on a value.**
    //
    // The first implementation classified motion blur from a single frame with an
    // anisotropy threshold of 3.0, calibrated on synthetic fixtures where every
    // non-motion frame measured 1.00-1.74. That looked decisive. It was not.
    //
    // This test asserts that the naive rule is WRONG on real photographs, and will keep
    // asserting it. If it ever starts failing — if real photographs became that
    // directionally uniform — then the single-frame rule could be reconsidered. Until
    // then, motion classification needs a same-scene reference and this test is why.
    const NAIVE_THRESHOLD: f64 = 3.0;

    let files = jpegs();
    let flagged: Vec<(String, f64, f64)> = files
        .iter()
        .map(|f| {
            let m = metrics_for(f);
            (f.file_name().unwrap().to_string_lossy().to_string(), m.anisotropy, m.normalized_focus)
        })
        .filter(|(_, a, _)| *a >= NAIVE_THRESHOLD)
        .collect();

    let rate = flagged.len() as f64 / files.len() as f64;
    assert!(
        rate > 0.10,
        "only {:.0}% of real photographs ({}/{}) exceeded the naive single-frame \
         anisotropy threshold of {NAIVE_THRESHOLD}. That is low enough that the \
         single-frame rule might be viable after all — revisit the retraction in \
         focus.rs::ShootBaseline before assuming this test is just stale.",
        rate * 100.0,
        flagged.len(),
        files.len()
    );

    // And it demonstrably flags frames that are not blurred: a well-focused frame above
    // the corpus median focus is among them.
    let median_focus = {
        let mut v: Vec<f64> = files.iter().map(|f| metrics_for(f).normalized_focus).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let sharp_but_flagged = flagged.iter().filter(|(_, _, n)| *n > median_focus).count();
    assert!(
        sharp_but_flagged > 0,
        "the naive threshold flagged {} frames, but none of them scored above the median \
         focus ({median_focus:.3}) — the false-positive argument would be weaker than \
         recorded",
        flagged.len()
    );
    eprintln!(
        "single-frame threshold {NAIVE_THRESHOLD}: flagged {}/{} photographs ({:.0}%), \
         of which {sharp_but_flagged} scored above the median focus",
        flagged.len(),
        files.len(),
        rate * 100.0
    );
}

#[test]
fn real_photographs_are_not_mass_flagged_using_a_shoot_baseline() {
    if skip("shoot-relative motion candidates") {
        return;
    }
    // The corrected design. The corpus is 50 unrelated photographs, which is a harsher
    // test than a real shoot: a genuine burst shares scene, lens, lighting and subject,
    // so its anisotropy baseline is far tighter and discrimination is easier. Treating
    // unrelated images as one shoot is the pessimistic case.
    let files = jpegs();
    let metrics: Vec<FocusMetrics> = files.iter().map(|f| metrics_for(f)).collect();
    let baseline = ShootBaseline::from_metrics(&metrics).expect("baseline from 50 frames");

    let flagged: Vec<(String, f64, f64)> = files
        .iter()
        .zip(&metrics)
        .filter(|(_, m)| m.is_motion_blur_candidate(&baseline))
        .map(|(f, m)| {
            (f.file_name().unwrap().to_string_lossy().to_string(), m.anisotropy, m.normalized_focus)
        })
        .collect();

    eprintln!(
        "shoot-relative: baseline aniso {:.3}, focus {:.3} -> flagged {}/{}",
        baseline.anisotropy_median,
        baseline.focus_median,
        flagged.len(),
        files.len()
    );
    for (name, a, n) in &flagged {
        eprintln!("   {name}  aniso={a:.2} norm={n:.3}");
    }

    // This corpus is 50 UNRELATED photographs, which is a category error as a stand-in
    // for a shoot: a real shoot shares scene, lens and lighting, so its baseline is far
    // tighter and discrimination is far easier. Do not read this number as the detector's
    // real-world false-positive rate — it is a pessimistic bound. The detector is
    // validated properly on a synthetic burst in src/scoring/focus.rs, because bursts are
    // the substrate the design actually requires and this corpus has none.
    let rate = flagged.len() as f64 / files.len() as f64;
    assert!(
        rate < 0.18,
        "the shoot-relative detector flagged {:.0}% of unrelated real photographs \
         ({}/{}), which is no better than the {:.0}% the single-frame rule produced. \
         Offenders: {flagged:?}",
        rate * 100.0,
        flagged.len(),
        files.len(),
        18.0
    );
}

#[test]
fn shoot_relative_detection_is_stricter_than_the_single_frame_rule() {
    if skip("detector comparison") {
        return;
    }
    // Direct comparison on the same data, so the improvement is measured rather than
    // asserted. If the reference-based version were ever no better than the naive rule,
    // it would be extra complexity for nothing.
    const NAIVE_THRESHOLD: f64 = 3.0;

    let files = jpegs();
    let metrics: Vec<FocusMetrics> = files.iter().map(|f| metrics_for(f)).collect();
    let baseline = ShootBaseline::from_metrics(&metrics).expect("baseline");

    let naive = metrics.iter().filter(|m| m.anisotropy >= NAIVE_THRESHOLD).count();
    let relative = metrics.iter().filter(|m| m.is_motion_blur_candidate(&baseline)).count();

    assert!(
        relative < naive,
        "shoot-relative detection flagged {relative} frames and the naive single-frame \
         rule flagged {naive} — the added complexity bought nothing"
    );
}

#[test]
fn real_photograph_anisotropy_matches_the_synthetic_calibration() {
    if skip("anisotropy calibration") {
        return;
    }
    // Synthetic non-motion fixtures measured 1.00-1.74. If real photographs routinely
    // exceeded that band, the threshold would be calibrated against a fiction.
    let anisos: Vec<f64> = jpegs().iter().map(|f| metrics_for(f).anisotropy).collect();
    let mut sorted = anisos.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = sorted[sorted.len() / 2];
    let p95 = sorted[(sorted.len() as f64 * 0.95) as usize];

    assert!(
        p50 < 3.0,
        "median real-photo anisotropy is {p50:.3}, at or above the motion threshold of \
         3.0 — the calibration band from synthetic fixtures does not describe real files"
    );
    assert!(p95.is_finite());
}

// ---------------------------------------------------------------------------
// Real RAW files: classification and pairing
// ---------------------------------------------------------------------------
#[test]
fn real_raw_files_are_classified_as_raw() {
    if skip("real RAW classification") {
        return;
    }
    let files = raws();
    assert!(!files.is_empty(), "corpus dir exists but holds no RAW files");

    // Every real extension the corpus ships must be recognised. A RAW file that is
    // classified as `Other` is silently excluded from pairing and from the library —
    // the user simply never sees that photograph.
    // `use crate::ext::RAW_EXTS` mirror of the classifier. `.tif` is the one documented
    // exception: TIFF-wrapped raws (Kodak DCS and similar) classify as rendered images
    // on purpose, because most TIFFs a photographer owns are scans and exports.
    const DOCUMENTED_RASTER_WRAPPERS: &[&str] = &["tif", "tiff"];

    let mut unknown = Vec::new();
    for f in &files {
        let ext = f.extension().unwrap().to_string_lossy().to_ascii_lowercase();
        let expected = if DOCUMENTED_RASTER_WRAPPERS.contains(&ext.as_str()) {
            FileKind::Raster
        } else {
            FileKind::Raw
        };
        if classify(f) != expected {
            unknown.push(format!(
                "{} (expected {expected:?}, got {:?})",
                f.file_name().unwrap().to_string_lossy(),
                classify(f)
            ));
        }
    }
    assert!(
        unknown.is_empty(),
        "{} real RAW file(s) were not classified as Raw: {unknown:?}",
        unknown.len()
    );
}

#[test]
fn real_corpus_tree_resolves_into_groups_without_mis_pairing() {
    if skip("real RAW pairing") {
        return;
    }
    // The corpus is a flat directory of RAWs and a flat directory of JPEGs, so nothing
    // here should pair. That is the point: a resolver that invents pairs across
    // directories would be catastrophic on a real library, and this proves it does not.
    let mut all: Vec<PathBuf> = raws();
    all.extend(jpegs());

    let groups = resolve(&all);
    assert!(!groups.is_empty(), "resolving the real corpus produced no groups");

    for g in &groups {
        if g.state == GroupState::Pair {
            // Two files with the same stem in the same directory is a genuine pair, but
            // these directories hold unrelated files, so any pair found here is a false
            // positive worth reporting.
            panic!(
                "invented a pair across unrelated files: {:?} + {:?}",
                g.raws, g.rasters
            );
        }
        assert!(!g.needs_review() || g.state == GroupState::Ambiguous || !g.review.is_empty());
    }

    // Every real file the resolver was given must appear in exactly one group. Counting
    // groups alone is not enough: a file dropped by the classifier vanishes silently.
    let grouped: usize = groups.iter().map(|g| g.file_count()).sum();
    assert_eq!(
        grouped,
        all.len(),
        "every real file must land in exactly one group; {grouped} of {} did",
        all.len()
    );
    let raw_groups = groups.iter().filter(|g| !g.raws.is_empty()).count();
    assert!(
        raw_groups >= raws().len() - 1,
        "expected roughly one group per real RAW, got {raw_groups} from {}",
        raws().len()
    );
}

#[test]
fn a_real_raw_and_a_real_jpeg_with_a_shared_stem_form_a_pair() {
    if skip("real pair formation") {
        return;
    }
    // Construct the pairing case from real bytes rather than empty placeholders: copy a
    // real RAW and a real JPEG to one stem in a temp dir and confirm they pair. This is
    // the closest the suite gets to the user's actual workflow without touching their
    // library.
    let Some(raw) = raws().into_iter().next() else { return };
    let Some(jpg) = jpegs().into_iter().next() else { return };

    let tmp = tempfile::tempdir().expect("temp dir");
    let raw_dest = tmp.path().join(format!("IMG_0001.{}", raw.extension().unwrap().to_string_lossy()));
    let jpg_dest = tmp.path().join("IMG_0001.jpg");
    std::fs::copy(&raw, &raw_dest).expect("copy raw");
    std::fs::copy(&jpg, &jpg_dest).expect("copy jpeg");

    let groups = resolve([raw_dest.clone(), jpg_dest.clone()]);
    assert_eq!(groups.len(), 1, "a real RAW and JPEG sharing a stem must form one group");
    assert_eq!(
        groups[0].state,
        GroupState::Pair,
        "expected a Pair, got {:?} with review {:?}",
        groups[0].state,
        groups[0].review
    );
    assert_eq!(groups[0].file_count(), 2);

    // And the pair moves as a unit.
    let files = groups[0].all_files();
    assert!(files.contains(&raw_dest.as_path()) && files.contains(&jpg_dest.as_path()));
}

#[test]
fn real_files_yield_a_decodable_jpeg_but_raw_decoding_is_not_yet_implemented() {
    if skip("decode capability") {
        return;
    }
    // Honest documentation of the current boundary, as a test rather than a comment.
    //
    // JPEG decodes today. Real RAW decoding needs LibRaw and lands with issue #8. Until
    // then a RAW file is classified and paired correctly but cannot produce a preview —
    // which is why the thumbnail pipeline (#7) depends on #8.
    let Some(jpg) = jpegs().into_iter().next() else { return };
    assert!(image::open(&jpg).is_ok(), "a real corpus JPEG must decode");

    let Some(raw) = raws().into_iter().next() else { return };
    assert_eq!(
        classify(&raw),
        FileKind::Raw,
        "real RAW must classify as Raw"
    );
    // Deliberately not asserting that `image::open` fails: some DNGs are TIFF-based and
    // the image crate may partially succeed. The claim being tested is only that RAW is
    // correctly *identified*, not that it is decoded.
    let _ = prepare(&load(&jpg));
}

// ---------------------------------------------------------------------------
// Indexing real files end to end
// ---------------------------------------------------------------------------
#[test]
fn the_real_corpus_indexes_into_the_catalog() {
    if skip("real-corpus indexing") {
        return;
    }
    // The whole Wave 1 spine over real bytes: walk the tree, classify, resolve, store.
    // RAW and JPEG live in separate directories here, so nothing should pair — which is
    // itself the assertion that the resolver does not invent pairs across directories.
    use chaff_core::catalog::{store, open_in_memory};
    use chaff_core::indexer;

    let root = corpus_root().expect("corpus present");
    let mut conn = open_in_memory().expect("catalog");
    let outcome = indexer::index(&mut conn, &root, 1_700_000_000).expect("index");

    assert!(outcome.is_clean(), "unreadable entries: {:?}", outcome.unreadable);

    let expected_raw = raws().len();
    let expected_jpeg = jpegs().len();
    assert_eq!(
        outcome.scanned_files,
        expected_raw + expected_jpeg,
        "every real image should be scanned; MANIFEST.json must not be"
    );

    // The corpus contains one TIFF-wrapped raw (a Kodak DCS 3). It classifies as a
    // rendered image on purpose — most TIFFs a photographer owns are scans and exports —
    // so counts must split on the extension rather than on where the file came from.
    let tiff_wrapped = raws()
        .iter()
        .filter(|p| {
            let e = p.extension().unwrap().to_string_lossy().to_ascii_lowercase();
            e == "tif" || e == "tiff"
        })
        .count();
    let expected_raw_groups = expected_raw - tiff_wrapped;

    let photos = store::photos(&conn, outcome.library_id).expect("photos");
    assert_eq!(photos.len(), expected_raw + expected_jpeg);
    assert_eq!(
        photos.iter().filter(|p| p.state == "raw_only").count(),
        expected_raw_groups,
        "each real RAW is an orphan in this layout ({tiff_wrapped} TIFF-wrapped file(s) \
         classified as rendered images by design)"
    );
    assert_eq!(
        photos.iter().filter(|p| p.state == "raster_only").count(),
        expected_jpeg + tiff_wrapped
    );
    assert_eq!(
        photos.iter().filter(|p| p.state == "pair").count(),
        0,
        "no pair may be invented across unrelated directories"
    );

    // Every real RAW must be identified as raw, not merely present.
    let raws_stored = store::files_by_role(&conn, outcome.library_id, "raw").expect("raw files");
    assert_eq!(raws_stored.len(), expected_raw_groups);
    assert!(
        raws_stored.iter().all(|f| f.size_bytes > 0),
        "real files must carry real sizes"
    );
}

#[test]
fn re_indexing_the_real_corpus_changes_nothing() {
    if skip("real-corpus re-index") {
        return;
    }
    use chaff_core::catalog::{open_in_memory, store};
    use chaff_core::indexer;

    let root = corpus_root().expect("corpus present");
    let mut conn = open_in_memory().expect("catalog");

    let first = indexer::index(&mut conn, &root, 1_700_000_000).expect("first index");
    let before = store::photos(&conn, first.library_id).expect("photos");

    let second = indexer::index(&mut conn, &root, 1_700_000_100).expect("second index");
    let after = store::photos(&conn, second.library_id).expect("photos");

    assert_eq!(second.library_id, first.library_id, "the same folder is the same library");
    assert_eq!(second.stats.removed_files, 0, "nothing on disk changed");
    assert_eq!(second.stats.removed_photos, 0);
    assert_eq!(before, after, "a re-index must not alter the catalog");
}

// ---------------------------------------------------------------------------
// The full scoring pipeline over real photographs
// ---------------------------------------------------------------------------
#[test]
fn the_full_scoring_pipeline_runs_over_real_photographs() {
    if skip("full scoring pipeline") {
        return;
    }
    use chaff_core::scoring::shoot::{self, ALL_METRICS, FrameMeasurement};

    let files = jpegs();

    // Build one measurement per real photograph, the way the indexer will: decode,
    // measure focus and exposure, carry the EXIF identity, and hand the set to the
    // shoot normaliser.
    let measurements: Vec<FrameMeasurement> = files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let meas = measurements_for(path);
            let mut m = FrameMeasurement::from_focus(i as i64, "/corpus", &meas.focus);
            m.from_exposure(&meas.exposure);
            m.with_exif(chaff_core::exif::read(path).ok().and_then(|r| r.data().cloned()).as_ref())
        })
        .collect();

    assert_eq!(measurements.len(), files.len());

    let normalised = shoot::normalise(&measurements, shoot::DEFAULT_SHOOT_GAP_SECONDS);
    assert_eq!(normalised.len(), measurements.len());

    for n in &normalised {
        for metric in ALL_METRICS {
            let p = n.shoot_percentile(metric);
            assert!(
                p.is_finite() && (0.0..=100.0).contains(&p),
                "percentile for {metric:?} was {p}"
            );
            // `score_percentile` is `Option` because some metrics have no better
            // direction (a blown mean is not "better" than a correct one). When it is
            // Some it must be a usable percentage.
            if let Some(oriented) = n.score_percentile(metric) {
                assert!(
                    oriented.is_finite() && (0.0..=100.0).contains(&oriented),
                    "oriented score for {metric:?} was {oriented}"
                );
            }
        }
    }

    // Every raw measurement must be finite, or a percentile computed over it is
    // meaningless in a way that is hard to see.
    for m in &measurements {
        for metric in ALL_METRICS {
            let v = m.get(metric);
            assert!(v.is_finite(), "{metric:?} was {v} for photo {}", m.photo_id);
        }
    }

    // And the ranking must actually discriminate. A pipeline that returns the same
    // percentile for everything ranks nothing, however correct each frame is alone.
    let focuses: Vec<f64> = normalised.iter().map(|n| n.shoot_percentile(shoot::Metric::Focus)).collect();
    let min = focuses.iter().cloned().fold(f64::MAX, f64::min);
    let max = focuses.iter().cloned().fold(f64::MIN, f64::max);
    assert!(
        max - min > 50.0,
        "real photographs produced a degenerate focus ranking: {min:.1}..{max:.1}"
    );
}

#[test]
fn shoot_normalisation_over_the_real_corpus_is_deterministic() {
    if skip("shoot determinism on real files") {
        return;
    }
    use chaff_core::scoring::shoot::{self, FrameMeasurement};

    let build = || -> Vec<FrameMeasurement> {
        jpegs()
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let meas = measurements_for(path);
                let mut m = FrameMeasurement::from_focus(i as i64, "/corpus", &meas.focus);
                m.from_exposure(&meas.exposure);
                m
            })
            .collect()
    };

    let a = shoot::normalise(&build(), shoot::DEFAULT_SHOOT_GAP_SECONDS);
    let b = shoot::normalise(&build(), shoot::DEFAULT_SHOOT_GAP_SECONDS);
    assert_eq!(a, b, "the same photographs must rank the same on every run");
}

// ---------------------------------------------------------------------------
// Composite scoring and explanations over real photographs
// ---------------------------------------------------------------------------
fn real_scores() -> Vec<chaff_core::scoring::composite::Score> {
    use chaff_core::scoring::composite::{self, BandThresholds};
    use chaff_core::scoring::shoot::{self, FrameMeasurement};

    let measurements: Vec<FrameMeasurement> = jpegs()
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let meas = measurements_for(path);
            let mut m = FrameMeasurement::from_focus(i as i64, "/corpus", &meas.focus);
            m.from_exposure(&meas.exposure);
            m
        })
        .collect();

    let normalised = shoot::normalise(&measurements, shoot::DEFAULT_SHOOT_GAP_SECONDS);
    let preset = composite::default_preset();
    composite::score_all(&normalised, &preset.weights, &BandThresholds::default(), preset.name)
}

#[test]
#[ignore]
fn real_corpus_score_report() {
    if skip("score report") {
        return;
    }
    let mut scores = real_scores();
    scores.sort_by(|a, b| b.composite.partial_cmp(&a.composite).unwrap());

    println!("\n{:<8} {:>6} {:>7}  explanation", "photo", "score", "band");
    println!("{}", "-".repeat(96));
    for s in &scores {
        let lines = s.explain();
        println!("{:<8} {:>6.1} {:>7}  {}", format!("#{}", s.photo_id), s.composite, s.band.label(), lines[1].trim());
        for l in &lines[2..] {
            println!("{:<8} {:>6} {:>7}  {}", "", "", "", l.trim());
        }
    }

    let bands = |b| scores.iter().filter(|s| s.band == b).count();
    println!(
        "\nKeep {} | Review {} | Reject {}  (of {})",
        bands(chaff_core::scoring::composite::Band::Keep),
        bands(chaff_core::scoring::composite::Band::Review),
        bands(chaff_core::scoring::composite::Band::Reject),
        scores.len()
    );
    println!();
}

#[test]
fn real_photographs_get_scores_bands_and_explanations() {
    if skip("real scoring") {
        return;
    }
    use chaff_core::scoring::composite::BandThresholds;

    let scores = real_scores();
    assert_eq!(scores.len(), jpegs().len());

    for s in &scores {
        assert!(s.composite.is_finite() && (0.0..=100.0).contains(&s.composite));
        assert_eq!(s.terms.len(), 5, "every score must carry all five terms");
        assert!(BandThresholds::default().is_valid());

        // Every score must be explainable, and the explanation must account for the
        // number. A score that cannot be attributed is one the user cannot check.
        let lines = s.explain();
        assert!(lines.len() >= 3, "an explanation needs a verdict and at least one term");
        let sum: f64 = s.terms.iter().map(|t| t.contribution).sum();
        assert!(
            (sum - s.composite).abs() < 1e-9,
            "photo {}: contributions sum to {sum}, composite is {}",
            s.photo_id,
            s.composite
        );
    }

    // The bands must not all be the same, or the scoring is not separating anything.
    let distinct: std::collections::BTreeSet<&str> =
        scores.iter().map(|s| s.band.label()).collect();
    assert!(
        distinct.len() >= 2,
        "every real photograph landed in one band ({distinct:?}) — the composite is not \
         discriminating"
    );
}

#[test]
fn real_photograph_scoring_is_deterministic() {
    if skip("real scoring determinism") {
        return;
    }
    assert_eq!(real_scores(), real_scores());
}

// ---------------------------------------------------------------------------
// Embedded preview extraction over real raw files
// ---------------------------------------------------------------------------
#[test]
#[ignore]
fn real_raw_preview_report() {
    if skip("raw preview report") {
        return;
    }
    use chaff_core::preview::{extract, PreviewSource, DEFAULT_MIN_LONG_EDGE};

    println!("\n{:<58} {:>10} {:>14} {:>10}", "file", "verdict", "dimensions", "jpeg");
    println!("{}", "-".repeat(96));
    for path in raws() {
        let name = path.file_name().unwrap().to_string_lossy();
        let short: String = name.chars().take(56).collect();
        match extract(&path, DEFAULT_MIN_LONG_EDGE) {
            Ok(PreviewSource::Embedded(p)) => println!(
                "{short:<58} {:>10} {:>14} {:>9} KB",
                "embedded",
                format!("{}x{}", p.width, p.height),
                p.jpeg.len() / 1024
            ),
            Ok(PreviewSource::TooSmall { width, height }) => println!(
                "{short:<58} {:>10} {:>14} {:>10}",
                "too small",
                format!("{width}x{height}"),
                "-"
            ),
            Ok(PreviewSource::NeedsDecode) => {
                println!("{short:<58} {:>10} {:>14} {:>10}", "needs decode", "-", "-")
            }
            Err(e) => println!("{short:<58} ERROR {e}"),
        }
    }
    println!();
}

#[test]
fn real_raw_files_yield_embedded_previews_that_actually_decode() {
    if skip("raw preview extraction") {
        return;
    }
    use chaff_core::preview::{extract, PreviewSource, DEFAULT_MIN_LONG_EDGE};

    let files = raws();
    let mut embedded = 0usize;
    let mut decodable = 0usize;
    let mut malformed: Vec<String> = Vec::new();
    let mut too_small = 0usize;
    let mut needs_decode = 0usize;

    for path in &files {
        match extract(path, DEFAULT_MIN_LONG_EDGE).expect("extraction must not error on a real raw") {
            PreviewSource::Embedded(p) => {
                embedded += 1;

                assert!(
                    p.long_edge() >= DEFAULT_MIN_LONG_EDGE,
                    "{}: a preview below the threshold must not be returned as usable",
                    path.display()
                );
                assert!(p.jpeg.starts_with(&[0xFF, 0xD8]));
                assert!(p.jpeg.ends_with(&[0xFF, 0xD9]));

                // Whether it decodes is a separate question from whether it was found,
                // and the answer is not always yes. A structurally perfect header says
                // nothing about the entropy data behind it.
                match image::load_from_memory(&p.jpeg) {
                    Ok(decoded) => {
                        decodable += 1;
                        // A header walk that agreed with itself but not with a real
                        // decoder would pass every unit test and produce broken
                        // thumbnails in the product.
                        assert_eq!(
                            (decoded.width(), decoded.height()),
                            (p.width, p.height),
                            "{}: header said {}x{} but the decoder said {}x{}",
                            path.display(),
                            p.width,
                            p.height,
                            decoded.width(),
                            decoded.height()
                        );
                    }
                    Err(e) => malformed.push(format!(
                        "{} ({}x{}): {e}",
                        path.file_name().unwrap().to_string_lossy(),
                        p.width,
                        p.height
                    )),
                }
            }
            PreviewSource::TooSmall { .. } => too_small += 1,
            PreviewSource::NeedsDecode => needs_decode += 1,
        }
    }

    eprintln!(
        "raw previews: {embedded} embedded ({decodable} decodable, {} malformed), \
         {too_small} too small, {needs_decode} need a decode (of {} files)",
        malformed.len(),
        files.len()
    );
    for m in &malformed {
        eprintln!("   malformed preview: {m}");
    }

    // The fast path must actually carry weight, or every grid cell costs a full demosaic.
    assert!(
        embedded >= 5,
        "only {embedded} of {} real raw files yielded a usable embedded preview",
        files.len()
    );
    assert_eq!(
        embedded + too_small + needs_decode,
        files.len(),
        "every file must be accounted for by exactly one verdict"
    );

    // Most previews must decode, or the extraction is not finding real streams.
    assert!(
        decodable * 2 > embedded,
        "only {decodable} of {embedded} extracted previews decoded — the extraction is \
         finding streams that are not really there"
    );

    // And at least one real file has a preview that does not decode. That is not a
    // failure of this test: it is the reason `preview::extract` deliberately does not
    // claim the bytes are usable, and the reason the thumbnail pipeline walks the
    // candidate chain rather than trusting the largest stream.
    assert!(
        !malformed.is_empty(),
        "no malformed preview found in the corpus. If that is genuinely true now, the \
         'caller must validate' note in preview.rs is describing a hypothetical rather \
         than a real file — check before removing it."
    );
}

#[test]
fn the_candidate_chain_recovers_files_whose_largest_preview_is_broken() {
    if skip("preview fallback chain") {
        return;
    }
    use chaff_core::preview::{candidates, DEFAULT_MIN_LONG_EDGE};

    // The contract the thumbnail pipeline depends on: try each candidate in order and
    // take the first that decodes. This is the whole reason `candidates` exists rather
    // than only `extract`.
    let files = raws();
    let mut served_first_try = 0usize;
    let mut served_after_fallback = 0usize;
    let mut no_usable_preview = 0usize;

    for path in &files {
        let data = std::fs::read(path).expect("read raw");
        let cands = candidates(&data, DEFAULT_MIN_LONG_EDGE);
        if cands.is_empty() {
            no_usable_preview += 1;
            continue;
        }

        let mut served = false;
        for (i, c) in cands.iter().enumerate() {
            if let Ok(decoded) = image::load_from_memory(&c.jpeg) {
                assert_eq!(
                    (decoded.width(), decoded.height()),
                    (c.width, c.height),
                    "{}: candidate {i} header disagrees with the decoder",
                    path.display()
                );
                if i == 0 {
                    served_first_try += 1;
                } else {
                    served_after_fallback += 1;
                    eprintln!(
                        "   {}: largest candidate failed, fell back to candidate {i} \
                         ({}x{}, {} KB)",
                        path.file_name().unwrap().to_string_lossy(),
                        c.width,
                        c.height,
                        c.jpeg.len() / 1024
                    );
                }
                served = true;
                break;
            }
        }
        if !served {
            no_usable_preview += 1;
        }
    }

    eprintln!(
        "fast path with fallback: {served_first_try} on the first candidate, \
         {served_after_fallback} after falling back, {no_usable_preview} need a full decode \
         (of {} files)",
        files.len()
    );

    // The fallback must actually rescue at least one real file, or the chain is dead
    // weight and `candidates` should be `extract`.
    assert!(
        served_after_fallback > 0,
        "no corpus file needed the fallback chain. If that is genuinely true now, \
         `candidates` is unjustified complexity — check before keeping it."
    );
    assert!(
        served_first_try + served_after_fallback >= 6,
        "the fast path served only {} of {} raw files",
        served_first_try + served_after_fallback,
        files.len()
    );
}

#[test]
fn raw_preview_extraction_is_deterministic() {
    if skip("raw preview determinism") {
        return;
    }
    use chaff_core::preview::{extract, DEFAULT_MIN_LONG_EDGE};
    for path in raws().iter().take(4) {
        let a = extract(path, DEFAULT_MIN_LONG_EDGE).expect("extract");
        let b = extract(path, DEFAULT_MIN_LONG_EDGE).expect("extract");
        assert_eq!(a, b, "{} extracted differently on two runs", path.display());
    }
}
