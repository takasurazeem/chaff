//! Generates the Swift bindings.
//!
//! Run from the build script rather than installed globally, so the generator version cannot
//! drift from the library's. A mismatch produces Swift that compiles and then misbehaves at
//! the boundary, which is the worst kind of bug to find.
fn main() {
    uniffi::uniffi_bindgen_main()
}
