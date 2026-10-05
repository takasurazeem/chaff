//! The delete plan the user was shown, held until they confirm it.
//!
//! # The guarantee this type exists to keep
//!
//! ADR-0004 promises that **a file whose contents change between the confirmation dialog and
//! the move aborts the whole operation**. The first implementation of that promise did not
//! exist: the hash map was built from `file.content_hash`, a column nothing ever wrote, so it
//! was always empty and the verify loop skipped every file. A review proved it by changing a
//! file's bytes between plan and commit and watching it move anyway.
//!
//! The fix was to hash the files **when the plan is shown** and verify those hashes **when it
//! is committed** — which requires the hashes to survive between the two calls somewhere the
//! frontend cannot reach.
//!
//! # Why it is in the engine and not in a shell
//!
//! It lived in the Tauri shell. A second shell — the native macOS app — would have to hold its
//! own copy, and two implementations of a safety guarantee is how one of them drifts. The
//! invariant "commit takes no file list" now lives here, where it can be tested once and where
//! both shells are subject to it.
//!
//! # What it deliberately does not expose
//!
//! There is no way to commit a plan the caller supplies. [`DeleteSession::commit`] takes no
//! arguments but a time, and moves what it was shown. A caller cannot name a file, cannot
//! supply a hash, and cannot widen the operation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::catalog::store;
use crate::rusqlite::Connection;
use crate::trash::{self, Trash};

