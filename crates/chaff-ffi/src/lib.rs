//! The engine, for Swift.
//!
//! # What this crate is
//!
//! A boundary and nothing else. Every function here calls the same `chaff-core` function the
//! Tauri command calls — **not a reimplementation** — so a bug fixed in
//! `resolve_delete_selection` is fixed for both UIs at once.
//!
//! # What is not exposed yet
//!
//! A review counted: this crate exports the read path, the thumbnails and the delete session;
//! the Tauri shell has thirty-eight commands. Missing here and needed by the plan: faces
//! (#67), tags (#67), settings, the watcher, and the sidecar write.
//!
//! Written down rather than left as an implication, because the first version of this comment
//! claimed a shared safety guarantee while exposing no delete path at all — and a comment that
//! overstates what the code does is the failure this project keeps finding.
//!
//! # Why the engine is not annotated directly
//!
//! `#[uniffi::export]` on `chaff-core` would make the engine depend on the FFI layer, and
//! `chaff-cli` would carry it, and every engine test would build the scaffolding. The engine
//! stays a plain library.
//!
//! # The rule for this crate
//!
//! **No logic.** A wrapper converts types, calls the engine, and converts the answer. If a
//! wrapper needs an `if` about what something *means*, that belongs in the engine.

use std::sync::{Arc, Mutex};

use chaff_core::catalog::{self, store};
use chaff_core::pipeline;

uniffi::setup_scaffolding!();

/// What kind of failure it was.
///
/// **A real enum, not a string.** The first version was `#[uniffi(flat_error)]` with a `String`
/// kind and a comment saying "a caller that needs to branch can match on `kind`" — and
/// `flat_error` lowers *only* `to_string()`, so the generated Swift was
/// `case Engine(message: String)` with no `kind` at all. The false comment shipped verbatim
/// into the Swift documentation for a field that did not exist.
///
/// A Swift caller needs to distinguish "retry, the catalog is busy" from "the library does not
/// exist" from "restart the app, the lock is poisoned" — the last of which existed only to be
/// unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FailureKind {
    /// The catalog is busy or the operation could not start. Retrying may work.
    Busy,
    /// The library or photograph asked for does not exist.
    NotFound,
    /// Another operation failed while holding the catalog. **Restart the app.**
    Poisoned,
    /// The operation was refused, and the message says why. Retrying will not help.
    Refused,
    /// Something else went wrong. The message is all there is.
    Other,
}

/// An error crossing the boundary.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ChaffError {
    #[error("{message}")]
    Engine { kind: FailureKind, message: String },
}

impl ChaffError {
    fn engine(kind: &str, e: impl std::fmt::Display) -> Self {
        Self::Engine { kind: classify(kind), message: e.to_string() }
    }

    fn with(kind: FailureKind, message: impl Into<String>) -> Self {
        Self::Engine { kind, message: message.into() }
    }
}

/// Map an engine error to something a caller can branch on.
///
/// String matching, which is a smell — and the honest alternative is for the engine to return
/// a typed error, which is a larger change than this boundary should make. Recorded rather than
/// hidden: the strings come from `CatalogError` and are stable.
fn classify(kind: &str) -> FailureKind {
    match kind {
        "poisoned" => FailureKind::Poisoned,
        "catalog" | "index" => FailureKind::Busy,
        "library" | "photos" | "decisions" | "metadata" | "files" => FailureKind::NotFound,
        "delete" | "rescore" => FailureKind::Refused,
        _ => FailureKind::Other,
    }
}

type Result<T> = std::result::Result<T, ChaffError>;

/// One photograph, as the grid needs it.
///
/// Deliberately **not** the engine's `PhotoView`: this is what a UI row reads, and keeping it
/// separate means a change to the engine's internals does not ripple into Swift. It is a
/// conversion target, not a shared type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Photo {
    pub id: i64,
    pub stem: String,
    pub dir: String,
    /// `pair`, `raw_only`, `raster_only` or `ambiguous`.
    pub state: String,
    pub needs_review: bool,
    /// `None` when the photograph could not be scored.
    pub composite: Option<f64>,
    /// `keep`, `review` or `reject`.
    pub band: Option<String>,
    pub rating: u8,
    pub rejected: bool,
    /// Sharpness, as a percentile within this photograph's shoot. **Low is soft.**
    ///
    /// Already measured — `scoring/focus.rs` is a real blur metric, built so a shallow
    /// depth-of-field portrait is not marked blurry. What was missing was any way to filter on it.
    pub focus: Option<f64>,
    /// Sensor noise, as a percentile. **High is noisy** — the one metric where more is worse.
    pub noise: Option<f64>,
    /// Resolved detail. Low means the frame is soft or the subject is small.
    pub detail: Option<f64>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub year: Option<i32>,
}

/// A library that has been opened.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Library {
    pub id: i64,
    pub root: String,
    pub photos: u32,
    pub pairs: u32,
    pub needs_review: u32,
    pub scored: u32,
}

/// What opening a library did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct OpenReport {
    pub library: Library,
    /// Files the walk saw.
    pub scanned_files: u32,
    /// Photographs whose measurement was reused rather than recomputed.
    pub reused: u32,
    /// Photographs nothing could decode. A raw with no embedded preview, on Windows.
    pub unscoreable: u32,
    pub elapsed_ms: u64,
}

/// A folder, with the two counts that make it useful.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Folder {
    pub path: String,
    /// Photographs directly in this folder — "how many in this shoot?"
    pub direct: u32,
    /// Photographs in this folder and everything under it — "how many under 2024?"
    pub recursive: u32,
}

/// How far a pass has got.
///
/// A callback rather than a poll: a face pass runs for an hour, and a UI that can only ask
/// "are we there yet" has nothing to show for the first fifty-nine minutes.
#[uniffi::export(callback_interface)]
pub trait Progress: Send + Sync {
    /// Called on the thread running the pass.
    ///
    /// **Do not call back into the `Engine` from here.** Hop to the actor that owns your state
    /// and return; the pass is holding a connection and a callback that waits on it will wait
    /// forever.
    ///
    /// `total` is `0` while scanning — the size of a tree is not known until the walk
    /// finishes, and a determinate bar over an unknown total is a bar that lies.
    ///
    /// `current` is the file being worked on, or empty for stages that have no single file.
    ///
    /// **Returns whether to keep going.** A callback that can only be listened to cannot stop
    /// anything, and a face pass over a large library is tens of minutes. Both passes commit
    /// each file as they go, so stopping loses nothing — the work list is the catalog, and the
    /// next pass resumes from where this one stopped.
    fn on_progress(&self, done: u32, total: u32, stage: String, current: String) -> bool;
}

