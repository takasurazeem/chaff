// Measure the same library indexed twice into a persistent catalog.
use std::time::Instant;
use chaff_core::catalog;
use chaff_core::pipeline;

fn main() {
    let dir = std::env::args().nth(1).expect("folder");
    let db = std::env::args().nth(2).expect("db path");
    let _ = std::fs::remove_file(&db);
    let mut conn = catalog::open(std::path::Path::new(&db)).expect("open");

    for pass in 1..=2 {
        let t = Instant::now();
        let r = pipeline::index_and_score(&mut conn, std::path::Path::new(&dir), 1_700_000_000 + pass)
            .expect("pipeline");
        println!(
            "  pass {pass}: {:>6.2}s   {} photographs, {} scored, {} reused",
            t.elapsed().as_secs_f64(),
            r.photos,
            r.scored,
            r.reused
        );
    }
}
