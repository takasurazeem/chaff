//! What the trash panel is given, against a real trash on disk.
//!
//! `trash.rs` has thorough tests of the trash itself. What they cannot cover is the **projection**
//! this layer performs: the manifest's moves turned into an operation with a file count, a byte
//! total and an `incomplete` flag. That arithmetic is what the panel shows, and a user decides to
//! purge on it.

use std::path::{Path, PathBuf};

struct Silent;
impl chaff_ffi::Progress for Silent {
    fn on_progress(&self, _d: u32, _t: u32, _s: String, _c: String) -> bool {
        true
    }
}

fn library() -> (tempfile::TempDir, std::sync::Arc<chaff_ffi::Engine>, i64, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("IMG_0001.JPG"), b"pretend jpeg bytes").unwrap();

    let engine =
        chaff_ffi::Engine::new(root.join("catalog.db").to_string_lossy().to_string()).unwrap();
    chaff_ffi::set_data_root(root.join("appdata").to_string_lossy().to_string());
    let report = engine
        .open_library(root.to_string_lossy().to_string(), Box::new(Silent))
        .unwrap();
    (dir, engine, report.library.id, root.to_string_lossy().to_string())
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}

#[test]
fn an_empty_trash_is_empty_rather_than_an_error() {
    let (_d, engine, _lib, root) = library();
    assert!(engine.trash(root).unwrap().is_empty());
}

#[test]
fn a_moved_operation_appears_with_its_count_and_size() {
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();
    let plan = engine.plan_delete(root.clone(), photos.iter().map(|p| p.id).collect()).unwrap();
    assert!(plan.refusals.is_empty(), "{plan:?}");
    let receipt = engine.commit_delete(root.clone()).unwrap();
    assert_eq!(receipt.moved, 1);

    let trash = engine.trash(root).unwrap();
    assert_eq!(trash.len(), 1, "{trash:?}");
    assert_eq!(trash[0].op_id, receipt.op_id);
    assert_eq!(trash[0].files, 1);
    assert!(trash[0].bytes > 0, "a zero byte total says the trash is empty while holding a file");
    assert!(!trash[0].incomplete);
}

#[test]
fn an_operation_missing_a_file_says_so() {
    // **Said rather than hidden.** A restore bringing back nine of twelve should not surprise
    // anyone, and the cause is usually outside this application.
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();
    assert!(engine.plan_delete(root.clone(), vec![photos[0].id]).unwrap().refusals.is_empty());
    engine.commit_delete(root.clone()).unwrap();

    let victim = walk(&Path::new(&root).join(".cull-trash"))
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "JPG"))
        .expect("the moved file is in the trash");
    std::fs::remove_file(&victim).unwrap();

    let trash = engine.trash(root).unwrap();
    assert!(trash[0].incomplete, "the panel must say a file is gone: {trash:?}");
    // The count stays the manifest's — "1 file" that restores nothing is more confusing than
    // "1 file, some are gone".
    assert_eq!(trash[0].files, 1);
}

#[test]
fn purging_reclaims_the_bytes() {
    // **The only irreversible thing in this application.** It has to actually free the disk, or a
    // user purging a large trash watches the space stay used.
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();
    assert!(engine.plan_delete(root.clone(), vec![photos[0].id]).unwrap().refusals.is_empty());
    let receipt = engine.commit_delete(root.clone()).unwrap();

    assert_eq!(engine.purge_trash(root.clone(), vec![receipt.op_id]).unwrap(), 1);
    assert!(engine.trash(root.clone()).unwrap().is_empty());
    let left = walk(&Path::new(&root).join(".cull-trash"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "JPG"))
        .count();
    assert_eq!(left, 0, "the bytes were actually reclaimed");
}

#[test]
fn restoring_puts_the_file_back_and_empties_the_listing() {
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();
    assert!(engine.plan_delete(root.clone(), vec![photos[0].id]).unwrap().refusals.is_empty());
    let receipt = engine.commit_delete(root.clone()).unwrap();

    assert_eq!(engine.restore_trash(root.clone(), receipt.op_id).unwrap(), 1);
    assert!(
        Path::new(&root).join("IMG_0001.JPG").exists(),
        "the photograph is back where it was"
    );
    // **And the catalog knows**, which is the half a file-level test would miss — the grid reads
    // the catalog, so a restore that moved the bytes and left the rows trashed would show nothing.
    let photos = engine.photos(lib).unwrap();
    assert_eq!(photos.len(), 1, "the restored photograph is visible again");
}
