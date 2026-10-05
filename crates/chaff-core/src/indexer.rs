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
/// Directories the walk never descends into.
///
/// `.cull-trash` is ours. `.dtrash` is darktable's, and it holds photographs the user has
/// already deleted in that application — indexing them would resurrect them as live
/// pictures, and worse, `Reject` them again in a culling pass over files that are already
/// on their way out.
///
/// `.git` because a repository is not a photo library. `.stfolder` and `.stversions` are
/// Syncthing's, which would otherwise index every conflict copy as a photograph.
/// Directories the indexer never descends into.
///
/// Public because the file watcher (#6) must ignore exactly the same set. Two lists would
/// drift, and the one that drifted would be the watcher's — re-indexing `.cull-trash`, which
/// is the opposite of what the user asked for.
pub const EXCLUDED_DIRS: &[&str] = &[
    TRASH_DIR_NAME,
    ".dtrash",
    ".git",
    ".stfolder",
    ".stversions",
    ".thumbnails",
];

/// How many files to walk between progress reports.
const PROGRESS_INTERVAL: usize = 64;

/// Is this a macOS AppleDouble stub rather than a real file?
///
/// When macOS writes to a filesystem without native extended-attribute support — a FAT
/// card, an SMB share, an exFAT drive, anything a download lands on — it puts the real
/// file's metadata in a sibling named `._` plus the original name.
///
/// **These are not duplicates and not images.** They are a few kilobytes of resource-fork
/// header, and because they keep the original extension they sail straight through
/// extension-based classification: `._IMG_0537.CR3` looks exactly like a raw file to
/// `classify`. A real library had nine of them, each becoming a photograph with no
/// readable pixels, which is what "some CR3 files did not load" turned out to mean.
///
/// They are skipped at the filesystem level, like the trash directories, rather than
/// flagged in pairing. There is nothing to pair, nothing to score and nothing to show —
/// a placeholder tile for a 4 KB metadata stub is worse than no tile.
fn is_appledouble(name: &str) -> bool {
    name.starts_with("._")
}

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
    /// Files of zero bytes, by path.
    ///
    /// A copy that never finished, a download that failed, a truncated transfer. They are not
    /// photographs and must not become tiles — but they are also **the user's files**, so they
    /// are named rather than deleted or silently ignored.
    pub empty: Vec<PathBuf>,
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
    /// Files of zero bytes, by path.
    ///
    /// A copy that never finished, a download that failed, a truncated transfer. They are not
    /// photographs and must not become tiles — but they are also **the user's files**, so they
    /// are named rather than deleted or silently ignored.
    pub empty: Vec<PathBuf>,
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
    scan_with_progress(root, &mut |_| {})
}

/// The same walk, reporting a running count.
///
/// Reported every [`PROGRESS_INTERVAL`] files rather than on every one: an event per file
/// over a hundred thousand files is more work than the walk itself, and a progress bar
/// that updates a hundred thousand times is one the eye cannot read anyway.
pub fn scan_with_progress(root: &Path, on_files: &mut dyn FnMut(usize)) -> ScanReport {
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

        // Before classification, because an AppleDouble stub carries the original
        // extension and would otherwise classify as a photograph.
        if entry
            .file_name()
            .to_str()
            .map(is_appledouble)
            .unwrap_or(false)
        {
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
            Ok(md) => {
                // **A zero-byte file is not a photograph.**
                //
                // Found in a real library: eight `IMG_XXXX 2.CR3` files of exactly 0 bytes —
                // the name macOS gives a copy, where the copy never finished. Each became a
                // tile in the grid: a photograph that cannot be opened, cannot be scored, and
                // cannot be told apart from a real frame without selecting it.
                //
                // Reported by name rather than silently dropped, because the user has to
                // delete them and cannot act on a count.
                if md.len() == 0 {
                    report.empty.push(path);
                    continue;
                }
                report.files.push(ScannedFile {
                    path,
                    kind,
                    meta: store::FileMeta::from_metadata(&md),
                })
            }
            Err(err) => report.unreadable.push((path, err.to_string())),
        }

        if report.files.len() % PROGRESS_INTERVAL == 0 {
            on_files(report.files.len());
        }
    }
    on_files(report.files.len());

    // Deterministic order regardless of what order the filesystem returned entries in.
    //
    // **Both lists.** The first version sorted only `files`, so the empty-file report came back
    // in whatever order the filesystem happened to give — and a report that reshuffles between
    // runs is one a user cannot diff against the last one.
    report.files.sort_by(|a, b| a.path.cmp(&b.path));
    report.empty.sort();
    report.unreadable.sort_by(|a, b| a.0.cmp(&b.0));
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
    index_with_progress(conn, root, now, &mut |_| {})
}

