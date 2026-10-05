//! The face pass, end to end, on a fixture.
//!
//! # Why this exists beside `faces.rs`
//!
//! `faces.rs` has eleven tests and **not one calls `pass::run`** — the function that walks a
//! library, detects faces, embeds them, groups them into people and writes all of it to the
//! catalog. Every test there covers a piece: detection on one image, determinism, the licence.
//!
//! That is the same gap `clip_pass.rs` was written to close, and it matters for the same reason:
//! `pass::run` is what a GUI calls. A path tested only in pieces breaks the first time it is
//! called as a whole — and the FFI had to be written twice for exactly that.
//!
//! # The models
//!
//! YuNet ships in the repository at 233 KB. SFace is a 38 MB download, and when it is absent the
//! tests that need it **say so and pass** rather than being `#[ignore]`d — an ignored test is one
//! nobody reads.

use std::path::Path;

/// A photograph with exactly one face in it.
const PORTRAIT: &str = "tests/fixtures/portrait_mona_lisa.jpg";
/// A photograph with none.
const LANDSCAPE: &str = "tests/fixtures/landscape.jpg";

/// A library with the given fixtures in it, and its id.
fn library_with(names: &[&str]) -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let file = name.rsplit('/').next().unwrap();
        std::fs::copy(Path::new(name), dir.path().join(file)).unwrap();
    }
    let mut conn = chaff_core::catalog::open_in_memory().unwrap();
    chaff_core::indexer::index(&mut conn, dir.path(), 0).unwrap();
    let lib = chaff_core::catalog::store::library_id_for_root(&conn, &dir.path().to_string_lossy())
        .unwrap()
        .unwrap();
    (dir, conn, lib)
}

/// How many faces the catalog holds for a library.
///
/// There is no `store::faces(library)` — faces belong to photographs, and the catalog is asked
/// per photograph. Summing is what a count *is* here, and writing it out rather than adding a
/// convenience accessor keeps the test from needing the engine to grow a method only tests use.
fn faces_in(conn: &rusqlite::Connection, library_id: i64) -> Vec<chaff_core::catalog::store::FaceRow> {
    let photos = chaff_core::catalog::store::photos(conn, library_id).unwrap();
    photos
        .iter()
        .flat_map(|p| chaff_core::catalog::store::faces_for_photo(conn, p.id).unwrap_or_default())
        .collect()
}

/// Whether the recogniser is where **the pass** looks for it.
///
/// I first checked the repository's model directory and the test failed on a run that had
/// plainly embedded a face. The pass looks in `<app_data>/models`, and it has a fallback to the
/// bundled store — so a check written from the outside was checking the wrong place and would
/// have gone on being wrong quietly.
///
/// `pass::model_store` is the answer to "where does it look", so this asks it.
fn recogniser_available(app_data: &Path) -> bool {
    let store = chaff_faces::pass::model_store(app_data);
    if store.path_for(&chaff_faces::models::SFACE).is_file() {
        return true;
    }
    // The bundled fallback, which is what the pass reaches for when the app data has none.
    chaff_faces::clip::default_store()
        .path_for(&chaff_faces::models::SFACE)
        .is_file()
}

#[test]
fn the_pass_detects_a_face_and_writes_it_to_the_catalog() {
    let (dir, mut conn, lib) = library_with(&[PORTRAIT]);
    let app_data = dir.path().join("appdata");
    std::fs::create_dir_all(&app_data).unwrap();

    let report = chaff_faces::pass::run(&mut conn, &app_data, lib, 0, &mut |_, _| true)
        .expect("the pass must not fail on a well-formed JPEG");

    // **Detection and embedding are separate stages**, and the report says so — a library can
    // have faces found and none embedded, which looks like a bug from the outside.
    assert_eq!(report.detected_files, 1, "the portrait must be examined: {report:?}");
    assert_eq!(report.faces_found, 1, "the Mona Lisa has exactly one face: {report:?}");
    assert_eq!(report.unreadable, 0, "a JPEG must be readable: {report:?}");
    assert!(!report.cancelled);

    // **And it reached the catalog**, which is the part a unit test of detection cannot see.
    let rows = faces_in(&conn, lib);
    assert_eq!(
        rows.len(),
        1,
        "the pass reported {} faces and the catalog has {} — the write is the part that matters",
        report.faces_found,
        rows.len()
    );
}

