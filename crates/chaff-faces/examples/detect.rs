//! Run the detector over an image and print what it found.
//!
//! `cargo run -p chaff-faces --example detect -- <image>`
use chaff_faces::{Detector, FaceEngine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: detect <image>");
    let img = image::open(&path)?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    println!("{path}: {w}x{h}");

    let model = chaff_faces::YuNet::bundled().expect("the bundled model");
    let engine = chaff_faces::YuNet::from_file(&model)?;
    println!("engine: {} ({})", chaff_faces::FaceEngine::name(&engine), engine.licence());

    let t = std::time::Instant::now();
    let faces = Detector::detect(&engine, img.as_raw(), w, h)?;
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    println!("{} face(s) in {ms:.0} ms", faces.len());
    for (i, f) in faces.iter().enumerate() {
        println!(
            "  [{i}] {:.0},{:.0} {:.0}x{:.0}  confidence {:.3}",
            f.x, f.y, f.width, f.height, f.confidence
        );
    }
    Ok(())
}
