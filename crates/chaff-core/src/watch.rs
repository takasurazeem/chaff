//! Noticing when the library changes underneath us.
//!
//! # Why this is not just "re-index on a timer"
//!
//! A re-index reads every file's metadata and re-scores what changed. That is the right
//! answer to "has anything changed?" and the wrong answer to "has anything changed *yet*?" —
//! a library being written to by a card import is thousands of files appearing over minutes,
//! and re-indexing on every event would re-index thousands of times.
//!
//! # What this does
//!
//! Accumulates events and **decides when they are worth acting on**. Two rules do the work:
//!
//! * **Settle first.** A file still being written changes repeatedly; acting on the first
//!   event indexes a half-copied raw. Events for a path are held until that path has been
//!   quiet for [`SETTLE`].
//! * **Coalesce.** One import is one re-index, not one per file.
//!
//! # What it deliberately does not do
//!
//! It does not watch subdirectories that the catalog excludes, and it does not follow
//! symlinks out of the library. A watcher that wandered into `.cull-trash` would re-index
//! everything the user just deleted, and one that followed a symlink to `/` would try to
//! index the machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::indexer::EXCLUDED_DIRS;

/// How long a path must be quiet before its change is acted on.
///
/// Long enough that a raw being copied is finished — a 45 MB file over USB takes a second or
/// two — and short enough that a user who has just imported a card does not sit waiting.
pub const SETTLE: Duration = Duration::from_secs(3);

/// What happened to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Created or modified.
    Appeared,
    /// Removed or renamed away.
    Vanished,
}

/// A batch of changes that have settled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    pub appeared: Vec<PathBuf>,
    pub vanished: Vec<PathBuf>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.appeared.is_empty() && self.vanished.is_empty()
    }

    /// How many paths are involved.
    pub fn len(&self) -> usize {
        self.appeared.len() + self.vanished.len()
    }
}

/// Accumulates filesystem events and decides when they are worth acting on.
///
/// Time is passed in rather than read from the clock, so the settle rule is testable without
/// sleeping — a test that sleeps for three seconds is a test that gets deleted.
pub struct Accumulator {
    /// The last time each path changed.
    last_seen: HashMap<PathBuf, (Change, Instant)>,
    root: PathBuf,
}

