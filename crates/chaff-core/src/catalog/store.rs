//! Reading and writing the catalog.
//!
//! Everything here takes `now` as a parameter rather than reading the clock. A store
//! that calls `SystemTime::now()` internally can only be tested by waiting, and a test
//! that waits is a test that flakes. The clock belongs to the caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use super::CatalogError;
use crate::ext::{classify, FileKind};
use crate::pair::{GroupState, PhotoGroup, ReviewReason};

/// Size and modification time for one file, as observed on disk.
///
/// `mtime_ns` is the modification time in nanoseconds since the Unix epoch. Two files are
/// considered unchanged when both size and mtime match, which is what makes a re-index
/// cheap: unchanged files are not re-read, and their cached hash is not invalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub size_bytes: i64,
    pub mtime_ns: i64,
}

impl FileMeta {
    /// Read size and modification time from filesystem metadata.
    ///
    /// A filesystem may report a modification time before the Unix epoch (a copied
    /// archive, a device with a wrong clock). `duration_since` errors on that, and
    /// propagating the error would make an entire file unindexable over a timestamp.
    /// It becomes 0 instead — a caveat, not a failure.
    pub fn from_metadata(md: &std::fs::Metadata) -> Self {
        let mtime_ns = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        Self { size_bytes: md.len() as i64, mtime_ns }
    }
}

/// What one indexing pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexStats {
    pub photos: usize,
    pub files: usize,
    pub pairs: usize,
    pub needs_review: usize,
    pub ambiguous: usize,
    /// Files that were in the catalog but not seen this pass — deleted or moved away.
    pub removed_files: usize,
    /// Photographs left with no files at all after the sweep.
    pub removed_photos: usize,
    /// Decisions that followed a renamed photograph rather than dying with the old row.
    pub adopted_decisions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoRow {
    pub id: i64,
    pub dir: String,
    pub stem: String,
    pub state: String,
    pub needs_review: bool,
}

impl PhotoRow {
    /// A display name. The stem is normalised (lowercased, NFC) in storage, so this is
    /// for debugging and for tests — the UI should use the on-disk `file.path`.
    pub fn display_stem(&self) -> &str {
        &self.stem
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub role: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub content_hash: Option<String>,
}

// ---------------------------------------------------------------------------
// Mappings between the engine's types and the schema's string enums
// ---------------------------------------------------------------------------
fn role_of(kind: FileKind) -> Option<&'static str> {
    match kind {
        FileKind::Raw => Some("raw"),
        FileKind::Raster => Some("raster"),
        FileKind::Sidecar => Some("sidecar"),
        FileKind::Video => Some("video"),
        // `Other` is never indexed. A file the engine does not understand must not enter
        // the catalog at all, because everything in the catalog is something a future
        // destructive operation might consider.
        FileKind::Other => None,
    }
}

fn state_of(state: GroupState) -> &'static str {
    match state {
        GroupState::Pair => "pair",
        GroupState::RawOnly => "raw_only",
        GroupState::RasterOnly => "raster_only",
        GroupState::Ambiguous => "ambiguous",
    }
}

fn reason_of(reason: ReviewReason) -> &'static str {
    match reason {
        ReviewReason::MultipleRaw => "multiple_raw",
        ReviewReason::MultipleRaster => "multiple_raster",
        ReviewReason::PossibleDuplicateImport => "possible_duplicate_import",
    }
}

// ---------------------------------------------------------------------------
// Libraries
// ---------------------------------------------------------------------------
/// Get the library id for `root`, creating it if needed.
///
/// Idempotent: re-opening the same folder must not create a second library, or every
/// re-index would orphan the previous run's photographs and the user's ratings with them.
pub fn upsert_library(conn: &Connection, root: &Path, now: i64) -> Result<i64, CatalogError> {
    let root_str = root.to_string_lossy().to_string();
    conn.execute(
        "INSERT INTO library (root, created_at) VALUES (?1, ?2)
         ON CONFLICT (root) DO NOTHING",
        params![root_str, now],
    )?;
    let id = conn.query_row("SELECT id FROM library WHERE root = ?1", params![root_str], |r| {
        r.get(0)
    })?;
    Ok(id)
}

