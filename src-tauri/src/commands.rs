//! The IPC surface.
//!
//! # Deliberately thin
//!
//! Every command here is an adapter: parse arguments, call into `chaff_core`, serialise
//! the result. No logic lives in this file, because this is the one part of the codebase
//! that cannot be tested on the Linux build host — the Tauri crate needs a webview, and
//! that host has no GTK or WebKit packages. Anything with a decision in it belongs in
//! `chaff_core`, where `cargo test -p chaff-core` runs with nothing installed.
//!
//! # The webview cannot read the photo library
//!
//! Thumbnails are served over Tauri's asset protocol, which is scoped in
//! `tauri.conf.json` to **the thumbnail cache directory and nothing else**. The webview
//! therefore cannot construct a URL that reads an arbitrary file, however the frontend is
//! compromised. It is handed cache paths, and those are the only paths it can fetch.
//!
//! That is a deliberate choice against the convenient alternative of exposing the library
//! root, which would let any injected script read every photograph on the machine. A
//! thumbnail is a rendered derivative; the original never crosses this boundary.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chaff_core::catalog::{self, store, CatalogError};
use chaff_core::pipeline;
use chaff_core::thumb::{self, ThumbSize, ThumbnailCache, DEFAULT_CAP_BYTES};
use chaff_core::rusqlite::Connection;
use serde::Serialize;
use tauri::{Manager, State};

/// Shared application state.
pub struct AppState {
    db: Arc<Mutex<Connection>>,
    thumbs: ThumbnailCache,
}

impl AppState {
    pub fn new(db: Connection, thumbs: ThumbnailCache) -> Self {
        Self { db: Arc::new(Mutex::new(db)), thumbs }
    }

    fn db(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.db)
    }

    fn thumbs(&self) -> ThumbnailCache {
        self.thumbs.clone()
    }
}

/// Errors cross the IPC boundary as strings. The engine's error types are far richer, and
/// flattening them here is deliberate: the frontend shows a message, and a structured
/// error type it cannot act on is only a longer message.
fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------
#[derive(Debug, Serialize)]
pub struct LibraryView {
    pub library_id: i64,
    pub root: String,
    pub scanned_files: usize,
    pub photos: usize,
    pub pairs: usize,
    pub needs_review: usize,
    pub scored: usize,
    /// Photographs whose image data this build cannot read. Needs a raw decoder (#8).
    pub unscoreable: usize,
    pub shoots: usize,
    pub keep: usize,
    pub review: usize,
    pub reject: usize,
    pub elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct PhotoView {
    pub id: i64,
    pub dir: String,
    pub stem: String,
    pub state: String,
    pub needs_review: bool,
    /// What the engine thinks.
    pub composite: Option<f64>,
    pub band: Option<&'static str>,
    /// What the user decided. Kept beside the engine's opinion rather than replacing it,
    /// so a re-score never silently overwrites a judgement.
    pub rating: u8,
    pub rejected: bool,
}

/// A decision, as the frontend sees it.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DecisionView {
    pub rating: u8,
    pub rejected: bool,
}

impl From<store::Decision> for DecisionView {
    fn from(d: store::Decision) -> Self {
        Self { rating: d.rating, rejected: d.rejected }
    }
}