#[test]
fn a_photograph_with_no_face_is_not_an_error() {
    // **The common case.** Most photographs in most libraries have no face, and a pass that
    // treated that as a failure would fail on nearly every file.
    let (dir, mut conn, lib) = library_with(&[LANDSCAPE]);
    let app_data = dir.path().join("appdata");
    std::fs::create_dir_all(&app_data).unwrap();

    let report = chaff_faces::pass::run(&mut conn, &app_data, lib, 0, &mut |_, _| true)
        .expect("no faces is a normal outcome");

    assert_eq!(report.detected_files, 1, "the file was examined: {report:?}");
    assert_eq!(report.faces_found, 0, "a landscape has no face: {report:?}");
    assert!(faces_in(&conn, lib).is_empty());
}

#[test]
fn a_cancelled_pass_stops_and_says_so() {
    // **"Stopped" is not "finished."** A library that is a third grouped must not read as
    // complete, and that is the whole reason the field exists.
    let (dir, mut conn, lib) = library_with(&[PORTRAIT, LANDSCAPE]);
    let app_data = dir.path().join("appdata");
    std::fs::create_dir_all(&app_data).unwrap();

    let report = chaff_faces::pass::run(&mut conn, &app_data, lib, 0, &mut |_, _| false)
        .expect("cancelling is not an error");

    assert!(report.cancelled, "the report must say it was stopped, not finished");
    assert_eq!(report.detected_files, 0, "nothing was examined after an immediate stop");
}

#[test]
fn running_the_pass_twice_does_not_duplicate_faces() {
    // **Idempotence, which is what makes a resumable pass safe.** Every pass is resumable by
    // design — the work list is the catalog — so a second run must converge rather than
    // accumulate. A pass that appended would double the face count on every run, and the
    // symptom would be clusters growing without anything new being photographed.
    let (dir, mut conn, lib) = library_with(&[PORTRAIT]);
    let app_data = dir.path().join("appdata");
    std::fs::create_dir_all(&app_data).unwrap();

    let first = chaff_faces::pass::run(&mut conn, &app_data, lib, 0, &mut |_, _| true).unwrap();
    let after_first = faces_in(&conn, lib).len();

    let second = chaff_faces::pass::run(&mut conn, &app_data, lib, 0, &mut |_, _| true).unwrap();
    let after_second = faces_in(&conn, lib).len();

    assert_eq!(
        after_first, after_second,
        "the second pass changed the face count from {after_first} to {after_second} — the pass \
         must converge, not accumulate. First: {first:?}, second: {second:?}"
    );
    assert_eq!(
        second.detected_files, 0,
        "the second pass had nothing to detect — the first already did it: {second:?}"
    );
}

#[test]
fn grouping_needs_the_recogniser_and_says_so_when_it_is_absent() {
    // The grouping stage is what turns faces into **people**, and it needs the 38 MB
    // recogniser. Detection does not — which is why a library can show faces and no groups,
    // and why the report distinguishes them.
    let (dir, mut conn, lib) = library_with(&[PORTRAIT]);
    let app_data = dir.path().join("appdata");
    std::fs::create_dir_all(&app_data).unwrap();

    let report = chaff_faces::pass::run(&mut conn, &app_data, lib, 0, &mut |_, _| true).unwrap();

    if recogniser_available(&app_data) {
        assert!(
            report.embedded > 0,
            "the recogniser is present, so the face must be embedded: {report:?}"
        );
    } else {
        eprintln!(
            "NOTE — the recogniser is absent, so `embedded` is {} and no people were grouped. \
             Detection is verified; grouping is not.",
            report.embedded
        );
        assert_eq!(report.embedded, 0, "nothing can be embedded without a recogniser");
    }

    // **And the report's own account of the two stages.** Whatever the recogniser situation,
    // `embedded` can never exceed `faces_found` — a pass claiming to have embedded more faces
    // than it found is arithmetic that does not close, and it is the kind of number a UI shows
    // without anyone checking.
    assert!(
        report.embedded <= report.faces_found,
        "embedded {} of {} faces found: {report:?}",
        report.embedded,
        report.faces_found
    );
}