/// The id of the library at a root, if it is open.
///
/// The inverse of [`library_root`]. The watcher resolves one from the other so a re-index
/// cannot be pointed at a library the user did not start watching.
pub fn library_id_for_root(conn: &Connection, root: &str) -> Result<Option<i64>, CatalogError> {
    let mut stmt = conn.prepare("SELECT id FROM library WHERE root = ?1")?;
    let mut rows = stmt.query(params![root])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub fn library_root(conn: &Connection, library_id: i64) -> Result<Option<String>, CatalogError> {
    let mut stmt = conn.prepare("SELECT root FROM library WHERE id = ?1")?;
    let mut rows = stmt.query(params![library_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------
/// Write a resolved set of photograph groups into the catalog.
///
/// Runs as one transaction, so an interrupted index leaves the catalog exactly as it was
/// rather than half-updated.
///
/// ## Reconciliation
///
/// Every file written is stamped `indexed_at = now`. Afterwards, any file for this
/// library whose `indexed_at` is older is deleted, and any photograph left with no files
/// goes with it. This is a mark-and-sweep rather than a comparison against a list of
/// seen paths, because the list version needs one bound parameter per file and a large
/// library has hundreds of thousands.
/// The marker written to every file row this pass, and compared by the sweep.
///
/// **Unique to the pass, not the wall clock.** `now` is epoch *seconds*, so two passes in
/// the same second could not be told apart and the sweep deleted nothing — leaving phantom
/// photographs that no longer existed on disk, visible and ratable. That was unreachable
/// while a re-run took nine minutes; the measurement cache took it to 0.1 s and made
/// back-to-back passes in one second ordinary, turning a dormant flaw into a live one.
///
/// Nanoseconds, so two passes cannot collide. `indexed_at` is written and compared and
/// never interpreted as a date, so the unit change is invisible everywhere else.
fn pass_marker(now: i64) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(now)
}

pub fn upsert_groups(
    conn: &mut Connection,
    library_id: i64,
    groups: &[PhotoGroup],
    meta: &HashMap<PathBuf, FileMeta>,
    now: i64,
) -> Result<IndexStats, CatalogError> {
    // One marker for the whole pass: every file row written gets it, and the sweep deletes
    // whatever does not. See `pass_marker`.
    let pass = pass_marker(now);

    let tx = conn.transaction()?;
    let mut stats = IndexStats::default();

    for group in groups {
        let dir = group.key.dir.to_string_lossy().to_string();
        let stem = group.key.stem.clone();
        let needs_review = i64::from(group.needs_review());

        tx.execute(
            // **`trashed_at` is cleared on conflict, and that is load-bearing.**
            //
            // This is only reached for a group that has files, so a photograph arriving
            // here is present on disk. Without the clear, a photograph that came back —
            // dragged out of `.cull-trash` in Finder, or restored by a backup or sync —
            // kept its trashed marker and stayed invisible forever. The file was there,
            // the catalog knew about it, and no UI path could reach it: it could not be
            // selected because it was not shown, so it could not be restored either.
            //
            // Silent, permanent, and invisible from the user's side. Found by the review
            // agent, which proved it by moving a file back and re-indexing.
            "INSERT INTO photo (library_id, dir, stem, state, needs_review)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (library_id, dir, stem) DO UPDATE SET
                 state        = excluded.state,
                 needs_review = excluded.needs_review,
                 trashed_at   = NULL",
            params![library_id, dir, stem, state_of(group.state), needs_review],
        )?;

        let photo_id: i64 = tx.query_row(
            "SELECT id FROM photo WHERE library_id = ?1 AND dir = ?2 AND stem = ?3",
            params![library_id, dir, stem],
            |r| r.get(0),
        )?;

        stats.photos += 1;
        match group.state {
            GroupState::Pair => stats.pairs += 1,
            GroupState::Ambiguous => stats.ambiguous += 1,
            GroupState::RawOnly | GroupState::RasterOnly => {}
        }
        if group.needs_review() {
            stats.needs_review += 1;
        }

        // Review reasons are replaced wholesale: a reason that no longer applies must
        // disappear, or a resolved ambiguity stays flagged forever.
        tx.execute("DELETE FROM photo_review WHERE photo_id = ?1", params![photo_id])?;
        for reason in &group.review {
            tx.execute(
                "INSERT OR IGNORE INTO photo_review (photo_id, reason) VALUES (?1, ?2)",
                params![photo_id, reason_of(*reason)],
            )?;
        }

        for path in group.all_files() {
            let Some(role) = role_of(classify(path)) else { continue };

            // A file that vanished between the directory walk and this write is skipped
            // rather than recorded with placeholder metadata. Recording it would put a
            // row in the catalog for something that does not exist, and the catalog is
            // what a future delete would act on.
            let Some(m) = meta.get(path) else { continue };

            tx.execute(
                "INSERT INTO file (library_id, photo_id, path, role, size_bytes, mtime_ns, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (library_id, path) DO UPDATE SET
                     photo_id     = excluded.photo_id,
                     role         = excluded.role,
                     size_bytes   = excluded.size_bytes,
                     mtime_ns     = excluded.mtime_ns,
                     indexed_at   = excluded.indexed_at,
                     -- A changed file loses its cached hash. Keeping a stale hash would
                     -- make the pre-move verification in ADR-0004 compare a file against
                     -- a fingerprint of its previous contents and conclude it was
                     -- tampered with.
                     content_hash = CASE
                         WHEN file.size_bytes <> excluded.size_bytes
                           OR file.mtime_ns   <> excluded.mtime_ns
                         THEN NULL
                         ELSE file.content_hash
                     END",
                params![
                    library_id,
                    photo_id,
                    path.to_string_lossy(),
                    role,
                    m.size_bytes,
                    m.mtime_ns,
                    pass
                ],
            )?;
            stats.files += 1;
        }
    }

    stats.removed_files = tx.execute(
        "DELETE FROM file WHERE library_id = ?1 AND indexed_at <> ?2",
        params![library_id, pass],
    )?;

    // Sweep photographs whose files are all gone — **except those in the trash**.
    //
    // A trashed photograph has no files in the library by definition, so without the
    // `trashed_at IS NULL` guard every index pass would delete every trashed row, and
    // `decision` cascades with `photo`. The user's ratings would be destroyed by an
    // unrelated re-index, which is the worst kind of data loss: silent, delayed, and
    // attributable to something they did not connect to it.
    //
    // The row is left for the trash to own. Emptying the trash by hand leaves a row whose
    // files are gone for good, and the *next* pass after `clear_photo_trashed` sweeps it —
    // which is the correct outcome, because the photograph really is gone.
    // **A decision about to be orphaned is carried across before the cascade takes it.**
    //
    // `decision` cascades with `photo`, so a rename would destroy the rating before anything
    // could adopt it — the row is deleted and the rating goes with it. Capturing here, in the
    // same transaction as the sweep, is what makes the identity recorded in migration 010
    // worth anything: without it the identity is a note on a row that is about to vanish.
    //
    // Only decisions whose photograph is genuinely gone are carried: a photograph still
    // present keeps its own decision, and a decision cannot be adopted twice.
    let doomed: Vec<(i64, i64, i64, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT d.source_size, d.source_mtime, d.rating, d.rejected
               FROM decision d
               JOIN photo p ON p.id = d.photo_id
              WHERE p.library_id = ?1
                AND p.trashed_at IS NULL
                AND d.source_size IS NOT NULL
                AND d.source_mtime IS NOT NULL
                AND p.id NOT IN (
                    SELECT photo_id FROM file
                     WHERE library_id = ?1 AND photo_id IS NOT NULL
                )",
        )?;
        let rows = stmt.query_map(params![library_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    stats.removed_photos = tx.execute(
        "DELETE FROM photo
          WHERE library_id = ?1
            AND trashed_at IS NULL
            AND id NOT IN (
                SELECT photo_id FROM file
                 WHERE library_id = ?1 AND photo_id IS NOT NULL
            )",
        params![library_id],
    )?;

    // Now hand each carried decision to the photograph that matches it, if one is here.
    for (size, mtime, rating, rejected) in &doomed {
        let heir: Option<i64> = tx
            .query_row(
                "SELECT p.id FROM photo p
                   JOIN file f ON f.photo_id = p.id
                  WHERE p.library_id = ?1
                    AND p.trashed_at IS NULL
                    AND f.size_bytes = ?2
                    AND f.mtime_ns = ?3
                    AND f.role IN ('raw', 'raster')
                    AND NOT EXISTS (SELECT 1 FROM decision d WHERE d.photo_id = p.id)
                  ORDER BY CASE f.role WHEN 'raw' THEN 0 ELSE 1 END, p.id
                  LIMIT 1",
                params![library_id, size, mtime],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;

        if let Some(heir) = heir {
            tx.execute(
                "INSERT INTO decision (photo_id, rating, rejected, decided_at, source_size, source_mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![heir, rating, rejected, now, size, mtime],
            )?;
            stats.adopted_decisions += 1;
            log::info!("a decision followed a renamed photograph to {heir}");
        }
    }

    tx.execute(
        "UPDATE library SET last_indexed_at = ?1 WHERE id = ?2",
        params![now, library_id],
    )?;

    tx.commit()?;
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------
pub fn photos(conn: &Connection, library_id: i64) -> Result<Vec<PhotoRow>, CatalogError> {
    // Trashed photographs are hidden. Their rows survive so their decisions do — see
    // migration 004 — but the grid must behave as if they were gone.
    let mut stmt = conn.prepare(
        "SELECT id, dir, stem, state, needs_review
           FROM photo WHERE library_id = ?1 AND trashed_at IS NULL
          ORDER BY dir, stem",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok(PhotoRow {
            id: r.get(0)?,
            dir: r.get(1)?,
            stem: r.get(2)?,
            state: r.get(3)?,
            needs_review: r.get::<_, i64>(4)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn photos_needing_review(
    conn: &Connection,
    library_id: i64,
) -> Result<Vec<PhotoRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT id, dir, stem, state, needs_review
           FROM photo WHERE library_id = ?1 AND needs_review = 1
          ORDER BY dir, stem",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok(PhotoRow {
            id: r.get(0)?,
            dir: r.get(1)?,
            stem: r.get(2)?,
            state: r.get(3)?,
            needs_review: r.get::<_, i64>(4)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn review_reasons(conn: &Connection, photo_id: i64) -> Result<Vec<String>, CatalogError> {
    let mut stmt =
        conn.prepare("SELECT reason FROM photo_review WHERE photo_id = ?1 ORDER BY reason")?;
    let rows = stmt.query_map(params![photo_id], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn files_for_photo(conn: &Connection, photo_id: i64) -> Result<Vec<FileRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, role, size_bytes, mtime_ns, content_hash
           FROM file WHERE photo_id = ?1
          ORDER BY role, path",
    )?;
    let rows = stmt.query_map(params![photo_id], |r| {
        Ok(FileRow {
            id: r.get(0)?,
            path: r.get(1)?,
            role: r.get(2)?,
            size_bytes: r.get(3)?,
            mtime_ns: r.get(4)?,
            content_hash: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Files of a given role across the whole library, in a stable order.
pub fn files_by_role(
    conn: &Connection,
    library_id: i64,
    role: &str,
) -> Result<Vec<FileRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, role, size_bytes, mtime_ns, content_hash
           FROM file WHERE library_id = ?1 AND role = ?2
          ORDER BY path",
    )?;
    let rows = stmt.query_map(params![library_id, role], |r| {
        Ok(FileRow {
            id: r.get(0)?,
            path: r.get(1)?,
            role: r.get(2)?,
            size_bytes: r.get(3)?,
            mtime_ns: r.get(4)?,
            content_hash: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Record a content hash for a file. Used by the lazy hashing path.
pub fn set_content_hash(
    conn: &Connection,
    file_id: i64,
    hash: &str,
) -> Result<(), CatalogError> {
    conn.execute("UPDATE file SET content_hash = ?1 WHERE id = ?2", params![hash, file_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pair::resolve;

    fn meta_for(paths: &[&str], size: i64, mtime: i64) -> HashMap<PathBuf, FileMeta> {
        paths
            .iter()
            .map(|p| (PathBuf::from(p), FileMeta { size_bytes: size, mtime_ns: mtime }))
            .collect()
    }

    fn index(
        conn: &mut Connection,
        library_id: i64,
        files: &[&str],
        meta: &HashMap<PathBuf, FileMeta>,
        now: i64,
    ) -> IndexStats {
        let groups = resolve(files.iter().map(PathBuf::from));
        upsert_groups(conn, library_id, &groups, meta, now).expect("upsert")
    }

    #[test]
    fn a_pair_becomes_one_photograph_with_two_files() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();

        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];
        let stats = index(&mut conn, lib, &files, &meta_for(&files, 10, 1), 100);

        assert_eq!(stats.photos, 1, "a RAW+JPEG pair is ONE photograph");
        assert_eq!(stats.files, 2);
        assert_eq!(stats.pairs, 1);
        assert_eq!(stats.needs_review, 0);

        let photos = photos(&conn, lib).unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].state, "pair");

        let stored = files_for_photo(&conn, photos[0].id).unwrap();
        assert_eq!(stored.len(), 2);
        let roles: Vec<&str> = stored.iter().map(|f| f.role.as_str()).collect();
        assert!(roles.contains(&"raw") && roles.contains(&"raster"));
    }

    #[test]
    fn re_indexing_the_same_files_does_not_duplicate_anything() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];
        let meta = meta_for(&files, 10, 1);

        index(&mut conn, lib, &files, &meta, 100);
        let second = index(&mut conn, lib, &files, &meta, 200);

        assert_eq!(second.photos, 1);
        assert_eq!(second.removed_files, 0, "nothing should have been swept");
        assert_eq!(second.removed_photos, 0);
        assert_eq!(photos(&conn, lib).unwrap().len(), 1);
        assert_eq!(files_by_role(&conn, lib, "raw").unwrap().len(), 1);
    }

    #[test]
    fn a_file_that_disappears_is_swept_from_the_catalog() {
        // The reconciliation path. Without it the catalog accumulates rows for files
        // that no longer exist, and the catalog is what a future delete acts on.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();

        let both = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];
        index(&mut conn, lib, &both, &meta_for(&both, 10, 1), 100);
        assert_eq!(photos(&conn, lib).unwrap().len(), 1);

        // The JPEG is gone from disk; only the RAW is seen this pass.
        let only_raw = ["/lib/IMG_0001.CR3"];
        let stats = index(&mut conn, lib, &only_raw, &meta_for(&only_raw, 10, 1), 200);

        assert_eq!(stats.removed_files, 1, "the vanished JPEG must be swept");
        let photos = photos(&conn, lib).unwrap();
        assert_eq!(photos.len(), 1, "the photograph survives as an orphan raw");
        assert_eq!(photos[0].state, "raw_only");
        assert_eq!(files_for_photo(&conn, photos[0].id).unwrap().len(), 1);
    }

    #[test]
    fn a_photograph_with_no_files_left_is_removed_entirely() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();

        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0002.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 10, 1), 100);
        assert_eq!(photos(&conn, lib).unwrap().len(), 2);

        let remaining = ["/lib/IMG_0001.CR3"];
        let stats = index(&mut conn, lib, &remaining, &meta_for(&remaining, 10, 1), 200);
        assert_eq!(stats.removed_photos, 1);
        assert_eq!(photos(&conn, lib).unwrap().len(), 1);
    }

    #[test]
    fn a_changed_file_loses_its_cached_hash() {
        // A stale hash would make the pre-move verification of ADR-0004 compare a file
        // against a fingerprint of its old contents and wrongly report tampering.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];

        index(&mut conn, lib, &files, &meta_for(&files, 10, 1), 100);
        let f = &files_by_role(&conn, lib, "raw").unwrap()[0];
        set_content_hash(&conn, f.id, "abc123").unwrap();
        assert_eq!(
            files_by_role(&conn, lib, "raw").unwrap()[0].content_hash.as_deref(),
            Some("abc123")
        );

        // Same size, different mtime -> changed.
        let changed: HashMap<PathBuf, FileMeta> = [(
            PathBuf::from("/lib/IMG_0001.CR3"),
            FileMeta { size_bytes: 10, mtime_ns: 2 },
        )]
        .into_iter()
        .collect();
        index(&mut conn, lib, &files, &changed, 200);

        assert_eq!(
            files_by_role(&conn, lib, "raw").unwrap()[0].content_hash,
            None,
            "a file whose mtime changed must not keep its old hash"
        );
    }

    #[test]
    fn an_unchanged_file_keeps_its_cached_hash() {
        // The complement, and the reason hashing is affordable: a re-index of an
        // unchanged library must not throw away work.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];
        let meta = meta_for(&files, 10, 1);

        index(&mut conn, lib, &files, &meta, 100);
        let f = &files_by_role(&conn, lib, "raw").unwrap()[0];
        set_content_hash(&conn, f.id, "abc123").unwrap();

        index(&mut conn, lib, &files, &meta, 200);
        assert_eq!(
            files_by_role(&conn, lib, "raw").unwrap()[0].content_hash.as_deref(),
            Some("abc123"),
            "an unchanged file must keep its hash so re-indexing stays cheap"
        );
    }

    #[test]
    fn ambiguous_groups_are_flagged_and_reasons_are_stored() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.NEF", "/lib/IMG_0001.JPG"];

        let stats = index(&mut conn, lib, &files, &meta_for(&files, 10, 1), 100);
        assert_eq!(stats.ambiguous, 1);
        assert_eq!(stats.needs_review, 1);

        let review = photos_needing_review(&conn, lib).unwrap();
        assert_eq!(review.len(), 1);
        assert_eq!(review[0].state, "ambiguous");
        assert_eq!(review_reasons(&conn, review[0].id).unwrap(), vec!["multiple_raw"]);
    }

    #[test]
    fn a_resolved_ambiguity_stops_being_flagged() {
        // Review reasons are replaced, not accumulated. A reason that no longer applies
        // must disappear or the user is asked to re-check something forever.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();

        let ambiguous = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.NEF", "/lib/IMG_0001.JPG"];
        index(&mut conn, lib, &ambiguous, &meta_for(&ambiguous, 10, 1), 100);
        assert_eq!(photos_needing_review(&conn, lib).unwrap().len(), 1);

        let fixed = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];
        index(&mut conn, lib, &fixed, &meta_for(&fixed, 10, 1), 200);

        let review = photos_needing_review(&conn, lib).unwrap();
        assert!(review.is_empty(), "the ambiguity is gone, so the flag must be too");
    }

    #[test]
    fn a_duplicate_import_suspect_is_flagged_but_stays_a_separate_photograph() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG", "/lib/IMG_0001 (1).CR3"];

        index(&mut conn, lib, &files, &meta_for(&files, 10, 1), 100);

        // Two photographs, not one merged group.
        let all = photos(&conn, lib).unwrap();
        assert_eq!(all.len(), 2, "duplicate markers must never merge photographs");

        let review = photos_needing_review(&conn, lib).unwrap();
        assert_eq!(review.len(), 1);
        assert_eq!(
            review_reasons(&conn, review[0].id).unwrap(),
            vec!["possible_duplicate_import"]
        );
    }

    #[test]
    fn files_with_no_metadata_on_disk_are_not_recorded() {
        // The file vanished between the directory walk and the database write.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];

        // Only the RAW has metadata.
        let stats = index(&mut conn, lib, &files, &meta_for(&files[..1], 10, 1), 100);
        assert_eq!(stats.files, 1);
        assert_eq!(files_by_role(&conn, lib, "raster").unwrap().len(), 0);
        assert_eq!(files_by_role(&conn, lib, "raw").unwrap().len(), 1);
    }

    #[test]
    fn non_image_files_never_reach_the_catalog() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/notes.txt", "/lib/README"];

        index(&mut conn, lib, &files, &meta_for(&files, 5, 1), 100);
        assert_eq!(stats_total_files(&conn, lib), 1);
    }

    fn stats_total_files(conn: &Connection, library_id: i64) -> usize {
        ["raw", "raster", "sidecar", "video"]
            .iter()
            .map(|r| files_by_role(conn, library_id, r).unwrap().len())
            .sum()
    }

    #[test]
    fn sidecars_travel_with_their_photograph() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG", "/lib/IMG_0001.XMP"];

        index(&mut conn, lib, &files, &meta_for(&files, 5, 1), 100);
        let p = &photos(&conn, lib).unwrap()[0];
        assert_eq!(files_for_photo(&conn, p.id).unwrap().len(), 3);
        assert_eq!(files_by_role(&conn, lib, "sidecar").unwrap().len(), 1);
    }

    #[test]
    fn upserting_a_library_twice_returns_the_same_id() {
        // Re-opening a folder must not create a second library, or every re-index would
        // orphan the previous run's photographs and the user's ratings with them.
        let conn = crate::catalog::open_in_memory().unwrap();
        let a = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let b = upsert_library(&conn, Path::new("/lib"), 200).unwrap();
        assert_eq!(a, b);

        let n: i64 = conn.query_row("SELECT COUNT(*) FROM library", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn two_libraries_do_not_see_each_others_photographs() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let a = upsert_library(&conn, Path::new("/a"), 100).unwrap();
        let b = upsert_library(&conn, Path::new("/b"), 100).unwrap();

        let fa = ["/a/IMG_0001.CR3"];
        index(&mut conn, a, &fa, &meta_for(&fa, 1, 1), 100);

        assert_eq!(photos(&conn, a).unwrap().len(), 1);
        assert_eq!(photos(&conn, b).unwrap().len(), 0);
    }

    #[test]
    fn deleting_a_photograph_cascades_to_its_files() {
        // The paired delete depends on this. If the cascade did not fire, removing a
        // photograph would leave orphaned file rows that a later sweep could act on.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG", "/lib/IMG_0001.XMP"];
        index(&mut conn, lib, &files, &meta_for(&files, 5, 1), 100);

        let p = &photos(&conn, lib).unwrap()[0];
        assert_eq!(files_for_photo(&conn, p.id).unwrap().len(), 3);

        conn.execute("DELETE FROM photo WHERE id = ?1", params![p.id]).unwrap();
        assert_eq!(stats_total_files(&conn, lib), 0, "files must cascade with the photo");
    }

    // ---------------------------------------------------------------------
    // Decisions
    // ---------------------------------------------------------------------
    #[test]
    fn setting_a_decision_returns_what_it_replaced() {
        // The previous value is what an undo stack is built from.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;

        // Nothing set yet.
        let previous = set_decision(&conn, id, Decision { rating: Rating::new(4), rejected: false }, 200).unwrap();
        assert!(previous.is_unrated(), "the first decision replaces nothing");

        let previous = set_decision(&conn, id, Decision { rating: Rating::new(5), rejected: false }, 300).unwrap();
        assert_eq!(previous, Decision { rating: Rating::new(4), rejected: false });

        assert_eq!(
            decision_for_photo(&conn, id).unwrap(),
            Some(Decision { rating: Rating::new(5), rejected: false })
        );
    }

    #[test]
    fn a_reject_flag_keeps_the_rating_beside_it() {
        // A flag beside the stars, not a rating value. Every photo tool behaves this way
        // and a rejected photograph that silently lost its rating would be a surprise.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;

        set_decision(&conn, id, Decision { rating: Rating::new(3), rejected: false }, 200).unwrap();
        set_decision(&conn, id, Decision { rating: Rating::new(3), rejected: true }, 300).unwrap();

        let d = decision_for_photo(&conn, id).unwrap().unwrap();
        assert_eq!(d.rating.get(), 3, "rejecting must not clear the rating");
        assert!(d.rejected);
    }

    #[test]
    fn decisions_survive_a_re_index_that_recomputes_everything_else() {
        // **The property that matters most.** Scores are derived and get rewritten on
        // every pass. Decisions exist nowhere else and must not be touched.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];
        let meta = meta_for(&files, 10, 1);

        index(&mut conn, lib, &files, &meta, 100);
        let id = photos(&conn, lib).unwrap()[0].id;
        set_decision(&conn, id, Decision { rating: Rating::new(5), rejected: false }, 150).unwrap();

        // Re-index, and re-score at a new version for good measure.
        index(&mut conn, lib, &files, &meta, 200);
        upsert_score(&conn, id, "composite", 12.0, 99, 200).unwrap();

        let after = photos(&conn, lib).unwrap()[0].id;
        assert_eq!(after, id, "a re-index of unchanged files keeps the same photograph row");
        assert_eq!(
            decision_for_photo(&conn, id).unwrap(),
            Some(Decision { rating: Rating::new(5), rejected: false }),
            "re-indexing must not disturb the user's judgement"
        );
    }

    #[test]
    fn deleting_a_photograph_takes_its_decision_with_it() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;
        set_decision(&conn, id, Decision { rating: Rating::new(4), rejected: false }, 200).unwrap();

        conn.execute("DELETE FROM photo WHERE id = ?1", params![id]).unwrap();
        assert_eq!(decision_for_photo(&conn, id).unwrap(), None);
    }

    #[test]
    fn decisions_can_be_listed_for_a_whole_library_at_once() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0002.CR3", "/lib/IMG_0003.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let all = photos(&conn, lib).unwrap();

        set_decision(&conn, all[0].id, Decision { rating: Rating::new(5), rejected: false }, 200).unwrap();
        set_decision(&conn, all[1].id, Decision { rating: Rating::new(0), rejected: true }, 200).unwrap();

        let map = decisions_for_library(&conn, lib).unwrap();
        assert_eq!(map.len(), 2, "the third photograph is undecided and must be absent");
        assert_eq!(map[&all[0].id].rating.get(), 5);
        assert!(map[&all[1].id].rejected);
        assert!(!map.contains_key(&all[2].id));
    }

    #[test]
    fn the_rating_range_is_enforced_by_the_schema() {
        // A CHECK constraint rather than trust. Six stars is not a rating, and a value
        // that escaped the UI would be stored happily without it.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;

        for bad in [-1i64, 6, 99] {
            let r = conn.execute(
                "INSERT INTO decision (photo_id, rating, rejected, decided_at) VALUES (?1, ?2, 0, 1)",
                params![id, bad],
            );
            assert!(r.is_err(), "rating {bad} must be rejected by the schema");
        }
    }

    #[test]
    fn the_decision_count_ignores_unrated_unrejected_rows() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0002.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let all = photos(&conn, lib).unwrap();

        assert_eq!(decision_count(&conn, lib).unwrap(), 0);
        // An explicit zero is the same as undecided, and must not be counted as a decision.
        set_decision(&conn, all[0].id, Decision { rating: Rating::new(0), rejected: false }, 200).unwrap();
        assert_eq!(decision_count(&conn, lib).unwrap(), 0);
        set_decision(&conn, all[1].id, Decision { rating: Rating::new(1), rejected: false }, 200).unwrap();
        assert_eq!(decision_count(&conn, lib).unwrap(), 1);
    }

    #[test]
    fn decisions_in_one_library_are_invisible_to_another() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let a = upsert_library(&conn, Path::new("/a"), 100).unwrap();
        let b = upsert_library(&conn, Path::new("/b"), 100).unwrap();
        let fa = ["/a/IMG_0001.CR3"];
        index(&mut conn, a, &fa, &meta_for(&fa, 1, 1), 100);
        let id = photos(&conn, a).unwrap()[0].id;
        set_decision(&conn, id, Decision { rating: Rating::new(5), rejected: false }, 200).unwrap();

        assert_eq!(decisions_for_library(&conn, b).unwrap().len(), 0);
        assert_eq!(decision_count(&conn, b).unwrap(), 0);
    }

    // ---------------------------------------------------------------------
    // Directories
    // ---------------------------------------------------------------------
    #[test]
    fn directories_count_both_directly_and_recursively() {
        // Both numbers, because they answer different questions: "how many in this shoot?"
        // and "how many under 2024?". A tree showing only direct counts makes every parent
        // look empty.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = [
            "/lib/2024/01 - Iceland/IMG_0001.CR3",
            "/lib/2024/01 - Iceland/IMG_0002.CR3",
            "/lib/2024/02 - Portugal/IMG_0003.CR3",
            "/lib/2023/party/IMG_0004.CR3",
        ];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);

        let dirs = directories(&conn, lib).unwrap();
        let find = |p: &str| dirs.iter().find(|d| d.path == p).cloned();

        assert_eq!(find("/lib/2024/01 - Iceland").unwrap().direct, 2);
        assert_eq!(find("/lib/2024/01 - Iceland").unwrap().recursive, 2);
        assert_eq!(find("/lib/2024").unwrap().direct, 0, "nothing sits directly in 2024");
        assert_eq!(find("/lib/2024").unwrap().recursive, 3, "but three are beneath it");
        assert_eq!(find("/lib").unwrap().recursive, 4);
    }

    #[test]
    fn a_prefix_does_not_leak_across_sibling_folders() {
        // `2024-01` must not be counted under `2024`. A plain string prefix would do
        // exactly that, which is why the test uses a separator.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/2024/a.CR3", "/lib/2024-01/b.CR3", "/lib/2024-01/c.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);

        let dirs = directories(&conn, lib).unwrap();
        let find = |p: &str| dirs.iter().find(|d| d.path == p).cloned();
        assert_eq!(find("/lib/2024").unwrap().recursive, 1, "only its own");
        assert_eq!(find("/lib/2024-01").unwrap().recursive, 2);
    }

    #[test]
    fn trashed_photographs_are_not_counted_in_the_tree() {
        // The tree must agree with the grid. A folder reading "12" that shows 9 is the
        // kind of small lie that erodes trust in every other number.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/shoot/IMG_0001.CR3", "/lib/shoot/IMG_0002.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);

        assert_eq!(directories(&conn, lib).unwrap().iter().find(|d| d.path == "/lib/shoot").unwrap().recursive, 2);

        let id = photos(&conn, lib).unwrap()[0].id;
        mark_photo_trashed(&conn, id, 200).unwrap();

        assert_eq!(
            directories(&conn, lib).unwrap().iter().find(|d| d.path == "/lib/shoot").unwrap().recursive,
            1,
            "the tree must count what the grid shows"
        );
    }

    #[test]
    fn an_empty_library_has_no_directories() {
        let conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        assert!(directories(&conn, lib).unwrap().is_empty());
    }

    /// An `ExifData` with just the fields the facet tests read.
    fn exif_of(make: &str, model: &str, lens: &str, captured_at: i64) -> crate::exif::ExifData {
        crate::exif::ExifData {
            captured_at: Some(captured_at),
            make: Some(make.to_string()),
            model: Some(model.to_string()),
            lens: Some(lens.to_string()),
            ..Default::default()
        }
    }

    /// A library with `n` faces in one photograph, each with a distinct embedding.
    fn faces_lib(embeddings: &[Vec<f32>]) -> (Connection, i64, Vec<i64>) {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let fid = files_by_role(&conn, lib, "raw").unwrap()[0].id;

        let rows: Vec<FaceRow> = (0..embeddings.len())
            .map(|i| FaceRow {
                file_id: fid,
                x: i as f64,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                confidence: 1.0,
            })
            .collect();
        let landmarks: Vec<Vec<u8>> = (0..embeddings.len()).map(|_| vec![0u8; 40]).collect();
        replace_faces(&conn, &FaceDetection {
            file_id: fid, faces: &rows, landmarks: &landmarks,
            size: 1, mtime: 1, detector: "test", now: 100,
        }).unwrap();

        let ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM face ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        for (id, v) in ids.iter().zip(embeddings) {
            upsert_embedding(&conn, *id, v, "test", 100).unwrap();
        }
        (conn, lib, ids)
    }

    fn basis(dim: usize, block: usize) -> Vec<f32> {
        let mut v = vec![0f32; dim];
        for k in 0..4 {
            v[(block * 16 + k) % dim] = 1.0;
        }
        v
    }

    /// A library with one photograph, for the tag tests.
    fn tag_lib() -> (Connection, i64, i64) {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;
        (conn, lib, id)
    }

    #[test]
    fn a_rename_does_not_lose_a_rating() {
        // **The bug this exists for.** A photograph is `(dir, stem)`, so renaming
        // `IMG_0001.CR3` to `IMG_0001-edit.CR3` creates a *new* row on the next index and
        // the rating stays attached to one nothing points at. The user renamed a file and
        // lost their work on it.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();

        let before = ["/lib/IMG_0001.CR3"];
        index(&mut conn, lib, &before, &meta_for(&before, 100, 5000), 100);
        let old_id = photos(&conn, lib).unwrap()[0].id;
        set_decision(&conn, old_id, Decision { rating: Rating::new(5), rejected: false }, 150)
            .unwrap();

        // Renamed, with the same size and modification time — which is what `mv` does.
        let after = ["/lib/IMG_0001-edit.CR3"];
        index(&mut conn, lib, &after, &meta_for(&after, 100, 5000), 200);

        let new_photos = photos(&conn, lib).unwrap();
        assert_eq!(new_photos.len(), 1, "the rename is a new photograph");
        let new_id = new_photos[0].id;
        assert_ne!(new_id, old_id, "and it really is a different row");

        // **The rating followed the file.** The first version of this test asserted the
        // opposite — that the decision stayed on the old row, adoptable by identity — and it
        // was wrong about the mechanism: `decision` cascades with `photo`, so the sweep
        // destroys the row before anything can adopt it. The decision has to be carried
        // across *inside* the index transaction, before the cascade, which is what the
        // implementation now does.
        let adopted = decision_for_photo(&conn, new_id).unwrap();
        assert!(adopted.is_some(), "the rating must follow the rename");
        assert_eq!(adopted.unwrap().rating.get(), 5);

        // And nothing is left adoptable: the decision moved rather than being copied.
        assert!(orphaned_decisions(&conn, lib).unwrap().is_empty());
    }

    #[test]
    fn a_decision_whose_photograph_still_exists_is_not_adoptable() {
        // **The rule that stops double-claiming.** Two photographs of the same size and
        // time — a burst frame, a re-export — must not both claim one rating, or the second
        // silently overwrites the first.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 100, 5000), 100);
        let id = photos(&conn, lib).unwrap()[0].id;
        set_decision(&conn, id, Decision { rating: Rating::new(4), rejected: false }, 150).unwrap();

        // The photograph is still here, so its decision is not looking for a new home.
        assert!(orphaned_decisions(&conn, lib).unwrap().is_empty());
    }

    #[test]
    fn a_decision_made_before_the_identity_existed_is_simply_not_adoptable() {
        // Rows written by an older catalog have no identity. They are not adopted and they
        // do not crash — the rating stays on the row it was made about, which is what
        // happened before this feature existed.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 100, 5000), 100);
        let id = photos(&conn, lib).unwrap()[0].id;

        conn.execute(
            "INSERT INTO decision (photo_id, rating, rejected, decided_at) VALUES (?1, 3, 0, 100)",
            params![id],
        )
        .unwrap();
        conn.execute("DELETE FROM photo WHERE id = ?1", params![id]).unwrap();

        assert!(orphaned_decisions(&conn, lib).unwrap().is_empty());
    }

    #[test]
    fn tags_are_normalised_to_one_vocabulary() {
        // "Beach", "beach" and "BEACH" are one tag. A vocabulary that treats them as three
        // is one nobody can filter, and the model does emit all three.
        let (conn, lib, id) = tag_lib();
        replace_tags(&conn, id, &[
            ("Beach".into(), 0.9),
            ("  beach  ".into(), 0.8),
            ("BEACH".into(), 0.7),
        ], "m", None, 100).unwrap();

        let tags = tags_for_photo(&conn, id).unwrap();
        assert_eq!(tags.len(), 1, "got {tags:?}");
        assert_eq!(tags[0].name, "beach");
        let counts = tag_counts(&conn, lib, None).unwrap();
        assert_eq!(counts, vec![("beach".to_string(), 1)]);
    }

    #[test]
    fn a_second_model_does_not_replace_the_first() {
        // **Two models are two opinions, and the user asked for both.** Replacing would make
        // "compare two models" impossible and would silently delete work.
        let (conn, lib, id) = tag_lib();
        replace_tags(&conn, id, &[("beach".into(), 0.9)], "model-a", Some("a shore"), 100).unwrap();
        replace_tags(&conn, id, &[("mountain".into(), 0.8)], "model-b", Some("a peak"), 200).unwrap();

        let tags = tags_for_photo(&conn, id).unwrap();
        assert_eq!(tags.len(), 2, "both models' tags must survive: {tags:?}");

        // And each can be filtered to on its own, which is the point of provenance.
        assert_eq!(tag_counts(&conn, lib, Some("model-a")).unwrap().len(), 1);
        assert_eq!(photos_with_tag(&conn, lib, "beach", Some("model-a")).unwrap(), vec![id]);
        assert!(photos_with_tag(&conn, lib, "beach", Some("model-b")).unwrap().is_empty());
    }

    #[test]
    fn re_tagging_with_the_same_model_replaces() {
        // A re-run is a complete answer for that model, not an addition to the last one.
        let (conn, _lib, id) = tag_lib();
        replace_tags(&conn, id, &[("beach".into(), 0.9), ("sunset".into(), 0.5)], "m", None, 100)
            .unwrap();
        replace_tags(&conn, id, &[("beach".into(), 0.95)], "m", None, 200).unwrap();

        let tags = tags_for_photo(&conn, id).unwrap();
        assert_eq!(tags.len(), 1, "sunset must be gone: {tags:?}");
        assert!((tags[0].confidence - 0.95).abs() < 1e-9, "and the new confidence used");
    }

    #[test]
    fn an_empty_tag_name_is_not_a_tag() {
        let (conn, _lib, id) = tag_lib();
        replace_tags(&conn, id, &[("".into(), 0.9), ("   ".into(), 0.8), ("real".into(), 0.7)], "m", None, 100)
            .unwrap();
        let tags = tags_for_photo(&conn, id).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "real");
    }

    #[test]
    fn tags_are_ranked_by_how_many_photographs_carry_them() {
        // The vocabulary a library actually has is more useful than an alphabetical list of
        // everything, most of which occurs once.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3", "/lib/b.CR3", "/lib/c.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let ids: Vec<i64> = photos(&conn, lib).unwrap().iter().map(|p| p.id).collect();

        replace_tags(&conn, ids[0], &[("sky".into(), 0.9)], "m", None, 100).unwrap();
        replace_tags(&conn, ids[1], &[("sky".into(), 0.9), ("rare".into(), 0.9)], "m", None, 100).unwrap();
        replace_tags(&conn, ids[2], &[("sky".into(), 0.9)], "m", None, 100).unwrap();

        let counts = tag_counts(&conn, lib, None).unwrap();
        assert_eq!(counts[0], ("sky".to_string(), 3));
        assert_eq!(counts[1], ("rare".to_string(), 1));
    }

    #[test]
    fn tags_cascade_with_the_photograph() {
        let (conn, _lib, id) = tag_lib();
        replace_tags(&conn, id, &[("beach".into(), 0.9)], "m", Some("a shore"), 100).unwrap();
        conn.execute("DELETE FROM photo WHERE id = ?1", params![id]).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tag", [], |r| r.get(0)).unwrap();
        let c: i64 = conn.query_row("SELECT COUNT(*) FROM photo_caption", [], |r| r.get(0)).unwrap();
        assert_eq!((n, c), (0, 0), "tags and captions must not outlive the photograph");
    }

    #[test]
    fn the_caption_is_read_back_per_model() {
        let (conn, _lib, id) = tag_lib();
        replace_tags(&conn, id, &[("beach".into(), 0.9)], "m", Some("  a shoreline  "), 100).unwrap();
        assert_eq!(caption_for_photo(&conn, id, "m").unwrap().as_deref(), Some("a shoreline"));
        assert_eq!(caption_for_photo(&conn, id, "other").unwrap(), None);
    }

    #[test]
    fn trashed_photographs_are_not_offered_for_tagging() {
        // Tagging something the user has already rejected is wasted GPU time, and a run over
        // a large library is minutes per hundred photographs.
        let (conn, lib, id) = tag_lib();
        assert_eq!(photos_needing_tags(&conn, lib, "m").unwrap().len(), 1);
        mark_photo_trashed(&conn, id, 200).unwrap();
        assert!(photos_needing_tags(&conn, lib, "m").unwrap().is_empty());
    }

    #[test]
    fn a_tagged_photograph_drops_out_of_the_work_list() {
        let (conn, lib, id) = tag_lib();
        assert_eq!(photos_needing_tags(&conn, lib, "m").unwrap().len(), 1);
        replace_tags(&conn, id, &[("beach".into(), 0.9)], "m", Some("x"), 100).unwrap();
        assert!(photos_needing_tags(&conn, lib, "m").unwrap().is_empty());
        // But a different model still has work to do.
        assert_eq!(photos_needing_tags(&conn, lib, "other").unwrap().len(), 1);
    }

    #[test]
    fn naming_a_person_confirms_the_group() {
        // Naming **is** the confirmation. A group a human put a name to is a decision, and
        // the next clustering pass must leave it alone.
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 0)]);
        replace_people(&conn, lib, std::slice::from_ref(&ids), 100).unwrap();
        let p = people(&conn, lib).unwrap()[0].id;
        assert!(!people(&conn, lib).unwrap()[0].confirmed);

        name_person(&conn, p, Some("  Ada  "), 200).unwrap();
        let after = &people(&conn, lib).unwrap()[0];
        assert_eq!(after.name.as_deref(), Some("Ada"), "the name must be trimmed");
        assert!(after.confirmed);
    }

    #[test]
    fn an_empty_name_is_the_same_as_no_name() {
        // "" and "not named" are one state to every reader, and two representations of one
        // state is how they drift apart.
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 0)]);
        replace_people(&conn, lib, &[ids], 100).unwrap();
        let p = people(&conn, lib).unwrap()[0].id;

        name_person(&conn, p, Some("   "), 200).unwrap();
        assert_eq!(people(&conn, lib).unwrap()[0].name, None);
    }

    #[test]
    fn merging_moves_every_face_and_confirms_both_sides() {
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 0), basis(128, 1), basis(128, 1)]);
        replace_people(&conn, lib, &[vec![ids[0], ids[1]], vec![ids[2], ids[3]]], 100).unwrap();
        let all = people(&conn, lib).unwrap();
        assert_eq!(all.len(), 2);

        let moved = merge_people(&conn, all[1].id, all[0].id, 200).unwrap();
        assert_eq!(moved, 2);

        let after = people(&conn, lib).unwrap();
        assert_eq!(after.len(), 1, "the source group must be gone");
        assert_eq!(after[0].faces, 4);
        assert!(after[0].confirmed, "a hand-made merge is a decision");
    }

    #[test]
    fn merging_a_group_into_itself_does_nothing() {
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 0)]);
        replace_people(&conn, lib, &[ids], 100).unwrap();
        let p = people(&conn, lib).unwrap()[0].id;
        assert_eq!(merge_people(&conn, p, p, 200).unwrap(), 0);
        assert_eq!(people(&conn, lib).unwrap().len(), 1, "and must not delete it");
    }

    #[test]
    fn splitting_creates_a_confirmed_group_and_leaves_the_source_alive() {
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 1), basis(128, 2), basis(128, 3)]);
        replace_people(&conn, lib, std::slice::from_ref(&ids), 100).unwrap();
        let p = people(&conn, lib).unwrap()[0].id;

        let new_id = split_person(&conn, p, &[ids[0], ids[1]], 200).unwrap();
        assert!(new_id.is_some());

        let after = people(&conn, lib).unwrap();
        assert_eq!(after.len(), 2, "one group became two");
        assert!(after.iter().all(|g| g.confirmed), "a hand-made split is a decision");
        let sizes: Vec<usize> = after.iter().map(|g| g.faces).collect();
        assert!(sizes.contains(&2), "each side has two faces: {sizes:?}");
    }

    #[test]
    fn splitting_everything_out_is_refused() {
        // A person with no faces is not a group. Leaving one behind puts an empty row in the
        // list that cannot be selected or removed.
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 1)]);
        replace_people(&conn, lib, std::slice::from_ref(&ids), 100).unwrap();
        let p = people(&conn, lib).unwrap()[0].id;

        assert_eq!(split_person(&conn, p, &ids, 200).unwrap(), None);
        assert_eq!(split_person(&conn, p, &[], 200).unwrap(), None);
        assert_eq!(people(&conn, lib).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_person_keeps_the_faces() {
        // Only the grouping is discarded. The faces are still detections, still in their
        // photographs, and the next pass may group them differently.
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 0)]);
        replace_people(&conn, lib, std::slice::from_ref(&ids), 100).unwrap();
        let p = people(&conn, lib).unwrap()[0].id;

        delete_person(&conn, p).unwrap();
        assert!(people(&conn, lib).unwrap().is_empty());
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM face", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, ids.len() as i64, "the faces must survive");
    }

    #[test]
    fn a_face_between_two_groups_is_flagged_for_review() {
        // **What the review queue exists for.** A face that landed on the wrong side of a
        // threshold is simply wrong, and without this nobody ever sees it.
        //
        // **Two groups are required** — the first version of this test put all three faces
        // in one group and expected ambiguity, which contradicts
        // `a_single_group_has_nothing_ambiguous` two tests down. A face is ambiguous
        // *relative to another group*; with one group there is nothing to be between.
        let a = basis(128, 0);
        let b = basis(128, 1);
        // A face sitting exactly between the two groups.
        let between: Vec<f32> = a.iter().zip(b.iter()).map(|(x, y)| (x + y) / 2.0).collect();

        // Two settled faces in A, two in B, and the waverer placed in A.
        let (conn, lib, ids) = faces_lib(&[
            a.clone(),
            a.clone(),
            b.clone(),
            b.clone(),
            between,
        ]);
        replace_people(&conn, lib, &[vec![ids[0], ids[1], ids[4]], vec![ids[2], ids[3]]], 100)
            .unwrap();

        let flagged = ambiguous_faces(&conn, lib, "test", 0.5, 10).unwrap();
        assert!(
            flagged.iter().any(|f| f.face_id == ids[4]),
            "the face between two groups must be flagged: {flagged:?}"
        );

        // The settled faces must not be. A review queue that flags everything is one nobody
        // reads.
        for settled in [ids[0], ids[2]] {
            assert!(
                !flagged.iter().any(|f| f.face_id == settled),
                "a settled face was flagged: {flagged:?}"
            );
        }

        // Worst margin first, because the most likely to be wrong is the most worth looking
        // at.
        if flagged.len() > 1 {
            assert!(flagged[0].own - flagged[0].other <= flagged[1].own - flagged[1].other);
        }
    }

    #[test]
    fn a_single_group_has_nothing_ambiguous() {
        // With one group there is no "other" to be confused with, so the queue is empty.
        // Returning everything would make the review queue useless on a small library.
        let (conn, lib, ids) = faces_lib(&[basis(128, 0), basis(128, 0)]);
        replace_people(&conn, lib, &[ids], 100).unwrap();
        assert!(ambiguous_faces(&conn, lib, "test", 0.5, 10).unwrap().is_empty());
    }

    #[test]
    fn an_embedding_round_trips_through_the_blob() {
        // 128 f32 as 512 bytes. An off-by-one in the encoding gives vectors that are subtly
        // wrong, and a wrong embedding clusters the wrong people together — a failure that
        // looks like a bad model rather than a bad encoder.
        let v: Vec<f32> = (0..128).map(|i| (i as f32 - 64.0) / 7.0).collect();
        let bytes = encode_vector(&v);
        assert_eq!(bytes.len(), 512);
        assert_eq!(decode_vector(&bytes), v);

        // And through the database, which is where it actually has to survive.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let fid = files_by_role(&conn, lib, "raw").unwrap()[0].id;
        let one = vec![FaceRow { file_id: fid, x: 0.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 }];
        replace_faces(&conn, &FaceDetection {
            file_id: fid, faces: &one, landmarks: &[vec![0; 40]],
            size: 1, mtime: 1, detector: "test", now: 100,
        }).unwrap();
        let face_id: i64 = conn.query_row("SELECT id FROM face LIMIT 1", [], |r| r.get(0)).unwrap();

        upsert_embedding(&conn, face_id, &v, "sface", 100).unwrap();
        let read = faces_with_embeddings(&conn, lib, "sface").unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].2, v);
    }

    #[test]
    fn confirmed_people_survive_a_recluster() {
        // **The property that makes corrections stick.** A user who has merged two groups
        // and split a third has changed the answer; discarding that on the next pass would
        // make the feature unusable, because the corrections would not survive a restart.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let fid = files_by_role(&conn, lib, "raw").unwrap()[0].id;

        let three = vec![
            FaceRow { file_id: fid, x: 0.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 },
            FaceRow { file_id: fid, x: 2.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 },
            FaceRow { file_id: fid, x: 4.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 },
        ];
        replace_faces(&conn, &FaceDetection {
            file_id: fid, faces: &three, landmarks: &[vec![0; 40], vec![0; 40], vec![0; 40]],
            size: 1, mtime: 1, detector: "test", now: 100,
        }).unwrap();
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM face ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };

        // A first pass groups all three, then the user confirms it.
        replace_people(&conn, lib, std::slice::from_ref(&ids), 100).unwrap();
        conn.execute("UPDATE person SET confirmed = 1, name = 'Someone'", []).unwrap();

        // A second pass suggests a different grouping entirely.
        let written = replace_people(&conn, lib, &[vec![ids[0], ids[1]], vec![ids[1], ids[2]]], 200).unwrap();
        assert!(written >= 1);

        let all = people(&conn, lib).unwrap();
        let confirmed: Vec<_> = all.iter().filter(|p| p.confirmed).collect();
        assert_eq!(confirmed.len(), 1, "the confirmed group must survive");
        assert_eq!(confirmed[0].name.as_deref(), Some("Someone"));
        assert_eq!(confirmed[0].faces, 3, "and keep its members");
    }

    #[test]
    fn a_recluster_does_not_pull_confirmed_faces_into_a_new_group() {
        // The confirmed group's members must be excluded from the new partition, or a
        // re-cluster would move them behind the user's back.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let fid = files_by_role(&conn, lib, "raw").unwrap()[0].id;
        let three = vec![
            FaceRow { file_id: fid, x: 0.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 },
            FaceRow { file_id: fid, x: 2.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 },
            FaceRow { file_id: fid, x: 4.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 },
        ];
        replace_faces(&conn, &FaceDetection {
            file_id: fid, faces: &three, landmarks: &[vec![0; 40], vec![0; 40], vec![0; 40]],
            size: 1, mtime: 1, detector: "test", now: 100,
        }).unwrap();
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM face ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };

        replace_people(&conn, lib, &[vec![ids[0], ids[1]]], 100).unwrap();
        conn.execute("UPDATE person SET confirmed = 1", []).unwrap();

        // A new pass suggests grouping all three. Faces 0 and 1 are taken.
        replace_people(&conn, lib, std::slice::from_ref(&ids), 200).unwrap();

        let groups = people(&conn, lib).unwrap();
        for g in &groups {
            let members = photos_for_person(&conn, g.id).unwrap();
            let _ = members;
        }
        // The unconfirmed suggestion must contain only the face that was free.
        let unconfirmed: Vec<_> = groups.iter().filter(|p| !p.confirmed).collect();
        assert!(
            unconfirmed.iter().all(|p| p.faces < 2),
            "a new group took a confirmed face: {unconfirmed:?}"
        );
    }

    #[test]
    fn faces_replace_rather_than_accumulate() {
        // A detection pass is a complete answer for the file it ran on. Merging would keep
        // a face the detector no longer finds — a ghost no later pass could remove.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let fid = files_by_role(&conn, lib, "raw").unwrap()[0].id;

        let two = vec![
            FaceRow { file_id: fid, x: 1.0, y: 2.0, width: 3.0, height: 4.0, confidence: 0.9 },
            FaceRow { file_id: fid, x: 5.0, y: 6.0, width: 7.0, height: 8.0, confidence: 0.8 },
        ];
        replace_faces(&conn, &FaceDetection {
            file_id: fid,
            faces: &two,
            landmarks: &[vec![0; 40], vec![0; 40]],
            size: 1,
            mtime: 1,
            detector: "test",
            now: 100,
        })
        .unwrap();
        assert_eq!(faces_for_photo(&conn, photos(&conn, lib).unwrap()[0].id).unwrap().len(), 2);

        // A second pass that finds one face must leave one, not three.
        replace_faces(&conn, &FaceDetection {
            file_id: fid,
            faces: &two[..1],
            landmarks: &[vec![0; 40]],
            size: 1,
            mtime: 1,
            detector: "test",
            now: 200,
        })
        .unwrap();
        assert_eq!(faces_for_photo(&conn, photos(&conn, lib).unwrap()[0].id).unwrap().len(), 1);
    }

    #[test]
    fn faces_cascade_with_their_file() {
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let fid = files_by_role(&conn, lib, "raw").unwrap()[0].id;
        let one = vec![FaceRow {
            file_id: fid,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            confidence: 1.0,
        }];
        replace_faces(&conn, &FaceDetection {
            file_id: fid,
            faces: &one,
            landmarks: &[vec![0; 40]],
            size: 1,
            mtime: 1,
            detector: "test",
            now: 100,
        })
        .unwrap();

        conn.execute("DELETE FROM file WHERE id = ?1", params![fid]).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM face", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "faces must not outlive the file they were found in");
    }

    #[test]
    fn only_stale_or_missing_files_are_offered_for_detection() {
        // The same rule the measurement cache uses, and for the same reason: it detects a
        // change without reading the file. A file whose size or mtime moved is detected
        // again; one that has not is left alone.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3", "/lib/b.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let raws = files_by_role(&conn, lib, "raw").unwrap();

        assert_eq!(files_needing_faces(&conn, lib, "yunet").unwrap().len(), 2);

        let one = vec![FaceRow { file_id: raws[0].id, x: 0.0, y: 0.0, width: 1.0, height: 1.0, confidence: 1.0 }];
        replace_faces(&conn, &FaceDetection {
            file_id: raws[0].id,
            faces: &one,
            landmarks: &[vec![0; 40]],
            size: raws[0].size_bytes,
            mtime: raws[0].mtime_ns,
            detector: "yunet",
            now: 100,
        })
        .unwrap();

        let pending = files_needing_faces(&conn, lib, "yunet").unwrap();
        assert_eq!(pending.len(), 1, "the detected file must drop out");
        assert_eq!(pending[0].id, raws[1].id);

        // A different detector's results do not count for this one.
        assert_eq!(files_needing_faces(&conn, lib, "other").unwrap().len(), 2);
    }

    #[test]
    fn a_photograph_that_comes_back_becomes_visible_again() {
        // **Finding 2 from the review.** A photograph dragged out of `.cull-trash` in
        // Finder, or restored by a backup or sync, kept its trashed marker and stayed
        // invisible forever. The file was on disk, the catalog knew about it, and no UI
        // path could reach it — it could not be selected because it was not shown, so it
        // could not be restored either. Silent, permanent, and invisible from the user's
        // side.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3"];
        let meta = meta_for(&files, 1, 1);

        index(&mut conn, lib, &files, &meta, 100);
        let id = photos(&conn, lib).unwrap()[0].id;
        mark_photo_trashed(&conn, id, 200).unwrap();
        assert!(photos(&conn, lib).unwrap().is_empty(), "trashed, so hidden");

        // The file comes back — by any means that is not Chaff's own restore.
        index(&mut conn, lib, &files, &meta, 300);

        let visible = photos(&conn, lib).unwrap();
        assert_eq!(visible.len(), 1, "a photograph with files on disk must be visible");
        assert_eq!(visible[0].id, id, "and it must be the same row, so the rating survives");
    }

    #[test]
    fn two_index_passes_in_the_same_second_still_sweep() {
        // **Finding 3 from the review.** The sweep compared `indexed_at` against epoch
        // *seconds*, so two passes in one second could not be told apart and nothing was
        // deleted — leaving phantom photographs visible and ratable. Unreachable while a
        // re-run took nine minutes; the measurement cache took it to 0.1 s and made
        // back-to-back passes in one second ordinary.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();

        let three = ["/lib/a.CR3", "/lib/b.CR3", "/lib/c.CR3"];
        index(&mut conn, lib, &three, &meta_for(&three, 1, 1), 1_700_000_000);
        assert_eq!(photos(&conn, lib).unwrap().len(), 3);

        // The same `now` — the caller's clock did not advance.
        let two = ["/lib/a.CR3", "/lib/b.CR3"];
        index(&mut conn, lib, &two, &meta_for(&two, 1, 1), 1_700_000_000);

        assert_eq!(
            photos(&conn, lib).unwrap().len(),
            2,
            "a file that is gone must be swept even when the clock has not moved"
        );
    }

    #[test]
    fn metadata_prefers_the_raw_and_does_not_repeat_the_make() {
        // Two things at once, because they are the same query. The raw must win — a JPEG
        // exported from it can have had its metadata rewritten — and "Canon" + "Canon EOS
        // R5" is one camera, not two entries in a filter list.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/IMG_0001.CR3", "/lib/IMG_0001.JPG"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;

        let raw_id = files_by_role(&conn, lib, "raw").unwrap()[0].id;
        let jpg_id = files_by_role(&conn, lib, "raster").unwrap()[0].id;
        upsert_exif(&conn, raw_id, 1, Some(&exif_of("Canon", "Canon EOS R5", "RF 24-70", 1_700_000_000)), 100)
            .unwrap();
        upsert_exif(&conn, jpg_id, 1, Some(&exif_of("SOMETHING", "ELSE ENTIRELY", "wrong lens", 1)), 100)
            .unwrap();

        let m = photo_metadata(&conn, lib).unwrap();
        let meta = m.get(&id).expect("the photograph must appear");
        assert_eq!(meta.camera.as_deref(), Some("Canon EOS R5"), "the raw must win");
        assert_eq!(meta.lens.as_deref(), Some("RF 24-70"));
        assert_eq!(meta.year, Some(2023));
    }

    #[test]
    fn a_make_the_model_does_not_contain_is_joined() {
        // The other half of the rule: "NIKON CORPORATION" + "NIKON Z 6" contains the make,
        // but "Canon" + "EOS R5" does not, and dropping it would lose the brand.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3", "/lib/b.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let all = photos(&conn, lib).unwrap();

        let raws = files_by_role(&conn, lib, "raw").unwrap();
        upsert_exif(&conn, raws[0].id, 1, Some(&exif_of("Canon", "EOS R5", "", 1_700_000_000)), 100)
            .unwrap();
        upsert_exif(
            &conn,
            raws[1].id,
            1,
            Some(&exif_of("NIKON CORPORATION", "NIKON Z 6", "", 1_700_000_000)),
            100,
        )
        .unwrap();

        let m = photo_metadata(&conn, lib).unwrap();
        assert_eq!(m[&all[0].id].camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(m[&all[1].id].camera.as_deref(), Some("NIKON Z 6"), "make already inside");
    }

    #[test]
    fn metadata_ignores_trashed_photographs() {
        // The filter lists must describe what the grid can show. An option that yields an
        // empty result is a dead end the user has to discover by trying it.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/a.CR3"];
        index(&mut conn, lib, &files, &meta_for(&files, 1, 1), 100);
        let id = photos(&conn, lib).unwrap()[0].id;
        let f = files_by_role(&conn, lib, "raw").unwrap()[0].id;
        upsert_exif(&conn, f, 1, Some(&exif_of("Canon", "EOS R5", "", 1_700_000_000)), 100).unwrap();

        assert!(photo_metadata(&conn, lib).unwrap().contains_key(&id));
        mark_photo_trashed(&conn, id, 200).unwrap();
        assert!(!photo_metadata(&conn, lib).unwrap().contains_key(&id));
    }

    #[test]
    fn settings_round_trip_and_replace() {
        let conn = crate::catalog::open_in_memory().unwrap();
        assert!(settings(&conn).unwrap().is_empty());

        set_setting(&conn, "last_library", "/photos", 100).unwrap();
        set_setting(&conn, "last_folder", "/photos/2024", 100).unwrap();
        let m = settings(&conn).unwrap();
        assert_eq!(m.get("last_library").map(String::as_str), Some("/photos"));
        assert_eq!(m.get("last_folder").map(String::as_str), Some("/photos/2024"));

        // Replacing, not appending — a preference has one value, not a history.
        set_setting(&conn, "last_folder", "/photos/2023", 200).unwrap();
        assert_eq!(settings(&conn).unwrap().len(), 2);
        assert_eq!(
            settings(&conn).unwrap().get("last_folder").map(String::as_str),
            Some("/photos/2023")
        );

        clear_setting(&conn, "last_folder").unwrap();
        assert_eq!(settings(&conn).unwrap().len(), 1);
        // Removing a key that is not there is not an error: a caller clearing a preference
        // that was never set should not have to check first.
        clear_setting(&conn, "never_set").unwrap();
    }

    #[test]
    fn stems_are_stored_normalised_so_platforms_agree() {
        // The same Unicode-correction that pair.rs applies must survive into storage,
        // or a pair matches in memory and diverges in the database.
        let mut conn = crate::catalog::open_in_memory().unwrap();
        let lib = upsert_library(&conn, Path::new("/lib"), 100).unwrap();
        let files = ["/lib/CAF\u{00e9}.CR3", "/lib/cafe\u{0301}.JPG"];

        index(&mut conn, lib, &files, &meta_for(&files, 5, 1), 100);
        let all = photos(&conn, lib).unwrap();
        assert_eq!(all.len(), 1, "NFC and NFD spellings must be one photograph");
        assert_eq!(all[0].stem, all[0].stem.to_lowercase());
    }
}

