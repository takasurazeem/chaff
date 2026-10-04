//! Reading and writing the catalog.
//!
//! Everything here takes `now` as a parameter rather than reading the clock. A store
//! that calls `SystemTime::now()` internally can only be tested by waiting, and a test
//! that waits is a test that flakes. The clock belongs to the caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

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
pub fn upsert_groups(
    conn: &mut Connection,
    library_id: i64,
    groups: &[PhotoGroup],
    meta: &HashMap<PathBuf, FileMeta>,
    now: i64,
) -> Result<IndexStats, CatalogError> {
    let tx = conn.transaction()?;
    let mut stats = IndexStats::default();

    for group in groups {
        let dir = group.key.dir.to_string_lossy().to_string();
        let stem = group.key.stem.clone();
        let needs_review = i64::from(group.needs_review());

        tx.execute(
            "INSERT INTO photo (library_id, dir, stem, state, needs_review)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (library_id, dir, stem) DO UPDATE SET
                 state        = excluded.state,
                 needs_review = excluded.needs_review",
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
                    now
                ],
            )?;
            stats.files += 1;
        }
    }

    stats.removed_files = tx.execute(
        "DELETE FROM file WHERE library_id = ?1 AND indexed_at <> ?2",
        params![library_id, now],
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

    conn.execute(
        "INSERT INTO decision (photo_id, rating, rejected, decided_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (photo_id) DO UPDATE SET
             rating     = excluded.rating,
             rejected   = excluded.rejected,
             decided_at = excluded.decided_at",
        params![photo_id, decision.rating.get() as i64, i64::from(decision.rejected), now],
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