impl Accumulator {
    /// An accumulator for a library rooted at `root`.
    ///
    /// Everything outside the root is ignored: a watcher is attached to the library, and a
    /// path outside it is either a bug in the caller or a symlink that led somewhere it
    /// should not have.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { last_seen: HashMap::new(), root: root.into() }
    }

    /// Record an event.
    ///
    /// Returns `false` when the path is not one this watcher cares about — excluded, outside
    /// the library, or a sidecar whose photograph is what actually matters.
    pub fn record(&mut self, path: &Path, change: Change, now: Instant) -> bool {
        if !self.wants(path) {
            return false;
        }
        // Later events for one path replace earlier ones. A file created and then modified is
        // one "appeared", and the timer restarts — which is the point of settling.
        self.last_seen.insert(path.to_path_buf(), (change, now));
        true
    }

    /// Is this path one the watcher should act on?
    pub fn wants(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }
        // An excluded directory anywhere in the path, not only at the root. `.cull-trash` is
        // the important one: watching it would re-index everything the user just deleted.
        if path.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| EXCLUDED_DIRS.contains(&s) || s.starts_with('.'))
        }) {
            return false;
        }
        // Only files the catalog would index.
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(crate::ext::is_indexable_extension)
    }

    /// Everything that has settled by `now`, and forget it.
    pub fn take_settled(&mut self, now: Instant) -> Batch {
        let mut batch = Batch::default();
        self.last_seen.retain(|path, (change, at)| {
            if now.duration_since(*at) < SETTLE {
                return true; // still moving; keep waiting
            }
            match change {
                Change::Appeared => batch.appeared.push(path.clone()),
                Change::Vanished => batch.vanished.push(path.clone()),
            }
            false
        });

        // Sorted, so a batch is deterministic and a test can assert on it.
        batch.appeared.sort();
        batch.vanished.sort();
        batch
    }

    /// How many paths are being tracked but have not settled.
    pub fn pending(&self) -> usize {
        self.last_seen.len()
    }

    /// Forget everything — used when a full re-index makes the accumulated events moot.
    pub fn clear(&mut self) {
        self.last_seen.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64) -> Instant {
        // A fixed base, so tests are not sensitive to when they run.
        Instant::now() + Duration::from_secs(secs)
    }

    #[test]
    fn a_change_is_not_acted_on_until_it_settles() {
        // **The rule that stops a half-copied raw being indexed.** A file still being written
        // changes repeatedly; acting on the first event reads a partial file.
        let mut a = Accumulator::new("/lib");
        let base = Instant::now();
        a.record(Path::new("/lib/IMG_0001.CR3"), Change::Appeared, base);

        assert!(a.take_settled(base + Duration::from_secs(1)).is_empty(), "too soon");
        assert_eq!(a.pending(), 1, "and it is still being tracked");

        let batch = a.take_settled(base + SETTLE + Duration::from_secs(1));
        assert_eq!(batch.appeared, vec![PathBuf::from("/lib/IMG_0001.CR3")]);
    }

    #[test]
    fn repeated_changes_restart_the_timer() {
        // A file being written emits many events. Each one means "still moving", and the
        // settle window runs from the *last* of them.
        let mut a = Accumulator::new("/lib");
        let base = Instant::now();
        let path = Path::new("/lib/IMG_0001.CR3");

        a.record(path, Change::Appeared, base);
        a.record(path, Change::Appeared, base + Duration::from_secs(2));
        a.record(path, Change::Appeared, base + Duration::from_secs(4));

        assert!(
            a.take_settled(base + Duration::from_secs(5)).is_empty(),
            "4s + settle has not elapsed"
        );
        assert_eq!(a.take_settled(base + Duration::from_secs(8)).len(), 1);
    }

    #[test]
    fn a_batch_coalesces_many_files_into_one() {
        // One card import is one re-index, not one per file. Without this the library would
        // be re-indexed thousands of times during an import.
        let mut a = Accumulator::new("/lib");
        let base = Instant::now();
        for i in 0..500 {
            a.record(&PathBuf::from(format!("/lib/IMG_{i:04}.CR3")), Change::Appeared, base);
        }
        let batch = a.take_settled(base + SETTLE + Duration::from_secs(1));
        assert_eq!(batch.appeared.len(), 500);
        assert_eq!(a.pending(), 0, "and nothing is left behind");
    }

    #[test]
    fn the_trash_folder_is_ignored() {
        // **The one that matters.** Watching `.cull-trash` would re-index everything the user
        // just deleted — the exact opposite of what they asked for.
        let mut a = Accumulator::new("/lib");
        assert!(!a.record(Path::new("/lib/.cull-trash/op/IMG_0001.CR3"), Change::Appeared, t(0)));
        assert!(!a.wants(Path::new("/lib/.cull-trash/anything.CR3")));
    }

    #[test]
    fn every_excluded_directory_is_ignored() {
        let a = Accumulator::new("/lib");
        for dir in crate::indexer::EXCLUDED_DIRS {
            let p = PathBuf::from(format!("/lib/{dir}/IMG_0001.CR3"));
            assert!(!a.wants(&p), "{dir} must not be watched");
        }
    }

    #[test]
    fn anything_outside_the_library_is_ignored() {
        // A watcher is attached to the library. A path outside it is a bug in the caller or a
        // symlink that led somewhere it should not have.
        let a = Accumulator::new("/lib");
        assert!(!a.wants(Path::new("/elsewhere/IMG_0001.CR3")));
        assert!(!a.wants(Path::new("/")));
        assert!(!a.wants(Path::new("/lib/../etc/passwd")));
    }

    #[test]
    fn a_file_the_catalog_would_not_index_is_ignored() {
        let a = Accumulator::new("/lib");
        assert!(a.wants(Path::new("/lib/IMG_0001.CR3")));
        assert!(a.wants(Path::new("/lib/IMG_0001.JPG")));
        assert!(!a.wants(Path::new("/lib/notes.txt")));
        assert!(!a.wants(Path::new("/lib/catalog.db")));
        assert!(!a.wants(Path::new("/lib/noextension")));
    }

    #[test]
    fn a_vanished_file_is_reported_as_vanished() {
        let mut a = Accumulator::new("/lib");
        let base = Instant::now();
        a.record(Path::new("/lib/gone.CR3"), Change::Vanished, base);
        let batch = a.take_settled(base + SETTLE + Duration::from_secs(1));
        assert_eq!(batch.vanished, vec![PathBuf::from("/lib/gone.CR3")]);
        assert!(batch.appeared.is_empty());
    }

    #[test]
    fn clearing_forgets_everything() {
        // A full re-index makes the accumulated events moot: it has already looked at
        // everything they describe.
        let mut a = Accumulator::new("/lib");
        a.record(Path::new("/lib/IMG_0001.CR3"), Change::Appeared, t(0));
        assert_eq!(a.pending(), 1);
        a.clear();
        assert_eq!(a.pending(), 0);
    }

    #[test]
    fn a_batch_is_sorted_so_it_is_deterministic() {
        let mut a = Accumulator::new("/lib");
        let base = Instant::now();
        for name in ["c.CR3", "a.CR3", "b.CR3"] {
            a.record(&PathBuf::from(format!("/lib/{name}")), Change::Appeared, base);
        }
        let batch = a.take_settled(base + SETTLE + Duration::from_secs(1));
        assert_eq!(
            batch.appeared,
            vec![
                PathBuf::from("/lib/a.CR3"),
                PathBuf::from("/lib/b.CR3"),
                PathBuf::from("/lib/c.CR3")
            ]
        );
    }

    #[test]
    fn an_empty_batch_is_empty() {
        let mut a = Accumulator::new("/lib");
        assert!(a.take_settled(Instant::now()).is_empty());
        assert_eq!(a.take_settled(Instant::now()).len(), 0);
    }
}