// ---------------------------------------------------------------------------
// EXIF
// ---------------------------------------------------------------------------
/// Files whose EXIF has never been read, or whose bytes have changed since it was.
///
/// This is what keeps a re-index cheap: everything already examined is skipped, so a
/// second pass over an unchanged library does no EXIF I/O whatsoever.
pub fn files_needing_exif(
    conn: &Connection,
    library_id: i64,
) -> Result<Vec<(i64, PathBuf, i64)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.mtime_ns
           FROM file f
           LEFT JOIN exif e ON e.file_id = f.id
          WHERE f.library_id = ?1
            AND f.role IN ('raw', 'raster')
            AND (e.file_id IS NULL OR e.source_mtime_ns <> f.mtime_ns)
          ORDER BY f.path",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((r.get::<_, i64>(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get::<_, i64>(2)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Record what a file's EXIF read produced.
///
/// A row is written even when nothing was found. Storing "examined, empty" is what
/// stops the reader being re-run against the same metadata-free file on every index —
/// and metadata-free files are common (stripped exports, screenshots, scans).
pub fn upsert_exif(
    conn: &Connection,
    file_id: i64,
    source_mtime_ns: i64,
    data: Option<&crate::exif::ExifData>,
    now: i64,
) -> Result<(), CatalogError> {
    let d = data.cloned().unwrap_or_default();
    conn.execute(
        "INSERT INTO exif (file_id, source_mtime_ns, captured_at, make, model, lens,
                           iso, f_number, exposure_time, focal_length, orientation, read_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (file_id) DO UPDATE SET
             source_mtime_ns = excluded.source_mtime_ns,
             captured_at     = excluded.captured_at,
             make            = excluded.make,
             model           = excluded.model,
             lens            = excluded.lens,
             iso             = excluded.iso,
             f_number        = excluded.f_number,
             exposure_time   = excluded.exposure_time,
             focal_length    = excluded.focal_length,
             orientation     = excluded.orientation,
             read_at         = excluded.read_at",
        params![
            file_id,
            source_mtime_ns,
            d.captured_at,
            d.make,
            d.model,
            d.lens,
            d.iso.map(|v| v as i64),
            d.f_number,
            d.exposure_time,
            d.focal_length,
            d.orientation.map(|v| v as i64),
            now
        ],
    )?;
    Ok(())
}

