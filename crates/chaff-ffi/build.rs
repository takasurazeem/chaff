// **An upstream warning, suppressed deliberately.**
//
// `uniffi::setup_scaffolding!` expands to a `#[macro_export] macro_rules!` inside this
// function, which the `non_local_definitions` lint flags. It is not fixable from here — the
// expansion is required and the macro is uniffi's — and leaving it produces a warning on
// every build of every crate that uses this pattern.
//
// Suppressed rather than tolerated because a build that always warns is a build where a real
// warning goes unread. That is the same reasoning as the `react-refresh` rule in the frontend
// config: record why, do not just silence it.
#![allow(non_local_definitions)]

fn main() {
    // A **proc macro**, not a function: the bang matters, and calling it without one is the
    // error `expected function, found macro`.
    uniffi::setup_scaffolding!();
}
