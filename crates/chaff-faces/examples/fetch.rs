//! Fetch a model through the verified store, reporting progress.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).expect("usage: fetch <cache-dir>");
    let store = chaff_faces::ModelStore::new(&dir);
    let spec = chaff_faces::SFACE;
    println!("{}: {} ({})", spec.file, spec.description, spec.licence);

    let mut last = 0u64;
    let path = store.ensure(&spec, |done, total| {
        // Reported in whole megabytes; a line per 64 KB chunk is unreadable.
        if done / 4_000_000 != last / 4_000_000 {
            eprintln!("  {}/{} MB", done / 1_000_000, total / 1_000_000);
        }
        last = done;
    })?;
    println!("ok: {}", path.display());
    println!("bytes: {}", std::fs::metadata(&path)?.len());
    Ok(())
}