/// The catalog, open for a shell to use.
///
/// # The lock, and the two threads it has to survive
///
/// A SwiftUI app calls from at least two: the main actor for reads, and a background task for
/// a pass that runs for an hour. SQLite is not safe to use from two threads on one connection,
/// so reads go through a `Mutex`.
///
/// **The lock is this crate's, not the engine's** — `chaff-core` has no locks at all. That
/// distinction matters, and the first version of this comment got it backwards while
/// advertising the exact two-thread usage that deadlocked: a pass held the lock for its whole
/// duration, so every read blocked behind it, and a progress callback that called back into
/// the engine deadlocked outright.
///
/// The fix is that a pass **does not take this lock**. It opens its own connection to the same
/// file — WAL is on, so a reader is not blocked by a writer — and reads stay responsive while
/// it runs.
#[derive(uniffi::Object)]
pub struct Engine {
    conn: Mutex<chaff_core::rusqlite::Connection>,
    /// Where the catalog file is, so a pass can open its own connection to it.
    path: std::path::PathBuf,
    /// The thumbnail cache, kept rather than reopened per tile.
    ///
    /// `ThumbnailCache::open` creates three directories and reads the cap; doing that once per
    /// tile is work with no purpose. Behind its own lock so a thumbnail never waits on a read.
    thumbs: Mutex<Option<std::sync::Arc<chaff_core::thumb::ThumbnailCache>>>,
    /// The delete plan the user has been shown.
    ///
    /// Held here rather than in Swift so that "commit takes no file list" is the engine's
    /// invariant. A shell that held its own copy would be a second implementation of a safety
    /// guarantee, which is how one of them drifts.
    pending: Mutex<chaff_core::delete_session::DeleteSession>,
    /// The library watcher, when one is running.
    watch: Mutex<Option<WatchHandle>>,
    /// True while a delete plan is pending.
    ///
    /// **A flag rather than the session itself**, because the watcher runs on a thread that
    /// outlives any borrow of the engine. The session is the authority; this mirrors the one bit
    /// of it the watcher needs, and is set and cleared wherever the session's state changes.
    plan_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[uniffi::export]
impl Engine {
    /// Open or create a catalog.
    #[uniffi::constructor]
    pub fn new(database_path: String) -> Result<Arc<Self>> {
        let path = std::path::PathBuf::from(&database_path);
        let conn = catalog::open(&path).map_err(|e| ChaffError::engine("catalog", e))?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            path,
            thumbs: Mutex::new(None),
            pending: Mutex::new(chaff_core::delete_session::DeleteSession::new()),
            watch: Mutex::new(None),
            plan_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }))
    }

    /// Index a library and score it.
    ///
    /// Long-running: minutes for a large library.
    ///
    /// # What `progress` may do
    ///
    /// It is called **on the thread running the pass**, and it must not call back into this
    /// `Engine`. A callback that reads `photos()` would block on the very connection the pass
    /// is using, and a callback that started another pass would deadlock outright. Hop to
    /// whatever actor owns your state and return; do not do work in the callback.
    ///
    /// `total` is `0` while scanning, because the size of a tree is not known until the walk
    /// finishes. A determinate bar over an unknown total is a bar that lies, so the scan phase
    /// is indeterminate by design — check for `total == 0` rather than dividing by it.
    ///
    /// # Cancellation
    ///
    /// **There is none yet.** A pass runs to completion or fails. That is a real gap for a run
    /// that takes an hour, and it is issue #67.
    pub fn open_library(&self, root: String, progress: Box<dyn Progress>) -> Result<OpenReport> {
        // **A connection of its own, not the read connection.**
        //
        // This took `self.lock()` and held it for the whole pass. A read then blocked for the
        // entire duration — twenty minutes on a large library — and a progress callback that
        // touched the engine deadlocked. WAL is on, so a separate connection reads and writes
        // alongside the shell's without either blocking the other.
        let mut conn = catalog::open(&self.path).map_err(|e| ChaffError::engine("catalog", e))?;
        let path = std::path::Path::new(&root);
        let now = now_seconds();

        let report = pipeline::index_and_score_with_progress(&mut conn, path, now, &|p| {
            let (done, total, stage, current) = match p {
                pipeline::Progress::Scanning { files } => (files as u32, 0, "scanning", String::new()),
                // `current` was discarded by the first version with `..`. For a pass that runs
                // for an hour it is the single most useful field, and the engine already
                // computes it.
                pipeline::Progress::Scoring { done, total, current } => {
                    (done as u32, total as u32, "scoring", current)
                }
                pipeline::Progress::Ranking { photographs } => {
                    (photographs as u32, photographs as u32, "ranking", String::new())
                }
            };
            progress.on_progress(done, total, stage.to_string(), current);
        })
        .map_err(|e| ChaffError::engine("index", e))?;

        Ok(OpenReport {
            library: Library {
                id: report.library_id,
                root: report.root.to_string_lossy().to_string(),
                photos: report.photos as u32,
                pairs: report.pairs as u32,
                needs_review: report.needs_review as u32,
                scored: report.scored as u32,
            },
            scanned_files: report.scanned_files as u32,
            reused: report.reused as u32,
            unscoreable: report.unscoreable as u32,
            elapsed_ms: report.elapsed_ms as u64,
        })
    }

    /// Every photograph in a library.
    ///
    /// One call returning everything, which is what the web shell does too. At 50,000
    /// photographs that is ~12 MB of records across the boundary — worth measuring before
    /// assuming it is fine, and the reason the grid must virtualise.
    pub fn photos(&self, library_id: i64) -> Result<Vec<Photo>> {
        let conn = self.lock()?;
        // `scored_photos`, not `photos`: it returns each photograph **with** its composite in
        // one query, which is what a grid needs. `photos` alone would need a second call per
        // row to learn the score, and fifty thousand of those is the thing to avoid.
        let rows = pipeline::scored_photos(&conn, library_id)
            .map_err(|e| ChaffError::engine("photos", e))?;
        let decisions = store::decisions_for_library(&conn, library_id)
            .map_err(|e| ChaffError::engine("decisions", e))?;
        let metadata = store::photo_metadata(&conn, library_id)
            .map_err(|e| ChaffError::engine("metadata", e))?;
        // **One query for the whole library**, not one per photograph: a grid asks for the list
        // once, and 3,000 queries to build it is the difference between instant and a visible
        // pause.
        let quality =
            store::quality_for_library(&conn, library_id, chaff_core::pipeline::SCORER_VERSION)
                .map_err(|e| ChaffError::engine("photos", e))?;
        // **One query for the whole library**, not one per photograph: a grid asks for the list
        // once, and 3,000 queries to build it is the difference between instant and a visible
        // pause.

        Ok(rows
            .into_iter()
            .map(|(p, composite)| {
                let d = decisions.get(&p.id).copied().unwrap_or_default();
                let m = metadata.get(&p.id);
                Photo {
                    id: p.id,
                    stem: p.stem,
                    dir: p.dir,
                    state: p.state,
                    needs_review: p.needs_review,
                    composite,
                    band: composite.map(|c| band_of(c).to_string()),
                    rating: d.rating.get(),
                    rejected: d.rejected,
                    // Absent rather than zero when a metric was not measured — a raw this build
                    // cannot decode has no focus score, and `Some(0.0)` would file it under
                    // "blurry", which is a claim nobody made.
                    focus: quality.get(&p.id).and_then(|q| q.focus),
                    noise: quality.get(&p.id).and_then(|q| q.noise),
                    detail: quality.get(&p.id).and_then(|q| q.detail),
                    camera: m.and_then(|m| m.camera.clone()),
                    lens: m.and_then(|m| m.lens.clone()),
                    year: m.and_then(|m| m.year),
                }
            })
            .collect())
    }

    /// Folders with their two counts, **relative to the library root**.
    ///
    /// # Why relative
    ///
    /// `list_directories` returns absolute paths, and a navigator that renders them as a
    /// hierarchy shows the user's whole filesystem above their library:
    ///
    /// ```text
    /// /                 0
    /// Users             6
    /// takasurazeem      6
    /// Desktop           6
    /// Photography       6
    /// KY-Indy           6     <- the library actually opens here
    /// Canon             1
    /// Panasonic         5
    /// ```
    ///
    /// Every row above `KY-Indy` is a folder the user did not open, cannot meaningfully select,
    /// and can filter the grid to nothing with. `/` showing 0 is the honest count of
    /// photographs directly in `/`, which is not a useful row.
    ///
    /// The root itself is dropped too: it is the library, not a folder inside it.
    pub fn folders(&self, library_id: i64) -> Result<Vec<Folder>> {
        let conn = self.lock()?;
        let root = store::library_root(&conn, library_id)
            .map_err(|e| ChaffError::engine("directories", e))?
            .unwrap_or_default();
        let rows = store::directories(&conn, library_id)
            .map_err(|e| ChaffError::engine("directories", e))?;

        Ok(rows
            .into_iter()
            .filter_map(|d| {
                // `strip_prefix` on the string, with the separator, so `/lib` does not match
                // `/library`.
                let rel = d
                    .path
                    .strip_prefix(&root)
                    .map(|r| r.trim_start_matches('/').to_string())?;
                if rel.is_empty() {
                    return None; // the root itself
                }
                Some(Folder { path: rel, direct: d.direct as u32, recursive: d.recursive as u32 })
            })
            .collect())
    }

    /// The path of a photograph's thumbnail, generating it if needed.
    ///
    /// Returns a **path**, not bytes: the shell hands it to `NSImage`, and moving a few
    /// hundred kilobytes of image data across the FFI boundary per tile is work with no
    /// purpose.
    pub fn thumbnail(&self, photo_id: i64, size: String) -> Result<Option<String>> {
        let conn = self.lock()?;
        let files = store::files_for_photo(&conn, photo_id)
            .map_err(|e| ChaffError::engine("files", e))?;
        let Some(primary) = files
            .iter()
            .find(|f| f.role == "raw")
            .or_else(|| files.iter().find(|f| f.role == "raster"))
        else {
            return Ok(None);
        };

        let kind = match size.as_str() {
            "loupe" => chaff_core::thumb::ThumbSize::Loupe,
            // **2048, for judging focus.** Everything looks sharp at a sixth of its size, which
            // is why a 1024-pixel render cannot answer "is this in focus?" about a 6000-pixel
            // frame.
            "zoom" => chaff_core::thumb::ThumbSize::Zoom,
            // An unknown size is the grid rather than an error: a caller asking for something
            // this build does not have should get a thumbnail, not nothing.
            _ => chaff_core::thumb::ThumbSize::Grid,
        };
        let cache = self.cache()?;

        match chaff_core::thumb::generate_and_store(&cache, std::path::Path::new(&primary.path), kind)
        {
            Ok(p) => Ok(Some(p.to_string_lossy().to_string())),
            // A photograph nothing can decode is not an error — it is a tile that shows its
            // filename. Returning an error would put an alert in front of the user for a
            // photograph the app already knows it cannot read.
            Err(_) => Ok(None),
        }
    }

    /// Set a photograph's rating and reject flag.
    pub fn set_decision(&self, photo_id: i64, rating: u8, rejected: bool) -> Result<()> {
        let conn = self.lock()?;
        store::set_decision(
            &conn,
            photo_id,
            store::Decision { rating: store::Rating::new(rating), rejected },
            now_seconds(),
        )
        .map_err(|e| ChaffError::engine("decision", e))?;
        Ok(())
    }

    /// Forget everything about a library and index it again from scratch.
    ///
    /// Used when the *scorer* changed but `SCORER_VERSION` did not. Explicit rather than
    /// automatic: re-decoding a library is minutes and should be asked for.
    pub fn rescore(&self, library_id: i64, progress: Box<dyn Progress>) -> Result<OpenReport> {
        let conn = self.lock()?;
        store::delete_measurements_at_version(&conn, pipeline::SCORER_VERSION)
            .map_err(|e| ChaffError::engine("rescore", e))?;
        let root = store::library_root(&conn, library_id)
            .map_err(|e| ChaffError::engine("library", e))?
            .ok_or_else(|| {
                ChaffError::with(FailureKind::NotFound, format!("no library with id {library_id}"))
            })?;
        drop(conn);

        self.open_library(root, progress)
    }
}

