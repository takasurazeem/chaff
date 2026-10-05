//! Compute CLIP text embeddings for a vocabulary, once (#53).
//!
//! # Why this is an example and not shipped code
//!
//! CLIP's text side needs a BPE tokenizer, and the tokenizer is a build-time tool for this
//! project: the vocabulary is **fixed**, so its embeddings are computed once and committed
//! as a small data file. At runtime the fallback needs only the vision encoder.
//!
//! Shipping a tokenizer in the application would mean a text encoder, a 61 MB model and a
//! dependency, all to recompute numbers that never change.
//!
//! `cargo run -p chaff-faces --example clip_vocab -- <tokenizer.json> <text_model.onnx> <out.bin>`

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let tokenizer_path = args.get(1).expect("usage: clip_vocab <tokenizer.json> <text.onnx> <out.bin>");
    let model_path = args.get(2).expect("the text model");
    let out_path = args.get(3).expect("the output file");

    let vocabulary = chaff_faces::clip::VOCABULARY;

    let tokenizer = tokenizers::Tokenizer::from_file(tokenizer_path)
        .map_err(|e| format!("{e}"))?;

    ort::init().with_name("chaff").commit();
    let mut session = ort::session::Session::builder()?.commit_from_file(model_path)?;
    let input_name = session.inputs()[0].name().to_string();
    let output_name = session.outputs()[0].name().to_string();

    // CLIP was trained with a prompt template, and using the bare word measurably degrades
    // zero-shot accuracy. "a photo of a beach" is not decoration — it is part of the model's
    // input distribution.
    let prompts: Vec<String> = vocabulary.iter().map(|v| format!("a photo of {v}")).collect();

    let encodings = tokenizer
        .encode_batch(prompts.clone(), true)
        .map_err(|e| format!("{e}"))?;

    // CLIP's context length. Truncated rather than padded: the text encoder masks padding,
    // and a padded sequence produces a different embedding.
    const CONTEXT: usize = 77;
    let mut ids = vec![0i64; encodings.len() * CONTEXT];
    for (i, enc) in encodings.iter().enumerate() {
        let t = enc.get_ids();
        let n = t.len().min(CONTEXT);
        for (j, id) in t[..n].iter().enumerate() {
            ids[i * CONTEXT + j] = *id as i64;
        }
    }

    let shape = [encodings.len(), CONTEXT];
    let tensor = ort::value::Tensor::from_array((shape, ids))?;
    let outputs = session.run(ort::inputs![input_name.as_str() => tensor])?;
    let value = outputs
        .get(output_name.as_str())
        .ok_or("no text output")?;
    let array = value.try_extract_array::<f32>()?;
    let flat: Vec<f32> = array.iter().copied().collect();

    let dim = flat.len() / vocabulary.len();
    println!("{} phrases → {} × {dim}", vocabulary.len(), vocabulary.len());

    // Normalised on write: the runtime compares with a dot product, and normalising here
    // means the application never has to.
    let mut file = std::fs::File::create(out_path)?;
    file.write_all(b"CHAFFCLIP1")?;
    file.write_all(&(vocabulary.len() as u32).to_le_bytes())?;
    file.write_all(&(dim as u32).to_le_bytes())?;
    for i in 0..vocabulary.len() {
        let row = &flat[i * dim..(i + 1) * dim];
        let n: f32 = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in row {
            file.write_all(&(x / n.max(1e-9)).to_le_bytes())?;
        }
    }
    println!("wrote {out_path}");
    Ok(())
}
