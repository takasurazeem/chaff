//! Detection against real photographs.
//!
//! The unit tests in `engine.rs` cover the decode arithmetic and the letterbox in
//! isolation. These run the model over actual images, because the arithmetic being right
//! and the model finding a face are different claims.
//!
//! Fixtures are public domain (a Wikimedia painting) and committed. No personal photograph
//! is read, here or anywhere in this project.

use std::path::Path;

use chaff_faces::{Detector, FaceEngine, YuNet};

/// A committed fixture, which must be there.
///
/// **Not an `Option`.** The first version returned `None` for a missing fixture and every
/// test using it quietly skipped — including the negative control, which then proved
/// nothing while reporting success. A fixture that is committed is not optional; if it is
/// gone, that is a failure and should look like one.
fn fixture(name: &str) -> std::path::PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    assert!(
        p.is_file(),
        "the committed fixture {name} is missing — the test cannot prove anything without it"
    );
    p
}

/// The engine, or `None` when the model is absent.
///
/// Optional on purpose: face detection is an enhancement, and the rest of the application
/// works without it. A *missing model* is a supported state; a missing fixture is not.
fn engine() -> Option<YuNet> {
    let model = YuNet::bundled()?;
    YuNet::from_file(&model).ok()
}

#[test]
fn a_portrait_yields_exactly_one_face() {
    let img = fixture("portrait_mona_lisa.jpg");
    let Some(engine) = engine() else {
        eprintln!("SKIP: the model is absent");
        return;
    };
    let img = image::open(&img).expect("decode").to_rgb8();
    let faces = engine.detect(img.as_raw(), img.width(), img.height()).expect("inference");

    assert_eq!(faces.len(), 1, "a single portrait must yield a single face");

    let f = faces[0];
    assert!(f.confidence > 0.8, "confidence was only {}", f.confidence);
    // The face is in the upper half of the painting and well inside the frame. A box in a
    // corner, or larger than the image, means the decode is wrong in a way the count alone
    // would not reveal.
    assert!(f.x > 0.0 && f.y > 0.0, "the box must be inside the image");
    assert!(f.x + f.width < img.width() as f32, "and not past its right edge");
    assert!(f.y + f.height < img.height() as f32, "nor its bottom");
    assert!(f.width > 20.0 && f.height > 20.0, "a face is not four pixels across");
    assert!(f.area() < (img.width() * img.height()) as f32 / 2.0, "nor half the frame");
}

#[test]
fn a_landscape_yields_none() {
    // **The negative control.** A detector that finds faces in mountains is worse than no
    // detector: it would invent people, and the whole feature is about grouping them.
    let img = fixture("landscape.jpg");
    let Some(engine) = engine() else {
        eprintln!("SKIP: the model is absent");
        return;
    };
    let img = image::open(&img).expect("decode").to_rgb8();
    let faces = engine.detect(img.as_raw(), img.width(), img.height()).expect("inference");
    assert!(faces.is_empty(), "found {} face(s) in a landscape", faces.len());
}

