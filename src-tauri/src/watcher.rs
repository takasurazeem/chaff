//! Watching a library for external changes (#6).
//!
//! # Why the logic lives in the engine and the events live here
//!
//! Deciding *when* a change is worth acting on — settling, coalescing, ignoring the trash —
//! is the part that is wrong in ways that are hard to see, and it is tested in
//! `chaff_core::watch` without touching a filesystem. This file is the driver: it turns OS
//! events into calls on that accumulator and re-indexes when a batch settles.
//!
//! # Why a re-index and not an incremental patch
//!
//! An incremental update would be faster and would have to reimplement the pairing, the
//! scoring, the shoot grouping and the sweep — four things that already work together and
//! would drift apart. A re-index is the operation that is known to produce a correct catalog,
//! and the measurement cache makes a no-op re-index cheap: files whose size and mtime have
//! not moved are not decoded again.
//!
//! # What it does not do
//!
//! It does not act while the user is mid-cull. A batch that settles during a delete dialog
//! would change the grid under a decision the user is about to confirm, so the re-index waits
//! for the dialog to close.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chaff_core::watch::{Accumulator, Change};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// A running watcher.
pub struct Watch {
    /// Flipped to stop the thread.
    stop: Arc<AtomicBool>,
    /// Set while a re-index is in progress, so events during one are coalesced rather than
    /// queueing a second.
    busy: Arc<AtomicBool>,
    /// Paths seen since the last re-index, for the log line.
    seen: Arc<Mutex<usize>>,
}

impl Watch {
    /// Start watching `root`, calling `on_settled` when a batch is worth acting on.
    ///
    /// The callback runs on the watcher's thread, so it should hand off rather than block:
    /// a re-index takes seconds and the OS event queue is finite.
    pub fn start(
        root: PathBuf,
        mut on_settled: impl FnMut(Vec<PathBuf>) + Send + 'static,
    ) -> notify::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(0usize));

        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res| {
                // A full channel means the consumer is behind; dropping an event is
                // survivable because the re-index reads the whole tree anyway. Blocking here
                // would stall the OS notifier for every other process on the machine.
                let _ = tx.send(res);
            })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;

        let thread_stop = Arc::clone(&stop);
        let thread_busy = Arc::clone(&busy);
        let thread_seen = Arc::clone(&seen);
        let thread_root = root.clone();

        std::thread::Builder::new()
            .name("chaff-watch".into())
            .spawn(move || {
                let mut accumulator = Accumulator::new(&thread_root);
                // The watcher must outlive the loop; dropping it stops the events.
                let _watcher = watcher;

                while !thread_stop.load(Ordering::Relaxed) {
                    // Drain whatever has arrived since the last tick.
                    // The timeout is the tick: the accumulator is asked whether anything has
                    // settled once per drain, and `SETTLE` is measured in seconds, so this
                    // only needs to be short enough not to add a visible delay.
                    while let Ok(Ok(event)) = rx.recv_timeout(Duration::from_millis(250)) {
                        let change = classify(&event);
                        for path in &event.paths {
                            if accumulator.record(path, change, Instant::now()) {
                                if let Ok(mut n) = thread_seen.lock() {
                                    *n += 1;
                                }
                            }
                        }
                    }

                    let batch = accumulator.take_settled(Instant::now());
                    if batch.is_empty() {
                        continue;
                    }

                    // **Not while a re-index is already running.** A card import produces
                    // batches faster than one takes, and queueing them would run a dozen
                    // re-indexes over the same growing tree.
                    if thread_busy.swap(true, Ordering::SeqCst) {
                        accumulator.clear();
                        continue;
                    }

                    let count = batch.len();
                    log::info!(
                        "library changed: {} path(s) settled ({} appeared, {} vanished)",
                        count,
                        batch.appeared.len(),
                        batch.vanished.len()
                    );
                    on_settled(batch.appeared);

                    if let Ok(mut n) = thread_seen.lock() {
                        *n = 0;
                    }
                    thread_busy.store(false, Ordering::SeqCst);
                }
            })
            .map_err(|e| notify::Error::io(std::io::Error::other(e)))?;

        Ok(Self { stop, busy, seen })
    }

    /// Stop watching.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether a re-index is in progress.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    /// How many paths have been seen since the last re-index.
    pub fn seen(&self) -> usize {
        self.seen.lock().map_or(0, |n| *n)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What kind of change an event represents.
///
/// Removal is the only case that is not "appeared". A rename arrives as a removal and a
/// creation, which is exactly right: the catalog has to forget one path and learn another.
fn classify(event: &notify::Event) -> Change {
    use notify::EventKind;
    match event.kind {
        EventKind::Remove(_) => Change::Vanished,
        _ => Change::Appeared,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removal_is_a_removal_and_everything_else_appeared() {
        // A rename arrives as a removal plus a creation, which is right: the catalog forgets
        // one path and learns another.
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        let ev = |kind| {
            let mut e = notify::Event::new(kind);
            e.paths = vec![PathBuf::from("/lib/a.CR3")];
            e
        };

        assert_eq!(classify(&ev(notify::EventKind::Remove(RemoveKind::File))), Change::Vanished);
        assert_eq!(classify(&ev(notify::EventKind::Create(CreateKind::File))), Change::Appeared);
        assert_eq!(classify(&ev(notify::EventKind::Modify(ModifyKind::Any))), Change::Appeared);
    }
}