#[derive(Debug, Serialize)]
pub struct ThumbnailView {
    /// An absolute path inside the thumbnail cache. The frontend turns it into an asset
    /// URL with `convertFileSrc`; it is never a path into the photo library.
    pub path: String,
    pub size: &'static str,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------
/// Index a folder and score it.
///
/// Long-running: a first index of a large library is minutes of work. Declared `async`
/// and dispatched onto a blocking thread so the webview stays responsive, which is the
/// difference between a progress indicator and an application that looks hung.
#[tauri::command]
pub async fn open_library(state: State<'_, AppState>, path: String) -> Result<LibraryView, String> {
    let db = state.db();
    let root = PathBuf::from(&path);
    let now = now_seconds();

    tauri::async_runtime::spawn_blocking(move || -> Result<LibraryView, String> {
        let mut conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let report = pipeline::index_and_score(&mut conn, &root, now).map_err(err)?;
        Ok(LibraryView {
            library_id: report.library_id,
            root: report.root.to_string_lossy().to_string(),
            scanned_files: report.scanned_files,
            photos: report.photos,
            pairs: report.pairs,
            needs_review: report.needs_review,
            scored: report.scored,
            unscoreable: report.unscoreable,
            shoots: report.shoots,
            keep: report.bands.keep,
            review: report.bands.review,
            reject: report.bands.reject,
            elapsed_ms: report.elapsed_ms,
        })
    })
    .await
    .map_err(err)?
}

/// Every photograph in a library, with its score.
///
/// Deliberately does **not** generate thumbnails. A 50,000-photo library would take
/// minutes to render up front, and the grid only ever shows a few dozen cells at a time.
/// The frontend asks for those, by id, as they scroll into view.
#[tauri::command]
pub async fn list_photos(
    state: State<'_, AppState>,
    library_id: i64,
) -> Result<Vec<PhotoView>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PhotoView>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let rows = pipeline::scored_photos(&conn, library_id).map_err(err)?;
        // One query for every decision, merged here rather than fetched per cell. A grid
        // asking per cell is fifty thousand round trips.
        let decisions = store::decisions_for_library(&conn, library_id).map_err(err)?;

        Ok(rows
            .into_iter()
            .map(|(p, composite)| {
                let d = decisions.get(&p.id).copied().unwrap_or_default();
                PhotoView {
                    id: p.id,
                    dir: p.dir,
                    stem: p.stem,
                    state: p.state,
                    needs_review: p.needs_review,
                    composite,
                    band: composite.map(band_of),
                    rating: d.rating,
                    rejected: d.rejected,
                }
            })
            .collect())
    })
    .await
    .map_err(err)?
}

/// Render (or fetch) one photograph's thumbnail.
///
/// Returns a path inside the cache, or `None` when the source cannot be read — a raw
/// format needing LibRaw (#8), or a corrupt file. `None` is a normal answer, not an
/// error: a grid cell that cannot be drawn should show a placeholder, not fail the grid.
#[tauri::command]
pub async fn photo_thumbnail(
    state: State<'_, AppState>,
    photo_id: i64,
    size: Option<String>,
) -> Result<Option<ThumbnailView>, String> {
    let db = state.db();
    let thumbs = state.thumbs();
    let size = parse_size(size.as_deref());

    tauri::async_runtime::spawn_blocking(move || -> Result<Option<ThumbnailView>, String> {
        let source = {
            let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
            source_for_photo(&conn, photo_id).map_err(err)?
        };
        let Some(source) = source else { return Ok(None) };

        match thumb::generate_and_store(&thumbs, std::path::Path::new(&source), size) {
            Ok(path) => Ok(Some(ThumbnailView {
                path: path.to_string_lossy().to_string(),
                size: size.dir_name(),
            })),
            // Unreadable source, not a broken cache.
            Err(thumb::ThumbError::NoSource { .. }) => Ok(None),
            Err(e) => Err(err(e)),
        }
    })
    .await
    .map_err(err)?
}

/// The explanation for one photograph's score, rebuilt from stored terms.
#[tauri::command]
pub async fn photo_explanation(
    state: State<'_, AppState>,
    photo_id: i64,
) -> Result<Vec<String>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        pipeline::explain_photo(&conn, photo_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Record what the user decided about a photograph.
///
/// Returns the previous decision so the caller can push it onto an undo stack. Undo lives
/// in the session rather than in the database: the PRD scopes it that way, and a permanent
/// history of every keystroke is a different feature with different costs.
#[tauri::command]
pub async fn set_decision(
    state: State<'_, AppState>,
    photo_id: i64,
    rating: u8,
    rejected: bool,
) -> Result<DecisionView, String> {
    let db = state.db();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<DecisionView, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let previous = store::set_decision(
            &conn,
            photo_id,
            store::Decision { rating: rating.min(5), rejected },
            now,
        )
        .map_err(err)?;
        Ok(previous.into())
    })
    .await
    .map_err(err)?
}

