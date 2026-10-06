import SwiftUI

@main
struct ChaffApp: App {
    @State private var model = EngineModel()
    @State private var culling = Culling()
    @State private var showTrash = false
    @State private var showFaceReview = false
    @State private var showCapabilities = false

    var body: some Scene {
        WindowGroup {
            LibraryView()
                .environment(model)
                .environment(culling)
                .sheet(isPresented: $showTrash) { TrashPanel() }
                .sheet(isPresented: $showFaceReview) { FaceReview() }
                .sheet(isPresented: $showCapabilities) { CapabilitiesSheet() }
                // A photo tool wants the room. The default window is sized for a form.
                .frame(minWidth: 900, minHeight: 600)
        }
        .defaultSize(width: 1400, height: 900)
        .commands {
            // **In the menu bar, which the web app cannot do.**
            //
            // The keyboard is how culling is done — a rating on every frame — and shortcuts a
            // user cannot discover are shortcuts they do not use. `CommandGroup` puts them
            // where macOS users look for them, and gives them a place to be remapped later.
            CommandGroup(after: .newItem) {
                Button("Open Library…") { openLibrary() }
                    .keyboardShortcut("o")
                // **Idempotent and incremental**, so this is safe to press — the engine reuses
                // cached measurements and reconciles rather than rebuilding.
                Button("Re-index") { Task { await model.reindex() } }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(model.library == nil)
            }

            // **The culling keys, in the menu bar.**
            //
            // This is what a native shell buys that a webview cannot: shortcuts a user can
            // *find*. Culling is a keyboard activity — a rating on every frame — and a shortcut
            // nobody can discover is one nobody uses.
            CommandMenu("Cull") {
                ForEach(1...5, id: \.self) { stars in
                    Button("\(stars) Star\(stars == 1 ? "" : "s")") {
                        Task { await culling.rate(UInt8(stars), in: model) }
                    }
                    .keyboardShortcut(KeyEquivalent(Character("\(stars)")), modifiers: [])
                }
                Divider()
                Button("Reject") { Task { await culling.toggleReject(in: model) } }
                    .keyboardShortcut("x", modifiers: [])
                Divider()
                // **Space, because that is what it is everywhere else.** Finder, Photos, Preview
                // and every other viewer on the platform use it for "show me this one", and a
                // culling tool that chose something else would be the odd one out.
                Button("Look at This One") { model.showLoupe = true }
                    .keyboardShortcut(.space, modifiers: [])
                Divider()
                Button("Undo") { Task { await culling.undo(in: model) } }
                    .keyboardShortcut("z", modifiers: .command)
                Divider()
                // **The AI passes, in the menu** rather than hidden in a panel. They are the
                // two longest-running operations in the app, and a user who cannot find them
                // cannot decide whether to start one.
                Button("Find People") { Task { await model.findFaces() } }
                    .keyboardShortcut("f", modifiers: [.command, .shift])
                Button("Tag Photographs") { Task { await model.tag() } }
                    .keyboardShortcut("t", modifiers: [.command, .shift])
                Divider()
                // **The trash, in the menu.** An application that fills a container it cannot
                // empty is one people stop trusting with their photographs — and after a
                // restart, ⌘Z has nothing to reverse.
                Button("Trash…") { showTrash = true }
                    .keyboardShortcut("t", modifiers: [.command, .shift, .option])
                Button("Faces to Check…") { showFaceReview = true }
                    .keyboardShortcut("r", modifiers: [.command, .shift])
                Divider()
                // **Ratings that do not leave the app are ratings a photographer re-does.**
                // Lightroom, Darktable and Bridge all read XMP.
                Button("Write Sidecars") { Task { await model.writeSidecars() } }
                Button("This Machine…") { showCapabilities = true }
                Button("Check Tagging Endpoint") { Task { await model.diagnoseTagging() } }
                Divider()
                // **A toggle, and it says what it is.** A watcher the user cannot see is one
                // that re-indexes the grid underneath them with nothing on screen to explain
                // why the photographs changed.
                Toggle(
                    "Watch This Library",
                    isOn: Binding(
                        get: { model.watchStatus.running },
                        set: { on in
                            Task { on ? await model.startWatching() : await model.stopWatching() }
                        }
                    )
                )
            }
        }
    }

    /// Ask for a folder.
    ///
    /// `fileImporter` is not available from a `Commands` builder, so this uses the panel
    /// directly — and the entitlement question is real: a sandboxed app cannot read
    /// `~/Pictures` without user selection, which is exactly what this is.
    private func openLibrary() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Open"
        panel.message = "Choose a folder of photographs."

        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await model.open(url) }
    }
}
