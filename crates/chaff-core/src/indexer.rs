//! Walking a library folder and recording what is there.
//!
//! # Three properties this module must never violate
//!
//! 1. **It never modifies the library.** Indexing opens files for metadata only. The
//!    only writes go to the catalog, which lives in app data.
//! 2. **It never follows a symlink.** `follow_links(false)` for two reasons: a symlink
//!    can point outside the library root, and a symlink loop makes the walk infinite. A
//!    culling tool that wanders out of the folder the user chose is a culling tool that
//!    can delete something the user never offered it.
//! 3. **It never enters the trash folder.** `.cull-trash` holds files the user has
//!    already rejected. Re-indexing them would resurrect rejected photographs into the
//!    grid and, worse, put them back in the catalog where a second delete could act on
//!    them again.
//!
//! Everything else here is ordinary: walk, classify, resolve, store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;
use walkdir::WalkDir;

use crate::catalog::{store, CatalogError};
use crate::exif::{self, ExifRead};
use crate::ext::{classify, FileKind};
use crate::pair::resolve;

/// The trash folder of ADR-0004. Indexing must skip it.
pub const TRASH_DIR_NAME: &str = ".cull-trash";

/// Directories the walk refuses to descend into.
///
/// Only two, and both for concrete reasons. `.cull-trash` is ours and holds rejected
/// files. `.git` never holds photographs and can contain tens of thousands of tiny
/// objects that would dominate a scan. Other dot-directories are left alone, because
/// guessing which ones hold photographs is not this module's business.
const EXCLUDED_DIRS: &[&str] = &[TRASH_DIR_NAME, ".git"];

#[derive(Debug, Error)]
pub enum IndexError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
}

/// What a walk found.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub files: Vec<ScannedFile>,
    /// Paths that could not be read, with the reason. Never fatal: one unreadable file
    /// must not abandon an index of ten thousand others.
    pub unreadable: Vec<(PathBuf, String)>,
}

impl ScanReport {
    pub fn count_of(&self, kind: FileKind) -> usize {
        self.files.iter().filter(|f| f.kind == kind).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub meta: store::FileMeta,
}

/// What an EXIF pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExifStats {
    /// Files actually opened and parsed this pass. Zero on a re-index of an unchanged
    /// library, which is the point of the mtime guard.
    pub examined: usize,
    pub parsed: usize,
    pub absent: usize,
    pub unsupported: usize,
}

/// The result of an indexing pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOutcome {
    pub library_id: i64,
    pub stats: store::IndexStats,
    pub exif: ExifStats,
    pub scanned_files: usize,
    pub unreadable: Vec<(PathBuf, String)>,
}

impl IndexOutcome {
    pub fn is_clean(&self) -> bool {
        self.unreadable.is_empty()
    }
}

fn is_excluded_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    match entry.file_name().to_str() {
        Some(name) => !EXCLUDED_DIRS.contains(&name),
        None => true, // non-UTF-8 directory name: not excluded
    }
}

/// Walk `root` and report every file the engine understands.
///
/// Read-only. Symlinks are not followed. The trash folder is skipped. Unreadable entries
/// are collected and returned rather than aborting the walk.
pub fn scan(root: &Path) -> ScanReport {
    let mut report = ScanReport::default();

    let walker = WalkDir::new(root).follow_links(false).into_iter();
    for entry in walker.filter_entry(is_excluded_dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                // `walkdir` reports a failure to descend as an error carrying the path.
                let path = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf());
                report.unreadable.push((path, err.to_string()));
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path().to_path_buf();
        let kind = classify(&path);
        if kind == FileKind::Other {
            // Not an image, not a sidecar, not a video. It must never enter the catalog,
            // because everything in the catalog is something a later operation may act on.
            continue;
        }

        match entry.metadata() {
            Ok(md) => report.files.push(ScannedFile {
                path,
                kind,
                meta: store::FileMeta::from_metadata(&md),
            }),
            Err(err) => report.unreadable.push((path, err.to_string())),
        }
    }

    // Deterministic order regardless of what order the filesystem returned entries in.
    report.files.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

