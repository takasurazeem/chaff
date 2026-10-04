//! Print every term for two photographs, so a ranking can be argued with rather than
//! taken on trust.
//!
//!     cargo run --release -p chaff-core --example why -- <folder> <name-a> <name-b>
use std::path::PathBuf;
use chaff_core::catalog::{self, store};
use chaff_core::pipeline;
use chaff_core::scoring::shoot::{ALL_METRICS, Metric};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(&a[0]);
    let want: Vec<&str> = a[1..].iter().map(String::as_str).collect();

    let mut conn = catalog::open_in_memory()?;
    let report = pipeline::index_and_score(&mut conn, &root, 1)?;
    let photos = pipeline::scored_photos(&conn, report.library_id)?;

    for (photo, score) in &photos {
        let files = store::files_for_photo(&conn, photo.id)?;
        let name = files
            .iter()
            .map(|f| std::path::Path::new(&f.path).file_name().unwrap().to_string_lossy().to_string())
            .next()
            .unwrap_or_default();
        if !want.iter().any(|w| name.starts_with(w)) {
            continue;
        }
        println!("\n{name}  composite {:?}", score.map(|s| s.round()));
        for m in ALL_METRICS {
            let rows = store::scores_for_photo(&conn, photo.id, pipeline::SCORER_VERSION)?;
            let key = format!("term:{}", metric_label(m));
            if let Some(r) = rows.iter().find(|r| r.metric == key) {
                println!("    {:<16} percentile {:>5.1}", metric_label(m), r.value);
            }
        }
    }
    Ok(())
}

fn metric_label(m: Metric) -> &'static str {
    match m {
        Metric::Focus => "focus",
        Metric::Detail => "detail",
        Metric::Anisotropy => "anisotropy",
        Metric::ExposureMean => "exposure_mean",
        Metric::ExposureRange => "tonal_range",
        Metric::ClippedHigh => "clipped_high",
        Metric::ClippedLow => "clipped_low",
        Metric::Noise => "noise",
    }
}