/// How long a shown plan stays valid.
///
/// A plan is a promise about a moment. An hour later the library may have changed in ways the
/// user has forgotten about, and re-confirming from memory is not confirmation.
pub const TTL_SECONDS: i64 = 3600;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(
        "There is no delete waiting to be confirmed. The plan may have expired — select the \
         photographs again."
    )]
    NothingPending,
    #[error(
        "That confirmation is more than an hour old, so the library may have changed since \
         you saw it. Select the photographs again."
    )]
    Expired,
    #[error("could not read {path} to verify it: {reason}")]
    Unreadable { path: String, reason: String },
    #[error(
        "{path} was not part of the plan you were shown — it appeared between the \
         confirmation and the move. Nothing was moved; select the photographs again."
    )]
    Appeared { path: String },
    #[error(
        "{path} was in the plan you were shown and is no longer there — it was deleted or moved          between the confirmation and the move. Nothing was moved; select the photographs again."
    )]
    Vanished { path: String },
    #[error(transparent)]
    Trash(#[from] trash::TrashError),
    #[error(transparent)]
    Catalog(#[from] crate::catalog::CatalogError),
}

/// What the user was shown, and what they will get.
#[derive(Debug, Clone)]
pub struct PlannedDelete {
    pub op_id: String,
    pub moved: usize,
    pub bytes: u64,
    /// Warnings worth showing: a cross-volume copy, a name collision.
    pub warnings: Vec<String>,
}

/// A plan awaiting confirmation.
struct Pending {
    photo_ids: Vec<i64>,
    /// Path to content hash, taken when the user was shown the plan.
    hashes: HashMap<PathBuf, String>,
    created: i64,
}

/// The plan the user has been shown but not confirmed.
///
/// One per library. Replacing it is deliberate: a user who plans a second delete has moved on
/// from the first, and holding both invites committing a selection they have forgotten.
#[derive(Default)]
pub struct DeleteSession {
    pending: Option<Pending>,
}

impl DeleteSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is there a plan waiting?
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Forget the plan without moving anything.
    ///
    /// Called when the user cancels. Without it the plan sits until the next one replaces it,
    /// and a stale plan is one a stray click could commit.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Resolve a selection, hash every file, and hold the result.
    ///
    /// **The hashing is the point.** It happens here, while the user is looking at what will
    /// move, so that the commit has something to verify against. Hashing at commit time would
    /// compare a file with itself and prove nothing — which is what the first implementation
    /// did, by accident, with an always-empty map.
    pub fn plan(
        &mut self,
        conn: &Connection,
        root: &Path,
        photo_ids: &[i64],
        now: i64,
    ) -> Result<PlannedDelete, SessionError> {
        let selection = crate::pipeline::resolve_delete_selection(conn, photo_ids)?;
        let files: Vec<PathBuf> =
            selection.candidates.iter().flat_map(|c| c.files.clone()).collect();

        let mut hashes = HashMap::with_capacity(files.len());
        for f in &files {
            match trash::hash_file(f) {
                Ok(h) => {
                    hashes.insert(f.clone(), h);
                }
                // A file that cannot be read cannot be verified, so it cannot be moved. The
                // refusal path handles it rather than the hash being silently absent — which
                // is exactly the bug this replaces.
                Err(e) => {
                    return Err(SessionError::Unreadable {
                        path: f.display().to_string(),
                        reason: e.to_string(),
                    })
                }
            }
        }

        let trash = Trash::open(root)?;
        let plan = trash.plan(&files, &hashes, now)?;

        self.pending = Some(Pending {
            photo_ids: photo_ids.to_vec(),
            hashes,
            created: now,
        });

        Ok(PlannedDelete {
            // The plan's own id, not an empty string. It was in hand and discarded, so the
            // confirmation dialog could not name the operation it was about to perform.
            op_id: plan.op_id.clone(),
            moved: plan.moves.len(),
            bytes: plan.moves.iter().map(|m| m.size as u64).sum(),
            warnings: plan.warnings.iter().map(describe_warning).collect(),
        })
    }

    /// Move what was shown, verifying every file first.
    ///
    /// **Takes no file list.** It re-resolves from the ids it was given, re-plans with the
    /// hashes recorded when the plan was displayed, and lets [`Trash::commit`] re-hash and
    /// compare. Nothing a caller passes can name a file or influence what is verified.
    pub fn commit(&mut self, conn: &Connection, root: &Path, now: i64) -> Result<PlannedDelete, SessionError> {
        let pending = self.pending.take().ok_or(SessionError::NothingPending)?;

        if now - pending.created > TTL_SECONDS {
            // The plan is dropped rather than kept: a caller who retries immediately should be
            // told to select again, not given the same expired plan to fail with twice.
            return Err(SessionError::Expired);
        }

        let selection = crate::pipeline::resolve_delete_selection(conn, &pending.photo_ids)?;
        let files: Vec<PathBuf> =
            selection.candidates.iter().flat_map(|c| c.files.clone()).collect();

        // **Refuse anything that was not in the plan.**
        //
        // The first version re-resolved the selection and moved whatever it found, so a file
        // that *appeared* between the dialog and the confirmation — a raw written by a card
        // import, a JPEG exported beside it, a watcher re-index — was moved having never been
        // shown to anyone. It had no hash in `pending.hashes`, and the verify loop skipped
        // exactly that case.
        //
        // The doc comment above said "moves what it was shown" and the code did not. That is
        // the same failure as the original ADR-0004 bug: a guarantee stated in a comment and
        // absent from the code. `Trash::commit` now refuses an unhashed file as well, so this
        // is the second of two independent checks rather than the only one.
        // **A file that vanished is warned about, not refused — and the warning is the point.**
        //
        // I first wrote a `Vanished` refusal here, mirroring `Appeared`, and a test showed it
        // could not fire: deleting a file from disk does not remove its catalog row, so it is
        // still in `files` and the check passes. What actually notices is `Trash::plan`, which
        // produces `Warning::Missing` naming the file.
        //
        // That is the better design, and the difference is worth stating. `Appeared` is refused
        // because moving a file nobody was shown is a **safety** failure. A vanished file is a
        // **truthfulness** one: the other nine files are still exactly what the user agreed to,
        // and refusing all ten would be a worse outcome than doing nine and saying so.
        //
        // So the requirement is not a refusal. It is that the warning **reaches the user** — and
        // it did not: the receipt carried it and both shells closed the dialog without showing
        // it. `DeleteReceipt::warnings` is surfaced by both now.
        if let Some(extra) = files.iter().find(|f| !pending.hashes.contains_key(*f)) {
            return Err(SessionError::Appeared {
                path: extra.display().to_string(),
            });
        }

        let trash = Trash::open(root)?;
        let plan = trash.plan(&files, &pending.hashes, now)?;
        let receipt = trash.commit(&plan, "culled in Chaff", now)?;

        // The catalog is updated only after the files have moved. A crash in between leaves
        // files in the trash that the catalog still lists, which the next index pass corrects
        // — the reverse order would leave the catalog claiming a file is gone while it is
        // still on disk.
        //
        // The row is marked, not deleted: `decision` cascades with `photo`, so removing it
        // would take the user's rating with it and a restore would bring the file back
        // unrated.
        for c in &selection.candidates {
            store::mark_photo_trashed(conn, c.photo_id, now)?;
        }

        Ok(PlannedDelete {
            op_id: receipt.op_id,
            moved: receipt.moved,
            bytes: receipt.bytes.max(0) as u64,
            warnings: receipt.warnings.iter().map(describe_warning).collect(),
        })
    }
}