pub fn exif_for_file(
    conn: &Connection,
    file_id: i64,
) -> Result<Option<crate::exif::ExifData>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT captured_at, make, model, lens, iso, f_number, exposure_time,
                focal_length, orientation
           FROM exif WHERE file_id = ?1",
    )?;
    let mut rows = stmt.query(params![file_id])?;
    match rows.next()? {
        None => Ok(None),
        Some(r) => Ok(Some(crate::exif::ExifData {
            captured_at: r.get(0)?,
            make: r.get(1)?,
            model: r.get(2)?,
            lens: r.get(3)?,
            iso: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
            f_number: r.get(5)?,
            exposure_time: r.get(6)?,
            focal_length: r.get(7)?,
            orientation: r.get::<_, Option<i64>>(8)?.map(|v| v as u16),
        })),
    }
}

/// Every photograph in a library with its capture time, ordered by time.
///
/// This is the input to burst grouping: one camera body, frames seconds apart. Rows
/// with no capture time are excluded rather than given a placeholder, because a
/// fabricated timestamp would group unrelated photographs into a burst and the user
/// would be shown a keeper selection for frames that were never a sequence.
pub fn capture_times(
    conn: &Connection,
    library_id: i64,
) -> Result<Vec<(i64, String, i64)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT p.id, COALESCE(e.model, e.make, ''), e.captured_at
           FROM photo p
           JOIN file f ON f.photo_id = p.id AND f.role = 'raw'
           JOIN exif e ON e.file_id = f.id
          WHERE p.library_id = ?1 AND e.captured_at IS NOT NULL
          ORDER BY e.captured_at",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// Scores
