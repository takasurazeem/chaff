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
use chaff_core::hardware;
use chaff_core::pipeline;
use chaff_core::thumb::{self, ThumbSize, ThumbnailCache, DEFAULT_CAP_BYTES};
use chaff_core::trash::Trash;
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
        Self { rating: d.rating.get(), rejected: d.rejected }
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
pub async fn open_library(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<LibraryView, String> {
    let db = state.db();
    let root = PathBuf::from(&path);
    let now = now_seconds();

    tauri::async_runtime::spawn_blocking(move || -> Result<LibraryView, String> {
        let mut conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;

        // Progress is emitted as an event rather than polled, because the work is one
        // blocking call — there is no state for the frontend to read while it runs. The
        // emit is best-effort: a window that has gone away must not fail an index that is
        // otherwise fine.
        use tauri::Emitter;
        let report = pipeline::index_and_score_with_progress(&mut conn, &root, now, &mut |p| {
            let _ = app.emit("chaff://index-progress", &p);
        })
        .map_err(err)?;
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
                    rating: d.rating.get(),
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
            store::Decision { rating: store::Rating::new(rating), rejected },
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

// ---------------------------------------------------------------------------
// Deleting — the only destructive surface
// ---------------------------------------------------------------------------
#[derive(Debug, Serialize)]
pub struct DeleteFileView {
    pub name: String,
    pub path: String,
    pub bytes: i64,
}

#[derive(Debug, Serialize)]
pub struct DeleteCandidateView {
    pub photo_id: i64,
    pub stem: String,
    pub files: Vec<DeleteFileView>,
    pub bytes: i64,
    /// True when this photograph has only one of its two halves.
    pub incomplete: bool,
}

#[derive(Debug, Serialize)]
pub struct DeletePlanView {
    pub candidates: Vec<DeleteCandidateView>,
    pub photographs: usize,
    pub files: usize,
    pub bytes: i64,
    pub incomplete: usize,
    /// Conditions worth showing before the user confirms. Empty is the common case.
    pub warnings: Vec<String>,
    /// Photographs that could not be resolved to anything on disk.
    pub missing: usize,
    /// Refusals that make the whole operation impossible. Non-empty means no delete.
    pub refusals: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DeleteReceiptView {
    pub op_id: String,
    pub moved: usize,
    pub bytes: i64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TrashOperationView {
    pub op_id: String,
    pub at: i64,
    pub date: String,
    pub reason: String,
    pub files: usize,
    pub bytes: i64,
    /// True once the bytes are gone. The record survives the purge, so the answer to
    /// "what did I delete, and when" outlives the files.
    pub purged: bool,
}

#[derive(Debug, Serialize)]
pub struct RestoreView {
    pub op_id: String,
    pub restored: usize,
    pub already_present: usize,
    pub blocked: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PurgeView {
    pub operations: usize,
    pub removed: usize,
    pub bytes: i64,
}

/// What a delete would move. **Changes nothing.**
///
/// The caller shows this to a person. It is deliberately advisory: [`commit_delete`]
/// recomputes rather than accepting this plan, so a file that changed between the two
/// calls is caught by the commit's own re-hash rather than by trusting a stale list.
#[tauri::command]
pub async fn plan_delete(
    state: State<'_, AppState>,
    library_root: String,
    photo_ids: Vec<i64>,
) -> Result<DeletePlanView, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<DeletePlanView, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let selection = pipeline::resolve_delete_selection(&conn, &photo_ids).map_err(err)?;

        let trash = Trash::open(std::path::Path::new(&library_root)).map_err(err)?;
        let hashes = std::collections::HashMap::new();
        let files: Vec<std::path::PathBuf> =
            selection.candidates.iter().flat_map(|c| c.files.clone()).collect();

        // Refusals are collected rather than short-circuited, so the user sees every
        // reason at once instead of fixing one and meeting the next.
        let mut refusals = Vec::new();
        let mut warnings = Vec::new();
        for f in &files {
            if let Err(r) = trash.check(f) {
                refusals.push(r.to_string());
            }
        }

        // The engine's own plan, for cross-volume warnings and collision suffixes.
        if refusals.is_empty() {
            match trash.plan(&files, &hashes, now_seconds()) {
                Ok(plan) => warnings.extend(plan.warnings.iter().map(describe_warning)),
                Err(e) => refusals.push(e.to_string()),
            }
        }

        Ok(DeletePlanView {
            photographs: selection.candidates.len(),
            files: selection.file_count(),
            bytes: selection.total_bytes(),
            incomplete: selection.incomplete_count(),
            missing: selection.missing.len(),
            refusals,
            warnings,
            candidates: selection
                .candidates
                .into_iter()
                .map(|c| DeleteCandidateView {
                    photo_id: c.photo_id,
                    stem: c.stem.clone(),
                    incomplete: c.is_incomplete(),
                    bytes: c.bytes,
                    files: c
                        .files
                        .iter()
                        .map(|f| DeleteFileView {
                            name: f
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            path: f.to_string_lossy().to_string(),
                            bytes: std::fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0),
                        })
                        .collect(),
                })
                .collect(),
        })
    })
    .await
    .map_err(err)?
}

/// Move the selection to the trash.
///
/// **Re-resolves and re-plans from the photograph ids.** It does not accept a plan from
/// the frontend, so nothing the webview sends can name a file the engine did not choose
/// itself. The commit re-hashes every file before moving it, so a file that changed since
/// the plan was shown aborts the operation rather than being moved unexamined.
#[tauri::command]
pub async fn commit_delete(
    state: State<'_, AppState>,
    library_root: String,
    photo_ids: Vec<i64>,
    reason: String,
) -> Result<DeleteReceiptView, String> {
    let db = state.db();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<DeleteReceiptView, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let selection = pipeline::resolve_delete_selection(&conn, &photo_ids).map_err(err)?;

        let trash = Trash::open(std::path::Path::new(&library_root)).map_err(err)?;
        let files: Vec<std::path::PathBuf> =
            selection.candidates.iter().flat_map(|c| c.files.clone()).collect();

        // The hash the catalog recorded, where it has one. `file.content_hash` is filled
        // lazily, so this is often empty and the commit's own read-back is the check.
        let mut hashes = std::collections::HashMap::new();
        for c in &selection.candidates {
            for f in &c.files {
                if let Ok(Some(h)) = store::content_hash_for_path(&conn, &f.to_string_lossy()) {
                    hashes.insert(f.clone(), h);
                }
            }
        }

        let plan = trash.plan(&files, &hashes, now).map_err(err)?;
        let receipt = trash.commit(&plan, &reason, now).map_err(err)?;

        // The catalog is updated only after the files have moved. A crash in between
        // leaves files in the trash that the catalog still lists, which the next index
        // pass corrects — the reverse order would leave the catalog claiming a file is
        // gone while it is still on disk.
        //
        // The row is marked, not deleted: `decision` cascades with `photo`, so removing it
        // would take the user's rating with it and a restore would bring the file back
        // unrated.
        for c in &selection.candidates {
            store::mark_photo_trashed(&conn, c.photo_id, now).map_err(err)?;
        }

        Ok(DeleteReceiptView {
            op_id: receipt.op_id,
            moved: receipt.moved,
            bytes: receipt.bytes,
            warnings: receipt.warnings.iter().map(describe_warning).collect(),
        })
    })
    .await
    .map_err(err)?
}

/// Every recorded trash operation, newest first.
#[tauri::command]
pub async fn list_trash(
    state: State<'_, AppState>,
    library_root: String,
) -> Result<Vec<TrashOperationView>, String> {
    let _ = state;
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<TrashOperationView>, String> {
        let trash = Trash::open(std::path::Path::new(&library_root)).map_err(err)?;
        let entries = trash.manifest().map_err(err)?;
        let purged: std::collections::HashSet<String> = entries
            .iter()
            .filter(|e| e.action == "purge")
            .map(|e| e.op_id.trim_end_matches("-purged").to_string())
            .collect();

        let mut out: Vec<TrashOperationView> = entries
            .iter()
            .filter(|e| e.action == "trash")
            .map(|e| TrashOperationView {
                op_id: e.op_id.clone(),
                at: e.at,
                date: chaff_core::trash::civil_date(e.at),
                reason: e.reason.clone(),
                files: e.files.len(),
                bytes: e.files.iter().map(|f| f.size).sum(),
                purged: purged.contains(&e.op_id),
            })
            .collect();
        out.reverse();
        Ok(out)
    })
    .await
    .map_err(err)?
}