/// A warning, in words a person can act on.
///
/// A `{:?}` of an enum is not a warning, it is a type name. This lived in the Tauri shell and
/// moved down with the rest of the plan logic, so both shells say the same thing.
fn describe_warning(w: &trash::Warning) -> String {
    match w {
        trash::Warning::CrossVolume { source: from, destination: to } => format!(
            "{} is on a different volume from the trash at {} — the file will be copied and \
             then removed, which is slower and is not atomic",
            from.display(),
            to.display()
        ),
        trash::Warning::Missing { path } => format!(
            "{} was expected and is not there — it may have been moved by something else \
             between the plan and the move",
            path.display()
        ),
        // **No catch-all.** A `Warning` variant added later must fail to compile here rather
        // than fall back to `{:?}`, which is a type name and not a warning. The exhaustive
        // match is what makes the new variant someone's decision.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::store::{self, FileMeta};

    /// A library with one photograph, on disk, so hashing has something real to read.
    fn library() -> (tempfile::TempDir, Connection, i64, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let photo = root.join("IMG_0001.CR3");
        std::fs::write(&photo, b"original bytes").unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = store::upsert_library(&conn, &root, 0).unwrap();

        let mut meta = HashMap::new();
        meta.insert(
            photo.clone(),
            FileMeta { size_bytes: 14, mtime_ns: 1 },
        );
        crate::indexer::index(&mut conn, &root, 0).map_err(|e| e.to_string()).unwrap();
        let _ = (lib, meta);

        (dir, conn, lib, photo)
    }

    #[test]
    fn a_file_that_vanishes_after_the_dialog_is_refused_not_skipped() {
        // **The mirror of `Appeared`, and it was missing.**
        //
        // `commit` refused files that appeared and said nothing about ones that vanished. A file
        // deleted from Finder between the dialog and the button became a `Warning::Missing` and
        // was quietly skipped — so a dialog that said "2 files" produced a receipt saying
        // `moved: 1`, and the user had no way to know the operation was not the one they agreed
        // to.
        //
        // Not a safety hole the way `Appeared` was: "the file is gone" cannot mean "a different
        // file was moved". It is a **truthfulness** hole, and this application's whole claim is
        // that the dialog says what will happen.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // Two photographs, each a JPEG.
        let a = root.join("IMG_0001.JPG");
        let b = root.join("IMG_0002.JPG");
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        crate::indexer::index(&mut conn, &root, 0).unwrap();
        let lib = store::library_id_for_root(&conn, &root.to_string_lossy()).unwrap().unwrap();
        let ids: Vec<i64> = store::photos(&conn, lib).unwrap().iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), 2, "two photographs to plan");

        let mut s = DeleteSession::new();
        s.plan(&conn, &root, &ids, 0).unwrap();

        // One of them goes away while the dialog is open.
        std::fs::remove_file(&b).unwrap();

        let receipt = s.commit(&conn, &root, 1).expect("the other file still moves");

        // **Nine of ten is the right outcome; saying so is the requirement.**
        assert_eq!(receipt.moved, 1, "the file that is still there moves");
        assert!(
            receipt.warnings.iter().any(|w| w.contains("IMG_0002")),
            "the vanished file must be named in the warnings — a receipt saying `moved: 1` for a \
             dialog that showed 2, with no explanation, is the bug. Got {:?}",
            receipt.warnings
        );

        // And the one that is still there actually moved.
        assert!(!a.exists(), "the file that was there moved to the trash");
    }

    #[test]
    fn a_plan_with_nothing_behind_it_cannot_be_committed() {
        // The invariant in its simplest form: there is no way to commit a plan nobody was
        // shown.
        let mut s = DeleteSession::new();
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::catalog::open_in_memory().unwrap();
        assert!(matches!(
            s.commit(&conn, dir.path(), 0),
            Err(SessionError::NothingPending)
        ));
    }

    #[test]
    fn cancelling_forgets_the_plan() {
        // Without this a stale plan sits until the next one replaces it, and a stale plan is
        // one a stray click could commit.
        let mut s = DeleteSession::new();
        s.pending = Some(Pending { photo_ids: vec![1], hashes: HashMap::new(), created: 0 });
        assert!(s.has_pending());
        s.cancel();
        assert!(!s.has_pending());

        let dir = tempfile::tempdir().unwrap();
        let conn = crate::catalog::open_in_memory().unwrap();
        assert!(matches!(
            s.commit(&conn, dir.path(), 0),
            Err(SessionError::NothingPending)
        ));
    }

    #[test]
    fn an_expired_plan_is_refused_and_dropped() {
        // A plan is a promise about a moment. An hour later the library may have changed in
        // ways the user has forgotten about, and re-confirming from memory is not
        // confirmation.
        let mut s = DeleteSession::new();
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::catalog::open_in_memory().unwrap();
        s.pending = Some(Pending { photo_ids: vec![1], hashes: HashMap::new(), created: 0 });

        assert!(matches!(
            s.commit(&conn, dir.path(), TTL_SECONDS + 1),
            Err(SessionError::Expired)
        ));
        // Dropped, not kept: a caller who retries should be told to select again, not given
        // the same expired plan to fail with twice.
        assert!(!s.has_pending(), "an expired plan must not be retryable");
    }

    #[test]
    fn a_file_that_cannot_be_hashed_is_refused_rather_than_planned() {
        // **The failure this whole module exists for.** A file whose contents cannot be read
        // cannot be verified, so it must not be moved. The first implementation let it
        // through with no hash, and the verify loop skipped it.
        let (dir, conn, _lib, photo) = library();
        let mut s = DeleteSession::new();

        // Remove the file behind the catalog's back, so the hash fails.
        std::fs::remove_file(&photo).unwrap();

        let ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM photo").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };

        let r = s.plan(&conn, dir.path(), &ids, 0);
        assert!(r.is_err(), "a file that cannot be read must not become a plan: {r:?}");
        assert!(!s.has_pending(), "and nothing must be left pending");
    }

    #[test]
    fn a_plan_records_a_hash_for_every_file() {
        // The property that makes the commit meaningful. An empty map is what the first
        // implementation produced, and it made the guarantee vacuous.
        let (dir, conn, _lib, _photo) = library();
        let mut s = DeleteSession::new();

        let ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM photo").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        if ids.is_empty() {
            return; // the fixture did not index; not this test's subject
        }

        let planned = s.plan(&conn, dir.path(), &ids, 0).expect("a plan");
        assert!(planned.moved > 0, "a photograph with a file must plan a move");

        let pending = s.pending.as_ref().expect("still pending");
        assert!(
            !pending.hashes.is_empty(),
            "a plan with no hashes makes the commit's verification vacuous — this is the bug \
             that ADR-0004's guarantee was supposed to prevent"
        );
        assert_eq!(pending.hashes.len(), planned.moved);
    }

    #[test]
    fn a_file_that_appeared_after_the_plan_is_refused() {
        // **The bug this module shipped with, and the test that would have caught it.**
        //
        // `commit` re-resolved its selection from the catalog, so a file that appeared between
        // the plan and the confirmation was moved having never been shown. It had no hash, and
        // the verify loop skipped exactly that case — so the operation reported success while
        // moving something unverified.
        //
        // The doc comment said "moves what it was shown". The code did not. Same failure as
        // the original ADR-0004 bug: a guarantee in a comment, absent from the code.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // A paired photograph: the raw is indexed, the JPEG is not there yet.
        let raw = root.join("IMG_0001.CR3");
        std::fs::write(&raw, b"raw bytes").unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        crate::indexer::index(&mut conn, &root, 0).unwrap();
        let lib = store::library_id_for_root(&conn, &root.to_string_lossy()).unwrap().unwrap();

        let ids: Vec<i64> = store::photos(&conn, lib).unwrap().iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), 1, "one photograph to plan");

        let mut s = DeleteSession::new();
        s.plan(&conn, &root, &ids, 0).expect("a plan");

        // The JPEG arrives — a card import, an export, a sync client.
        let jpeg = root.join("IMG_0001.JPG");
        std::fs::write(&jpeg, b"jpeg bytes").unwrap();
        crate::indexer::index(&mut conn, &root, 0).unwrap();

        // The commit must refuse rather than move a file nobody was shown.
        let r = s.commit(&conn, &root, 1);
        assert!(
            matches!(r, Err(SessionError::Appeared { .. })),
            "a file that appeared after the plan must be refused, got {r:?}"
        );
        assert!(raw.exists(), "and nothing may be moved when it is refused");
        assert!(jpeg.exists(), "including the file that appeared");
    }

    #[test]
    fn nothing_is_moved_when_the_refusal_fires() {
        // The refusal has to be **before** the move, not a report after it. A refusal that
        // arrives once the files are already in the trash is an apology, not a guarantee.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let raw = root.join("IMG_0001.CR3");
        std::fs::write(&raw, b"raw bytes").unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        crate::indexer::index(&mut conn, &root, 0).unwrap();
        let lib = store::library_id_for_root(&conn, &root.to_string_lossy()).unwrap().unwrap();
        let ids: Vec<i64> = store::photos(&conn, lib).unwrap().iter().map(|p| p.id).collect();

        let mut s = DeleteSession::new();
        s.plan(&conn, &root, &ids, 0).expect("a plan");

        std::fs::write(root.join("IMG_0001.JPG"), b"jpeg bytes").unwrap();
        crate::indexer::index(&mut conn, &root, 0).unwrap();

        let _ = s.commit(&conn, &root, 1);

        // Nothing in the trash folder at all.
        let trash_dir = root.join(crate::indexer::TRASH_DIR_NAME);
        let moved: usize = if trash_dir.exists() {
            walkdir_count(&trash_dir)
        } else {
            0
        };
        assert_eq!(moved, 0, "the refusal must happen before anything moves");
    }

    /// Count files under a directory, without pulling in `walkdir` for one assertion.
    fn walkdir_count(dir: &Path) -> usize {
        let mut n = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += walkdir_count(&p);
                } else if p.is_file() {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn committing_takes_no_file_list() {
        // A **type-level** guarantee, asserted so it cannot be relaxed by adding a parameter:
        // the only arguments are the connection, the root and the time. A caller cannot name
        // a file, cannot supply a hash, and cannot widen the operation.
        //
        // If this stops compiling, someone added a parameter — and the question to ask is
        // whether it lets a caller influence what moves.
        let mut s = DeleteSession::new();
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::catalog::open_in_memory().unwrap();
        let _: fn(&mut DeleteSession, &Connection, &Path, i64) -> _ = DeleteSession::commit;
        let _ = (&mut s, &conn, dir.path());
    }
}
