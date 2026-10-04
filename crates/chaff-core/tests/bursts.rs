//! Burst and bracket grouping against real image files.
//!
//! The unit tests in `src/burst.rs` work on constructed frames, which is right for
//! testing the logic. This file runs the same logic over actual JPEGs and actual EXIF,
//! because the two signals grouping depends on — a perceptual hash and an exposure
//! time — both come from real decoding rather than from a literal.
//!
//! Everything here uses `fixtures/synthetic/`, which is generated, committed and
//! offline. No personal photograph is read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chaff_core::burst::{
    group_into_bursts, perceptual_hash, select_keepers, BurstConfig, BurstFrame, BurstKind,
};
use chaff_core::exif;
use chaff_core::imaging::Luma;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic/images")
        .join(format!("{name}.jpg"))
}

fn load(name: &str) -> Luma {
    let path = fixture_path(name);
    let img = image::open(&path)
        .unwrap_or_else(|e| panic!("could not open {}: {e}", path.display()))
        .to_luma8();
    Luma::from_gray8(img.width() as usize, img.height() as usize, img.as_raw())
}

/// Build a `BurstFrame` the way the indexer will: decode, hash, read EXIF.
fn frame_for(photo_id: i64, name: &str, capture_time: i64) -> BurstFrame {
    let path = fixture_path(name);
    if !path.is_file() {
        eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
        return BurstFrame::new(photo_id);
    }
    let luma = load(name);
    let exif = exif::read(&path).ok().and_then(|r| r.data().cloned());

    let mut f = BurstFrame::new(photo_id)
        .with_hash(Some(perceptual_hash(&luma)))
        .with_time(Some(capture_time))
        .with_exif(exif.as_ref());
    // The fixtures all carry one camera, but the fixture EXIF is shared, so the capture
    // time is supplied by the caller to model a sequence.
    f.captured_at = Some(capture_time);
    f
}

fn all_present(names: &[&str]) -> bool {
    names.iter().all(|n| fixture_path(n).is_file())
}

#[test]
fn a_real_burst_groups_into_one_burst() {
    let names = [
        "burst_0_sharp",
        "burst_1_sharp",
        "burst_2_sharp",
        "burst_3_smeared",
        "burst_4_sharp",
    ];
    if !all_present(&names) {
        eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
        return;
    }

    // One second apart, as a held shutter produces.
    let frames: Vec<BurstFrame> = names
        .iter()
        .enumerate()
        .map(|(i, n)| frame_for(i as i64, n, 1_000_000 + i as i64))
        .collect();

    let bursts = group_into_bursts(&frames, &BurstConfig::default());
    assert_eq!(
        bursts.len(),
        1,
        "the five burst frames are one moment and must group together, got {:?}",
        bursts.iter().map(|b| b.size()).collect::<Vec<_>>()
    );
    assert_eq!(bursts[0].kind, BurstKind::Burst);
}

#[test]
fn the_hashes_of_a_real_burst_are_close_and_a_different_scene_is_not() {
    // The property grouping rests on, measured on real decoded pixels rather than
    // asserted about a literal.
    if !all_present(&["burst_0_sharp", "burst_4_sharp", "bokeh_portrait", "flat_low_contrast"]) {
        eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
        return;
    }

    let h0 = perceptual_hash(&load("burst_0_sharp"));
    let h4 = perceptual_hash(&load("burst_4_sharp"));
    let hb = perceptual_hash(&load("bokeh_portrait"));
    let hf = perceptual_hash(&load("flat_low_contrast"));

    let within = chaff_core::burst::hash_distance(h0, h4);
    let across_a = chaff_core::burst::hash_distance(h0, hb);
    let across_b = chaff_core::burst::hash_distance(h0, hf);

    assert!(
        within <= BurstConfig::default().max_hash_distance,
        "frames of one burst must be within the threshold, got {within}"
    );
    assert!(
        across_a > BurstConfig::default().max_hash_distance
            || across_b > BurstConfig::default().max_hash_distance,
        "at least one unrelated scene must fall outside the threshold, or the hash does \
         not discriminate: bokeh {across_a}, flat {across_b}"
    );
}

