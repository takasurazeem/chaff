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
    fn on_progress(&self, done: u32, total: u32, stage: String, current: String);
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

        let report = pipeline::index_and_score_with_progress(&mut conn, path, now, &mut |p| {
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
                    camera: m.and_then(|m| m.camera.clone()),
                    lens: m.and_then(|m| m.lens.clone()),
                    year: m.and_then(|m| m.year),
                }
            })
            .collect())
    }

    /// Folders with their two counts.
    pub fn folders(&self, library_id: i64) -> Result<Vec<Folder>> {
        let conn = self.lock()?;
        let rows = store::directories(&conn, library_id)
            .map_err(|e| ChaffError::engine("directories", e))?;
        Ok(rows
            .into_iter()
            .map(|d| Folder { path: d.path, direct: d.direct as u32, recursive: d.recursive as u32 })
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
        Ok(())
    }

    /// Everything known about one photograph.
    pub fn photo_detail(&self, photo_id: i64) -> Result<PhotoDetail> {
        let conn = self.lock()?;
        let d = pipeline::photo_detail(&conn, photo_id)
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

fn thumbnail_root() -> std::path::PathBuf {
    DATA_ROOT
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("thumbnails")
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
        fn on_progress(&self, _done: u32, _total: u32, _stage: String, _current: String) {}
    }

    /// A progress sink that records, for the tests that are.
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<(u32, u32, String)>>,
        currents: Mutex<Vec<String>>,
    }
    impl Progress for Recording {
        fn on_progress(&self, done: u32, total: u32, stage: String, current: String) {
            if let Ok(mut v) = self.seen.lock() {
                v.push((done, total, stage));
            }
            if let Ok(mut v) = self.currents.lock() {
                v.push(current);
            }
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
            fn on_progress(&self, done: u32, total: u32, stage: String, current: String) {
                self.0.on_progress(done, total, stage, current);
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
