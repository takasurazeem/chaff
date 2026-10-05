//! Print a model's inputs and outputs.
//!
//! Written before the decoder, because guessing a model's output layout is how you spend an
//! afternoon on a shape error that the model would have told you in one line.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: inspect <model.onnx>");
    ort::init().with_name("chaff").commit();
    let session = ort::session::Session::builder()?.commit_from_file(&path)?;

    println!("inputs:");
    for i in session.inputs() {
        println!("  {}  {:?}", i.name(), i.dtype());
    }
    println!("outputs:");
    for o in session.outputs() {
        println!("  {}  {:?}", o.name(), o.dtype());
    }
    Ok(())
}
