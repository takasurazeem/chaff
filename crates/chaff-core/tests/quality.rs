//! Quality percentiles: the data behind "show me the soft ones".
//!
//! # Why these tests are about *absence* as much as value
//!
//! Every one of these metrics is optional. A photograph the engine could not decode has no focus
//! score, and **that is not the same as a low one** — treating it as zero would file every raw
//! this build cannot read under "blurry", which is a claim nobody made and a filter that hides
//! real photographs.

use chaff_core::catalog::store;

/// A library with `n` photographs and a score row for each metric given.
fn library(n: usize, metrics: &[(&str, f64)]) -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        std::fs::write(dir.path().join(format!("IMG_{i:04}.JPG")), b"pretend jpeg").unwrap();
    }
    let mut conn = chaff_core::catalog::open_in_memory().unwrap();
    chaff_core::indexer::index(&mut conn, dir.path(), 0).unwrap();
    let lib = store::library_id_for_root(&conn, &dir.path().to_string_lossy())
        .unwrap()
        .unwrap();

    let version = chaff_core::pipeline::SCORER_VERSION;
    for photo in store::photos(&conn, lib).unwrap() {
        for (metric, value) in metrics {
            conn.execute(
                "INSERT INTO score (photo_id, metric, value, scorer_version, computed_at)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                rusqlite::params![photo.id, metric, value, version],
            )
            .unwrap();
        }
    }
    (dir, conn, lib)
}

#[test]
fn a_library_with_no_scores_has_no_quality_rather_than_zeroes() {
    // The state before a scoring pass, and the one a filter is most likely to get wrong.
    let (_d, conn, lib) = library(3, &[]);
    let q = store::quality_for_library(&conn, lib, chaff_core::pipeline::SCORER_VERSION)
        .unwrap();
    assert!(q.is_empty(), "no scores means no quality rows at all: {q:?}");
}

#[test]
fn each_metric_lands_in_its_own_field() {
    let (_d, conn, lib) = library(1, &[("focus", 12.0), ("noise", 88.0), ("detail", 40.0)]);
    let q = store::quality_for_library(&conn, lib, chaff_core::pipeline::SCORER_VERSION)
        .unwrap();
    let one = q.values().next().expect("one photograph");
    assert_eq!(one.focus, Some(12.0));
    assert_eq!(one.noise, Some(88.0));
    assert_eq!(one.detail, Some(40.0));
}

#[test]
fn a_missing_metric_is_none_and_not_zero() {
    // **The distinction the whole feature rests on.** A photograph with a focus score and no
    // noise score is one where noise could not be measured — not one with no noise. Filtering it
    // as "0" would put it under "clean", which is a claim nobody made.
    let (_d, conn, lib) = library(1, &[("focus", 55.0)]);
    let q = store::quality_for_library(&conn, lib, chaff_core::pipeline::SCORER_VERSION)
        .unwrap();
    let one = q.values().next().unwrap();
    assert_eq!(one.focus, Some(55.0));
    assert_eq!(one.noise, None, "an unmeasured metric is absent, not zero");
    assert_ne!(one.noise, Some(0.0), "and specifically not zero");
}

#[test]
fn only_the_current_scorer_version_is_read() {
    // Scores from an older scorer are **kept on purpose**, so a regression can be diagnosed. A
    // query that read all versions would return two rows per metric and the last one written
    // would win — which is whichever the database happened to return, not whichever is current.
    let (_d, conn, lib) = library(1, &[]);
    let version = chaff_core::pipeline::SCORER_VERSION;
    let photo = store::photos(&conn, lib).unwrap()[0].id;

    for (v, value) in [(version - 1, 90.0), (version, 10.0)] {
        conn.execute(
            "INSERT INTO score (photo_id, metric, value, scorer_version, computed_at)
             VALUES (?1, 'focus', ?2, ?3, 0)",
            rusqlite::params![photo, value, v],
        )
        .unwrap();
    }

    let q = store::quality_for_library(&conn, lib, version).unwrap();
    assert_eq!(
        q[&photo].focus,
        Some(10.0),
        "the current scorer's number, not the older one"
    );
}

#[test]
fn an_unknown_metric_is_ignored_rather_than_misplaced() {
    // `composite` is in the same table and is not a quality signal. A query that took every
    // metric would put the overall score into whichever field it was matched against.
    let (_d, conn, lib) = library(1, &[("composite", 99.0), ("focus", 5.0)]);
    let q = store::quality_for_library(&conn, lib, chaff_core::pipeline::SCORER_VERSION)
        .unwrap();
    let one = q.values().next().unwrap();
    assert_eq!(one.focus, Some(5.0), "focus is focus");
    assert_eq!(one.detail, None, "the composite must not land in another field");
    assert_eq!(one.noise, None);
}