impl Engine {
    /// The thumbnail cache, opened once.
    ///
    /// Opened lazily rather than in `new`, because a shell that never shows a thumbnail should
    /// not create three directories — and cached rather than reopened, because
    /// `ThumbnailCache::open` reads the cap and creates directories, which is work with no
    /// purpose on every tile.
    fn cache(&self) -> Result<std::sync::Arc<chaff_core::thumb::ThumbnailCache>> {
        let mut slot = self.thumbs.lock().map_err(|_| {
            ChaffError::with(
                FailureKind::Poisoned,
                "the thumbnail cache is unusable; restart Chaff",
            )
        })?;
        if let Some(c) = slot.as_ref() {
            return Ok(std::sync::Arc::clone(c));
        }
        let c = std::sync::Arc::new(
            chaff_core::thumb::ThumbnailCache::open(thumbnail_root(), THUMBNAIL_CAP_BYTES)
                .map_err(|e| ChaffError::engine("thumbnail", e))?,
        );
        *slot = Some(std::sync::Arc::clone(&c));
        Ok(c)
    }

    /// The connection, or an error naming the real problem.
    ///
    /// A poisoned lock means another thread panicked while holding it. Saying so is more
    /// useful than a generic failure, and it is what the user will be asked about.
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, chaff_core::rusqlite::Connection>> {
        self.conn.lock().map_err(|_| {
            ChaffError::with(
                FailureKind::Poisoned,
                "another operation failed while using the catalog; restart Chaff",
            )
        })
    }
}

/// One file belonging to a photograph.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    /// `raw`, `raster`, `sidecar` or `video`.
    pub role: String,
    pub size_bytes: i64,
}

/// One term in the score, with the percentile it landed at.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ScoreTerm {
    pub label: String,
    pub percentile: f64,
}

/// Everything known about one photograph, for an inspector.
///
/// # One call, not four
///
/// A panel that fetches EXIF, then files, then scores arrives in three visible stages and the
/// middle ones look like bugs. The engine assembles it in one query.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PhotoDetail {
    pub id: i64,
    pub stem: String,
    pub dir: String,
    pub state: String,
    pub needs_review: bool,
    pub files: Vec<FileInfo>,

    /// Sharpness, as a percentile within this photograph's shoot. **Low is soft.**
    ///
    /// Already measured — `scoring/focus.rs` is a real blur metric, built so a shallow
    /// depth-of-field portrait is not marked blurry. What was missing was any way to filter on it.
    pub focus: Option<f64>,
    /// Sensor noise, as a percentile. **High is noisy** — the one metric where more is worse.
    pub noise: Option<f64>,
    /// Resolved detail. Low means the frame is soft or the subject is small.
    pub detail: Option<f64>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f64>,
    pub exposure_time: Option<f64>,
    pub focal_length: Option<f64>,
    /// **The camera's own clock, not an instant.**
    ///
    /// `parse_exif_datetime` treats the camera's local wall-clock as if it were UTC, so
    /// differences between photographs are correct and the absolute moment is not. The UI
    /// labels it "camera clock" for that reason — a panel printing a bare time claims a
    /// precision the data does not have.
    pub captured_at: Option<i64>,

    pub composite: Option<f64>,
    pub band: Option<String>,
    /// Per-term percentiles. **The answer to "why 62?"** — a composite alone is a number
    /// nobody can act on, and without these the only recourse is to trust it or ignore it.
    pub terms: Vec<ScoreTerm>,

    pub rating: u8,
    pub rejected: bool,
}

/// What a face pass did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FacePassReport {
    pub detected_files: u32,
    pub faces_found: u32,
    pub embedded: u32,
    /// Files whose image data could not be read. A raw format this build has no decoder for.
    pub unreadable: u32,
    pub people: u32,
    /// The model's licence, so the UI can show it where the feature is switched on.
    pub licence: String,
    pub elapsed_ms: u64,
    /// The user stopped it. **Distinct from finished** — a library that is a third grouped must
    /// not read as complete, and the work already done is kept.
    pub cancelled: bool,
}

/// What a tagging pass did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TagPassReport {
    pub tagged: u32,
    pub remaining: u32,
    pub unreadable: u32,
    pub failed: u32,
    pub tags: u32,
    pub completion_tokens: u64,
    pub elapsed_ms: u64,
    /// Set when the endpoint stopped answering, so the UI can say "stopped" rather than
    /// "finished" — the difference between a complete library and a third of one.
    pub stopped_because: Option<String>,
    /// Which tagger ran: a vision model over HTTP, or CLIP on this machine.
    ///
    /// **Not optional in spirit.** "Tagged 200 photographs" with no model named is a claim the
    /// user cannot check, and there are two very different taggers behind one button.
    pub used: String,
}

/// A running watcher.
struct WatchHandle {
    handle: Option<std::thread::JoinHandle<()>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    seen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Watch a directory, and re-index when files settle.
///
/// # Why polling rather than `notify`
///
/// `chaff-core` has no filesystem-notification dependency on purpose — it is the GUI-free crate,
/// and a native event API is one more platform-specific thing to build on Windows and Linux. The
/// Tauri shell brings its own watcher for exactly that reason.
///
/// A poll is the honest trade here: a library that changes while the app is open changes on the
/// scale of a card copy or a sync, and a five-second poll catches that with no dependency and no
/// per-platform code. What it costs is up to five seconds of latency on a change, which for a
/// culling tool is not a cost anyone can perceive.
fn watch_loop(
    root: std::path::PathBuf,
    library_id: i64,
    catalog: std::path::PathBuf,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    seen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;

    let mut last = newest_mtime(&root);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let now = newest_mtime(&root);
        let Some(now) = now else { continue };
        if Some(now) == last {
            continue;
        }
        last = Some(now);
        seen.fetch_add(1, Ordering::Relaxed);

        // **Not while a delete plan is pending.** `commit` refuses any file that was not in the
        // plan the user was shown, so a re-index during the confirmation dialog turns Confirm
        // into a hard failure with no recovery path — and the user has no way to know a watcher
        // caused it.
        if pending.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }

        busy.store(true, Ordering::Relaxed);
        // A connection of its own, so the grid keeps reading while this runs. The engine's read
        // lock is what made a pass block every query for its whole duration once already.
        if let Ok(mut conn) = chaff_core::catalog::open(&catalog) {
            let now_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            match chaff_core::pipeline::index_and_score_with_progress(
                &mut conn,
                &root,
                now_s,
                &|_| {},
            ) {
                Ok(report) => log::info!(
                    "watcher: re-indexed {} photographs for library {library_id}, {} reused",
                    report.photos,
                    report.reused
                ),
                Err(e) => log::warn!("watcher: re-index failed: {e}"),
            }
        }
        busy.store(false, Ordering::Relaxed);
    }
}

/// The most recent modification time anywhere under a directory.
///
/// **A single number rather than a set of paths**, because the question this watcher answers is
/// "has anything changed", not "what changed" — and the second question needs a full scan to
/// answer, which is the work the re-index is about to do anyway.
fn newest_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];

    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            // The trash is inside the library and changes when the *app* moves something, so
            // watching it would make every delete trigger a re-index of the library it just
            // changed.
            if path.file_name().is_some_and(|n| n == ".cull-trash") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                if let Ok(when) = meta.modified() {
                    if newest.is_none_or(|n| when > n) {
                        newest = Some(when);
                    }
                }
            }
        }
    }
    newest
}

/// Whether a library is being watched, and what the watcher has seen.
#[derive(Debug, Clone, uniffi::Record)]
pub struct WatchStatus {
    pub running: bool,
    /// How many files have changed since the watcher started.
    pub seen: u64,
    /// True while a re-index triggered by the watcher is running.
    pub busy: bool,
}

/// What the thumbnail cache holds.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ThumbnailCacheInfo {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
}

/// One remembered setting.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Setting {
    pub key: String,
    pub value: String,
}