// ---------------------------------------------------------------------------
/// One metric for one photograph.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreRow {
    pub metric: String,
    pub value: f64,
    pub scorer_version: i64,
}

/// Write a frame's scores.
///
/// Keyed by `scorer_version`, so re-scoring after a metric changes writes *new* rows
/// rather than overwriting the old ones. That keeps "why did this photograph's score
/// change?" answerable, and makes a scorer regression reversible by reverting the version
/// instead of by re-deriving what the numbers used to be.
pub fn upsert_score(
    conn: &Connection,
    photo_id: i64,
    metric: &str,
    value: f64,
    scorer_version: i64,
    now: i64,
) -> Result<(), CatalogError> {
    conn.execute(
        "INSERT INTO score (photo_id, metric, value, scorer_version, computed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (photo_id, metric, scorer_version) DO UPDATE SET
             value       = excluded.value,
             computed_at = excluded.computed_at",
        params![photo_id, metric, value, scorer_version, now],
    )?;
    Ok(())
}

/// Every score recorded for a photograph at a given scorer version.
pub fn scores_for_photo(
    conn: &Connection,
    photo_id: i64,
    scorer_version: i64,
) -> Result<Vec<ScoreRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT metric, value, scorer_version FROM score
          WHERE photo_id = ?1 AND scorer_version = ?2
          ORDER BY metric",
    )?;
    let rows = stmt.query_map(params![photo_id, scorer_version], |r| {
        Ok(ScoreRow { metric: r.get(0)?, value: r.get(1)?, scorer_version: r.get(2)? })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Every photograph's composite score, as a map for the grid.
///
/// One query rather than one per row: a 50,000-photo grid asking per cell is 50,000
/// round trips through SQLite, which is the difference between a grid that appears and
/// one that crawls.
pub fn composites(
    conn: &Connection,
    library_id: i64,
    scorer_version: i64,
) -> Result<std::collections::HashMap<i64, f64>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT s.photo_id, s.value
           FROM score s
           JOIN photo p ON p.id = s.photo_id
          WHERE p.library_id = ?1 AND s.metric = 'composite' AND s.scorer_version = ?2",
    )?;
    let rows = stmt.query_map(params![library_id, scorer_version], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
}

/// Remove scores from an older scorer version.
///
/// Explicit rather than automatic: keeping a previous version's numbers is how a
/// regression gets diagnosed, so discarding them is the user's decision and not a
/// side effect of re-scoring.
pub fn delete_scores_at_version(
    conn: &Connection,
    scorer_version: i64,
) -> Result<usize, CatalogError> {
    let n = conn.execute("DELETE FROM score WHERE scorer_version = ?1", params![scorer_version])?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// Decisions — the user's own judgement
// ---------------------------------------------------------------------------
/// A star rating, guaranteed to be in range.
///
/// A newtype rather than a bare `u8`, so an out-of-range rating is **unrepresentable**
/// rather than merely rejected later. The `CHECK` constraint in the schema stays — defence
/// in depth costs nothing — but a value that cannot be constructed cannot reach it.
///
/// This replaced a bare `u8` plus a `rating.min(5)` at the call site. The test for that
/// clamp read `assert_eq!(9u8.min(5), 5)`, which asserts the Rust standard library rather
/// than this crate, and clippy said so out loud ("`9u8` is never smaller than `5`"). The
/// clamp now lives in one place and is tested through the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Rating(u8);

impl Rating {
    /// The highest rating, and the value an out-of-range input becomes.
    pub const MAX: u8 = 5;

    /// Build a rating, clamping anything above [`Self::MAX`].
    ///
    /// Clamps rather than rejects: the input comes from a keystroke, and a user pressing
    /// `9` should get five stars rather than an error dialog. A database read also passes
    /// through here, where an out-of-range value means corruption — and a visible
    /// degradation beats a panic.
    pub fn new(value: u8) -> Self {
        Self(value.min(Self::MAX))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// True when no rating has been given. Distinct from one star.
    pub fn is_unrated(self) -> bool {
        self.0 == 0
    }
}

impl From<Rating> for u8 {
    fn from(r: Rating) -> u8 {
        r.0
    }
}

impl std::fmt::Display for Rating {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a person decided about a photograph.
///
/// Distinct from [`super::super::scoring::composite::Band`], which is what the *engine*
/// thinks. A photograph can be scored `Keep` and rated one star, or scored `Reject` and
/// rated five. Both are stored, and neither overwrites the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decision {
    pub rating: Rating,
    pub rejected: bool,
}

impl Decision {
    pub fn is_unrated(&self) -> bool {
        self.rating.is_unrated() && !self.rejected
    }

    /// The XMP `xmp:Rating` value this maps to. Kept here so the interop path (#54) and
    /// the UI cannot disagree about what a rating means.
    pub fn xmp_rating(&self) -> u8 {
        self.rating.get()
    }
}

/// Set a photograph's decision, returning what it was before.
///
/// The previous value is returned so a caller can push it onto an undo stack. Undo lives
/// in the session rather than in the database — the PRD scopes it that way, and a
/// permanent history of every keystroke is a different feature with different costs.
pub fn set_decision(
    conn: &Connection,
    photo_id: i64,
    decision: Decision,
    now: i64,
) -> Result<Decision, CatalogError> {
    let previous = decision_for_photo(conn, photo_id)?.unwrap_or_default();

    // **What this decision is about, recorded while the file is known.**
    //
    // A photograph is `(dir, stem)`, so a rename creates a new one and the rating would stay
    // attached to a row nothing points at. Size and modification time both survive a rename
    // and cost nothing to record here — see migration 010.
    let identity = primary_file_identity(conn, photo_id)?;

    conn.execute(
        "INSERT INTO decision (photo_id, rating, rejected, decided_at, source_size, source_mtime)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (photo_id) DO UPDATE SET
             rating       = excluded.rating,
             rejected     = excluded.rejected,
             decided_at   = excluded.decided_at,
             source_size  = excluded.source_size,
             source_mtime = excluded.source_mtime",
        params![
            photo_id,
            decision.rating.get() as i64,
            i64::from(decision.rejected),
            now,
            identity.map(|(s, _)| s),
            identity.map(|(_, m)| m)
        ],
    )?;
    Ok(previous)
}

pub fn decision_for_photo(
    conn: &Connection,
    photo_id: i64,
) -> Result<Option<Decision>, CatalogError> {
    let mut stmt =
        conn.prepare("SELECT rating, rejected FROM decision WHERE photo_id = ?1")?;
    let mut rows = stmt.query(params![photo_id])?;
    match rows.next()? {
        Some(r) => Ok(Some(Decision {
            rating: Rating::new(r.get::<_, i64>(0)?.clamp(0, u8::MAX as i64) as u8),
            rejected: r.get::<_, i64>(1)? != 0,
        })),
        None => Ok(None),
    }
}

/// Every decision in a library, keyed by photograph.
///
/// One query rather than one per cell: the grid asks about fifty thousand photographs at
/// once, and a round trip per cell is the difference between a grid that appears and one
/// that crawls.
pub fn decisions_for_library(
    conn: &Connection,
    library_id: i64,
) -> Result<std::collections::HashMap<i64, Decision>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT d.photo_id, d.rating, d.rejected
           FROM decision d
           JOIN photo p ON p.id = d.photo_id
          WHERE p.library_id = ?1",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Decision {
                rating: Rating::new(r.get::<_, i64>(1)?.clamp(0, u8::MAX as i64) as u8),
                rejected: r.get::<_, i64>(2)? != 0,
            },
        ))
    })?;
    Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
}

