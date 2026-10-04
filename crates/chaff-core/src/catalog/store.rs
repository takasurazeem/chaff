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

    stats.removed_photos = tx.execute(
        "DELETE FROM photo
          WHERE library_id = ?1
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
    let mut stmt = conn.prepare(
        "SELECT id, dir, stem, state, needs_review
           FROM photo WHERE library_id = ?1
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
