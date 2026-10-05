//! blake3 of a file, for filling in a `ModelSpec`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: hash <file>");
    let data = std::fs::read(&path)?;
    println!("{}", blake3::hash(&data).to_hex());
    println!("{} bytes", data.len());
    Ok(())
}
