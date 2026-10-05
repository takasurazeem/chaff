//! The VLM client against a real endpoint.
//!
//! # Why these are separate from the unit tests
//!
//! The unit tests in `vlm.rs` cover base64, the schema, the prompt and URL building — the
//! parts that are wrong in a way reading can catch. They cannot tell you whether a real
//! model accepts the request, honours `response_format`, or puts its answer where the parser
//! looks. Only a live endpoint can, and that is what these do.
//!
//! # They skip, visibly, when there is no endpoint
//!
//! CI has no GPU. A test that failed there would be turned off, and a test that passed by
//! not running is worse. So these print why they are skipping and return.

use chaff_core::vlm::{self, Endpoint, TagRequest};

/// The endpoint to test against, from the environment.
///
/// `CHAFF_VLM` — e.g. `http://192.168.1.150:8080`. Unset means skip.
fn endpoint() -> Option<Endpoint> {
    let base = std::env::var("CHAFF_VLM").ok()?;
    let model = std::env::var("CHAFF_VLM_MODEL").unwrap_or_else(|_| "chaff-vlm".into());
    Some(Endpoint { base, model })
}

/// A fixture, downscaled and re-encoded the way the client sends one.
fn fixture_jpeg() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../chaff-faces/tests/fixtures/portrait_mona_lisa.jpg");
    let img = image::open(&path).expect("the committed fixture").to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
        .expect("encode");
    out
}

#[test]
fn a_live_endpoint_answers_a_health_check() {
    let Some(e) = endpoint() else {
        eprintln!("SKIP: set CHAFF_VLM to run the live tests");
        return;
    };
    match vlm::health(&e, 10) {
        Ok(()) => {}
        Err(err) => panic!("{err}"),
    }
}

#[test]
fn a_real_model_returns_parseable_tags() {
    let Some(e) = endpoint() else {
        eprintln!("SKIP: set CHAFF_VLM to run the live tests");
        return;
    };

    let result = vlm::tag(
        &e,
        &TagRequest { image: fixture_jpeg(), vocabulary: None, extra_instructions: None },
        300,
    )
    .expect("a tag result");

    // **The property that matters is that it parses at all.** Everything downstream — the
    // catalog, the filter, the batch runner — assumes a `TagResult`, and the failure this
    // guards is a model whose reply the schema did not constrain, producing prose that the
    // parser rejects on some photographs and not others.
    assert!(!result.tags.is_empty(), "a photograph of a face must yield tags");
    assert!(
        result.tags.len() <= 12,
        "the prompt asks for 3 to 8; got {}",
        result.tags.len()
    );
    for t in &result.tags {
        assert!(!t.name.trim().is_empty(), "an empty tag name is not a tag");
        assert!(
            (0.0..=1.0).contains(&t.confidence),
            "confidence {} is outside the schema's range",
            t.confidence
        );
    }
    assert!(
        result.description.as_deref().is_some_and(|d| d.split_whitespace().count() >= 3),
        "the description should be a sentence, not a tag list: {:?}",
        result.description
    );
    assert!(result.completion_tokens > 0, "the usage block must be reported");
}

#[test]
fn a_vocabulary_is_actually_honoured() {
    // The schema has an `enum`, and whether a given server enforces it is exactly the sort
    // of thing that can only be checked against the server.
    let Some(e) = endpoint() else {
        eprintln!("SKIP: set CHAFF_VLM to run the live tests");
        return;
    };

    let vocabulary = vec![
        "person".to_string(),
        "landscape".to_string(),
        "building".to_string(),
        "animal".to_string(),
    ];
    let result = vlm::tag(
        &e,
        &TagRequest {
            image: fixture_jpeg(),
            vocabulary: Some(vocabulary.clone()),
            extra_instructions: None,
        },
        300,
    )
    .expect("a tag result");

    for t in &result.tags {
        assert!(
            vocabulary.contains(&t.name),
            "the model returned {:?}, which is not in the vocabulary it was given",
            t.name
        );
    }
}

#[test]
fn tagging_is_deterministic_at_temperature_zero() {
    // **Why `temperature: 0` is not a detail.** A re-run that produces different tags
    // silently rewrites the library, and the user has no way to tell a changed model from a
    // changed opinion.
    let Some(e) = endpoint() else {
        eprintln!("SKIP: set CHAFF_VLM to run the live tests");
        return;
    };
    let image = fixture_jpeg();
    let request = TagRequest { image, vocabulary: None, extra_instructions: None };

    let a = vlm::tag(&e, &request, 300).expect("first");
    let b = vlm::tag(&e, &request, 300).expect("second");

    let names = |r: &vlm::TagResult| {
        let mut v: Vec<String> = r.tags.iter().map(|t| t.name.to_lowercase()).collect();
        v.sort();
        v
    };
    assert_eq!(names(&a), names(&b), "the same photograph tagged twice must give the same tags");
}
