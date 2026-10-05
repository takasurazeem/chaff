//! The engine, for Swift.
//!
//! # What this crate is
//!
//! A boundary and nothing else. Every function here calls the same `chaff-core` function the
//! Tauri command calls — **not a reimplementation** — so a bug fixed in
//! `resolve_delete_selection` is fixed for both UIs at once, and a safety guarantee has one
//! implementation rather than two that drift.
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

/// An error crossing the boundary.
///
/// A flat struct rather than an enum with associated data: UniFFI maps this to a Swift
/// `throws` with a readable message, and a caller that needs to branch can match on `kind`.
/// The alternative — one error type per engine error — would be forty types for a shell that
/// shows a message and moves on.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ChaffError {
    #[error("{message}")]
    Engine { kind: String, message: String },
}

impl ChaffError {
    fn engine(kind: &str, e: impl std::fmt::Display) -> Self {
        Self::Engine { kind: kind.to_string(), message: e.to_string() }
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
    fn on_progress(&self, done: u32, total: u32, stage: String);
}

/// The catalog, open for a shell to use.
///
/// Holds the connection behind a `Mutex` because SQLite is not safe to use from two threads
/// on one connection, and a SwiftUI app will call from at least two — the main actor for
/// reads and a background task for a pass. The lock is the engine's, not this crate's.
#[derive(uniffi::Object)]
pub struct Engine {
    conn: Mutex<chaff_core::rusqlite::Connection>,
}

#[uniffi::export]
impl Engine {
    /// Open or create a catalog.
    #[uniffi::constructor]
    pub fn new(database_path: String) -> Result<Arc<Self>> {
        let conn = catalog::open(std::path::Path::new(&database_path))
            .map_err(|e| ChaffError::engine("catalog", e))?;
        Ok(Arc::new(Self { conn: Mutex::new(conn) }))
    }

    /// Index a library and score it.
    ///
    /// Long-running: minutes for a large library. `progress` is called as it goes.
    pub fn open_library(&self, root: String, progress: Box<dyn Progress>) -> Result<OpenReport> {
        let mut conn = self.lock()?;
        let path = std::path::Path::new(&root);
        let now = now_seconds();

        let report = pipeline::index_and_score_with_progress(&mut conn, path, now, &mut |p| {
            let (done, total, stage) = match p {
                pipeline::Progress::Scanning { files } => (files as u32, 0, "scanning"),
                pipeline::Progress::Scoring { done, total, .. } => (done as u32, total as u32, "scoring"),
                pipeline::Progress::Ranking { .. } => (1, 1, "ranking"),
            };
            progress.on_progress(done, total, stage.to_string());
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
        let cache = chaff_core::thumb::ThumbnailCache::open(thumbnail_root(), THUMBNAIL_CAP_BYTES)
            .map_err(|e| ChaffError::engine("thumbnail", e))?;

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
            .ok_or_else(|| ChaffError::Engine {
                kind: "library".into(),
                message: format!("no library with id {library_id}"),
            })?;
        drop(conn);

        self.open_library(root, progress)
    }
}

impl Engine {
    /// The connection, or an error naming the real problem.
    ///
    /// A poisoned lock means another thread panicked while holding it. Saying so is more
    /// useful than a generic failure, and it is what the user will be asked about.
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, chaff_core::rusqlite::Connection>> {
        self.conn.lock().map_err(|_| ChaffError::Engine {
            kind: "poisoned".into(),
            message: "another operation failed while using the catalog; restart Chaff".into(),
        })
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
        fn on_progress(&self, _done: u32, _total: u32, _stage: String) {}
    }

    /// A progress sink that records, for the tests that are.
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<(u32, u32, String)>>,
    }
    impl Progress for Recording {
        fn on_progress(&self, done: u32, total: u32, stage: String) {
            if let Ok(mut v) = self.seen.lock() {
                v.push((done, total, stage));
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
            fn on_progress(&self, done: u32, total: u32, stage: String) {
                self.0.on_progress(done, total, stage);
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
    fn the_data_root_is_used_for_thumbnails() {
        // A sandboxed app has a container path the engine cannot guess. Writing outside it
        // is how a sandboxed app gets killed.
        let dir = tempfile::tempdir().unwrap();
        set_data_root(dir.path().to_string_lossy().to_string());
        assert!(thumbnail_root().starts_with(dir.path()));
        assert!(thumbnail_root().ends_with("thumbnails"));
    }
}
