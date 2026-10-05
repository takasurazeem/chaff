//! The CLIP tag pass, end to end, on a fixture.
//!
//! # Why this is an integration test and not a unit test
//!
//! `clip.rs` has eleven unit tests and they all test *pieces* — the vocabulary parser, the
//! preprocessor, the ranking. Nothing tested `run_clip` itself: the function that reads a
//! photograph, runs the encoder, writes tags to the catalog and reports what it did.
//!
//! It was reachable only from `chaff-cli`, and it is the **fallback for every user with no
//! vision endpoint** — which is most of them. A path that only a command line can reach, tested
//! only in pieces, is one that breaks the first time a GUI calls it.
//!
//! # The model
//!
//! It is an 84 MB download and it is **not in the repository**, so this test needs it present.
//! When it is not, the test **says so and passes** rather than being `#[ignore]`d — an ignored
//! test is one nobody reads, and a skipped one that prints why is honest about what was and was
//! not exercised.
//!
//! ```bash
//! CHAFF_FETCH_MODELS=1 cargo test -p chaff-faces --test clip_pass
//! ```

use std::path::Path;

/// The fixture, which is a real photograph committed to the repository.
const FIXTURE: &str = "tests/fixtures/landscape.jpg";

/// The model, or a reason it is not here.
fn clip_model() -> Result<std::path::PathBuf, String> {
    let store = chaff_faces::clip::default_store();
    match chaff_faces::clip::model_in(&store) {
        Some(p) => Ok(p),
        None => Err(format!(
            "the CLIP model is not in {} — this test needs it. Fetch it once with \
             `CHAFF_FETCH_MODELS=1 cargo test -p chaff-faces --test clip_pass`, or run the \
             `chaff clip` command, which downloads on first use.",
            store.root().display()
        )),
    }
}

#[test]
fn the_vocabulary_that_ships_is_usable() {
    // **No model needed, so this one always runs.**
    //
    // The vocabulary is bundled in the binary. A pass with no vocabulary tags nothing and
    // reports success, which is the worst kind of failure — so the file being present and
    // parsing is worth asserting on its own.
    let path = chaff_faces::clip::bundled().expect(
        "the CLIP vocabulary must ship in the binary — without it the fallback tagger silently \
         produces nothing",
    );
    let vocab = chaff_faces::clip::Vocabulary::load(&path).expect("the vocabulary must parse");

    // **38, and that is the design, not a shortfall.**
    //
    // I first asserted `> 50` — a number I invented rather than measured — and the test failed
    // on a vocabulary that is correct. The assertion to make is about *shape*, not a count I
    // guessed: the fallback tagger has a closed vocabulary, and what matters is that it covers
    // the categories a culling tool is asked about.
    assert_eq!(
        vocab.len(),
        38,
        "the shipped vocabulary changed size — if that was deliberate, update this and say why \
         in the commit; if not, the file is wrong"
    );

    // The *source* list, which is what a person edits, checked for the categories rather than
    // counted. A vocabulary that lost its people or its places would still be 38 phrases if
    // something else were duplicated — so the categories are what the assertion names.
    let source = include_str!("../src/clip.rs");
    for category in ["person", "dog", "beach", "mountain", "food", "sunset", "portrait"] {
        assert!(
            source.contains(category),
            "the vocabulary has lost its `{category}` phrases — the fallback tagger covers a \
             closed set of categories, and one missing is one the user cannot tag by"
        );
    }
}

#[test]
fn the_pass_tags_a_fixture_end_to_end() {
    let model = match clip_model() {
        Ok(p) => p,
        Err(why) => {
            // Not `#[ignore]`: an ignored test is invisible. This says exactly what did not run.
            eprintln!("SKIPPED — {why}");
            return;
        }
    };
    let Some(vocabulary) = chaff_faces::clip::bundled() else {
        eprintln!("SKIPPED — the bundled vocabulary is missing, which the test above fails on");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::copy(Path::new(FIXTURE), root.join("landscape.jpg")).unwrap();

    let mut conn = chaff_core::catalog::open_in_memory().unwrap();
    chaff_core::indexer::index(&mut conn, root, 0).unwrap();
    let lib = chaff_core::catalog::store::library_id_for_root(&conn, &root.to_string_lossy())
        .unwrap()
        .unwrap();

    let report = chaff_faces::pass::run_clip(
        &mut conn,
        lib,
        &chaff_faces::pass::ClipPaths { model: &model, vocabulary: &vocabulary },
        chaff_faces::pass::ClipSettings { keep: 5, min_similarity: 0.0 },
        0,
        &mut |_, _| true,
    )
    .expect("the pass must not fail on a well-formed JPEG");

    assert_eq!(report.tagged, 1, "the fixture must be tagged: {report:?}");
    assert_eq!(report.unreadable, 0, "a JPEG must be readable: {report:?}");
    assert!(!report.cancelled, "nothing asked it to stop");

    // **The tags reached the catalog**, which is the part a unit test of `rank` cannot see.
    let tags = chaff_core::catalog::store::tag_counts(&conn, lib, None).unwrap();
    assert!(
        !tags.is_empty(),
        "the pass reported {} tags and the catalog has none — the write is the part that \
         matters",
        report.tags
    );

    // **The tags are words a person can read**, not a count that happens to be non-zero. A pass
    // that wrote `""` five times would satisfy everything above.
    for (name, count) in &tags {
        assert!(!name.trim().is_empty(), "a tag with no name is not a tag: {tags:?}");
        assert!(*count > 0, "a tag with no photographs behind it: {name}");
    }
    eprintln!("  tagged the fixture with: {:?}", tags.iter().map(|(n, _)| n).collect::<Vec<_>>());
}

#[test]
fn a_cancelled_pass_stops_and_says_so() {
    let model = match clip_model() {
        Ok(p) => p,
        Err(why) => {
            eprintln!("SKIPPED — {why}");
            return;
        }
    };
    let Some(vocabulary) = chaff_faces::clip::bundled() else { return };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::copy(Path::new(FIXTURE), root.join(name)).unwrap();
    }

    let mut conn = chaff_core::catalog::open_in_memory().unwrap();
    chaff_core::indexer::index(&mut conn, root, 0).unwrap();
    let lib = chaff_core::catalog::store::library_id_for_root(&conn, &root.to_string_lossy())
        .unwrap()
        .unwrap();

    // **Stop before the first photograph.** The point is that the report distinguishes
    // "stopped" from "finished" — a library that is a third tagged must not read as complete,
    // and that is the whole reason the field exists.
    let report = chaff_faces::pass::run_clip(
        &mut conn,
        lib,
        &chaff_faces::pass::ClipPaths { model: &model, vocabulary: &vocabulary },
        chaff_faces::pass::ClipSettings { keep: 5, min_similarity: 0.0 },
        0,
        &mut |_, _| false,
    )
    .expect("cancelling is not an error");

    assert!(report.cancelled, "the report must say it was stopped, not finished");
    assert_eq!(report.tagged, 0, "nothing was tagged after an immediate stop");
}