/// How many photographs in a library carry any decision at all.
/// How many photographs have a composite score.
///
/// `score` is `(photo_id, metric, value)` — a long table, not a wide one — so this counts
/// rows for the `composite` metric rather than a column. The first version queried
/// `s.composite`, which does not exist, and reported **zero** on a library the same run had
/// just reported 33 scored photographs for.
pub fn scored_count(conn: &Connection, library_id: i64) -> Result<usize, CatalogError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT s.photo_id) FROM score s
           JOIN photo p ON p.id = s.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL AND s.metric = 'composite'",
        params![library_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

pub fn decision_count(conn: &Connection, library_id: i64) -> Result<usize, CatalogError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM decision d JOIN photo p ON p.id = d.photo_id
          WHERE p.library_id = ?1 AND (d.rating > 0 OR d.rejected = 1)",
        params![library_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

// ---------------------------------------------------------------------------
// Trash state
// ---------------------------------------------------------------------------
/// Record that a photograph's files have moved to the trash.
///
/// The row survives deliberately. `decision` cascades with `photo`, so removing the row
/// would take the user's rating with it and a restore would bring the file back unrated.
pub fn mark_photo_trashed(
    conn: &Connection,
    photo_id: i64,
    now: i64,
) -> Result<(), CatalogError> {
    conn.execute(
        "UPDATE photo SET trashed_at = ?2 WHERE id = ?1",
        params![photo_id, now],
    )?;
    Ok(())
}

/// Bring a photograph back into the library, keeping whatever was decided about it.
pub fn clear_photo_trashed(conn: &Connection, photo_id: i64) -> Result<(), CatalogError> {
    conn.execute("UPDATE photo SET trashed_at = NULL WHERE id = ?1", params![photo_id])?;
    Ok(())
}

/// Bring back every photograph whose files are at these paths.
///
/// Used by restore, which knows the paths but not the row ids.
pub fn clear_trashed_for_paths(
    conn: &Connection,
    paths: &[String],
) -> Result<usize, CatalogError> {
    let mut n = 0;
    for path in paths {
        n += conn.execute(
            "UPDATE photo SET trashed_at = NULL
              WHERE id IN (SELECT photo_id FROM file WHERE path = ?1)",
            params![path],
        )?;
    }
    Ok(n)
}

/// The content hash the catalog recorded for a file, if it has one.
///
/// Filled lazily, so `Ok(None)` is the common answer rather than an error. The trash
/// engine treats a missing hash as "nothing to verify against" and relies on its own
/// read-back instead.
pub fn content_hash_for_path(
    conn: &Connection,
    path: &str,
) -> Result<Option<String>, CatalogError> {
    let mut stmt = conn.prepare("SELECT content_hash FROM file WHERE path = ?1")?;
    let mut rows = stmt.query(params![path])?;
    match rows.next()? {
        Some(r) => Ok(r.get::<_, Option<String>>(0)?),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Measurements — what a score was computed from
// ---------------------------------------------------------------------------
/// A stored measurement, with the file identity it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMeasurement {
    pub values: [f64; crate::scoring::shoot::N_METRICS],
    pub camera: Option<String>,
    pub captured_at: Option<i64>,
    pub measured_path: String,
    pub measured_size: i64,
    pub measured_mtime: i64,
}

/// Write a measurement, replacing any previous one for this photograph and version.
pub fn upsert_measurement(
    conn: &Connection,
    photo_id: i64,
    scorer_version: i64,
    m: &StoredMeasurement,
    now: i64,
) -> Result<(), CatalogError> {
    conn.execute(
        "INSERT INTO measurement
            (photo_id, scorer_version, measured_path, measured_size, measured_mtime,
             camera, captured_at, m0, m1, m2, m3, m4, m5, m6, m7, measured_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT (photo_id, scorer_version) DO UPDATE SET
             measured_path  = excluded.measured_path,
             measured_size  = excluded.measured_size,
             measured_mtime = excluded.measured_mtime,
             camera         = excluded.camera,
             captured_at    = excluded.captured_at,
             m0=excluded.m0, m1=excluded.m1, m2=excluded.m2, m3=excluded.m3,
             m4=excluded.m4, m5=excluded.m5, m6=excluded.m6, m7=excluded.m7,
             measured_at    = excluded.measured_at",
        params![
            photo_id,
            scorer_version,
            m.measured_path,
            m.measured_size,
            m.measured_mtime,
            m.camera,
            m.captured_at,
            m.values[0], m.values[1], m.values[2], m.values[3],
            m.values[4], m.values[5], m.values[6], m.values[7],
            now,
        ],
    )?;
    Ok(())
}

/// Every stored measurement for a scorer version, keyed by photograph.
///
/// One query rather than one per photograph: a pass over fifty thousand photographs asking
/// per row is fifty thousand round trips, which is the cost this whole table exists to
/// avoid.
pub fn measurements(
    conn: &Connection,
    library_id: i64,
    scorer_version: i64,
) -> Result<std::collections::HashMap<i64, StoredMeasurement>, CatalogError> {
    // **Scoped to the library being indexed.** Without the join this read every library's
    // measurements and discarded the ones that did not match — tens of thousands of rows
    // at scale, on every pass, for nothing.
    let mut stmt = conn.prepare(
        "SELECT m.photo_id, m.measured_path, m.measured_size, m.measured_mtime, m.camera,
                m.captured_at, m.m0, m.m1, m.m2, m.m3, m.m4, m.m5, m.m6, m.m7
           FROM measurement m
           JOIN photo p ON p.id = m.photo_id
          WHERE p.library_id = ?1 AND m.scorer_version = ?2",
    )?;
    let rows = stmt.query_map(params![library_id, scorer_version], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            StoredMeasurement {
                measured_path: r.get(1)?,
                measured_size: r.get(2)?,
                measured_mtime: r.get(3)?,
                camera: r.get(4)?,
                captured_at: r.get(5)?,
                values: [
                    r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?,
                    r.get(10)?, r.get(11)?, r.get(12)?, r.get(13)?,
                ],
            },
        ))
    })?;
    Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
}

/// Remove measurements from an older scorer version.
pub fn delete_measurements_at_version(
    conn: &Connection,
    scorer_version: i64,
) -> Result<usize, CatalogError> {
    Ok(conn.execute("DELETE FROM measurement WHERE scorer_version = ?1", params![scorer_version])?)
}

// ---------------------------------------------------------------------------
// Directories
// ---------------------------------------------------------------------------
/// One folder in a library, with how many photographs it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRow {
    pub path: String,
    /// Photographs whose files sit directly in this folder.
    pub direct: usize,
    /// Photographs in this folder **or any folder beneath it**.
    ///
    /// Both numbers, because they answer different questions. "How many are in this
    /// shoot?" is `direct`. "How many are under 2024?" is `recursive`, and a tree that
    /// showed only the direct count would make every parent look empty.
    pub recursive: usize,
}

/// Every folder in a library that holds photographs, and its counts.
///
/// One query for the direct counts, then the recursive totals accumulated in memory.
/// Doing it in SQL would need a recursive CTE per folder, and a library has tens of
/// thousands of photographs across a few hundred folders — the arithmetic is cheaper than
/// the query.
pub fn directories(conn: &Connection, library_id: i64) -> Result<Vec<DirectoryRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT dir, COUNT(*) FROM photo
          WHERE library_id = ?1 AND trashed_at IS NULL
          GROUP BY dir ORDER BY dir",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
    })?;
    let direct: Vec<(String, usize)> = rows.collect::<Result<Vec<_>, _>>()?;

    // Every ancestor of every folder that holds a photograph, so a parent appears even
    // when nothing sits directly in it. A tree with holes is not a tree.
    let mut all: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (dir, n) in &direct {
        all.insert(dir.clone(), *n);
        let mut current = Path::new(dir).parent();
        while let Some(parent) = current {
            let p = parent.to_string_lossy().to_string();
            if p.is_empty() {
                break;
            }
            all.entry(p).or_insert(0);
            current = parent.parent();
        }
    }

    let mut out: Vec<DirectoryRow> = all
        .into_iter()
        .map(|(path, count)| {
            // Recursive: this folder's own photographs plus every descendant's. A prefix
            // test on the path with a separator, so `2024-01` does not match `2024-010`.
            let prefix = format!("{path}/");
            let recursive: usize = direct
                .iter()
                .filter(|(d, _)| *d == path || d.starts_with(&prefix))
                .map(|(_, n)| *n)
                .sum();
            DirectoryRow { path, direct: count, recursive }
        })
        .collect();

    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------
/// Every remembered value.
pub fn settings(conn: &Connection) -> Result<std::collections::HashMap<String, String>, CatalogError> {
    let mut stmt = conn.prepare("SELECT key, value FROM setting")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
}

/// Remember a value, replacing any previous one.
pub fn set_setting(conn: &Connection, key: &str, value: &str, now: i64) -> Result<(), CatalogError> {
    conn.execute(
        "INSERT INTO setting (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now],
    )?;
    Ok(())
}

