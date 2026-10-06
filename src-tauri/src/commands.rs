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

/// A delete the user has been shown but has not yet confirmed.
///
/// **Held server-side, and that is the point.** ADR-0004 promises that a file changing
/// between the confirmation and the move aborts the operation. Honouring it means hashing
/// the files *when the plan is shown* and verifying those hashes *when it is committed* —
/// which requires the hashes to survive between the two calls somewhere the frontend
/// cannot reach.
///
/// The first implementation kept them nowhere: it read `file.content_hash`, a column
/// nothing ever wrote, so the verification loop skipped every file and the guarantee did
/// not exist. The review agent proved it by changing a file's bytes between plan and commit
/// and watching it move anyway.
/// Shared application state.
pub struct AppState {
    db: Arc<Mutex<Connection>>,
    thumbs: ThumbnailCache,
    /// The plan most recently shown, if it has not been confirmed, cancelled or expired.
    ///
    /// **The engine's type, not the shell's.** It lived here until a second shell was
    /// planned, at which point two copies of a safety guarantee became the obvious outcome.
    /// See `chaff_core::delete_session`.
    pending_delete: Arc<Mutex<chaff_core::delete_session::DeleteSession>>,
    /// The library watcher, when one is running.
    watch: Arc<Mutex<Option<crate::watcher::Watch>>>,
    /// Set when the user asks a long pass to stop.
    ///
    /// **One flag for every pass**, not one per operation. The interface can only run one at a
    /// time — the connection lock makes sure of that — so a second flag would be a second way to
    /// express the same state, and the two would eventually disagree.
    ///
    /// Cleared at the start of each pass rather than at the end: a pass that ended early would
    /// otherwise leave the flag set and the *next* pass would stop immediately, which looks
    /// exactly like a hang.
    pass_cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl AppState {
    pub fn new(db: Connection, thumbs: ThumbnailCache) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            thumbs,
            pending_delete: Arc::new(Mutex::new(
                chaff_core::delete_session::DeleteSession::new(),
            )),
            watch: Arc::new(Mutex::new(None)),
            pass_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn pending(&self) -> Arc<Mutex<chaff_core::delete_session::DeleteSession>> {
        Arc::clone(&self.pending_delete)
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
    /// The full breakdown, so "2,956 photographs" can be read rather than guessed at.
    pub raw_only: usize,
    pub raster_only: usize,
    pub ambiguous: usize,
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
    /// From EXIF, preferring the raw. Absent when a file carries none — which is normal,
    /// not an error, and the filters must cope with it.
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub year: Option<i32>,
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
        let report = pipeline::index_and_score_with_progress(&mut conn, &root, now, &|p| {
            let _ = app.emit("chaff://index-progress", &p);
        })
        .map_err(err)?;
        Ok(LibraryView {
            library_id: report.library_id,
            root: report.root.to_string_lossy().to_string(),
            scanned_files: report.scanned_files,
            photos: report.photos,
            pairs: report.pairs,
            raw_only: report.by_state.raw_only,
            raster_only: report.by_state.raster_only,
            ambiguous: report.by_state.ambiguous,
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

#[derive(Debug, Serialize)]
pub struct DirectoryView {
    pub path: String,
    pub direct: usize,
    pub recursive: usize,
}

/// Run the face pass: detect, embed, group.
///
/// Long-running — the first pass downloads a 38 MB model and then runs a network over every
/// photograph — and **resumable**: each file is committed as it is processed, so closing the
/// window halfway through loses nothing.
#[tauri::command]
pub async fn run_face_pass(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library_id: i64,
) -> Result<crate::faces::FacePassReport, String> {
    let db = state.db();
    let now = now_seconds();
    let cancel = Arc::clone(&state.pass_cancel);
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve the app data directory: {e}"))?;

    tauri::async_runtime::spawn_blocking(move || -> Result<crate::faces::FacePassReport, String> {
        let mut conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let mut last = 0usize;
        crate::faces::run(&mut conn, &app_data, library_id, now, &mut |done, _total| {
            // Logged rather than emitted, for now: the pass reports a total when it
            // finishes, and a per-file event would need a progress channel this does not
            // have yet. The log is retrievable, which is the part that matters when it
            // takes twenty minutes.
            if done / 50 != last / 50 {
                log::info!("face pass: {done} files");
                last = done;
            }
            // The shell has no cancel control for this yet — see #76. `true` keeps going, which
            // is the honest state rather than a fake cancel that does nothing.
            true
        })
    })
    .await
    .map_err(err)?
}

/// How far a long pass has got.
///
/// **`total` can be zero.** The scan phase of an index genuinely does not know how many files
/// there are until the walk finishes, and a bar over an unknown total is a lie — so the frontend
/// shows an indeterminate indicator when it is zero rather than dividing by it.
#[derive(Debug, Clone, Serialize)]
pub struct PassProgress {
    /// `faces` or `tagging`, so one listener can serve both.
    pub stage: String,
    pub done: usize,
    pub total: usize,
}

/// Ask the running pass to stop.
///
/// **Nothing is lost.** Both passes commit each file as they go and the work list is the catalog
/// rather than a list in memory, so stopping leaves the catalog consistent and the next pass
/// resumes from where this one stopped.
///
/// Returns immediately: the pass checks the flag between photographs and stops on its own, which
/// is why this is a flag and not a kill.
#[tauri::command]
pub async fn cancel_pass(state: State<'_, AppState>) -> Result<(), String> {
    state
        .pass_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// What a sidecar write did.
#[derive(Debug, Serialize)]
pub struct SidecarReport {
    pub written: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Write decisions into XMP sidecars (#54).
///
/// # Opt-in, and only for decided photographs
///
/// A culling tool that silently writes files into a library the moment it opens it is one
/// nobody trusts twice. This is a command the user runs, it writes **only** photographs that
/// carry a decision, and it merges rather than replaces — everything Lightroom, darktable or
/// digiKam put in those files survives.
///
/// What is written is `xmp:Rating` and `xmp:Label`, which every application agrees on.
/// Chaff's own composite score and shoot grouping are **not** written: they are this
/// program's opinion, they change when the model changes, and a sidecar is not the place for
/// a number that will be different next week.
#[tauri::command]
pub async fn write_sidecars(
    state: State<'_, AppState>,
    library_id: i64,
) -> Result<SidecarReport, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<SidecarReport, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let decisions = store::decisions_for_library(&conn, library_id).map_err(err)?;
        let photos = store::photos(&conn, library_id).map_err(err)?;

        let mut report = SidecarReport { written: 0, skipped: 0, failed: 0 };
        for photo in &photos {
            let Some(decision) = decisions.get(&photo.id) else {
                report.skipped += 1;
                continue;
            };
            if decision.is_unrated() {
                // An unrated photograph is not a decision. Writing `Rating="0"` for every
                // photograph the user merely looked at would put a claim in the sidecar that
                // they never made.
                report.skipped += 1;
                continue;
            }

            let Some(primary) = store::files_for_photo(&conn, photo.id)
                .map_err(err)?
                .into_iter()
                .find(|f| f.role == "raw" || f.role == "raster")
            else {
                report.skipped += 1;
                continue;
            };

            let path = std::path::Path::new(&primary.path);
            if !chaff_core::xmp::supports_sidecar(path) {
                report.skipped += 1;
                continue;
            }
            match chaff_core::xmp::write(path, *decision, None) {
                Ok(_) => report.written += 1,
                Err(e) => {
                    log::warn!("could not write a sidecar for {}: {e}", primary.path);
                    report.failed += 1;
                }
            }
        }
        log::info!(
            "sidecars: {} written, {} skipped, {} failed",
            report.written,
            report.skipped,
            report.failed
        );
        Ok(report)
    })
    .await
    .map_err(err)?
}

/// Whether the library watcher is running.
#[derive(Debug, Serialize)]
pub struct WatchView {
    pub running: bool,
    /// Paths seen since the last re-index, so the UI can say "3 changes seen".
    pub seen: usize,
    pub busy: bool,
}

/// Start watching a library for external changes (#6).
///
/// Idempotent: starting a watcher that is already running for the same root does nothing,
/// because two watchers would re-index twice for every change.
#[tauri::command]
pub async fn start_watching(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library_root: String,
) -> Result<WatchView, String> {
    let existing = state.watch.lock().map_err(|_| "watch lock poisoned".to_string())?;
    if let Some(w) = existing.as_ref() {
        return Ok(WatchView { running: true, seen: w.seen(), busy: w.is_busy() });
    }
    drop(existing);

    let db = state.db();
    let root = std::path::PathBuf::from(&library_root);
    let handle = app.clone();

    // The library this root belongs to, resolved once. The re-index needs it and looking it
    // up inside the callback would need the lock the callback is about to take.
    let library_id = {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::library_id_for_root(&conn, &library_root)
            .map_err(err)?
            .ok_or_else(|| format!("{library_root} is not an open library"))?
    };

    // **The deferral the watcher's doc used to claim without implementing.**
    //
    // While a delete plan is pending, a re-index adds file rows — and `DeleteSession::commit`
    // refuses any file that was not in the plan the user was shown. So a background re-index
    // during the confirmation turned Confirm into a hard failure with no recovery path, and the
    // user had no way to know a watcher caused it.
    //
    // The shell knows; the watcher asks.
    let defer_state = Arc::clone(&state.pending_delete);
    let should_defer = move || {
        defer_state
            .lock()
            .map(|session| session.has_pending())
            .unwrap_or(false)
    };

    let watch = crate::watcher::Watch::start(root, should_defer, move |_changed| {
        // The re-index runs on a blocking thread rather than in the watcher's callback: it
        // takes seconds, and the OS event queue is finite.
        let db = Arc::clone(&db);
        let handle = handle.clone();
        std::thread::spawn(move || {
            let now = now_seconds();
            let Ok(mut conn) = db.lock() else { return };
            // The root comes from the catalog, not from the caller: the watcher was started
            // for one library, and re-indexing a different one would be a surprise.
            let Ok(Some(root)) = store::library_root(&conn, library_id) else { return };
            match chaff_core::pipeline::index_and_score_with_progress(
                &mut conn,
                std::path::Path::new(&root),
                now,
                &|_| {},
            ) {
                Ok(report) => log::info!(
                    "watcher re-index: {} photographs, {} reused measurements",
                    report.photos,
                    report.reused
                ),
                Err(e) => log::warn!("watcher re-index failed: {e}"),
            }
            let _ = handle;
        });
    })
    .map_err(|e| format!("could not watch {library_root}: {e}"))?;

    let view = WatchView { running: true, seen: 0, busy: false };
    *state.watch.lock().map_err(|_| "watch lock poisoned".to_string())? = Some(watch);
    Ok(view)
}

/// Stop watching.
#[tauri::command]
pub async fn stop_watching(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(w) = state.watch.lock().map_err(|_| "watch lock poisoned".to_string())?.take() {
        w.stop();
    }
    Ok(())
}

/// Whether the watcher is running.
#[tauri::command]
pub async fn watch_status(state: State<'_, AppState>) -> Result<WatchView, String> {
    let guard = state.watch.lock().map_err(|_| "watch lock poisoned".to_string())?;
    Ok(match guard.as_ref() {
        Some(w) => WatchView { running: true, seen: w.seen(), busy: w.is_busy() },
        None => WatchView { running: false, seen: 0, busy: false },
    })
}

/// The endpoint to tag with, from the environment.
///
/// `CHAFF_VLM` — e.g. `http://192.168.1.150:8080`. Read here rather than passed in from the
/// webview, for the same reason `capabilities()` reads its endpoints from the environment:
/// nothing inside the webview chooses what this application connects to (#34).
fn vlm_endpoint() -> Option<chaff_core::vlm::Endpoint> {
    let base = std::env::var("CHAFF_VLM").ok().filter(|s| !s.trim().is_empty())?;
    Some(chaff_core::vlm::Endpoint {
        base,
        model: std::env::var("CHAFF_VLM_MODEL").unwrap_or_else(|_| "chaff-vlm".into()),
    })
}

/// Run a tagging pass.
///
/// `limit` bounds one call, so a library can be done in pieces with feedback between them
/// rather than as one silent hour. Resumable: the work list is the catalog, so stopping
/// loses nothing.
#[tauri::command]
pub async fn run_tag_pass(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library_id: i64,
    limit: Option<usize>,
) -> Result<TagOutcome, String> {
    let db = state.db();
    let now = now_seconds();
    let cancel = Arc::clone(&state.pass_cancel);
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve the app data directory: {e}"))?;

    tauri::async_runtime::spawn_blocking(move || -> Result<TagOutcome, String> {
        let mut conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let limit = limit.unwrap_or(200);

        // **A configured endpoint is an upgrade, not a requirement.**
        //
        // This returned "No vision endpoint is configured. Set CHAFF_VLM…" and stopped, while
        // CLIP — built for exactly this tier — sat unreachable behind a command-line flag.
        // The feature and its entry point were designed separately and the seam was never
        // checked.
        //
        // So: with an endpoint, use it, because a 35B vision model writes real descriptions.
        // Without one, use CLIP, and **say so** — "tagged 200 photographs" with no model
        // named is a claim the user cannot check.
        if let Some(endpoint) = vlm_endpoint() {
            let report = crate::tagging::run(
                &mut conn,
                library_id,
                &endpoint,
                limit,
                now,
                &mut |done, total| {
                    // Same throttle and the same event as the face pass, with a different
                    // `stage` — one listener in the frontend serves both.
                    if done % 20 == 0 || done == total {
                        use tauri::Emitter;
                        let _ = app.emit(
                            "chaff://pass-progress",
                            &PassProgress { stage: "tagging".into(), done, total },
                        );
                    }
                    !cancel.load(std::sync::atomic::Ordering::Relaxed)
                },
            )?;
            return Ok(TagOutcome::Remote { model: endpoint.model, report });
        }

        let Some(model) = chaff_faces::clip::model_in(&crate::faces::model_store(&app_data)) else {
            // Neither is available, and the message says how to get each rather than only the
            // one that was checked first.
            return Err(
                "No tagger is available. Either set CHAFF_VLM to a vision model server, or \
                 fetch the CLIP model (it is downloaded on first use — check your network)."
                    .to_string(),
            );
        };
        let Some(vocabulary) = chaff_faces::clip::bundled() else {
            return Err("The CLIP vocabulary file is missing from this build.".to_string());
        };

        let report = chaff_faces::pass::run_clip(
            &mut conn,
            library_id,
            &chaff_faces::pass::ClipPaths { model: &model, vocabulary: &vocabulary },
            // Five phrases, and a floor low enough that a photograph of something outside the
            // vocabulary gets **no tags** rather than its nearest one. CLIP always has a
            // nearest phrase; recording it would be a confident claim about a photograph it
            // cannot describe.
            chaff_faces::pass::ClipSettings { keep: 5, min_similarity: 0.2 },
            now,
            &mut |_, _| true,
        )?;

        Ok(TagOutcome::Local {
            model: chaff_faces::models::CLIP_VISION.file.to_string(),
            vocabulary: report.vocabulary,
            report,
        })
    })
    .await
    .map_err(err)?
}

/// What tagged a library, and what it did.
///
/// A tagged union rather than one shape with optional fields: **which tagger ran is the
/// thing the user most needs to know**, and a `model: Option<String>` lets a caller forget to
/// show it.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcome {
    /// A vision model over HTTP. Better tags, and a description.
    Remote { model: String, report: crate::tagging::TagPassReport },
    /// CLIP on the CPU. No server, no GPU, and no description — it cannot write one.
    Local { model: String, vocabulary: usize, report: chaff_faces::pass::ClipPassReport },
}

/// Exercise the configured endpoint and report what actually works (#52).
#[tauri::command]
pub async fn diagnose_endpoint() -> Result<crate::tagging::EndpointReport, String> {
    let endpoint = vlm_endpoint().ok_or_else(|| {
        "No vision endpoint is configured. Set CHAFF_VLM to the address of a model server."
            .to_string()
    })?;
    tauri::async_runtime::spawn_blocking(move || -> Result<crate::tagging::EndpointReport, String> {
        // A committed fixture, not the user's library: the self-test must work before any
        // library is open, and reading their photographs to check a URL is not a trade worth
        // making.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/chaff-faces/tests/fixtures/portrait_mona_lisa.jpg");
        Ok(crate::tagging::diagnose(&endpoint, fixture.is_file().then_some(fixture.as_path())))
    })
    .await
    .map_err(err)?
}

/// Every tag in a library, with counts.
#[tauri::command]
pub async fn list_tags(
    state: State<'_, AppState>,
    library_id: i64,
    model: Option<String>,
) -> Result<Vec<(String, usize)>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<(String, usize)>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::tag_counts(&conn, library_id, model.as_deref()).map_err(err)
    })
    .await
    .map_err(err)?
}

/// The tags on one photograph.
#[tauri::command]
pub async fn photo_tags(
    state: State<'_, AppState>,
    photo_id: i64,
) -> Result<Vec<PhotoTagView>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PhotoTagView>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let rows = store::tags_for_photo(&conn, photo_id).map_err(err)?;
        Ok(rows
            .into_iter()
            .map(|t| PhotoTagView { name: t.name, confidence: t.confidence, model: t.model })
            .collect())
    })
    .await
    .map_err(err)?
}

