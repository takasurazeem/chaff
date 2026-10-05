//! The 50k grid benchmark (#37).
//!
//! # What it measures, and why these numbers
//!
//! The grid's cost is not rendering — it is virtualised, so it draws what is on screen. The
//! cost is **the list the grid is built from**: filtering, counting and sorting 50,000
//! photographs every time a keystroke lands in the search box.
//!
//! So this measures the engine's own work — the queries and the passes the UI calls on every
//! interaction — at 50k photographs, and prints a budget line per operation. A number without
//! a budget is a number nobody acts on.
//!
//! `cargo run --release -p chaff-core --example grid_bench`

use std::path::Path;
use std::time::Instant;

use chaff_core::catalog::store;

/// How many photographs to synthesise.
const COUNT: usize = 50_000;

/// The per-operation budget, in milliseconds, that CI enforces.
///
/// Generous on purpose. This runs on a shared runner where a neighbour's build can double
/// any number; a tight budget produces a flaky check that gets disabled, and a disabled check
/// catches nothing. These are set to catch an *order of magnitude* regression — an accidental
/// O(n²), a query that lost its index — not a 20% drift.
const BUDGET_MS: &[(&str, f64)] = &[
    ("insert 50k photographs", 20_000.0),
    ("list_photos", 2_000.0),
    ("list_directories", 2_000.0),
    ("face_counts", 1_000.0),
    ("tag_counts", 2_000.0),
    ("photo_metadata", 3_000.0),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = chaff_core::catalog::open_in_memory()?;
    let lib = store::upsert_library(&conn, Path::new("/bench"), 0)?;

    // Synthesised in memory. **No photographs are read** — this measures the catalog, and
    // reading 50,000 files would measure the disk instead.
    println!("building a {COUNT}-photograph catalog…");
    // Rows written directly, in one transaction.
    //
    // **Not the indexer.** This measures the catalog at 50k — the queries the grid makes on
    // every keystroke — and walking a real tree would measure the disk instead. The shape is
    // what matters: 200 folders of 250, because a photographer's library is folders and a
    // flat list would flatter every query that groups by one.
    let started = Instant::now();
    {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO library (root, created_at, last_indexed_at) VALUES ('/bench', 0, 0)",
            [],
        )
        .ok();
        for i in 0..COUNT {
            let dir = format!("/bench/2024-{:02}", i % 200);
            let stem = format!("IMG_{:05}", i);
            tx.execute(
                "INSERT INTO photo (library_id, dir, stem, state, needs_review)
                 VALUES (?1, ?2, ?3, 'raw_only', 0)",
                chaff_core::rusqlite::params![lib, dir, stem],
            )?;
            let photo_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO file (library_id, photo_id, path, role, size_bytes, mtime_ns, indexed_at)
                 VALUES (?1, ?2, ?3, 'raw', ?4, ?5, 1)",
                chaff_core::rusqlite::params![
                    lib,
                    photo_id,
                    format!("{dir}/{stem}.CR3"),
                    20_000_000 + (i as i64 % 1000),
                    1_700_000_000_000_000_000i64 + i as i64
                ],
            )?;
        }
        tx.commit()?;
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut results: Vec<(&str, f64)> = vec![("insert 50k photographs", build_ms)];

    let t = Instant::now();
    let photos = store::photos(&conn, lib)?;
    results.push(("list_photos", t.elapsed().as_secs_f64() * 1000.0));
    assert_eq!(photos.len(), COUNT);

    let t = Instant::now();
    let dirs = store::directories(&conn, lib)?;
    results.push(("list_directories", t.elapsed().as_secs_f64() * 1000.0));

    let t = Instant::now();
    let _ = store::face_counts(&conn, lib)?;
    results.push(("face_counts", t.elapsed().as_secs_f64() * 1000.0));

    let t = Instant::now();
    let _ = store::tag_counts(&conn, lib, None)?;
    results.push(("tag_counts", t.elapsed().as_secs_f64() * 1000.0));

    let t = Instant::now();
    let _ = store::photo_metadata(&conn, lib)?;
    results.push(("photo_metadata", t.elapsed().as_secs_f64() * 1000.0));

    println!("\n  {:32} {:>10}  {:>10}  {}", "operation", "actual", "budget", "verdict");
    let mut over = Vec::new();
    for (name, actual) in &results {
        let budget = BUDGET_MS
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| *b)
            .unwrap_or(f64::INFINITY);
        let ok = *actual <= budget;
        if !ok {
            over.push(*name);
        }
        println!(
            "  {:32} {:>8.1}ms  {:>8.0}ms  {}",
            name,
            actual,
            budget,
            if ok { "ok" } else { "OVER" }
        );
    }

    println!("\n  {} directories, {} photographs", dirs.len(), photos.len());

    if !over.is_empty() {
        // **A non-zero exit, so CI fails.** A benchmark that only prints is a benchmark
        // nobody reads.
        eprintln!("\n  over budget: {}", over.join(", "));
        std::process::exit(1);
    }
    println!("  all within budget");
    Ok(())
}
