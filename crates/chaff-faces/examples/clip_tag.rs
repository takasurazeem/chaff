//! Tag one photograph with CLIP, no server needed (#53).
//!
//! `cargo run -p chaff-faces --example clip_tag -- <image>`
use chaff_faces::clip::{Clip, Vocabulary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: clip_tag <image>");

    let model = chaff_faces::clip::bundled_model().expect("the vision encoder");
    let vocab_path = chaff_faces::clip::bundled().expect("the vocabulary file");

    let img = image::open(&path)?.to_rgb8();
    println!("{path}: {}x{}", img.width(), img.height());

    let clip = Clip::from_file(&model)?;
    let vocab = Vocabulary::load(&vocab_path)?;
    println!("vocabulary: {} phrases", vocab.len());

    let t = std::time::Instant::now();
    let embedding = clip.embed(img.as_raw(), img.width(), img.height())?;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("embedded in {ms:.0} ms (CPU, no server)");

    for (phrase, score) in vocab.rank(&embedding).into_iter().take(6) {
        println!("  {score:+.3}  {phrase}");
    }
    Ok(())
}
