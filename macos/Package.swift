// swift-tools-version: 6.0
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
    // macOS 14 window on a macOS 27 machine: the deployment target is what asks the system for
    // the compatibility appearance. Toolbars, sidebars and inspectors adopt the new material
    // automatically once the app declares it can use it.
    //
    // The cost is real and worth stating: this no longer runs on macOS 14 or 15. Given the
    // machine it is built for is on 27, that is a trade worth making — and the Tauri build
    // remains for anything older.
    // **macOS 14, with the newer APIs availability-gated.**
    //
    // This was briefly `.macOS("26.0")` — a hard floor — because that is what makes the system
    // hand the chrome Liquid Glass. It works and it drops every user on 14 or 15 to get a
    // material on a toolbar.
    //
    // The deployment target says what you *require*; availability says what you *prefer*.
    // Conflating them charges the cost to people who did nothing but not upgrade.
    //
    // Everything newer lives behind `#available` in `Compatibility.swift`, so the checks are in
    // one file rather than scattered through layout code where a missed one is a crash on an
    // older machine instead of a compile error.
    platforms: [.macOS(.v14)],
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
            path: "Sources/Chaff"
        ),
        .testTarget(
            name: "ChaffTests",
            dependencies: ["Chaff"],
            path: "Tests/ChaffTests"
        ),
    ]
)
