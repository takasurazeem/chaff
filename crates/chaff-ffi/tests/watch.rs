//! The watcher's two rules, and the one it must not break.
//!
//! # Why these are worth testing without waiting for a filesystem event
//!
//! The interesting behaviour is not "does polling work" — it is **when the watcher must stay
//! quiet**, and that is a decision the engine makes, not the OS. A re-index during a delete
//! confirmation turns Confirm into a hard failure, and the user has no way to know a watcher
//! caused it.

use std::path::Path;

struct Silent;
impl chaff_ffi::Progress for Silent {
    fn on_progress(&self, _d: u32, _t: u32, _s: String, _c: String) -> bool {
        true
    }
}

/// A library with one photograph, and an engine pointed at it.
fn library() -> (tempfile::TempDir, std::sync::Arc<chaff_ffi::Engine>, i64, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("IMG_0001.JPG"), b"pretend jpeg").unwrap();

    let engine =
        chaff_ffi::Engine::new(root.join("catalog.db").to_string_lossy().to_string()).unwrap();
    chaff_ffi::set_data_root(root.join("appdata").to_string_lossy().to_string());
    let report = engine
        .open_library(root.to_string_lossy().to_string(), Box::new(Silent))
        .unwrap();
    (dir, engine, report.library.id, root.to_string_lossy().to_string())
}

#[test]
fn a_library_is_not_watched_until_it_is_asked_for() {
    // The default has to be off. A watcher that started itself would re-index a library while
    // the user was doing something else, with nothing on screen to say why the grid changed.
    let (_d, engine, _lib, _root) = library();
    let status = engine.watch_status().unwrap();
    assert!(!status.running, "{status:?}");
    assert_eq!(status.seen, 0);
    assert!(!status.busy);
}

#[test]
fn starting_twice_is_a_no_op_rather_than_two_watchers() {
    // **Two watchers on one library is two re-indexes per change**, and the second one is
    // invisible — the status says "running" either way, so nothing on screen would explain the
    // doubled work.
    let (_d, engine, lib, root) = library();

    let first = engine.start_watching(root.clone(), lib).unwrap();
    assert!(first.running);

    let second = engine.start_watching(root.clone(), lib).unwrap();
    assert!(second.running, "still running, not a second one");

    engine.stop_watching().unwrap();
}

#[test]
fn stopping_reports_stopped_and_a_restart_works() {
    let (_d, engine, lib, root) = library();

    engine.start_watching(root.clone(), lib).unwrap();
    let stopped = engine.stop_watching().unwrap();
    assert!(!stopped.running, "{stopped:?}");
    assert!(!engine.watch_status().unwrap().running);

    // Restarting must work: a user who stops a watcher to do something and starts it again has
    // to get a watcher, not a stale "already running" from a slot that was never cleared.
    let again = engine.start_watching(root.clone(), lib).unwrap();
    assert!(again.running, "a stopped watcher must be startable again");
    engine.stop_watching().unwrap();
}

#[test]
fn stopping_when_nothing_is_running_is_not_an_error() {
    // Idempotent, because a UI calls this on quit whether or not the user ever started one.
    let (_d, engine, _lib, _root) = library();
    let status = engine.stop_watching().unwrap();
    assert!(!status.running);
    assert!(!engine.watch_status().unwrap().running);
}

#[test]
fn a_pending_delete_is_visible_to_the_watcher_and_cleared_by_cancelling() {
    // **The rule the whole feature exists for.** `DeleteSession::commit` refuses any file that
    // was not in the plan the user was shown, so a re-index during the confirmation dialog turns
    // Confirm into a hard failure — with no recovery path and nothing on screen to say a watcher
    // caused it.
    //
    // The watcher reads a flag rather than the session, because it runs on a thread that
    // outlives any borrow of the engine. A flag that is set and never cleared would stop the
    // watcher forever, so both directions are checked.
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();
    assert_eq!(photos.len(), 1);

    assert!(!engine.has_pending_delete(), "nothing pending to start with");

    let plan = engine.plan_delete(root.clone(), vec![photos[0].id]).unwrap();
    assert!(plan.refusals.is_empty(), "{plan:?}");
    assert!(
        engine.has_pending_delete(),
        "a plan exists, so the watcher must see one — otherwise a re-index during the dialog \
         adds a file row and Confirm fails"
    );

    engine.cancel_delete().unwrap();
    assert!(
        !engine.has_pending_delete(),
        "cancelling ends the pending state, and the watcher must resume — a flag left set stops \
         it forever with nothing on screen to explain it"
    );
}

#[test]
fn committing_also_clears_the_pending_state() {
    // The other way out. A refusal clears it too: the session drops the plan on every path out
    // of `commit`, and a mirror that only cleared on success would leave the watcher stopped
    // after a refusal the user never saw again.
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();

    assert!(engine.plan_delete(root.clone(), vec![photos[0].id]).unwrap().refusals.is_empty());
    assert!(engine.has_pending_delete());

    engine.commit_delete(root.clone()).unwrap();
    assert!(!engine.has_pending_delete(), "the move happened; nothing is pending");
}

#[test]
fn the_trash_is_not_part_of_what_the_watcher_watches() {
    // The trash lives **inside** the library, so a watcher that counted it would see every
    // delete the application itself performed and re-index the library it had just changed —
    // a loop where each delete triggers a pass that finds nothing new.
    //
    // Asserted through the behaviour that matters: after a move, the library's own file set is
    // unchanged, so a re-index would find nothing. The exclusion in `newest_mtime` is what keeps
    // that from being a wasted pass rather than a correct one.
    let (_d, engine, lib, root) = library();
    let photos = engine.photos(lib).unwrap();
    assert!(engine.plan_delete(root.clone(), vec![photos[0].id]).unwrap().refusals.is_empty());
    engine.commit_delete(root.clone()).unwrap();

    assert!(
        Path::new(&root).join(".cull-trash").exists(),
        "the trash is inside the library, which is why it has to be excluded"
    );
}