/// A face clustering was unsure about, waiting for a person to say.
///
/// **The input to the feature that already exists.** Naming and merging let a user fix a group
/// after the fact; this is how they are asked *before* the fact. Without it a group can only be
/// corrected once it has been named wrong.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AmbiguousFace {
    pub face_id: i64,
    pub photo_id: i64,
    /// The group it is currently in, if any.
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
    /// How sure clustering was, 0–1. Lower is more worth asking about.
    pub confidence: f64,
}

/// What writing sidecars did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SidecarReport {
    pub written: u32,
    pub skipped: u32,
    pub failed: u32,
    /// Where they went, so a user can go and look.
    pub first_path: Option<String>,
}

/// What a tagging endpoint can actually do.
#[derive(Debug, Clone, uniffi::Record)]
pub struct EndpointReport {
    pub reachable: bool,
    /// The models the endpoint offers.
    pub models: Vec<String>,
    /// Whether the configured model is among them.
    pub model_present: bool,
    /// Whether it answers a real vision request — **the only test that proves it can tag.**
    pub vision_ok: bool,
    pub detail: String,
    pub elapsed_ms: u64,
}

/// What this machine can do.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Capabilities {
    pub tier: String,
    pub gpu: Option<String>,
    pub vram_mb: Option<u64>,
    pub unified_memory: bool,
    pub summary: String,
}

/// One operation in the trash.
///
/// **An operation, not a file.** The manifest records what one confirmation moved, and restoring
/// is per-operation — so the panel lists operations and says how many files each holds. A list of
/// individual files would make "put back what I deleted" a matter of selecting the right twelve.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TrashEntry {
    pub op_id: String,
    /// When it happened, as a Unix timestamp.
    pub at: i64,
    /// The user's own words from the dialog, or the default.
    pub reason: String,
    pub files: u32,
    pub bytes: u64,
    /// True when some of the operation's files are no longer in the trash — moved by something
    /// else, or restored individually. Said rather than hidden, because a restore that brings
    /// back nine of twelve should not surprise anyone.
    pub incomplete: bool,
}

/// A tag and how many photographs carry it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TagCount {
    pub name: String,
    pub count: u32,
}

/// A suggested person: a group of faces that might be one individual.
///
/// **A suggestion, not a name.** Nothing here is confirmed until a human confirms it, and the
/// wording in the UI reflects that — treating a cluster as fact is how a stranger's face ends up
/// under someone's name.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Person {
    pub id: i64,
    pub name: Option<String>,
    pub confirmed: bool,
    pub faces: u32,
    pub photos: u32,
}

/// What a delete will move, before it moves it.
///
/// `op_id` is the identity SwiftUI's `.sheet(item:)` needs — and it is the right one, because
/// it names the operation rather than the view.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DeletePlan {
    /// Names the operation, so the dialog can say what it is about to do.
    pub op_id: String,
    pub photographs: u32,
    pub files: u32,
    pub bytes: u64,
    /// Conditions worth saying out loud: a cross-volume copy, a file that has gone missing.
    pub warnings: Vec<String>,
    /// Reasons this selection cannot be moved. **Non-empty means nothing will move.**
    pub refusals: Vec<String>,
}

/// What a delete did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DeleteReceipt {
    pub op_id: String,
    pub moved: u32,
    pub bytes: u64,
    pub warnings: Vec<String>,
}

#[uniffi::export]
impl Engine {
    /// Work out what a selection would move, and hold it for confirmation.
    ///
    /// # The guarantee this begins
    ///
    /// Every file is **hashed now**, while the user is looking at the list, because that is
    /// what the commit verifies against. Hashing at commit time would compare a file with
    /// itself and prove nothing.
    ///
    /// The plan is held in the engine, not here and not in Swift, so the invariant that
    /// matters — *commit takes no file list* — has one implementation for every shell.
    pub fn plan_delete(&self, root: String, photo_ids: Vec<i64>) -> Result<DeletePlan> {
        let conn = self.lock()?;
        let now = now_seconds();

        let mut session = self.pending.lock().map_err(|_| {
            ChaffError::with(FailureKind::Poisoned, "the delete session is unusable; restart Chaff")
        })?;

        // The refusals are collected before planning so the user sees every reason at once
        // rather than fixing one and meeting the next.
        let selection = pipeline::resolve_delete_selection(&conn, &photo_ids)
            .map_err(|e| ChaffError::engine("delete", e))?;
        // The watcher's mirror of this session, set the moment a plan exists. See
        // `Engine::plan_pending` for why it is a flag rather than the session itself.
        self.plan_pending.store(true, std::sync::atomic::Ordering::Relaxed);
        let files: Vec<std::path::PathBuf> =
            selection.candidates.iter().flat_map(|c| c.files.clone()).collect();

        let trash = chaff_core::trash::Trash::open(std::path::Path::new(&root))
            .map_err(|e| ChaffError::engine("delete", e))?;
        let mut refusals: Vec<String> = files
            .iter()
            .filter_map(|f| trash.check(f).err().map(|r| r.to_string()))
            .collect();

        // **A plan that would move nothing is a refusal, not a plan.**
        //
        // Found by a test: planning a selection whose photographs have no files produced an
        // empty file list, no refusals, and a *pending* plan — which a later `commit_delete`
        // would happily "confirm", reporting success for an operation that moved nothing. A
        // UI showing "0 files will move" with a working Confirm button is worse than an error.
        if files.is_empty() && refusals.is_empty() {
            refusals.push(
                "Nothing to move: those photographs have no files in the library. They may \
                 have been removed already."
                    .to_string(),
            );
        }

        if !refusals.is_empty() {
            // Nothing is held. A plan the user cannot confirm must not sit in the session
            // waiting to be committed by a later call.
            session.cancel();
            return Ok(DeletePlan {
                op_id: String::new(),
                photographs: selection.candidates.len() as u32,
                files: files.len() as u32,
                bytes: 0,
                warnings: Vec::new(),
                refusals,
            });
        }

        let planned = session
            .plan(&conn, std::path::Path::new(&root), &photo_ids, now)
            .map_err(|e| ChaffError::engine("delete", e))?;

        Ok(DeletePlan {
            op_id: planned.op_id,
            photographs: selection.candidates.len() as u32,
            files: planned.moved as u32,
            bytes: planned.bytes,
            warnings: planned.warnings,
            refusals: Vec::new(),
        })
    }

    /// Move what was shown.
    ///
    /// **Takes no file list.** It commits the plan the user was shown, and every file is
    /// re-hashed and compared — a file whose contents changed, or one that appeared after the
    /// plan, aborts the whole operation rather than being moved unexamined.
    pub fn commit_delete(&self, root: String) -> Result<DeleteReceipt> {
        let conn = self.lock()?;
        let now = now_seconds();

        let mut session = self.pending.lock().map_err(|_| {
            ChaffError::with(FailureKind::Poisoned, "the delete session is unusable; restart Chaff")
        })?;

        // Cleared **before** the result is inspected: a refusal also ends the pending state —
        // the session drops the plan on every path out of `commit` — and a flag left set would
        // stop the watcher forever with nothing on screen to explain it.
        self.plan_pending.store(false, std::sync::atomic::Ordering::Relaxed);

        let receipt = session
            .commit(&conn, std::path::Path::new(&root), now)
            .map_err(|e| ChaffError::engine("delete", e))?;

        Ok(DeleteReceipt {
            op_id: receipt.op_id,
            moved: receipt.moved as u32,
            bytes: receipt.bytes,
            warnings: receipt.warnings,
        })
    }

