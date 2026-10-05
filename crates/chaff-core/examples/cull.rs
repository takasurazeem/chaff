//! Index and score a folder from the command line.
//!
//!     cargo run --release -p chaff-core --example cull -- <folder> [--top N]
//!
//! Exists so the engine can be exercised without the window — on a fixture folder, on a
//! test shoot, or on anything else the operator chooses to point it at. The GUI is a view
//! over the same pipeline; if the two ever disagree, this is the one telling the truth.
//!
//! **Read-only with respect to the folder.** It opens files for reading and writes only
//! the catalog, which lives in a temporary directory when run this way.

use std::path::PathBuf;
use std::time::Instant;

use chaff_core::catalog;
use chaff_core::pipeline::{self, SCORER_VERSION};
use chaff_core::catalog::store;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: cull <folder> [--top N]");
        std::process::exit(2);
    };
    let mut top = 20usize;
    while let Some(a) = args.next() {
        if a == "--top" {
            if let Some(n) = args.next().and_then(|s| s.parse().ok()) {
                top = n;
            }
        }
    }

    let root = PathBuf::from(root);
    if !root.is_dir() {
        eprintln!("not a folder: {}", root.display());
        std::process::exit(2);
    }

    // An in-memory catalog: running this must not disturb a real one.
    let mut conn = catalog::open_in_memory()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let started = Instant::now();
    let report = pipeline::index_and_score(&mut conn, &root, now)?;
    let wall = started.elapsed();

    println!("\n{}", root.display());
    println!("{}", "-".repeat(72));
    println!("  files scanned   {}", report.scanned_files);
    println!("  photographs     {}", report.photos);
    println!("  pairs           {}", report.pairs);
    println!("  shoots          {}", report.shoots);
    println!("  scored          {}", report.scored);
    if report.unscoreable > 0 {
        println!(
            "  unreadable      {}  (need a raw decoder this build does not have)",
            report.unscoreable
        );
    }
    if report.needs_review > 0 {
        println!("  to check        {}", report.needs_review);
    }
    println!(
        "  paired          {}  (raw-only {}, jpeg-only {}, ambiguous {})",
        report.by_state.pair,
        report.by_state.raw_only,
        report.by_state.raster_only,
        report.by_state.ambiguous
    );
    println!(
        "  bands           {} keep / {} review / {} reject",
        report.bands.keep, report.bands.review, report.bands.reject
    );
    if report.reused > 0 {
        println!("  reused          {} measurements (no decode)", report.reused);
    }
    println!("  elapsed         {:.1}s", wall.as_secs_f64());

    // The report checks itself. Printed rather than asserted, because a real library is a
    // different shape from a fixture folder and this is where the numbers get believed.
    let problems = report.inconsistencies();
    if problems.is_empty() {
        println!("  self-check      consistent");
    } else {
        println!("  self-check      INCONSISTENT");
        for p in &problems {
            println!("                    {p}");
        }
    }

    let scored = pipeline::scored_photos(&conn, report.library_id)?;
    let mut ranked: Vec<_> = scored.iter().filter_map(|(p, s)| s.map(|s| (p, s))).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    println!("\n  best {top}:");
    for (photo, score) in ranked.iter().take(top) {
        let files = store::files_for_photo(&conn, photo.id)?;
        let name = files
            .iter()
            .find(|f| f.role == "raw")
            .or_else(|| files.iter().find(|f| f.role == "raster"))
            .map(|f| {
                std::path::Path::new(&f.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| photo.stem.clone());
        let band = match score {
            s if *s >= 70.0 => "Keep  ",
            s if *s >= 35.0 => "Review",
            _ => "Reject",
        };
        println!("    {band} {score:5.1}  {name}");
    }

    // One explanation, so the reasoning behind the ranking is visible rather than
    // something the operator has to take on trust.
    if let Some((photo, _)) = ranked.first() {
        println!("\n  why the top one:");
        for line in pipeline::explain_photo(&conn, photo.id)? {
            println!("    {line}");
        }
    }
    println!("\n  (scorer version {SCORER_VERSION}; nothing in the folder was modified)\n");
    Ok(())
}