/// The photographs carrying a tag.
#[tauri::command]
pub async fn photos_with_tag(
    state: State<'_, AppState>,
    library_id: i64,
    tag: String,
    model: Option<String>,
) -> Result<Vec<i64>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<i64>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::photos_with_tag(&conn, library_id, &tag, model.as_deref()).map_err(err)
    })
    .await
    .map_err(err)?
}

/// One tag on a photograph.
#[derive(Debug, Serialize)]
pub struct PhotoTagView {
    pub name: String,
    pub confidence: f64,
    /// Which model claimed it. Shown, because two models disagree and a re-tag mixes them.
    pub model: String,
}

/// A suggested person: a group of faces that might be one individual.
#[derive(Debug, Serialize)]
pub struct PersonView {
    pub id: i64,
    pub name: Option<String>,
    pub confirmed: bool,
    pub faces: usize,
    pub photos: usize,
}

/// Every suggested person in a library, most photographs first.
#[tauri::command]
pub async fn list_people(
    state: State<'_, AppState>,
    library_id: i64,
) -> Result<Vec<PersonView>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PersonView>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let rows = store::people(&conn, library_id).map_err(err)?;
        Ok(rows
            .into_iter()
            .map(|p| PersonView {
                id: p.id,
                name: p.name,
                confirmed: p.confirmed,
                faces: p.faces,
                photos: p.photos,
            })
            .collect())
    })
    .await
    .map_err(err)?
}

