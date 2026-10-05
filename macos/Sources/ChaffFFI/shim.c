// A C target with only headers produces **no object file**, and SwiftPM then fails at link time
// with `Build input file cannot be found: .../chaff_ffiFFI.o` — a message that names a file the
// target was never going to produce and says nothing about why.
//
// This file exists so the target has something to compile. The actual implementation is the
// Rust static library, linked with `-lchaff_ffi`.
//
// It is deliberately empty of declarations: everything this module publishes comes from the
// UniFFI-generated header beside it.