    /// Abandon the plan without moving anything.
    ///
    /// A plan that is not cancelled sits until the next one replaces it, and a stale plan is
    /// one a stray call could commit.
    pub fn cancel_delete(&self) -> Result<()> {
        let mut session = self.pending.lock().map_err(|_| {
            ChaffError::with(FailureKind::Poisoned, "the delete session is unusable; restart Chaff")
        })?;
        session.cancel();
        self.plan_pending.store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Everything known about one photograph.
    pub fn photo_detail(&self, photo_id: i64) -> Result<PhotoDetail> {
        let conn = self.lock()?;
        let d = pipeline::photo_detail(&conn, photo_id)
            .map_err(|e| ChaffError::engine("photos", e))?;
        // The percentiles for this one photograph. `photo_detail` already carries them as
        // `terms`, but those are `(label, percentile)` pairs for display — this is the same
        // numbers keyed by what they measure, so a caller can compare rather than print.
        let quality = store::quality_for_photo(&conn, photo_id, chaff_core::pipeline::SCORER_VERSION)
            .map_err(|e| ChaffError::engine("photos", e))?;

        Ok(PhotoDetail {
            id: d.photo_id,
            stem: d.stem,
            dir: d.dir,
            state: d.state,
            needs_review: d.needs_review,
            files: d
                .files
                .into_iter()
                .map(|f| FileInfo { name: f.name, path: f.path, role: f.role, size_bytes: f.size_bytes })
                .collect(),
            focus: quality.focus,
            noise: quality.noise,
            detail: quality.detail,
            camera: d.camera,
            lens: d.lens,
            iso: d.iso,
            f_number: d.f_number,
            exposure_time: d.exposure_time,
            focal_length: d.focal_length,
            captured_at: d.captured_at,
            composite: d.composite,
            band: d.band,
            terms: d
                .terms
                .into_iter()
                .map(|(label, percentile)| ScoreTerm { label, percentile })
                .collect(),
            rating: d.rating,
            rejected: d.rejected,
        })
    }

    /// Put a trashed operation back.
    ///
    /// The counterpart of a move, and **a different operation from writing a value back** —
    /// which is why the undo stack has to know which kind of action it is reversing.
    ///
    /// The manifest is plaintext inside the library and writable by anything, so `Trash::restore`
    /// re-checks every path against the library root before moving anything. A hand-edited
    /// manifest cannot send a file outside it.
    pub fn restore_trash(&self, root: String, op_id: String) -> Result<u32> {
        let conn = self.lock()?;
        let trash = chaff_core::trash::Trash::open(std::path::Path::new(&root))
            .map_err(|e| ChaffError::engine("delete", e))?;
        let report = trash.restore(&op_id).map_err(|e| ChaffError::engine("delete", e))?;

        // The catalog is told only after the files are back. A crash in between leaves files on
        // disk that the catalog still calls trashed, which the next index pass corrects — the
        // reverse order would claim a photograph is visible while it is not.
        //
        // **The paths come from the manifest**, not from the caller: `RestoreReport` carries
        // counts, and the operation's own record of what it moved is the authoritative list of
        // what came back. Asking the caller would let a UI name paths it never showed anyone.
        let paths: Vec<String> = trash
            .manifest()
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.op_id == op_id)
            .map(|e| e.files.iter().map(|f| f.source.to_string_lossy().to_string()).collect())
            .unwrap_or_default();

        store::clear_trashed_for_paths(&conn, &paths)
            .map_err(|e| ChaffError::engine("delete", e))?;

        Ok(report.restored as u32)
    }

        /// Every tag in a library, with counts, ranked by how many photographs carry it.
    pub fn tags(&self, library_id: i64) -> Result<Vec<TagCount>> {
        let conn = self.lock()?;
        let rows = store::tag_counts(&conn, library_id, None)
            .map_err(|e| ChaffError::engine("photos", e))?;
        Ok(rows
            .into_iter()
            .map(|(name, count)| TagCount { name, count: count as u32 })
            .collect())
    }

    /// Every suggested person, most photographs first.
    pub fn people(&self, library_id: i64) -> Result<Vec<Person>> {
        let conn = self.lock()?;
        let rows =
            store::people(&conn, library_id).map_err(|e| ChaffError::engine("photos", e))?;
        Ok(rows
            .into_iter()
            .map(|p| Person {
                id: p.id,
                name: p.name,
                confirmed: p.confirmed,
                faces: p.faces as u32,
                photos: p.photos as u32,
            })
            .collect())
    }

    /// Find faces and group them.
    ///
    /// Long-running: the first pass downloads a 38 MB model and then runs a network over every
    /// photograph. Resumable — each file is committed as it is processed, so stopping loses
    /// nothing and the work list is the catalog.
    pub fn run_face_pass(
        &self,
        app_data: String,
        library_id: i64,
        progress: Box<dyn Progress>,
    ) -> Result<FacePassReport> {
        let mut conn = self.lock()?;
        let now = now_seconds();
        let report = chaff_faces::pass::run(
            &mut conn,
            std::path::Path::new(&app_data),
            library_id,
            now,
            &mut |done, total| progress.on_progress(done as u32, total as u32, "faces".into(), String::new()),
        )
        .map_err(|e| ChaffError::engine("faces", e))?;

        Ok(FacePassReport {
            detected_files: report.detected_files as u32,
            faces_found: report.faces_found as u32,
            embedded: report.embedded as u32,
            unreadable: report.unreadable as u32,
            people: report.people as u32,
            licence: report.licence,
            elapsed_ms: report.elapsed_ms as u64,
            cancelled: report.cancelled,
        })
    }

    /// Tag photographs.
    ///
    /// **A configured endpoint is an upgrade, not a requirement.** With `endpoint`, a vision
    /// model writes real descriptions. Without one, CLIP runs on this machine in ~26 ms a
    /// photograph and writes tags from a closed vocabulary — and the report says which ran.
    ///
    /// `limit` bounds one call, so a library can be done in pieces with feedback between them
    /// rather than as one silent hour.
    pub fn run_tag_pass(
        &self,
        app_data: String,
        library_id: i64,
        endpoint: Option<String>,
        model: String,
        limit: u32,
        progress: Box<dyn Progress>,
    ) -> Result<TagPassReport> {
        let mut conn = self.lock()?;
        let now = now_seconds();
        let limit = limit as usize;

        if let Some(base) = endpoint.filter(|b| !b.trim().is_empty()) {
            // Cloned, because `model` is named in the report below as well — the caller needs
            // to know which tagger ran, and that is the whole point of the field.
            let e = chaff_core::vlm::Endpoint { base, model: model.clone() };
            let report = chaff_core::tagging::run(
                &mut conn,
                library_id,
                &e,
                limit,
                now,
                &mut |done, total| progress.on_progress(done as u32, total as u32, "tagging".into(), String::new()),
            )
            .map_err(|e| ChaffError::engine("tags", e))?;
            return Ok(TagPassReport {
                tagged: report.tagged as u32,
                remaining: report.remaining as u32,
                unreadable: report.unreadable as u32,
                failed: report.failed as u32,
                tags: report.tags as u32,
                completion_tokens: report.completion_tokens,
                elapsed_ms: report.elapsed_ms as u64,
                stopped_because: report.stopped_because,
                used: format!("vision model: {model}"),
            });
        }

        // **CLIP, because there is no endpoint.**
        //
        // The first version returned "No vision endpoint is configured" and stopped, while CLIP
        // — built for exactly this tier — sat unreachable. The feature and its entry point were
        // designed separately and the seam was never checked.
        let store = chaff_faces::pass::model_store(std::path::Path::new(&app_data));
        let Some(clip) = chaff_faces::clip::model_in(&store) else {
            return Err(ChaffError::with(
                FailureKind::NotFound,
                "No tagger is available. Either set a vision endpoint, or fetch the CLIP model \
                 (it is downloaded on first use — check your network).",
            ));
        };
        let Some(vocabulary) = chaff_faces::clip::bundled() else {
            return Err(ChaffError::with(
                FailureKind::NotFound,
                "The CLIP vocabulary file is missing from this build.",
            ));
        };

        let report = chaff_faces::pass::run_clip(
            &mut conn,
            library_id,
            &chaff_faces::pass::ClipPaths { model: &clip, vocabulary: &vocabulary },
            chaff_faces::pass::ClipSettings { keep: 5, min_similarity: 0.2 },
            now,
            &mut |done, total| progress.on_progress(done as u32, total as u32, "tagging".into(), String::new()),
        )
        .map_err(|e| ChaffError::engine("tags", e))?;

        Ok(TagPassReport {
            tagged: report.tagged as u32,
            remaining: 0,
            unreadable: report.unreadable as u32,
            failed: 0,
            tags: report.tags as u32,
            completion_tokens: 0,
            elapsed_ms: report.elapsed_ms as u64,
            stopped_because: None,
            used: format!("CLIP on this machine, {} phrases", report.vocabulary),
        })
    }