/// Forget a value. Removing a key that is not there is not an error.
pub fn clear_setting(conn: &Connection, key: &str) -> Result<(), CatalogError> {
    conn.execute("DELETE FROM setting WHERE key = ?1", params![key])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Facets — what a library contains, for filtering by it
// ---------------------------------------------------------------------------
/// The camera, lens and capture time of a photograph, from whichever file carries them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PhotoMetadata {
    pub camera: Option<String>,
    pub lens: Option<String>,
    /// The year the camera recorded, from its local wall-clock. See `exif.rs`.
    pub year: Option<i32>,
}

/// Record what a decision was made about, so a rename does not orphan it.
///
/// Called whenever a decision is written. The primary file's size and modification time both
/// survive a rename, and neither costs anything to record — see migration 010 for why a
/// content hash is deliberately not used here.
pub fn remember_decision_identity(
    conn: &Connection,
    photo_id: i64,
) -> Result<(), CatalogError> {
    conn.execute(
        "UPDATE decision SET source_size = ?, source_mtime = ?
          WHERE photo_id = ?",
        params![
            primary_file_identity(conn, photo_id)?.map(|(s, _)| s),
            primary_file_identity(conn, photo_id)?.map(|(_, m)| m),
            photo_id
        ],
    )?;
    Ok(())
}

/// The size and modification time of a photograph's primary file.
fn primary_file_identity(conn: &Connection, photo_id: i64) -> Result<Option<(i64, i64)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT size_bytes, mtime_ns FROM file
          WHERE photo_id = ?1 AND role IN ('raw', 'raster')
          ORDER BY CASE role WHEN 'raw' THEN 0 ELSE 1 END, id
          LIMIT 1",
    )?;
    let mut rows = stmt.query(params![photo_id])?;
    match rows.next()? {
        Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
        None => Ok(None),
    }
}

/// Decisions that belong to no photograph any more, keyed by what they were made about.
///
/// **Restricted to orphans on purpose.** A decision whose photograph still exists is not
/// available for adoption: two photographs of the same size and time — a burst frame, a
/// re-export — would both claim one rating, and the second would silently overwrite the
/// first. Only a decision whose photograph has gone is looking for a new home.
pub fn orphaned_decisions(
    conn: &Connection,
    library_id: i64,
) -> Result<std::collections::HashMap<(i64, i64), Decision>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT d.source_size, d.source_mtime, d.rating, d.rejected
           FROM decision d
          WHERE d.source_size IS NOT NULL
            AND d.source_mtime IS NOT NULL
            AND NOT EXISTS (
                SELECT 1 FROM photo p WHERE p.id = d.photo_id AND p.library_id = ?1
            )",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((
            (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?),
            Decision {
                rating: Rating::new(r.get::<_, i64>(2)? as u8),
                rejected: r.get::<_, i64>(3)? != 0,
            },
        ))
    })?;
    Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
}

/// Every photograph's camera, lens and year, keyed by photograph.
///
/// One query rather than one per photograph. The join prefers the **raw** — it is the file
/// the camera wrote, and a JPEG exported from it may have had its metadata rewritten or
/// stripped — using the same `CASE` ordering the detail panel uses, so the two cannot
/// disagree about which file describes a photograph.
pub fn photo_metadata(
    conn: &Connection,
    library_id: i64,
) -> Result<std::collections::HashMap<i64, PhotoMetadata>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.photo_id, e.make, e.model, e.lens, e.captured_at
           FROM exif e
           JOIN file f ON f.id = e.file_id
           JOIN photo p ON p.id = f.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL AND f.photo_id IS NOT NULL
          ORDER BY CASE f.role WHEN 'raw' THEN 0 ELSE 1 END",
    )?;

    let mut out: std::collections::HashMap<i64, PhotoMetadata> = std::collections::HashMap::new();
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;

    for row in rows {
        let (photo_id, make, model, lens, captured_at) = row?;
        // First row wins: the ORDER BY has already put the raw first, and a second file's
        // EXIF must not overwrite it.
        out.entry(photo_id).or_insert_with(|| PhotoMetadata {
            camera: combine_camera(make, model),
            lens: lens.filter(|l| !l.trim().is_empty()),
            year: captured_at.and_then(year_of),
        });
    }
    Ok(out)
}

/// `make` + `model`, without repeating the make when the model already contains it.
///
/// "Canon" and "Canon EOS R5" are one fact. A filter list showing both as separate options
/// splits one camera's photographs across two entries, which is worse than showing neither.
fn combine_camera(make: Option<String>, model: Option<String>) -> Option<String> {
    let make = make.filter(|m| !m.trim().is_empty());
    let model = model.filter(|m| !m.trim().is_empty());
    match (make, model) {
        (Some(m), Some(d)) if model_already_names_the_make(&m, &d) => Some(d),
        (Some(m), Some(d)) => Some(format!("{m} {d}")),
        (None, Some(d)) => Some(d),
        (Some(m), None) => Some(m),
        (None, None) => None,
    }
}

/// Does the model already say who made it?
///
/// Two shapes, both common. `Canon` + `Canon EOS R5` repeats the make exactly. `NIKON
/// CORPORATION` + `NIKON Z 6` does not — the model carries the make's **first word**, which
/// is the brand, while the make carries the legal entity. A plain `starts_with` catches the
/// first and misses the second, which is how one camera ends up split across two entries in
/// a filter list.
fn model_already_names_the_make(make: &str, model: &str) -> bool {
    let make = make.to_lowercase();
    let model_lower = model.to_lowercase();
    if model_lower.starts_with(&make) {
        return true;
    }
    match make.split_whitespace().next() {
        // Only when the make has more than one word — otherwise this is the check above.
        Some(first) if first.len() >= 3 && make.contains(' ') => model_lower.starts_with(first),
        _ => false,
    }
}

/// The year of a capture timestamp, which is a local wall-clock treated as UTC.
fn year_of(epoch: i64) -> Option<i32> {
    chaff_civil_year(epoch)
}

/// `YYYY` for an epoch second, in the same convention `exif.rs` uses.
fn chaff_civil_year(epoch: i64) -> Option<i32> {
    let date = crate::trash::civil_date(epoch);
    date.get(..4)?.parse().ok()
}

// ---------------------------------------------------------------------------
// Faces
// ---------------------------------------------------------------------------
/// One detection pass over one file.
///
/// A struct rather than eight positional arguments. Four of them are `i64` — file id, size,
/// mtime, timestamp — and swapping two of those compiles, runs, and produces a catalog that
/// is quietly wrong. Clippy flagged the count; the reason it is worth fixing is the types.
#[derive(Debug, Clone, Copy)]
pub struct FaceDetection<'a> {
    pub file_id: i64,
    pub faces: &'a [FaceRow],
    /// One blob per face, in the same order.
    pub landmarks: &'a [Vec<u8>],
    /// The file's size and modification time at detection, for staleness.
    pub size: i64,
    pub mtime: i64,
    pub detector: &'a str,
    pub now: i64,
}

/// One detected face, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceRow {
    pub file_id: i64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub confidence: f64,
}

/// Replace the faces recorded for a file.
///
/// Replaced wholesale rather than merged: a detection pass is a complete answer for the
/// file it ran on, and merging would keep a face that the detector no longer finds — a
/// ghost that no later pass could remove.
pub fn replace_faces(
    conn: &Connection,
    detection: &FaceDetection<'_>,
) -> Result<(), CatalogError> {
    let FaceDetection { file_id, faces, landmarks, size, mtime, detector, now } = *detection;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM face WHERE file_id = ?1", params![file_id])?;
    for (f, lm) in faces.iter().zip(landmarks) {
        tx.execute(
            "INSERT INTO face (file_id, x, y, width, height, confidence, landmarks,
                               detected_size, detected_mtime, detector, detected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                file_id, f.x, f.y, f.width, f.height, f.confidence, lm, size, mtime, detector, now
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// How many faces were found in each photograph of a library.
///
/// One query, and the count is what the "has people" filter needs. Returning the boxes for
/// a whole library would be tens of thousands of rows to draw one badge.
pub fn face_counts(
    conn: &Connection,
    library_id: i64,
) -> Result<std::collections::HashMap<i64, usize>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.photo_id, COUNT(face.id)
           FROM face
           JOIN file f ON f.id = face.file_id
           JOIN photo p ON p.id = f.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL AND f.photo_id IS NOT NULL
          GROUP BY f.photo_id",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize))
    })?;
    Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
}