/// Index `root` into the catalog.
///
/// Idempotent and incremental: re-running reconciles the catalog against what is now on
/// disk, sweeping files that have disappeared and preserving cached hashes for files
/// that have not changed.
pub fn index(
    conn: &mut rusqlite::Connection,
    root: &Path,
    now: i64,
) -> Result<IndexOutcome, IndexError> {
    let report = scan(root);

    let meta: HashMap<PathBuf, store::FileMeta> =
        report.files.iter().map(|f| (f.path.clone(), f.meta)).collect();
    let paths: Vec<&Path> = report.files.iter().map(|f| f.path.as_path()).collect();
    let groups = resolve(paths);

    let library_id = store::upsert_library(conn, root, now)?;
    let stats = store::upsert_groups(conn, library_id, &groups, &meta, now)?;

    let mut unreadable = report.unreadable;
    let exif_stats = extract_exif(conn, library_id, now, &mut unreadable)?;

    Ok(IndexOutcome {
        library_id,
        stats,
        exif: exif_stats,
        scanned_files: report.files.len(),
        unreadable,
    })
}

/// Read EXIF for files that have never been examined, or whose bytes have changed.
///
/// ## Why an I/O failure deliberately does not write a row
///
/// "No EXIF" and "could not be read" look the same from the outside and must be handled
/// oppositely:
///
/// * A file that was read and holds no metadata gets a row. It will never be examined
///   again, which matters because metadata-free files are common — stripped exports,
///   screenshots, scans — and re-reading all of them on every index is pure waste.
/// * A file that could not be *opened* gets no row, so the next pass retries it. Writing
///   an empty row would record a transient failure as a permanent fact, and a file on a
///   drive that was asleep would be metadata-less forever.
pub fn extract_exif(
    conn: &mut rusqlite::Connection,
    library_id: i64,
    now: i64,
    unreadable: &mut Vec<(PathBuf, String)>,
) -> Result<ExifStats, IndexError> {
    let pending = store::files_needing_exif(conn, library_id)?;
    let mut stats = ExifStats::default();

    for (file_id, path, mtime_ns) in pending {
        stats.examined += 1;
        match exif::read(&path) {
            Ok(ExifRead::Parsed(data)) => {
                stats.parsed += 1;
                store::upsert_exif(conn, file_id, mtime_ns, Some(&data), now)?;
            }
            Ok(ExifRead::Absent) => {
                stats.absent += 1;
                store::upsert_exif(conn, file_id, mtime_ns, None, now)?;
            }
            Ok(ExifRead::Unsupported) => {
                stats.unsupported += 1;
                store::upsert_exif(conn, file_id, mtime_ns, None, now)?;
            }
            Err(err) => {
                // No row: retry next pass.
                unreadable.push((path, err.to_string()));
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::store::{files_by_role, photos, photos_needing_review};
    use std::fs;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn scan_classifies_by_extension_and_ignores_everything_else() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));
        touch(&root.join("IMG_0001.JPG"));
        touch(&root.join("IMG_0001.XMP"));
        touch(&root.join("clip.MP4"));
        touch(&root.join("notes.txt"));
        touch(&root.join(".DS_Store"));

        let report = scan(root);
        assert_eq!(report.count_of(FileKind::Raw), 1);
        assert_eq!(report.count_of(FileKind::Raster), 1);
        assert_eq!(report.count_of(FileKind::Sidecar), 1);
        assert_eq!(report.count_of(FileKind::Video), 1);
        assert_eq!(report.files.len(), 4, "non-images must not be reported at all");
    }

    #[test]
    fn scan_recurses_into_subdirectories() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("shoot_a/IMG_0001.CR3"));
        touch(&root.join("shoot_b/nested/deeper/IMG_0002.CR3"));

        let report = scan(root);
        assert_eq!(report.files.len(), 2);
    }

    #[test]
    fn scan_never_enters_the_trash_folder() {
        // Safety-critical. Files in `.cull-trash` have already been rejected by the
        // user. Re-indexing them would resurrect them into the grid and put them back
        // where a second delete could act on them.
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));
        touch(&root.join(TRASH_DIR_NAME).join("2026-10-04/IMG_9999.CR3"));
        touch(&root.join(TRASH_DIR_NAME).join("2026-10-04/nested/IMG_8888.JPG"));

        let report = scan(root);
        assert_eq!(report.files.len(), 1, "only the live file should be found");
        assert!(
            report.files.iter().all(|f| !f.path.to_string_lossy().contains(TRASH_DIR_NAME)),
            "nothing under the trash folder may be indexed: {:?}",
            report.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_symlinks() {
        // Safety-critical. A symlink can point anywhere; a culling tool that follows one
        // is a culling tool that operates outside the folder the user chose.
        let outside = tempdir().unwrap();
        touch(&outside.path().join("SECRET.CR3"));

        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));
        std::os::unix::fs::symlink(outside.path(), root.join("link_to_outside")).unwrap();

        let report = scan(root);
        assert_eq!(
            report.files.len(),
            1,
            "a symlinked directory must not be descended into: {:?}",
            report.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_a_symlink_loop() {
        // The other half of the same hazard: a loop makes the walk infinite.
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("sub/IMG_0001.CR3"));
        std::os::unix::fs::symlink(root, root.join("sub/loop")).unwrap();

        let report = scan(root);
        assert_eq!(report.files.len(), 1, "the walk must terminate and index once");
    }

    #[test]
    fn indexing_a_tree_produces_the_expected_photographs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("shoot/IMG_0001.CR3"));
        touch(&root.join("shoot/IMG_0001.JPG"));
        touch(&root.join("shoot/IMG_0002.NEF"));
        touch(&root.join("shoot/IMG_0002.JPG"));
        touch(&root.join("shoot/IMG_0003.CR3")); // orphan raw

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let outcome = index(&mut conn, root, 100).unwrap();

        assert!(outcome.is_clean());
        assert_eq!(outcome.scanned_files, 5);
        assert_eq!(outcome.stats.photos, 3);
        assert_eq!(outcome.stats.pairs, 2);

        let all = photos(&conn, outcome.library_id).unwrap();
        assert_eq!(all.iter().filter(|p| p.state == "pair").count(), 2);
        assert_eq!(all.iter().filter(|p| p.state == "raw_only").count(), 1);
    }

    #[test]
    fn re_indexing_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));
        touch(&root.join("IMG_0001.JPG"));

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let first = index(&mut conn, root, 100).unwrap();
        let second = index(&mut conn, root, 200).unwrap();

        assert_eq!(first.library_id, second.library_id, "a re-index is the same library");
        assert_eq!(second.stats.removed_files, 0);
        assert_eq!(second.stats.removed_photos, 0);
        assert_eq!(photos(&conn, first.library_id).unwrap().len(), 1);
    }

    #[test]
    fn a_file_deleted_from_disk_is_swept_on_the_next_index() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));
        let jpg = root.join("IMG_0001.JPG");
        touch(&jpg);

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let first = index(&mut conn, root, 100).unwrap();
        assert_eq!(first.stats.pairs, 1);

        fs::remove_file(&jpg).unwrap();
        let second = index(&mut conn, root, 200).unwrap();

        assert_eq!(second.stats.removed_files, 1);
        assert_eq!(files_by_role(&conn, second.library_id, "raster").unwrap().len(), 0);
        let all = photos(&conn, second.library_id).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, "raw_only", "the pair became an orphan raw");
    }

    #[test]
    fn an_ambiguous_directory_is_flagged_for_review_not_resolved() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));
        touch(&root.join("IMG_0001.NEF"));
        touch(&root.join("IMG_0001.JPG"));

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let outcome = index(&mut conn, root, 100).unwrap();

        let review = photos_needing_review(&conn, outcome.library_id).unwrap();
        assert_eq!(review.len(), 1);
        assert_eq!(review[0].state, "ambiguous");
    }

    #[test]
    fn indexing_does_not_modify_the_library() {
        // The hard boundary, asserted rather than assumed. Compare a full recursive
        // snapshot of the tree before and after.
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("shoot/IMG_0001.CR3"));
        touch(&root.join("shoot/IMG_0001.JPG"));
        touch(&root.join("shoot/notes.txt"));

        let snapshot = |p: &Path| -> Vec<(PathBuf, u64, std::time::SystemTime)> {
            let mut v: Vec<_> = WalkDir::new(p)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
                .map(|e| {
                    let md = e.metadata().unwrap();
                    (e.path().to_path_buf(), md.len(), md.modified().unwrap())
                })
                .collect();
            v.sort();
            v
        };

        let before = snapshot(root);
        let mut conn = crate::catalog::open_in_memory().unwrap();
        index(&mut conn, root, 100).unwrap();
        let after = snapshot(root);

        assert_eq!(before, after, "indexing must not add, remove or touch any file");
    }

    #[test]
    fn the_synthetic_pairing_tree_indexes_to_its_declared_groups() {
        // A real directory tree, generated with known contents. This is the integration
        // check that the walker and the resolver agree on actual files rather than on
        // fabricated paths.
        let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/pairtree");
        if !tree.is_dir() {
            eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
            return;
        }

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let outcome = index(&mut conn, &tree, 100).unwrap();

        assert!(outcome.is_clean(), "unreadable: {:?}", outcome.unreadable);

        let all = photos(&conn, outcome.library_id).unwrap();
        // shoot_a holds pairs, orphans, an ambiguity and a suspected duplicate import;
        // shoot_b holds a separate pair, a same-stem-different-directory case, and the
        // NFC/NFD Unicode pair.
        assert!(all.len() >= 8, "expected the tree's photographs, got {}", all.len());
        assert!(
            all.iter().any(|p| p.state == "ambiguous"),
            "the tree contains an ambiguous stem on purpose"
        );
        assert!(
            all.iter().any(|p| p.needs_review),
            "the tree contains a suspected duplicate import on purpose"
        );
    }

    #[test]
    fn unreadable_entries_are_reported_without_aborting_the_walk() {
        // A missing directory is the easiest reproducible failure. The point is that the
        // walk continues and reports, rather than returning an error and indexing nothing.
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("IMG_0001.CR3"));

        let report = scan(&root.join("does-not-exist"));
        assert!(report.files.is_empty());
        // `walkdir` reports the missing root as an error entry rather than panicking.
        assert!(
            !report.unreadable.is_empty(),
            "a missing root must be reported, not silently treated as an empty folder"
        );
    }

    #[test]
    fn exif_is_extracted_and_stored_for_generated_fixtures() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images/sharp_a.jpg");
        if !src.is_file() {
            eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
            return;
        }

        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::copy(&src, root.join("IMG_0001.JPG")).unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let outcome = index(&mut conn, root, 100).unwrap();

        assert_eq!(outcome.exif.examined, 1);
        assert_eq!(outcome.exif.parsed, 1, "the fixture carries full EXIF");

        let files = files_by_role(&conn, outcome.library_id, "raster").unwrap();
        let data = store::exif_for_file(&conn, files[0].id).unwrap().expect("exif row");
        assert_eq!(data.make.as_deref(), Some("Chaff"));
        assert_eq!(data.iso, Some(400));
        assert!(data.captured_at.is_some(), "burst grouping needs the capture time");
        assert!(data.exposure_signature().is_some(), "bracket detection needs ISO+shutter");
    }

    #[test]
    fn a_re_index_does_no_exif_io_at_all() {
        // The mtime guard. Without it, every index re-parses metadata for the whole
        // library, which on a 50k-photo library is minutes of pointless file I/O.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images/sharp_a.jpg");
        if !src.is_file() {
            return;
        }
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::copy(&src, root.join("IMG_0001.JPG")).unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let first = index(&mut conn, root, 100).unwrap();
        assert_eq!(first.exif.examined, 1);

        let second = index(&mut conn, root, 200).unwrap();
        assert_eq!(second.exif.examined, 0, "nothing changed, so nothing should be re-read");
    }

    #[test]
    fn a_modified_file_has_its_exif_re_read() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images/sharp_a.jpg");
        let other = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images/bokeh_portrait.jpg");
        if !src.is_file() || !other.is_file() {
            return;
        }

        let dir = tempdir().unwrap();
        let root = dir.path();
        let target = root.join("IMG_0001.JPG");
        fs::copy(&src, &target).unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        index(&mut conn, root, 100).unwrap();

        // Replace the file's contents with a different photograph's bytes.
        fs::copy(&other, &target).unwrap();
        let second = index(&mut conn, root, 200).unwrap();
        assert_eq!(
            second.exif.examined, 1,
            "a replaced file must have its metadata re-read, not inherited"
        );
    }

    #[test]
    fn a_file_with_no_exif_is_examined_once_and_then_left_alone() {
        // "Read, nothing there" is recorded so metadata-free files are not re-read
        // forever. Those files are common: stripped exports, screenshots, scans.
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("IMG_0001.JPG"), b"not really a jpeg").unwrap();

        let mut conn = crate::catalog::open_in_memory().unwrap();
        let first = index(&mut conn, root, 100).unwrap();
        assert_eq!(first.exif.examined, 1);
        assert_eq!(first.exif.parsed, 0);

        let second = index(&mut conn, root, 200).unwrap();
        assert_eq!(second.exif.examined, 0, "an examined-and-empty file must not be retried");
    }

    #[test]
    fn scan_order_is_deterministic_across_runs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for i in 0..10 {
            touch(&root.join(format!("sub{}/IMG_{i:04}.CR3", i % 3)));
        }
        let a = scan(root);
        let b = scan(root);
        assert_eq!(
            a.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
            b.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }
}