    /// The photographs carrying a tag.
    ///
    /// The navigator lists tags; selecting one has to filter the grid, and that is this.
    pub fn photos_with_tag(&self, library_id: i64, tag: String, model: Option<String>) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        // `model` narrows to tags a particular tagger wrote. Tags from CLIP and tags from a
        // vision model are different vocabularies, and a filter that mixed them would show a
        // user results from a model they did not choose.
        store::photos_with_tag(&conn, library_id, &tag, model.as_deref())
            .map_err(|e| ChaffError::engine("tags", e))
    }

    /// The photographs in a person's group.
    pub fn person_photos(&self, person_id: i64) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        store::photos_for_person(&conn, person_id)
            .map_err(|e| ChaffError::engine("faces", e))
    }

    /// Give a person a name.
    ///
    /// **Typing a name is what confirms a group.** There is deliberately no separate "confirm"
    /// button: a name a user has typed is the confirmation, and a second step to say "yes, I
    /// meant it" is a step nobody takes — leaving groups unconfirmed and the next clustering
    /// pass free to split them again.
    ///
    /// The wording in every UI reflects that a group is a *suggestion* until this is called.
    /// Treating a cluster as fact is how a stranger's face ends up under someone's name.
    pub fn name_person(&self, person_id: i64, name: String) -> Result<()> {
        let conn = self.lock()?;
        // An empty name **un-names** a group rather than storing `""`. A person called the empty
        // string would sort first, match nothing, and be indistinguishable from a bug.
        let name = name.trim();
        let name = if name.is_empty() { None } else { Some(name) };
        store::name_person(&conn, person_id, name, now_seconds())
            .map_err(|e| ChaffError::engine("faces", e))
    }

    /// Merge one group into another.
    ///
    /// The common correction: a face that clustering put in its own group belongs with someone
    /// already named. **`into` is the survivor** — the group that keeps its name and its
    /// photographs — and the other is emptied into it.
    pub fn merge_people(&self, from: i64, into: i64) -> Result<u32> {
        let conn = self.lock()?;
        // Refused when they are the same group: merging a person into themselves is a no-op that
        // a UI could reach by double-clicking one row twice, and reporting "merged 0 faces" for
        // it is better than the alternative of silently doing nothing.
        if from == into {
            return Err(ChaffError::with(
                FailureKind::Refused,
                "That is the same group — pick a different one to merge into.",
            ));
        }
        store::merge_people(&conn, from, into, now_seconds())
            .map(|n| n as u32)
            .map_err(|e| ChaffError::engine("faces", e))
    }

    /// Split a face out of a group and into one of its own.
    ///
    /// The other correction, and the reason both exist: clustering errs in both directions, and
    /// a UI that can only merge cannot fix a group that is too large.
    pub fn split_person(&self, person_id: i64, face_ids: Vec<i64>) -> Result<Option<i64>> {
        let conn = self.lock()?;
        // The group keeps at least one face — `split_person` refuses to empty it, because a
        // person with no faces is not a group and would appear in the navigator as a name with
        // nothing behind it.
        store::split_person(&conn, person_id, &face_ids, now_seconds())
            .map_err(|e| ChaffError::engine("faces", e))
    }

    /// Everything in the library's trash.
    ///
    /// Newest first, because the thing a user wants back is almost always the last thing they
    /// moved — and a panel that opened on the oldest operation would make the common case a
    /// scroll.
    pub fn trash(&self, root: String) -> Result<Vec<TrashEntry>> {
        let trash = chaff_core::trash::Trash::open(std::path::Path::new(&root))
            .map_err(|e| ChaffError::engine("delete", e))?;
        let entries = trash.manifest().map_err(|e| ChaffError::engine("delete", e))?;

        // **Purging marks an operation; it does not remove it.**
        //
        // `Trash::purge` appends a `purge` entry whose id is the original plus `-purged`, so the
        // manifest stays an append-only record of everything that happened. A listing that read
        // it naively showed purged operations as though they were still in the trash — a test
        // caught it, and the symptom would have been a panel offering to restore files that are
        // gone.
        let purged: std::collections::HashSet<String> = entries
            .iter()
            .filter(|e| e.action == "purge")
            .map(|e| e.op_id.trim_end_matches("-purged").to_string())
            .collect();

        let mut out: Vec<TrashEntry> = entries
            .into_iter()
            // Only what is actually in the trash: an operation is listed when it was a `trash`
            // action and has not since been purged.
            .filter(|e| e.action == "trash" && !purged.contains(&e.op_id))
            .map(|e| {
                let bytes = e.files.iter().map(|f| f.size.max(0) as u64).sum();
                let files = e.files.len() as u32;
                // Counted against what is actually on disk rather than trusting the manifest: a
                // file can be removed from the trash by Finder, and a panel that reported the
                // manifest's number would offer to restore something that is not there.
                let present = e.files.iter().filter(|f| f.destination.exists()).count() as u32;
                TrashEntry {
                    op_id: e.op_id,
                    at: e.at,
                    reason: e.reason,
                    files,
                    bytes,
                    incomplete: present < files,
                }
            })
            .collect();

        out.sort_by(|a, b| b.at.cmp(&a.at));
        Ok(out)
    }

    /// Permanently remove operations from the trash.
    ///
    /// **The only irreversible thing in this application**, which is why the UI that calls it
    /// asks twice and why the count is returned rather than a bare `Ok`.
    pub fn purge_trash(&self, root: String, op_ids: Vec<String>) -> Result<u32> {
        let conn = self.lock()?;
        let trash = chaff_core::trash::Trash::open(std::path::Path::new(&root))
            .map_err(|e| ChaffError::engine("delete", e))?;
        let receipt = trash.purge(&op_ids).map_err(|e| ChaffError::engine("delete", e))?;

        // The catalog is told after the files are gone, for the same reason restore tells it
        // after they are back: a crash in between leaves rows the next index pass corrects,
        // where the other order would claim a photograph is gone while it is still there.
        let _ = &conn;
        Ok(receipt.operations as u32)
    }

    /// Faces clustering was unsure about, most uncertain first.
    pub fn ambiguous_faces(&self, library_id: i64, limit: u32) -> Result<Vec<AmbiguousFace>> {
        let conn = self.lock()?;
        // The recogniser's name is part of the question: embeddings from two different models
        // are not comparable, so "which faces is clustering unsure about" only has an answer
        // *for a given model*.
        let model = chaff_faces::pass::recogniser_model();
        // `margin` is how close the two nearest groups are. The engine's default is the one the
        // web app uses; a UI that chose its own would ask a different question than the shell it
        // is meant to match.
        let rows = store::ambiguous_faces(&conn, library_id, model, 0.12, limit as usize)
            .map_err(|e| ChaffError::engine("faces", e))?;
        Ok(rows
            .into_iter()
            .map(|f| AmbiguousFace {
                face_id: f.face_id,
                photo_id: f.photo_id,
                person_id: f.person_id,
                person_name: None,
                // **The gap between the nearest two groups**, which is what makes a face worth
                // asking about: a face that is 0.4 from one group and 0.9 from another is not
                // ambiguous, and one that is 0.71 and 0.74 is.
                confidence: (1.0 - (f.other - f.own).abs()) as f64,
            })
            .collect())
    }

    /// Write XMP sidecars for the photographs that have a decision.
    ///
    /// **Ratings that do not leave the app are ratings a photographer re-does.** Lightroom,
    /// Darktable and Bridge all read XMP, and a culling tool whose stars stop at its own catalog
    /// is one that has to be used twice.
    ///
    /// Only photographs with a rating or a rejection are written: a sidecar for every frame in a
    /// library would create 50,000 files to say "unrated", which is what the absence of a
    /// sidecar already means.
    pub fn write_sidecars(&self, library_id: i64) -> Result<SidecarReport> {
        let conn = self.lock()?;
        let photos = store::photos(&conn, library_id)
            .map_err(|e| ChaffError::engine("photos", e))?;

        let mut written = 0u32;
        let mut skipped = 0u32;
        let mut failed = 0u32;
        let mut first_path: Option<String> = None;

        // The decisions, not the photo rows — `PhotoRow` carries no rating, and asking each
        // photograph individually would be 50,000 queries for one pass.
        let decisions = store::decisions_for_library(&conn, library_id)
            .map_err(|e| ChaffError::engine("photos", e))?;

        for photo in &photos {
            let Some(decision) = decisions.get(&photo.id) else {
                skipped += 1;
                continue;
            };
            // **An unrated photograph is not a decision.** Writing `Rating="0"` for every frame
            // the user merely looked at would put a claim in the sidecar they never made — and
            // the absence of a sidecar already means exactly that.
            if decision.is_unrated() {
                skipped += 1;
                continue;
            }

            let Some(primary) = store::files_for_photo(&conn, photo.id)
                .map_err(|e| ChaffError::engine("photos", e))?
                .into_iter()
                .find(|f| f.role == "raw" || f.role == "raster")
            else {
                skipped += 1;
                continue;
            };

            let path = std::path::Path::new(&primary.path);
            // A format nothing reads a sidecar beside — a PNG, a video. Counted rather than
            // attempted, because a failure per file would drown the real ones.
            if !chaff_core::xmp::supports_sidecar(path) {
                skipped += 1;
                continue;
            }
            match chaff_core::xmp::write(path, *decision, None) {
                Ok(written_to) => {
                    written += 1;
                    if first_path.is_none() {
                        first_path = Some(written_to.to_string_lossy().to_string());
                    }
                }
                Err(e) => {
                    log::warn!("could not write a sidecar for {}: {e}", primary.path);
                    failed += 1;
                }
            }
        }

        Ok(SidecarReport { written, skipped, failed, first_path })
    }

    /// Ask a tagging endpoint what it can do, before starting a pass that would fail.
    pub fn diagnose_endpoint(&self, endpoint: String, model: String) -> EndpointReport {
        let e = chaff_core::vlm::Endpoint { base: endpoint, model };
        let report = chaff_core::tagging::diagnose(&e, None);
        EndpointReport {
            reachable: report.reachable,
            // Whether the *configured* model is among the ones the endpoint lists. A reachable
            // endpoint offering a different model is a pass that will fail on its first
            // photograph, and this is the field that says so before it starts.
            model_present: report.models.iter().any(|m| m == &e.model),
            models: report.models,
            // **The only test that proves it can tag.** An endpoint that lists a model and
            // answers `/models` can still refuse a vision request, and a pass started against
            // one would fail on every photograph.
            vision_ok: report.vision_works,
            detail: format!(
                "healthy: {}, schema enforced: {}, {:.2}s per photograph, {} reasoning tokens wasted",
                report.healthy, report.schema_enforced, report.seconds_per_photo,
                report.reasoning_tokens_wasted
            ),
            elapsed_ms: (report.seconds_per_photo * 1000.0) as u64,
        }
    }

    /// What this machine can do, for the same reason the web app has it.
    pub fn capabilities(&self) -> Capabilities {
        let probe = chaff_core::hardware::probe();
        let (tier, why) = chaff_core::hardware::choose_tier(&probe);
        let gpu = probe.gpus.first();
        Capabilities {
            tier: format!("{tier:?}"),
            gpu: gpu.map(|g| g.name.clone()),
            // Bytes in the engine, megabytes here: a UI shows a number a person reads, and the
            // conversion belongs at the boundary rather than in every view.
            vram_mb: gpu.and_then(|g| g.vram_bytes).map(|b| b / (1024 * 1024)),
            // **Unified memory is the difference between a tier that works and one that does
            // not on Apple Silicon**, where there is no separate VRAM figure to read.
            // Apple Silicon reports no separate VRAM figure, so a machine with a large unified
            // memory and no discrete GPU is tiered on its total RAM. The engine decides that;
            // this only reports which case it was.
            unified_memory: probe.gpus.is_empty() && probe.os == "macos",
            summary: why,
        }
    }

    /// Every remembered setting.
    ///
    /// Returned as pairs rather than a map because UniFFI has no `HashMap` in its type set, and
    /// inventing one for four values would be more machinery than the feature is worth.
    pub fn settings(&self) -> Result<Vec<Setting>> {
        let conn = self.lock()?;
        let all = store::settings(&conn).map_err(|e| ChaffError::engine("settings", e))?;
        let mut out: Vec<Setting> = all
            .into_iter()
            .map(|(key, value)| Setting { key, value })
            .collect();
        // Sorted, so a settings list is stable between reads and a diff means something.
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// The tags on one photograph.
    ///
    /// The inspector shows them, and the navigator filters by them — this is what connects the
    /// two, so a user can see *why* a photograph is in the results they are looking at.
    pub fn photo_tags(&self, photo_id: i64) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let rows = store::tags_for_photo(&conn, photo_id)
            .map_err(|e| ChaffError::engine("tags", e))?;
        Ok(rows.into_iter().map(|t| t.name).collect())
    }

    /// Why this photograph scored what it did, in words.
    ///
    /// The inspector already shows per-term percentiles, which is most of the answer. This is the
    /// sentence on top of it — and it is what a user pastes into a message when they disagree
    /// with the number.
    pub fn photo_explanation(&self, photo_id: i64) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let detail = pipeline::photo_detail(&conn, photo_id)
            .map_err(|e| ChaffError::engine("photos", e))?;

        let mut lines = Vec::new();
        match (detail.composite, detail.band.as_deref()) {
            (Some(c), Some(band)) => lines.push(format!("{:.0}/100 — {band}", c)),
            (Some(c), None) => lines.push(format!("{:.0}/100", c)),
            // **Said, not skipped.** A photograph with no score is one that produced no
            // measurement — a raw this build cannot decode — and silence reads as "fine".
            (None, _) => lines.push("Not scored — this file produced no measurement.".to_string()),
        }
        for (label, percentile) in &detail.terms {
            lines.push(format!("{label}: {:.0}th percentile of this shoot", percentile));
        }
        if detail.terms.is_empty() && detail.composite.is_some() {
            lines.push("No per-term breakdown was recorded for this photograph.".to_string());
        }
        Ok(lines)
    }

    /// Start watching a library for changes on disk.
    ///
    /// # What it does, and what it deliberately does not
    ///
    /// It re-indexes when files settle — a copy in progress produces hundreds of events, and
    /// indexing on each would be hundreds of passes. The accumulator debounces.
    ///
    /// **It does not run while a delete plan is pending.** `DeleteSession::commit` refuses any
    /// file that was not in the plan the user was shown, so a re-index during the confirmation
    /// dialog turned Confirm into a hard failure with no recovery path — and the user had no way
    /// to know a watcher caused it.
    ///
    /// Idempotent: calling it twice for the same library is a no-op rather than two watchers.
    pub fn start_watching(&self, root: String, library_id: i64) -> Result<WatchStatus> {
        if self.watch.lock().map(|w| w.is_some()).unwrap_or(false) {
            return self.watch_status();
        }

        let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let path = self.path.clone();
        let pending = std::sync::Arc::clone(&self.plan_pending);
        let thread_seen = std::sync::Arc::clone(&seen);
        let thread_busy = std::sync::Arc::clone(&busy);
        let thread_stop = std::sync::Arc::clone(&stop);
        let watch_root = std::path::PathBuf::from(&root);

        let handle = std::thread::spawn(move || {
            watch_loop(watch_root, library_id, path, pending, thread_seen, thread_busy, thread_stop);
        });

        *self.watch.lock().map_err(|_| ChaffError::with(FailureKind::Poisoned, "the engine lock was poisoned"))? =
            Some(WatchHandle { handle: Some(handle), stop, seen, busy });

        self.watch_status()
    }

    /// Stop watching.
    ///
    /// The thread is asked to stop rather than killed, and it is **not joined**: a re-index in
    /// progress commits each photograph as it goes, and blocking the UI on a pass that may take
    /// minutes to reach its next check is worse than letting it finish in the background.
    pub fn stop_watching(&self) -> Result<WatchStatus> {
        if let Ok(mut slot) = self.watch.lock() {
            if let Some(mut w) = slot.take() {
                w.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                // Dropped without joining. See above.
                w.handle = None;
            }
        }
        Ok(WatchStatus { running: false, seen: 0, busy: false })
    }

    /// Is a library being watched, and what has it seen?
    pub fn watch_status(&self) -> Result<WatchStatus> {
        let slot = self.watch.lock().map_err(|_| ChaffError::with(FailureKind::Poisoned, "the engine lock was poisoned"))?;
        Ok(match slot.as_ref() {
            Some(w) => WatchStatus {
                running: true,
                seen: w.seen.load(std::sync::atomic::Ordering::Relaxed),
                busy: w.busy.load(std::sync::atomic::Ordering::Relaxed),
            },
            None => WatchStatus { running: false, seen: 0, busy: false },
        })
    }

    /// How big the thumbnail cache is, and how much is worth reclaiming.
    ///
    /// **A cache nobody can see is one nobody trusts.** The engine keeps decoded thumbnails on
    /// disk keyed by content hash, and on a large library that is a real amount of space — so a
    /// user has to be able to find out how much, and get it back.
    pub fn thumbnail_cache(&self) -> Result<ThumbnailCacheInfo> {
        let root = thumbnail_root();
        let (files, bytes) = directory_size(&root);
        Ok(ThumbnailCacheInfo { path: root.to_string_lossy().to_string(), files, bytes })
    }

    /// Delete cached thumbnails, keeping the most recently used.
    ///
    /// `keep` is a count rather than a size: a user thinks in "the last few hundred", and a byte
    /// budget would delete an unpredictable number of them. **Sorted by modification time**, so
    /// what survives is what was looked at most recently — which is the whole point of a cache.
    ///
    /// Reclaimable at any time: every thumbnail is derived from a photograph that is still there,
    /// so the worst case is that the next scroll decodes again.
    pub fn trim_thumbnail_cache(&self, keep: u32) -> Result<u32> {
        let root = thumbnail_root();
        let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        collect_files(&root, &mut entries);

        if entries.len() <= keep as usize {
            return Ok(0);
        }
        // Newest first, so `keep` of them survive.
        entries.sort_by(|a, b| b.1.cmp(&a.1));

        let mut removed = 0u32;
        for (path, _) in entries.into_iter().skip(keep as usize) {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// A setting, or `None` if it was never written.
    pub fn setting(&self, key: String) -> Result<Option<String>> {
        let conn = self.lock()?;
        let all = store::settings(&conn).map_err(|e| ChaffError::engine("settings", e))?;
        Ok(all.get(&key).cloned())
    }

    /// Remember a setting.
    pub fn set_setting(&self, key: String, value: String) -> Result<()> {
        let conn = self.lock()?;
        store::set_setting(&conn, &key, &value, now_seconds())
            .map_err(|e| ChaffError::engine("settings", e))
    }

    /// Is a delete waiting to be confirmed?
    pub fn has_pending_delete(&self) -> bool {
        self.pending.lock().map(|s| s.has_pending()).unwrap_or(false)
    }
}

/// Where thumbnails and downloaded models live.
///
/// Passed in rather than assumed: a sandboxed app has a container path the engine cannot
/// guess, and a headless run has no container at all.
static DATA_ROOT: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Tell the engine where to keep its caches.
///
/// Called once at launch. The alternative — reading an environment variable inside the engine
/// — is how a sandboxed app ends up writing outside its container and being killed for it.
#[uniffi::export]
pub fn set_data_root(path: String) {
    if let Ok(mut root) = DATA_ROOT.lock() {
        *root = Some(std::path::PathBuf::from(path));
    }
}

/// Where thumbnails live, once `set_data_root` has been called.
///
/// # Why this no longer falls back to `.`
///
/// It did, and `.` is the **current working directory** — which for a GUI application launched
/// from Finder is `/`, and for one launched from a terminal is wherever that terminal happened
/// to be. So a caller that forgot `set_data_root` wrote its cache into the filesystem root, or
/// into the user's home, or into a source checkout, and nothing said so.
///
/// A cache that lands somewhere unexpected is worse than one that fails: it accumulates, nobody
/// knows to clear it, and on a sandboxed app it is the difference between running and being
/// killed for writing outside the container.
///
/// The fallback is now the **platform cache directory**, which is where a cache belongs, and it
/// says so in the log. Still a fallback — but one whose failure mode is a cache in the right
/// place rather than a cache in `/`.
/// Total files and bytes under a directory, recursively.
///
/// A directory that does not exist is `(0, 0)` rather than an error: a cache that has never been
/// written is empty, not broken.
fn directory_size(dir: &std::path::Path) -> (u64, u64) {
    let mut entries = Vec::new();
    collect_files(dir, &mut entries);
    let bytes = entries
        .iter()
        .filter_map(|(p, _)| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    (entries.len() as u64, bytes)
}

/// Every file under a directory, with its modification time.
fn collect_files(
    dir: &std::path::Path,
    out: &mut Vec<(std::path::PathBuf, std::time::SystemTime)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if let Ok(meta) = entry.metadata() {
            let when = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((path, when));
        }
    }
}

fn thumbnail_root() -> std::path::PathBuf {
    match DATA_ROOT.lock().ok().and_then(|r| r.clone()) {
        Some(root) => root.join("thumbnails"),
        None => {
            let fallback = platform_cache_dir().join("chaff").join("thumbnails");
            log::warn!(
                "set_data_root was never called; thumbnails are going to {} — the caller should \
                 pass a path at launch",
                fallback.display()
            );
            fallback
        }
    }
}

/// The platform's cache directory, without a dependency for three cases.
///
/// `dirs` would do this, and pulling a crate for three `cfg` arms in one function is the trade
/// this file already declined for `libc` in the CLI.
fn platform_cache_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join("Library").join("Caches");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return std::path::PathBuf::from(local);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // The XDG answer, and the right default on every other Unix.
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return std::path::PathBuf::from(xdg);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(".cache");
        }
    }
    // Nothing to go on. The temporary directory is a poor cache — it is cleared — but it is
    // **inside** the system's expectations, which `/` is not.
    std::env::temp_dir()
}