#[test]
fn detection_is_deterministic() {
    // The pipeline caches results and compares runs; a detector that returned different
    // boxes each time would make every re-index look like the library changed.
    let img = fixture("portrait_mona_lisa.jpg");
    let Some(engine) = engine() else { return };
    let img = image::open(&img).expect("decode").to_rgb8();
    let a = engine.detect(img.as_raw(), img.width(), img.height()).unwrap();
    let b = engine.detect(img.as_raw(), img.width(), img.height()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn the_engine_reports_its_licence() {
    // Faces are biometric data and the models that read them carry different licences. The
    // default is MIT; the more accurate alternative is non-commercial and must say so where
    // it is chosen rather than in a README.
    let Some(engine) = engine() else { return };
    assert_eq!(FaceEngine::name(&engine), "YuNet (OpenCV Zoo)");
    assert_eq!(engine.licence(), "MIT");
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

/// The recogniser, if the model has been fetched.
///
/// Not committed — 38 MB — so these tests skip when it is absent, and say so. That is
/// honest: the alternative is a test that passes by not running.
fn recogniser() -> Option<chaff_faces::Recogniser> {
    let path = std::env::var("CHAFF_SFACE").ok().map(std::path::PathBuf::from)?;
    if !path.is_file() {
        return None;
    }
    chaff_faces::Recogniser::from_file(&path).ok()
}

fn detect_one(engine: &YuNet, img: &image::RgbImage) -> Option<chaff_faces::Detection> {
    let faces = engine.detect(img.as_raw(), img.width(), img.height()).ok()?;
    faces.into_iter().next()
}

/// The same image, resized and shifted — a different photograph of the same face.
///
/// Stands in for the real case, which is two frames from one shoot: the face is in a
/// different place and a different size, and everything downstream has to treat it as the
/// same person.
fn transformed(img: &image::RgbImage, scale: u32) -> image::RgbImage {
    let (w, h) = (img.width() * scale, img.height() * scale);
    let mut out = image::RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // A shift as well as a scale, so the alignment has rotation and translation to
            // undo and not only a resize.
            let sx = (x / scale + 17).min(img.width() - 1);
            let sy = (y / scale + 11).min(img.height() - 1);
            out.put_pixel(x, y, *img.get_pixel(sx, sy));
        }
    }
    out
}

#[test]
fn one_face_embeds_to_the_same_place_after_moving_and_resizing() {
    // **The property the whole grouping feature rests on.** If alignment did not work, the
    // same face in a different position would embed somewhere else, and the clusters would
    // be noise — every photograph its own person.
    let (Some(engine), Some(rec)) = (engine(), recogniser()) else {
        eprintln!("SKIP: the SFace model is not fetched (set CHAFF_SFACE)");
        return;
    };
    let img = image::open(fixture("portrait_mona_lisa.jpg")).unwrap().to_rgb8();

    let original = detect_one(&engine, &img).expect("a face in the original");
    let moved = transformed(&img, 2);
    let moved_face = detect_one(&engine, &moved).expect("a face in the transformed copy");

    let a = rec.embed(img.as_raw(), img.width(), img.height(), &original).unwrap();
    let b = rec.embed(moved.as_raw(), moved.width(), moved.height(), &moved_face).unwrap();

    assert_eq!(a.len(), chaff_faces::DIMENSIONS);
    assert_eq!(b.len(), chaff_faces::DIMENSIONS);
    assert!(a.iter().all(|v| v.is_finite()), "the embedding must be finite");

    let similarity = chaff_faces::cosine(&a, &b);
    assert!(
        similarity > 0.6,
        "the same face embedded at {similarity:.3} similarity after moving and resizing — \
         alignment is not doing its job"
    );
}

#[test]
fn an_embedding_is_deterministic() {
    // The catalog caches these. A recogniser that returned a different vector each time
    // would make every re-run produce different clusters.
    let (Some(engine), Some(rec)) = (engine(), recogniser()) else { return };
    let img = image::open(fixture("portrait_mona_lisa.jpg")).unwrap().to_rgb8();
    let face = detect_one(&engine, &img).expect("a face");

    let a = rec.embed(img.as_raw(), img.width(), img.height(), &face).unwrap();
    let b = rec.embed(img.as_raw(), img.width(), img.height(), &face).unwrap();
    assert_eq!(a, b);
    assert!((chaff_faces::cosine(&a, &b) - 1.0).abs() < 1e-5);
}

#[test]
fn an_embedding_is_not_degenerate() {
    // A vector of zeros would compare as "similar to nothing", which is safe but useless —
    // every face would be its own person and grouping would do nothing. This asserts the
    // model actually produced something.
    let (Some(engine), Some(rec)) = (engine(), recogniser()) else { return };
    let img = image::open(fixture("portrait_mona_lisa.jpg")).unwrap().to_rgb8();
    let face = detect_one(&engine, &img).expect("a face");

    let e = rec.embed(img.as_raw(), img.width(), img.height(), &face).unwrap();
    let magnitude: f32 = e.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!(magnitude > 1.0, "the embedding is near-zero (magnitude {magnitude})");
    assert!(e.iter().any(|v| v.abs() > 0.01), "every component is tiny");
}
