import SwiftUI

@main
struct ChaffApp: App {
    @State private var model = EngineModel()

    var body: some Scene {
        WindowGroup {
            LibraryView()
                .environment(model)
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