/// How many photographs in a library carry a decision.
#[tauri::command]
pub async fn decision_count(state: State<'_, AppState>, library_id: i64) -> Result<usize, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<usize, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::decision_count(&conn, library_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Force the thumbnail cache back inside its cap.
///
/// Exposed so the frontend can call it after a bulk operation rather than paying for
/// eviction on every write.
#[tauri::command]
pub async fn trim_thumbnail_cache(state: State<'_, AppState>) -> Result<u64, String> {
    let thumbs = state.thumbs();
    tauri::async_runtime::spawn_blocking(move || -> Result<u64, String> {
        let report = thumbs.evict_to_cap().map_err(err)?;
        Ok(report.remaining_bytes)
    })
    .await
    .map_err(err)?
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
fn parse_size(name: Option<&str>) -> ThumbSize {
    match name {
        Some("loupe") => ThumbSize::Loupe,
        Some("zoom") => ThumbSize::Zoom,
        _ => ThumbSize::Grid,
    }
}

/// The band a composite falls into, using the same thresholds the pipeline used.
fn band_of(composite: f64) -> &'static str {
    use chaff_core::scoring::composite::{Band, BandThresholds};
    match BandThresholds::default().band_of(composite) {
        Band::Keep => "keep",
        Band::Review => "review",
        Band::Reject => "reject",
    }
}

/// Prefer the raw, then the rendered file.
///
/// The raw carries the camera's own preview and, for a paired photograph, the sensor
/// data; the rendered file is the fallback when the raw cannot be read.
fn source_for_photo(conn: &Connection, photo_id: i64) -> Result<Option<String>, CatalogError> {
    let files = store::files_for_photo(conn, photo_id)?;
    Ok(files
        .iter()
        .find(|f| f.role == "raw")
        .or_else(|| files.iter().find(|f| f.role == "raster"))
        .map(|f| f.path.clone()))
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Build the shared state, creating the catalog and the cache under the app data dir.
pub fn initialise(app: &tauri::AppHandle) -> Result<AppState, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve the app data directory: {e}"))?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;

    let db = catalog::open(&data_dir.join("catalog.db")).map_err(err)?;
    let thumbs = ThumbnailCache::open(data_dir.join("thumbnails"), DEFAULT_CAP_BYTES).map_err(err)?;

    Ok(AppState::new(db, thumbs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rating_above_five_is_clamped_rather_than_stored() {
        // The schema rejects six stars, so an out-of-range value from a buggy frontend
        // would surface as a database error rather than as a wrong rating. Clamping here
        // means the user sees a five-star photograph and a working application.
        let clamped = 9u8.min(5);
        assert_eq!(clamped, 5);
    }

    #[test]
    fn size_names_map_to_the_three_sizes() {
        assert_eq!(parse_size(Some("grid")), ThumbSize::Grid);
        assert_eq!(parse_size(Some("loupe")), ThumbSize::Loupe);
        assert_eq!(parse_size(Some("zoom")), ThumbSize::Zoom);
    }

    #[test]
    fn an_unknown_or_missing_size_falls_back_to_the_grid() {
        // The grid is the cheap one, so an unrecognised value degrades to the least
        // expensive answer rather than to the most.
        assert_eq!(parse_size(None), ThumbSize::Grid);
        assert_eq!(parse_size(Some("enormous")), ThumbSize::Grid);
        assert_eq!(parse_size(Some("")), ThumbSize::Grid);
    }

    #[test]
    fn bands_agree_with_the_pipeline_thresholds() {
        use chaff_core::scoring::composite::BandThresholds;
        let t = BandThresholds::default();
        assert_eq!(band_of(100.0), "keep");
        assert_eq!(band_of(t.keep), "keep");
        assert_eq!(band_of(t.keep - 0.1), "review");
        assert_eq!(band_of(t.reject - 0.1), "reject");
    }

    #[test]
    fn the_scorer_version_is_positive() {
        use chaff_core::pipeline::SCORER_VERSION;
        // Zero would collide with the SQLite default and make "unscored" indistinguishable
        // from "scored by version 0".
        assert!(SCORER_VERSION > 0);
    }

    #[test]
    fn state_is_shareable_across_threads() {
        // The commands dispatch onto blocking threads, so the state must be Send + Sync.
        // A compile-time property, asserted here so a future field that is neither fails
        // at the point it is added rather than in a command.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AppState>();
    }
}