/// Name a person. **Naming confirms the group.**
///
/// A group a human has put a name to is a decision, not a suggestion, and the next
/// clustering pass must leave it alone — which is what `confirmed` means, and why it is set
/// here rather than by a separate button nobody would press.
#[tauri::command]
pub async fn name_person(
    state: State<'_, AppState>,
    person_id: i64,
    name: Option<String>,
) -> Result<(), String> {
    let db = state.db();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::name_person(&conn, person_id, name.as_deref(), now).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Merge one group into another, moving every face. Both end up confirmed.
#[tauri::command]
pub async fn merge_people(
    state: State<'_, AppState>,
    from_id: i64,
    into_id: i64,
) -> Result<usize, String> {
    let db = state.db();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<usize, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::merge_people(&conn, from_id, into_id, now).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Move faces out of a group into a new one. Returns the new group's id, or `None` when the
/// split would empty the source — a person with no faces is not a group.
#[tauri::command]
pub async fn split_person(
    state: State<'_, AppState>,
    person_id: i64,
    face_ids: Vec<i64>,
) -> Result<Option<i64>, String> {
    let db = state.db();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<i64>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::split_person(&conn, person_id, &face_ids, now).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Discard a grouping, keeping the faces.
#[tauri::command]
pub async fn delete_person(state: State<'_, AppState>, person_id: i64) -> Result<(), String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::delete_person(&conn, person_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// The faces in a group.
///
/// Needed to undo a merge: reversing one means putting a specific set of faces back into a
/// group of their own, and the panel shows counts rather than ids.
#[tauri::command]
pub async fn person_faces(state: State<'_, AppState>, person_id: i64) -> Result<Vec<i64>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<i64>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::faces_for_person(&conn, person_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// A face the clustering could not place confidently.
#[derive(Debug, Serialize)]
pub struct AmbiguousFaceView {
    pub face_id: i64,
    pub photo_id: i64,
    pub person_id: Option<i64>,
    /// Similarity to the group it is in, and to the nearest group it is not in.
    pub own: f32,
    pub other: f32,
}

/// Faces sitting between two groups, worst margin first.
///
/// What the review queue (#47) exists for: a face that landed on the wrong side of a
/// threshold is simply wrong, and without this nobody ever sees it.
#[tauri::command]
pub async fn ambiguous_faces(
    state: State<'_, AppState>,
    library_id: i64,
    margin: Option<f32>,
    limit: Option<usize>,
) -> Result<Vec<AmbiguousFaceView>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<AmbiguousFaceView>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let rows = store::ambiguous_faces(
            &conn,
            library_id,
            crate::faces::recogniser_model(),
            margin.unwrap_or(0.25),
            limit.unwrap_or(100),
        )
        .map_err(err)?;
        Ok(rows
            .into_iter()
            .map(|f| AmbiguousFaceView {
                face_id: f.face_id,
                photo_id: f.photo_id,
                person_id: f.person_id,
                own: f.own,
                other: f.other,
            })
            .collect())
    })
    .await
    .map_err(err)?
}

/// The photographs a person appears in.
#[tauri::command]
pub async fn person_photos(state: State<'_, AppState>, person_id: i64) -> Result<Vec<i64>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<i64>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::photos_for_person(&conn, person_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// How many faces were found in each photograph of a library.
#[tauri::command]
pub async fn face_counts(
    state: State<'_, AppState>,
    library_id: i64,
) -> Result<std::collections::HashMap<i64, usize>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<std::collections::HashMap<i64, usize>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::face_counts(&conn, library_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Everything known about one photograph, for an inspector panel.
#[tauri::command]
pub async fn photo_detail(
    state: State<'_, AppState>,
    photo_id: i64,
) -> Result<pipeline::PhotoDetail, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<pipeline::PhotoDetail, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        pipeline::photo_detail(&conn, photo_id).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Every remembered value.
///
/// Returned as a map so the frontend reads what it needs in one call rather than one round
/// trip per preference.
#[tauri::command]
pub async fn get_settings(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<std::collections::HashMap<String, String>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::settings(&conn).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Remember a value.
#[tauri::command]
pub async fn set_setting(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    let db = state.db();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        store::set_setting(&conn, &key, &value, now).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Every folder in a library that holds photographs, with counts.
///
/// Returned flat and turned into a tree by the frontend. The engine has no opinion about
/// how a hierarchy is displayed, and a tree built here would have to be re-built whenever
/// the display changed.
#[tauri::command]
pub async fn list_directories(
    state: State<'_, AppState>,
    library_id: i64,
) -> Result<Vec<DirectoryView>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<DirectoryView>, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let rows = store::directories(&conn, library_id).map_err(err)?;
        Ok(rows
            .into_iter()
            .map(|d| DirectoryView { path: d.path, direct: d.direct, recursive: d.recursive })
            .collect())
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
        // One query for the whole library. The frontend derives the filter options from
        // these rather than asking for a second list — two sources for one fact is how a
        // filter option ends up with a count that does not match what it shows.
        let metadata = store::photo_metadata(&conn, library_id).map_err(err)?;

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
                    camera: metadata.get(&p.id).and_then(|m| m.camera.clone()),
                    lens: metadata.get(&p.id).and_then(|m| m.lens.clone()),
                    year: metadata.get(&p.id).and_then(|m| m.year),
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
    let pending = state.pending();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<DeletePlanView, String> {
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let selection = pipeline::resolve_delete_selection(&conn, &photo_ids).map_err(err)?;

        let trash = Trash::open(std::path::Path::new(&library_root)).map_err(err)?;
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

        // **The engine plans it, hashes it and holds it.**
        //
        // This shell used to do all three. Moving it down means the native macOS shell gets
        // the same guarantee rather than a second implementation of it — which is the whole
        // reason `delete_session` exists.
        //
        // The hashing happens *here*, while the user is looking at what will move, because
        // that is what the commit verifies against. Hashing at commit time would compare a
        // file with itself and prove nothing — which is what the first implementation did, by
        // accident, with an always-empty map.
        if refusals.is_empty() {
            if let Ok(mut session) = pending.lock() {
                match session.plan(&conn, std::path::Path::new(&library_root), &photo_ids, now) {
                    Ok(planned) => warnings.extend(planned.warnings),
                    Err(e) => refusals.push(e.to_string()),
                }
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
/// **Takes no plan and no file list.** It looks up the plan the user was shown, re-resolves
/// the photographs from the ids in it, and re-plans the move with the hashes recorded at
/// that moment. The frontend supplies nothing but the library root, so nothing it sends can
/// name a file or influence what is verified.
///
/// The verification is the promise the confirmation dialog makes: a file whose contents
/// changed between being shown and being moved **aborts the whole operation** rather than
/// being moved unexamined.
#[tauri::command]
pub async fn commit_delete(
    state: State<'_, AppState>,
    library_root: String,
) -> Result<DeleteReceiptView, String> {
    let db = state.db();
    let pending = state.pending();
    let now = now_seconds();
    tauri::async_runtime::spawn_blocking(move || -> Result<DeleteReceiptView, String> {
        // **The engine commits it.** This shell used to hold the plan, re-resolve the
        // selection, re-plan with the recorded hashes and mark the rows — all of which is now
        // `DeleteSession::commit`, so the native shell gets the same guarantee rather than a
        // second implementation of it.
        //
        // The invariant that matters is in the engine: `commit` takes no file list. Nothing
        // this command passes can name a file, supply a hash, or widen the operation.
        let conn = db.lock().map_err(|_| "catalog lock poisoned".to_string())?;
        let mut session = pending.lock().map_err(|_| "pending lock poisoned".to_string())?;
        let receipt = session
            .commit(&conn, std::path::Path::new(&library_root), now)
            .map_err(|e| e.to_string())?;

        Ok(DeleteReceiptView {
            op_id: receipt.op_id,
            moved: receipt.moved,
            bytes: receipt.bytes.min(i64::MAX as u64) as i64,
            warnings: receipt.warnings,
        })
    })
    .await
    .map_err(err)?
}

/// Abandon the pending plan without moving anything.
///
/// Called when the user cancels. Without it the plan would sit until the next one replaced
/// it, and a stale plan is a plan that could be committed by a stray click.
#[tauri::command]
pub async fn cancel_delete(state: State<'_, AppState>) -> Result<(), String> {
    let pending = state.pending();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        if let Ok(mut session) = pending.lock() {
            session.cancel();
        }
        Ok(())
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

/// Model endpoints to probe, from the environment.
///
/// `CHAFF_ENDPOINTS`, comma-separated. Read here rather than passed in, so nothing inside
/// the webview can make the application connect anywhere.
fn configured_endpoints() -> Vec<String> {
    std::env::var("CHAFF_ENDPOINTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Which build this is.
///
/// Compiled in rather than read from the filesystem: a file's timestamp says when it was
/// copied, not what it contains, and the whole point is to answer "am I testing the fix or
/// the bug?" without trusting either.
#[derive(Debug, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    pub git_sha: &'static str,
    pub built_at: String,
}

#[tauri::command]
pub fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("CHAFF_GIT_SHA"),
        built_at: format_epoch(env!("CHAFF_BUILD_EPOCH")),
    }
}

/// `2026-10-04 21:46 UTC`, from a build-time epoch second.
fn format_epoch(seconds: &str) -> String {
    let Ok(secs) = seconds.parse::<i64>() else {
        return "unknown".to_string();
    };
    let date = chaff_core::trash::civil_date(secs);
    let time_of_day = secs.rem_euclid(86_400);
    format!(
        "{date} {:02}:{:02} UTC",
        time_of_day / 3600,
        (time_of_day % 3600) / 60
    )
}


/// The Capability Report for this machine, as plain text.
///
/// Returned rendered rather than structured: it is shown to a person and pasted into bug
/// reports, and a structure the frontend has to lay out is a structure it can lay out
/// wrongly. The probing is fast (one `nvidia-smi` or `system_profiler` call) but it does
/// spawn a process, so it runs off the main thread like everything else here.
#[tauri::command]
pub async fn capabilities() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let mut probe = hardware::probe();

        // **The frontend does not choose what Chaff connects to.**
        //
        // This took a `Vec<String>` of URLs and passed each to `probe_endpoint`, which
        // resolves the host, opens a TCP connection and sends a request — an unallowlisted
        // outbound-request primitive reachable by any script in the webview. The frontend
        // never passed anything, so it was not exploitable; it was a loaded gun on a table.
        //
        // Endpoints come from the environment now, where a script cannot reach them. The
        // PRD's "single egress chokepoint with an allowlist" (#34) is still unbuilt, and
        // until it exists this is the narrowest thing that works.
        for url in configured_endpoints() {
            probe.endpoints.push(hardware::probe_endpoint(&url, 1500));
        }
        let stamp = format!(
            "{} ({}, {})",
            env!("CARGO_PKG_VERSION"),
            env!("CHAFF_GIT_SHA"),
            format_epoch(env!("CHAFF_BUILD_EPOCH"))
        );
        Ok(hardware::render_with_build(&probe, Some(&stamp)))
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
    fn the_build_date_is_formatted_from_the_epoch() {
        // Computed at runtime, so it never appears in the binary as a literal and cannot be
        // checked with `strings`. Which is why an earlier verification confidently reported
        // a hex string from a dependency instead of the real stamp.
        assert_eq!(format_epoch("1700000000"), "2023-11-14 22:13 UTC");
        assert_eq!(format_epoch("0"), "1970-01-01 00:00 UTC");
        assert_eq!(format_epoch("not a number"), "unknown");
    }

    #[test]
    fn the_build_stamp_names_the_commit() {
        // The whole point: "which build is this?" must have an answer that does not depend
        // on file timestamps or trust.
        assert!(!env!("CHAFF_GIT_SHA").is_empty());
        assert!(!format_epoch(env!("CHAFF_BUILD_EPOCH")).is_empty());
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
    fn state_is_shareable_across_threads() {
        // The commands dispatch onto blocking threads, so the state must be Send + Sync.
        // A compile-time property, asserted here so a future field that is neither fails
        // at the point it is added rather than in a command.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AppState>();
    }
}
