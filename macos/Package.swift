// swift-tools-version: 6.2
import PackageDescription

// The native macOS shell.
//
// # Why a Swift package rather than an Xcode project
//
// An `.xcodeproj` is a binary blob that merges badly and cannot be reviewed. A package is text:
// a diff shows what changed, and CI can build it with `swift build` without an Xcode GUI.
//
// # What this links
//
// `chaff-ffi` — a static library built by cargo. The engine is 25,565 lines of Rust with no GUI
// dependency; this package is the shell around it.
let package = Package(
    name: "Chaff",
    // **macOS 26, so the system gives the chrome Liquid Glass.**
    //
    // This was `.v14`, chosen without thinking, and it is the whole reason the app looked like a
    // macOS 14 window on a macOS 27 machine: the deployment floor is what asks the system for
    // the compatibility appearance — Glass is granted from the *linked* SDK, and SwiftPM links
    // the SDK matching the floor no matter which SDK the toolchain itself carries. An
    // intermediate try held the floor at 14 and forced the `sdk` field past 26 with an
    // `-platform_version` linker flag; the 27 runtime did not treat that as adoption, so the
    // floor itself is the only knob that moved the chrome.
    //
    // `swift-tools-version` was 6.0 and could not name `.v26` — it had to be 6.2 to say this.
    platforms: [.macOS(.v26)],
    products: [
        .executable(name: "Chaff", targets: ["Chaff"])
    ],
    targets: [
        // The generated UniFFI header and modulemap, as a C target.
        //
        // **A `.target`, not a `.systemLibrary`.** `systemLibrary` is for a module already
        // installed on the system — it looks for the library by name in the default search
        // paths and cannot be told where a cargo build put it. This is a header in the tree and
        // a `.a` beside the build products, so it is a normal target with the header path
        // published and the library linked explicitly.
        //
        // The failure mode of getting this wrong is `unable to resolve module dependency:
        // 'ChaffFFI'`, which names neither the search path nor the library.
        // **Named `chaff_ffiFFI` to match what uniffi generates.** The bindings do
        // `#if canImport(chaff_ffiFFI)`, so a target named anything else silently skips the
        // import and every FFI type becomes "cannot find type in scope" — a failure that names
        // the missing type and not the module that was supposed to provide it.
        .target(
            name: "chaff_ffiFFI",
            path: "Sources/ChaffFFI",
            publicHeadersPath: ".",
            linkerSettings: [
                // **The `.a` by path, not `-lchaff_ffi`.**
                //
                // `-l` searches for a dylib first, and cargo produces both — so the linker
                // picked `libchaff_ffi.dylib` and rejected it with `mis-aligned LINKEDIT string
                // pool`, a message about the file's internal layout that says nothing about
                // having chosen the wrong one of two.
                //
                // `unsafeFlags` is allowed here because this is the root package; it is the
                // only way to point the linker at a library SwiftPM did not build.
                .unsafeFlags([
                    "\(Context.packageDirectory)/../target/release/libchaff_ffi.a",
                    // **The C++ runtime.** LibRaw is C++, and linking the static library
                    // without it fails on `std::length_error` and a page of similar symbols —
                    // a failure that reads as a Rust problem and is not one. The Linux build
                    // needs `-lstdc++` for the same reason and ONNX Runtime, which is how
                    // `gcc-c++` became a documented prerequisite.
                    "-lc++",
                ]),
            ]
        ),
        .executableTarget(
            name: "Chaff",
            dependencies: ["chaff_ffiFFI"],
            path: "Sources/Chaff",
            linkerSettings: [
                // **Link with the SDK the toolchain actually carries, not the floor's.**
                //
                // With the floor at 26, SwiftPM still linked the binary as `sdk 26.0` — the
                // SDK field is pinned to the deployment value, not to the newest SDK in the
                // instalaltion, and this build's Mac runs 27.0. A new OS release adopts
                // apps linked with its own SDK; Xcode's own chrome on this same machine is
                // glassy where a `sdk 26.0`-linked binary was not. The flag re-pairs them:
                // minos stays the floor the `platforms` line declares, and the sdk field
                // becomes the newest SDK present so the runtime treats the binary as
                // built-with-that-SDK.
                //
                // # Why a flag and not the obvious question
                //
                // `swift build --sdk` did not reach the link step when tried against a 14
                // floor: SwiftPM composes its own `-platform_version` from the platform
                // declaration. This override comes *after* SwiftPM's and wins.
                //
                // # Upkeep
                //
                // The `27.0` must track the SDK on the machine building. When the toolchain
                // moves past it, bump this and rebuild — the symptom of a stale value is the
                // chrome quietly losing the current design.
                .unsafeFlags([
                    "-Xlinker", "-platform_version",
                    "-Xlinker", "macos", "-Xlinker", "26.0", "-Xlinker", "27.0",
                ]),
            ]
        ),
        .testTarget(
            name: "ChaffTests",
            dependencies: ["Chaff"],
            path: "Tests/ChaffTests"
        ),
    ]
)
