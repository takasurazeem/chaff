//! What the cache reports, and what trimming actually reclaims.
//!
//! A cache nobody can see is one nobody trusts. These are the two numbers a user acts on — how
//! much space it holds, and how much came back — and both are easy to get wrong in a way that
//! looks plausible: a size that counts directories, a trim that reports files it failed to
//! delete.

use std::path::Path;

/// A cache directory with `n` files of known size, oldest first.
fn seed(dir: &Path, n: usize, each: usize) {
    std::fs::create_dir_all(dir).unwrap();
    for i in 0..n {
        let p = dir.join(format!("thumb-{i}.bin"));
        std::fs::write(&p, vec![0u8; each]).unwrap();
        // Stagger the modification times so "most recently used" is a real order rather than a
        // tie the filesystem breaks arbitrarily.
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs((n - i) as u64);
        let _ = filetime_set(&p, when);
    }
}

/// Set a file's modification time, without a dependency.
fn filetime_set(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
    let secs = when
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    let times = [
        libc::timeval { tv_sec: secs, tv_usec: 0 },
        libc::timeval { tv_sec: secs, tv_usec: 0 },
    ];
    // SAFETY: `file` is open for writing and the array is two valid `timeval`s.
    let rc = unsafe { libc::futimes(file.as_raw_fd(), times.as_ptr()) };
    if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

use std::os::unix::io::AsRawFd;

/// Serialises the tests, because `set_data_root` is a **process-global**.
///
/// This is not a workaround for flakiness — it is the property of the thing under test. The
/// engine is told where its caches live **once, at launch**, through a `static`; two tests
/// setting it concurrently is two applications sharing a process, which cannot happen.
///
/// The first run of these tests failed for exactly this reason: one test's `set_data_root`
/// overwrote another's, and a cache that had three files reported zero. A test that hid the
/// global's real behaviour would be worse than one that acknowledges it.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point the engine at a fresh cache directory, and hold the lock for the test's duration.
fn isolated() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir, std::sync::Arc<chaff_ffi::Engine>) {
    // A poisoned lock means another test panicked while holding it. Taking it anyway is right:
    // the failure to report is the one in *this* test, not a cascading one about the lock.
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    chaff_ffi::set_data_root(dir.path().to_string_lossy().to_string());
    let engine =
        chaff_ffi::Engine::new(dir.path().join("c.db").to_string_lossy().to_string()).unwrap();
    (guard, dir, engine)
}

#[test]
fn an_empty_cache_reports_zero_rather_than_failing() {
    // The state on a fresh install, and a listing that errored on it would make the menu item
    // look broken.
    let (_guard, _dir, engine) = isolated();

    let info = engine.thumbnail_cache().unwrap();

    assert_eq!(info.files, 0, "{info:?}");
    assert_eq!(info.bytes, 0, "{info:?}");
    assert!(info.path.contains("thumbnails"), "the path must be the cache: {info:?}");
}

#[test]
fn the_size_counts_files_and_bytes_recursively() {
    // **Recursively**, because the cache shards by hash prefix — a count that only looked at the
    // top level would report zero while holding gigabytes.
    let (_guard, dir, engine) = isolated();

    let nested = dir.path().join("thumbnails").join("ab").join("cd");
    seed(&nested, 3, 100);

    let info = engine.thumbnail_cache().unwrap();
    assert_eq!(info.files, 3, "a nested cache must still be counted: {info:?}");
    assert_eq!(info.bytes, 300, "and its bytes: {info:?}");
}

#[test]
fn trimming_keeps_the_most_recent_and_reclaims_the_rest() {
    // **Sorted by modification time**, so what survives is what was looked at most recently —
    // which is the whole point of a cache. Trimming by name or by inode would keep an arbitrary
    // subset and evict the tile the user is about to scroll back to.
    let (_guard, dir, engine) = isolated();

    let cache = dir.path().join("thumbnails");
    seed(&cache, 10, 50);
    assert_eq!(engine.thumbnail_cache().unwrap().files, 10);

    let removed = engine.trim_thumbnail_cache(4).unwrap();
    assert_eq!(removed, 6, "ten files, keep four");

    let info = engine.thumbnail_cache().unwrap();
    assert_eq!(info.files, 4, "the survivors: {info:?}");
    assert_eq!(info.bytes, 200, "and their bytes: {info:?}");

    // The newest four are the ones left.
    for i in 6..10 {
        assert!(
            cache.join(format!("thumb-{i}.bin")).exists(),
            "thumb-{i} was among the most recent and must have survived"
        );
    }
    assert!(!cache.join("thumb-0.bin").exists(), "the oldest went first");
}

#[test]
fn trimming_below_the_keep_count_removes_nothing() {
    // A no-op has to be a no-op, and report zero — "reclaimed 0 files" for a trim that deleted
    // something would be worse than the deletion.
    let (_guard, dir, engine) = isolated();

    seed(&dir.path().join("thumbnails"), 3, 10);
    assert_eq!(engine.trim_thumbnail_cache(100).unwrap(), 0);
    assert_eq!(engine.thumbnail_cache().unwrap().files, 3);
}

#[test]
fn trimming_an_empty_cache_is_not_an_error() {
    let (_guard, _dir, engine) = isolated();
    assert_eq!(engine.trim_thumbnail_cache(10).unwrap(), 0);
}
