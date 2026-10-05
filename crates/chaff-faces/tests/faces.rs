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