/// Move a trashed operation's files back.
#[tauri::command]
pub async fn restore_trash(
    state: State<'_, AppState>,
    library_root: String,
    op_id: String,
) -> Result<RestoreView, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<RestoreView, String> {
        let trash = Trash::open(std::path::Path::new(&library_root)).map_err(err)?;
        let report = trash.restore(&op_id).map_err(err)?;

        // Bring the rows back **before** re-indexing, so the sweep sees them as live and
        // the index pass reattaches the restored files to the same photograph — which is
        // what keeps the rating attached to it.
        if report.restored > 0 {
            if let Ok(mut conn) = db.lock() {
                let paths: Vec<String> = trash
                    .manifest()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|e| e.op_id == op_id)
                    .map(|e| e.files.iter().map(|f| f.source.to_string_lossy().to_string()).collect())
                    .unwrap_or_default();
                let _ = store::clear_trashed_for_paths(&conn, &paths);
                let _ = pipeline::index_and_score(
                    &mut conn,
                    std::path::Path::new(&library_root),
                    now_seconds(),
                );
            }
        }

        Ok(RestoreView {
            op_id: report.op_id,
            restored: report.restored,
            already_present: report.already_present,
            blocked: report.missing.iter().map(|p| p.to_string_lossy().to_string()).collect(),
        })
    })
    .await
    .map_err(err)?
}