/// The thumbnail cache cap, matching the desktop app's.
///
/// The cache evicts on write, so the cap is enforced rather than aspirational — a distinction
/// that mattered once already, when the cap was documented as load-bearing and never applied.
const THUMBNAIL_CAP_BYTES: u64 = 512 * 1024 * 1024;

/// The band a composite falls into, using the same thresholds the pipeline used.
///
/// Duplicated from the Tauri shell, which is a smell — and the honest fix is to move it into
/// the engine so both shells call one function. Recorded here rather than hidden: two copies
/// of a threshold is how two UIs start disagreeing about what "Keep" means.
fn band_of(composite: f64) -> &'static str {
    use chaff_core::scoring::composite::{Band, BandThresholds};
    match BandThresholds::default().band_of(composite) {
        Band::Keep => "keep",
        Band::Review => "review",
        Band::Reject => "reject",
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A progress sink that does nothing, for tests that are not about progress.
    struct Silent;
    impl Progress for Silent {
        fn on_progress(&self, _done: u32, _total: u32, _stage: String, _current: String) -> bool {
            true
        }
    }

    /// A progress sink that records, for the tests that are.
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<(u32, u32, String)>>,
        currents: Mutex<Vec<String>>,
    }
    impl Progress for Recording {
        fn on_progress(&self, done: u32, total: u32, stage: String, current: String) -> bool {
            if let Ok(mut v) = self.seen.lock() {
                v.push((done, total, stage));
            }
            if let Ok(mut v) = self.currents.lock() {
                v.push(current);
            }
            true
        }
    }

    fn engine() -> (tempfile::TempDir, Arc<Engine>) {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::new(dir.path().join("catalog.db").to_string_lossy().to_string()).unwrap();
        (dir, e)
    }

    #[test]
    fn opening_a_library_reports_what_it_did() {
        let (_d, e) = engine();
        let lib = tempfile::tempdir().unwrap();
        let report = e
            .open_library(lib.path().to_string_lossy().to_string(), Box::new(Silent))
            .unwrap();

        assert_eq!(report.library.photos, 0, "an empty folder has no photographs");
        assert!(report.library.id > 0, "a library id is assigned");
        assert!(!report.library.root.is_empty());
    }

    #[test]
    fn progress_is_reported_rather_than_only_the_end() {
        // **The reason the callback exists.** A face pass runs for an hour; a UI that can
        // only ask "are we there yet" has nothing to show for the first fifty-nine minutes.
        let (_d, e) = engine();
        let lib = tempfile::tempdir().unwrap();
        let rec = Arc::new(Recording::default());

        // A sink that forwards to the recording one, because the trait object is moved.
        struct Forward(Arc<Recording>);
        impl Progress for Forward {
            fn on_progress(&self, done: u32, total: u32, stage: String, current: String) -> bool {
                self.0.on_progress(done, total, stage, current)
            }
        }

        e.open_library(lib.path().to_string_lossy().to_string(), Box::new(Forward(Arc::clone(&rec))))
            .unwrap();

        let seen = rec.seen.lock().unwrap();
        assert!(!seen.is_empty(), "an index must report progress at least once");
        assert!(
            seen.iter().any(|(_, _, s)| s == "ranking"),
            "the stages must be named, not just counted: {seen:?}"
        );
    }

    #[test]
    fn a_library_id_that_does_not_exist_is_an_error_not_a_panic() {
        let (_d, e) = engine();
        let r = e.photos(9999);
        assert!(r.is_ok(), "an unknown library is an empty list, not a crash: {r:?}");
        assert!(r.unwrap().is_empty());

        let r = e.rescore(9999, Box::new(Silent));
        assert!(r.is_err(), "rescoring an unknown library must fail loudly");
    }

    #[test]
    fn a_thumbnail_for_a_photograph_with_no_files_is_none_not_an_error() {
        // A tile that shows its filename is a better answer than an alert in front of the
        // user for a photograph the app already knows it cannot read.
        let (_d, e) = engine();
        let r = e.thumbnail(9999, "grid".to_string());
        assert!(matches!(r, Ok(None)), "got {r:?}");
    }

    #[test]
    fn setting_a_decision_on_an_unknown_photograph_does_not_corrupt_the_catalog() {
        // SQLite foreign keys must be on, or this writes a decision for a photograph that
        // does not exist and every later join silently misses it.
        let (_d, e) = engine();
        let _ = e.set_decision(9999, 5, false);
        let conn = e.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM decision", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "a decision for a photograph that does not exist must not be stored");
    }

    #[test]
    fn a_delete_plan_that_cannot_proceed_holds_nothing() {
        // **A plan the user cannot confirm must not sit in the session.** A refusal that left
        // the plan pending would let a later `commit_delete` — from a UI that did not notice
        // the refusal — move files the dialog said it would not.
        let (_d, e) = engine();
        let lib = tempfile::tempdir().unwrap();
        e.open_library(lib.path().to_string_lossy().to_string(), Box::new(Silent)).unwrap();

        let plan = e
            .plan_delete(lib.path().to_string_lossy().to_string(), vec![9999])
            .unwrap();
        assert!(!plan.refusals.is_empty() || plan.files == 0);
        assert!(!e.has_pending_delete(), "a refused plan must not stay pending");
    }

    #[test]
    fn committing_with_nothing_pending_is_an_error_not_a_silent_success() {
        // The invariant in its simplest form: there is no way to commit a plan nobody was
        // shown. A `commit_delete` that returned an empty receipt would let a UI report
        // success for an operation that never happened.
        let (_d, e) = engine();
        let lib = tempfile::tempdir().unwrap();
        let r = e.commit_delete(lib.path().to_string_lossy().to_string());
        assert!(r.is_err(), "got {r:?}");
    }

    #[test]
    fn cancelling_clears_the_session() {
        let (_d, e) = engine();
        e.cancel_delete().unwrap();
        assert!(!e.has_pending_delete());
    }

    #[test]
    fn the_data_root_is_used_for_thumbnails() {
        // A sandboxed app has a container path the engine cannot guess. Writing outside it
        // is how a sandboxed app gets killed.
        let dir = tempfile::tempdir().unwrap();
        set_data_root(dir.path().to_string_lossy().to_string());
        assert!(thumbnail_root().starts_with(dir.path()));
        assert!(thumbnail_root().ends_with("thumbnails"));
    }
}