#[test]
fn a_real_bracket_is_detected_and_never_culled() {
    let names = ["bracket_m1", "bracket_0", "bracket_p1"];
    if !all_present(&names) {
        eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
        return;
    }

    // A bracket fires faster than a burst, so the frames are fractions of a second apart.
    let frames: Vec<BurstFrame> = names
        .iter()
        .enumerate()
        .map(|(i, n)| frame_for(i as i64, n, 2_000_000 + i as i64))
        .collect();

    // The fixture EXIF must actually carry the stepped exposures, or this proves nothing.
    let exposures: Vec<Option<f64>> = frames.iter().map(|f| f.exposure_time).collect();
    assert!(
        exposures.iter().all(Option::is_some),
        "the bracket fixtures must carry exposure times, got {exposures:?}"
    );

    let bursts = group_into_bursts(&frames, &BurstConfig::default());
    assert_eq!(bursts.len(), 1, "a bracket is one group");
    assert_eq!(
        bursts[0].kind,
        BurstKind::Bracket,
        "a one-stop exposure sequence must be recognised as a bracket, not culled as a \
         burst. exposures: {exposures:?}"
    );

    let scores: HashMap<i64, f64> = [(0, 90.0), (1, 50.0), (2, 10.0)].into_iter().collect();
    let sel = select_keepers(
        &bursts[0],
        &frames,
        &scores,
        &BurstConfig::default(),
        &HashSet::new(),
    );
    assert_eq!(sel.keepers.len(), 3, "every bracket frame is kept");
    assert!(sel.redundant.is_empty());
}

#[test]
fn a_plain_burst_beside_a_bracket_is_treated_differently() {
    // The contrast that makes the feature worth having: two groups that look identical
    // in a grid, one of which must be culled and one of which must not.
    let burst_names = ["burst_0_sharp", "burst_1_sharp", "burst_2_sharp", "burst_3_smeared"];
    let bracket_names = ["bracket_m1", "bracket_0", "bracket_p1"];
    if !all_present(&burst_names) || !all_present(&bracket_names) {
        eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
        return;
    }

    // Same camera, same second, different scenes -> the hash must separate them.
    let mut frames: Vec<BurstFrame> = burst_names
        .iter()
        .enumerate()
        .map(|(i, n)| frame_for(i as i64, n, 3_000_000 + i as i64))
        .collect();
    frames.extend(
        bracket_names
            .iter()
            .enumerate()
            .map(|(i, n)| frame_for(100 + i as i64, n, 3_000_010 + i as i64)),
    );

    let bursts = group_into_bursts(&frames, &BurstConfig::default());
    let brackets = bursts.iter().filter(|b| b.kind == BurstKind::Bracket).count();
    assert!(
        brackets >= 1,
        "the bracket must still be recognised alongside a burst, got {:?}",
        bursts.iter().map(|b| (b.size(), b.kind)).collect::<Vec<_>>()
    );

    let scores: HashMap<i64, f64> =
        frames.iter().map(|f| (f.photo_id, 50.0 + f.photo_id as f64)).collect();
    for b in &bursts {
        let sel = select_keepers(b, &frames, &scores, &BurstConfig::default(), &HashSet::new());
        if b.kind == BurstKind::Bracket {
            assert_eq!(sel.keepers.len(), b.size(), "a bracket keeps everything");
        } else if b.size() > 2 {
            assert!(
                sel.keepers.len() < b.size(),
                "a real burst of {} must be culled",
                b.size()
            );
        }
    }
}

#[test]
fn grouping_real_files_is_deterministic() {
    let names = ["burst_0_sharp", "burst_1_sharp", "burst_2_sharp"];
    if !all_present(&names) {
        return;
    }
    let build = || -> Vec<BurstFrame> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| frame_for(i as i64, n, 1_000_000 + i as i64))
            .collect()
    };
    assert_eq!(
        group_into_bursts(&build(), &BurstConfig::default()),
        group_into_bursts(&build(), &BurstConfig::default())
    );
}
