//! Thumbnail generation and caching against real files.
//!
//! The unit tests in `src/thumb.rs` use synthesised JPEGs, which is right for testing the
//! cache. This file runs the pipeline over the real corpus — real JPEGs from a real CDN,
//! real camera raw files, real embedded previews — because the decode path is where the
//! interesting failures live.
//!
//! Everything here reads `fixtures/`, which is downloaded or generated. No personal
//! photograph is touched.

use std::path::{Path, PathBuf};
use std::time::Instant;

use chaff_core::thumb::{
    generate_and_store, key_for_file, ThumbError, ThumbSize, ThumbnailCache, DEFAULT_CAP_BYTES,
};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

fn jpegs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(corpus().join("jpeg"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn raws() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(corpus().join("raw"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn cache_in(dir: &Path) -> ThumbnailCache {
    ThumbnailCache::open(dir.join("thumbs"), DEFAULT_CAP_BYTES).expect("open cache")
}

#[test]
#[ignore]
fn real_corpus_thumbnail_report() {
    if !corpus().is_dir() {
        eprintln!("SKIP: run tools/fixtures/fetch_corpus.py first");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path());

    println!("\n{:<30} {:>10} {:>12} {:>10}", "group", "files", "thumbnails", "time");
    println!("{}", "-".repeat(66));

    for (label, files) in [("jpeg", jpegs()), ("raw", raws())] {
        let t0 = Instant::now();
        let mut ok = 0;
        let mut failed = Vec::new();
        for f in &files {
            match generate_and_store(&cache, f, ThumbSize::Grid) {
                Ok(_) => ok += 1,
                Err(e) => failed.push(format!("{}: {e}", f.file_name().unwrap().to_string_lossy())),
            }
        }
        let elapsed = t0.elapsed();
        println!(
            "{label:<30} {:>10} {:>12} {:>9.2}s",
            files.len(),
            ok,
            elapsed.as_secs_f64()
        );
        for f in &failed {
            println!("     no thumbnail: {f}");
        }
        if ok > 0 {
            println!(
                "     {:.0} ms per thumbnail",
                elapsed.as_millis() as f64 / ok as f64
            );
        }
    }

    let stats = cache.stats().unwrap();
    println!(
        "\ncache: {} files, {} KB, cap {} MB",
        stats.files,
        stats.bytes / 1024,
        stats.cap_bytes / (1024 * 1024)
    );
    println!();
}

#[test]
fn every_real_jpeg_produces_a_grid_thumbnail() {
    if !corpus().is_dir() {
        eprintln!("SKIP: run tools/fixtures/fetch_corpus.py first");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path());

    let files = jpegs();
    assert!(!files.is_empty(), "the corpus holds no JPEGs");

    for f in &files {
        let path = generate_and_store(&cache, f, ThumbSize::Grid)
            .unwrap_or_else(|e| panic!("{}: {e}", f.display()));

        let bytes = std::fs::read(&path).expect("read thumbnail");
        let decoded = image::load_from_memory(&bytes)
            .unwrap_or_else(|e| panic!("{}: thumbnail did not decode: {e}", f.display()));

        assert!(
            decoded.width().max(decoded.height()) <= ThumbSize::Grid.pixels(),
            "{}: thumbnail is {}x{}, over the grid size",
            f.display(),
            decoded.width(),
            decoded.height()
        );
        assert!(
            (bytes.len() as u64) < std::fs::metadata(f).unwrap().len(),
            "{}: the thumbnail is not smaller than the source",
            f.display()
        );
    }
}

#[test]
fn real_raw_files_with_usable_previews_produce_thumbnails() {
    if !corpus().is_dir() {
        eprintln!("SKIP: run tools/fixtures/fetch_corpus.py first");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path());

    let mut ok = 0;
    let mut no_source = 0;
    for f in raws() {
        match generate_and_store(&cache, &f, ThumbSize::Grid) {
            Ok(path) => {
                ok += 1;
                let decoded = image::load_from_memory(&std::fs::read(&path).unwrap())
                    .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
                assert!(decoded.width().max(decoded.height()) <= 256);
            }
            // A raw file this build cannot read without LibRaw. Expected for some
            // formats, and issue #8 rather than a failure here.
            Err(ThumbError::NoSource { .. }) => no_source += 1,
            Err(e) => panic!("{}: {e}", f.display()),
        }
    }

    eprintln!("raw thumbnails: {ok} produced, {no_source} need a real raw decoder");
    assert!(
        ok >= 5,
        "only {ok} of {} raw files produced a thumbnail",
        raws().len()
    );
}

#[test]
fn a_renamed_photograph_reuses_its_thumbnail() {
    // The property content addressing exists for. A photographer reorganising a library
    // must not pay to regenerate 40,000 thumbnails.
    if !corpus().is_dir() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path());

    let Some(source) = jpegs().into_iter().next() else { return };

    let a = dir.path().join("original.jpg");
    let b = dir.path().join("moved/renamed.jpg");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    std::fs::copy(&source, &a).unwrap();

    let first = generate_and_store(&cache, &a, ThumbSize::Grid).unwrap();

    // Same bytes, different path.
    std::fs::copy(&a, &b).unwrap();
    let second = generate_and_store(&cache, &b, ThumbSize::Grid).unwrap();

    assert_eq!(
        first, second,
        "a move must not invalidate a thumbnail — the key is the content, not the path"
    );
    assert_eq!(key_for_file(&a).unwrap(), key_for_file(&b).unwrap());
}

#[test]
fn an_edited_photograph_gets_a_new_thumbnail() {
    // The complement: content addressing means an edit self-invalidates.
    if !corpus().is_dir() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path());

    let files = jpegs();
    let (Some(a), Some(b)) = (files.first(), files.get(1)) else { return };

    let target = dir.path().join("photo.jpg");
    std::fs::copy(a, &target).unwrap();
    let first = generate_and_store(&cache, &target, ThumbSize::Grid).unwrap();

    // Replace the contents with a different photograph's bytes.
    std::fs::copy(b, &target).unwrap();
    let second = generate_and_store(&cache, &target, ThumbSize::Grid).unwrap();

    assert_ne!(
        first, second,
        "different contents must map to different cache entries, with no invalidation logic"
    );
}

#[test]
fn all_three_sizes_generate_for_a_real_photograph() {
    if !corpus().is_dir() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path());
    let Some(source) = jpegs().into_iter().next() else { return };

    let mut sizes = Vec::new();
    for size in ThumbSize::ALL {
        let path = generate_and_store(&cache, &source, size).unwrap();
        let decoded = image::load_from_memory(&std::fs::read(&path).unwrap()).unwrap();
        assert!(decoded.width().max(decoded.height()) <= size.pixels());
        sizes.push((size, std::fs::metadata(&path).unwrap().len()));
    }

    // Bigger size, bigger file. If they were the same, the sizes are not being honoured.
    assert!(
        sizes[0].1 < sizes[1].1 && sizes[1].1 < sizes[2].1,
        "thumbnail sizes must increase with the requested size: {sizes:?}"
    );
}

#[test]
fn the_cache_stays_within_its_cap_under_real_load() {
    // A cap that is never enforced is not a cap. This drives real files through a small
    // cache and asserts the invariant holds afterwards.
    if !corpus().is_dir() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Small enough that the corpus overflows it.
    let cache = ThumbnailCache::open(dir.path().join("thumbs"), 400 * 1024).unwrap();

    for f in jpegs().iter().take(30) {
        let _ = generate_and_store(&cache, f, ThumbSize::Grid);
        cache.evict_to_cap().expect("evict");
    }

    let stats = cache.stats().unwrap();
    assert!(
        stats.bytes <= cache.cap_bytes(),
        "cache holds {} bytes against a {} byte cap",
        stats.bytes,
        cache.cap_bytes()
    );
    assert!(stats.files > 0, "eviction removed everything, which is not the goal");
}