/// The same, reporting how many files have been seen so far.
///
/// A running count rather than a fraction: the size of a tree is not known until the walk
/// finishes, and a determinate bar over an unknown total is a bar that lies.
pub fn index_with_progress(
    conn: &mut rusqlite::Connection,
    root: &Path,
    now: i64,
    on_files: &mut dyn FnMut(usize),
) -> Result<IndexOutcome, IndexError> {
    let report = scan_with_progress(root, on_files);

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
        empty: report.empty,
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
    fn a_zero_byte_file_is_not_a_photograph() {
        // **Found in a real library.** Eight `IMG_XXXX 2.CR3` files of exactly 0 bytes — the
        // name macOS gives a copy, where the copy never finished. Each became a tile: a
        // photograph that cannot be opened, cannot be scored, and cannot be told apart from a
        // real frame without selecting it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("IMG_0001.CR3"), b"real bytes").unwrap();
        std::fs::write(dir.path().join("IMG_0001 2.CR3"), b"").unwrap();

        let report = scan(dir.path());

        assert_eq!(report.files.len(), 1, "only the real file is a photograph");
        assert!(report.files[0].path.ends_with("IMG_0001.CR3"));
        assert_eq!(report.empty.len(), 1, "the empty one is reported, not silently dropped");
        assert!(report.empty[0].ends_with("IMG_0001 2.CR3"), "and named, so it can be deleted");
    }

    #[test]
    fn an_empty_file_is_reported_by_path_because_the_user_has_to_delete_it() {
        // A count is not actionable. The user needs to know *which* files to remove, and this
        // is their library — the application names them and touches nothing.
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.CR3", "b.JPG", "c.NEF"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let report = scan(dir.path());
        assert!(report.files.is_empty());
        assert_eq!(report.empty.len(), 3);
        // Sorted, so a report is stable between runs.
        let names: Vec<String> = report
            .empty
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.CR3", "b.JPG", "c.NEF"]);
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

    #[test]
    fn darktables_trash_is_never_walked() {
        // `.dtrash` holds photographs the user already deleted in darktable. Indexing them
        // resurrects them as live pictures — and a culling pass would then Reject files
        // that are already on their way out.
        assert!(EXCLUDED_DIRS.contains(&".dtrash"));
        assert!(EXCLUDED_DIRS.contains(&TRASH_DIR_NAME));
        for name in [".dtrash", ".cull-trash", ".git", ".stfolder", ".stversions"] {
            assert!(
                EXCLUDED_DIRS.contains(&name),
                "{name} must not be walked into"
            );
        }
    }

#[cfg(test)]
mod appledouble_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn appledouble_stubs_are_recognised() {
        assert!(is_appledouble("._IMG_0537.CR3"));
        assert!(is_appledouble("._IMG_0616 2.CR3"));
        assert!(is_appledouble("._photo.jpg"));
        // A real file that merely starts with a dot is not a stub.
        assert!(!is_appledouble(".DS_Store"));
        assert!(!is_appledouble("IMG_0537.CR3"));
        assert!(!is_appledouble("_IMG_0537.CR3"));
    }

    #[test]
    fn a_scan_skips_appledouble_stubs_but_keeps_the_real_files() {
        // The bug this fixes: `._IMG.CR3` keeps the `.CR3` extension, so extension-based
        // classification called it a raw file and it became a photograph with no pixels.
        // A real library had nine of them, reported by the user as "some CR3 files did not
        // load" — which was true, and was not about CR3 at all.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("shoot")).unwrap();

        fs::write(root.join("shoot/IMG_0001.CR3"), b"pretend raw").unwrap();
        fs::write(root.join("shoot/IMG_0001.JPG"), b"pretend jpeg").unwrap();
        // The stubs macOS leaves beside them on a non-native filesystem.
        fs::write(root.join("shoot/._IMG_0001.CR3"), [0u8; 4096]).unwrap();
        fs::write(root.join("shoot/._IMG_0001.JPG"), [0u8; 4096]).unwrap();

        let report = scan(root);
        let names: Vec<String> = report
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert_eq!(
            names,
            vec!["IMG_0001.CR3".to_string(), "IMG_0001.JPG".to_string()],
            "the stubs must be gone and the real files untouched"
        );
    }

    #[test]
    fn a_stub_alone_in_a_folder_yields_nothing() {
        // A folder holding only stubs is a folder holding no photographs, and must not
        // produce a placeholder tile.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("._orphan.CR3"), [0u8; 4096]).unwrap();
        assert!(scan(dir.path()).files.is_empty());
    }
}