/// **Empty the trash.** The only command in this application that unlinks a file.
///
/// Takes explicit operation ids rather than "everything": a purge that decides for itself
/// what to remove is a purge that can be wrong about it.
#[tauri::command]
pub async fn purge_trash(
    state: State<'_, AppState>,
    library_root: String,
    op_ids: Vec<String>,
) -> Result<PurgeView, String> {
    let _ = state;
    tauri::async_runtime::spawn_blocking(move || -> Result<PurgeView, String> {
        let trash = Trash::open(std::path::Path::new(&library_root)).map_err(err)?;
        let receipt = trash.purge(&op_ids).map_err(err)?;
        Ok(PurgeView {
            operations: receipt.operations,
            removed: receipt.removed,
            bytes: receipt.bytes,
        })
    })
    .await
    .map_err(err)?
}

fn describe_warning(w: &chaff_core::trash::Warning) -> String {
    use chaff_core::trash::Warning;
    match w {
        Warning::CrossVolume { source, .. } => format!(
            "{} is on a different volume from the trash folder. Moving it is a copy              followed by a delete, not an atomic rename.",
            source.display()
        ),
        Warning::Missing { path } => {
            format!("{} is no longer on disk and will not be moved.", path.display())
        }
    }
}

/// The Capability Report for this machine, as plain text.
///
/// Returned rendered rather than structured: it is shown to a person and pasted into bug
/// reports, and a structure the frontend has to lay out is a structure it can lay out
/// wrongly. The probing is fast (one `nvidia-smi` or `system_profiler` call) but it does
/// spawn a process, so it runs off the main thread like everything else here.
#[tauri::command]
pub async fn capabilities(endpoints: Vec<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let mut probe = hardware::probe();
        for url in &endpoints {
            probe.endpoints.push(hardware::probe_endpoint(url, 1500));
        }
        Ok(hardware::render(&probe))
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