/// Every face in one photograph, with the file it was found in.
pub fn faces_for_photo(conn: &Connection, photo_id: i64) -> Result<Vec<FaceRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT face.file_id, face.x, face.y, face.width, face.height, face.confidence
           FROM face JOIN file f ON f.id = face.file_id
          WHERE f.photo_id = ?1
          ORDER BY face.confidence DESC",
    )?;
    let rows = stmt.query_map(params![photo_id], |r| {
        Ok(FaceRow {
            file_id: r.get(0)?,
            x: r.get(1)?,
            y: r.get(2)?,
            width: r.get(3)?,
            height: r.get(4)?,
            confidence: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The files whose faces are missing or stale, for a detection pass to work through.
///
/// A file is stale when its size or modification time differs from what it had when
/// detection last ran — the same rule the measurement cache uses, and for the same reason:
/// it detects a change without reading the file.
pub fn files_needing_faces(
    conn: &Connection,
    library_id: i64,
    detector: &str,
) -> Result<Vec<FileRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.role, f.size_bytes, f.mtime_ns, f.content_hash
           FROM file f
           JOIN photo p ON p.id = f.photo_id
          WHERE p.library_id = ?1
            AND p.trashed_at IS NULL
            AND f.role IN ('raw', 'raster')
            AND NOT EXISTS (
                SELECT 1 FROM face
                 WHERE face.file_id = f.id
                   AND face.detector = ?2
                   AND face.detected_size = f.size_bytes
                   AND face.detected_mtime = f.mtime_ns
            )
          ORDER BY f.id",
    )?;
    let rows = stmt.query_map(params![library_id, detector], |r| {
        Ok(FileRow {
            id: r.get(0)?,
            path: r.get(1)?,
            role: r.get(2)?,
            size_bytes: r.get(3)?,
            mtime_ns: r.get(4)?,
            content_hash: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// Embeddings and people
// ---------------------------------------------------------------------------
/// Every face in a library, with its embedding, in a stable order.
///
/// The order matters: clustering is a function of position in this list, and a list that
/// came back differently ordered on each query would produce different groups from the same
/// data. Ordered by face id, which is assigned once and never changes.
pub fn faces_with_embeddings(
    conn: &Connection,
    library_id: i64,
    model: &str,
) -> Result<Vec<(i64, i64, Vec<f32>)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT face.id, f.photo_id, e.vector
           FROM face
           JOIN face_embedding e ON e.face_id = face.id
           JOIN file f ON f.id = face.file_id
           JOIN photo p ON p.id = f.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL AND e.model = ?2
          ORDER BY face.id",
    )?;
    let rows = stmt.query_map(params![library_id, model], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (face_id, photo_id, blob) = row?;
        out.push((face_id, photo_id, decode_vector(&blob)));
    }
    Ok(out)
}

/// Store an embedding.
pub fn upsert_embedding(
    conn: &Connection,
    face_id: i64,
    vector: &[f32],
    model: &str,
    now: i64,
) -> Result<(), CatalogError> {
    conn.execute(
        "INSERT INTO face_embedding (face_id, vector, model, computed_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (face_id) DO UPDATE SET
             vector = excluded.vector, model = excluded.model, computed_at = excluded.computed_at",
        params![face_id, encode_vector(vector), model, now],
    )?;
    Ok(())
}

/// The faces still needing an embedding from this model.
pub fn faces_needing_embeddings(
    conn: &Connection,
    library_id: i64,
    model: &str,
) -> Result<Vec<(i64, i64)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT face.id, f.photo_id
           FROM face
           JOIN file f ON f.id = face.file_id
           JOIN photo p ON p.id = f.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM face_embedding e WHERE e.face_id = face.id AND e.model = ?2
            )
          ORDER BY face.id",
    )?;
    let rows = stmt.query_map(params![library_id, model], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Replace the unconfirmed clusters of a library with a new partition.
///
/// **Confirmed groups are left alone.** A user who has merged two clusters and split a
/// third has changed the answer, and discarding that on the next pass would make the
/// feature unusable — the corrections would not survive a restart.
///
/// Faces belonging to a confirmed group are excluded from the new partition entirely, so a
/// re-cluster cannot pull them into a different suggestion behind the user's back.
pub fn replace_people(
    conn: &Connection,
    library_id: i64,
    clusters: &[Vec<i64>],
    now: i64,
) -> Result<usize, CatalogError> {
    let tx = conn.unchecked_transaction()?;

    tx.execute(
        "DELETE FROM person WHERE library_id = ?1 AND confirmed = 0",
        params![library_id],
    )?;

    let confirmed: std::collections::HashSet<i64> = {
        let mut stmt = tx.prepare(
            "SELECT pf.face_id FROM person_face pf
               JOIN person pe ON pe.id = pf.person_id
              WHERE pe.library_id = ?1 AND pe.confirmed = 1",
        )?;
        let rows = stmt.query_map(params![library_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<std::collections::HashSet<_>, _>>()?
    };

    let mut written = 0usize;
    for members in clusters {
        // A group of one is not a group, and a cluster that has been reduced to one member
        // by the confirmed set is not worth showing.
        if members.len() < 2 {
            continue;
        }
        tx.execute(
            "INSERT INTO person (library_id, name, confirmed, created_at, updated_at)
             VALUES (?1, NULL, 0, ?2, ?2)",
            params![library_id, now],
        )?;
        let person_id = tx.last_insert_rowid();

        for face_id in members {
            if confirmed.contains(face_id) {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO person_face (person_id, face_id) VALUES (?1, ?2)",
                params![person_id, face_id],
            )?;
        }
        written += 1;
    }

    tx.commit()?;
    Ok(written)
}

/// A person, with how many faces and photographs they appear in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonRow {
    pub id: i64,
    pub name: Option<String>,
    pub confirmed: bool,
    pub faces: usize,
    /// Distinct photographs, which is what a person actually appears in — a group of six
    /// faces from two photographs is two photographs, not six.
    pub photos: usize,
}

pub fn people(conn: &Connection, library_id: i64) -> Result<Vec<PersonRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT pe.id, pe.name, pe.confirmed,
                COUNT(pf.face_id),
                COUNT(DISTINCT f.photo_id)
           FROM person pe
           LEFT JOIN person_face pf ON pf.person_id = pe.id
           LEFT JOIN face ON face.id = pf.face_id
           LEFT JOIN file f ON f.id = face.file_id
          WHERE pe.library_id = ?1
          GROUP BY pe.id
          ORDER BY COUNT(DISTINCT f.photo_id) DESC, pe.id",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok(PersonRow {
            id: r.get(0)?,
            name: r.get(1)?,
            confirmed: r.get::<_, i64>(2)? != 0,
            faces: r.get::<_, i64>(3)? as usize,
            photos: r.get::<_, i64>(4)? as usize,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The faces in a group.
pub fn faces_for_person(conn: &Connection, person_id: i64) -> Result<Vec<i64>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT face_id FROM person_face WHERE person_id = ?1 ORDER BY face_id",
    )?;
    let rows = stmt.query_map(params![person_id], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The photographs a person appears in.
pub fn photos_for_person(conn: &Connection, person_id: i64) -> Result<Vec<i64>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.photo_id
           FROM person_face pf
           JOIN face ON face.id = pf.face_id
           JOIN file f ON f.id = face.file_id
          WHERE pf.person_id = ?1 AND f.photo_id IS NOT NULL
          ORDER BY f.photo_id",
    )?;
    let rows = stmt.query_map(params![person_id], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// 128 f32 as 512 little-endian bytes.
///
/// A blob rather than 128 columns: nothing queries an individual component, and the only
/// operation is "compare this whole vector to that one".
fn encode_vector(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn decode_vector(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

// ---------------------------------------------------------------------------
// Naming, merging and splitting people
// ---------------------------------------------------------------------------
/// Name a person, and mark the group confirmed.
///
/// **Naming confirms.** A group a human has put a name to is a decision, not a suggestion,
/// and the next clustering pass must leave it alone — which is what `confirmed` means and
/// why it is set here rather than by a separate button.
pub fn name_person(conn: &Connection, person_id: i64, name: Option<&str>, now: i64) -> Result<(), CatalogError> {
    // An empty name is not a name. Stored as NULL, because "" and "not named" are the same
    // thing to every reader and two representations of one state is how they drift.
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    conn.execute(
        "UPDATE person SET name = ?2, confirmed = 1, updated_at = ?3 WHERE id = ?1",
        params![person_id, name, now],
    )?;
    Ok(())
}

/// Move every face from one person into another, and remove the source.
///
/// Merging is how a user says "these are the same person". Both groups end up confirmed:
/// the destination because it now carries a decision, the faces because they were moved by
/// hand and a later pass must not pull them apart again.
pub fn merge_people(conn: &Connection, from_id: i64, into_id: i64, now: i64) -> Result<usize, CatalogError> {
    if from_id == into_id {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;

    let moved = tx.execute(
        "INSERT OR IGNORE INTO person_face (person_id, face_id)
         SELECT ?2, face_id FROM person_face WHERE person_id = ?1",
        params![from_id, into_id],
    )?;

    tx.execute("DELETE FROM person WHERE id = ?1", params![from_id])?;
    tx.execute(
        "UPDATE person SET confirmed = 1, updated_at = ?2 WHERE id = ?1",
        params![into_id, now],
    )?;

    tx.commit()?;
    Ok(moved)
}

/// Move faces out of a person into a new one.
///
/// Splitting is how a user says "these are two different people". The new group is created
/// confirmed for the same reason a merge is: the faces were separated by hand.
///
/// Refuses to empty the source. A "person" with no faces is not a group, and leaving one
/// behind would put an empty row in the list that cannot be selected or removed.
pub fn split_person(
    conn: &Connection,
    person_id: i64,
    face_ids: &[i64],
    now: i64,
) -> Result<Option<i64>, CatalogError> {
    let tx = conn.unchecked_transaction()?;

    let total: i64 = tx.query_row(
        "SELECT COUNT(*) FROM person_face WHERE person_id = ?1",
        params![person_id],
        |r| r.get(0),
    )?;
    if face_ids.is_empty() || face_ids.len() as i64 >= total {
        tx.commit()?;
        return Ok(None);
    }

    let library_id: i64 = tx.query_row(
        "SELECT library_id FROM person WHERE id = ?1",
        params![person_id],
        |r| r.get(0),
    )?;

    tx.execute(
        "INSERT INTO person (library_id, name, confirmed, created_at, updated_at)
         VALUES (?1, NULL, 1, ?2, ?2)",
        params![library_id, now],
    )?;
    let new_id = tx.last_insert_rowid();

    for face_id in face_ids {
        tx.execute(
            "INSERT OR IGNORE INTO person_face (person_id, face_id) VALUES (?1, ?2)",
            params![new_id, face_id],
        )?;
        tx.execute(
            "DELETE FROM person_face WHERE person_id = ?1 AND face_id = ?2",
            params![person_id, face_id],
        )?;
    }

    tx.execute(
        "UPDATE person SET confirmed = 1, updated_at = ?2 WHERE id = ?1",
        params![person_id, now],
    )?;

    tx.commit()?;
    Ok(Some(new_id))
}

/// Remove a person without touching the faces.
///
/// The faces stay in the catalog — they are still detections, still in their photographs.
/// Only the grouping is discarded, and the next clustering pass may group them differently.
pub fn delete_person(conn: &Connection, person_id: i64) -> Result<(), CatalogError> {
    conn.execute("DELETE FROM person WHERE id = ?1", params![person_id])?;
    Ok(())
}

/// A face that the clustering could not place confidently.
#[derive(Debug, Clone, PartialEq)]
pub struct AmbiguousFace {
    pub face_id: i64,
    pub photo_id: i64,
    /// The group it was put in, if any.
    pub person_id: Option<i64>,
    /// Similarity to the group it is in.
    pub own: f32,
    /// Similarity to the nearest group it is *not* in.
    pub other: f32,
}

/// Faces that sit between two groups.
///
/// # What "ambiguous" means here, precisely
///
/// A face whose similarity to the group it is in is barely higher than its similarity to
/// some other group. The margin is the whole signal: a face at 0.8 in its own group and 0.3
/// in the next is settled; one at 0.52 and 0.50 could go either way, and a person looking at
/// it can decide in a second what no threshold can.
///
/// This is what the review queue (#47) exists for. Without it, a face that landed on the
/// wrong side of a threshold is simply wrong, and nobody ever sees it.
pub fn ambiguous_faces(
    conn: &Connection,
    library_id: i64,
    model: &str,
    margin: f32,
    limit: usize,
) -> Result<Vec<AmbiguousFace>, CatalogError> {
    let faces = faces_with_embeddings(conn, library_id, model)?;
    if faces.len() < 2 {
        return Ok(Vec::new());
    }

    // Which group each face is in, and what its centroid is.
    let mut membership: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut members: std::collections::HashMap<i64, Vec<usize>> = std::collections::HashMap::new();
    for (i, (face_id, _, _)) in faces.iter().enumerate() {
        if let Some(pid) = person_for_face(conn, *face_id)? {
            membership.insert(*face_id, pid);
            members.entry(pid).or_default().push(i);
        }
    }

    let mut centroids: std::collections::HashMap<i64, Vec<f32>> = std::collections::HashMap::new();
    for (pid, idxs) in &members {
        centroids.insert(*pid, centroid(&idxs.iter().map(|i| faces[*i].2.clone()).collect::<Vec<_>>()));
    }

    let mut out = Vec::new();
    for (i, (face_id, photo_id, vector)) in faces.iter().enumerate() {
        let own_id = membership.get(face_id).copied();
        let own = own_id
            .and_then(|p| centroids.get(&p))
            .map(|c| cosine_f32(vector, c))
            .unwrap_or(0.0);

        // The best group this face is not in. A face in no group is ambiguous by definition
        // if it is close to any group at all.
        let (other, other_id) = centroids
            .iter()
            .filter(|(pid, _)| Some(**pid) != own_id)
            .map(|(pid, c)| (cosine_f32(vector, c), *pid))
            .fold((f32::NEG_INFINITY, None), |acc, (s, pid)| {
                if s > acc.0 { (s, Some(pid)) } else { acc }
            });

        let Some(other_id) = other_id else { continue };
        let _ = (i, other_id);

        // Close to another group, and not clearly settled in its own.
        if other > 0.0 && own - other < margin {
            out.push(AmbiguousFace {
                face_id: *face_id,
                photo_id: *photo_id,
                person_id: own_id,
                own,
                other,
            });
        }
    }

    // Worst margin first: the faces most likely to be wrong are the ones worth looking at.
    out.sort_by(|a, b| {
        (a.own - a.other)
            .partial_cmp(&(b.own - b.other))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.face_id.cmp(&b.face_id))
    });
    out.truncate(limit);
    Ok(out)
}

/// The person a face belongs to, if any.
pub fn person_for_face(conn: &Connection, face_id: i64) -> Result<Option<i64>, CatalogError> {
    let mut stmt = conn.prepare("SELECT person_id FROM person_face WHERE face_id = ?1 LIMIT 1")?;
    let mut rows = stmt.query(params![face_id])?;
    match rows.next()? {
        Some(r) => Ok(Some(r.get(0)?)),
        None => Ok(None),
    }
}

/// The average of a set of embeddings, renormalised.
fn centroid(vectors: &[Vec<f32>]) -> Vec<f32> {
    if vectors.is_empty() {
        return Vec::new();
    }
    let dim = vectors[0].len();
    let mut sum = vec![0f32; dim];
    for v in vectors {
        for (i, x) in v.iter().enumerate().take(dim) {
            sum[i] += x;
        }
    }
    let n = vectors.len() as f32;
    for x in sum.iter_mut() {
        *x /= n;
    }
    sum
}

/// Cosine similarity, over `f64`-free slices.
fn cosine_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let d = na.sqrt() * nb.sqrt();
    if d <= f32::EPSILON { 0.0 } else { dot / d }
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------
/// One tag, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct TagRow {
    pub name: String,
    pub confidence: f64,
    pub model: String,
}

/// Replace the tags a model produced for a photograph.
///
/// **Scoped to the model.** A second model's tags are kept alongside rather than replaced,
/// because they are a different opinion and the user chose to ask for both. Replacing them
/// would make "compare two models" impossible and would silently delete work.
pub fn replace_tags(
    conn: &Connection,
    photo_id: i64,
    tags: &[(String, f64)],
    model: &str,
    description: Option<&str>,
    now: i64,
) -> Result<(), CatalogError> {
    let tx = conn.unchecked_transaction()?;

    tx.execute("DELETE FROM tag WHERE photo_id = ?1 AND model = ?2", params![photo_id, model])?;

    for (name, confidence) in tags {
        // Normalised on write. "Beach", "beach" and "BEACH" are one tag, and a vocabulary
        // that treats them as three is one nobody can filter.
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT OR REPLACE INTO tag (photo_id, name, confidence, model, description, tagged_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![photo_id, name, confidence, model, description, now],
        )?;
    }

    if let Some(d) = description.filter(|d| !d.trim().is_empty()) {
        tx.execute(
            "INSERT OR REPLACE INTO photo_caption (photo_id, model, description, tagged_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![photo_id, model, d.trim(), now],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Every tag in a library, with how many photographs carry it.
///
/// Ranked by count: the vocabulary a library actually has is more useful than an
/// alphabetical list of everything, most of which occurs once.
pub fn tag_counts(
    conn: &Connection,
    library_id: i64,
    model: Option<&str>,
) -> Result<Vec<(String, usize)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT tag.name, COUNT(DISTINCT tag.photo_id)
           FROM tag
           JOIN photo p ON p.id = tag.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL
            AND (?2 IS NULL OR tag.model = ?2)
          GROUP BY tag.name
          ORDER BY COUNT(DISTINCT tag.photo_id) DESC, tag.name",
    )?;
    let rows = stmt.query_map(params![library_id, model], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The tags on one photograph.
pub fn tags_for_photo(conn: &Connection, photo_id: i64) -> Result<Vec<TagRow>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT name, confidence, model FROM tag
          WHERE photo_id = ?1
          ORDER BY confidence DESC, name",
    )?;
    let rows = stmt.query_map(params![photo_id], |r| {
        Ok(TagRow { name: r.get(0)?, confidence: r.get(1)?, model: r.get(2)? })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The caption a model wrote for a photograph.
pub fn caption_for_photo(
    conn: &Connection,
    photo_id: i64,
    model: &str,
) -> Result<Option<String>, CatalogError> {
    let mut stmt =
        conn.prepare("SELECT description FROM photo_caption WHERE photo_id = ?1 AND model = ?2")?;
    let mut rows = stmt.query(params![photo_id, model])?;
    match rows.next()? {
        Some(r) => Ok(Some(r.get(0)?)),
        None => Ok(None),
    }
}

/// Photographs carrying a tag.
pub fn photos_with_tag(
    conn: &Connection,
    library_id: i64,
    tag: &str,
    model: Option<&str>,
) -> Result<Vec<i64>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT tag.photo_id
           FROM tag
           JOIN photo p ON p.id = tag.photo_id
          WHERE p.library_id = ?1 AND p.trashed_at IS NULL AND tag.name = ?2
            AND (?3 IS NULL OR tag.model = ?3)
          ORDER BY tag.photo_id",
    )?;
    let rows = stmt.query_map(params![library_id, tag.trim().to_lowercase(), model], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Photographs that have no tag from this model yet.
///
/// The batch runner's work list. Excludes trashed photographs, because tagging something
/// the user has already rejected is wasted GPU time.
pub fn photos_needing_tags(
    conn: &Connection,
    library_id: i64,
    model: &str,
) -> Result<Vec<(i64, String)>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT p.id, f.path
           FROM photo p
           JOIN file f ON f.photo_id = p.id
          WHERE p.library_id = ?1
            AND p.trashed_at IS NULL
            AND f.role IN ('raw', 'raster')
            AND NOT EXISTS (
                SELECT 1 FROM photo_caption c WHERE c.photo_id = p.id AND c.model = ?2
            )
            AND f.id = (
                SELECT f2.id FROM file f2
                 WHERE f2.photo_id = p.id AND f2.role IN ('raw', 'raster')
                 ORDER BY CASE f2.role WHEN 'raw' THEN 0 ELSE 1 END, f2.id
                 LIMIT 1
            )
          ORDER BY p.id",
    )?;
    let rows = stmt.query_map(params![library_id, model], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
