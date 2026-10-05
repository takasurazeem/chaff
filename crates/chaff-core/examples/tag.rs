//! Tag one photograph through a live endpoint.
//!
//! `cargo run -p chaff-core --example tag -- <image> [base-url] [model]`
use chaff_core::vlm::{self, Endpoint, TagRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: tag <image> [base] [model]");
    let base = std::env::args().nth(2).unwrap_or_else(|| "http://192.168.1.150:8080".into());
    let model = std::env::args().nth(3).unwrap_or_else(|| "chaff-vlm".into());
    let endpoint = Endpoint { base, model };

    println!("endpoint: {}", endpoint.chat_url());
    vlm::health(&endpoint, 10)?;
    println!("health: ok");

    // Downscaled before sending, which is what the client is for.
    let img = image::open(&path)?;
    let small = img.resize(768, 768, image::imageops::FilterType::Lanczos3).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode(small.as_raw(), small.width(), small.height(), image::ExtendedColorType::Rgb8)?;
    println!("image: {}x{} -> {} KB", img.width(), img.height(), jpeg.len() / 1024);

    let started = std::time::Instant::now();
    let result = vlm::tag(&endpoint, &TagRequest {
        image: jpeg,
        vocabulary: None,
        extra_instructions: None,
    }, 300)?;

    println!("{:.1}s · {} prompt + {} completion tokens",
        started.elapsed().as_secs_f64(), result.prompt_tokens, result.completion_tokens);
    println!("description: {}", result.description.as_deref().unwrap_or("(none)"));
    for t in &result.tags {
        println!("  {:.2}  {}", t.confidence, t.name);
    }
    Ok(())
}
